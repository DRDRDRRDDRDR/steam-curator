//! `steam-curator/tests/` 下各集成测试共用的工具。
//!
//! # 写盘安全约定（硬性）
//!
//! * 所有写盘只发生在**带唯一名字的一次性沙箱目录**里，测试结束递归删除；
//! * 沙箱根目录优先取 `std::env::temp_dir()`；若运行环境禁止写工作区以外
//!   （DSH 沙箱下 `%TEMP%` 会返回 `Os error 5 拒绝访问`），自动退回
//!   `<crate>/target/test-scratch/`——两者都在工作区之外/构建产物目录内，
//!   与真实 Steam 无关；
//! * 任何沙箱路径创建时都要过 [`assert_not_real_steam`]，
//!   绝不会写入 `C:\Program Files (x86)\Steam` 之类的真实安装目录。
//!
//! # 其它约定
//!
//! * `ApplyOptions.force` 一律置 `true`：测试必须**确定性**，不能取决于本机
//!   「此刻 Steam 是否在运行」；写入目标是假沙箱，绕过该保护没有副作用。
//! * 读取写回结果时用 [`entry_object`]，它**同时接受** `[key, entry]` 二元组
//!   与裸对象两种形状，以免「形状」这一条不变量把别的断言一起带红。
//!   形状本身由 `apply_test.rs` 里的 `repro_known_bug_written_entries_are_key_entry_pairs`
//!   专门把关。

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use serde_json::Value;
use steam_curator::apply::{run, ApplyOutcome, ApplyOptions};
use steam_curator::model::*;

// ---------------------------------------------------------------------------
// 沙箱目录
// ---------------------------------------------------------------------------

static SEQ: AtomicU64 = AtomicU64::new(0);

/// 真实 Steam 安装前缀黑名单（小写、反斜杠归一）。
const REAL_STEAM_PREFIXES: [&str; 2] = [
    r"c:\program files (x86)\steam",
    r"c:\program files\steam",
];

fn normalize_path(p: &Path) -> String {
    p.to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase()
}

/// 硬安全闸：测试目录绝不允许落在真实 Steam 目录内，命中即 panic。
pub fn assert_not_real_steam(p: &Path) {
    let s = normalize_path(p);
    for prefix in REAL_STEAM_PREFIXES {
        assert!(
            !s.starts_with(prefix),
            "拒绝执行：测试目录落在真实 Steam 安装目录内 —— {}",
            p.display()
        );
    }
}

fn can_write(dir: &Path) -> bool {
    let probe = dir.join(format!(".probe-{}", std::process::id()));
    if std::fs::create_dir_all(&probe).is_err() {
        return false;
    }
    let ok = std::fs::write(probe.join("t"), b"1").is_ok();
    let _ = std::fs::remove_dir_all(&probe);
    ok
}

/// 可写的沙箱根目录（进程内只判定一次）。
pub fn scratch_root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let candidates = [
            // 首选：系统临时目录（沙箱宽松时走这条）
            std::env::temp_dir().join("steam-curator-tests"),
            // 兜底：crate 内的构建产物目录（DSH 沙箱只允许写工作区）
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .join("test-scratch"),
        ];
        for c in candidates {
            if can_write(&c) {
                assert_not_real_steam(&c);
                eprintln!("[steam-curator tests] 沙箱根目录 = {}", c.display());
                return c;
            }
        }
        panic!("找不到可写的沙箱根目录（%TEMP% 与 target/ 均不可写）");
    })
}

/// 一次性沙箱目录；`Drop` 时递归删除自己这一层。
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new(tag: &str) -> TempDir {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let path = scratch_root().join(format!(
            "{}-{}-{}-{}",
            tag,
            std::process::id(),
            seq,
            nanos
        ));
        std::fs::create_dir_all(&path).expect("创建沙箱目录失败");
        assert_not_real_steam(&path);
        TempDir { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 取一个子目录（自动创建）。
    pub fn dir(&self, name: &str) -> PathBuf {
        let p = self.path.join(name);
        std::fs::create_dir_all(&p).expect("创建沙箱子目录失败");
        p
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

// ---------------------------------------------------------------------------
// 断言工具（含自证）
// ---------------------------------------------------------------------------

/// 目录快照：相对路径 -> 文件字节。
pub type Tree = BTreeMap<String, Vec<u8>>;

/// 递归读取一个目录下所有普通文件的字节。
pub fn snapshot_tree(dir: &Path) -> Tree {
    fn walk(base: &Path, dir: &Path, out: &mut Tree) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                walk(base, &p, out);
            } else if let Ok(bytes) = std::fs::read(&p) {
                let rel = p
                    .strip_prefix(base)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, bytes);
            }
        }
    }
    let mut out = Tree::new();
    walk(dir, dir, &mut out);
    out
}

