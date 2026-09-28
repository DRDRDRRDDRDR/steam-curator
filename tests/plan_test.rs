//! `plan` 阶段的集成测试：把 AI 返回的 JSON 清洗成整理方案。
//!
//! 每个测试名都对应一条**要被永久看住的不变量**。测试只用假数据，
//! 不读真实 Steam 目录、不联网、不依赖本机是否装了 Steam。

mod common;

use common::*;
use steam_curator::model::{Library, PLAN_SCHEMA};
use steam_curator::plan::{build, extract_json, PlanOptions};

/// 两款游戏的最小库。
fn two_games() -> Library {
    library(vec![game(100, "Alpha"), game(200, "Beta")], Vec::new())
}

// ---------------------------------------------------------------------------
// 解析容错
// ---------------------------------------------------------------------------

/// 不变量：AI 输出的 ``` 围栏与前后寒暄不得影响解析结果。
#[test]
fn fenced_json_with_surrounding_chatter_still_parses() {
    let lib = two_games();
    let ai = "好的，我分析完了。下面是整理方案：\n\n\
              ```json\n\
              {\"collections\":[{\"name\":\"Shooter\",\"description\":\"射击游戏\",\"appids\":[200,100]}]}\n\
              ```\n\n\
              希望对你有帮助！";
    let plan = build(&lib, ai, "ai-plan.json", &PlanOptions::default()).expect("应当解析成功");

    assert_eq!(plan.schema, PLAN_SCHEMA);
    assert_eq!(plan.source, "ai-plan.json");
    assert_eq!(plan.collections.len(), 1);
    let c = &plan.collections[0];
    assert_eq!(c.name, "Shooter");
    assert_eq!(c.appids, vec![100, 200], "合集内 appid 应当被排序");
    assert_eq!(c.description.as_deref(), Some("射击游戏"));
    assert!(!c.existing, "库里没有同名合集，不该标为已存在");

    assert_eq!(plan.stats.total_games, 2);
    assert_eq!(plan.stats.assigned_games, 2);
    assert_eq!(plan.stats.unassigned_games, 0);
    assert_eq!(plan.stats.collection_count, 1);
    assert_eq!(plan.stats.new_collections, 1);
    assert_eq!(plan.stats.updated_collections, 0);
}

/// 不变量：裸数组、`{"collections":[...]}`、键名别名、以及带寒暄的变体，
/// 都必须解析成同一个方案。
///
/// 历史 bug（已修，见 `bare_array_is_not_misread_as_a_name_to_ids_map`）：
/// `extract_json` 曾先试 `{...}`、一旦命中就 return，于是顶层裸数组被切成第一个
/// 元素对象，落进退化分支后合集名变成数组键名。
#[test]
fn top_level_array_and_collections_object_are_both_accepted() {
    let lib = library(vec![game(100, "Alpha")], Vec::new());
    let cases = [
        // 顶层裸数组（两种成员键名写法）
        r#"[{"name":"A","apps":[100]}]"#,
        r#"[{"name":"A","appids":[100]}]"#,
        // 裸数组 + 前后寒暄
        r#"结果如下：[{"name":"A","appids":[100]}] 以上。"#,
        // 标准对象形态
        r#"{"collections":[{"name":"A","appids":[100]}]}"#,
        // 寒暄里恰好带方括号，后面跟标准对象形态（不能因为 `[` 在前就切错）
        "好的 [见下]\n{\"collections\":[{\"name\":\"A\",\"appids\":[100]}]}",
        // 键名别名
        r#"{"groups":[{"title":"A","games":[100]}]}"#,
    ];
    for ai in cases {
        let plan = build(&lib, ai, "ai", &PlanOptions::default())
            .unwrap_or_else(|e| panic!("形态应当被接受: {}\n错误: {}", ai, e));
        assert_eq!(plan.collections.len(), 1, "形态 {} 解析出的合集数不对", ai);
        assert_eq!(plan.collections[0].name, "A", "形态 {}", ai);
        assert_eq!(plan.collections[0].appids, vec![100], "形态 {}", ai);
    }
}

