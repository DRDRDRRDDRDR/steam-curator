//! 供 CLI 与 GUI 共用的高层操作。
//!
//! 每个 `do_*` 函数做一件事，返回一段**可以直接给人看的中文日志**。
//! CLI 把它们逐行打印；GUI 把它们塞进日志文本框 —— 两边共享同一套行为，
//! 不会出现「命令行里对、图形界面里不一样」的偏差。

use std::path::{Path, PathBuf};

use crate::apply::{self, ApplyOptions, ApplyOutcome};
use crate::model::{Library, Plan};
use crate::plan::{self, PlanOptions};
use crate::preview::{self, PreviewInput};
use crate::prompt::{self, PromptOptions};
use crate::scan::{self, ScanOptions};
use crate::util;

pub const F_LIBRARY: &str = "library.json";
pub const F_CSV: &str = "library.csv";
pub const F_PROMPT: &str = "AI-PROMPT.md";
pub const F_AI_PLAN: &str = "ai-plan.json";
pub const F_PLAN: &str = "plan.json";
pub const F_REPORT: &str = "report.html";
pub const F_APPLY: &str = "apply-report.json";

/// 一次操作的结果：可读日志 + 结构化产物（GUI 想拿去做进一步展示时用）。
#[derive(Default)]
pub struct Outcome {
    pub log: Vec<String>,
    pub library: Option<Library>,
    pub plan: Option<Plan>,
    pub apply: Option<ApplyOutcome>,
}

impl Outcome {
    pub fn new() -> Self {
        Outcome::default()
    }

    fn say(&mut self, line: impl Into<String>) -> &mut Self {
        self.log.push(line.into());
        self
    }

    pub fn text(&self) -> String {
        self.log.join("\n")
    }
}

// ---------------------------------------------------------------------------
// 文件级辅助
// ---------------------------------------------------------------------------

pub fn load_library(out: &Path) -> Result<Library, String> {
    let path = out.join(F_LIBRARY);
    let text = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "读取 {} 失败: {}\n请先执行「扫描 Steam 库」。",
            path.display(),
            e
        )
    })?;
    serde_json::from_str(&text).map_err(|e| format!("{} 解析失败: {}", path.display(), e))
}

pub fn load_plan_optional(out: &Path) -> Option<Plan> {
    std::fs::read_to_string(out.join(F_PLAN))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
}

pub fn load_apply_optional(out: &Path) -> Option<ApplyOutcome> {
    std::fs::read_to_string(out.join(F_APPLY))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
}

pub fn read_prompt(out: &Path) -> Option<String> {
    std::fs::read_to_string(out.join(F_PROMPT)).ok()
}

/// 把 AI 的回复原样落盘（GUI 的「保存并校验」按钮用）。
pub fn save_ai_reply(out: &Path, text: &str) -> Result<PathBuf, String> {
    util::ensure_dir(out)?;
    let path = out.join(F_AI_PLAN);
    util::write_file(&path, text)?;
    Ok(path)
}

pub fn write_report(
    out: &Path,
    lib: &Library,
    plan: Option<&Plan>,
    outcome: Option<&ApplyOutcome>,
) -> Result<PathBuf, String> {
    let html = preview::build(PreviewInput {
        library: lib,
        plan,
        outcome,
        title: if plan.is_some() {
            "Steam 库整理预览".to_string()
        } else {
            "Steam 库现状".to_string()
        },
    });
    let path = out.join(F_REPORT);
    util::write_file(&path, &html)?;
    Ok(path)
}

pub fn open_in_browser(path: &Path) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "", &path.to_string_lossy()])
            .spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
}

pub fn open_directory(path: &Path) {
    let _ = util::ensure_dir(path);
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer")
            .arg(path.as_os_str())
            .spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
}

// ---------------------------------------------------------------------------
// 各步骤
// ---------------------------------------------------------------------------

pub struct ScanArgs<'a> {
    pub steam_dir: Option<&'a str>,
    pub user: Option<&'a str>,
    pub write_csv: bool,
    /// 是否把「已拥有但未安装」的游戏也纳入（默认开；CLI 用 `--installed-only` 关掉）。
    ///
    /// 关掉之后只统计已安装的，省掉读 45 MB 的 `appinfo.vdf`。
    pub include_uninstalled: bool,
}

