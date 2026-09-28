//! steam-curator 图形界面版 —— 原生 Win32 窗口（不需要浏览器，也不需要控制台）。
//!
//! 用的是 `native-windows-gui`：它只是把 Win32 控件包了一层，依赖极少
//! （winapi + lazy_static + bitflags），编译产物小、启动快、不依赖 OpenGL。
//!
//! 界面逻辑刻意做得极简：左边一列按钮 = 流水线的每一步，右边上面是运行日志、
//! 下面是粘贴 AI 回复的输入框。所有真正的活儿都调用 `steam_curator::session`，
//! 与命令行版共享同一套实现。

#![windows_subsystem = "windows"]

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use native_windows_gui as nwg;

use steam_curator::session::{self, ApplyArgs, PromptArgs, ScanArgs};

// 窗口与布局常量（客户区约 1164 × 781）
const WIN_W: i32 = 1180;
const WIN_H: i32 = 820;
const LEFT_X: i32 = 15;
const LEFT_W: i32 = 200;
const BTN_H: i32 = 38;
const RIGHT_X: i32 = 230;
const RIGHT_W: i32 = 915;
/// 客户区宽度减去左右留白，用于状态栏
const STATUS_W: i32 = RIGHT_X + RIGHT_W - LEFT_X;

struct Ui {
    window: nwg::Window,
    #[allow(dead_code)]
    font: nwg::Font,
    status: nwg::Label,
    #[allow(dead_code)]
    log_label: nwg::Label,
    log: nwg::TextBox,
    #[allow(dead_code)]
    reply_label: nwg::Label,
    reply: nwg::TextBox,
    #[allow(dead_code)]
    hint: nwg::Label,
    b_scan: nwg::Button,
    b_prompt: nwg::Button,
    b_copy: nwg::Button,
    b_plan: nwg::Button,
    b_preview: nwg::Button,
    b_opendir: nwg::Button,
    b_dry: nwg::Button,
    b_write: nwg::Button,
    b_restore: nwg::Button,
    b_inspect: nwg::Button,
    b_clear: nwg::Button,
}

impl Ui {
    fn set_log(&self, text: &str) {
        self.log.set_text(text);
    }
}

struct State {
    out_dir: PathBuf,
    /// 「正式写回」的两步确认：第一次点击只是把按钮切成确认态
    confirm_write: bool,
    busy: bool,
}

impl State {
    fn refresh_status(&self, ui: &Ui) {
        let steam = if steam_curator::steam::steam_running() {
            "Steam 正在运行（写入会被拒绝）"
        } else {
            "Steam 未运行（可安全写入）"
        };
        let plan = if self.out_dir.join(session::F_PLAN).is_file() {
            "已生成方案"
        } else {
            "尚无方案"
        };
        ui.status.set_text(&format!(
            "输出目录：{}　|　{}　|　{}",
            self.out_dir.display(),
            plan,
            steam
        ));
    }
}

// ---------------------------------------------------------------------------
// 输出目录定位
// ---------------------------------------------------------------------------

