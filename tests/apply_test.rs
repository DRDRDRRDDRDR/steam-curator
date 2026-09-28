//! `apply` 阶段的集成测试：把方案写回 Steam 合集。
//!
//! 全部在**假 cloudstorage 沙箱**里跑（见 `tests/common/mod.rs` 的写盘安全约定），
//! 绝不触碰真实 Steam 目录；`ApplyOptions.force = true` 让测试不依赖
//! 「本机此刻是否正在跑 Steam」。
//!
//! # 这里守的是什么
//!
//! `apply --write` 是整条流水线上唯一会**改动用户真实文件**的一步，历史上出过一次
//! 严重事故的雏形：`make_entry` 曾把条目写成**裸对象**而不是 Steam 与
//! `steam::read_cloud` 都要求的 `[key, entry]` 二元组。后果是 update 路径先把原有
//! 二元组过滤删除、再追加一个谁也读不出来的裸对象——等于把用户已有的合集抹掉；
//! 同时幂等性失效，每跑一次就重复新建一批合集并抬版本号。
//!
//! 因为那条路径当时只跑过演练（`apply-report.json` 里 `dry_run: true`）才没暴露。
//! 现在它被两层看住：
//!
//! * `written_entries_are_key_entry_pairs_...` —— 形状不变量本身；
//! * `round_trip_...` —— 用 `read_cloud` 把写出的文件重新读回来（真正能抓住这个 bug 的测试）；
//! * `malformed_cloud_file_...` —— 覆盖 `src/apply.rs::validate_cloud_array` 这道落盘前闸门。

mod common;

use std::collections::BTreeMap;

use common::*;
use serde_json::Value;
use steam_curator::apply::bump_namespace_version;

/// 一个「更新已有合集 + 新建一个合集」的方案，多数测试都用它。
fn update_plus_create() -> steam_curator::model::Plan {
    plan_of(vec![
        plan_collection("RPG", &[200, 300]),
        plan_collection("新合集", &[100]),
    ])
}

// ---------------------------------------------------------------------------
// dry-run：默认不写盘
// ---------------------------------------------------------------------------

/// 不变量：`write=false` 时**一个字节都不许动**，但仍然要算出动作让用户预览。
#[test]
fn dry_run_touches_no_file_but_still_computes_actions() {
    let sb = standard_sandbox("dryrun");
    let before = snapshot_tree(sb.tmp.path());

    let outcome = apply_dry(&sb, &update_plus_create());

    assert!(outcome.dry_run);
    assert!(outcome.backup_dir.is_none(), "演练不该产生备份目录");
    assert!(
        !sb.out_dir.join("backups").exists(),
        "演练不该创建 backups 目录"
    );
    assert!(
        outcome.actions.iter().any(|a| a.kind == "update"),
        "演练也必须算出动作：{:?}",
        outcome.actions
    );
    assert_eq!(outcome.old_namespace_version, FIXTURE_NAMESPACE_VERSION);

    assert_tree_eq("dry-run 之后", &before, &snapshot_tree(sb.tmp.path()));
}

/// 自证：上一条断言不是空跑——同样的快照机制必须能发现一次**真实**写入。
///
/// 如果这个测试红了，说明 `snapshot_tree`/`assert_tree_eq` 分辨不出变化，
/// 那么 `dry_run_touches_no_file_but_still_computes_actions` 就是假的绿。
#[test]
fn dry_run_byte_comparison_is_not_vacuous() {
    let sb = standard_sandbox("negctl");
    let plan = update_plus_create();

    let before = snapshot_tree(sb.tmp.path());
    apply_dry(&sb, &plan);
    assert_tree_eq("演练后", &before, &snapshot_tree(sb.tmp.path()));

    apply_write(&sb, &plan, "merge");
    let after = snapshot_tree(sb.tmp.path());

    assert!(
        after != before,
        "write=true 竟然没改动任何文件 —— 那么上一条「演练零写盘」的断言毫无意义"
    );
    let ns_before = before
        .get("steam/userdata/111111111/config/cloudstorage/cloud-storage-namespaces.json")
        .expect("快照里应当有 namespaces 文件");
    let ns_after = after
        .get("steam/userdata/111111111/config/cloudstorage/cloud-storage-namespaces.json")
        .expect("快照里应当有 namespaces 文件");
    assert!(
        ns_after != ns_before,
        "真写一次之后 namespaces 版本号应当被抬高"
    );
}