/// 回归防线：顶层裸数组**不得**被误读成「合集名 -> appid 数组」的退化形态。
///
/// 这正是上一个 bug 的具体症状：合集名会变成 `apps`/`appids`/`games`/`added`
/// 这类数组键名；又因为名字全都一样，多个合集会被「同名合并」并成一个垃圾名合集，
/// merge 模式下这个合集会被真的建进用户库。成员 appid 反而是对的，所以静默、难发现。
#[test]
fn bare_array_is_not_misread_as_a_name_to_ids_map() {
    let lib = library(vec![game(100, "Alpha"), game(200, "Beta")], Vec::new());
    let forbidden = ["apps", "appids", "app_ids", "games", "added", "ids", "members"];

    let shapes = [
        r#"[{"name":"射击","apps":[100]}]"#,
        r#"[{"name":"射击","appids":[100]},{"name":"策略","games":[200]}]"#,
    ];
    for ai in shapes {
        let plan = build(&lib, ai, "ai", &PlanOptions::default())
            .unwrap_or_else(|e| panic!("形态 {} 应当被接受: {}", ai, e));
        for c in &plan.collections {
            assert!(
                !forbidden.contains(&c.name.as_str()),
                "合集名被误当成了数组键名「{}」—— 顶层裸数组被切错了。形态: {}\n{}",
                c.name,
                ai,
                issue_dump(&plan)
            );
        }
    }

    // 两个合集的裸数组必须真的解析成两个名字正确的合集
    let plan = build(
        &lib,
        r#"[{"name":"射击","appids":[100]},{"name":"策略","games":[200]}]"#,
        "ai",
        &PlanOptions::default(),
    )
    .unwrap();
    assert_eq!(plan.collections.len(), 2, "多合集的裸数组不得被并成一个");
    assert_eq!(plan.collections[0].name, "射击");
    assert_eq!(plan.collections[1].name, "策略");
    assert_eq!(plan.collections[0].appids, vec![100]);
    assert_eq!(plan.collections[1].appids, vec![200]);
}

/// 不变量：退化的 `{"合集名":[appid,...]}` 形态也要能用。
#[test]
fn degenerate_name_to_ids_object_is_accepted() {
    let lib = two_games();
    let plan = build(
        &lib,
        r#"{"射击":[100,200]}"#,
        "ai",
        &PlanOptions::default(),
    )
    .expect("退化形态应当被接受");
    assert_eq!(plan.collections.len(), 1);
    assert_eq!(plan.collections[0].name, "射击");
    assert_eq!(plan.collections[0].appids, vec![100, 200]);
}

