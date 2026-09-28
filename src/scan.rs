//! `scan`：把本地 Steam 库读成一份结构化数据。

use std::collections::{HashMap, HashSet};

use crate::appinfo;
use crate::model::*;
use crate::steam;
use crate::util;

pub struct ScanOptions {
    pub steam_dir: Option<String>,
    pub user: Option<String>,
    /// 是否把「已拥有但未安装」的游戏也纳入。
    ///
    /// 一个真实的库往往大部分游戏并没有装（本机：已安装 35 款、被 Steam 记住的 appid 约 1290 个）。
    /// 但补名字要额外读 45 MB 的 `appcache/appinfo.vdf`，所以留一个开关。
    pub include_uninstalled: bool,
}

pub struct ScanOutcome {
    pub library: Library,
    pub steam_root: std::path::PathBuf,
    pub steam3: String,
    pub found_by: String,
}

pub fn run(opts: &ScanOptions) -> Result<ScanOutcome, String> {
    let install = steam::find_root(opts.steam_dir.as_deref())?;
    let account = steam::pick_account(&install.root, opts.user.as_deref())?;
    let mut warnings: Vec<String> = Vec::new();

    // ---- 库根 ----
    let roots = steam::read_library_roots(&install.root)?;

    // ---- 游玩时长 ----
    let localconfig = install
        .root
        .join("userdata")
        .join(&account.steam3)
        .join("config")
        .join("localconfig.vdf");
    let playtime = match steam::read_playtime(&localconfig) {
        Ok(map) => map,
        Err(e) => {
            warnings.push(format!("读取游玩时长失败，相关字段将留空: {}", e));
            HashMap::new()
        }
    };

    // ---- 现有合集 ----
    let cloud = match steam::read_cloud(&install.root, &account.steam3) {
        Ok(c) => Some(c),
        Err(e) => {
            warnings.push(e);
            None
        }
    };

    let mut membership: HashMap<u32, Vec<String>> = HashMap::new();
    let mut hidden_ids: HashSet<u32> = HashSet::new();
    let mut favorite_ids: HashSet<u32> = HashSet::new();
    let mut existing_collections: Vec<CollectionInfo> = Vec::new();

    if let Some(cloud) = &cloud {
        for col in &cloud.collections {
            if col.is_deleted {
                continue;
            }
            if col.id == "hidden" {
                hidden_ids.extend(col.added.iter().copied());
            }
            if col.id == "favorite" {
                favorite_ids.extend(col.added.iter().copied());
            }
            // 只把「可编辑的静态合集」算作用户的归档归属；
            // 动态合集由 Steam 按过滤器实时计算，favorite/hidden 是系统位。
            if col.editable() {
                for appid in &col.added {
                    membership.entry(*appid).or_default().push(col.name.clone());
                }
            }
            existing_collections.push(CollectionInfo {
                id: col.id.clone(),
                name: if col.is_builtin && col.name.is_empty() {
                    col.id.clone()
                } else {
                    col.name.clone()
                },
                appids: col.added.clone(),
                is_builtin: col.is_builtin,
                is_dynamic: col.is_dynamic,
                is_deleted: col.is_deleted,
            });
        }
        existing_collections.sort_by(|a, b| {
            a.is_builtin
                .cmp(&b.is_builtin)
                .then_with(|| a.name.cmp(&b.name))
        });
    }

    // ---- 逐个库根读 appmanifest ----
    let mut games: Vec<Game> = Vec::new();
    let mut root_infos: Vec<RootInfo> = Vec::new();
    let mut seen_appids: HashSet<u32> = HashSet::new();

    for root in &roots {
        let reachable = root.path.is_dir();
        let manifests = if reachable {
            steam::read_manifests(&root.path)
        } else {
            warnings.push(format!(
                "库根不可达（盘符可能未挂载），已跳过: {}",
                root.path.display()
            ));
            Vec::new()
        };

        for m in &manifests {
            if !seen_appids.insert(m.appid) {
                warnings.push(format!(
                    "appid {} ({}) 在多个库根里都有 manifest，只保留第一个",
                    m.appid, m.name
                ));
                continue;
            }
            let stats = playtime.get(&m.appid).cloned().unwrap_or_default();
            let last_played = stats.last_played.or(m.last_played);
            let mut cols = membership.get(&m.appid).cloned().unwrap_or_default();
            cols.sort();
            cols.dedup();

            let days_since_played = last_played.map(util::days_since);
            games.push(Game {
                appid: m.appid,
                name: if m.name.is_empty() {
                    format!("(未知名称 {})", m.appid)
                } else {
                    m.name.clone()
                },
                installdir: m.installdir.clone(),
                library: root.path.to_string_lossy().into_owned(),
                size_bytes: m.size_on_disk,
                state_flags: m.state_flags,
                installed: m.installed(),
                last_updated: m.last_updated,
                language: m.language.clone(),
                auto_update: m.auto_update,
                franchises: Vec::new(),
                developers: Vec::new(),
                publishers: Vec::new(),
                achievements_total: 0,
                achievements_unlocked: 0,
                playtime_minutes: stats.playtime_minutes,
                playtime_2wks_minutes: stats.playtime_2wks_minutes,
                last_played,
                last_played_local: last_played.map(util::fmt_local),
                collections: cols,
                hidden: hidden_ids.contains(&m.appid),
                favorite: favorite_ids.contains(&m.appid),
                never_launched: stats.playtime_minutes == 0 && last_played.is_none(),
                days_since_played,
                barely_played: stats.playtime_minutes > 0 && stats.playtime_minutes < 60,
                dormant: days_since_played.map(|d| d > 365).unwrap_or(false),
            });
        }

        root_infos.push(RootInfo {
            index: root.index.clone(),
            path: root.path.to_string_lossy().into_owned(),
            label: root.label.clone(),
            total_size_bytes: root.total_size,
            reachable,
            game_count: manifests.len(),
        });
    }

    // ---- 用 librarycache 补上系列 / 厂商 / 成就进度 ----
    if !games.is_empty() {
        let wanted: HashSet<u32> = games.iter().map(|g| g.appid).collect();
        let cache = steam::read_library_cache(&install.root, &account.steam3, &wanted);
        let mut enriched = 0usize;
        for g in games.iter_mut() {
            if let Some(m) = cache.get(&g.appid) {
                g.franchises = m.franchises.clone();
                g.developers = m.developers.clone();
                g.publishers = m.publishers.clone();
                g.achievements_total = m.achievements_total;
                g.achievements_unlocked = m.achievements_unlocked;
                enriched += 1;
            }
        }
        if enriched == 0 {
            warnings.push(
                "librarycache 里没有匹配到任何元数据，系列/厂商/成就字段将为空。".into(),
            );
        }
    }

    // ---- 补上「已拥有但未安装」的游戏 ----
    //
    // appmanifest 只覆盖已安装的；`librarycache` 里有「被 Steam 记住的全部 appid」但没有名字，
    // 所以名字只能去 `appcache/appinfo.vdf` 取（45 MB 的二进制 KeyValues，实测 19423 条、
    // 19361 条有名字，与 appmanifest 的 35 个名字逐条一致）。
    //
    // 这一步刻意做成「失败就降级」：Steam 运行时会**就地重写** appinfo.vdf
    // （实测会话内文件从 47,370,030 涨到 47,370,333 字节），随时可能读到半截文件。
    // 读不动就只统计已安装的游戏，并给出可读告警，绝不因此让整次扫描失败。
    if opts.include_uninstalled {
        let owned = owned_appids(&install.root, &account.steam3);
        // 排序后再处理，保证输出顺序稳定（HashSet 的遍历顺序是不确定的）
        let mut missing: Vec<u32> = owned
            .iter()
            .copied()
            .filter(|id| !seen_appids.contains(id))
            .collect();
        missing.sort_unstable();

        if !missing.is_empty() {
            let mut wanted: HashSet<u32> = HashSet::with_capacity(missing.len());
            wanted.extend(missing.iter().copied());

            let candidate = install.root.join("appcache").join("appinfo.vdf");
            let appinfo_path = if candidate.is_file() {
                Some(candidate)
            } else {
                appinfo::default_appinfo_path()
            };

            match appinfo_path {
                Some(path) => match appinfo::parse_names(&path, &wanted) {
                    Ok(entries) => {
                        for appid in missing.iter().copied() {
                            let stats = playtime.get(&appid).cloned().unwrap_or_default();
                            let mut cols = membership.get(&appid).cloned().unwrap_or_default();
                            cols.sort();
                            cols.dedup();
                            let name = entries
                                .get(&appid)
                                .map(|e| e.name.trim().to_string())
                                .filter(|n| !n.is_empty())
                                .unwrap_or_else(|| format!("App {}", appid));
                            games.push(make_uninstalled_game(
                                appid,
                                &name,
                                &stats,
                                cols,
                                hidden_ids.contains(&appid),
                                favorite_ids.contains(&appid),
                            ));
                        }
                    }
                    Err(e) => warnings.push(format!(
                        "读取 appinfo.vdf 失败，本次只统计已安装的游戏（可用 --installed-only 静默跳过）: {}",
                        e
                    )),
                },
                None => warnings.push(
                    "找不到 appcache/appinfo.vdf，「已拥有但未安装」的游戏这次无法补名字，只统计已安装的游戏。"
                        .into(),
                ),
            }
        }
    }

    games.sort_by(|a, b| {
        b.playtime_minutes
            .cmp(&a.playtime_minutes)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    if games.is_empty() {
        warnings.push("一个已安装游戏都没读到：确认 Steam 库路径是否正确。".into());
    }

    let now = util::now_epoch();
    let library = Library {
        schema: LIBRARY_SCHEMA.to_string(),
        scanned_at: now,
        scanned_at_local: util::fmt_local(now),
        steam_root: install.root.to_string_lossy().into_owned(),
        account: AccountInfo {
            steam3_id: account.steam3.clone(),
            steamid64: account.steamid64.clone(),
            persona_name: account.persona.clone(),
        },
        library_roots: root_infos,
        games,
        existing_collections,
        warnings,
    };

    Ok(ScanOutcome {
        library,
        steam_root: install.root,
        steam3: account.steam3,
        found_by: install.found_by,
    })
}

/// 导出 CSV，方便丢进 Excel 自己看一眼。
pub fn to_csv(lib: &Library) -> String {
    let mut out = String::new();
    out.push_str("appid,name,playtime_hours,playtime_2wks_hours,size_gb,last_played,days_since_played,never_launched,dormant,collections,library,installdir\n");
    for g in &lib.games {
        out.push_str(&format!(
            "{},{},{:.1},{:.1},{:.2},{},{},{},{},{},{},{}\n",
            g.appid,
            csv_field(&g.name),
            util::hours(g.playtime_minutes),
            util::hours(g.playtime_2wks_minutes),
            util::gb(g.size_bytes),
            g.last_played_local.clone().unwrap_or_else(|| "从未启动".into()),
            g.days_since_played
                .map(|d| d.to_string())
                .unwrap_or_else(|| "-".into()),
            if g.never_launched { "是" } else { "" },
            if g.dormant { "是" } else { "" },
            csv_field(&g.collections.join("|")),
            csv_field(&g.library),
            csv_field(&g.installdir),
        ));
    }
    out
}

fn csv_field(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

// ---------------------------------------------------------------------------
// 「已拥有但未安装」
// ---------------------------------------------------------------------------

/// 这个账号在本机被 Steam 记住的全部 appid。
///
/// `appmanifest` 只覆盖**已安装**的游戏，而一个真实的库往往大部分游戏并没装
/// （本机：已安装 35 款，librarycache 里却有约 1292 个 appid）。这个集合来自两处：
///   * `userdata/<steam3>/config/librarycache/<appid>.json` 的文件名；
///   * 现有用户合集的成员。
///
/// 注意：librarycache 里**没有游戏名**（已实测），所以这里只给 appid 集合，
/// 名字要靠 `crate::appinfo` 去 `appcache/appinfo.vdf` 里取。
pub fn owned_appids(steam_root: &std::path::Path, steam3: &str) -> HashSet<u32> {
    let mut out: HashSet<u32> = HashSet::new();

    let dir = steam_root
        .join("userdata")
        .join(steam3)
        .join("config")
        .join("librarycache");
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(stem) = name.strip_suffix(".json") {
                if let Ok(id) = stem.parse::<u32>() {
                    out.insert(id);
                }
            }
        }
    }

    if let Ok(cloud) = steam::read_cloud(steam_root, steam3) {
        for c in &cloud.collections {
            if c.is_deleted {
                continue;
            }
            out.extend(c.added.iter().copied());
        }
    }

    out
}

/// 把一个「已拥有但未安装」的 appid 变成一条 `Game` 记录。
///
/// 这类记录刻意与已安装的区分开：`installed=false`、体积为 0、没有库路径与安装目录。
/// 但**游玩时长与最后游玩时间是真实的**（卸载不会抹掉 localconfig 里的记录），
/// 所以「从未启动 / 沉寂」这些派生位照样有意义。
pub fn make_uninstalled_game(
    appid: u32,
    name: &str,
    stats: &steam::PlayStats,
    collections: Vec<String>,
    hidden: bool,
    favorite: bool,
) -> Game {
    let last_played = stats.last_played;
    let days_since_played = last_played.map(util::days_since);
    Game {
        appid,
        name: if name.trim().is_empty() {
            format!("(未知名称 {})", appid)
        } else {
            name.to_string()
        },
        installdir: String::new(),
        library: String::new(),
        size_bytes: 0,
        state_flags: 0,
        installed: false,
        last_updated: None,
        language: None,
        auto_update: None,
        franchises: Vec::new(),
        developers: Vec::new(),
        publishers: Vec::new(),
        achievements_total: 0,
        achievements_unlocked: 0,
        playtime_minutes: stats.playtime_minutes,
        playtime_2wks_minutes: stats.playtime_2wks_minutes,
        last_played,
        last_played_local: last_played.map(util::fmt_local),
        collections,
        hidden,
        favorite,
        never_launched: stats.playtime_minutes == 0 && last_played.is_none(),
        days_since_played,
        barely_played: stats.playtime_minutes > 0 && stats.playtime_minutes < 60,
        dormant: days_since_played.map(|d| d > 365).unwrap_or(false),
    }
}

// ---------------------------------------------------------------------------
// 概览统计（inspect / 预览头部共用）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct Summary {
    pub games: usize,
    /// 其中已安装的
    pub installed: usize,
    /// 其中「已拥有但未安装」的
    pub uninstalled: usize,
    pub total_size: u64,
    pub total_playtime_minutes: u64,
    pub never_launched: usize,
    pub barely_played: usize,
    pub dormant: usize,
    pub hidden: usize,
    pub favorite: usize,
    pub unassigned: usize,
    pub collection_count: usize,
}

pub fn summarize(lib: &Library) -> Summary {
    let mut s = Summary {
        games: lib.games.len(),
        ..Default::default()
    };
    for g in &lib.games {
        if g.installed {
            s.installed += 1;
        } else {
            s.uninstalled += 1;
        }
        s.total_size += g.size_bytes;
        s.total_playtime_minutes += g.playtime_minutes;
        if g.never_launched {
            s.never_launched += 1;
        }
        if g.barely_played {
            s.barely_played += 1;
        }
        if g.dormant {
            s.dormant += 1;
        }
        if g.hidden {
            s.hidden += 1;
        }
        if g.favorite {
            s.favorite += 1;
        }
        if g.collections.is_empty() {
            s.unassigned += 1;
        }
    }
    s.collection_count = lib
        .existing_collections
        .iter()
        .filter(|c| !c.is_builtin && !c.is_dynamic)
        .count();
    s
}