pub fn do_scan(out: &Path, args: &ScanArgs) -> Result<Outcome, String> {
    util::ensure_dir(out)?;
    let mut o = Outcome::new();
    o.say("→ 正在扫描 Steam 库…");

    let scanned = scan::run(&ScanOptions {
        steam_dir: args.steam_dir.map(|s| s.to_string()),
        user: args.user.map(|s| s.to_string()),
        include_uninstalled: args.include_uninstalled,
    })?;
    let lib = &scanned.library;

    o.say(format!(
        "  Steam 根目录：{}（{}）",
        scanned.steam_root.display(),
        scanned.found_by
    ));
    o.say(format!(
        "  账号：{}（steam3 {}）",
        lib.account.persona_name.clone().unwrap_or_else(|| "?".into()),
        lib.account.steam3_id
    ));
    for r in &lib.library_roots {
        o.say(format!(
            "  库根 [{}] {} — {} 个游戏{}",
            r.index,
            r.path,
            r.game_count,
            if r.reachable { "" } else { "（不可达）" }
        ));
    }
    let s = scan::summarize(lib);
    o.say(format!(
        "  共 {} 款游戏（已安装 {} · 已拥有未安装 {}）· {} · 累计 {}",
        s.games,
        s.installed,
        s.uninstalled,
        util::fmt_size(s.total_size),
        util::fmt_duration(s.total_playtime_minutes)
    ));
    o.say(format!(
        "  从未启动 {} · 一年以上没玩 {} · 尚未归档 {} · 已有合集 {} 个",
        s.never_launched, s.dormant, s.unassigned, s.collection_count
    ));

    let lib_json = serde_json::to_string_pretty(lib).map_err(|e| e.to_string())?;
    util::write_file(&out.join(F_LIBRARY), &lib_json)?;
    o.say(format!("  ✔ {}", out.join(F_LIBRARY).display()));

    if args.write_csv {
        util::write_file(&out.join(F_CSV), &scan::to_csv(lib))?;
        o.say(format!("  ✔ {}", out.join(F_CSV).display()));
    }
    for w in &lib.warnings {
        o.say(format!("  ⚠ {}", w));
    }
    o.library = Some(scanned.library);
    Ok(o)
}

pub struct PromptArgs<'a> {
    pub lang: &'a str,
    pub target: usize,
    pub max_name: usize,
    pub include_json: bool,
    pub mode: &'a str,
    /// 游戏表最多列多少行；0 = 不限
    pub max_games: usize,
}

impl Default for PromptArgs<'_> {
    fn default() -> Self {
        PromptArgs {
            lang: "zh",
            target: 12,
            max_name: 14,
            include_json: true,
            mode: "auto",
            max_games: 0,
        }
    }
}

pub fn do_prompt(out: &Path, args: &PromptArgs) -> Result<Outcome, String> {
    let mut o = Outcome::new();
    let lib = load_library(out)?;

    // 紧凑 JSON 是同一份数据的第二遍，库一大就把 prompt 撑成两倍。
    // 本机实测 2031 款时，表格本身已经约 240 KB，再附 JSON 会到 480 KB —— 自动关掉并说明。
    let auto_off_json = args.include_json && lib.games.len() > 200;
    let include_json = args.include_json && !auto_off_json;

    let text = prompt::build(
        &lib,
        &PromptOptions {
            lang: args.lang.to_string(),
            target_collections: args.target,
            max_name_chars: args.max_name,
            include_json,
            mode: args.mode.to_string(),
            max_games: args.max_games,
        },
    );
    let path = out.join(F_PROMPT);
    util::write_file(&path, &text)?;

    let extend = prompt::is_extend_mode(&lib, args.mode);
    o.say(format!("✔ 已生成 {}", path.display()));
    if auto_off_json {
        o.say(format!(
            "  ℹ 库里有 {} 款游戏，已自动省略附录的紧凑 JSON（否则 prompt 会翻倍）",
            lib.games.len()
        ));
    }
    o.say(format!(
        "  长度 {} 字符，约 {} token。",
        text.chars().count(),
        text.len() / 3
    ));
    if text.len() > 300_000 {
        o.say(format!(
            "  ⚠ prompt 偏大（{} KB）。若目标模型上下文吃不下，用 `--max-games 500` 只列未归档优先的前 500 款。",
            text.len() / 1024
        ));
    }
    o.say(format!(
        "  模式：{}",
        if extend {
            "extend（你已有成体系的合集，只做补全，不推倒重来）"
        } else {
            "fresh（从零设计一套分类）"
        }
    ));
    o.say("");
    o.say("下一步：打开该文件整份复制给任意 AI，让它按文件里的 JSON 契约回答，");
    o.say("然后把 AI 的回复整段粘到下面的输入框（含 ``` 围栏也没关系），");
    o.say("再点「校验 AI 回复」。");
    o.library = Some(lib);
    Ok(o)
}