// ---------------------------------------------------------------------------
// merge / replace 语义
// ---------------------------------------------------------------------------

/// 不变量（merge）：已有同名合集的原有成员必须被保留（取并集），不能丢。
#[test]
fn merge_mode_keeps_existing_members_as_union() {
    let sb = standard_sandbox("merge");
    // 磁盘上 uc-ExistingAA 的 members = [100,200]；方案只写 [200,300]。
    let outcome = apply_write(&sb, &plan_of(vec![plan_collection("RPG", &[200, 300])]), "merge");

    assert_eq!(outcome.actions.len(), 1);
    let a = &outcome.actions[0];
    assert_eq!(a.kind, "update", "同名合集已存在，应当是 update 而不是 create");
    assert_eq!(a.id, "uc-ExistingAA", "必须复用原有 id");
    assert_eq!(a.name, "RPG");
    assert_eq!(a.added, vec![100, 200, 300], "merge 语义 = 并集");
    assert_eq!(a.kept_from_existing, vec![100], "原有成员 100 应被计为保留");
    assert!(a.dropped.is_empty(), "merge 模式下不该丢成员");

    let raw = cloud_entries(&sb.cs);
    let v = collection_value(&raw, KEY_EXISTING_RPG).expect("写回的 RPG 条目应当能解析");
    assert_eq!(value_added(&v), vec![100, 200, 300]);
    assert_eq!(value_removed(&v), Vec::<u32>::new());
}

/// 不变量（replace）：以方案为准，被移除的成员必须出现在 `dropped` 里并且真的消失。
#[test]
fn replace_mode_drops_members_absent_from_the_plan() {
    let sb = standard_sandbox("replace");
    let outcome = apply_write(&sb, &plan_of(vec![plan_collection("RPG", &[200, 300])]), "replace");

    let a = &outcome.actions[0];
    assert_eq!(a.kind, "update");
    assert_eq!(a.added, vec![200, 300], "replace 语义 = 以方案为准");
    assert_eq!(a.dropped, vec![100], "被移除的成员必须出现在 dropped 里");
    assert!(a.kept_from_existing.is_empty());

    let raw = cloud_entries(&sb.cs);
    let v = collection_value(&raw, KEY_EXISTING_RPG).unwrap();
    assert_eq!(value_added(&v), vec![200, 300]);
}

/// 不变量：同名动态合集（带 filterSpec）绝对不许被覆盖，必须 skip 并说明原因。
#[test]
fn apply_never_overwrites_a_dynamic_collection_with_the_same_name() {
    let sb = standard_sandbox("dynamic");
    let outcome = apply_write(
        &sb,
        &plan_of(vec![plan_collection("动态测试", &[100])]),
        "merge",
    );

    assert_eq!(outcome.actions.len(), 1);
    assert_eq!(outcome.actions[0].kind, "skip");
    assert_eq!(outcome.actions[0].id, "uc-DynamicAAA");
    assert!(
        outcome.actions[0]
            .note
            .as_deref()
            .unwrap_or("")
            .contains("动态合集"),
        "skip 必须说明原因：{:?}",
        outcome.actions[0].note
    );

    let raw = cloud_entries(&sb.cs);
    let v = collection_value(&raw, KEY_DYNAMIC).unwrap();
    assert!(
        v.get("filterSpec").is_some(),
        "动态合集的 filterSpec 必须原样保留"
    );
    assert_eq!(entry_version(&raw, KEY_DYNAMIC), Some(450), "动态合集版本号不得被抬高");
    // 只有 skip，没有新增条目 → 命名空间版本不该抬
    assert_eq!(outcome.new_namespace_version, outcome.old_namespace_version);
}

