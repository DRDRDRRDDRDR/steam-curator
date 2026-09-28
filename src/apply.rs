//! `apply` / `restore`：把整理方案写回 Steam，并且随时能完整回滚。
//!
//! 写入目标（新版 Steam 的权威位置）：
//!   `<steam>/userdata/<steam3>/config/cloudstorage/cloud-storage-namespace-1.json`
//!   `<steam>/userdata/<steam3>/config/cloudstorage/cloud-storage-namespaces.json`
//!
//! 安全约定（全部是刻意的设计，不要"顺手优化"掉）：
//!   * 默认 dry-run，只有 `--write` 才落盘；
//!   * Steam 正在运行时拒绝写入（Steam 退出时会用内存状态覆盖这个文件）；
//!   * 写前把两个文件完整备份到 `<out>/backups/<时间戳>/`；
//!   * 只增删改 `user-collections.` 开头、且属于本方案的条目；
//!     其它条目（showcases、NewContentRollup、tombstone、别的命名空间）原样保留；
//!   * 动态合集（带 filterSpec）与系统合集（favorite/hidden）一律不碰；
//!   * 结果与现状一致时不改版本号，重复执行不会反复抬版本。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::model::Plan;
use crate::steam;
use crate::util;

pub struct ApplyOptions {
    pub write: bool,
    pub force: bool,
    /// merge：保留已有同名合集的原有成员；replace：以方案为准完全替换
    pub mode: String,
    /// 删除本工具此前管理、但当前方案中已不存在的合集
    pub prune: bool,
}

