//! steam-curator — Steam 库整理器
//!
//! 流水线：scan → prompt →（AI）→ plan → preview → apply
//!
//! 刻意不依赖 clap 等重量级 crate：整个二进制只用到 serde / serde_json，
//! 依赖树极小、编译快、也不需要 C 工具链。

#![allow(dead_code)]

mod apply;
mod model;
mod plan;
mod preview;
mod prompt;
mod scan;
mod steam;
mod util;
mod vdf;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use model::{Library, Plan};

const F_LIBRARY: &str = "library.json";
const F_CSV: &str = "library.csv";
const F_PROMPT: &str = "AI-PROMPT.md";
const F_AI_PLAN: &str = "ai-plan.json";
const F_PLAN: &str = "plan.json";
const F_REPORT: &str = "report.html";
const F_APPLY: &str = "apply-report.json";

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match run(&argv) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("\n[错误] {}\n", e);
            std::process::exit(1);
        }
    }
}

fn run(argv: &[String]) -> Result<(), String> {
    let parsed = parse_args(argv)?;
    let cmd = parsed.command.as_str();

    match cmd {
        "" | "help" => {
            print_help();
            Ok(())
        }
        "version" => {
            println!("steam-curator {}", VERSION);
            Ok(())
        }
        "scan" => cmd_scan(&parsed),
        "prompt" => cmd_prompt(&parsed),
        "plan" => cmd_plan(&parsed),
        "preview" => cmd_preview(&parsed),
        "apply" => cmd_apply(&parsed),
        "restore" => cmd_restore(&parsed),
        "inspect" => cmd_inspect(&parsed),
        "all" => {
            cmd_scan(&parsed)?;
            println!();
            cmd_prompt(&parsed)
        }
        other => Err(format!(
            "未知命令 `{}`。运行 `steam-curator help` 查看用法。",
            other
        )),
    }
}

// ---------------------------------------------------------------------------
// 参数解析
// ---------------------------------------------------------------------------

struct Parsed {
    command: String,
    opts: HashMap<String, String>,
    positional: Vec<String>,
}

impl Parsed {
    fn get(&self, key: &str) -> Option<&str> {
        self.opts.get(key).map(|s| s.as_str())
    }
    fn flag(&self, key: &str) -> bool {
        self.opts.contains_key(key)
    }
    fn out_dir(&self) -> PathBuf {
        PathBuf::from(self.get("out").unwrap_or("output"))
    }
}

/// 不需要跟值的开关
const BOOL_FLAGS: &[&str] = &[
    "help",
    "version",
    "write",
    "force",
    "prune",
    "open",
    "no-csv",
    "no-json",
    "latest",
    "keep-unknown",
    "allow-empty",
    "replace",
    "confirm",
];

fn canonical(key: &str) -> String {
    match key {
        "h" => "help".into(),
        "V" => "version".into(),
        "o" => "out".into(),
        "s" => "steam-dir".into(),
        "u" => "user".into(),
        "i" => "input".into(),
        other => other.to_string(),
    }
}

fn parse_args(argv: &[String]) -> Result<Parsed, String> {
    let mut command = String::new();
    let mut opts: HashMap<String, String> = HashMap::new();
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0usize;

    while i < argv.len() {
        let arg = &argv[i];
        if let Some(rest) = arg.strip_prefix("--") {
            let (raw_key, inline) = match rest.split_once('=') {
                Some((k, v)) => (k, Some(v.to_string())),
                None => (rest, None),
            };
            let key = canonical(raw_key);
            if let Some(v) = inline {
                opts.insert(key, v);
            } else if BOOL_FLAGS.contains(&key.as_str()) {
                opts.insert(key, "true".into());
            } else {
                let value = argv.get(i + 1).ok_or_else(|| {
                    format!("选项 --{} 缺少取值，例如 --{} <值>", raw_key, raw_key)
                })?;
                opts.insert(key, value.clone());
                i += 1;
            }
        } else if arg.starts_with('-') && arg.len() > 1 {
            let key = canonical(&arg[1..]);
            if BOOL_FLAGS.contains(&key.as_str()) {
                opts.insert(key, "true".into());
            } else {
                let value = argv
                    .get(i + 1)
                    .ok_or_else(|| format!("选项 {} 缺少取值", arg))?;
                opts.insert(key, value.clone());
                i += 1;
            }
        } else if command.is_empty() {
            command = arg.clone();
        } else {
            positional.push(arg.clone());
        }
        i += 1;
    }

    if opts.contains_key("help") && command.is_empty() {
        command = "help".into();
    }
    if opts.contains_key("version") && command.is_empty() {
        command = "version".into();
    }

    Ok(Parsed {
        command,
        opts,
        positional,
    })
}