/// 断言两棵目录树完全一致，失败时报出每个差异文件。
pub fn assert_tree_eq(label: &str, before: &Tree, after: &Tree) {
    let mut problems: Vec<String> = Vec::new();
    for (name, bytes) in before {
        match after.get(name) {
            None => problems.push(format!("  文件消失: {}", name)),
            Some(other) if other != bytes => problems.push(format!(
                "  内容变化: {} ({} -> {} 字节)",
                name,
                bytes.len(),
                other.len()
            )),
            _ => {}
        }
    }
    for name in after.keys() {
        if !before.contains_key(name) {
            problems.push(format!("  多出新文件: {}", name));
        }
    }
    assert!(
        problems.is_empty(),
        "{}：目录内容不一致\n{}",
        label,
        problems.join("\n")
    );
}

/// 断言两个字节串完全一致。
pub fn assert_bytes_eq(label: &str, expected: &[u8], actual: &[u8]) {
    assert!(
        expected == actual,
        "{}：字节不一致（期望 {} 字节，实际 {} 字节）\n期望: {}\n实际: {}",
        label,
        expected.len(),
        actual.len(),
        String::from_utf8_lossy(expected),
        String::from_utf8_lossy(actual)
    );
}

/// 自证：断言工具不是空壳——拿两个不同的字节串去比，**必须** panic。
///
/// 这个测试本身永远跑（在 plan_test 与 apply_test 两个 target 里各跑一次），
/// 用来证明整套测试里的 `assert_bytes_eq` 真的会红。
#[test]
fn assertion_helpers_actually_reject_mismatches() {
    let left = b"alpha".to_vec();
    let right = b"beta".to_vec();
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_bytes_eq("自证", &left, &right);
    }));
    assert!(
        caught.is_err(),
        "assert_bytes_eq 对不同输入没有报错 —— 断言是空壳，整套测试都不可信"
    );
}

// ---------------------------------------------------------------------------
// 模型构造
// ---------------------------------------------------------------------------

pub fn game(appid: u32, name: &str) -> Game {
    Game {
        appid,
        name: name.to_string(),
        installdir: format!("app{}", appid),
        library: "C:\\fake-library".into(),
        size_bytes: 0,
        state_flags: 4,
        installed: true,
        last_updated: None,
        language: None,
        auto_update: None,
        franchises: Vec::new(),
        developers: Vec::new(),
        publishers: Vec::new(),
        achievements_total: 0,
        achievements_unlocked: 0,
        playtime_minutes: 0,
        playtime_2wks_minutes: 0,
        last_played: None,
        last_played_local: None,
        collections: Vec::new(),
        hidden: false,
        favorite: false,
        never_launched: true,
        days_since_played: None,
        barely_played: false,
        dormant: false,
    }
}

pub fn game_with(appid: u32, name: &str, playtime_minutes: u64, size_bytes: u64) -> Game {
    let mut g = game(appid, name);
    g.playtime_minutes = playtime_minutes;
    g.size_bytes = size_bytes;
    g
}

pub fn empty_library() -> Library {
    Library {
        schema: LIBRARY_SCHEMA.to_string(),
        scanned_at: 1_700_000_000,
        scanned_at_local: "2023-11-14 00:00".into(),
        steam_root: "C:\\fake-steam".into(),
        account: AccountInfo::default(),
        library_roots: Vec::new(),
        games: Vec::new(),
        existing_collections: Vec::new(),
        warnings: Vec::new(),
    }
}

pub fn library(games: Vec<Game>, existing: Vec<CollectionInfo>) -> Library {
    Library {
        games,
        existing_collections: existing,
        ..empty_library()
    }
}