/// 不变量：`extract_json` 能剥掉围栏、BOM、前后寒暄。
#[test]
fn extract_json_strips_fences_chatter_and_bom() {
    assert_eq!(extract_json("```json\n{\"a\":1}\n```"), "{\"a\":1}");
    assert_eq!(extract_json("```\n[1,2]\n```"), "[1,2]");
    assert_eq!(extract_json("\u{feff}  {\"a\":1}  "), "{\"a\":1}");
    assert_eq!(extract_json("先说明\n{\"a\":1}\n后说明"), "{\"a\":1}");
    assert_eq!(extract_json("{\"a\":1}"), "{\"a\":1}");
    // 顶层裸数组（元素是对象）不得被切成第一个对象——见 bare_array_... 的回归防线
    assert_eq!(
        extract_json(r#"[{"name":"A","apps":[100]}]"#),
        r#"[{"name":"A","apps":[100]}]"#
    );
    assert_eq!(
        extract_json(r#"结果如下：[{"name":"A","appids":[100]}] 以上。"#),
        r#"[{"name":"A","appids":[100]}]"#
    );
    // 寒暄里的方括号不能让切法选错
    assert_eq!(
        extract_json("好的 [见下]\n{\"collections\":[]}"),
        "{\"collections\":[]}"
    );
}

/// 不变量：坏输入必须给出**可操作**的错误，而不是静默产出空方案。
#[test]
fn malformed_ai_output_is_rejected_with_actionable_error() {
    let lib = two_games();
    let bad_json = build(&lib, "这不是 JSON", "ai", &PlanOptions::default()).unwrap_err();
    assert!(
        bad_json.contains("不是合法 JSON"),
        "错误信息应当点明 JSON 非法，实际: {}",
        bad_json
    );

    let no_collections =
        build(&lib, r#"{"collections":[]}"#, "ai", &PlanOptions::default()).unwrap_err();
    assert!(
        no_collections.contains("没有解析到任何合集"),
        "错误信息应当点明没有合集，实际: {}",
        no_collections
    );
}

// ---------------------------------------------------------------------------
// 幻觉 appid
// ---------------------------------------------------------------------------

/// 不变量（默认）：库里不存在的 appid 必须被丢弃，并且**点名报告**。
#[test]
fn unknown_appids_are_dropped_by_default_and_reported() {
    let lib = two_games();
    let plan = build(
        &lib,
        r#"{"collections":[{"name":"A","appids":[100,999999]}]}"#,
        "ai",
        &PlanOptions::default(),
    )
    .unwrap();

    assert_eq!(
        plan.collections[0].appids,
        vec![100],
        "不在库里的 appid 必须被丢弃"
    );
    assert!(
        has_issue_at(&plan, "warn", "999999"),
        "issues 里应当点名这个幻觉 appid\n{}",
        issue_dump(&plan)
    );
    assert!(
        has_issue(&plan, "1 个不存在的 appid"),
        "应当有一条汇总告警\n{}",
        issue_dump(&plan)
    );
}

/// 不变量：`keep_unknown=true` 是**显式**的反向选择，二者都能被验证。
#[test]
fn keep_unknown_true_preserves_hallucinated_appids() {
    let lib = two_games();
    let opts = PlanOptions {
        keep_unknown: true,
        allow_empty: false,
    };
    let plan = build(
        &lib,
        r#"{"collections":[{"name":"A","appids":[100,999999]}]}"#,
        "ai",
        &opts,
    )
    .unwrap();

    assert_eq!(plan.collections[0].appids, vec![100, 999999]);
    assert!(
        has_issue(&plan, "已保留"),
        "保留模式下告警措辞应当指出「已保留」\n{}",
        issue_dump(&plan)
    );
}

// ---------------------------------------------------------------------------
// 去重与合并
// ---------------------------------------------------------------------------

/// 不变量：同一合集内重复的 appid 必须去重，并报出去掉的条数。
#[test]
fn duplicate_appids_inside_one_collection_are_deduped_and_reported() {
    let lib = two_games();
    let plan = build(
        &lib,
        r#"{"collections":[{"name":"A","appids":[100,100,200,200]}]}"#,
        "ai",
        &PlanOptions::default(),
    )
    .unwrap();

    assert_eq!(plan.collections[0].appids, vec![100, 200]);
    assert!(
        has_issue(&plan, "2 个重复 appid"),
        "应当报出去重条数\n{}",
        issue_dump(&plan)
    );
}

/// 不变量：同名合集出现两次必须合并成一个（取并集），不能产出两个同名合集。
#[test]
fn duplicate_collection_names_are_merged_into_one() {
    let lib = two_games();
    let plan = build(
        &lib,
        r#"{"collections":[{"name":"RPG","appids":[100]},{"name":"RPG","appids":[200,100]}]}"#,
        "ai",
        &PlanOptions::default(),
    )
    .unwrap();

    assert_eq!(plan.collections.len(), 1, "同名合集必须合并");
    assert_eq!(plan.collections[0].name, "RPG");
    assert_eq!(plan.collections[0].appids, vec![100, 200]);
    assert!(
        has_issue(&plan, "出现多次，已合并"),
        "合并必须留痕\n{}",
        issue_dump(&plan)
    );
}

// ---------------------------------------------------------------------------
// 与现有合集的模糊匹配
// ---------------------------------------------------------------------------

/// 不变量：计划里的名字与磁盘上的名字只差空白时，必须判定为同一合集
/// 并**沿用磁盘原名**——否则每次 apply 都会新建一个重复合集。
#[test]
fn planned_name_differing_only_by_whitespace_reuses_on_disk_name() {
    let lib = library(
        vec![game(100, "Alpha"), game(200, "Beta")],
        vec![user_collection("uc-A3Tool", " A3 Tool", &[100])],
    );
    let plan = build(
        &lib,
        r#"{"collections":[{"name":"A3 Tool","appids":[200]}]}"#,
        "ai",
        &PlanOptions::default(),
    )
    .unwrap();

    let c = &plan.collections[0];
    assert_eq!(c.name, " A3 Tool", "必须沿用磁盘上的原名（含前导空格）");
    assert!(c.existing, "应当被判定为已存在的合集");
    assert_eq!(c.kept_from_existing, 1, "原有成员 100 不在方案里，应被计为保留");
    assert_eq!(plan.stats.updated_collections, 1);
    assert_eq!(plan.stats.new_collections, 0);
    assert!(
        has_issue(&plan, "沿用原名"),
        "沿用原名必须留痕\n{}",
        issue_dump(&plan)
    );
}

/// 不变量：模糊匹配忽略大小写与 `_`/`-` 分隔符差异。
#[test]
fn fuzzy_match_ignores_case_separator_and_underscore() {
    let lib = library(
        vec![game(100, "Alpha")],
        vec![user_collection("uc-rpg", "RPG Collection", &[100])],
    );
    let plan = build(
        &lib,
        r#"{"collections":[{"name":"rpg_collection","appids":[100]}]}"#,
        "ai",
        &PlanOptions::default(),
    )
    .unwrap();

    assert_eq!(plan.collections[0].name, "RPG Collection");
    assert!(plan.collections[0].existing);
}

// ---------------------------------------------------------------------------
// 「未归档」口径
// ---------------------------------------------------------------------------

/// 不变量（关键口径）：已经躺在现有**静态**合集里的游戏，不能被算作「未归档」。
#[test]
fn games_in_existing_static_collections_are_not_unassigned() {
    let lib = library(
        vec![game(1, "a"), game(2, "b"), game(3, "c")],
        vec![user_collection("uc-backlog", "Backlog", &[2, 3])],
    );
    let plan = build(
        &lib,
        r#"{"collections":[{"name":"Mine","appids":[1]}]}"#,
        "ai",
        &PlanOptions::default(),
    )
    .unwrap();

    assert!(
        plan.unassigned.is_empty(),
        "2/3 已被现有静态合集收录，不该算未归档，实际: {:?}",
        plan.unassigned
    );
    assert_eq!(plan.stats.unassigned_games, 0);
    assert_eq!(plan.stats.already_filed_games, 2);
    assert!(
        !has_issue(&plan, "既不在本方案里"),
        "不该报假缺口\n{}",
        issue_dump(&plan)
    );
    assert!(
        has_issue(&plan, "没有遗漏"),
        "应当给出「没有遗漏」的结论\n{}",
        issue_dump(&plan)
    );
}

/// 不变量：口径跟着**当前**归属走——把游戏从现有合集里去掉，它必须重新变成未归档。
#[test]
fn unassigned_is_recomputed_when_membership_changes() {
    let games = || vec![game(1, "a"), game(2, "b"), game(3, "c")];
    let ai = r#"{"collections":[{"name":"Mine","appids":[1]}]}"#;

    let filed = library(games(), vec![user_collection("uc-b", "Backlog", &[2, 3])]);
    let p1 = build(&filed, ai, "ai", &PlanOptions::default()).unwrap();
    assert_eq!(p1.unassigned, Vec::<u32>::new());

    let half = library(games(), vec![user_collection("uc-b", "Backlog", &[2])]);
    let p2 = build(&half, ai, "ai", &PlanOptions::default()).unwrap();
    assert_eq!(
        p2.unassigned,
        vec![3],
        "3 已不在任何现有合集里，必须重新算作未归档"
    );
    assert_eq!(p2.stats.already_filed_games, 1);
}

/// 不变量：动态合集与系统合集**不**算「已归档」——它们不是用户的静态归属。
#[test]
fn games_only_in_dynamic_or_builtin_collections_still_count_as_unassigned() {
    let lib = library(
        vec![game(1, "a"), game(2, "b")],
        vec![
            dynamic_collection("uc-dyn", "动态", &[2]),
            builtin_collection("hidden", "hidden", &[2]),
        ],
    );
    let plan = build(
        &lib,
        r#"{"collections":[{"name":"Mine","appids":[1]}]}"#,
        "ai",
        &PlanOptions::default(),
    )
    .unwrap();

    assert_eq!(plan.unassigned, vec![2]);
    assert_eq!(plan.stats.already_filed_games, 0);
    assert_eq!(plan.stats.unassigned_games, 1);
    assert!(has_issue(&plan, "既不在本方案里"));
}

/// 不变量：AI 自报的 `unassigned` 只能**收窄交集**，不得塞进库里没有的 appid。
#[test]
fn declared_unassigned_is_intersected_with_the_library() {
    let lib = library(vec![game(1, "a"), game(2, "b"), game(3, "c")], Vec::new());
    let plan = build(
        &lib,
        r#"{"collections":[{"name":"Mine","appids":[1]}],"unassigned":[2,999999]}"#,
        "ai",
        &PlanOptions::default(),
    )
    .unwrap();

    assert_eq!(
        plan.unassigned,
        vec![2, 3],
        "999999 不在库里必须被忽略；2/3 是真实缺口且不得重复"
    );
}

// ---------------------------------------------------------------------------
// 丢弃规则
// ---------------------------------------------------------------------------

/// 不变量：清洗后为空的合集必须被丢弃（含「字面为空」与「成员全是幻觉」两种）。
#[test]
fn collections_that_end_up_empty_are_dropped() {
    let lib = library(vec![game(100, "Alpha")], Vec::new());
    let plan = build(
        &lib,
        r#"{"collections":[
             {"name":"字面空","appids":[]},
             {"name":"全幻觉","appids":[999]},
             {"name":"正常","appids":[100]}
           ]}"#,
        "ai",
        &PlanOptions::default(),
    )
    .unwrap();

    assert_eq!(plan.collections.len(), 1);
    assert_eq!(plan.collections[0].name, "正常");
    assert_eq!(plan.stats.collection_count, 1);
    assert!(has_issue(&plan, "「字面空」清洗后为空"), "{}", issue_dump(&plan));
    assert!(has_issue(&plan, "「全幻觉」清洗后为空"), "{}", issue_dump(&plan));
}

/// 不变量：`allow_empty=true` 时空的合集才允许留下（反向验证默认丢弃是刻意的）。
#[test]
fn allow_empty_true_keeps_empty_collections() {
    let lib = library(vec![game(100, "Alpha")], Vec::new());
    let opts = PlanOptions {
        keep_unknown: false,
        allow_empty: true,
    };
    let plan = build(
        &lib,
        r#"{"collections":[
             {"name":"字面空","appids":[]},
             {"name":"全幻觉","appids":[999]},
             {"name":"正常","appids":[100]}
           ]}"#,
        "ai",
        &opts,
    )
    .unwrap();

    assert_eq!(plan.collections.len(), 3);
    assert_eq!(
        plan.collections
            .iter()
            .find(|c| c.name == "字面空")
            .unwrap()
            .appids,
        Vec::<u32>::new()
    );
}