fn print_help() {
    println!(
        r#"steam-curator {ver} — Steam 库整理器
本地扫描 → 生成 prompt 交给 AI → 校验结果 → 预览 → 写回 Steam 合集

用法: steam-curator <命令> [选项]

命令:
  scan        扫描本地 Steam 库，生成 library.json / library.csv
  prompt      基于扫描结果生成给 AI 的 prompt（{prompt}）
  plan        导入 AI 返回的 JSON，校验清洗后生成 {plan}
  preview     生成自包含的 HTML 预览报告（{report}）
  apply       把方案写回 Steam 合集（默认演练，加 --write 才落盘）
  restore     从备份回滚
  inspect     只读查看当前 Steam 库与合集概况
  all         等价于 scan 然后 prompt
  help        显示本帮助
  version     显示版本

通用选项:
  -o, --out <目录>          输出目录（默认 ./output）
  -s, --steam-dir <目录>    指定 Steam 根目录（默认自动定位）
  -u, --user <ID>           指定账号（steam3 或 steamid64，默认最近登录的）
  -h, --help                显示帮助
  -V, --version             显示版本

scan 选项:
      --no-csv              不生成 library.csv

prompt 选项:
      --lang <zh|en>        模板语言（默认 zh）
      --target <N>          建议 AI 生成的合集数量（默认 12）
      --max-name <N>        合集名长度上限（默认 14）
      --mode <auto|fresh|extend>
                            整理思路。auto（默认）在已有合集 >= 20 个时自动用
                            extend：只补全未归档的游戏，不推倒重来。
      --no-json             prompt 里不附紧凑 JSON

plan 选项:
  -i, --input <文件>        AI 返回内容（默认 <out>/ai-plan.json，也可直接用 -
                            从标准输入读取，便于粘贴）
      --keep-unknown        保留库里不存在的 appid（默认丢弃并报警）
      --allow-empty         允许生成空合集

preview 选项:
      --open                生成后用默认浏览器打开

apply 选项:
      --write               真正写入（不加则只演练）
      --force               忽略「Steam 正在运行」的保护
      --replace             以方案为准完全替换同名合集（默认合并保留原有成员）
      --prune               删除本工具此前创建、但当前方案里已没有的合集

restore 选项:
      --backup <名字>       指定备份目录名（默认最新一份）
      --latest              明确使用最新一份备份
      --confirm             真正执行回滚（不加则只演练，列出会还原哪些文件）

示例:
  steam-curator all
  steam-curator plan -i reply.txt
  steam-curator apply            # 演练
  steam-curator apply --write    # 落盘
  steam-curator restore --latest
"#,
        ver = VERSION,
        prompt = F_PROMPT,
        plan = F_PLAN,
        report = F_REPORT
    );
}

// ---------------------------------------------------------------------------
// 各命令
// ---------------------------------------------------------------------------

fn scan_options(p: &Parsed) -> scan::ScanOptions {
    scan::ScanOptions {
        steam_dir: p.get("steam-dir").map(|s| s.to_string()),
        user: p.get("user").map(|s| s.to_string()),
    }
}

