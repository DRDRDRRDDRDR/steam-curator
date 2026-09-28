//! `plan`：导入 AI 返回的 JSON，做校验、纠错与归一化。
//!
//! AI 输出天然会飘：包一层 ``` 围栏、键名写成 `apps`、塞进不存在的 appid、
//! 同一款游戏在一个合集里出现两次。这一步的职责就是把这些都收拾干净，
//! 并且**明确报告每一种修复**，而不是静默吞掉。

use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde_json::Value;

use crate::model::*;
use crate::util;

pub struct PlanOptions {
    /// 保留无法识别的 appid（默认丢弃并报警）
    pub keep_unknown: bool,
    /// 允许生成空合集（默认丢弃）
    pub allow_empty: bool,
}

impl Default for PlanOptions {
    fn default() -> Self {
        PlanOptions {
            keep_unknown: false,
            allow_empty: false,
        }
    }
}

pub fn build(
    lib: &Library,
    ai_text: &str,
    source: &str,
    opts: &PlanOptions,
) -> Result<Plan, String> {
    let mut issues: Vec<Issue> = Vec::new();
    let json_text = extract_json(ai_text);
    let parsed: Value = serde_json::from_str(&json_text).map_err(|e| {
        format!(
            "AI 输出不是合法 JSON: {}\n--- 前 400 字符 ---\n{}",
            e,
            json_text.chars().take(400).collect::<String>()
        )
    })?;

    let known: HashSet<u32> = lib.games.iter().map(|g| g.appid).collect();
    let name_of: BTreeMap<u32, &str> = lib
        .games
        .iter()
        .map(|g| (g.appid, g.name.as_str()))
        .collect();

    let raw_collections = extract_collections(&parsed)?;
    if raw_collections.is_empty() {
        return Err(
            "AI 输出里没有解析到任何合集。期望形如 {\"collections\":[{\"name\":\"..\",\"appids\":[..]}]}"
                .into(),
        );
    }

    // 同名合集合并
    let mut merged: Vec<(String, Option<String>, Vec<u32>)> = Vec::new();
    for (raw_name, desc, appids) in raw_collections {
        let name = raw_name.trim().to_string();
        if name.is_empty() {
            issues.push(Issue {
                level: "warn".into(),
                message: "忽略了一个没有名字的合集".into(),
            });
            continue;
        }
        if let Some(slot) = merged.iter_mut().find(|(n, _, _)| n == &name) {
            slot.2.extend(appids);
            issues.push(Issue {
                level: "warn".into(),
                message: format!("合集「{}」出现多次，已合并", name),
            });
        } else {
            merged.push((name, desc, appids));
        }
    }

    let mut collections: Vec<PlanCollection> = Vec::new();
    let mut unknown_total: BTreeSet<u32> = BTreeSet::new();
    let mut declared_unassigned: Vec<u32> = Vec::new();

    for (name, description, raw_appids) in merged {
        let mut seen_in_collection: HashSet<u32> = HashSet::new();
        let mut appids: Vec<u32> = Vec::new();
        let mut dupes = 0usize;
        let mut unknown_here: Vec<u32> = Vec::new();

        for appid in raw_appids {
            if !seen_in_collection.insert(appid) {
                dupes += 1;
                continue;
            }
            if known.contains(&appid) {
                appids.push(appid);
            } else {
                unknown_here.push(appid);
                unknown_total.insert(appid);
            }
        }
        if opts.keep_unknown {
            appids.extend(unknown_here.iter().copied());
        }
        if dupes > 0 {
            issues.push(Issue {
                level: "warn".into(),
                message: format!("合集「{}」内有 {} 个重复 appid，已去重", name, dupes),
            });
        }
        if !unknown_here.is_empty() {
            let sample: Vec<String> = unknown_here
                .iter()
                .take(5)
                .map(|a| a.to_string())
                .collect();
            issues.push(Issue {
                level: "warn".into(),
                message: format!(
                    "合集「{}」里有 {} 个当前库里不存在的 appid（{}），已{}",
                    name,
                    unknown_here.len(),
                    sample.join(", "),
                    if opts.keep_unknown { "保留" } else { "丢弃" }
                ),
            });
        }
        if appids.is_empty() && !opts.allow_empty {
            issues.push(Issue {
                level: "warn".into(),
                message: format!("合集「{}」清洗后为空，已丢弃", name),
            });
            continue;
        }
        appids.sort_unstable();
        collections.push(PlanCollection {
            name,
            description,
            appids,
            existing: false,
            kept_from_existing: 0,
        });
    }

    // 内置合集名不可占用
    collections.retain(|c| {
        let clash = crate::steam::BUILTIN_IDS.contains(&c.name.as_str());
        if clash {
            issues.push(Issue {
                level: "error".into(),
                message: format!("合集名「{}」与 Steam 系统合集冲突，已丢弃", c.name),
            });
        }
        !clash
    });

    if let Some(arr) = parsed.get("unassigned").and_then(|v| v.as_array()) {
        declared_unassigned = arr.iter().filter_map(value_to_appid).collect();
    }

    // 与 Steam 现状比对
    let mut new_count = 0usize;
    let mut updated_count = 0usize;
    for c in collections.iter_mut() {
        if let Some(existing) = find_existing(lib, &c.name) {
            // 实测用户的合集名带前导空格（如 " A3 Tool"），模型不可能还原这种细节。
            // 模糊命中时沿用磁盘上的原名，否则每次都会新建一个重复合集。
            if existing.name != c.name {
                issues.push(Issue {
                    level: "info".into(),
                    message: format!(
                        "「{}」与已有合集「{}」视为同一个（仅空白/大小写差异），已沿用原名",
                        c.name, existing.name
                    ),
                });
                c.name = existing.name.clone();
            }
            c.existing = true;
            updated_count += 1;
            let planned: HashSet<u32> = c.appids.iter().copied().collect();
            c.kept_from_existing = existing
                .appids
                .iter()
                .filter(|a| !planned.contains(a) && known.contains(a))
                .count();
        } else {
            new_count += 1;
        }
    }

    // 未被任何合集收录的游戏。
    //
    // 关键口径：一个游戏只要已经躺在用户**现有的**任何一个静态合集里，
    // 就不能算「未归档」—— 否则在已有上百个合集的库上会报出一堆假缺口。
    let assigned: HashSet<u32> = collections
        .iter()
        .flat_map(|c| c.appids.iter().copied())
        .collect();
    let filed_by_existing: HashSet<u32> = lib
        .existing_collections
        .iter()
        .filter(|c| !c.is_builtin && !c.is_dynamic)
        .flat_map(|c| c.appids.iter().copied())
        .collect();

    let mut unassigned: Vec<u32> = lib
        .games
        .iter()
        .map(|g| g.appid)
        .filter(|a| !assigned.contains(a) && !filed_by_existing.contains(a))
        .collect();
    for a in declared_unassigned {
        if !assigned.contains(&a)
            && !filed_by_existing.contains(&a)
            && known.contains(&a)
            && !unassigned.contains(&a)
        {
            unassigned.push(a);
        }
    }
    unassigned.sort_unstable();

    let already_filed = lib
        .games
        .iter()
        .filter(|g| !assigned.contains(&g.appid) && filed_by_existing.contains(&g.appid))
        .count();

    if !unassigned.is_empty() {
        let names: Vec<String> = unassigned
            .iter()
            .take(8)
            .map(|a| format!("{}({})", name_of.get(a).copied().unwrap_or("?"), a))
            .collect();
        issues.push(Issue {
            level: "info".into(),
            message: format!(
                "有 {} 款游戏既不在本方案里、也不在你现有的任何合集中：{}{}",
                unassigned.len(),
                names.join("、"),
                if unassigned.len() > 8 { " …" } else { "" }
            ),
        });
    } else {
        issues.push(Issue {
            level: "info".into(),
            message: format!(
                "本方案覆盖 {} 款，另有 {} 款已由你现有的合集收录，没有遗漏。",
                assigned.len(),
                already_filed
            ),
        });
    }
    if !unknown_total.is_empty() {
        issues.push(Issue {
            level: "warn".into(),
            message: format!(
                "AI 总共提到了 {} 个不存在的 appid —— 这是模型幻觉，已全部处理",
                unknown_total.len()
            ),
        });
    }
    if !lib.existing_collections.is_empty() {
        let replaced: Vec<String> = collections
            .iter()
            .filter(|c| c.existing)
            .map(|c| c.name.clone())
            .collect();
        if !replaced.is_empty() {
            issues.push(Issue {
                level: "info".into(),
                message: format!(
                    "以下合集在 Steam 里已存在，merge 模式下会保留原有成员并并入新成员：{}",
                    replaced.join("、")
                ),
            });
        }
    }

    // 统计
    let mut stats = PlanStats {
        total_games: lib.games.len(),
        assigned_games: assigned.len(),
        unassigned_games: unassigned.len(),
        already_filed_games: already_filed,
        collection_count: collections.len(),
        new_collections: new_count,
        updated_collections: updated_count,
        ..Default::default()
    };
    for g in &lib.games {
        if assigned.contains(&g.appid) {
            stats.assigned_playtime_minutes += g.playtime_minutes;
            stats.assigned_size_bytes += g.size_bytes;
        }
    }

    let now = util::now_epoch();
    Ok(Plan {
        schema: PLAN_SCHEMA.to_string(),
        created_at: now,
        created_at_local: util::fmt_local(now),
        source: source.to_string(),
        collections,
        unassigned,
        stats,
        issues,
    })
}