pub fn user_collection(id: &str, name: &str, appids: &[u32]) -> CollectionInfo {
    CollectionInfo {
        id: id.to_string(),
        name: name.to_string(),
        appids: appids.to_vec(),
        is_builtin: false,
        is_dynamic: false,
        is_deleted: false,
    }
}

pub fn dynamic_collection(id: &str, name: &str, appids: &[u32]) -> CollectionInfo {
    CollectionInfo {
        is_dynamic: true,
        ..user_collection(id, name, appids)
    }
}

pub fn builtin_collection(id: &str, name: &str, appids: &[u32]) -> CollectionInfo {
    CollectionInfo {
        is_builtin: true,
        ..user_collection(id, name, appids)
    }
}

pub fn plan_collection(name: &str, appids: &[u32]) -> PlanCollection {
    PlanCollection {
        name: name.to_string(),
        description: None,
        appids: appids.to_vec(),
        existing: false,
        kept_from_existing: 0,
    }
}

pub fn plan_of(collections: Vec<PlanCollection>) -> Plan {
    Plan {
        schema: PLAN_SCHEMA.to_string(),
        created_at: 1_700_000_000,
        created_at_local: "2023-11-14 00:00".into(),
        source: "test-plan".into(),
        collections,
        unassigned: Vec::new(),
        stats: PlanStats::default(),
        issues: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// plan 断言辅助
// ---------------------------------------------------------------------------

pub fn has_issue(plan: &Plan, needle: &str) -> bool {
    plan.issues.iter().any(|i| i.message.contains(needle))
}

pub fn has_issue_at(plan: &Plan, level: &str, needle: &str) -> bool {
    plan.issues
        .iter()
        .any(|i| i.level == level && i.message.contains(needle))
}

pub fn count_issues_at(plan: &Plan, level: &str, needle: &str) -> usize {
    plan.issues
        .iter()
        .filter(|i| i.level == level && i.message.contains(needle))
        .count()
}

pub fn issue_dump(plan: &Plan) -> String {
    plan.issues
        .iter()
        .map(|i| format!("[{}] {}", i.level, i.message))
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------
// cloudstorage 假数据
// ---------------------------------------------------------------------------

pub const STEAM3: &str = "111111111";

/// 标准 fixtures 的命名空间版本（900）与最大条目版本（700）刻意拉开，
/// 便于断言「新条目版本 > 旧命名空间版本」。
pub const FIXTURE_NAMESPACES: &str = r#"[[3,"0"],[1,"900"]]"#;
pub const FIXTURE_NAMESPACE_VERSION: i64 = 900;
pub const FIXTURE_MAX_ENTRY_VERSION: i64 = 700;

pub const KEY_EXISTING_RPG: &str = "user-collections.uc-ExistingAA";
pub const KEY_HIDDEN: &str = "user-collections.hidden";
pub const KEY_DYNAMIC: &str = "user-collections.uc-DynamicAAA";
pub const KEY_TOMBSTONE: &str = "user-collections.uc-DeadTombst";
pub const KEY_SHOWCASE: &str = "showcases.76561198000000000";
pub const KEY_ROLLUP: &str = "NewContentRollup_440";

fn json_u32_list(v: &[u32]) -> String {
    format!(
        "[{}]",
        v.iter().map(|a| a.to_string()).collect::<Vec<_>>().join(",")
    )
}

/// 静态合集 `value` 的内容：`{"id":..,"name":..,"added":[..],"removed":[]}`。
pub fn static_collection_value(id: &str, name: &str, added: &[u32]) -> String {
    format!(
        r#"{{"id":{},"name":{},"added":{},"removed":[]}}"#,
        serde_json::to_string(id).unwrap(),
        serde_json::to_string(name).unwrap(),
        json_u32_list(added)
    )
}

/// 动态合集（带 filterSpec）的 `value` 内容。
pub fn dynamic_collection_value(id: &str, name: &str) -> String {
    format!(
        r#"{{"id":{},"name":{},"added":[],"removed":[],"filterSpec":{{"nFormatVersion":2}}}}"#,
        serde_json::to_string(id).unwrap(),
        serde_json::to_string(name).unwrap()
    )
}

/// Steam 真实形状的条目：`["<key>",{...}]`；`value = None` 时造 tombstone。
pub fn pair_entry(key: &str, value: Option<&str>, version: i64) -> String {
    let payload = match value {
        Some(v) => format!("\"value\":{},", serde_json::to_string(v).unwrap()),
        None => "\"is_deleted\":true,".to_string(),
    };
    format!(r#"["{key}",{{"key":"{key}","timestamp":1700000000,{payload}"version":"{version}"}}]"#)
}

/// 把条目按 key 排序后拼成文件（与 `apply` 写回后的顺序一致）。
pub fn cloud_file(mut entries: Vec<(&str, String)>) -> String {
    entries.sort_by(|a, b| a.0.cmp(b.0));
    format!(
        "[\n{}\n]",
        entries
            .iter()
            .map(|(_, json)| json.clone())
            .collect::<Vec<_>>()
            .join(",\n")
    )
}

/// 标准假 cloudstorage：内置合集 / 动态合集 / tombstone / showcases / NewContentRollup
/// / 一个名为 RPG 的静态合集（added = [100,200]）都在里面。
pub fn standard_fixture_cloud() -> String {
    cloud_file(vec![
        (
            KEY_ROLLUP,
            pair_entry(KEY_ROLLUP, Some(r#"{"rollup":true}"#), 120),
        ),
        (
            KEY_SHOWCASE,
            pair_entry(KEY_SHOWCASE, Some(r#"{"showcase":1}"#), 400),
        ),
        (
            KEY_HIDDEN,
            pair_entry(
                KEY_HIDDEN,
                Some(&static_collection_value("hidden", "已隐藏", &[200])),
                700,
            ),
        ),
        (KEY_TOMBSTONE, pair_entry(KEY_TOMBSTONE, None, 300)),
        (
            KEY_DYNAMIC,
            pair_entry(
                KEY_DYNAMIC,
                Some(&dynamic_collection_value("uc-DynamicAAA", "动态测试")),
                450,
            ),
        ),
        (
            KEY_EXISTING_RPG,
            pair_entry(
                KEY_EXISTING_RPG,
                Some(&static_collection_value("uc-ExistingAA", "RPG", &[100, 200])),
                500,
            ),
        ),
    ])
}

pub fn cloudstorage_dir(steam_root: &Path) -> PathBuf {
    steam_curator::steam::cloudstorage_dir(steam_root, STEAM3)
}

/// 造一份假 cloudstorage；返回 cloudstorage 目录。
pub fn write_cloud(steam_root: &Path, cloud: &str, namespaces: Option<&str>) -> PathBuf {
    let cs = cloudstorage_dir(steam_root);
    std::fs::create_dir_all(&cs).unwrap();
    std::fs::write(cs.join("cloud-storage-namespace-1.json"), cloud).unwrap();
    if let Some(ns) = namespaces {
        std::fs::write(cs.join("cloud-storage-namespaces.json"), ns).unwrap();
    }
    cs
}

/// 一个完整的假环境：steam 根目录 + out 目录 + cloudstorage。
pub struct Sandbox {
    pub tmp: TempDir,
    pub steam_root: PathBuf,
    pub out_dir: PathBuf,
    pub cs: PathBuf,
}

pub fn sandbox(tag: &str, cloud: &str, namespaces: Option<&str>) -> Sandbox {
    let tmp = TempDir::new(tag);
    let steam_root = tmp.dir("steam");
    let out_dir = tmp.dir("out");
    let cs = write_cloud(&steam_root, cloud, namespaces);
    Sandbox {
        tmp,
        steam_root,
        out_dir,
        cs,
    }
}

pub fn standard_sandbox(tag: &str) -> Sandbox {
    sandbox(tag, &standard_fixture_cloud(), Some(FIXTURE_NAMESPACES))
}

// ---------------------------------------------------------------------------
// 读回写回结果的辅助
// ---------------------------------------------------------------------------

pub fn read_json(path: &Path) -> Value {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("读取 {} 失败: {}", path.display(), e));
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("解析 {} 失败: {}\n原文: {}", path.display(), e, text))
}

/// 读取 namespace-1 文件的顶层数组。
pub fn cloud_entries(cs: &Path) -> Vec<Value> {
    read_json(&cs.join("cloud-storage-namespace-1.json"))
        .as_array()
        .expect("namespace-1 文件顶层不是数组")
        .clone()
}

/// 兼容两种形状取条目对象：`[key, entry]` 二元组 **或** 裸对象。
pub fn entry_object<'a>(raw: &'a [Value], key: &str) -> Option<&'a Value> {
    raw.iter().find_map(|item| {
        if let Some(pair) = item.as_array() {
            if pair.len() == 2 && pair[0].as_str() == Some(key) {
                return Some(&pair[1]);
            }
        }
        let obj = item.as_object()?;
        if obj.get("key").and_then(|v| v.as_str()) == Some(key) {
            Some(item)
        } else {
            None
        }
    })
}

pub fn entry_version(raw: &[Value], key: &str) -> Option<i64> {
    entry_object(raw, key)?
        .get("version")?
        .as_str()?
        .parse::<i64>()
        .ok()
}

/// 解析条目里的 `value`（它本身是字符串化的 JSON）。
pub fn collection_value(raw: &[Value], key: &str) -> Option<Value> {
    let entry = entry_object(raw, key)?;
    let text = entry.get("value")?.as_str()?;
    serde_json::from_str(text).ok()
}

pub fn value_added(value: &Value) -> Vec<u32> {
    value
        .get("added")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_u64().map(|n| n as u32))
                .collect()
        })
        .unwrap_or_default()
}

pub fn value_removed(value: &Value) -> Vec<u32> {
    value
        .get("removed")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_u64().map(|n| n as u32))
                .collect()
        })
        .unwrap_or_default()
}

