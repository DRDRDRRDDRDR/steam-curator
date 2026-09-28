//! steam-curator 图形界面版 —— 原生 Win32 窗口（不需要浏览器，也不需要控制台）。
//!
//! # 为什么静态层是自绘的
//!
//! 第一版用一堆 `nwg::Label` 拼标题带，结果在真机上翻车：Win32 子窗口的 z-order
//! 与 `WM_CTLCOLORSTATIC` 的返回刷语义很难精确控制，实测出现「右侧状态文字一个像素
//! 都没画出来」和「`WM_ERASEBKGND` 不生效、底色仍是系统灰」两个问题。
//!
//! 所以这一版把**所有静态内容（标题带、分组小标题、面板标题、提示文字）改为在
//! 父窗口的 `WM_PAINT` 里用 GDI 直接绘制**：
//!
//! * `BeginPaint` → `FillRect` 铺底色与标题带 → `DrawTextW` 逐条写字 → `EndPaint`；
//! * 只有**按钮**和**输入框**是真控件，其余全部是画出来的 —— 于是没有 z-order、
//!   没有透明背景、没有子控件遮挡的问题，颜色、内边距、字体完全可控；
//! * 窗口设 `WS_CLIPCHILDREN`，父窗口作画时自动避开子控件区域，既正确又少闪烁；
//! * 高 DPI 由 `nwg::scale_factor()` 统一缩放坐标与字号，避免被系统位图拉伸而发虚。
//!
//! 界面逻辑保持简单：左边一列按钮 = 流水线的每一步，右边上面是运行日志、
//! 下面是粘贴 AI 回复的输入框。所有真正的活儿都调用 `steam_curator::session`，
//! 与命令行版共享同一套实现。

#![windows_subsystem = "windows"]

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use native_windows_gui as nwg;

use steam_curator::session::{self, ApplyArgs, PromptArgs, ScanArgs};

// ---------------------------------------------------------------------------
// 配色
// ---------------------------------------------------------------------------

const C_BAND: [u8; 3] = [22, 33, 48]; // 深色标题带
const C_BODY: [u8; 3] = [245, 246, 248]; // 客户区底色
const C_TEXT: [u8; 3] = [24, 32, 44]; // 主文字
const C_MUTED: [u8; 3] = [92, 103, 120]; // 次要文字（够深，保证可读）
const C_ACCENT: [u8; 3] = [46, 126, 214]; // 强调色
const C_BAND_TEXT: [u8; 3] = [236, 243, 251];
const C_BAND_SUB: [u8; 3] = [154, 176, 200];
const C_PANEL: [u8; 3] = [236, 243, 253]; // 「粘贴区」底色
const C_PANEL_EDGE: [u8; 3] = [46, 126, 214]; // 「粘贴区」边框

// ---------------------------------------------------------------------------
// 布局（设计尺寸，运行时按 DPI 缩放）
// ---------------------------------------------------------------------------

const WIN_W: i32 = 1280;
const WIN_H: i32 = 950;
const BAND_H: i32 = 84;
const LEFT_X: i32 = 24;
const LEFT_W: i32 = 268;
const RIGHT_X: i32 = 308;
const RIGHT_W: i32 = 948;
/// 按钮高度与纵向间距 —— 之前 40/6 太挤，是「难看」的主因
const BTN_H: i32 = 44;
const BTN_GAP: i32 = 8;

// ---------------------------------------------------------------------------
// 极小的 GDI / user32 绑定
// ---------------------------------------------------------------------------

mod gdi {
    use std::ffi::c_void;

    pub type Hwnd = *mut c_void;
    pub type Hdc = *mut c_void;
    pub type Hbrush = *mut c_void;
    pub type Hfont = *mut c_void;
    pub type Hgdiobj = *mut c_void;

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    pub struct Rect {
        pub left: i32,
        pub top: i32,
        pub right: i32,
        pub bottom: i32,
    }

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    pub struct PaintStruct {
        pub hdc: Hdc,
        pub f_erase: i32,
        pub rc_paint: Rect,
        pub f_restore: i32,
        pub f_inc_update: i32,
        pub reserved: [u8; 32],
    }