/// 只比较「可编辑的静态合集」。
fn find_existing<'a>(lib: &'a Library, name: &str) -> Option<&'a CollectionInfo> {
    let candidates: Vec<&CollectionInfo> = lib
        .existing_collections
        .iter()
        .filter(|e| !e.is_builtin && !e.is_dynamic)
        .collect();
    if let Some(e) = candidates.iter().find(|e| e.name == name) {
        return Some(e);
    }
    if let Some(e) = candidates.iter().find(|e| e.name.trim() == name.trim()) {
        return Some(e);
    }
    let folded = normalize_name(name);
    if folded.is_empty() {
        return None;
    }
    candidates
        .into_iter()
        .find(|e| normalize_name(&e.name) == folded)
}

/// 去掉空白、大小写与分隔符差异，用于「是不是同一个合集」的判断。
fn normalize_name(s: &str) -> String {
    s.trim()
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '_' && *c != '-')
        .collect()
}

/// 从模型输出里抠出 JSON 主体：容忍 ``` 围栏、前后寒暄、BOM。
pub fn extract_json(text: &str) -> String {
    let t = text.trim().trim_start_matches('\u{feff}').trim();
    if let Some(start) = t.find("```") {
        let after = &t[start + 3..];
        let after = after.trim_start_matches(|c: char| c.is_ascii_alphabetic() || c == ' ' || c == '\n' || c == '\r' || c == '\t');
        let body = match after.rfind("```") {
            Some(end) => &after[..end],
            None => after,
        };
        let body = body.trim();
        if !body.is_empty() {
            return body.to_string();
        }
    }
    if let (Some(a), Some(b)) = (t.find('{'), t.rfind('}')) {
        if a < b {
            return t[a..=b].to_string();
        }
    }
    if let (Some(a), Some(b)) = (t.find('['), t.rfind(']')) {
        if a < b {
            return t[a..=b].to_string();
        }
    }
    t.to_string()
}

