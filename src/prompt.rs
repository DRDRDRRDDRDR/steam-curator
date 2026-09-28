//! `prompt`：把扫描数据 + 输出契约合成一份可直接粘给任意 AI 的 prompt。

use crate::model::{Game, Library};
use crate::scan;
use crate::util;

pub struct PromptOptions {
    pub lang: String,
    pub target_collections: usize,
    pub max_name_chars: usize,
    /// 是否附带一份紧凑 JSON（部分模型对 JSON 输入更稳）
    pub include_json: bool,
    /// auto | fresh | extend
    pub mode: String,
}

impl Default for PromptOptions {
    fn default() -> Self {
        PromptOptions {
            lang: "zh".into(),
            target_collections: 12,
            max_name_chars: 14,
            include_json: true,
            mode: "auto".into(),
        }
    }
}

/// 已有 20 个以上静态合集就说明用户已经建立过自己的整理体系，
/// 这时该做的是「补全」而不是「推倒重来」。
const EXTEND_THRESHOLD: usize = 20;

pub fn is_extend_mode(lib: &Library, mode: &str) -> bool {
    match mode {
        "extend" => true,
        "fresh" => false,
        _ => {
            lib.existing_collections
                .iter()
                .filter(|c| !c.is_builtin && !c.is_dynamic)
                .count()
                >= EXTEND_THRESHOLD
        }
    }
}

pub fn build(lib: &Library, opts: &PromptOptions) -> String {
    if opts.lang.eq_ignore_ascii_case("en") {
        build_en(lib, opts)
    } else {
        build_zh(lib, opts)
    }
}