// ---------------------------------------------------------------------------
// 版本号
// ---------------------------------------------------------------------------

/// 不变量：写回的条目版本必须**严格大于**旧命名空间版本，
/// 且 `cloud-storage-namespaces.json` 里的命名空间 1 被同步抬高、其它命名空间不动。
#[test]
fn new_entry_versions_exceed_namespace_version_and_namespaces_file_is_bumped() {
    let sb = standard_sandbox("versions");
    let outcome = apply_write(&sb, &update_plus_create(), "merge");

    assert_eq!(outcome.old_namespace_version, FIXTURE_NAMESPACE_VERSION);
    assert!(
        outcome.new_namespace_version > outcome.old_namespace_version,
        "有新增条目时必须抬高命名空间版本：{} -> {}",
        outcome.old_namespace_version,
        outcome.new_namespace_version
    );

    let raw = cloud_entries(&sb.cs);
    for a in &outcome.actions {
        if a.kind == "unchanged" || a.kind == "skip" {
            continue;
        }
        let v = entry_version(&raw, &a.key)
            .unwrap_or_else(|| panic!("动作 {} 对应的条目 {} 不在写回文件里", a.kind, a.key));
        assert!(
            v > outcome.old_namespace_version,
            "条目 {} 的版本 {} 必须严格大于旧命名空间版本 {}",
            a.key,
            v,
            outcome.old_namespace_version
        );
    }

    // 版本号按动作顺序递增，最后一个是命名空间新版本
    assert_eq!(
        entry_version(&raw, KEY_EXISTING_RPG),
        Some(FIXTURE_NAMESPACE_VERSION + 1)
    );
    let created = outcome
        .actions
        .iter()
        .find(|a| a.kind == "create")
        .expect("应当有一个 create 动作");
    assert_eq!(
        entry_version(&raw, &created.key),
        Some(outcome.new_namespace_version)
    );

    // namespaces 文件被同步抬高
    assert_eq!(
        namespace_version(&sb.cs, 1),
        Some(outcome.new_namespace_version)
    );
    // 其它命名空间不受影响
    assert_eq!(namespace_version(&sb.cs, 3), Some(0));
    // 没被碰过的条目版本不变
    assert_eq!(entry_version(&raw, KEY_HIDDEN), Some(700));
}

// ---------------------------------------------------------------------------
// 其它键族 / tombstone 保留
// ---------------------------------------------------------------------------

/// 不变量：`showcases.*`、`NewContentRollup_*`、tombstone、内置合集、动态合集
/// 在写回后必须**语义上一字不差**地保留；`*.modified.json` 完全不动。
#[test]
fn unrelated_key_families_and_tombstones_survive_the_write() {
    let sb = standard_sandbox("preserve");
    let modified = sb.cs.join("cloud-storage-namespace-1.modified.json");
    std::fs::write(&modified, "[]").unwrap();

    let untouched = [
        KEY_ROLLUP,
        KEY_SHOWCASE,
        KEY_HIDDEN,
        KEY_TOMBSTONE,
        KEY_DYNAMIC,
    ];
    let before_raw = cloud_entries(&sb.cs);
    let before: BTreeMap<&str, Value> = untouched
        .iter()
        .map(|k| {
            (
                *k,
                entry_object(&before_raw, k)
                    .unwrap_or_else(|| panic!("fixture 里应当有 {}", k))
                    .clone(),
            )
        })
        .collect();

    apply_write(&sb, &update_plus_create(), "merge");

    let after_raw = cloud_entries(&sb.cs);
    for key in untouched {
        let after = entry_object(&after_raw, key)
            .unwrap_or_else(|| panic!("无关键族 {} 在写回后消失了", key));
        assert_eq!(
            after, &before[key],
            "无关键族 {} 被改动了（必须原样带走）",
            key
        );
    }

    assert_eq!(
        std::fs::read(&modified).unwrap(),
        b"[]",
        "cloud-storage-namespace-1.modified.json 必须完全不动"
    );
}