    #[link(name = "gdi32")]
    extern "system" {
        pub fn CreateSolidBrush(color: u32) -> Hbrush;
        pub fn CreateFontW(
            height: i32,
            width: i32,
            escapement: i32,
            orientation: i32,
            weight: i32,
            italic: u32,
            underline: u32,
            strike_out: u32,
            char_set: u32,
            out_precision: u32,
            clip_precision: u32,
            quality: u32,
            pitch_and_family: u32,
            face: *const u16,
        ) -> Hfont;
        pub fn SelectObject(hdc: Hdc, obj: Hgdiobj) -> Hgdiobj;
        pub fn SetTextColor(hdc: Hdc, color: u32) -> u32;
        pub fn SetBkMode(hdc: Hdc, mode: i32) -> i32;
    }

    #[link(name = "user32")]
    extern "system" {
        pub fn GetClientRect(hwnd: Hwnd, rect: *mut Rect) -> i32;
        pub fn FillRect(hdc: Hdc, rect: *const Rect, brush: Hbrush) -> i32;
        pub fn DrawTextW(hdc: Hdc, text: *const u16, count: i32, rect: *mut Rect, format: u32)
            -> i32;
        pub fn BeginPaint(hwnd: Hwnd, ps: *mut PaintStruct) -> Hdc;
        pub fn EndPaint(hwnd: Hwnd, ps: *const PaintStruct) -> i32;
        pub fn InvalidateRect(hwnd: Hwnd, rect: *const Rect, erase: i32) -> i32;
        pub fn SetWindowLongPtrW(hwnd: Hwnd, index: i32, value: isize) -> isize;
        pub fn SetWindowPos(
            hwnd: Hwnd,
            after: Hwnd,
            x: i32,
            y: i32,
            cx: i32,
            cy: i32,
            flags: u32,
        ) -> i32;
    }

    pub const TRANSPARENT: i32 = 1;

    // DrawText 格式
    pub const DT_LEFT: u32 = 0x0000;
    pub const DT_VCENTER: u32 = 0x0004;
    pub const DT_WORDBREAK: u32 = 0x0010;
    pub const DT_SINGLELINE: u32 = 0x0020;
    pub const DT_NOPREFIX: u32 = 0x0800;
    pub const DT_END_ELLIPSIS: u32 = 0x8000;

    // 字体参数
    pub const FW_NORMAL: i32 = 400;
    pub const FW_SEMIBOLD: i32 = 600;
    pub const FW_BOLD: i32 = 700;
    pub const DEFAULT_CHARSET: u32 = 1;
    pub const OUT_TT_PRECIS: u32 = 4;
    pub const CLEARTYPE_QUALITY: u32 = 5;

    // SetWindowLongPtrW / SetWindowPos
    pub const GWL_STYLE: i32 = -16;
    pub const WS_CLIPCHILDREN: isize = 0x0200_0000;
    pub const SWP_NOSIZE: u32 = 0x0001;
    pub const SWP_NOMOVE: u32 = 0x0002;
    pub const SWP_NOZORDER: u32 = 0x0004;
    pub const SWP_NOACTIVATE: u32 = 0x0010;
    pub const SWP_FRAMECHANGED: u32 = 0x0020;

    /// COLORREF 是 0x00BBGGRR —— 注意 BGR 顺序
    pub fn rgb(c: [u8; 3]) -> u32 {
        (c[0] as u32) | ((c[1] as u32) << 8) | ((c[2] as u32) << 16)
    }

    pub fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }
}

// ---------------------------------------------------------------------------
// 自绘层
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum FontId {
    BandTitle = 0,
    BandSmall = 1,
    Cap = 2,
    H2 = 3,
    Small = 4,
}

struct DrawItem {
    /// 供运行时改文本（状态栏）
    key: &'static str,
    text: String,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    color: [u8; 3],
    font: FontId,
    dt: u32,
}