fn build_zh(lib: &Library, opts: &PromptOptions) -> String {
    let s = scan::summarize(lib);
    let extend = is_extend_mode(lib, &opts.mode);
    let mut o = String::new();

    o.push_str("# Steam 库整理任务\n\n");
    o.push_str("你是一个游戏库整理助手。下面是一份**真实的本地 Steam 游戏库清单**，");
    o.push_str("请把它整理成一套可直接使用的 Steam「合集」。\n\n");

    // ---- 数据概览 ----
    o.push_str("## 一、库概览\n\n");
    o.push_str(&format!(
        "- 已安装游戏：**{}** 款\n- 占用磁盘：**{}**\n- 累计游玩：**{}**\n",
        s.games,
        util::fmt_size(s.total_size),
        util::fmt_duration(s.total_playtime_minutes)
    ));
    o.push_str(&format!(
        "- 从未启动：**{}** 款\n- 启动过但不足 1 小时：**{}** 款\n- 超过一年没玩：**{}** 款\n",
        s.never_launched, s.barely_played, s.dormant
    ));
    o.push_str(&format!(
        "- 目前未归入任何合集：**{}** 款\n- 已有用户合集：**{}** 个\n",
        s.unassigned, s.collection_count
    ));
    if !lib.existing_collections.is_empty() {
        let mut names: Vec<String> = lib
            .existing_collections
            .iter()
            .filter(|c| !c.is_builtin && !c.is_dynamic)
            .map(|c| c.name.clone())
            .collect();
        names.sort();
        if !names.is_empty() {
            let total = names.len();
            names.truncate(36);
            o.push_str(&format!(
                "- 已有用户合集：**{}** 个{}：{}\n",
                total,
                if total > names.len() { "（下面只列前 36 个）" } else { "" },
                names.join("、")
            ));
        }
    }
    o.push('\n');

    // 用户已有规模可观的整理体系时，明确告诉模型「别推倒重来」
    if extend {
        o.push_str("> ⚠️ **这个库已经有成体系的整理了。**它的主人按「游戏系列 / 厂商」");
        o.push_str("建了上百个合集，覆盖了库里绝大多数游戏。\n");
        o.push_str("> 你的任务是**补全**，不是重建：只需要处理下面「尚未归入任何合集」的那批游戏，");
        o.push_str("以及指出明显缺失的合集。**不要重新设计整套分类体系。**\n\n");
    }

    // ---- 游戏清单 ----
    o.push_str("## 二、游戏清单\n\n");
    o.push_str("| appid | 名称 | 系列/厂商 | 时长(h) | 体积(GB) | 最后游玩 | 状态 | 现有合集 |\n");
    o.push_str("|---:|---|---|---:|---:|---|---|---|\n");
    for g in &lib.games {
        let last = match (g.days_since_played, &g.last_played_local) {
            (Some(d), Some(_)) => {
                let date = g.last_played_local.clone().unwrap_or_default();
                let date = date.split(' ').next().unwrap_or("").to_string();
                format!("{}（{}天前）", date, d)
            }
            _ => "从未启动".to_string(),
        };
        let mut flags: Vec<&str> = Vec::new();
        if g.never_launched {
            flags.push("未启动");
        } else if g.barely_played {
            flags.push("浅尝");
        }
        if g.dormant {
            flags.push("沉寂");
        }
        if g.favorite {
            flags.push("收藏");
        }
        if g.hidden {
            flags.push("已隐藏");
        }
        if g.achievements_total > 0 && g.achievements_unlocked >= g.achievements_total {
            flags.push("全成就");
        }
        let colls = if g.collections.is_empty() {
            "—".to_string()
        } else {
            g.collections.join(" / ")
        };
        o.push_str(&format!(
            "| {} | {} | {} | {:.1} | {:.2} | {} | {} | {} |\n",
            g.appid,
            g.name.replace('|', "\\|"),
            studio_label(g).replace('|', "\\|"),
            util::hours(g.playtime_minutes),
            util::gb(g.size_bytes),
            last,
            if flags.is_empty() {
                "正常".to_string()
            } else {
                flags.join("+")
            },
            colls.replace('|', "\\|"),
        ));
    }
    o.push('\n');

    // ---- 分析素材 ----
    o.push_str("## 三、可直接利用的信号\n\n");
    let mut hogs: Vec<_> = lib.games.iter().collect();
    hogs.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
    o.push_str("**体积榜前 8（占盘大户，值得优先归类）**：\n");
    for g in hogs.iter().take(8) {
        o.push_str(&format!(
            "- {}（{}，玩了 {}）\n",
            g.name,
            util::fmt_size(g.size_bytes),
            util::fmt_duration(g.playtime_minutes)
        ));
    }
    o.push('\n');

    let never: Vec<_> = lib.games.iter().filter(|g| g.never_launched).collect();
    if !never.is_empty() {
        o.push_str(&format!(
            "**从未启动（{} 款，建议专门归一类）**：\n",
            never.len()
        ));
        let names: Vec<String> = never
            .iter()
            .map(|g| format!("{}[{}]", g.name, g.appid))
            .collect();
        o.push_str(&format!("{}\n\n", names.join("、")));
    }

    let mut recent: Vec<_> = lib
        .games
        .iter()
        .filter(|g| g.days_since_played.map(|d| d <= 60).unwrap_or(false))
        .collect();
    recent.sort_by_key(|g| g.days_since_played);
    if !recent.is_empty() {
        o.push_str("**最近 60 天玩过（说明是当下主力）**：\n");
        let names: Vec<String> = recent
            .iter()
            .map(|g| format!("{}({}天前)", g.name, g.days_since_played.unwrap_or(0)))
            .collect();
        o.push_str(&format!("{}\n\n", names.join("、")));
    }

    // 官方 series 字段分组：用户既有的整理体系就是按系列来的，这个信号最省事
    let mut by_franchise: std::collections::BTreeMap<String, Vec<&Game>> =
        std::collections::BTreeMap::new();
    for g in &lib.games {
        for f in &g.franchises {
            by_franchise.entry(f.clone()).or_default().push(g);
        }
    }
    let groups: Vec<(&String, &Vec<&Game>)> = by_franchise
        .iter()
        .filter(|(_, v)| v.len() >= 2)
        .collect();
    if !groups.is_empty() {
        o.push_str("**官方「系列」字段分组（Steam 自己标注的同系列作品，可直接作为一个合集）**：\n");
        for (f, games) in groups {
            let names: Vec<String> = games
                .iter()
                .map(|g| format!("{}[{}]", g.name, g.appid))
                .collect();
            o.push_str(&format!("- {}：{}\n", f, names.join("、")));
        }
        o.push('\n');
    }

    // 未归档清单：补全模式下的核心目标
    let unassigned: Vec<&Game> = lib
        .games
        .iter()
        .filter(|g| g.collections.is_empty())
        .collect();
    if !unassigned.is_empty() {
        o.push_str(&format!(
            "**尚未归入任何合集（{} 款）{}**：\n",
            unassigned.len(),
            if extend { " —— 本次整理的主要目标" } else { "" }
        ));
        for g in &unassigned {
            o.push_str(&format!(
                "- {}(appid {}) · {} · 玩过 {} · 最后游玩 {}\n",
                g.name,
                g.appid,
                util::fmt_size(g.size_bytes),
                util::fmt_duration(g.playtime_minutes),
                g.last_played_local.clone().unwrap_or_else(|| "从未启动".into())
            ));
        }
        o.push('\n');
    }

    // ---- 输出契约 ----
    o.push_str("## 四、输出要求（必须严格遵守）\n\n");
    o.push_str("只输出**一个 JSON 对象**，不要任何解释文字，不要 Markdown 代码块围栏。\n\n");
    o.push_str("```\n");
    o.push_str("{\n");
    o.push_str("  \"collections\": [\n");
    o.push_str("    {\n");
    o.push_str("      \"name\": \"合集名\",\n");
    o.push_str("      \"description\": \"一句话说明这个合集收什么\",\n");
    o.push_str("      \"appids\": [123, 456]\n");
    o.push_str("    }\n");
    o.push_str("  ],\n");
    o.push_str("  \"unassigned\": [789],\n");
    o.push_str("  \"notes\": \"给用户看的额外说明，可省略\"\n");
    o.push_str("}\n");
    o.push_str("```\n\n");
    if extend {
        o.push_str("硬性规则：\n\n");
        o.push_str("1. `appids` 只能使用上表中出现过的 appid，**绝对不许编造或猜 appid**。\n");
        o.push_str("2. 优先处理「尚未归入任何合集」的那批游戏，让它们尽可能各有归属。\n");
        o.push_str(&format!(
            "3. 只提出 **2~{} 个**要新建的合集就够，不要多。宁可少而准。\n",
            (opts.target_collections / 3).max(3)
        ));
        o.push_str(&format!(
            "4. 合集名不超过 {} 个字，不要 emoji，不要编号前缀。「其他」「杂项」这类垃圾桶命名一律不要。\n",
            opts.max_name_chars
        ));
        o.push_str("5. **可以复用现有同名合集**：如果你认为某款游戏属于上面列出的某个已有合集，");
        o.push_str("直接用那个名字即可 —— 工具会把新成员并入已有合集，原有成员不会丢。\n");
        o.push_str("6. 一个游戏可以出现在多个合集里；但同一款游戏不要在一个合集里重复出现。\n");
        o.push_str("7. 如果某款游戏确实无处可归（例如 Steamworks 运行库、纯工具类），放进 `unassigned`，不要硬塞。\n");
        o.push_str("8. `notes` 里用两三句话说明你的判断依据和发现的缺口。\n");
    } else {
        o.push_str("硬性规则：\n\n");
        o.push_str("1. `appids` 只能使用上表中出现过的 appid，**绝对不许编造或猜 appid**。\n");
        o.push_str(&format!(
            "2. 目标合集数量约 {} 个（{}~{} 个之间都算合理）。\n",
            opts.target_collections,
            opts.target_collections.saturating_sub(4),
            opts.target_collections + 4
        ));
        o.push_str(&format!(
            "3. 合集名不超过 {} 个字，不要 emoji，不要「01.」「其他」这类编号或垃圾桶式命名。\n",
            opts.max_name_chars
        ));
        o.push_str("4. 一个游戏可以同时出现在多个合集里（例如既属于「合作」又属于「射击」），这是允许且推荐的。\n");
        o.push_str("5. 同一款游戏不要在一个合集里重复出现。\n");
        o.push_str("6. 分类依据要**对「下次打开 Steam 想玩什么」有用**，而不是照抄商店标签。");
        o.push_str("优先考虑：玩法类型、单局时长、是否适合多人/联机、是否适合放松或沉浸、是否长期沉迷（长线养成/刷装）。\n");
        o.push_str("7. 把上面「从未启动」「沉寂」的游戏尽量单独归类，方便用户决定要不要清理磁盘。\n");
        o.push_str("8. 如果确实有游戏不适合归入任何合集，放进 `unassigned` 而不是硬塞。\n");
        o.push_str("9. 合集名用中文（专有名词如 Roguelike、FPS 可保留英文）。\n");
    }

    if opts.include_json {
        o.push_str("\n## 五、同一份数据的紧凑 JSON（便于你精确核对 appid）\n\n");
        o.push_str("```json\n");
        o.push_str(&compact_json(lib));
        o.push_str("\n```\n");
    }

    o
}

