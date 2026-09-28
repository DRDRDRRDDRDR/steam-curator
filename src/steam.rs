//! Steam 安装定位与各类本地文件的读取。
//!
//! 涉及的文件（全部为实测确认的路径，见 docs/STEAM-FORMATS.md）：
//!   * `<steam>/steamapps/libraryfolders.vdf`              各库根
//!   * `<lib>/steamapps/appmanifest_<appid>.acf`           已安装游戏清单
//!   * `<steam>/config/loginusers.vdf`                     账号列表与 MostRecent
//!   * `<steam>/userdata/<steam3>/config/localconfig.vdf`  游玩时长
//!   * `<steam>/userdata/<steam3>/config/cloudstorage/cloud-storage-namespace-1.json`
//!                                                         新版合集（权威）
//!   * `<steam>/userdata/<steam3>/config/cloudstorage/cloud-storage-namespaces.json`
//!                                                         命名空间版本号

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::vdf::{self, Table};

// ---------------------------------------------------------------------------
// 定位 Steam
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct SteamInstall {
    pub root: PathBuf,
    pub found_by: String,
}

pub fn find_root(explicit: Option<&str>) -> Result<SteamInstall, String> {
    let mut tried: Vec<String> = Vec::new();

    if let Some(dir) = explicit {
        let p = PathBuf::from(dir);
        if looks_like_steam(&p) {
            return Ok(SteamInstall {
                root: p,
                found_by: "--steam-dir 参数".into(),
            });
        }
        return Err(format!("--steam-dir 指向的目录不像 Steam 安装目录: {}", dir));
    }

    for var in ["STEAM_DIR", "STEAM_ROOT", "STEAM_PATH"] {
        if let Ok(v) = std::env::var(var) {
            if !v.trim().is_empty() {
                let p = PathBuf::from(v.trim());
                if looks_like_steam(&p) {
                    return Ok(SteamInstall {
                        root: p,
                        found_by: format!("环境变量 {}", var),
                    });
                }
                tried.push(format!("{}={}", var, v));
            }
        }
    }

    // 注册表：HKCU\Software\Valve\Steam -> SteamPath
    if let Some(p) = read_registry_steam_path() {
        if looks_like_steam(&p) {
            return Ok(SteamInstall {
                root: p,
                found_by: "注册表 HKCU\\Software\\Valve\\Steam\\SteamPath".into(),
            });
        }
        tried.push(format!("注册表 SteamPath={}", p.display()));
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(pf86) = std::env::var("ProgramFiles(x86)") {
        candidates.push(PathBuf::from(pf86).join("Steam"));
    }
    if let Ok(pf) = std::env::var("ProgramFiles") {
        candidates.push(PathBuf::from(pf).join("Steam"));
    }
    candidates.push(PathBuf::from(r"C:\Program Files (x86)\Steam"));
    candidates.push(PathBuf::from(r"C:\Program Files\Steam"));
    for drive in ["C", "D", "E", "F", "G"] {
        candidates.push(PathBuf::from(format!("{}:\\Steam", drive)));
        candidates.push(PathBuf::from(format!("{}:\\SteamLibrary", drive)));
    }

    for p in &candidates {
        if looks_like_steam(p) {
            return Ok(SteamInstall {
                root: p.clone(),
                found_by: "常见安装路径探测".into(),
            });
        }
    }

    Err(format!(
        "找不到 Steam 安装目录。已尝试：{}。请用 --steam-dir 显式指定。",
        if tried.is_empty() {
            "常见路径".to_string()
        } else {
            tried.join("; ")
        }
    ))
}

fn looks_like_steam(p: &Path) -> bool {
    p.join("steamapps").join("libraryfolders.vdf").is_file() || p.join("steam.exe").is_file()
}

fn read_registry_steam_path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let out = std::process::Command::new("reg")
            .args([
                "query",
                r"HKCU\Software\Valve\Steam",
                "/v",
                "SteamPath",
            ])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            if line.contains("SteamPath") {
                if let Some(idx) = line.find("REG_SZ") {
                    let value = line[idx + "REG_SZ".len()..].trim();
                    if !value.is_empty() {
                        return Some(PathBuf::from(value));
                    }
                }
            }
        }
        None
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Steam 客户端是否正在运行。写入前必须为 false。
pub fn steam_running() -> bool {
    #[cfg(windows)]
    {
        if let Ok(out) = std::process::Command::new("tasklist")
            .args(["/FI", "IMAGENAME eq steam.exe", "/NH"])
            .output()
        {
            let text = String::from_utf8_lossy(&out.stdout).to_lowercase();
            return text.contains("steam.exe");
        }
        false
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("pgrep")
            .args(["-x", "steam"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

// ---------------------------------------------------------------------------
// 库根
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct LibraryRoot {
    pub index: String,
    pub path: PathBuf,
    pub label: String,
    pub total_size: u64,
}

pub fn read_library_roots(steam_root: &Path) -> Result<Vec<LibraryRoot>, String> {
    let file = steam_root.join("steamapps").join("libraryfolders.vdf");
    let table = vdf::parse_file(&file)?;
    let mut roots = Vec::new();
    for (key, value) in &table {
        let Some(entry) = value.as_table() else { continue };
        if !key.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let Some(path) = entry.str_ci("path") else {
            continue;
        };
        roots.push(LibraryRoot {
            index: key.clone(),
            path: PathBuf::from(path),
            label: entry.str_ci("label").unwrap_or("").to_string(),
            total_size: entry.u64_ci("totalsize").unwrap_or(0),
        });
    }
    roots.sort_by_key(|r| {
        r.index
            .parse::<u32>()
            .unwrap_or(u32::MAX)
    });
    if roots.is_empty() {
        return Err(format!(
            "{} 里没有解析出任何库根",
            file.display()
        ));
    }
    Ok(roots)
}

// ---------------------------------------------------------------------------
// appmanifest
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Manifest {
    pub appid: u32,
    pub name: String,
    pub installdir: String,
    pub size_on_disk: u64,
    pub state_flags: u32,
    pub last_updated: Option<i64>,
    pub last_played: Option<i64>,
    pub language: Option<String>,
    pub auto_update: Option<u32>,
}

const STATE_FULLY_INSTALLED: u32 = 4;

impl Manifest {
    pub fn installed(&self) -> bool {
        self.state_flags & STATE_FULLY_INSTALLED != 0
    }
}

/// 读取一个库根下所有 `appmanifest_*.acf`。
pub fn read_manifests(library_root: &Path) -> Vec<Manifest> {
    let dir = library_root.join("steamapps");
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("appmanifest_") || !name.ends_with(".acf") {
            continue;
        }
        let Ok(table) = vdf::parse_file(&path) else {
            continue;
        };
        if let Some(m) = manifest_from_table(&table) {
            out.push(m);
        }
    }
    out.sort_by_key(|m| m.appid);
    out
}

fn manifest_from_table(t: &Table) -> Option<Manifest> {
    let appid: u32 = t.str_ci("appid")?.trim().parse().ok()?;
    let language = t
        .table_ci("UserConfig")
        .and_then(|c| c.str_ci("language"))
        .map(|s| s.to_string());
    Some(Manifest {
        appid,
        name: t.str_ci("name").unwrap_or("").to_string(),
        installdir: t.str_ci("installdir").unwrap_or("").to_string(),
        size_on_disk: t.u64_ci("SizeOnDisk").unwrap_or(0),
        state_flags: t.u64_ci("StateFlags").unwrap_or(0) as u32,
        last_updated: sanitize_epoch(t.i64_ci("LastUpdated")),
        last_played: sanitize_epoch(t.i64_ci("LastPlayed")),
        language,
        auto_update: t.u64_ci("AutoUpdateBehavior").map(|v| v as u32),
    })
}

/// 剔除「伪时间戳」。实测：`LastPlayed = 86400`（1970-01-02）是
/// 「早于 Steam 记录时间戳时代」的哨兵值，不能当成真实日期。
pub fn sanitize_epoch(v: Option<i64>) -> Option<i64> {
    const FLOOR: i64 = 315_532_800; // 1980-01-01
    match v {
        Some(x) if x > FLOOR => Some(x),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// 账号
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Account {
    pub steam3: String,
    pub steamid64: String,
    pub persona: Option<String>,
    /// 注册表 HKCU\Software\Valve\Steam\ActiveProcess\ActiveUser 命中的那个
    pub active: bool,
    pub in_loginusers: bool,
    /// `userdata/<steam3>/config/localconfig.vdf` 是否存在
    pub has_config: bool,
    /// `config/cloudstorage/cloud-storage-namespace-1.json` 是否存在
    pub has_cloud: bool,
    pub login_timestamp: i64,
}

const STEAMID64_BASE: u64 = 76_561_197_960_265_728;

/// 注册表里的「当前使用账号」（steam3 id，十进制）。
///
/// 实测本机 `loginusers.vdf` 里**没有** `MostRecent` 字段，所以这是最可靠的信号。
fn read_registry_active_user() -> Option<String> {
    #[cfg(windows)]
    {
        let out = std::process::Command::new("reg")
            .args([
                r"HKCU\Software\Valve\Steam\ActiveProcess",
                "/v",
                "ActiveUser",
            ])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            if !line.contains("ActiveUser") {
                continue;
            }
            let Some(pos) = line.find("0x") else { continue };
            let hex: String = line[pos + 2..]
                .chars()
                .take_while(|c| c.is_ascii_hexdigit())
                .collect();
            if let Ok(v) = u64::from_str_radix(&hex, 16) {
                if v > 0 {
                    return Some(v.to_string());
                }
            }
        }
        None
    }
    #[cfg(not(windows))]
    {
        None
    }
}

pub fn list_accounts(steam_root: &Path) -> Vec<Account> {
    let active = read_registry_active_user();

    // loginusers.vdf -> steamid64 => (PersonaName, MostRecent, timestamp)
    let mut login_users: HashMap<String, (Option<String>, bool, i64)> = HashMap::new();
    let loginusers = steam_root.join("config").join("loginusers.vdf");
    if let Ok(table) = vdf::parse_file(&loginusers) {
        // 根键是 "users"；不同版本可能不同，两种形状都兼容
        let users: &Table = table.table_ci("users").unwrap_or(&table);
        for (steamid64, value) in users {
            let Some(t) = value.as_table() else { continue };
            login_users.insert(
                steamid64.clone(),
                (
                    t.str_ci("PersonaName").map(|s| s.to_string()),
                    t.u64_ci("MostRecent").unwrap_or(0) == 1,
                    t.i64_ci("timestamp").unwrap_or(0),
                ),
            );
        }
    }

    let userdata = steam_root.join("userdata");
    let mut accounts = Vec::new();
    let Ok(entries) = std::fs::read_dir(&userdata) else {
        return accounts;
    };
    for entry in entries.flatten() {
        let steam3 = entry.file_name().to_string_lossy().into_owned();
        if !steam3.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let steamid64 = (steam3.parse::<u64>().unwrap_or(0) + STEAMID64_BASE).to_string();
        let config_dir = entry.path().join("config");
        let localconfig = config_dir.join("localconfig.vdf");
        let has_config = localconfig.is_file();
        let has_cloud = config_dir
            .join("cloudstorage")
            .join("cloud-storage-namespace-1.json")
            .is_file();

        let (mut persona, mut recent, mut ts) = (None, false, 0i64);
        if let Some((p, r, t)) = login_users.get(&steamid64) {
            persona = p.clone();
            recent = *r;
            ts = *t;
        }
        if persona.is_none() && has_config {
            persona = read_persona(&localconfig);
        }

        accounts.push(Account {
            active: active.as_deref() == Some(steam3.as_str()),
            in_loginusers: login_users.contains_key(&steamid64),
            has_config,
            has_cloud,
            login_timestamp: ts,
            steam3,
            steamid64,
            persona,
        });
        // `recent` 目前只用于兼容旧版 Steam，不参与排序主键
        let _ = recent;
    }

    // 排序：真实在用 > 有配置 > 有合集 > 在 loginusers 里 > 最近登录 > id
    accounts.sort_by(|a, b| {
        b.active
            .cmp(&a.active)
            .then_with(|| b.has_config.cmp(&a.has_config))
            .then_with(|| b.has_cloud.cmp(&a.has_cloud))
            .then_with(|| b.in_loginusers.cmp(&a.in_loginusers))
            .then_with(|| b.login_timestamp.cmp(&a.login_timestamp))
            .then_with(|| a.steam3.cmp(&b.steam3))
    });
    accounts
}

fn read_persona(localconfig: &Path) -> Option<String> {
    let table = vdf::parse_file(localconfig).ok()?;
    let friends = table.table_ci("friends")?;
    friends.str_ci("PersonaName").map(|s| s.to_string())
}

pub fn pick_account(steam_root: &Path, explicit: Option<&str>) -> Result<Account, String> {
    let accounts = list_accounts(steam_root);
    if accounts.is_empty() {
        return Err(format!(
            "{} 下没有任何 userdata 账号目录",
            steam_root.join("userdata").display()
        ));
    }
    if let Some(id) = explicit {
        let wanted = id.trim();
        return accounts
            .iter()
            .find(|a| a.steam3 == wanted || a.steamid64 == wanted)
            .cloned()
            .ok_or_else(|| format!("找不到账号 {}（可用：{}）", wanted, account_list(&accounts)));
    }
    Ok(accounts[0].clone())
}

fn account_list(accounts: &[Account]) -> String {
    accounts
        .iter()
        .map(|a| {
            format!(
                "{}({}{})",
                a.steam3,
                a.persona.clone().unwrap_or_else(|| "?".into()),
                if a.has_cloud { "" } else { ", 无合集文件" }
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// 从 `config/librarycache/<appid>.json` 里取到的元数据。
///
/// 实测这个缓存里**没有游戏名**，但有三类很有用的东西：
///   * `associations` → 官方系列 / 开发商 / 发行商（按系列整理时最有用）
///   * `achievements` → `nTotal` / `nAchieved`，可判断「玩透了没」
///   * `descriptions` → 简短描述
#[derive(Debug, Clone, Default)]
pub struct CachedMeta {
    pub franchises: Vec<String>,
    pub developers: Vec<String>,
    pub publishers: Vec<String>,
    pub achievements_total: u32,
    pub achievements_unlocked: u32,
    pub snippet: String,
}

/// 只读取 `wanted` 里那些 appid 的缓存文件，避免为几千个未安装游戏做无谓 IO。
pub fn read_library_cache(
    steam_root: &Path,
    steam3: &str,
    wanted: &std::collections::HashSet<u32>,
) -> HashMap<u32, CachedMeta> {
    let dir = steam_root
        .join("userdata")
        .join(steam3)
        .join("config")
        .join("librarycache");
    let mut out = HashMap::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name().to_string_lossy().into_owned();
        let Some(stem) = file_name.strip_suffix(".json") else {
            continue;
        };
        let Ok(appid) = stem.parse::<u32>() else { continue };
        if !wanted.contains(&appid) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let Some(pairs) = value.as_array() else { continue };

        let mut meta = CachedMeta::default();
        for pair in pairs {
            let Some(p) = pair.as_array().filter(|p| p.len() == 2) else {
                continue;
            };
            let key = p[0].as_str().unwrap_or("");
            let data = p[1].get("data").unwrap_or(&Value::Null);
            match key {
                "associations" => {
                    meta.franchises = named_list(data.get("rgFranchises"));
                    meta.developers = named_list(data.get("rgDevelopers"));
                    meta.publishers = named_list(data.get("rgPublishers"));
                }
                "achievements" => {
                    meta.achievements_total =
                        data.get("nTotal").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
                    meta.achievements_unlocked =
                        data.get("nAchieved").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
                }
                "descriptions" => {
                    meta.snippet = data
                        .get("strSnippet")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .trim()
                        .to_string();
                }
                _ => {}
            }
        }
        if meta.franchises.is_empty()
            && meta.developers.is_empty()
            && meta.achievements_total == 0
            && meta.snippet.is_empty()
        {
            continue;
        }
        out.insert(appid, meta);
    }
    out
}

/// 这些列表的元素形如 `{"strName":"...","strURL":...}`，但也容忍直接是字符串。
fn named_list(v: Option<&Value>) -> Vec<String> {
    let Some(arr) = v.and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for item in arr {
        let name = item
            .as_str()
            .map(|s| s.to_string())
            .or_else(|| {
                item.as_object().and_then(|o| {
                    ["strName", "name", "strLocalizedName"]
                        .iter()
                        .find_map(|k| o.get(*k).and_then(|x| x.as_str()))
                        .map(|s| s.to_string())
                })
            })
            .unwrap_or_default();
        let name = name.trim().to_string();
        if !name.is_empty() && !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// localconfig：游玩时长
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct PlayStats {
    pub playtime_minutes: u64,
    pub playtime_2wks_minutes: u64,
    pub last_played: Option<i64>,
}

/// 读取 `UserLocalConfigStore/Software/Valve/Steam/apps/<appid>`。
///
/// 不能随便找「第一个以 appid 为键的节点」——`apptickets`、`depots`、
/// `UserAppConfig` 都是同样的形状。
pub fn read_playtime(localconfig: &Path) -> Result<HashMap<u32, PlayStats>, String> {
    let table = vdf::parse_file(localconfig)?;
    let mut out = HashMap::new();
    let apps = table
        .table_ci("Software")
        .and_then(|t| t.table_ci("Valve"))
        .and_then(|t| t.table_ci("Steam"))
        .and_then(|t| t.table_ci("apps"));
    let Some(apps) = apps else {
        return Ok(out);
    };
    for (key, value) in apps {
        let Ok(appid) = key.trim().parse::<u32>() else {
            continue;
        };
        let Some(t) = value.as_table() else { continue };
        let playtime = t.u64_ci("Playtime").unwrap_or(0);
        let playtime2 = t.u64_ci("Playtime2wks").unwrap_or(0);
        let last = sanitize_epoch(t.i64_ci("LastPlayed"));
        // 只带 cloud 块、没有任何游玩信息的条目直接跳过，避免输出一堆 0
        if playtime == 0 && playtime2 == 0 && last.is_none() {
            continue;
        }
        out.insert(
            appid,
            PlayStats {
                playtime_minutes: playtime,
                playtime_2wks_minutes: playtime2,
                last_played: last,
            },
        );
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// cloudstorage：新版合集
// ---------------------------------------------------------------------------

pub const KEY_PREFIX: &str = "user-collections.";
pub const BUILTIN_IDS: [&str; 2] = ["favorite", "hidden"];

#[derive(Debug, Clone)]
pub struct Collection {
    pub id: String,
    pub key: String,
    pub name: String,
    pub added: Vec<u32>,
    pub removed: Vec<u32>,
    pub is_dynamic: bool,
    pub is_builtin: bool,
    pub is_deleted: bool,
    pub version: i64,
}

impl Collection {
    /// 只有「非内置、非动态、未删除」的合集才是本工具可以改写的。
    pub fn editable(&self) -> bool {
        !self.is_deleted && !self.is_dynamic && !self.is_builtin
    }
}

#[derive(Debug, Clone)]
pub struct CloudStore {
    pub namespace_path: PathBuf,
    pub namespaces_path: PathBuf,
    pub raw: Vec<Value>,
    /// 命名空间 1 的当前版本号
    pub namespace_version: i64,
    /// 文件里出现过的最大条目版本号
    pub max_entry_version: i64,
    pub collections: Vec<Collection>,
}

impl CloudStore {
    pub fn by_name(&self, name: &str) -> Option<&Collection> {
        self.collections
            .iter()
            .find(|c| !c.is_deleted && c.name == name)
    }

    pub fn live(&self) -> Vec<&Collection> {
        self.collections.iter().filter(|c| !c.is_deleted).collect()
    }
}

pub fn cloudstorage_dir(steam_root: &Path, steam3: &str) -> PathBuf {
    steam_root
        .join("userdata")
        .join(steam3)
        .join("config")
        .join("cloudstorage")
}

pub fn read_cloud(steam_root: &Path, steam3: &str) -> Result<CloudStore, String> {
    let dir = cloudstorage_dir(steam_root, steam3);
    let namespace_path = dir.join("cloud-storage-namespace-1.json");
    let namespaces_path = dir.join("cloud-storage-namespaces.json");

    if !namespace_path.is_file() {
        return Err(format!(
            "找不到合集文件 {}。\n\
             该文件由 Steam 客户端在首次进入「库」界面时创建；请先启动一次 Steam 再重试。",
            namespace_path.display()
        ));
    }

    let text = std::fs::read_to_string(&namespace_path)
        .map_err(|e| format!("读取 {} 失败: {}", namespace_path.display(), e))?;
    let parsed: Value = serde_json::from_str(&text).map_err(|e| {
        format!(
            "解析 {} 失败: {}（文件可能正被 Steam 写入，请先完全退出 Steam）",
            namespace_path.display(),
            e
        )
    })?;
    let raw = parsed
        .as_array()
        .ok_or_else(|| "合集文件顶层不是数组".to_string())?
        .clone();

    let namespace_version = read_namespace_version(&namespaces_path, 1).unwrap_or(0);

    let mut collections = Vec::new();
    let mut max_entry_version = 0i64;
    for item in &raw {
        let pair = item.as_array().filter(|a| a.len() == 2);
        let Some(pair) = pair else { continue };
        let Some(entry) = pair[1].as_object() else {
            continue;
        };
        let key = entry
            .get("key")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let version = entry
            .get("version")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);
        if version > max_entry_version {
            max_entry_version = version;
        }
        let Some(id) = key.strip_prefix(KEY_PREFIX) else {
            continue;
        };
        let is_deleted = entry
            .get("is_deleted")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if is_deleted {
            collections.push(Collection {
                id: id.to_string(),
                key: key.clone(),
                name: String::new(),
                added: Vec::new(),
                removed: Vec::new(),
                is_dynamic: false,
                is_builtin: BUILTIN_IDS.contains(&id),
                is_deleted: true,
                version,
            });
            continue;
        }
        let value_str = entry.get("value").and_then(|v| v.as_str()).unwrap_or("");
        let Ok(value) = serde_json::from_str::<Value>(value_str) else {
            continue;
        };
        let name = value
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let added = appid_list(value.get("added"));
        let removed = appid_list(value.get("removed"));
        let is_dynamic = value.get("filterSpec").map(|v| !v.is_null()).unwrap_or(false);
        collections.push(Collection {
            id: id.to_string(),
            key: key.clone(),
            name,
            added,
            removed,
            is_dynamic,
            is_builtin: BUILTIN_IDS.contains(&id),
            is_deleted: false,
            version,
        });
    }

    Ok(CloudStore {
        namespace_path,
        namespaces_path,
        raw,
        namespace_version,
        max_entry_version,
        collections,
    })
}

fn appid_list(v: Option<&Value>) -> Vec<u32> {
    let Some(arr) = v.and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut out: Vec<u32> = arr
        .iter()
        .filter_map(|x| {
            x.as_u64()
                .map(|n| n as u32)
                .or_else(|| x.as_str().and_then(|s| s.parse::<u32>().ok()))
        })
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// `cloud-storage-namespaces.json` 形如 `[[3,"0"],[1,"7481"]]`。
pub fn read_namespace_version(path: &Path, namespace: u64) -> Option<i64> {
    let text = std::fs::read_to_string(path).ok()?;
    let parsed: Value = serde_json::from_str(&text).ok()?;
    for item in parsed.as_array()? {
        let pair = item.as_array()?;
        if pair.len() != 2 {
            continue;
        }
        if pair[0].as_u64() == Some(namespace) {
            if let Some(s) = pair[1].as_str() {
                if let Ok(v) = s.parse::<i64>() {
                    return Some(v);
                }
            }
            if let Some(v) = pair[1].as_i64() {
                return Some(v);
            }
        }
    }
    None
}
