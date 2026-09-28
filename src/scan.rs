//! `scan`：把本地 Steam 库读成一份结构化数据。

use std::collections::{HashMap, HashSet};

use crate::model::*;
use crate::steam;
use crate::util;

pub struct ScanOptions {
    pub steam_dir: Option<String>,
    pub user: Option<String>,
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
// 概览统计（inspect / 预览头部共用）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct Summary {
    pub games: usize,
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