// ---------------------------------------------------------------------------
// 写回形状 / 往返（历史 bug 的正面防线）
// ---------------------------------------------------------------------------

/// 不变量：写回的每一项都必须是 `[key, entry]` 二元组，且 `pair[0] == entry.key`。
#[test]
fn written_entries_are_key_entry_pairs() {
    let sb = standard_sandbox("shape");
    apply_write(&sb, &update_plus_create(), "merge");

    let raw = cloud_entries(&sb.cs);
    assert!(!raw.is_empty());
    for item in &raw {
        let pair = item
            .as_array()
            .unwrap_or_else(|| panic!("写回的条目必须是 [key, entry] 二元组，实际: {}", item));
        assert_eq!(pair.len(), 2, "二元组长度必须为 2: {}", item);
        assert!(pair[0].is_string(), "pair[0] 必须是 key 字符串: {}", item);
        assert!(pair[1].is_object(), "pair[1] 必须是 entry 对象: {}", item);
        assert_eq!(
            pair[0].as_str(),
            pair[1].get("key").and_then(|v| v.as_str()),
            "pair[0] 必须与 entry.key 一致: {}",
            item
        );
    }
}

/// 不变量（往返）：`apply --write` 之后，用 `steam::read_cloud` 重新读回同一个目录，
/// 必须看到刚写下的东西；连续跑两次，第二次全是 `unchanged` 且不抬版本、文件字节不变。
#[test]
fn round_trip_read_cloud_sees_what_apply_wrote_and_is_idempotent() {
    let sb = standard_sandbox("roundtrip");
    let plan = update_plus_create();

    let first = apply_write(&sb, &plan, "merge");
    assert_eq!(
        first
            .actions
            .iter()
            .map(|a| a.kind.as_str())
            .collect::<Vec<_>>(),
        vec!["update", "create"],
        "第一次运行：更新 RPG、新建 新合集"
    );

    // ①/② 用工具自己的读取路径把文件读回来
    let cloud = steam_curator::steam::read_cloud(&sb.steam_root, STEAM3)
        .expect("写回之后 read_cloud 必须能读回来");

    let created = cloud
        .by_name("新合集")
        .expect("新合集必须能被 read_cloud 读回 —— 读不回来说明写出的形状不是 Steam 的形状");
    assert_eq!(created.added, vec![100]);
    assert!(created.editable(), "新合集必须是可编辑的静态合集");

    let rpg = cloud.by_name("RPG").expect("已有的 RPG 必须还在");
    assert_eq!(rpg.id, "uc-ExistingAA", "必须复用原有 id");
    assert_eq!(rpg.added, vec![100, 200, 300], "merge 语义必须成立");

    // 其它合集没被误伤
    assert_eq!(cloud.by_name("已隐藏").map(|c| c.id.as_str()), Some("hidden"));
    assert!(cloud.by_name("动态测试").is_some());
    assert!(
        cloud.collections.iter().any(|c| c.is_deleted),
        "tombstone 必须还在文件里"
    );

    // ③ 幂等：同一方案再跑一次
    let before = snapshot_tree(&sb.cs);
    let second = apply_write(&sb, &plan, "merge");

    assert_eq!(
        second
            .actions
            .iter()
            .map(|a| a.kind.as_str())
            .collect::<Vec<_>>(),
        vec!["unchanged", "unchanged"],
        "结果与现状一致时必须报 unchanged：{:?}",
        second.actions
    );
    assert_eq!(
        second.new_namespace_version, second.old_namespace_version,
        "结果没变时不该抬命名空间版本"
    );
    assert_tree_eq(
        "第二次 apply 之后 cloudstorage 必须逐字节不变",
        &before,
        &snapshot_tree(&sb.cs),
    );
}