pub fn namespace_version(cs: &Path, namespace: u64) -> Option<i64> {
    steam_curator::steam::read_namespace_version(
        &cs.join("cloud-storage-namespaces.json"),
        namespace,
    )
}

// ---------------------------------------------------------------------------
// apply 调用辅助
// ---------------------------------------------------------------------------

/// 统一的 `apply::run` 包装。`force = true` 见文件头说明。
pub fn apply_run(
    steam_root: &Path,
    out_dir: &Path,
    plan: &Plan,
    mode: &str,
    write: bool,
    prune: bool,
) -> ApplyOutcome {
    let opts = ApplyOptions {
        write,
        force: true,
        mode: mode.to_string(),
        prune,
    };
    run(plan, steam_root, STEAM3, out_dir, &opts)
        .unwrap_or_else(|e| panic!("apply::run 失败: {}", e))
}

/// 不 unwrap 的版本，供「必须失败」的测试使用。
pub fn try_apply_run(
    steam_root: &Path,
    out_dir: &Path,
    plan: &Plan,
    mode: &str,
    write: bool,
    prune: bool,
) -> Result<ApplyOutcome, String> {
    let opts = ApplyOptions {
        write,
        force: true,
        mode: mode.to_string(),
        prune,
    };
    run(plan, steam_root, STEAM3, out_dir, &opts)
}

pub fn try_apply_write(
    sb: &Sandbox,
    plan: &Plan,
    mode: &str,
) -> Result<ApplyOutcome, String> {
    try_apply_run(&sb.steam_root, &sb.out_dir, plan, mode, true, false)
}

pub fn apply_dry(sb: &Sandbox, plan: &Plan) -> ApplyOutcome {
    apply_run(&sb.steam_root, &sb.out_dir, plan, "merge", false, false)
}

pub fn apply_write(sb: &Sandbox, plan: &Plan, mode: &str) -> ApplyOutcome {
    apply_run(&sb.steam_root, &sb.out_dir, plan, mode, true, false)
}

pub fn apply_write_at(
    steam_root: &Path,
    out_dir: &Path,
    plan: &Plan,
    mode: &str,
) -> ApplyOutcome {
    apply_run(steam_root, out_dir, plan, mode, true, false)
}

/// 已知 bug 复现的开关（见 `apply_test.rs` 顶部说明）。
pub fn repro_known_bug_enabled() -> bool {
    std::env::var("STEAM_CURATOR_REPRO_KNOWN_BUG")
        .map(|v| v == "1")
        .unwrap_or(false)
}