pub fn do_plan(
    out: &Path,
    ai_text: &str,
    source_label: &str,
    keep_unknown: bool,
    allow_empty: bool,
) -> Result<Outcome, String> {
    let mut o = Outcome::new();
    let lib = load_library(out)?;

    let built = plan::build(
        &lib,
        ai_text,
        source_label,
        &PlanOptions {
            keep_unknown,
            allow_empty,
        },
    )?;

    o.say(format!("✔ 方案校验完成：{} 个合集", built.collections.len()));
    o.say(format!(
        "  本方案覆盖 {}/{} 款游戏；另有 {} 款已由你现有的合集收录；既无方案也无现有归属的 {} 款",
        built.stats.assigned_games,
        built.stats.total_games,
        built.stats.already_filed_games,
        built.stats.unassigned_games
    ));
    o.say(format!(
        "  新建 {} 个，更新 {} 个",
        built.stats.new_collections, built.stats.updated_collections
    ));
    if !built.issues.is_empty() {
        o.say("");
        o.say("  校验报告：");
        for issue in &built.issues {
            let mark = match issue.level.as_str() {
                "error" => "✖",
                "warn" => "⚠",
                _ => "·",
            };
            o.say(format!("    {} {}", mark, issue.message));
        }
    }
    o.say("");
    for c in &built.collections {
        let mut flags = Vec::new();
        if c.existing {
            flags.push(if c.kept_from_existing > 0 {
                format!("已有合集，保留原有 {} 款", c.kept_from_existing)
            } else {
                "已有合集".to_string()
            });
        } else {
            flags.push("新建".to_string());
        }
        o.say(format!(
            "  · {} — {} 款（{}）",
            c.name,
            c.appids.len(),
            flags.join("，")
        ));
    }

    util::write_file(
        &out.join(F_PLAN),
        &serde_json::to_string_pretty(&built).map_err(|e| e.to_string())?,
    )?;
    o.say("");
    o.say(format!("  ✔ {}", out.join(F_PLAN).display()));

    let report = write_report(out, &lib, Some(&built), None)?;
    o.say(format!("  ✔ {}", report.display()));
    o.plan = Some(built);
    o.library = Some(lib);
    Ok(o)
}

pub fn do_preview(out: &Path) -> Result<Outcome, String> {
    let mut o = Outcome::new();
    let lib = load_library(out)?;
    let plan = load_plan_optional(out);
    let outcome = load_apply_optional(out);
    let path = write_report(out, &lib, plan.as_ref(), outcome.as_ref())?;
    o.say(format!("✔ 已生成预览报告 {}", path.display()));
    o.say("  报告是完全自包含的 HTML，双击即可看，断网也能看。");
    o.library = Some(lib);
    o.plan = plan;
    Ok(o)
}

pub struct ApplyArgs<'a> {
    pub write: bool,
    pub force: bool,
    pub mode: &'a str,
    pub prune: bool,
}

pub fn do_apply(out: &Path, args: &ApplyArgs) -> Result<Outcome, String> {
    let mut o = Outcome::new();
    let lib = load_library(out)?;
    let plan = load_plan_optional(out).ok_or_else(|| {
        format!(
            "找不到 {}。请先完成「校验 AI 回复」这一步。",
            out.join(F_PLAN).display()
        )
    })?;

    let opts = ApplyOptions {
        write: args.write,
        force: args.force,
        mode: args.mode.to_string(),
        prune: args.prune,
    };
    let steam_root = PathBuf::from(&lib.steam_root);
    let steam3 = lib.account.steam3_id.clone();

    o.say(format!(
        "→ {} Steam 合集…",
        if args.write {
            "写入"
        } else {
            "演练（不会改动任何文件）"
        }
    ));
    let outcome = apply::run(&plan, &steam_root, &steam3, out, &opts)?;

    if let Some(reason) = &outcome.blocked_reason {
        o.say("");
        o.say(format!("✖ 已阻止写入：{}", reason));
    }

    let (mut created, mut updated, mut unchanged, mut deleted, mut skipped) = (0, 0, 0, 0, 0);
    for a in &outcome.actions {
        match a.kind.as_str() {
            "create" => created += 1,
            "update" => updated += 1,
            "unchanged" => unchanged += 1,
            "delete" => deleted += 1,
            _ => skipped += 1,
        }
    }
    o.say("");
    o.say(format!("  目标文件：{}", outcome.namespace_path));
    o.say(format!(
        "  命名空间版本：{} → {}",
        outcome.old_namespace_version, outcome.new_namespace_version
    ));
    o.say(format!(
        "  新建 {} · 更新 {} · 无变化 {} · 删除 {} · 跳过 {}",
        created, updated, unchanged, deleted, skipped
    ));
    o.say("");
    for a in &outcome.actions {
        let tag = match a.kind.as_str() {
            "create" => "[新建]",
            "update" => "[更新]",
            "unchanged" => "[不变]",
            "delete" => "[删除]",
            _ => "[跳过]",
        };
        let mut detail = format!("{} {} — {} 款", tag, a.name, a.added.len());
        if !a.added_delta.is_empty() {
            detail.push_str(&format!("，新增 {}", a.added_delta.len()));
        }
        if !a.kept_from_existing.is_empty() {
            detail.push_str(&format!("，保留原有 {}", a.kept_from_existing.len()));
        }
        if !a.dropped.is_empty() {
            detail.push_str(&format!("，移除 {}", a.dropped.len()));
        }
        o.say(format!("  {}", detail));
        if a.kind == "skip" {
            if let Some(note) = &a.note {
                o.say(format!("        {}", note));
            }
        }
    }
    if let Some(dir) = &outcome.backup_dir {
        o.say("");
        o.say(format!("  ✔ 备份已保存到 {}", dir));
    }

    let report = write_report(out, &lib, Some(&plan), Some(&outcome))?;
    o.say(format!("  ✔ 预览报告 {}", report.display()));
    util::write_file(
        &out.join(F_APPLY),
        &serde_json::to_string_pretty(&outcome).map_err(|e| e.to_string())?,
    )?;

    o.say("");
    if !args.write {
        o.say("这是演练结果，Steam 里没有任何改动。");
        o.say("确认无误后点「正式写回」（执行前必须完全退出 Steam，含托盘图标）。");
    } else if outcome.blocked_reason.is_none() {
        o.say("已写入。请启动 Steam 打开「库」界面查看效果。");
        o.say("如需回滚：点「回滚最近备份」。");
    }
    o.apply = Some(outcome);
    o.library = Some(lib);
    o.plan = Some(plan);
    Ok(o)
}