fn cmd_scan(p: &Parsed) -> Result<(), String> {
    let out = p.out_dir();
    util::ensure_dir(&out)?;
    println!("→ 正在扫描 Steam 库…");
    let outcome = scan::run(&scan_options(p))?;
    let lib = &outcome.library;

    println!(
        "  Steam 根目录：{}（{}）",
        outcome.steam_root.display(),
        outcome.found_by
    );
    println!(
        "  账号：{}（steam3 {}）",
        lib.account.persona_name.clone().unwrap_or_else(|| "?".into()),
        lib.account.steam3_id
    );
    for r in &lib.library_roots {
        println!(
            "  库根 [{}] {} — {} 个游戏{}",
            r.index,
            r.path,
            r.game_count,
            if r.reachable { "" } else { "（不可达）" }
        );
    }
    let s = scan::summarize(lib);
    println!(
        "  共 {} 款游戏 · {} · 累计 {}",
        s.games,
        util::fmt_size(s.total_size),
        util::fmt_duration(s.total_playtime_minutes)
    );
    println!(
        "  从未启动 {} · 一年以上没玩 {} · 尚未归档 {}",
        s.never_launched, s.dormant, s.unassigned
    );

    let lib_json = serde_json::to_string_pretty(lib).map_err(|e| e.to_string())?;
    util::write_file(&out.join(F_LIBRARY), &lib_json)?;
    println!("  ✔ {}", out.join(F_LIBRARY).display());

    if !p.flag("no-csv") {
        util::write_file(&out.join(F_CSV), &scan::to_csv(lib))?;
        println!("  ✔ {}", out.join(F_CSV).display());
    }

    for w in &lib.warnings {
        println!("  ⚠ {}", w);
    }
    Ok(())
}

fn cmd_prompt(p: &Parsed) -> Result<(), String> {
    let out = p.out_dir();
    let lib = load_library(&out)?;
    let opts = prompt::PromptOptions {
        lang: p.get("lang").unwrap_or("zh").to_string(),
        target_collections: p
            .get("target")
            .and_then(|s| s.parse().ok())
            .unwrap_or(12),
        max_name_chars: p.get("max-name").and_then(|s| s.parse().ok()).unwrap_or(14),
        include_json: !p.flag("no-json"),
        mode: p.get("mode").unwrap_or("auto").to_string(),
    };
    let text = prompt::build(&lib, &opts);
    let path = out.join(F_PROMPT);
    util::write_file(&path, &text)?;
    println!("✔ 已生成 {}", path.display());
    println!("  长度 {} 字符，约 {} token。", text.chars().count(), text.len() / 3);
    println!();
    println!("下一步：把该文件内容整份粘给 AI，让它按文件里的 JSON 契约输出，");
    println!("然后把 AI 的回复（整段，含 ``` 围栏也没关系）存成");
    println!("  {}", out.join(F_AI_PLAN).display());
    println!("再运行：steam-curator plan");
    Ok(())
}

fn cmd_plan(p: &Parsed) -> Result<(), String> {
    let out = p.out_dir();
    let lib = load_library(&out)?;

    let input_path = match p.get("input") {
        Some("-") => None,
        Some(v) => Some(PathBuf::from(v)),
        None => Some(out.join(F_AI_PLAN)),
    };
    let (text, source) = match &input_path {
        None => {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| format!("读取标准输入失败: {}", e))?;
            (buf, "<stdin>".to_string())
        }
        Some(path) => {
            let text = std::fs::read_to_string(path).map_err(|e| {
                format!(
                    "读取 AI 结果 {} 失败: {}\n请把 AI 的回复保存到该文件，或用 -i <文件> / -i - 指定。",
                    path.display(),
                    e
                )
            })?;
            (text, path.to_string_lossy().into_owned())
        }
    };

    let opts = plan::PlanOptions {
        keep_unknown: p.flag("keep-unknown"),
        allow_empty: p.flag("allow-empty"),
    };
    let built = plan::build(&lib, &text, &source, &opts)?;

    println!("✔ 方案校验完成：{} 个合集", built.collections.len());
    println!(
        "  本方案覆盖 {}/{} 款游戏；另有 {} 款已由现有合集收录；既无方案也无现有归属的 {} 款",
        built.stats.assigned_games,
        built.stats.total_games,
        built.stats.already_filed_games,
        built.stats.unassigned_games
    );
    println!(
        "  新建 {} 个，更新 {} 个",
        built.stats.new_collections, built.stats.updated_collections
    );
    if !built.issues.is_empty() {
        println!("\n  校验报告：");
        for issue in &built.issues {
            let mark = match issue.level.as_str() {
                "error" => "✖",
                "warn" => "⚠",
                _ => "·",
            };
            println!("    {} {}", mark, issue.message);
        }
    }

    let path = out.join(F_PLAN);
    let text = serde_json::to_string_pretty(&built).map_err(|e| e.to_string())?;
    util::write_file(&path, &text)?;
    println!("\n  ✔ {}", path.display());

    // 顺手出一份预览
    write_report(&out, &lib, Some(&built), None)?;
    println!("  ✔ {}", out.join(F_REPORT).display());
    Ok(())
}