impl Default for ApplyOptions {
    fn default() -> Self {
        ApplyOptions {
            write: false,
            force: false,
            mode: "merge".into(),
            prune: false,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Action {
    /// create | update | unchanged | delete | skip
    pub kind: String,
    pub name: String,
    pub id: String,
    pub key: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added: Vec<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added_delta: Vec<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kept_from_existing: Vec<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dropped: Vec<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ApplyOutcome {
    pub dry_run: bool,
    pub blocked_reason: Option<String>,
    pub namespace_path: String,
    pub namespaces_path: String,
    pub old_namespace_version: i64,
    pub new_namespace_version: i64,
    pub backup_dir: Option<String>,
    pub actions: Vec<Action>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct State {
    pub managed: Vec<Managed>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Managed {
    pub id: String,
    pub name: String,
    pub updated_at: i64,
}

pub fn state_path(out_dir: &Path) -> PathBuf {
    out_dir.join("state.json")
}

pub fn load_state(out_dir: &Path) -> State {
    std::fs::read_to_string(state_path(out_dir))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_state(out_dir: &Path, state: &State) -> Result<(), String> {
    let text = serde_json::to_string_pretty(state).map_err(|e| e.to_string())?;
    util::write_file(&state_path(out_dir), &text)
}

/// 分析与（可选的）执行。`write=false` 时不会碰任何文件。
pub fn run(
    plan: &Plan,
    steam_root: &Path,
    steam3: &str,
    out_dir: &Path,
    opts: &ApplyOptions,
) -> Result<ApplyOutcome, String> {
    let cloud = steam::read_cloud(steam_root, steam3)?;
    let mut outcome = ApplyOutcome {
        dry_run: !opts.write,
        namespace_path: cloud.namespace_path.to_string_lossy().into_owned(),
        namespaces_path: cloud.namespaces_path.to_string_lossy().into_owned(),
        old_namespace_version: cloud.namespace_version,
        ..Default::default()
    };

    if opts.write {
        if !opts.force && steam::steam_running() {
            outcome.blocked_reason = Some(
                "Steam 正在运行：退出时会用内存里的状态覆盖合集文件。请先完全退出 Steam（含托盘图标），或加 --force 强行写入。"
                    .into(),
            );
            return Ok(outcome);
        }
    }

    let now = util::now_epoch();
    let mut version = cloud.namespace_version.max(cloud.max_entry_version) + 1;
    let mut state = load_state(out_dir);

    // 现有静态合集：name -> Collection
    let by_name: std::collections::BTreeMap<String, &steam::Collection> = cloud
        .collections
        .iter()
        .filter(|c| !c.is_deleted && !c.is_builtin)
        .map(|c| (c.name.clone(), c))
        .collect();

    let mut new_entries: Vec<Value> = Vec::new();
    let mut replaced_keys: Vec<String> = Vec::new();
    let mut plan_names: Vec<String> = Vec::new();

    for pc in &plan.collections {
        plan_names.push(pc.name.clone());

        let existing = by_name.get(&pc.name).copied();

        if let Some(e) = existing {
            if e.is_dynamic {
                outcome.actions.push(Action {
                    kind: "skip".into(),
                    name: pc.name.clone(),
                    id: e.id.clone(),
                    key: e.key.clone(),
                    note: Some(
                        "Steam 里已有同名「动态合集」（由过滤器自动生成），绝不覆盖。请在方案里改名。"
                            .into(),
                    ),
                    ..Default::default()
                });
                continue;
            }
        }

        let planned: std::collections::BTreeSet<u32> = pc.appids.iter().copied().collect();
        let (final_added, kept, dropped) = if opts.mode == "replace" {
            let prev: std::collections::BTreeSet<u32> =
                existing.map(|e| e.added.iter().copied().collect()).unwrap_or_default();
            let dropped: Vec<u32> = prev.difference(&planned).copied().collect();
            (planned.iter().copied().collect::<Vec<u32>>(), Vec::new(), dropped)
        } else {
            let mut union: std::collections::BTreeSet<u32> = planned.clone();
            let prev: std::collections::BTreeSet<u32> =
                existing.map(|e| e.added.iter().copied().collect()).unwrap_or_default();
            union.extend(prev.iter().copied());
            let kept: Vec<u32> = prev.difference(&planned).copied().collect();
            (union.into_iter().collect(), kept, Vec::new())
        };

        let removed = existing.map(|e| e.removed.clone()).unwrap_or_default();

        // 幂等：结果没变就不动它
        if let Some(e) = existing {
            if e.added == final_added && e.removed == removed {
                outcome.actions.push(Action {
                    kind: "unchanged".into(),
                    name: pc.name.clone(),
                    id: e.id.clone(),
                    key: e.key.clone(),
                    added: final_added.clone(),
                    note: Some("已与现状一致，跳过（不抬版本号）".into()),
                    ..Default::default()
                });
                continue;
            }
        }

        let id = existing
            .map(|e| e.id.clone())
            .unwrap_or_else(new_collection_id);
        let key = format!("{}{}", steam::KEY_PREFIX, id);

        let added_delta: Vec<u32> = match existing {
            Some(e) => {
                let prev: std::collections::BTreeSet<u32> = e.added.iter().copied().collect();
                final_added
                    .iter()
                    .copied()
                    .filter(|a| !prev.contains(a))
                    .collect()
            }
            None => final_added.clone(),
        };

        outcome.actions.push(Action {
            kind: if existing.is_some() { "update".into() } else { "create".into() },
            name: pc.name.clone(),
            id: id.clone(),
            key: key.clone(),
            added: final_added.clone(),
            added_delta,
            kept_from_existing: kept,
            dropped,
            note: pc.description.clone(),
        });

        replaced_keys.push(key.clone());
        new_entries.push(make_entry(
            &key,
            &build_value_json(&id, &pc.name, &final_added, &removed),
            now,
            version,
        ));
        version += 1;

        // 记录到 state，供 prune 精确清理
        if let Some(slot) = state.managed.iter_mut().find(|m| m.id == id) {
            slot.name = pc.name.clone();
            slot.updated_at = now;
        } else {
            state.managed.push(Managed {
                id: id.clone(),
                name: pc.name.clone(),
                updated_at: now,
            });
        }
    }

    // prune：本工具管过、但已不在方案里的合集
    let mut pruned_keys: Vec<String> = Vec::new();
    if opts.prune {
        for m in &state.managed {
            if plan_names.contains(&m.name) && replaced_keys.contains(&format!("{}{}", steam::KEY_PREFIX, m.id)) {
                continue;
            }
            let key = format!("{}{}", steam::KEY_PREFIX, m.id);
            if replaced_keys.contains(&key) {
                continue;
            }
            // 只删仍然存在、且可编辑的
            if let Some(c) = cloud.collections.iter().find(|c| c.id == m.id) {
                if c.is_deleted || !c.editable() {
                    continue;
                }
            } else {
                continue;
            }
            outcome.actions.push(Action {
                kind: "delete".into(),
                name: m.name.clone(),
                id: m.id.clone(),
                key: key.clone(),
                note: Some("本工具此前创建、当前方案中已不存在".into()),
                ..Default::default()
            });
            pruned_keys.push(key);
        }
    }

    outcome.new_namespace_version = version - 1;

    if !opts.write {
        return Ok(outcome);
    }

    // ---------------- 真正落盘 ----------------
    let backup_dir = out_dir.join("backups").join(util::stamp());
    util::ensure_dir(&backup_dir)?;
    backup_file(&cloud.namespace_path, &backup_dir)?;
    if cloud.namespaces_path.is_file() {
        backup_file(&cloud.namespaces_path, &backup_dir)?;
    }
    let localconfig = steam_root
        .join("userdata")
        .join(steam3)
        .join("config")
        .join("localconfig.vdf");
    if localconfig.is_file() {
        backup_file(&localconfig, &backup_dir)?;
    }
    // 记下「备份文件名 -> 原始绝对路径」，让回滚完全不用猜
    let mut pairs: Vec<(String, String)> = vec![(
        cloud
            .namespace_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        cloud.namespace_path.to_string_lossy().into_owned(),
    )];
    if cloud.namespaces_path.is_file() {
        pairs.push((
            cloud
                .namespaces_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            cloud.namespaces_path.to_string_lossy().into_owned(),
        ));
    }
    if localconfig.is_file() {
        pairs.push((
            localconfig
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            localconfig.to_string_lossy().into_owned(),
        ));
    }
    remember_paths(out_dir, &pairs)?;

    // 重建数组：删掉将被覆盖/删除的键，追加新条目
    let touched: std::collections::BTreeSet<&str> = replaced_keys
        .iter()
        .chain(pruned_keys.iter())
        .map(|s| s.as_str())
        .collect();

    let mut raw: Vec<Value> = cloud
        .raw
        .iter()
        .filter(|item| {
            let key = entry_key(item);
            match key {
                Some(k) => !touched.contains(k.as_str()),
                None => true,
            }
        })
        .cloned()
        .collect();
    raw.extend(new_entries);
    raw.sort_by(|a, b| entry_key(a).cmp(&entry_key(b)));

    let serialized = serde_json::to_string(&raw).map_err(|e| format!("序列化合集文件失败: {}", e))?;
    write_atomically(&cloud.namespace_path, &serialized)?;

    if cloud.namespaces_path.is_file() {
        let text = std::fs::read_to_string(&cloud.namespaces_path)
            .map_err(|e| format!("读取 {} 失败: {}", cloud.namespaces_path.display(), e))?;
        let updated = bump_namespace_version(&text, 1, outcome.new_namespace_version);
        write_atomically(&cloud.namespaces_path, &updated)?;
    }

    // 清理 state 中被 prune 掉的记录
    if !pruned_keys.is_empty() {
        let pruned_ids: Vec<String> = outcome
            .actions
            .iter()
            .filter(|a| a.kind == "delete")
            .map(|a| a.id.clone())
            .collect();
        state.managed.retain(|m| !pruned_ids.contains(&m.id));
    }
    save_state(out_dir, &state)?;

    write_manifest(&backup_dir, plan, &outcome)?;
    outcome.backup_dir = Some(backup_dir.to_string_lossy().into_owned());
    Ok(outcome)
}

// ---------------------------------------------------------------------------
// 回滚
// ---------------------------------------------------------------------------

/// 列出所有备份目录（新 -> 旧）。
pub fn list_backups(out_dir: &Path) -> Vec<PathBuf> {
    let dir = out_dir.join("backups");
    let mut out: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out.reverse();
    out
}

/// 从备份目录还原。`latest=true` 时用最新一份。
pub fn restore(out_dir: &Path, backup: Option<&str>, latest: bool, dry_run: bool) -> Result<Vec<String>, String> {
    let backups = list_backups(out_dir);
    if backups.is_empty() {
        return Err(format!(
            "{} 下没有任何备份目录",
            out_dir.join("backups").display()
        ));
    }
    let chosen = if latest || backup.is_none() {
        backups[0].clone()
    } else {
        let wanted = backup.unwrap();
        let by_name = backups
            .iter()
            .find(|p| p.file_name().map(|n| n.to_string_lossy() == wanted).unwrap_or(false));
        by_name
            .cloned()
            .ok_or_else(|| format!("找不到备份 {}", wanted))?
    };

    let mut restored = Vec::new();
    for entry in std::fs::read_dir(&chosen).map_err(|e| e.to_string())?.flatten() {
        let src = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".manifest.json") || !src.is_file() {
            continue;
        }
        // 备份目录里文件名 -> 原始路径，靠 manifest 或约定推断
        let target = resolve_restore_target(out_dir, &name)?;
        restored.push(format!("{} -> {}", src.display(), target.display()));
        if !dry_run {
            let bytes = std::fs::read(&src).map_err(|e| format!("读取备份 {} 失败: {}", src.display(), e))?;
            std::fs::write(&target, bytes)
                .map_err(|e| format!("写回 {} 失败: {}", target.display(), e))?;
        }
    }
    if restored.is_empty() {
        return Err(format!("备份 {} 里没有可还原的文件", chosen.display()));
    }
    Ok(restored)
}

fn resolve_restore_target(out_dir: &Path, file_name: &str) -> Result<PathBuf, String> {
    let manifest = out_dir.join("backups").join("steam-paths.json");
    if let Ok(text) = std::fs::read_to_string(&manifest) {
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            if let Some(m) = v.get(file_name).and_then(|x| x.as_str()) {
                return Ok(PathBuf::from(m));
            }
        }
    }
    Err(format!(
        "无法确定备份文件 {} 的原始路径：缺少 {}",
        file_name,
        manifest.display()
    ))
}

/// 记录「备份文件名 -> 原始绝对路径」的映射，让回滚不依赖任何猜测。
pub fn remember_paths(out_dir: &Path, pairs: &[(String, String)]) -> Result<(), String> {
    let manifest = out_dir.join("backups").join("steam-paths.json");
    let mut map: serde_json::Map<String, Value> = std::fs::read_to_string(&manifest)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    for (name, path) in pairs {
        map.insert(name.clone(), Value::String(path.clone()));
    }
    let text = serde_json::to_string_pretty(&Value::Object(map)).map_err(|e| e.to_string())?;
    util::write_file(&manifest, &text)
}

// ---------------------------------------------------------------------------
// 工具函数
// ---------------------------------------------------------------------------

fn entry_key(item: &Value) -> Option<String> {
    let pair = item.as_array()?;
    if pair.len() != 2 {
        return None;
    }
    if let Some(s) = pair[0].as_str() {
        return Some(s.to_string());
    }
    pair[1]
        .get("key")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn make_entry(key: &str, value_json: &str, timestamp: i64, version: i64) -> Value {
    json!({
        "key": key,
        "timestamp": timestamp,
        "value": value_json,
        "version": version.to_string(),
    })
}

/// 手工拼接合集 value，锁定键顺序为 Steam 自己的 `id,name,added,removed`。
fn build_value_json(id: &str, name: &str, added: &[u32], removed: &[u32]) -> String {
    let mut s = String::with_capacity(64 + added.len() * 8);
    s.push_str("{\"id\":");
    s.push_str(&json_string(id));
    s.push_str(",\"name\":");
    s.push_str(&json_string(name));
    s.push_str(",\"added\":[");
    for (i, a) in added.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&a.to_string());
    }
    s.push_str("],\"removed\":[");
    for (i, a) in removed.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&a.to_string());
    }
    s.push_str("]}");
    s
}

fn json_string(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

/// Steam 的合集 id 形如 `uc-Ab3dEf9Klm2Q`（12 位字母数字）。
fn new_collection_id() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut seed = util::now_epoch() as u64 ^ (std::process::id() as u64) << 17;
    if let Ok(d) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        seed ^= d.subsec_nanos() as u64;
    }
    if seed == 0 {
        seed = 0x9E3779B97F4A7C15;
    }
    let mut out = String::from("uc-");
    for _ in 0..12 {
        // xorshift64
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        out.push(ALPHABET[(seed % ALPHABET.len() as u64) as usize] as char);
    }
    out
}

fn backup_file(src: &Path, dir: &Path) -> Result<(), String> {
    let name = src
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".into());
    let dst = dir.join(&name);
    std::fs::copy(src, &dst).map_err(|e| format!("备份 {} 失败: {}", src.display(), e))?;
    Ok(())
}

fn write_manifest(dir: &Path, plan: &Plan, outcome: &ApplyOutcome) -> Result<(), String> {
    let value = json!({
        "schema": "steam-curator/backup@1",
        "created_at": util::now_epoch(),
        "created_at_local": util::fmt_local(util::now_epoch()),
        "plan_source": plan.source,
        "old_namespace_version": outcome.old_namespace_version,
        "new_namespace_version": outcome.new_namespace_version,
        "actions": outcome.actions,
    });
    let text = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
    util::write_file(&dir.join("apply.manifest.json"), &text)
}

/// 原子写入：先写 `.tmp` 再 rename，避免中途断电留下半个文件。
fn write_atomically(path: &Path, content: &str) -> Result<(), String> {
    let tmp = path.with_extension("json.tmp-steam-curator");
    std::fs::write(&tmp, content.as_bytes())
        .map_err(|e| format!("写入临时文件 {} 失败: {}", tmp.display(), e))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("替换 {} 失败: {}", path.display(), e)
    })
}

/// 更新 `cloud-storage-namespaces.json` 里某个命名空间的版本号。
pub fn bump_namespace_version(text: &str, namespace: u64, new_version: i64) -> String {
    let Ok(mut value) = serde_json::from_str::<Value>(text) else {
        return text.to_string();
    };
    if let Some(arr) = value.as_array_mut() {
        for item in arr.iter_mut() {
            if let Some(pair) = item.as_array_mut() {
                if pair.len() == 2 && pair[0].as_u64() == Some(namespace) {
                    let current = pair[1]
                        .as_str()
                        .and_then(|s| s.parse::<i64>().ok())
                        .or_else(|| pair[1].as_i64())
                        .unwrap_or(0);
                    if new_version > current {
                        pair[1] = Value::String(new_version.to_string());
                    }
                }
            }
        }
    }
    serde_json::to_string(&value).unwrap_or_else(|_| text.to_string())
}