/// 不变量：畸形合集文件（含非二元组条目）必须**拒绝写入**并保持原文件不变。
///
/// 覆盖 `src/apply.rs::validate_cloud_array`：宁可报错，也不能写出一个
/// Steam 读不了的文件。
#[test]
fn malformed_cloud_file_is_rejected_and_left_untouched() {
    let good = pair_entry(
        KEY_EXISTING_RPG,
        Some(&static_collection_value("uc-ExistingAA", "RPG", &[100, 200])),
        500,
    );
    // 手工混入一个裸对象（不是 [key, entry] 二元组）的畸形条目
    let bad = r#"{"key":"user-collections.uc-BadObject","timestamp":1700000000,"value":"{\"id\":\"uc-BadObject\",\"name\":\"畸形\",\"added\":[100],\"removed\":[]}","version":"10"}"#;
    let sb = sandbox(
        "malformed",
        &format!("[\n{},\n{}\n]", good, bad),
        Some(FIXTURE_NAMESPACES),
    );

    let before = snapshot_tree(&sb.cs);
    let err = try_apply_write(&sb, &plan_of(vec![plan_collection("新合集", &[100])]), "merge")
        .expect_err("畸形文件必须被拒绝写入");

    // 只断言「形状自检挡住了这次写入」，不锁死措辞
    assert!(
        err.contains("自检失败") || err.contains("二元组"),
        "错误信息应当点明是落盘前的形状自检失败，实际: {}",
        err
    );
    assert!(
        err.contains("未") && err.contains("改动"),
        "错误信息应当向用户保证原文件未被改动，实际: {}",
        err
    );
    assert_tree_eq(
        "拒绝写入后 cloudstorage 必须原样",
        &before,
        &snapshot_tree(&sb.cs),
    );
}

// ---------------------------------------------------------------------------
// prune
// ---------------------------------------------------------------------------

/// 不变量：prune 只删「本工具管过、且当前方案里已不存在」的合集，
/// 且必须显式开启；它不许碰任何无关键族。
#[test]
fn prune_deletes_only_tool_managed_collections_and_only_when_asked() {
    let state = r#"{"managed":[{"id":"uc-ExistingAA","name":"RPG","updated_at":0}]}"#;
    let plan = plan_of(vec![plan_collection("新合集", &[100])]);

    // 不开启 prune：本工具管过的合集即使不在方案里也不许删
    let off = standard_sandbox("prune-off");
    std::fs::write(off.out_dir.join("state.json"), state).unwrap();
    let o_off = apply_write(&off, &plan, "merge");
    assert!(
        o_off.actions.iter().all(|a| a.kind != "delete"),
        "prune=false 时不该有 delete：{:?}",
        o_off.actions
    );
    assert!(
        collection_value(&cloud_entries(&off.cs), KEY_EXISTING_RPG).is_some(),
        "prune=false 时 RPG 必须还在"
    );

    // 开启 prune：删掉它，并清掉 state 里的记录
    let on = standard_sandbox("prune-on");
    std::fs::write(on.out_dir.join("state.json"), state).unwrap();
    let o_on = apply_run(&on.steam_root, &on.out_dir, &plan, "merge", true, true);
    let deleted: Vec<&str> = o_on
        .actions
        .iter()
        .filter(|a| a.kind == "delete")
        .map(|a| a.name.as_str())
        .collect();
    assert_eq!(deleted, vec!["RPG"], "只该删掉那个工具管过的合集：{:?}", o_on.actions);

    let raw = cloud_entries(&on.cs);
    assert!(
        entry_object(&raw, KEY_EXISTING_RPG).is_none(),
        "被 prune 的合集必须从文件里消失"
    );
    for key in [KEY_ROLLUP, KEY_SHOWCASE, KEY_HIDDEN, KEY_TOMBSTONE, KEY_DYNAMIC] {
        assert!(entry_object(&raw, key).is_some(), "prune 不该动 {}", key);
    }

    let saved: Value = read_json(&on.out_dir.join("state.json"));
    let ids: Vec<String> = saved["managed"]
        .as_array()
        .expect("state.managed 应当是数组")
        .iter()
        .filter_map(|m| m["id"].as_str().map(str::to_string))
        .collect();
    assert!(
        !ids.contains(&"uc-ExistingAA".to_string()),
        "被删掉的合集应当从 state 里清掉：{:?}",
        ids
    );
    assert_eq!(ids.len(), 1, "应当只剩新记入的那个合集：{:?}", ids);
}