fn cmd_preview(p: &Parsed) -> Result<(), String> {
    let out = p.out_dir();
    let lib = load_library(&out)?;
    let plan = load_plan_optional(&out);
    let outcome = load_apply_optional(&out);
    let path = write_report(&out, &lib, plan.as_ref(), outcome.as_ref())?;
    println!("✔ 已生成预览报告 {}", path.display());
    if p.flag("open") {
        open_in_browser(&path);
    }
    Ok(())
}

fn cmd_apply(p: &Parsed) -> Result<(), String> {
    let out = p.out_dir();
    let lib = load_library(&out)?;
    let plan = load_plan_optional(&out).ok_or_else(|| {
        format!(
            "找不到 {}。请先运行 `steam-curator plan` 生成整理方案。",
            out.join(F_PLAN).display()
        )
    })?;

    let opts = apply::ApplyOptions {
        write: p.flag("write"),
        force: p.flag("force"),
        mode: if p.flag("replace") {
            "replace".into()
        } else {
            "merge".into()
        },
        prune: p.flag("prune"),
    };

    let steam_root = PathBuf::from(&lib.steam_root);
    let steam3 = lib.account.steam3_id.clone();

    println!(
        "→ {} Steam 合集…",
        if opts.write { "写入" } else { "演练（不会改动任何文件）" }
    );
    let outcome = apply::run(&plan, &steam_root, &steam3, &out, &opts)?;

    if let Some(reason) = &outcome.blocked_reason {
        println!("\n✖ 已阻止写入：{}", reason);
    }

    let mut created = 0;
    let mut updated = 0;
    let mut unchanged = 0;
    let mut deleted = 0;
    let mut skipped = 0;
    for a in &outcome.actions {
        match a.kind.as_str() {
            "create" => created += 1,
            "update" => updated += 1,
            "unchanged" => unchanged += 1,
            "delete" => deleted += 1,
            _ => skipped += 1,
        }
    }
    println!(
        "\n  目标文件：{}",
        outcome.namespace_path
    );
    println!(
        "  命名空间版本：{} → {}",
        outcome.old_namespace_version, outcome.new_namespace_version
    );
    println!(
        "  新建 {} · 更新 {} · 无变化 {} · 删除 {} · 跳过 {}",
        created, updated, unchanged, deleted, skipped
    );
    println!();
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
        println!("  {}", detail);
        if let Some(note) = &a.note {
            if a.kind == "skip" {
                println!("        {}", note);
            }
        }
    }

    if let Some(dir) = &outcome.backup_dir {
        println!("\n  ✔ 备份已保存到 {}", dir);
    }

    let report = write_report(&out, &lib, Some(&plan), Some(&outcome))?;
    println!("  ✔ 预览报告 {}", report.display());

    let json = serde_json::to_string_pretty(&outcome).map_err(|e| e.to_string())?;
    util::write_file(&out.join(F_APPLY), &json)?;

    if !opts.write {
        println!();
        println!("这是演练结果，Steam 里没有任何改动。");
        println!("确认无误后执行：steam-curator apply --write");
        println!("（执行前请先完全退出 Steam，否则退出时会被覆盖）");
    } else if outcome.blocked_reason.is_none() {
        println!();
        println!("已写入。请启动 Steam 打开「库」界面查看效果。");
        println!("如需回滚：steam-curator restore --latest");
    }
    Ok(())
}