struct Panel {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

struct Painter {
    items: Vec<DrawItem>,
    panels: Vec<Panel>,
    band_brush: gdi::Hbrush,
    body_brush: gdi::Hbrush,
    panel_brush: gdi::Hbrush,
    panel_edge_brush: gdi::Hbrush,
    fonts: Vec<gdi::Hfont>,
    band_h: i32,
    client_w: i32,
    edge: i32,
}

impl Painter {
    fn set_text(&mut self, key: &str, text: &str) {
        if let Some(it) = self.items.iter_mut().find(|i| i.key == key) {
            it.text = text.to_string();
        }
    }

    fn paint(&self, hwnd: gdi::Hwnd) {
        unsafe {
            let mut ps = gdi::PaintStruct::default();
            let hdc = gdi::BeginPaint(hwnd, &mut ps);
            if hdc.is_null() {
                return;
            }

            let mut rc = gdi::Rect::default();
            gdi::GetClientRect(hwnd, &mut rc);

            // 整体底色
            gdi::FillRect(hdc, &rc, self.body_brush);
            // 顶部标题带
            let band = gdi::Rect {
                left: 0,
                top: 0,
                right: self.client_w.max(rc.right),
                bottom: self.band_h,
            };
            gdi::FillRect(hdc, &band, self.band_brush);

            // 面板（「粘贴区」）：先铺边框色，再内缩铺底色，形成 2px 描边
            for p in &self.panels {
                let outer = gdi::Rect {
                    left: p.x,
                    top: p.y,
                    right: p.x + p.w,
                    bottom: p.y + p.h,
                };
                gdi::FillRect(hdc, &outer, self.panel_edge_brush);
                let inner = gdi::Rect {
                    left: p.x + self.edge,
                    top: p.y + self.edge,
                    right: p.x + p.w - self.edge,
                    bottom: p.y + p.h - self.edge,
                };
                gdi::FillRect(hdc, &inner, self.panel_brush);
            }

            // 逐条写字
            gdi::SetBkMode(hdc, gdi::TRANSPARENT);
            for it in &self.items {
                let f = self.fonts[it.font as usize];
                if !f.is_null() {
                    gdi::SelectObject(hdc, f);
                }
                gdi::SetTextColor(hdc, gdi::rgb(it.color));
                let mut r = gdi::Rect {
                    left: it.x,
                    top: it.y,
                    right: it.x + it.w,
                    bottom: it.y + it.h,
                };
                let mut buf = gdi::wide(&it.text);
                gdi::DrawTextW(
                    hdc,
                    buf.as_mut_ptr(),
                    buf.len() as i32,
                    &mut r,
                    it.dt | gdi::DT_NOPREFIX,
                );
            }

            gdi::EndPaint(hwnd, &ps);
        }
    }
}

// ---------------------------------------------------------------------------
// 缩放
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Lay {
    s: f64,
}

impl Lay {
    fn new() -> Self {
        let s = nwg::scale_factor();
        Lay {
            s: if s > 0.1 { s } else { 1.0 },
        }
    }
    fn px(&self, v: i32) -> i32 {
        (v as f64 * self.s).round() as i32
    }
    fn at(&self, x: i32, y: i32, w: i32, h: i32) -> ((i32, i32), (i32, i32)) {
        ((self.px(x), self.px(y)), (self.px(w), self.px(h)))
    }
}

fn make_font(l: &Lay, size_pt: i32, weight: i32) -> gdi::Hfont {
    // CreateFontW 的高度取负值 = 字符高度（而不是单元格高度），更符合直觉
    let h = -l.px(size_pt);
    let face = gdi::wide("Microsoft YaHei UI");
    unsafe {
        gdi::CreateFontW(
            h,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            gdi::DEFAULT_CHARSET,
            gdi::OUT_TT_PRECIS,
            0,
            gdi::CLEARTYPE_QUALITY,
            0,
            face.as_ptr(),
        )
    }
}

// ---------------------------------------------------------------------------
// 界面对象
// ---------------------------------------------------------------------------

struct Ui {
    window: nwg::Window,
    #[allow(dead_code)]
    fonts: Vec<nwg::Font>,
    log: nwg::TextBox,
    reply: nwg::TextBox,
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
    confirm_write: bool,
    busy: bool,
}

/// 路径太长会把标题带撑破，只留最后两级。
fn shorten_path(p: &std::path::Path) -> String {
    let parts: Vec<String> = p
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if parts.len() <= 3 {
        return p.display().to_string();
    }
    format!("…\\{}", parts[parts.len() - 2..].join("\\"))
}

/// 单击 exe 时工作目录就是 exe 所在目录，直接拿它当输出目录会得到一个空的
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
// 搭建
// ---------------------------------------------------------------------------

fn build_ui() -> Result<(Ui, Painter), Box<dyn std::error::Error>> {
    let l = Lay::new();

    let mut window = nwg::Window::default();
    let ((wx, wy), (ww, wh)) = l.at(0, 0, WIN_W, WIN_H);
    nwg::Window::builder()
        .flags(
            nwg::WindowFlags::WINDOW | nwg::WindowFlags::MINIMIZE_BOX | nwg::WindowFlags::VISIBLE,
        )
        .size((ww, wh))
        .position((wx.max(60), wy.max(40)))
        .title("steam-curator — Steam 库整理器")
        .build(&mut window)?;

    // 让父窗口作画时自动避开子控件，避免自绘把按钮刷掉
    if let Some(h) = window.handle.hwnd() {
        unsafe {
            let h = h as gdi::Hwnd;
            let style = gdi::SetWindowLongPtrW(h, gdi::GWL_STYLE, 0);
            gdi::SetWindowLongPtrW(h, gdi::GWL_STYLE, style | gdi::WS_CLIPCHILDREN);
            gdi::SetWindowPos(
                h,
                std::ptr::null_mut(),
                0,
                0,
                0,
                0,
                gdi::SWP_NOMOVE
                    | gdi::SWP_NOSIZE
                    | gdi::SWP_NOZORDER
                    | gdi::SWP_NOACTIVATE
                    | gdi::SWP_FRAMECHANGED,
            );
        }
    }

    // 按钮/输入框用 NWG 自己的字体对象（真控件需要 HWND 级别的字体）
    let mut nwg_font = nwg::Font::default();
    nwg::Font::builder()
        .family("Microsoft YaHei UI")
        .size(((15.0 * l.s).round() as u32).max(9))
        .build(&mut nwg_font)?;
    let mut nwg_font_bold = nwg::Font::default();
    nwg::Font::builder()
        .family("Microsoft YaHei UI")
        .size(((15.0 * l.s).round() as u32).max(9))
        .weight(700)
        .build(&mut nwg_font_bold)?;

    let mkbtn = |out: &mut nwg::Button,
                     text: &str,
                     x: i32,
                     y: i32,
                     w: i32,
                     h: i32,
                     font: &nwg::Font|
     -> Result<(), nwg::NwgError> {
        let ((px, py), (pw, ph)) = l.at(x, y, w, h);
        nwg::Button::builder()
            .text(text)
            .parent(&window)
            .position((px, py))
            .size((pw, ph))
            .font(Some(font))
            .build(out)
    };

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

    mkbtn(&mut b_scan, "扫描 Steam 库", LEFT_X, 126, LEFT_W, BTN_H, &nwg_font)?;
    mkbtn(&mut b_prompt, "生成 AI Prompt", LEFT_X, 178, LEFT_W, BTN_H, &nwg_font)?;
    mkbtn(&mut b_copy, "复制 Prompt 到剪贴板", LEFT_X, 230, LEFT_W, BTN_H, &nwg_font)?;
    mkbtn(&mut b_plan, "校验 AI 回复", LEFT_X, 322, LEFT_W, BTN_H, &nwg_font)?;
    mkbtn(&mut b_preview, "打开预览报告", LEFT_X, 374, LEFT_W, BTN_H, &nwg_font)?;
    mkbtn(&mut b_dry, "演练写回（不改文件）", LEFT_X, 466, LEFT_W, BTN_H, &nwg_font)?;
    mkbtn(&mut b_write, "正式写回", LEFT_X, 518, LEFT_W, BTN_H, &nwg_font_bold)?;
    mkbtn(&mut b_restore, "回滚最近备份", LEFT_X, 570, LEFT_W, BTN_H, &nwg_font)?;
    mkbtn(&mut b_inspect, "查看库现状（只读）", LEFT_X, 662, LEFT_W, BTN_H, &nwg_font)?;
    mkbtn(&mut b_opendir, "打开输出目录", LEFT_X, 714, LEFT_W, BTN_H, &nwg_font)?;
    mkbtn(&mut b_clear, "清空日志", LEFT_X, 766, LEFT_W, BTN_H, &nwg_font)?;

    let mut log = nwg::TextBox::default();
    {
        let ((px, py), (pw, ph)) = l.at(RIGHT_X, 126, RIGHT_W, 400);
        nwg::TextBox::builder()
            .text("")
            .parent(&window)
            .position((px, py))
            .size((pw, ph))
            .readonly(true)
            .font(Some(&nwg_font))
            .build(&mut log)?;
    }
    let mut reply = nwg::TextBox::default();
    {
        // 落在「粘贴区」面板的内层里
        let ((px, py), (pw, ph)) = l.at(RIGHT_X + 16, 598, RIGHT_W - 32, 240);
        nwg::TextBox::builder()
            .text("")
            .parent(&window)
            .position((px, py))
            .size((pw, ph))
            .font(Some(&nwg_font))
            .build(&mut reply)?;
    }

    // ---- 自绘层的静态文字 ----
    let px = |v: i32| l.px(v);
    let single = gdi::DT_LEFT | gdi::DT_SINGLELINE | gdi::DT_VCENTER;
    let wrap = gdi::DT_LEFT | gdi::DT_WORDBREAK;
    let d = |key: &'static str,
                 text: &str,
                 x: i32,
                 y: i32,
                 w: i32,
                 h: i32,
                 color: [u8; 3],
                 font: FontId,
                 dt: u32| DrawItem {
        key,
        text: text.to_string(),
        x: px(x),
        y: px(y),
        w: px(w),
        h: px(h),
        color,
        font,
        dt,
    };

    let items = vec![
        d("title", "steam-curator", 28, 14, 700, 36, C_BAND_TEXT, FontId::BandTitle, single),
        d(
            "subtitle",
            "Steam 库整理器    ·    本地扫描 → 交给 AI 分类 → 校验 → 预览 → 写回合集",
            28, 50, 900, 24,
            C_BAND_SUB, FontId::BandSmall, single,
        ),
        d("status", "准备就绪", 790, 16, WIN_W - 790 - 28, 56, C_BAND_SUB, FontId::BandSmall, wrap),
        d("cap1", "①  准备", LEFT_X, 100, LEFT_W, 22, C_ACCENT, FontId::Cap, single),
        d("cap2", "②  整理", LEFT_X, 296, LEFT_W, 22, C_ACCENT, FontId::Cap, single),
        d("cap3", "③  写回", LEFT_X, 440, LEFT_W, 22, C_ACCENT, FontId::Cap, single),
        d("cap4", "④  其它", LEFT_X, 636, LEFT_W, 22, C_ACCENT, FontId::Cap, single),
        d(
            "safety",
            "⑤ 演练不改任何文件\n⑥ 写回前须完全退出 Steam（含托盘图标）\n写前自动整份备份，可随时回滚\n不联网，数据只在本地 output",
            LEFT_X, 828, LEFT_W, 78,
            C_MUTED, FontId::Small, wrap,
        ),
        d("logtitle", "运行日志 / 结果", RIGHT_X, 100, RIGHT_W, 22, C_TEXT, FontId::H2, single),
        d(
            "replytitle",
            "把 AI 的回复整段粘到这里　↓　然后点左边的「校验 AI 回复」",
            RIGHT_X + 16, 566, RIGHT_W - 32, 24, C_TEXT, FontId::H2, single,
        ),
        d(
            "hint",
            "提示：AI 编造的 appid 会被逐个拦下并报告，不会写进 Steam；校验前会先剥掉 ``` 围栏与前后寒暄。",
            RIGHT_X, 866, RIGHT_W, 24,
            C_MUTED, FontId::Small, single,
        ),
    ];

    let edge = l.px(2).max(1);
    let painter = Painter {
        items,
        // 「粘贴区」：浅蓝底 + 强调色描边，一眼就能看出 AI 回复该放哪
        panels: vec![Panel {
            x: px(RIGHT_X),
            y: px(552),
            w: px(RIGHT_W),
            h: px(300),
        }],
        band_brush: unsafe { gdi::CreateSolidBrush(gdi::rgb(C_BAND)) },
        body_brush: unsafe { gdi::CreateSolidBrush(gdi::rgb(C_BODY)) },
        panel_brush: unsafe { gdi::CreateSolidBrush(gdi::rgb(C_PANEL)) },
        panel_edge_brush: unsafe { gdi::CreateSolidBrush(gdi::rgb(C_PANEL_EDGE)) },
        fonts: vec![
            make_font(&l, 22, gdi::FW_BOLD),
            make_font(&l, 12, gdi::FW_NORMAL),
            make_font(&l, 12, gdi::FW_BOLD),
            make_font(&l, 14, gdi::FW_BOLD),
            make_font(&l, 12, gdi::FW_NORMAL),
        ],
        band_h: px(BAND_H),
        client_w: px(WIN_W),
        edge,
    };

    let ui = Ui {
        window,
        fonts: vec![nwg_font, nwg_font_bold],
        log,
        reply,
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
    };
    Ok((ui, painter))
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

    let (ui, painter) = build_ui()?;
    let ui = Rc::new(ui);
    let painter = Rc::new(RefCell::new(painter));

    let out_dir = resolve_out_dir();
    let state = Rc::new(RefCell::new(State {
        out_dir,
        confirm_write: false,
        busy: false,
    }));

    refresh_status(&painter, &state.borrow());
    ui.set_log(&boot_message(&state.borrow().out_dir));

    // ---- 自绘：整个静态层 ----
    let win_hwnd = ui.window.handle.hwnd().map(|h| h as gdi::Hwnd);
    let _paint = nwg::bind_raw_event_handler(&ui.window.handle, 0x10001, {
        let painter = painter.clone();
        move |hwnd, msg, _w, _l| {
            const WM_PAINT: u32 = 0x000F;
            const WM_ERASEBKGND: u32 = 0x0014;
            match msg {
                // 底色与标题带都在 WM_PAINT 里画，这里直接声称已擦除以免闪一下系统灰
                WM_ERASEBKGND => Some(1),
                WM_PAINT => {
                    painter.borrow().paint(hwnd as gdi::Hwnd);
                    Some(0)
                }
                _ => None,
            }
        }
    })?;

    // 首帧强制重绘一次
    if let Some(h) = win_hwnd {
        unsafe {
            gdi::InvalidateRect(h, std::ptr::null(), 0);
        }
    }

    let handler = nwg::full_bind_event_handler(&ui.window.handle, {
        let ui = ui.clone();
        let state = state.clone();
        let painter = painter.clone();
        move |evt, _evt_data, handle| match evt {
            nwg::Event::OnWindowClose => nwg::stop_thread_dispatch(),
            nwg::Event::OnButtonClick => handle_click(&ui, &painter, &state, handle),
            _ => {}
        }
    });

    nwg::dispatch_thread_events();
    nwg::unbind_event_handler(&handler);
    Ok(())
}

fn refresh_status(painter: &Rc<RefCell<Painter>>, st: &State) {
    let steam = if steam_curator::steam::steam_running() {
        "Steam 运行中，写入会被拒绝"
    } else {
        "Steam 未运行，可安全写入"
    };
    let plan = if st.out_dir.join(session::F_PLAN).is_file() {
        "已有整理方案"
    } else {
        "尚无整理方案"
    };
    let text = format!(
        "输出目录   {}\n当前状态   {} · {}",
        shorten_path(&st.out_dir),
        plan,
        steam
    );
    painter.borrow_mut().set_text("status", &text);
}

fn boot_message(out_dir: &std::path::Path) -> String {
    let has_lib = out_dir.join(session::F_LIBRARY).is_file();
    let has_plan = out_dir.join(session::F_PLAN).is_file();
    let has_backup = out_dir.join("backups").is_dir();
    let mut s = String::new();
    s.push_str("steam-curator\r\n");
    s.push_str("==============================\r\n\r\n");
    s.push_str(&format!("输出目录：{}\r\n", out_dir.display()));
    s.push_str(&format!(
        "当前状态：{}{}\r\n\r\n",
        if has_lib { "已有扫描数据" } else { "尚未扫描" },
        if has_plan {
            "，已有整理方案"
        } else {
            "，尚无整理方案"
        }
    ));
    s.push_str("使用顺序：\r\n");
    s.push_str("  ① 扫描 Steam 库\r\n");
    s.push_str("  ② 生成 AI Prompt  →  复制到剪贴板  →  贴给任意 AI\r\n");
    s.push_str("  ③ 把 AI 的回复粘到下面的输入框，点「校验 AI 回复」\r\n");
    s.push_str("  ④ 打开预览报告，确认效果\r\n");
    s.push_str("  ⑤ 演练写回（不改文件）  →  ⑥ 正式写回\r\n\r\n");
    if has_backup {
        s.push_str("检测到已有备份目录，「回滚最近备份」可随时还原。\r\n");
    }
    s
}

// ---------------------------------------------------------------------------
// 按钮分发
// ---------------------------------------------------------------------------

fn handle_click(
    ui: &Rc<Ui>,
    painter: &Rc<RefCell<Painter>>,
    state: &Rc<RefCell<State>>,
    handle: nwg::ControlHandle,
) {
    let mut st = state.borrow_mut();
    if st.busy {
        return;
    }

    // 除了「正式写回」的第二次点击，其余任何点击都要复位确认态
    let is_write_btn = handle == ui.b_write.handle;
    if !is_write_btn && st.confirm_write {
        st.confirm_write = false;
        ui.b_write.set_text("正式写回");
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
        report(ui, painter, &mut st, r);
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
                "⚠ 还没生成 prompt。请先点「生成 AI Prompt」。\r\n（找不到 {}）",
                out_dir.join(session::F_PROMPT).display()
            )),
        }
        return;
    }

    // 正式写回：两步确认
    if is_write_btn {
        if !st.confirm_write {
            st.confirm_write = true;
            ui.b_write.set_text("⚠ 再点一次确认");
            ui.set_log(
                "【确认】正式写回会修改你 Steam 里的合集。\r\n\r\n\
                 写之前请确认：\r\n\
                 1. Steam 已经完全退出（包括右下角托盘图标）；\r\n\
                 2. 你已经用「演练写回」看过结果，确认无误。\r\n\r\n\
                 写之前程序会自动把合集文件、命名空间版本文件、localconfig.vdf\r\n\
                 整份备份到 output\\backups\\ 下，随时可用「回滚最近备份」还原。\r\n\r\n\
                 确认无误就再点一次「⚠ 再点一次确认」。",
            );
            return;
        }
        st.confirm_write = false;
        ui.b_write.set_text("正式写回");
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
        report(ui, painter, &mut st, r);
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
                "输入框是空的。\r\n\r\n请把 AI 的回复整段粘到右边那个框里再点「校验 AI 回复」。\r\n\
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
    report(ui, painter, &mut st, result);
}

fn report(
    ui: &Rc<Ui>,
    painter: &Rc<RefCell<Painter>>,
    st: &mut State,
    result: Result<session::Outcome, String>,
) {
    match result {
        Ok(outcome) => ui.set_log(&outcome.text()),
        Err(e) => ui.set_log(&format!("✖ 出错了\r\n\r\n{}", e)),
    }
    refresh_status(painter, st);
    // 状态文字变了，重画标题带那一块
    if let Some(h) = ui.window.handle.hwnd() {
        unsafe {
            gdi::InvalidateRect(h as gdi::Hwnd, std::ptr::null(), 0);
        }
    }
}