// ---------------------------------------------------------------------------
// bump_namespace_version（纯函数）
// ---------------------------------------------------------------------------

/// 不变量：`bump_namespace_version` 只升不降、只动目标命名空间、坏输入原样返回。
#[test]
fn bump_namespace_version_only_raises_and_ignores_malformed_input() {
    let text = r#"[[3,"0"],[1,"900"]]"#;

    // 抬高
    assert_eq!(bump_namespace_version(text, 1, 950), r#"[[3,"0"],[1,"950"]]"#);
    // 降低 / 相等：不改
    assert_eq!(bump_namespace_version(text, 1, 800), text);
    assert_eq!(bump_namespace_version(text, 1, 900), text);
    // 幂等且确定
    let once = bump_namespace_version(text, 1, 913);
    assert_eq!(bump_namespace_version(&once, 1, 913), once);
    assert_eq!(
        bump_namespace_version(text, 1, 950),
        bump_namespace_version(text, 1, 950)
    );
    // 只动目标命名空间
    assert_eq!(bump_namespace_version(text, 3, 5), r#"[[3,"5"],[1,"900"]]"#);
    // 命名空间不存在：原样
    assert_eq!(bump_namespace_version(text, 7, 999), text);
    // 数字型版本号也认得，并统一写成字符串
    assert_eq!(bump_namespace_version(r#"[[1,900]]"#, 1, 950), r#"[[1,"950"]]"#);
    // 非法 JSON：原样返回，不 panic
    assert_eq!(bump_namespace_version("not json", 1, 999), "not json");
    assert_eq!(bump_namespace_version("", 1, 999), "");
}

// ---------------------------------------------------------------------------
// 端到端：plan 沿用磁盘原名 → apply 命中同一合集
// ---------------------------------------------------------------------------

/// 不变量（跨模块）：plan 阶段把「只差空白/大小写」的名字归一成磁盘原名之后，
/// apply 阶段必须**命中同一个合集做 update**，而不是新建一个重复合集。
#[test]
fn end_to_end_plan_name_reuse_makes_apply_update_instead_of_create() {
    use steam_curator::plan::{build, PlanOptions};

    let tmp = TempDir::new("e2e");
    let steam_root = tmp.dir("steam");
    let out_dir = tmp.dir("out");
    const ID: &str = "uc-A3ToolAAAA";
    let cloud = cloud_file(vec![(
        "user-collections.uc-A3ToolAAAA",
        pair_entry(
            "user-collections.uc-A3ToolAAAA",
            Some(&static_collection_value(ID, " A3 Tool", &[100])),
            500,
        ),
    )]);
    write_cloud(&steam_root, &cloud, Some(r#"[[1,"900"]]"#));

    let lib = library(
        vec![game(100, "Alpha"), game(200, "Beta")],
        vec![user_collection(ID, " A3 Tool", &[100])],
    );
    // 模型不可能还原「磁盘上名字带前导空格」这种细节
    let plan = build(
        &lib,
        r#"{"collections":[{"name":"A3 Tool","appids":[200]}]}"#,
        "ai",
        &PlanOptions::default(),
    )
    .expect("plan 应当成功");
    assert_eq!(plan.collections[0].name, " A3 Tool");

    let outcome = apply_write_at(&steam_root, &out_dir, &plan, "merge");

    assert_eq!(outcome.actions.len(), 1, "不该额外新建合集：{:?}", outcome.actions);
    assert_eq!(
        outcome.actions[0].kind, "update",
        "沿用磁盘原名后必须命中同一个合集"
    );
    assert_eq!(outcome.actions[0].id, ID);
    assert_eq!(outcome.actions[0].added, vec![100, 200]);

    let cloud = steam_curator::steam::read_cloud(&steam_root, STEAM3).unwrap();
    assert_eq!(cloud.live().iter().filter(|c| !c.is_builtin).count(), 1);
}