/// 双击 exe 时工作目录就是 exe 所在目录，直接拿它当输出目录会得到一个空的
/// `target\release\output`，与命令行版的 `./output` 对不上。
/// 所以从 exe 位置向上找 `Cargo.toml`，命中就用 `<项目根>/output`。
fn resolve_out_dir() -> PathBuf {
    let argv: Vec<String> = std::env::args().collect();
    for flag in ["--out", "-o"] {
        if let Some(i) = argv.iter().position(|a| a == flag) {
            if let Some(v) = argv.get(i + 1) {
                return PathBuf::from(v);
            }
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.parent().map(|p| p.to_path_buf());
        while let Some(d) = dir {
            if d.join("Cargo.toml").is_file() {
                return d.join("output");
            }
            dir = d.parent().map(|p| p.to_path_buf());
        }
        if let Some(d) = exe.parent() {
            return d.join("output");
        }
    }
    PathBuf::from("output")
}

// ---------------------------------------------------------------------------
// 界面搭建
// ---------------------------------------------------------------------------

fn build_ui() -> Result<Ui, nwg::NwgError> {
    let mut window = nwg::Window::default();
    let mut font = nwg::Font::default();
    let mut status = nwg::Label::default();
    let mut log_label = nwg::Label::default();
    let mut log = nwg::TextBox::default();
    let mut reply_label = nwg::Label::default();
    let mut reply = nwg::TextBox::default();
    let mut hint = nwg::Label::default();
    let mut b_scan = nwg::Button::default();
    let mut b_prompt = nwg::Button::default();
    let mut b_copy = nwg::Button::default();
    let mut b_plan = nwg::Button::default();
    let mut b_preview = nwg::Button::default();
    let mut b_opendir = nwg::Button::default();
    let mut b_dry = nwg::Button::default();
    let mut b_write = nwg::Button::default();
    let mut b_restore = nwg::Button::default();
    let mut b_inspect = nwg::Button::default();
    let mut b_clear = nwg::Button::default();

    // 中文字体；默认的旧版系统字体对中文很不友好
    nwg::Font::builder()
        .family("Microsoft YaHei UI")
        .size(16)
        .build(&mut font)?;

    // WINDOW = 标题栏 + 系统菜单（不可缩放）；再加一个最小化按钮。
    // 布局是固定像素的，所以刻意不给 WS_THICKFRAME，免得拉伸后错位。
    nwg::Window::builder()
        .flags(nwg::WindowFlags::WINDOW | nwg::WindowFlags::MINIMIZE_BOX | nwg::WindowFlags::VISIBLE)
        .size((WIN_W, WIN_H))
        .position((260, 120))
        .title("steam-curator — Steam 库整理器")
        .build(&mut window)?;

    nwg::Label::builder()
        .text("准备就绪")
        .parent(&window)
        .position((LEFT_X, 10))
        .size((STATUS_W, 22))
        .font(Some(&font))
        .build(&mut status)?;

    // ---- 左侧按钮列 ----
    let gap = BTN_H + 6;
    let mut y = 42i32;
    let mut next_y = move || {
        let cur = y;
        y += gap;
        cur
    };

    nwg::Button::builder()
        .text("① 扫描 Steam 库")
        .parent(&window)
        .position((LEFT_X, next_y()))
        .size((LEFT_W, BTN_H))
        .font(Some(&font))
        .build(&mut b_scan)?;
    nwg::Button::builder()
        .text("② 生成 AI Prompt")
        .parent(&window)
        .position((LEFT_X, next_y()))
        .size((LEFT_W, BTN_H))
        .font(Some(&font))
        .build(&mut b_prompt)?;
    nwg::Button::builder()
        .text("　 复制 Prompt 到剪贴板")
        .parent(&window)
        .position((LEFT_X, next_y()))
        .size((LEFT_W, BTN_H))
        .font(Some(&font))
        .build(&mut b_copy)?;
    nwg::Button::builder()
        .text("③ 校验 AI 回复")
        .parent(&window)
        .position((LEFT_X, next_y()))
        .size((LEFT_W, BTN_H))
        .font(Some(&font))
        .build(&mut b_plan)?;
    nwg::Button::builder()
        .text("④ 打开预览报告")
        .parent(&window)
        .position((LEFT_X, next_y()))
        .size((LEFT_W, BTN_H))
        .font(Some(&font))
        .build(&mut b_preview)?;
    nwg::Button::builder()
        .text("　 打开输出目录")
        .parent(&window)
        .position((LEFT_X, next_y()))
        .size((LEFT_W, BTN_H))
        .font(Some(&font))
        .build(&mut b_opendir)?;
    nwg::Button::builder()
        .text("⑤ 演练写回")
        .parent(&window)
        .position((LEFT_X, next_y()))
        .size((LEFT_W, BTN_H))
        .font(Some(&font))
        .build(&mut b_dry)?;
    nwg::Button::builder()
        .text("⑥ 正式写回")
        .parent(&window)
        .position((LEFT_X, next_y()))
        .size((LEFT_W, BTN_H))
        .font(Some(&font))
        .build(&mut b_write)?;
    nwg::Button::builder()
        .text("⑦ 回滚最近备份")
        .parent(&window)
        .position((LEFT_X, next_y()))
        .size((LEFT_W, BTN_H))
        .font(Some(&font))
        .build(&mut b_restore)?;
    nwg::Button::builder()
        .text("⑧ 查看库现状（只读）")
        .parent(&window)
        .position((LEFT_X, next_y()))
        .size((LEFT_W, BTN_H))
        .font(Some(&font))
        .build(&mut b_inspect)?;
    nwg::Button::builder()
        .text("　 清空日志")
        .parent(&window)
        .position((LEFT_X, next_y()))
        .size((LEFT_W, BTN_H))
        .font(Some(&font))
        .build(&mut b_clear)?;

    // ---- 右侧：运行日志 ----
    nwg::Label::builder()
        .text("运行日志 / 结果")
        .parent(&window)
        .position((RIGHT_X, 42))
        .size((RIGHT_W, 20))
        .font(Some(&font))
        .build(&mut log_label)?;
    nwg::TextBox::builder()
        .text("")
        .parent(&window)
        .position((RIGHT_X, 64))
        .size((RIGHT_W, 398))
        .readonly(true)
        .font(Some(&font))
        .build(&mut log)?;

    // ---- 右侧：AI 回复输入 ----
    nwg::Label::builder()
        .text("把 AI 的回复整段粘到这里（含 ``` 围栏、前后寒暄都没关系），然后点「③ 校验 AI 回复」")
        .parent(&window)
        .position((RIGHT_X, 470))
        .size((RIGHT_W, 20))
        .font(Some(&font))
        .build(&mut reply_label)?;
    nwg::TextBox::builder()
        .text("")
        .parent(&window)
        .position((RIGHT_X, 492))
        .size((RIGHT_W, 214))
        .font(Some(&font))
        .build(&mut reply)?;

    nwg::Label::builder()
        .text("安全说明：⑤ 演练不会改动任何文件。\n⑥ 正式写回前必须完全退出 Steam（含托盘图标），写前会自动整份备份，随时可用 ⑦ 回滚。\n整个程序不联网，你的库数据只留在本地 output 目录。")
        .parent(&window)
        .position((RIGHT_X, 712))
        .size((RIGHT_W, 62))
        .font(Some(&font))
        .build(&mut hint)?;

    Ok(Ui {
        window,
        font,
        status,
        log_label,
        log,
        reply_label,
        reply,
        hint,
        b_scan,
        b_prompt,
        b_copy,
        b_plan,
        b_preview,
        b_opendir,
        b_dry,
        b_write,
        b_restore,
        b_inspect,
        b_clear,
    })
}

// ---------------------------------------------------------------------------
// 启动
// ---------------------------------------------------------------------------

fn main() {
    if let Err(e) = run() {
        // 图形程序没有控制台，出错就落在 exe 旁边，免得用户看到「双击没反应」
        let msg = format!("steam-curator GUI 启动失败：\n{}\n", e);
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                let _ = std::fs::write(dir.join("steam-curator-gui-error.txt"), &msg);
            }
        }
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    nwg::init()?;

    let ui = Rc::new(build_ui()?);
    let out_dir = resolve_out_dir();
    let state = Rc::new(RefCell::new(State {
        out_dir,
        confirm_write: false,
        busy: false,
    }));

    {
        let st = state.borrow();
        st.refresh_status(&ui);
        ui.set_log(&boot_message(&st.out_dir));
    }

    let handler = nwg::full_bind_event_handler(&ui.window.handle, {
        let ui = ui.clone();
        let state = state.clone();
        move |evt, _evt_data, handle| match evt {
            nwg::Event::OnWindowClose => nwg::stop_thread_dispatch(),
            nwg::Event::OnButtonClick => handle_click(&ui, &state, handle),
            _ => {}
        }
    });

    nwg::dispatch_thread_events();
    nwg::unbind_event_handler(&handler);
    Ok(())
}