fn cmd_restore(p: &Parsed) -> Result<(), String> {
    let out = p.out_dir();
    let dry = !p.flag("confirm");
    let restored = apply::restore(
        &out,
        p.get("backup"),
        p.flag("latest") || p.get("backup").is_none(),
        dry,
    )?;
    println!(
        "{} 共 {} 个文件：",
        if dry { "将还原" } else { "已还原" },
        restored.len()
    );
    for line in &restored {
        println!("  {}", line);
    }
    if dry {
        println!("\n（演练模式）加 --confirm 真正执行回滚。");
    }
    Ok(())
}

fn cmd_inspect(p: &Parsed) -> Result<(), String> {
    let outcome = scan::run(&scan_options(p))?;
    let lib = &outcome.library;
    let s = scan::summarize(lib);
    println!("Steam 根目录: {}", outcome.steam_root.display());
    println!(
        "账号: {} (steam3 {})",
        lib.account.persona_name.clone().unwrap_or_else(|| "?".into()),
        lib.account.steam3_id
    );
    println!();
    println!("游戏 {} 款 · {} · 累计 {}", s.games, util::fmt_size(s.total_size), util::fmt_duration(s.total_playtime_minutes));
    println!("从未启动 {} · 浅尝 {} · 沉寂 {} · 已隐藏 {} · 收藏 {}", s.never_launched, s.barely_played, s.dormant, s.hidden, s.favorite);
    println!();
    println!("现有合集（{} 个）：", lib.existing_collections.len());
    for c in &lib.existing_collections {
        let kind = if c.is_builtin {
            "系统"
        } else if c.is_dynamic {
            "动态"
        } else {
            "静态"
        };
        println!("  [{}] {} — {} 款", kind, c.name, c.appids.len());
    }
    match steam::read_cloud(&outcome.steam_root, &outcome.steam3) {
        Ok(cloud) => {
            println!();
            println!("合集文件: {}", cloud.namespace_path.display());
            println!(
                "命名空间版本 {} · 文件内最大条目版本 {}",
                cloud.namespace_version, cloud.max_entry_version
            );
        }
        Err(e) => {
            println!();
            println!("⚠ {}", e);
        }
    }
    println!("Steam 进程: {}", if steam::steam_running() { "运行中（写入会被拒绝）" } else { "未运行（可安全写入）" });
    Ok(())
}

// ---------------------------------------------------------------------------
// 辅助
// ---------------------------------------------------------------------------

fn load_library(out: &Path) -> Result<Library, String> {
    let path = out.join(F_LIBRARY);
    let text = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "读取 {} 失败: {}\n请先运行 `steam-curator scan`。",
            path.display(),
            e
        )
    })?;
    serde_json::from_str(&text).map_err(|e| format!("{} 解析失败: {}", path.display(), e))
}

fn load_plan_optional(out: &Path) -> Option<Plan> {
    let path = out.join(F_PLAN);
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
}

fn load_apply_optional(out: &Path) -> Option<apply::ApplyOutcome> {
    let path = out.join(F_APPLY);
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
}

fn write_report(
    out: &Path,
    lib: &Library,
    plan: Option<&Plan>,
    outcome: Option<&apply::ApplyOutcome>,
) -> Result<PathBuf, String> {
    let title = match plan {
        Some(_) => "Steam 库整理预览".to_string(),
        None => "Steam 库现状".to_string(),
    };
    let html = preview::build(preview::PreviewInput {
        library: lib,
        plan,
        outcome,
        title,
    });
    let path = out.join(F_REPORT);
    util::write_file(&path, &html)?;
    Ok(path)
}

fn open_in_browser(path: &Path) {
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