pub fn do_restore(out: &Path, backup: Option<&str>, latest: bool, confirm: bool) -> Result<Outcome, String> {
    let mut o = Outcome::new();
    let restored = apply::restore(out, backup, latest, !confirm)?;
    o.say(format!(
        "{} 共 {} 个文件：",
        if confirm { "已还原" } else { "将还原" },
        restored.len()
    ));
    for line in &restored {
        o.say(format!("  {}", line));
    }
    if !confirm {
        o.say("");
        o.say("（演练模式）确认无误后点「确认回滚」真正执行。");
    }
    Ok(o)
}

/// 只读概览：不写任何文件。
pub fn do_inspect(steam_dir: Option<&str>, user: Option<&str>) -> Result<Outcome, String> {
    let mut o = Outcome::new();
    let scanned = scan::run(&ScanOptions {
        steam_dir: steam_dir.map(|s| s.to_string()),
        user: user.map(|s| s.to_string()),
        // 概览也把「已拥有但未安装」算进来，这样 inspect 与 scan 的数字对得上
        include_uninstalled: true,
    })?;
    let lib = &scanned.library;
    let s = scan::summarize(lib);
    o.say(format!("Steam 根目录: {}", scanned.steam_root.display()));
    o.say(format!(
        "账号: {} (steam3 {})",
        lib.account.persona_name.clone().unwrap_or_else(|| "?".into()),
        lib.account.steam3_id
    ));
    o.say("");
    o.say(format!(
        "游戏 {} 款 · {} · 累计 {}",
        s.games,
        util::fmt_size(s.total_size),
        util::fmt_duration(s.total_playtime_minutes)
    ));
    o.say(format!(
        "从未启动 {} · 浅尝 {} · 沉寂 {} · 已隐藏 {} · 收藏 {}",
        s.never_launched, s.barely_played, s.dormant, s.hidden, s.favorite
    ));
    o.say("");
    o.say(format!("现有合集（{} 个）：", lib.existing_collections.len()));
    for c in &lib.existing_collections {
        let kind = if c.is_builtin {
            "系统"
        } else if c.is_dynamic {
            "动态"
        } else {
            "静态"
        };
        o.say(format!("  [{}] {} — {} 款", kind, c.name, c.appids.len()));
    }
    match crate::steam::read_cloud(&scanned.steam_root, &scanned.steam3) {
        Ok(cloud) => {
            o.say("");
            o.say(format!("合集文件: {}", cloud.namespace_path.display()));
            o.say(format!(
                "命名空间版本 {} · 文件内最大条目版本 {}",
                cloud.namespace_version, cloud.max_entry_version
            ));
        }
        Err(e) => {
            o.say("");
            o.say(format!("⚠ {}", e));
        }
    }
    o.say(format!(
        "Steam 进程: {}",
        if crate::steam::steam_running() {
            "运行中（写入会被拒绝）"
        } else {
            "未运行（可安全写入）"
        }
    ));
    o.library = Some(scanned.library);
    Ok(o)
}