/// 表格里的「系列/厂商」标签：优先取 Steam 官方系列字段，其次开发商。
fn studio_label(g: &crate::model::Game) -> String {
    if !g.franchises.is_empty() {
        g.franchises.join(" / ")
    } else if !g.developers.is_empty() {
        g.developers.join(" / ")
    } else {
        "—".to_string()
    }
}

fn compact_json(lib: &Library) -> String {    let mut items: Vec<serde_json::Value> = Vec::new();
    for g in &lib.games {
        items.push(serde_json::json!({
            "appid": g.appid,
            "name": g.name,
            "h": util::hours(g.playtime_minutes),
            "gb": util::gb(g.size_bytes),
            "last": g.last_played_local.clone(),
            "never": g.never_launched,
            "dormant": g.dormant,
            "in": g.collections,
        }));
    }
    serde_json::to_string(&serde_json::json!({ "games": items })).unwrap_or_default()
}

fn build_en(lib: &Library, opts: &PromptOptions) -> String {
    let s = scan::summarize(lib);
    let mut o = String::new();
    o.push_str("# Steam library curation task\n\n");
    o.push_str("You are a game-library curator. Below is a **real local Steam library**.");
    o.push_str(" Turn it into a set of Steam collections.\n\n");
    o.push_str(&format!(
        "Games: **{}**  |  Disk: **{}**  |  Total playtime: **{}**\n",
        s.games,
        util::fmt_size(s.total_size),
        util::fmt_duration(s.total_playtime_minutes)
    ));
    o.push_str(&format!(
        "Never launched: **{}**  |  Played under 1h: **{}**  |  Idle > 1 year: **{}**  |  Unfiled: **{}**\n\n",
        s.never_launched, s.barely_played, s.dormant, s.unassigned
    ));
    o.push_str("| appid | name | series/studio | hours | GB | last played | state | collections |\n");
    o.push_str("|---:|---|---|---:|---:|---|---|---|\n");
    for g in &lib.games {
        let last = g
            .last_played_local
            .clone()
            .unwrap_or_else(|| "never".into());
        let mut flags: Vec<&str> = Vec::new();
        if g.never_launched {
            flags.push("never");
        } else if g.barely_played {
            flags.push("barely");
        }
        if g.dormant {
            flags.push("dormant");
        }
        if g.achievements_total > 0 && g.achievements_unlocked >= g.achievements_total {
            flags.push("100%");
        }
        o.push_str(&format!(
            "| {} | {} | {} | {:.1} | {:.2} | {} | {} | {} |\n",
            g.appid,
            g.name.replace('|', "\\|"),
            studio_label(g).replace('|', "\\|"),
            util::hours(g.playtime_minutes),
            util::gb(g.size_bytes),
            last,
            if flags.is_empty() {
                "ok".to_string()
            } else {
                flags.join("+")
            },
            if g.collections.is_empty() {
                "—".to_string()
            } else {
                g.collections.join(" / ")
            }
        ));
    }
    o.push_str("\n## Output contract\n\nReturn **one JSON object only**, no prose, no code fences.\n\n");
    o.push_str("```\n{\n  \"collections\": [\n    {\"name\": \"...\", \"description\": \"...\", \"appids\": [123]}\n  ],\n  \"unassigned\": [789],\n  \"notes\": \"...\"\n}\n```\n\n");
    o.push_str("Rules:\n\n");
    o.push_str("1. `appids` must come from the table above. Never invent appids.\n");
    o.push_str(&format!(
        "2. Aim for about {} collections.\n",
        opts.target_collections
    ));
    o.push_str(&format!(
        "3. Collection names at most {} characters, no emoji, no numeric prefixes, no catch-all \"Other\".\n",
        opts.max_name_chars
    ));
    o.push_str("4. A game may appear in several collections. Never repeat a game inside one collection.\n");
    o.push_str("5. Classify by *how the user will pick what to play next*, not by store tags.\n");
    o.push_str("6. Group never-launched / dormant games so the user can decide what to uninstall.\n");
    o.push_str("7. Put genuinely unfittable games in `unassigned` instead of forcing them.\n");
    o.push_str("\n## Compact JSON of the same data\n\n```json\n");
    o.push_str(&compact_json(lib));
    o.push_str("\n```\n");
    o
}