fn extract_collections(v: &Value) -> Result<Vec<(String, Option<String>, Vec<u32>)>, String> {
    if let Some(arr) = v.as_array() {
        return Ok(arr.iter().filter_map(parse_collection_obj).collect());
    }
    let obj = v
        .as_object()
        .ok_or_else(|| "AI 输出的顶层既不是对象也不是数组".to_string())?;

    for key in ["collections", "groups", "categories", "plan", "result", "shelves"] {
        if let Some(arr) = obj.get(key).and_then(|v| v.as_array()) {
            return Ok(arr.iter().filter_map(parse_collection_obj).collect());
        }
    }
    // 退化形态：{"射击": [1,2], "策略": [3]}
    let mut out = Vec::new();
    for (name, value) in obj {
        if let Some(arr) = value.as_array() {
            let appids = arr.iter().filter_map(value_to_appid).collect();
            out.push((name.clone(), None, appids));
        }
    }
    Ok(out)
}

fn parse_collection_obj(v: &Value) -> Option<(String, Option<String>, Vec<u32>)> {
    let obj = v.as_object()?;
    let name = ["name", "title", "collection", "group"]
        .iter()
        .find_map(|k| obj.get(*k).and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string();
    let description = ["description", "desc", "note", "reason"]
        .iter()
        .find_map(|k| obj.get(*k).and_then(|v| v.as_str()))
        .map(|s| s.to_string());
    let mut appids: Vec<u32> = Vec::new();
    for key in ["appids", "apps", "added", "games", "ids", "app_ids", "members"] {
        if let Some(arr) = obj.get(key).and_then(|v| v.as_array()) {
            appids = arr.iter().filter_map(value_to_appid).collect();
            break;
        }
    }
    Some((name, description, appids))
}

fn value_to_appid(v: &Value) -> Option<u32> {
    if let Some(n) = v.as_u64() {
        return Some(n as u32);
    }
    if let Some(n) = v.as_i64() {
        if n > 0 {
            return Some(n as u32);
        }
    }
    if let Some(s) = v.as_str() {
        return s.trim().parse::<u32>().ok();
    }
    if let Some(o) = v.as_object() {
        for key in ["appid", "id", "app_id"] {
            if let Some(x) = o.get(key) {
                if let Some(a) = value_to_appid(x) {
                    return Some(a);
                }
            }
        }
    }
    None
}