fn boot_message(out_dir: &std::path::Path) -> String {
    let has_lib = out_dir.join(session::F_LIBRARY).is_file();
    let has_plan = out_dir.join(session::F_PLAN).is_file();
    let has_backup = out_dir.join("backups").is_dir();
    let mut s = String::new();
    s.push_str("steam-curator 图形界面\r\n");
    s.push_str("======================\r\n\r\n");
    s.push_str(&format!("输出目录：{}\r\n", out_dir.display()));
    s.push_str(&format!(
        "当前状态：{}　{}\r\n\r\n",
        if has_lib { "已有扫描数据" } else { "尚未扫描" },
        if has_plan {
            "已有整理方案"
        } else {
            "尚无整理方案"
        }
    ));
    s.push_str("使用顺序：\r\n");
    s.push_str("  ① 扫描 Steam 库\r\n");
    s.push_str("  ② 生成 AI Prompt  →  复制 Prompt 到剪贴板  →  贴给任意 AI\r\n");
    s.push_str("  ③ 把 AI 的回复粘到下面的输入框，点「校验 AI 回复」\r\n");
    s.push_str("  ④ 打开预览报告，确认效果\r\n");
    s.push_str("  ⑤ 演练写回（不改文件）  →  ⑥ 正式写回\r\n\r\n");
    if has_backup {
        s.push_str("检测到已有备份目录，⑦ 可随时回滚。\r\n");
    }
    s
}

// ---------------------------------------------------------------------------
// 按钮分发
// ---------------------------------------------------------------------------