/// 不变量：与 Steam 系统合集名（favorite / hidden）冲突的合集必须被丢弃并报 error。
#[test]
fn collections_clashing_with_builtin_names_are_dropped_with_error() {
    let lib = library(vec![game(100, "Alpha")], Vec::new());
    let plan = build(
        &lib,
        r#"{"collections":[
             {"name":"favorite","appids":[100]},
             {"name":"hidden","appids":[100]},
             {"name":"正常","appids":[100]}
           ]}"#,
        "ai",
        &PlanOptions::default(),
    )
    .unwrap();

    assert_eq!(plan.collections.len(), 1);
    assert_eq!(plan.collections[0].name, "正常");
    assert_eq!(
        count_issues_at(&plan, "error", "系统合集冲突"),
        2,
        "两个冲突名都应当报 error 级\n{}",
        issue_dump(&plan)
    );
}

// ---------------------------------------------------------------------------
// 统计
// ---------------------------------------------------------------------------

/// 不变量：统计口径要把「被本方案收录的游戏」的时长与体积如实累加。
#[test]
fn stats_sum_playtime_and_size_of_assigned_games_only() {
    let lib = library(
        vec![
            game_with(100, "Alpha", 600, 10 * 1024 * 1024),
            game_with(200, "Beta", 120, 2 * 1024 * 1024),
            game_with(300, "Gamma", 9999, 99 * 1024 * 1024),
        ],
        vec![user_collection("uc-x", "Filed", &[300])],
    );
    let plan = build(
        &lib,
        r#"{"collections":[{"name":"Mine","appids":[100,200]}]}"#,
        "ai",
        &PlanOptions::default(),
    )
    .unwrap();

    assert_eq!(plan.stats.assigned_games, 2);
    assert_eq!(plan.stats.assigned_playtime_minutes, 720);
    assert_eq!(plan.stats.assigned_size_bytes, 12 * 1024 * 1024);
    assert_eq!(plan.stats.total_games, 3);
    assert_eq!(plan.stats.already_filed_games, 1);
    assert_eq!(plan.stats.unassigned_games, 0);
}
