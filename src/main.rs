//! steam-curator 命令行版。
//!
//! 真正的活儿都在 `steam_curator::session` 里，这里只负责：解析参数 → 调用 → 打印日志。
//! 图形界面版（`steam-curator-gui`）调用的是同一批函数，行为完全一致。

use std::collections::HashMap;
use std::path::PathBuf;

use steam_curator::session::{self, ApplyArgs, PromptArgs, ScanArgs};
use steam_curator::VERSION;

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
    match parsed.command.as_str() {
        "" | "help" => {
            print_help();
            Ok(())
        }
        "version" => {
            println!("steam-curator {}", VERSION);
            Ok(())
        }
        "scan" => {
            emit(step_scan(&parsed)?);
            Ok(())
        }
        "prompt" => {
            emit(step_prompt(&parsed)?);
            Ok(())
        }
        "plan" => {
            emit(step_plan(&parsed)?);
            Ok(())
        }
        "preview" => {
            let outcome = step_preview(&parsed)?;
            let path = parsed.out_dir().join(session::F_REPORT);
            emit(outcome);
            if parsed.flag("open") {
                session::open_in_browser(&path);
            }
            Ok(())
        }
        "apply" => {
            emit(step_apply(&parsed, parsed.flag("write"))?);
            Ok(())
        }
        "restore" => {
            emit(session::do_restore(
                &parsed.out_dir(),
                parsed.get("backup"),
                parsed.flag("latest") || parsed.get("backup").is_none(),
                parsed.flag("confirm"),
            )?);
            Ok(())
        }
        "inspect" => {
            emit(session::do_inspect(
                parsed.get("steam-dir"),
                parsed.get("user"),
            )?);
            Ok(())
        }
        "all" => {
            emit(step_scan(&parsed)?);
            println!();
            emit(step_prompt(&parsed)?);
            Ok(())
        }
        other => Err(format!(
            "未知命令 `{}`。运行 `steam-curator help` 查看用法。",
            other
        )),
    }
}

fn emit(outcome: session::Outcome) {
    println!("{}", outcome.text());
}

// ---------------------------------------------------------------------------
// 参数解析
// ---------------------------------------------------------------------------

struct Parsed {
    command: String,
    opts: HashMap<String, String>,
    /// 位置参数目前用不到，保留以便将来扩展（例如 `steam-curator apply 名称`）
    #[allow(dead_code)]
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
    fn scan_args(&self) -> ScanArgs<'_> {
        ScanArgs {
            steam_dir: self.get("steam-dir"),
            user: self.get("user"),
            write_csv: !self.flag("no-csv"),
        }
    }
}

const BOOL_FLAGS: &[&str] = &[
    "help", "version", "write", "force", "prune", "open", "no-csv", "no-json", "latest",
    "keep-unknown", "allow-empty", "replace", "confirm",
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
                let value = argv
                    .get(i + 1)
                    .ok_or_else(|| format!("选项 --{} 缺少取值，例如 --{} <值>", raw_key, raw_key))?;
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

// ---------------------------------------------------------------------------
// 各步骤的参数装配
// ---------------------------------------------------------------------------

fn step_scan(p: &Parsed) -> Result<session::Outcome, String> {
    session::do_scan(&p.out_dir(), &p.scan_args())
}

fn step_prompt(p: &Parsed) -> Result<session::Outcome, String> {
    let lang = p.get("lang").unwrap_or("zh").to_string();
    let mode = p.get("mode").unwrap_or("auto").to_string();
    session::do_prompt(
        &p.out_dir(),
        &PromptArgs {
            lang: &lang,
            target: p.get("target").and_then(|s| s.parse().ok()).unwrap_or(12),
            max_name: p.get("max-name").and_then(|s| s.parse().ok()).unwrap_or(14),
            include_json: !p.flag("no-json"),
            mode: &mode,
        },
    )
}

fn step_plan(p: &Parsed) -> Result<session::Outcome, String> {
    let out = p.out_dir();
    let (text, source) = match p.get("input") {
        Some("-") => {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| format!("读取标准输入失败: {}", e))?;
            (buf, "<stdin>".to_string())
        }
        Some(v) => {
            let path = PathBuf::from(v);
            let text = std::fs::read_to_string(&path).map_err(|e| {
                format!(
                    "读取 AI 结果 {} 失败: {}\n请把 AI 的回复保存到该文件，或用 -i <文件> / -i - 指定。",
                    path.display(),
                    e
                )
            })?;
            (text, path.to_string_lossy().into_owned())
        }
        None => {
            let path = out.join(session::F_AI_PLAN);
            let text = std::fs::read_to_string(&path).map_err(|e| {
                format!(
                    "读取 AI 结果 {} 失败: {}\n请把 AI 的回复保存到该文件，或用 -i <文件> / -i - 指定。",
                    path.display(),
                    e
                )
            })?;
            (text, path.to_string_lossy().into_owned())
        }
    };
    session::do_plan(
        &out,
        &text,
        &source,
        p.flag("keep-unknown"),
        p.flag("allow-empty"),
    )
}

fn step_preview(p: &Parsed) -> Result<session::Outcome, String> {
    session::do_preview(&p.out_dir())
}

fn step_apply(p: &Parsed, write: bool) -> Result<session::Outcome, String> {
    let mode = if p.flag("replace") { "replace" } else { "merge" };
    session::do_apply(
        &p.out_dir(),
        &ApplyArgs {
            write,
            force: p.flag("force"),
            mode,
            prune: p.flag("prune"),
        },
    )
}

// ---------------------------------------------------------------------------
// 帮助
// ---------------------------------------------------------------------------

fn print_help() {
    println!(
        r#"steam-curator {ver} — Steam 库整理器（命令行版）
本地扫描 → 生成 prompt 交给 AI → 校验结果 → 预览 → 写回 Steam 合集

想要图形界面就直接运行 steam-curator-gui。

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
  steam-curator restore --latest --confirm
"#,
        ver = VERSION,
        prompt = session::F_PROMPT,
        plan = session::F_PLAN,
        report = session::F_REPORT
    );
}