fn handle_click(ui: &Rc<Ui>, state: &Rc<RefCell<State>>, handle: nwg::ControlHandle) {
    let mut st = state.borrow_mut();
    if st.busy {
        return;
    }

    // 除了「正式写回」的第二次点击，其余任何点击都要复位确认态
    let is_write_btn = handle == ui.b_write.handle;
    if !is_write_btn && st.confirm_write {
        st.confirm_write = false;
        ui.b_write.set_text("⑥ 正式写回");
    }

    let out_dir = st.out_dir.clone();

    if handle == ui.b_clear.handle {
        ui.set_log("");
        return;
    }
    if handle == ui.b_opendir.handle {
        session::open_directory(&out_dir);
        return;
    }
    if handle == ui.b_preview.handle {
        st.busy = true;
        let r = session::do_preview(&out_dir).map(|o| {
            session::open_in_browser(&out_dir.join(session::F_REPORT));
            o
        });
        st.busy = false;
        report(ui, &mut st, r);
        return;
    }
    if handle == ui.b_copy.handle {
        match session::read_prompt(&out_dir) {
            Some(text) => {
                let n = text.chars().count();
                nwg::Clipboard::set_data_text(&ui.window, &text);
                ui.set_log(&format!(
                    "✔ 已把 AI Prompt（{} 字符）复制到剪贴板。\r\n\r\n现在粘贴给任意对话式 AI 即可。",
                    n
                ));
            }
            None => ui.set_log(&format!(
                "⚠ 还没生成 prompt。请先点「② 生成 AI Prompt」。\r\n（找不到 {}）",
                out_dir.join(session::F_PROMPT).display()
            )),
        }
        return;
    }

    // 正式写回：两步确认
    if is_write_btn {
        if !st.confirm_write {
            st.confirm_write = true;
            ui.b_write.set_text("⚠ 再点一次确认写回");
            ui.set_log(
                "【确认】正式写回会修改你 Steam 里的合集。\r\n\r\n\
                 写之前请确认：\r\n\
                 1. Steam 已经**完全退出**（包括右下角托盘图标）；\r\n\
                 2. 你已经用「⑤ 演练写回」看过结果，确认无误。\r\n\r\n\
                 写之前程序会自动把合集文件、命名空间版本文件、localconfig.vdf\r\n\
                 整份备份到 output\\backups\\ 下，随时可用「⑦ 回滚最近备份」还原。\r\n\r\n\
                 确认无误就再点一次「⚠ 再点一次确认写回」。",
            );
            return;
        }
        st.confirm_write = false;
        ui.b_write.set_text("⑥ 正式写回");
        st.busy = true;
        let r = session::do_apply(
            &out_dir,
            &ApplyArgs {
                write: true,
                force: false,
                mode: "merge",
                prune: false,
            },
        );
        st.busy = false;
        report(ui, &mut st, r);
        return;
    }

    st.busy = true;
    let result = if handle == ui.b_scan.handle {
        session::do_scan(
            &out_dir,
            &ScanArgs {
                steam_dir: None,
                user: None,
                write_csv: true,
            },
        )
    } else if handle == ui.b_prompt.handle {
        session::do_prompt(
            &out_dir,
            &PromptArgs {
                lang: "zh",
                target: 12,
                max_name: 14,
                include_json: true,
                mode: "auto",
            },
        )
    } else if handle == ui.b_plan.handle {
        let reply = ui.reply.text();
        if reply.trim().is_empty() {
            Err(format!(
                "输入框是空的。\r\n\r\n请把 AI 的回复整段粘到下面那个框里再点「③ 校验 AI 回复」。\r\n\
                 （也可以手动把回复存成 {} 后重试）",
                out_dir.join(session::F_AI_PLAN).display()
            ))
        } else {
            session::save_ai_reply(&out_dir, &reply).and_then(|path| {
                session::do_plan(&out_dir, &reply, &path.to_string_lossy(), false, false)
            })
        }
    } else if handle == ui.b_dry.handle {
        session::do_apply(
            &out_dir,
            &ApplyArgs {
                write: false,
                force: false,
                mode: "merge",
                prune: false,
            },
        )
    } else if handle == ui.b_restore.handle {
        session::do_restore(&out_dir, None, true, true)
    } else if handle == ui.b_inspect.handle {
        session::do_inspect(None, None)
    } else {
        st.busy = false;
        return;
    };
    st.busy = false;
    report(ui, &mut st, result);
}

fn report(ui: &Rc<Ui>, st: &mut State, result: Result<session::Outcome, String>) {
    match result {
        Ok(outcome) => ui.set_log(&outcome.text()),
        Err(e) => ui.set_log(&format!("✖ 出错了\r\n\r\n{}", e)),
    }
    st.refresh_status(ui);
}
