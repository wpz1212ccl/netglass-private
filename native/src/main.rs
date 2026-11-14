// GUI 子系统：不派生 conhost 控制台（双击/自启时不再弹黑窗口）。
// 诊断输出（--bench/--selftest 等）在 release 下仍走 stdout——从已有
// 终端里运行时照常可见；只是不再由系统自动配一个控制台。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! NetGlass native widget.
//!
//! A layered window (`WS_EX_LAYERED` + `UpdateLayeredWindow`) whose alpha channel
//! is exactly the shape: no rectangular backdrop, no WebView, per-pixel alpha
//! hit-testing for free.
//!
//! 形态由 `mode.rs` 的 260 ms 前台轮询状态机决定（expanded / island / hidden），
//! 形变由 `anim.rs` 的弹簧线程推进：每帧「纯形状重绘 → ULW(psize=当前矩形)」，
//! 中间帧没有文字（`.morphing` 语义，见 `morph.rs`）。
//!
//! Modes:
//!   (no args)              run the widget
//!   --silent               自启动：桌面稳定到展开前不显窗
//!   --pos X,Y              initial window position (physical px)
//!   --pixels X,Y[;X,Y…]    print screen RGB at physical coordinates
//!   --hit X,Y[;X,Y…]       WindowFromPoint identity at physical coordinates
//!   --selftest             render offscreen, report DIB alpha inside/outside the shape
//!   --morph-frame MS       定格形变中间帧（配 --morph-dir=expand|collapse 取证）
//!   --morph-demo           诊断：真实 apply_mode 路径上交替展开/收回（几何连续性取证）
//!   --show-after MS        诊断：起手隐藏，MS 后走真实路径显示初始形态（恢复首帧取证）
//!   --dpi-test DPI         诊断：1.5 s 后投递一次 DPI 变化（跨屏重建取证）
//!
//! 系统集成（T6，纯 Win32 零新增三方 crate）：
//!   托盘（`tray.rs` + `menu.rs`）、全局快捷键（`hotkey.rs`）、锁定（`lock.rs`）、
//!   自启（`autostart.rs`）、config.json（`config.rs`）、单实例（`instance.rs`）。

mod anim;
mod autostart;
mod card;
mod config;
mod font;
mod fx;
mod hotkey;
mod input;
mod instance;
mod island;
mod lock;
mod menu;
mod mode;
mod morph;
mod number;
mod netmon;
mod probe;
mod render;
mod tray;
mod window;

use std::cell::RefCell;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::core::Result;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, ReleaseCapture, SetCapture, TrackMouseEvent, UnregisterHotKey, TME_LEAVE,
    TRACKMOUSEEVENT,
};
use windows::Win32::UI::HiDpi::{
    GetDpiForSystem, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::*;

const LOGICAL_W: f32 = mode::CARD_W as f32;
const LOGICAL_H: f32 = mode::CARD_H as f32;
const HIST: usize = 60;
// wry/windows-rs does not expose WM_MOUSELEAVE; the value is stable since Win2000.
const WM_MOUSELEAVE: u32 = 0x02A3;
/// 岛态呼吸/颜色过渡定时器 id（66 ms，D1 15 fps）。
const TIMER_BREATHE: usize = 1;
/// 展开态 hover 详情时间轴定时器 id（16 ms，只在动画进行中运行）。
const TIMER_HOVER: usize = 2;
/// 数字弹簧定时器 id（16 ms，只在动画进行中运行；渲染另有 30 fps 闸）。
const TIMER_NUM: usize = 3;
/// 隐藏/显示 220 ms 淡出淡入定时器 id。
const TIMER_FADE: usize = 4;
/// 形变看门狗定时器 id（16 ms，判内容进场与形变收尾）。
const TIMER_MORPH: usize = 5;
/// 形变保险丝：弹簧异常时也必须收尾，不能卡在中间帧。
const MORPH_TIMEOUT_MS: u64 = 2000;
/// 全局快捷键 id（`RegisterHotKey`/`WM_HOTKEY` 共用）。
const HOTKEY_ID: i32 = 1;

/// 实际显示形态。apply_mode 维护；形态线程写、UI 线程读。
static MODE: AtomicU8 = AtomicU8::new(0);
/// 岛态挂载点与 hover 放大状态：形态线程（躲让）与 UI 线程（hover）共享，
/// 否则两边各算一套 target，切换时胶囊会跳回居中/缩回原尺寸。
static ANCHOR: AtomicU8 = AtomicU8::new(0);
static HOT: AtomicU8 = AtomicU8::new(0);
/// 手动展开的 8 秒 pin（对照 `expand_temporarily`）：形态线程读它决定是否保持展开。
static PIN_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);
static PIN_FG: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
/// 双击收回后的手动胶囊态（对照 `MANUAL_ISLAND`）：压过桌面 2 s settle。
static MANUAL_ISLAND: AtomicU8 = AtomicU8::new(0);
/// D6：托盘手动隐藏。形态线程读它写进 `ModeGate`，全屏路过/退出全屏都不重显。
static MANUAL_HIDDEN: AtomicU8 = AtomicU8::new(0);
/// 跨屏 DPI 变化后的新布局（UI 线程在 `WM_DPICHANGED` 里写，形态线程每拍取走）。
/// 形态线程持有自己的 `Layout` 副本，不跟着换就会按旧缩放把窗口弹回旧尺寸。
static DPI_UPDATE: Mutex<Option<DpiUpdate>> = Mutex::new(None);

#[derive(Clone, Copy)]
struct DpiUpdate {
    scale: f32,
    screen: anim::Rect,
    home: (i32, i32),
}

/// 几何线程待呈现的矩形（UI 线程在 WM_APP_GEOMETRY 里取走）。
static PENDING: Mutex<Option<anim::Rect>> = Mutex::new(None);
/// 窗口当前在屏幕上的矩形（几何线程从空闲被唤醒时用它重新起算）。
static PRESENTED: Mutex<anim::Rect> = Mutex::new(anim::Rect::new(0, 0, 1, 1));

fn main() {
    window::dpi_aware();
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "--dpi") {
        let r =
            unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        println!(
            "SetProcessDpiAwarenessContext(PMv2) -> {r:?} err={:?}",
            std::io::Error::last_os_error()
        );
        println!(
            "GetDpiForSystem={} SM_CXSCREEN={} SM_CYSCREEN={}",
            unsafe { GetDpiForSystem() },
            unsafe { GetSystemMetrics(SM_CXSCREEN) },
            unsafe { GetSystemMetrics(SM_CYSCREEN) }
        );
        return;
    }
    if args.iter().any(|a| a == "--pixels") {
        for (x, y) in window::parse_points(&args, "--pixels") {
            let (r, g, b) = window::screen_pixel(x, y);
            println!("{x},{y} #{r:02X}{g:02X}{b:02X} rgb({r},{g},{b})");
        }
        return;
    }
    if args.iter().any(|a| a == "--hit") {
        for (x, y) in window::parse_points(&args, "--hit") {
            println!("{}", window::describe_at(x, y));
        }
        return;
    }
    if args.iter().any(|a| a == "--fonts") {
        if let Err(e) = fonts() {
            eprintln!("fonts failed: {e:?}");
            std::process::exit(1);
        }
        return;
    }
    if args.iter().any(|a| a == "--bench") {
        if let Err(e) = bench() {
            eprintln!("bench failed: {e:?}");
            std::process::exit(1);
        }
        return;
    }
    if args.iter().any(|a| a == "--selftest") {
        if let Err(e) = selftest() {
            eprintln!("selftest failed: {e:?}");
            std::process::exit(1);
        }
        return;
    }
    if let Some(ms) = parse_morph_frame(&args) {
        if let Err(e) = morph_frame(ms, &args) {
            eprintln!("morph-frame failed: {e:?}");
            std::process::exit(1);
        }
        return;
    }
    // T6 诊断：托盘图标解码 / 自启注册表通路（都在临时资源上，不碰用户状态）。
    if args.iter().any(|a| a == "--icon") {
        tray::dump_icon();
        return;
    }
    if args.iter().any(|a| a == "--autostart-selftest") {
        let ok = autostart::selftest();
        println!("autostart selftest: {}", if ok { "PASS" } else { "FAIL" });
        if !ok {
            std::process::exit(1);
        }
        return;
    }
    if args.iter().any(|a| a == "--tray-menu") {
        // 菜单结构自证：真的建一个 Win32 菜单再读回来（不需要点托盘图标）。
        let hotkey = hotkey::pick(|_| true);
        let on = autostart::is_enabled();
        println!("hotkey={:?} autostart={on}", hotkey.map(|h| h.label));
        tray::dump_menu(&menu::items(hotkey, on));
        return;
    }
    if args.iter().any(|a| a == "--config") {
        println!("path={:?}", config::path());
        println!("{}", config::to_pretty(&config::load()));
        return;
    }
    if let Err(e) = run(&args) {
        eprintln!("netglass-native failed: {e:?}");
        std::process::exit(1);
    }
}

/// 一块画布 + 绑定它的渲染器。形态变化时按目标尺寸重建。
/// 字段顺序决定 drop 顺序：renderer（引用 mem_dc）必须先于 surface 释放。
struct Ui {
    renderer: render::Renderer,
    surface: window::Surface,
    w: i32,
    h: i32,
}

impl Ui {
    fn new(hwnd: Option<HWND>, w: i32, h: i32, scale: f32) -> Result<Self> {
        let surface = window::Surface::new(hwnd, w, h)?;
        let renderer = render::Renderer::new(surface.mem_dc, w, h, scale)?;
        Ok(Self {
            renderer,
            surface,
            w,
            h,
        })
    }
}

struct App {
    ui: Ui,
    hwnd: HWND,
    scale: f32,
    layout: mode::Layout,
    /// 已应用的形态码（`applied == 目标码 && visible` 时跳过重复应用）。
    applied: u8,
    visible: bool,

    hist: [f64; HIST],
    up_hist: [f64; HIST],
    unit_mb: bool,
    hover: bool,
    /// 岛态按下（玻璃 brightness .965）。
    active: bool,
    drag: Option<(POINT, i32, i32)>,
    rx: Receiver<netmon::Sample>,
    probe: Arc<Mutex<Option<probe::ProbePayload>>>,

    /// 呼吸灯状态（66 ms 定时器；DC/隐藏/展开时回到静态点）。
    battery: bool,
    tint: island::TintTween,
    dot_target: island::DotColor,
    dot_override: Option<island::DotColor>,
    breathe_on: bool,
    timer_on: bool,
    breathe_t0: Instant,
    last_tick: Instant,

    title: String,
    state: String,
    wifi: String,
    wifi_ok: bool,
    /// 底栏代理值 + 详情第 5 格代理值（同一份 probe 载荷的两种文案）。
    foot_proxy: String,
    proxy_tone: card::Tone,
    detail_proxy: String,
    detail_tone: card::Tone,
    latency: Option<f64>,
    link_mbps: Option<f64>,
    down: (String, String),
    up: (String, String),
    last_key: String,
    /// 展开态 hover 时间轴（16 ms 定时器只在动画期间运行）。
    hover_anim: fx::HoverAnim,
    hover_timer_on: bool,
    hover_t0: Instant,

    /// 数字弹簧（P3-7）：目标值是原始字节速率，显示时再格式化。
    numbers: number::NumberAnim,
    number_timer_on: bool,
    num_t0: Instant,
    num_last: Instant,
    /// 上行/下行当前显示的字节速率（未取整），供 content_key 判重。
    rate_down: f64,
    rate_up: f64,

    /// 形变状态机（P3-6）：None = 静止。
    morph: Option<Morph>,
    morph_timer_on: bool,
    morph_alpha_step: u8,
    /// 形变中间帧的纯形状绘制器（按当前矩形重画，不缩放整张卡片）。
    shapes: Option<render::ShapePainter>,
    /// 隐藏/显示淡出（0..=255 全局 alpha，`FADE_MS` 内线性）。
    fade: Option<Fade>,
    fade_timer_on: bool,

    /// 最近一次左键抬起（双击判定用）与单调时钟基准。
    last_click: Option<input::Click>,
    /// 上一击的**手势起点形态**：双击的第二击沿用它判定，避免被第一击改掉的
    /// 形变目标（`effective_mode`）误判成「卡片双击」。
    last_click_mode: Option<mode::UiMode>,
    click_t0: Instant,
    /// 按下起点与当前矩形（拖动阈值 / 点击判定）。
    press: Option<(POINT, i32, i32)>,
    press_moved: bool,

    /// 锁定态（P4-2）：写 `WS_EX_TRANSPARENT`，并写回 config.json。
    locked: lock::Lock,
    /// 实际注册到的快捷键（菜单文案要用它的名字）。
    hotkey: Option<&'static hotkey::Hotkey>,
    /// 自启当前是否已写入注册表（菜单勾选态的唯一来源）。
    autostart_on: bool,
    /// 最近一次真正显示过的形态（隐藏前的），托盘重新显示时还原它。
    last_shown: mode::UiMode,
}

/// 一次形变：起点/终点矩形 + 形状弹簧与内容时间轴。
struct Morph {
    from: anim::Rect,
    to: anim::Rect,
    /// 目标形态（进场延迟与弹簧 delay 都由它决定）。
    target: mode::UiMode,
    t0: Instant,
    /// 最近一帧的几何（几何线程回报，内容进场判据要读它）。
    cur: anim::Rect,
    /// 进场起点（相对 t0 的毫秒数）：None = 还在退场/纯形状阶段。
    enter_at: Option<f64>,
}

/// 隐藏/显示的 220 ms 淡出（`.card.out`）。
struct Fade {
    t0: Instant,
    /// true = 淡出到 0 后隐藏；false = 从 0 淡入。
    out: bool,
}

impl App {
    fn frame(&self) -> render::Frame {
        let elapsed = self.breathe_t0.elapsed().as_secs_f64() * 1000.0;
        let (alpha, scale) = if self.breathe_on {
            island::breathe_at(elapsed)
        } else {
            (1.0, 1.0)
        };
        let phase = if self.breathe_on {
            island::breathe_phase(elapsed)
        } else {
            0
        };
        let tint = self.tint.current();
        let peak = self.hist.iter().cloned().fold(0.0f64, f64::max);
        let peak_up = self.up_hist.iter().cloned().fold(0.0f64, f64::max);
        let details = card::detail_rows(
            self.latency,
            peak,
            self.link_mbps,
            peak_up,
            (&self.detail_proxy, self.detail_tone),
        );
        let now_ms = self.hover_t0.elapsed().as_secs_f64() * 1000.0;
        render::Frame {
            title: self.title.clone(),
            // 现版前端把锁定状态拼在连接状态后面（`index.html:478`）。
            state: lock::state_text(&self.state, self.locked.is_on()),
            down_value: self.down.0.clone(),
            down_unit: self.down.1.clone(),
            up_value: self.up.0.clone(),
            up_unit: self.up.1.clone(),
            wifi: self.wifi.clone(),
            wifi_ok: self.wifi_ok,
            proxy: self.foot_proxy.clone(),
            proxy_tone: self.proxy_tone,
            details,
            hist: self.hist.to_vec(),
            hover: self.hover,
            hover_fx: self.hover_anim.at(now_ms),
            active: self.active,
            dot: render::DotPaint {
                rgb: tint.rgb,
                glow: tint.glow,
                alpha,
                scale,
                phase,
            },
            content_alpha: self.content_alpha(),
        }
    }

    /// 内容不透明度：退场（旧内容淡出）→ 0（纯形状）→ 进场（新内容淡入）。
    /// 不在形变中时恒为 1。
    fn content_alpha(&self) -> f32 {
        let Some(m) = &self.morph else {
            return 1.0;
        };
        let t = m.t0.elapsed().as_secs_f64() * 1000.0;
        match m.enter_at {
            None => morph::fade_out_alpha(t, m.target),
            Some(at) => morph::content_in_alpha(t - at),
        }
    }

    fn current_mode(&self) -> mode::UiMode {
        mode::mode_from_code(self.applied)
    }

    /// 交互判定用的形态：形变途中以**目标**形态为准。
    /// `applied` 要等内容进场才切（中间帧仍按旧形态画），但点击/拖动发生在
    /// 途中时，用户看到的是正在变过去的那一态。
    fn effective_mode(&self) -> mode::UiMode {
        match &self.morph {
            Some(m) => m.target,
            None => self.current_mode(),
        }
    }

    fn repaint(&self) {
        let f = self.frame();
        let r = match self.current_mode() {
            mode::UiMode::Island => self.ui.renderer.paint_island(&f),
            _ => self.ui.renderer.paint(&f),
        };
        if let Err(e) = r {
            log_line(&format!("paint err {e:?}"));
        }
    }

    /// 按 ULW 的 pptDst+psize 落位：形变期间尺寸来自弹簧，不碰 SetWindowPos。
    fn present_rect(&self, r: anim::Rect) {
        let alpha = self.fade_alpha();
        let _ = self
            .ui
            .surface
            .present_rect_alpha(r.x, r.y, r.w, r.h, alpha);
        if let Ok(mut g) = PRESENTED.lock() {
            *g = r;
        }
    }

    /// 淡出/淡入的全局 alpha（无淡出时 255）。
    fn fade_alpha(&self) -> u8 {
        let Some(f) = &self.fade else {
            return 255;
        };
        let k = (f.t0.elapsed().as_secs_f64() * 1000.0 / morph::FADE_MS).clamp(0.0, 1.0);
        let a = if f.out { 1.0 - k } else { k };
        (a * 255.0).round() as u8
    }

    /// 形变中间帧：按当前矩形重画纯形状再 ULW（psize=当前矩形，1:1 不缩放）。
    fn paint_shape_frame(&mut self, r: anim::Rect) {
        let Some(m) = &self.morph else {
            return;
        };
        let (from_w, to_w) = (m.from.w, m.to.w);
        let radius = morph::morph_frame_radius(r, from_w, to_w, self.scale);
        if self.shapes.is_none() {
            let (w, h) = self.shape_buffer();
            match render::ShapePainter::new(w, h, self.scale) {
                Ok(s) => self.shapes = Some(s),
                Err(e) => {
                    log_line(&format!("shape painter err {e:?}"));
                    return;
                }
            }
        }
        let alpha = self.fade_alpha();
        let hwnd = self.hwnd;
        let shapes = self.shapes.as_ref().unwrap();
        if let Err(e) = shapes.paint(r.w, r.h, radius) {
            log_line(&format!("shape paint err {e:?}"));
            return;
        }
        if let Err(e) = window::present_dc(hwnd, shapes.dc(), r.x, r.y, r.w, r.h, alpha) {
            log_line(&format!("shape present err {e:?}"));
        }
    }

    /// 形变帧缓冲区尺寸：要装得下弹簧过冲（实测 +7%）与两种形态的较大者。
    fn shape_buffer(&self) -> (i32, i32) {
        morph::shape_buffer(self.scale)
    }

    /// 几何线程回报一帧矩形。形变中：**按当前矩形重画纯形状**再 ULW；
    /// 非形变（hover 放大、躲让、呼吸）：沿用当前画布按 psize 裁切呈现。
    fn on_geometry(&mut self, r: anim::Rect) {
        if let Some(m) = &mut self.morph {
            m.cur = r;
            // 退场窗口内还没进入纯形状阶段：旧内容仍在淡出，别让形状帧盖上去。
            // 进场阶段由正文接管，几何线程的帧一律忽略（避免形状帧盖住文字）。
            if m.enter_at.is_some() {
                return;
            }
            let t = m.t0.elapsed().as_secs_f64() * 1000.0;
            if t < morph::retreat_ms(m.target) {
                return;
            }
            self.paint_shape_frame(r);
            if let Ok(mut g) = PRESENTED.lock() {
                *g = r;
            }
            return;
        }
        // 岛态 hover 放大 / 躲让：画布尺寸可能和目标不同，先按目标重建。
        if self.ui.w != r.w || self.ui.h != r.h {
            if let Some(t) = self.layout.target(self.current_mode()) {
                self.ensure_ui(t);
                self.repaint();
            }
        }
        self.present_rect(r);
    }

    /// 目标尺寸变了就重建画布（形态切换才发生，不是每帧）。
    fn ensure_ui(&mut self, target: anim::Rect) {
        if self.ui.w == target.w && self.ui.h == target.h {
            return;
        }
        match Ui::new(Some(self.hwnd), target.w, target.h, self.scale) {
            Ok(ui) => self.ui = ui,
            Err(e) => log_line(&format!("ui rebuild err {e:?}")),
        }
    }

    fn apply_mode(&mut self, m: mode::UiMode) {
        let code = mode::mode_code(m);
        // 形变途中目标没变：不重开动画（形态线程 260 ms 一拍会重复报同一形态）。
        if let Some(morph) = &self.morph {
            if morph.target == m {
                MODE.store(code, Ordering::Relaxed);
                return;
            }
        }
        if self.applied == code && self.visible {
            return;
        }
        // 形态切换时 hover 时间轴归零：展开态的详情动画不能带进胶囊/隐藏态，
        // 反向切换也从头开始（鼠标若仍压着，下一次 WM_MOUSEMOVE 会重新触发）。
        self.hover = false;
        self.hover_anim.reset();
        self.sync_hover_timer();
        // 躲让（形态线程）与 hover（UI 线程）共享挂载点/放大状态。
        self.layout.hot = HOT.load(Ordering::Relaxed) != 0;
        self.layout.anchor = mode::anchor_from_code(ANCHOR.load(Ordering::Relaxed));
        if m != mode::UiMode::Island {
            self.layout.hot = false;
            HOT.store(0, Ordering::Relaxed);
            self.active = false;
        }

        if m == mode::UiMode::Hidden {
            self.applied = code;
            MODE.store(code, Ordering::Relaxed);
            self.stop_morph();
            self.breathe_on = false;
            if self.timer_on {
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), TIMER_BREATHE);
                }
                self.timer_on = false;
            }
            if self.visible {
                // `.card.out` 220 ms 淡出画完再隐藏（对照 fade_out）。
                self.fade = Some(Fade {
                    t0: Instant::now(),
                    out: true,
                });
                self.sync_fade_timer();
            }
            return;
        }

        let Some(target) = self.layout.target(m) else {
            return;
        };
        let was_hidden = !self.visible;
        // 隐藏前的形态：托盘"显示"时还原它（现版 LAST_SHOWN 语义）。
        self.last_shown = m;
        // MODE 立刻更新（形态线程据此判"已应用"，不再重复派发）；
        // `applied` 留到收尾才切——形变中间帧要按**旧形态**的内容淡出。
        MODE.store(code, Ordering::Relaxed);

        if was_hidden {
            self.applied = code;
            // 首次显示 / 从隐藏态回来：先把目标尺寸的内容画好再露出来，避免空板。
            self.stop_morph();
            self.ensure_ui(target);
            self.repaint();
            self.present_rect(target);
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOWNA);
            }
            self.visible = true;
            VISIBLE_FLAG.store(1, Ordering::Relaxed);
            anim::snap_to(target);
            // 显示走 220 ms 淡入（`enter()` 的语义：内容已就绪，只淡不闪）
            self.fade = Some(Fade {
                t0: Instant::now(),
                out: false,
            });
            self.sync_fade_timer();
            self.sync_timer();
            return;
        }

        let from = *PRESENTED.lock().unwrap_or_else(|e| e.into_inner());
        if from.w == target.w && from.h == target.h && from.x == target.x && from.y == target.y {
            self.applied = code;
            self.stop_morph();
            self.ensure_ui(target);
            self.repaint();
            self.present_rect(target);
            self.sync_timer();
            return;
        }

        // 形变：内容退场 → 形状弹簧逐帧重绘 → 内容进场（morph.rs 的时间轴）。
        self.fade = None;
        self.sync_fade_timer();
        // 起点帧：旧形态内容 + 旧矩形（接下来 120 ms 内淡出，只剩形状）。
        if self.ui.w != from.w || self.ui.h != from.h {
            self.ensure_ui(from);
        }
        self.morph = Some(Morph {
            from,
            to: target,
            target: m,
            t0: Instant::now(),
            cur: from,
            enter_at: None,
        });
        self.morph_alpha_step = 16;
        self.repaint();
        self.present_rect(from);
        anim::animate_to(target, mode::motion_for(m));
        self.sync_morph_timer();
        self.sync_timer();
    }

    /// 形变收尾：目标尺寸的内容整帧画好再呈现。
    fn finish_morph(&mut self) {
        let Some(m) = self.morph.take() else {
            return;
        };
        self.applied = mode::mode_code(m.target);
        MODE.store(self.applied, Ordering::Relaxed);
        self.ensure_ui(m.to);
        self.repaint();
        self.present_rect(m.to);
        self.sync_morph_timer();
        self.sync_timer();
    }

    fn stop_morph(&mut self) {
        self.morph = None;
        self.shapes = None;
        self.sync_morph_timer();
    }

    /// 16 ms 形变定时器：只在形变进行中运行（阶段推进、进场判据、看门狗）。
    fn sync_morph_timer(&mut self) {
        let need = self.morph.is_some() && self.visible;
        if need && !self.morph_timer_on {
            unsafe {
                let _ = SetTimer(Some(self.hwnd), TIMER_MORPH, 16, None);
            }
            self.morph_timer_on = true;
        } else if !need && self.morph_timer_on {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_MORPH);
            }
            self.morph_timer_on = false;
        }
    }

    /// 形变三阶段推进：
    /// 1. **退场**（0 → 弹簧 delay）：旧形态内容淡出，窗口还停在起点矩形；
    /// 2. **纯形状**（delay → 几何落定）：几何线程每帧重画形状，一帧文字都没有；
    /// 3. **进场**：几何落定后按目标尺寸画正文，260 ms 淡入。
    ///
    /// 进场不等 `enter_delay`（250/360 ms）而等几何落定，是因为原生没有 WebView
    /// 在中间尺寸里重排文字的能力——差几个像素画上去就是被拉伸的字。落定时刻
    /// 实测：展开 ≈500 ms、收回 ≈400 ms，与前端 360/250 ms 的观感同量级。
    fn on_morph_timer(&mut self) {
        let Some(m) = &self.morph else {
            self.sync_morph_timer();
            return;
        };
        let t = m.t0.elapsed().as_secs_f64() * 1000.0;
        let (cur, target, to, enter_at) = (m.cur, m.target, m.to, m.enter_at);

        // 阶段 3：内容已在淡入，只等它走完（此阶段不再看几何/看门狗）。
        if let Some(at) = enter_at {
            if t - at >= morph::CONTENT_IN_MS {
                self.finish_morph();
                return;
            }
            let alpha = self.content_alpha();
            let step = (alpha * 16.0).round() as u8;
            if step != self.morph_alpha_step {
                self.morph_alpha_step = step;
                self.repaint();
                self.present_rect(to);
            }
            return;
        }

        // 保险丝：弹簧异常（几何线程没回报）也必须收尾，且只触发一次。
        if t >= MORPH_TIMEOUT_MS as f64 {
            log_line(&format!(
                "morph watchdog: t={t:.0} err={:.3}",
                morph::size_error(cur, to)
            ));
            self.begin_content_enter(t);
            return;
        }

        // 阶段 1：退场（旧内容淡出，窗口仍停在起点矩形）。
        if t < morph::retreat_ms(target) {
            let alpha = self.content_alpha();
            let step = (alpha * 16.0).round() as u8;
            if step != self.morph_alpha_step {
                self.morph_alpha_step = step;
                self.repaint();
                self.present_rect(cur);
            }
            return;
        }

        // 阶段 2：纯形状。几何线程逐帧重画；等到尺寸误差 ≤1.5% 才允许文字进场。
        if !morph::content_ready(t, target, cur, to) {
            return;
        }
        // 残余 ≤1.5%：吸附掉，之后几何线程不再发帧。
        anim::snap_to(to);
        self.begin_content_enter(t);
    }

    /// 切到目标形态、按目标尺寸整帧画好（文字从此第一次出现），进入淡入阶段。
    fn begin_content_enter(&mut self, t_ms: f64) {
        let Some(m) = &self.morph else {
            return;
        };
        let to = m.to;
        let target = m.target;
        anim::snap_to(to);
        self.applied = mode::mode_code(target);
        MODE.store(self.applied, Ordering::Relaxed);
        self.ensure_ui(to);
        if let Some(m) = &mut self.morph {
            m.cur = to;
            m.enter_at = Some(t_ms);
        }
        self.morph_alpha_step = 0;
        self.repaint();
        self.present_rect(to);
    }

    /// 左键单击（未拖动）：双击 = 卡片收回胶囊；胶囊单击 = 展开 8 秒。
    /// 对照前端：`dblclick` → `collapse_to_island`、`.mini` click →
    /// `expand_temporarily`。窗口没有 CS_DBLCLKS，双击由 `input` 自己数。
    fn on_click(&mut self) {
        let c = window::cursor_pos();
        let now = input::Click {
            at_ms: self.click_t0.elapsed().as_millis() as u64,
            x: c.x,
            y: c.y,
        };
        let dbl = input::is_double_click(self.last_click, now);
        // 双击的第二击必须沿用第一击的形态：第一击若已把形变目标改成卡片，
        // `effective_mode()` 此刻已返回 Expanded，按它判定就会把卡片又收回去。
        let mode_at_gesture_start = if dbl {
            self.last_click_mode.unwrap_or_else(|| self.effective_mode())
        } else {
            self.effective_mode()
        };
        self.last_click = if dbl { None } else { Some(now) };
        self.last_click_mode = if dbl {
            None
        } else {
            Some(mode_at_gesture_start)
        };
        match input::click_action(mode_at_gesture_start, dbl) {
            Some(input::ClickAction::CollapseCard) => {
                // 双击卡片：手动收回顶部胶囊，并压住桌面 2 s settle
                MANUAL_ISLAND.store(1, Ordering::Relaxed);
                if let Ok(mut g) = PIN_UNTIL.lock() {
                    *g = None;
                }
                self.layout.home = {
                    let cur = *PRESENTED.lock().unwrap_or_else(|e| e.into_inner());
                    (cur.x, cur.y)
                };
                MODE.store(mode::mode_code(mode::UiMode::Island), Ordering::Relaxed);
                self.apply_mode(mode::UiMode::Island);
            }
            Some(input::ClickAction::ExpandIsland) => {
                // 单击胶囊：立即展开并 pin 8 秒（前台窗口切走即失效）
                MANUAL_ISLAND.store(0, Ordering::Relaxed);
                if let Ok(mut g) = PIN_UNTIL.lock() {
                    *g = Some(Instant::now() + Duration::from_secs(mode::PIN_SECS));
                }
                let fg = unsafe { GetForegroundWindow() };
                PIN_FG.store(fg.0 as isize, Ordering::Relaxed);
                MODE.store(mode::mode_code(mode::UiMode::Expanded), Ordering::Relaxed);
                self.apply_mode(mode::UiMode::Expanded);
            }
            _ => {}
        }
    }

    /// 220 ms 淡出/淡入定时器：只在 fade 进行中运行。
    fn sync_fade_timer(&mut self) {
        let need = self.fade.is_some() && self.visible;
        if need && !self.fade_timer_on {
            unsafe {
                let _ = SetTimer(Some(self.hwnd), TIMER_FADE, 16, None);
            }
            self.fade_timer_on = true;
        } else if !need && self.fade_timer_on {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_FADE);
            }
            self.fade_timer_on = false;
        }
    }

    fn on_fade_timer(&mut self) {
        let Some(f) = &self.fade else {
            self.sync_fade_timer();
            return;
        };
        let done = f.t0.elapsed().as_secs_f64() * 1000.0 >= morph::FADE_MS;
        let out = f.out;
        let r = *PRESENTED.lock().unwrap_or_else(|e| e.into_inner());
        self.present_rect(r);
        if !done {
            return;
        }
        self.fade = None;
        self.sync_fade_timer();
        if out {
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
            self.visible = false;
            VISIBLE_FLAG.store(0, Ordering::Relaxed);
        }
    }

    /// 岛态呼吸/颜色过渡的 66 ms 定时器开关（D1：15 fps，DC/隐藏/展开暂停）。
    fn sync_timer(&mut self) {
        let island = self.current_mode() == mode::UiMode::Island && self.visible;
        // 形变期间呼吸暂停（`.morphing` 里点也一起退场，对照 updateBreathe 的 morphNow）。
        let morphing = self.morph.is_some();
        self.breathe_on = island::should_breathe(island, self.battery, !self.visible, morphing);
        let need = island && (self.breathe_on || !self.tint.settled());
        if need && !self.timer_on {
            unsafe {
                let _ = SetTimer(Some(self.hwnd), TIMER_BREATHE, island::BREATHE_TICK.as_millis() as u32, None);
            }
            self.timer_on = true;
            self.last_tick = Instant::now();
        } else if !need && self.timer_on {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_BREATHE);
            }
            self.timer_on = false;
        }
    }

    fn on_timer(&mut self) {
        let now = Instant::now();
        let dt = (now - self.last_tick).as_secs_f64().clamp(0.0, 0.2);
        self.last_tick = now;
        self.tint.step(dt);
        // 形变期间呼吸暂停，也不许 repaint（纯形状帧由 morph 独占）。
        if self.current_mode() != mode::UiMode::Island || !self.visible || self.morph.is_some() {
            return;
        }
        self.repaint();
        let r = *PRESENTED.lock().unwrap_or_else(|e| e.into_inner());
        self.present_rect(r);
        if !self.breathe_on && self.tint.settled() {
            self.sync_timer();
        }
    }

    /// 岛态 hover：144×34 → 162×42 弹簧放大（窗口矩形走 ULW，不跳变）。
    fn set_hot(&mut self, hot: bool) {
        if self.layout.hot == hot {
            return;
        }
        self.layout.hot = hot;
        HOT.store(hot as u8, Ordering::Relaxed);
        if !self.visible || self.effective_mode() != mode::UiMode::Island {
            return;
        }
        let Some(target) = self.layout.target(mode::UiMode::Island) else {
            return;
        };
        // 形变途中：只把落点改成新的胶囊尺寸，弹簧继续朝它跑（速度连续）。
        // 不重开 morph 时间轴，也不动内容——中间帧仍然只有形状。
        if let Some(m) = &mut self.morph {
            m.to = target;
            anim::animate_to(target, mode::HOVER_MOTION);
            return;
        }
        self.ensure_ui(target);
        self.repaint();
        let r = *PRESENTED.lock().unwrap_or_else(|e| e.into_inner());
        self.present_rect(r);
        anim::animate_to(target, mode::HOVER_MOTION);
    }

    /// 展开态 hover：启动 cubic-bezier(.6,0,.25,1) 时间轴；岛态仍走弹簧放大。
    fn set_hover(&mut self, on: bool) {
        if self.hover == on {
            return;
        }
        self.hover = on;
        if self.effective_mode() == mode::UiMode::Island {
            self.set_hot(on);
            return;
        }
        if self.effective_mode() != mode::UiMode::Expanded || !self.visible {
            return;
        }
        let now = self.hover_t0.elapsed().as_secs_f64() * 1000.0;
        self.hover_anim.begin(now, on);
        self.repaint();
        let r = *PRESENTED.lock().unwrap_or_else(|e| e.into_inner());
        self.present_rect(r);
        self.sync_hover_timer();
    }

    /// 16 ms 定时器只在展开态动画未走完时运行（空闲展开态不产生任何出帧）。
    fn sync_hover_timer(&mut self) {
        let need = self.hover_anim.is_active()
            && self.current_mode() == mode::UiMode::Expanded
            && self.visible;
        if need && !self.hover_timer_on {
            unsafe {
                let _ = SetTimer(Some(self.hwnd), TIMER_HOVER, 16, None);
            }
            self.hover_timer_on = true;
        } else if !need && self.hover_timer_on {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_HOVER);
            }
            self.hover_timer_on = false;
        }
    }

    fn on_hover_timer(&mut self) {
        let now = self.hover_t0.elapsed().as_secs_f64() * 1000.0;
        if self.hover_anim.done(now) {
            self.hover_anim.finish();
        }
        if self.visible && self.morph.is_none() {
            self.repaint();
            let r = *PRESENTED.lock().unwrap_or_else(|e| e.into_inner());
            self.present_rect(r);
        }
        self.sync_hover_timer();
    }

    fn refresh(&mut self) {
        let mut last = None;
        while let Ok(s) = self.rx.try_recv() {
            last = Some(s);
        }
        if let Some(s) = last {
            self.apply(&s);
        }
        let payload = self.probe.lock().ok().and_then(|g| g.clone());
        let status = proxy_status(payload.as_ref());
        let (foot, ftone) = card::proxy_foot(status);
        let (detail, dtone) = card::proxy_detail(status);
        self.foot_proxy = foot;
        self.proxy_tone = ftone;
        self.detail_proxy = detail;
        self.detail_tone = dtone;
        self.latency = payload
            .as_ref()
            .and_then(|p| if p.direct.ok { p.direct.ms } else { None });

        // 呼吸灯三态色 + DC 条件；颜色变化走 0.4 s 插值。
        let mut dot_changed = false;
        if let Some(p) = payload {
            let color = self
                .dot_override
                .unwrap_or_else(|| island::dot_color(&p.proxy.accelerating));
            if color != self.dot_target {
                self.dot_target = color;
                self.tint.set(color);
                dot_changed = true;
            }
            if p.battery != self.battery {
                self.battery = p.battery;
                dot_changed = true;
            }
        }
        self.sync_timer();

        // 隐藏时不重绘/不 present：卡片内容看不见，1 Hz 全量重绘纯属浪费。
        if !self.visible {
            return;
        }
        // 形变期间由 morph 时间轴独占呈现：这里只更新数据（采样已写进 hist /
        // 弹簧目标），绝不能 repaint —— 否则旧形态画布会盖掉纯形状帧。
        if self.morph.is_some() {
            return;
        }
        let mut need = false;
        let key = self.content_key();
        if key != self.last_key {
            self.last_key = key;
            need = true;
        }
        if dot_changed && self.current_mode() == mode::UiMode::Island {
            need = true;
        }
        // 1 Hz ticks with nothing new on screen (no traffic at all) would only
        // burn a composite; skip them.
        if need {
            self.repaint();
            let r = *PRESENTED.lock().unwrap_or_else(|e| e.into_inner());
            self.present_rect(r);
        }
    }

    fn content_key(&self) -> String {
        use std::fmt::Write as _;
        // 胶囊只显示下行/上行；标题/曲线/代理都不可见，别为看不见的内容重绘。
        if self.current_mode() == mode::UiMode::Island {
            return format!(
                "i|{} {}|{} {}",
                self.down.0, self.down.1, self.up.0, self.up.1
            );
        }
        let mut k = String::new();
        let _ = write!(
            k,
            "{}|{}|{} {}|{} {}|{}|{}|{}|{:?}|{}|{:?}|{:?}|{:?}|{}",
            self.title,
            self.state,
            self.down.0,
            self.down.1,
            self.up.0,
            self.up.1,
            self.wifi,
            self.wifi_ok,
            self.foot_proxy,
            self.proxy_tone,
            self.detail_proxy,
            self.detail_tone,
            self.latency,
            self.link_mbps,
            self.hover
        );
        for v in self.hist.iter() {
            let _ = write!(k, ",{v:.0}");
        }
        k.push('|');
        for v in self.up_hist.iter() {
            let _ = write!(k, ",{v:.0}");
        }
        k
    }

    fn apply(&mut self, s: &netmon::Sample) {
        let primary = s.ifaces.iter().find(|i| Some(i.id) == s.primary_id);
        self.title = match primary {
            Some(i) => i.alias.clone(),
            None => "无网络".to_string(),
        };
        self.state = if primary.is_some() { "已连接" } else { "无网络" }.to_string();
        self.link_mbps = primary.and_then(|i| i.link_mbps);
        match s.ifaces.iter().find(|i| i.kind == "wifi") {
            None => {
                self.wifi = "无".to_string();
                self.wifi_ok = false;
            }
            Some(i) if i.state == "up" => {
                self.wifi = "已连接".to_string();
                self.wifi_ok = true;
            }
            Some(_) => {
                self.wifi = "未连接".to_string();
                self.wifi_ok = false;
            }
        }
        self.hist.rotate_left(1);
        self.hist[HIST - 1] = s.total_down;
        self.up_hist.rotate_left(1);
        self.up_hist[HIST - 1] = s.total_up;
        // 数字弹簧（P3-7）：胶囊态直接改文字，展开态 1% 阈值起摆。
        self.rate_down = s.total_down;
        self.rate_up = s.total_up;
        let now = self.num_t0.elapsed().as_secs_f64() * 1000.0;
        let island = self.current_mode() == mode::UiMode::Island;
        if self.numbers.on_sample(s.total_down, s.total_up, island, now) {
            self.sync_num_timer();
        } else if !self.numbers.running() {
            self.apply_number_values();
        }
    }

    /// 把弹簧当前值写进显示字符串（`Math.max(0, x)` 已在 values() 里钳过）。
    fn apply_number_values(&mut self) {
        let (d, u) = self.numbers.values();
        self.down = fmt_hysteresis(d, &mut self.unit_mb);
        self.up = card::format_rate(u);
    }

    /// 16 ms 数字定时器：只在弹簧跑动时运行（30 fps 渲染闸在 number.rs）。
    fn sync_num_timer(&mut self) {
        let need = self.numbers.running() && self.visible;
        if need && !self.number_timer_on {
            self.num_last = Instant::now();
            unsafe {
                let _ = SetTimer(Some(self.hwnd), TIMER_NUM, 16, None);
            }
            self.number_timer_on = true;
        } else if !need && self.number_timer_on {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_NUM);
            }
            self.number_timer_on = false;
        }
    }

    fn on_num_timer(&mut self) {
        let now_instant = Instant::now();
        let dt = (now_instant - self.num_last).as_secs_f64().clamp(0.0, number::MAX_DT);
        self.num_last = now_instant;
        let now = self.num_t0.elapsed().as_secs_f64() * 1000.0;
        let tick = self.numbers.tick(now, dt);
        if tick.render {
            self.apply_number_values();
            if self.visible && self.morph.is_none() {
                self.repaint();
                let r = *PRESENTED.lock().unwrap_or_else(|e| e.into_inner());
                self.present_rect(r);
            }
        }
        if tick.finished {
            self.apply_number_values();
            self.sync_num_timer();
        }
    }

    // ---- T6 系统集成 ----------------------------------------------------

    /// 托盘菜单选中一项后的动作（菜单已经关掉，可以安全持 APP 借用）。
    fn on_menu_action(&mut self, action: menu::MenuAction) {
        match action {
            menu::MenuAction::ToggleVisible => self.toggle_visible(),
            menu::MenuAction::ToggleLock => self.toggle_lock(),
            menu::MenuAction::ToggleAutostart => self.toggle_autostart(),
            menu::MenuAction::Quit => unsafe {
                let _ = DestroyWindow(self.hwnd);
            },
        }
    }

    /// 第二实例唤起：显示自己并按当前形态重新落位（不改变三态决策）。
    fn wake_from_second_instance(&mut self) {
        log_line("second instance: wake");
        MANUAL_HIDDEN.store(0, Ordering::Relaxed);
        let m = if self.visible {
            self.effective_mode()
        } else {
            self.last_shown
        };
        if !self.visible {
            MODE.store(mode::mode_code(m), Ordering::Relaxed);
            self.apply_mode(m);
        }
        if let Some(target) = self.layout.target(m) {
            // 只做置顶与非激活显示：用户按了第二次，应当看到窗口。
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOWNA);
                let _ = SetWindowPos(
                    self.hwnd,
                    Some(HWND_TOPMOST),
                    target.x,
                    target.y,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
        }
    }

    /// 托盘「显示 / 隐藏」（也用于左键单击）：与现版 `toggle_visible` 同语义。
    fn toggle_visible(&mut self) {
        if self.visible {
            MANUAL_HIDDEN.store(1, Ordering::Relaxed);
            if self.current_mode() != mode::UiMode::Hidden {
                MODE.store(mode::mode_code(mode::UiMode::Hidden), Ordering::Relaxed);
                self.apply_mode(mode::UiMode::Hidden);
            }
        } else {
            MANUAL_HIDDEN.store(0, Ordering::Relaxed);
            // 还原到"最后显示过的形态"（现版 LAST_SHOWN 的语义）；
            // 从没显示过（--mode=hidden 冷启动）就按展开态出来。
            let m = match self.last_shown {
                mode::UiMode::Hidden => mode::UiMode::Expanded,
                m => m,
            };
            MODE.store(mode::mode_code(m), Ordering::Relaxed);
            self.apply_mode(m);
        }
    }

    /// 托盘「锁定 / 解锁」与快捷键共用：切换 `WS_EX_TRANSPARENT` 并持久化。
    fn toggle_lock(&mut self) {
        let on = self.locked.toggle();
        self.apply_lock(on);
        self.save_config();
        log_line(&format!("lock={on}"));
        if self.visible {
            self.repaint();
            let r = *PRESENTED.lock().unwrap_or_else(|e| e.into_inner());
            self.present_rect(r);
        }
    }

    fn apply_lock(&mut self, on: bool) {
        // 锁定 = 命中测试跳过本窗口（对照现版 set_ignore_cursor_events(true)）。
        // WS_EX_TRANSPARENT 是系统级"这里没有窗口"，比在 wndproc 里吞消息彻底：
        // 吞消息挡不住拖动/悬停对光标的抢占。代价是锁定期间右键也穿透，
        // 解锁只能走托盘菜单或快捷键——与现版一致（现版锁定后同样只能托盘解锁）。
        unsafe {
            let ex = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE);
            let flag = WS_EX_TRANSPARENT.0 as isize;
            let next = if on { ex | flag } else { ex & !flag };
            if next != ex {
                SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, next);
                // 改 exstyle 后必须 SWP_FRAMECHANGED 才生效（同一位置/尺寸）。
                let _ = SetWindowPos(
                    self.hwnd,
                    None,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
                );
            }
        }
        if on {
            self.hover = false;
            self.hover_anim.reset();
            self.set_hot(false);
            self.drag = None;
            self.press = None;
        }
    }

    /// 托盘「开机自启」：写/删 HKCU Run 并同步勾选态与 config.json。
    fn toggle_autostart(&mut self) {
        let next = !self.autostart_on;
        let ok = autostart::set_enabled(next);
        if ok {
            self.autostart_on = next;
            log_line(&format!("autostart={next}"));
        } else {
            log_line(&format!("autostart={next} FAILED"));
        }
        self.save_config_autostart(self.autostart_on);
    }

    /// 托盘菜单内容（结构与现版一致，见 `menu.rs`）。
    fn menu_items(&self) -> Vec<menu::Item> {
        menu::items(self.hotkey, self.autostart_on)
    }

    /// 把当前状态写回 config.json（与 WebView 版字段/排版互通）。
    ///
    /// `autostart` 与现版一致：只有托盘菜单点过才改写，其余时候保留文件里的
    /// 原值——否则用户手工编辑的 `autostart: false` 会被注册表状态覆盖。
    fn save_config(&mut self) {
        let old = config::load();
        let pos = self.expanded_home();
        let cfg = config::Config {
            x: pos.map(|p| p.0).or(old.x),
            y: pos.map(|p| p.1).or(old.y),
            locked: Some(self.locked.is_on()),
            autostart: old.autostart,
        };
        config::save(&cfg);
    }

    /// 托盘点了「开机自启」：把新状态显式写进 config.json。
    fn save_config_autostart(&mut self, on: bool) {
        let old = config::load();
        let pos = self.expanded_home();
        let cfg = config::Config {
            x: pos.map(|p| p.0).or(old.x),
            y: pos.map(|p| p.1).or(old.y),
            locked: Some(self.locked.is_on()),
            autostart: Some(on),
        };
        config::save(&cfg);
    }

    /// 只在"展开且落定"时把当前矩形当作桌面锚点（对照 remember_expanded_pos）。
    fn expanded_home(&self) -> Option<(i32, i32)> {
        if !self.visible || self.current_mode() != mode::UiMode::Expanded || self.morph.is_some() {
            return None;
        }
        let r = *PRESENTED.lock().unwrap_or_else(|e| e.into_inner());
        let want = self.layout.card();
        let settled = (r.w - want.w).abs() <= 2 && (r.h - want.h).abs() <= 2;
        settled.then_some((r.x, r.y))
    }

    /// WM_DPICHANGED（P4-5）：换屏后 DPI 变了 → 作废屏幕 DC 缓存、按新缩放
    /// 重算所有目标尺寸与字号、重建画布。
    ///
    /// T3 复核留的联动验证项就在 `window::reset_screen_dc()`：缓存的屏幕 DC
    /// 属于取它时的显示器，跨屏后继续用它会让 ULW 落位偏到旧屏坐标。
    fn on_dpi_changed(&mut self, new_dpi: u32) {
        let scale = (new_dpi as f32 / 96.0).max(0.5);
        if (scale - self.scale).abs() < 0.001 {
            return;
        }
        window::reset_screen_dc();
        let screen = screen_rect();
        // 锚点按新缩放重新钳进屏幕，别把卡片留在屏外。
        let card_w = (mode::CARD_W * scale as f64).round() as i32;
        let card_h = (mode::CARD_H * scale as f64).round() as i32;
        let home = (
            self.layout
                .home
                .0
                .clamp(screen.x, (screen.x + screen.w - card_w).max(screen.x)),
            self.layout
                .home
                .1
                .clamp(screen.y, (screen.y + screen.h - card_h).max(screen.y)),
        );
        self.scale = scale;
        self.layout = mode::Layout {
            scale,
            screen,
            home,
            hot: false,
            anchor: self.layout.anchor,
            island_pos: self.layout.island_pos,
        };
        // 画布与字号都按 scale 生成，必须整块重建（Renderer 里的 DWrite
        // format 尺寸在构造时定死，改不了）。
        let mode_now = self.effective_mode();
        let target = self.layout.target(mode_now).unwrap_or(self.layout.card());
        self.stop_morph();
        self.shapes = None;
        self.ensure_ui(target);
        if self.visible {
            self.repaint();
            self.present_rect(target);
        }
        anim::snap_to(target);
        if let Ok(mut g) = PRESENTED.lock() {
            *g = target;
        }
        log_line(&format!(
            "dpi {new_dpi} scale {scale} home={home:?} screen={}x{}",
            screen.w, screen.h
        ));
        // 形态线程有自己的一份 Layout：把新缩放/屏幕/锚点交给它。
        if let Ok(mut g) = DPI_UPDATE.lock() {
            *g = Some(DpiUpdate {
                scale,
                screen,
                home,
            });
        }
    }
}

/// 把 probe 载荷归一化成卡片要显示的代理状态（纯映射，文案在 card.rs）。
fn proxy_status(p: Option<&probe::ProbePayload>) -> card::ProxyStatus<'_> {
    let Some(p) = p else {
        return card::ProxyStatus::Off;
    };
    if let Some(a) = p.proxy.accelerating.first() {
        return card::ProxyStatus::Accelerating(a);
    }
    if let Some(via) = &p.via {
        if via.ok {
            if let Some(ms) = via.ms {
                return card::ProxyStatus::Via(ms);
            }
        }
    }
    if p.direct.error.as_deref() == Some("paused") {
        return card::ProxyStatus::Paused;
    }
    if p.proxy.port.is_some() {
        return card::ProxyStatus::Blocked;
    }
    if let Some(a) = p.proxy.accelerators.first() {
        return card::ProxyStatus::NotAccelerating(a);
    }
    card::ProxyStatus::Off
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    /// 托盘句柄单独放：`TrackPopupMenu` 会开**嵌套消息循环**，期间 WM_TIMER
    /// 会再次借用 `APP`；如果菜单回调还在 `APP` 借用里就会 `RefCell already
    /// borrowed` panic（实测过）。所以托盘不放进 App。
    static TRAY: RefCell<Option<tray::Tray>> = const { RefCell::new(None) };
    /// 单实例唤醒消息 id（`RegisterWindowMessageW` 返回值，0 = 注册失败）。
    static WAKE_MSG: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // 单实例：第二实例通过 RegisterWindowMessageW 投递唤醒消息（P4-4）。
    if msg == WAKE_MSG.with(|m| m.get()) && msg != 0 {
        APP.with(|a| {
            if let Some(app) = a.borrow_mut().as_mut() {
                app.wake_from_second_instance();
            }
        });
        return LRESULT(0);
    }
    match msg {
        tray::WM_TRAYICON => {
            let mouse = tray::mouse_message(wp.0);
            let pt = tray::point_from_lparam(lp.0);
            // 左键抬起 = 显/隐（对照现版 Click(Up) → toggle_visible）。这条路径
            // 不进嵌套消息循环，可以持借用。
            if tray::is_left_up(mouse) {
                APP.with(|a| {
                    if let Some(app) = a.borrow_mut().as_mut() {
                        app.toggle_visible();
                    }
                });
                return LRESULT(0);
            }
            if !tray::is_right_up(mouse) {
                return LRESULT(0);
            }
            // 右键抬起 = 弹菜单。**先取菜单内容并释放 APP 借用**，再 TrackPopupMenu：
            // 它会开嵌套消息循环，嵌套里的 WM_TIMER/WM_MOUSEMOVE 会再次借用 APP，
            // 持借用调用必 panic（实测 `RefCell already borrowed`）。
            let items = APP.with(|a| a.borrow().as_ref().map(|app| app.menu_items()));
            let Some(items) = items else {
                return LRESULT(0);
            };
            let action = TRAY.with(|t| {
                t.borrow()
                    .as_ref()
                    .and_then(|tray| tray.show_menu(&items, pt))
            });
            if let Some(action) = action {
                APP.with(|a| {
                    if let Some(app) = a.borrow_mut().as_mut() {
                        app.on_menu_action(action);
                    }
                });
            }
            LRESULT(0)
        }
        WM_HOTKEY => {
            APP.with(|a| {
                if let Some(app) = a.borrow_mut().as_mut() {
                    app.toggle_lock();
                }
            });
            LRESULT(0)
        }
        WM_DPICHANGED => {
            // wparam 低字是新 DPI；lparam 是系统建议的窗口矩形（我们不用它：
            // 窗口矩形由 ULW 的 pptDst/psize 决定，不是 SetWindowPos）。
            let dpi = (wp.0 & 0xFFFF) as u32;
            APP.with(|a| {
                if let Some(app) = a.borrow_mut().as_mut() {
                    app.on_dpi_changed(dpi);
                }
            });
            LRESULT(0)
        }
        window::WM_APP_DPI => {
            // 轮询兜底路径（ULW 落位不触发 WM_DPICHANGED 的场合）。
            let dpi = wp.0 as u32;
            APP.with(|a| {
                if let Some(app) = a.borrow_mut().as_mut() {
                    app.on_dpi_changed(dpi);
                }
            });
            LRESULT(0)
        }
        window::WM_APP_SAMPLE => {
            APP.with(|a| {
                if let Some(app) = a.borrow_mut().as_mut() {
                    app.refresh();
                }
            });
            LRESULT(0)
        }
        window::WM_APP_MODE => {
            let m = mode::mode_from_code(MODE.load(Ordering::Relaxed));
            APP.with(|a| {
                if let Some(app) = a.borrow_mut().as_mut() {
                    app.apply_mode(m);
                }
            });
            LRESULT(0)
        }
        window::WM_APP_GEOMETRY => {
            let r = PENDING.lock().ok().and_then(|mut g| g.take());
            if let Some(r) = r {
                APP.with(|a| {
                    if let Some(app) = a.borrow_mut().as_mut() {
                        app.on_geometry(r);
                    }
                });
            }
            LRESULT(0)
        }
        WM_TIMER => {
            if wp.0 == TIMER_BREATHE {
                APP.with(|a| {
                    if let Some(app) = a.borrow_mut().as_mut() {
                        app.on_timer();
                    }
                });
            } else if wp.0 == TIMER_HOVER {
                APP.with(|a| {
                    if let Some(app) = a.borrow_mut().as_mut() {
                        app.on_hover_timer();
                    }
                });
            } else if wp.0 == TIMER_NUM {
                APP.with(|a| {
                    if let Some(app) = a.borrow_mut().as_mut() {
                        app.on_num_timer();
                    }
                });
            } else if wp.0 == TIMER_FADE {
                APP.with(|a| {
                    if let Some(app) = a.borrow_mut().as_mut() {
                        app.on_fade_timer();
                    }
                });
            } else if wp.0 == TIMER_MORPH {
                APP.with(|a| {
                    if let Some(app) = a.borrow_mut().as_mut() {
                        app.on_morph_timer();
                    }
                });
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            APP.with(|a| {
                if let Some(app) = a.borrow_mut().as_mut() {
                    if let Some((start, wx, wy)) = app.drag {
                        let c = window::cursor_pos();
                        // 自绘拖动：按下即跟随（不用 HTCAPTION，双击不会最大化）
                        if input::is_drag((start.x, start.y), (c.x, c.y)) {
                            app.press_moved = true;
                        }
                        let r = anim::Rect::new(wx + (c.x - start.x), wy + (c.y - start.y), app.ui.w, app.ui.h);
                        app.present_rect(r);
                    } else if !app.hover {
                        let mut tme = TRACKMOUSEEVENT {
                            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE,
                            hwndTrack: hwnd,
                            dwHoverTime: 0,
                        };
                        let _ = TrackMouseEvent(&mut tme);
                        app.set_hover(true);
                    }
                }
            });
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            APP.with(|a| {
                if let Some(app) = a.borrow_mut().as_mut() {
                    if app.hover {
                        app.set_hover(false);
                    }
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            APP.with(|a| {
                if let Some(app) = a.borrow_mut().as_mut() {
                    let c = window::cursor_pos();
                    let cur = *PRESENTED.lock().unwrap_or_else(|e| e.into_inner());
                    app.drag = Some((c, cur.x, cur.y));
                    app.press = Some((c, cur.x, cur.y));
                    app.press_moved = false;
                    SetCapture(hwnd);
                    if app.current_mode() == mode::UiMode::Island && !app.active {
                        app.active = true;
                        app.repaint();
                        let r = *PRESENTED.lock().unwrap_or_else(|e| e.into_inner());
                        app.present_rect(r);
                    }
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            APP.with(|a| {
                if let Some(app) = a.borrow_mut().as_mut() {
                    app.drag = None;
                    if app.active {
                        app.active = false;
                        app.repaint();
                        let r = *PRESENTED.lock().unwrap_or_else(|e| e.into_inner());
                        app.present_rect(r);
                    }
                    // 拖动落点成为新的桌面锚点（展开态才有意义）；
                    // 没动过就还是点击，别把"点一下"记成新位置。
                    if app.press_moved && app.current_mode() == mode::UiMode::Expanded {
                        let cur = *PRESENTED.lock().unwrap_or_else(|e| e.into_inner());
                        app.layout.home = (cur.x, cur.y);
                    }
                    if !app.press_moved {
                        app.on_click();
                    }
                }
            });
            let _ = ReleaseCapture();
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            // 右键不再退出（T7 review：原型遗留，常驻应用误触即退；托盘「退出」
            // 与 WM_CLOSE 才是正式出口）。右键菜单未要求，吞掉即可。
            LRESULT(0)
        }
        WM_CLOSE => {
            // 托盘「退出」与外部 CloseMainWindow 都走这里：先落盘再销毁。
            APP.with(|a| {
                if let Some(app) = a.borrow_mut().as_mut() {
                    app.save_config();
                }
            });
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            // 退出前把状态落盘（位置/锁定/自启）并注销快捷键。
            APP.with(|a| {
                if let Some(app) = a.borrow_mut().as_mut() {
                    app.save_config();
                }
            });
            unsafe {
                let _ = UnregisterHotKey(Some(hwnd), HOTKEY_ID);
            }
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe extern "system" fn bench_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    DefWindowProcW(hwnd, msg, wp, lp)
}

fn screen_rect() -> anim::Rect {
    unsafe {
        anim::Rect::new(
            0,
            0,
            GetSystemMetrics(SM_CXSCREEN),
            GetSystemMetrics(SM_CYSCREEN),
        )
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn fg_class(h: HWND) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(h, &mut buf) }.max(0) as usize;
    String::from_utf16_lossy(&buf[..n])
}

fn fg_is_fullscreen(fg: HWND) -> bool {
    use windows::Win32::Foundation::RECT;
    unsafe {
        let style = GetWindowLongW(fg, GWL_STYLE) as u32;
        if style & WS_MAXIMIZE.0 != 0 {
            return false; // 最大化不是全屏
        }
        if style & WS_CAPTION.0 != 0 {
            return false;
        }
        let mut r = RECT::default();
        if GetWindowRect(fg, &mut r).is_err() {
            return false;
        }
        let sw = GetSystemMetrics(SM_CXSCREEN);
        let sh = GetSystemMetrics(SM_CYSCREEN);
        (r.right - r.left) >= sw && (r.bottom - r.top) >= sh
    }
}

/// 前台窗口矩形（物理像素）。最小化窗口的坐标是 -32000，阴影/辅助窗口
/// 可能整块离屏或只有几十像素宽——这些都不能代表用户正在看的界面，返回 None。
fn fg_window_rect(fg: HWND) -> Option<anim::Rect> {
    use windows::Win32::Foundation::RECT;
    unsafe {
        if IsIconic(fg).as_bool() {
            return None;
        }
        let mut r = RECT::default();
        GetWindowRect(fg, &mut r).ok()?;
        let w = r.right - r.left;
        let h = r.bottom - r.top;
        if w < 200 || h < 120 {
            return None;
        }
        let screen = screen_rect();
        let on_screen = r.right > screen.x
            && r.left < screen.x + screen.w
            && r.bottom > screen.y
            && r.top < screen.y + screen.h;
        if !on_screen {
            return None;
        }
        Some(anim::Rect::new(r.left, r.top, w, h))
    }
}

/// boot 诊断同时进 stderr 与日志（GUI 进程 stderr 无处可看）。
fn log_boot(s: &str) {
    eprintln!("{s}");
    log_line(s);
}

fn log_line(s: &str) {
    use std::io::Write;
    let p = std::env::temp_dir().join("netglass-log.txt");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(p)
    {
        // 常驻进程无界追加会慢慢长大；超 256KB 清空重来
        if f.metadata().map(|m| m.len() > 256 * 1024).unwrap_or(false) {
            let _ = f.set_len(0);
        }
        let _ = writeln!(f, "{s}");
    }
}

/// 诊断用形态钳制（`--mode=island|expanded|hidden`）：测量/截图时保持单一形态，
/// 不影响正常运行路径（不传就是 auto）。
#[derive(Clone, Copy, PartialEq, Debug)]
enum ModeOverride {
    Island,
    Expanded,
    Hidden,
}

fn parse_mode_override(args: &[String]) -> Option<ModeOverride> {
    let mut i = 0;
    while i < args.len() {
        let value = if let Some(v) = args[i].strip_prefix("--mode=") {
            Some(v.to_string())
        } else if args[i] == "--mode" && i + 1 < args.len() {
            Some(args[i + 1].clone())
        } else {
            None
        };
        if let Some(v) = value {
            return match v.as_str() {
                "island" => Some(ModeOverride::Island),
                "expanded" => Some(ModeOverride::Expanded),
                "hidden" => Some(ModeOverride::Hidden),
                _ => None,
            };
        }
        i += 1;
    }
    None
}

fn parse_dot_override(args: &[String]) -> Option<island::DotColor> {
    let mut i = 0;
    while i < args.len() {
        let value = if let Some(v) = args[i].strip_prefix("--dot=") {
            Some(v.to_string())
        } else if args[i] == "--dot" && i + 1 < args.len() {
            Some(args[i + 1].clone())
        } else {
            None
        };
        if let Some(v) = value {
            return match v.as_str() {
                "green" => Some(island::DotColor::Green),
                "orange" => Some(island::DotColor::Orange),
                "red" => Some(island::DotColor::Red),
                _ => None,
            };
        }
        i += 1;
    }
    None
}

/// `--morph-frame MS`：定格形变中间帧（诊断/取证）。
fn parse_morph_frame(args: &[String]) -> Option<f64> {
    let mut i = 0;
    while i < args.len() {
        let v = args[i]
            .strip_prefix("--morph-frame=")
            .map(|s| s.to_string())
            .or_else(|| {
                if args[i] == "--morph-frame" && i + 1 < args.len() {
                    Some(args[i + 1].clone())
                } else {
                    None
                }
            });
        if let Some(v) = v {
            return v.trim().parse().ok();
        }
        i += 1;
    }
    None
}

fn parse_flag_value(args: &[String], flag: &str) -> Option<String> {
    let mut i = 0;
    while i < args.len() {
        if let Some(v) = args[i].strip_prefix(&format!("{flag}=")) {
            return Some(v.to_string());
        }
        if args[i] == flag && i + 1 < args.len() {
            return Some(args[i + 1].clone());
        }
        i += 1;
    }
    None
}

/// 把弹簧在 `ms` 时刻的矩形算出来，用**真实形变路径**（ShapePainter）画一帧
/// 然后停住。取证截图与运行时同源，不是另写一套近似。
fn morph_frame(ms: f64, args: &[String]) -> Result<()> {
    let scale = window::dpi_scale();
    let screen = screen_rect();
    let home = match window::parse_points(args, "--pos").first() {
        Some(p) => *p,
        None => mode::default_home(screen, scale),
    };
    let layout = mode::Layout {
        scale,
        screen,
        home,
        hot: false,
        anchor: mode::IslandAnchor::Center,
        island_pos: window::parse_points(args, "--island-pos").first().copied(),
    };
    let island = layout.target(mode::UiMode::Island).unwrap();
    let card = layout.target(mode::UiMode::Expanded).unwrap();
    let dir = parse_flag_value(args, "--morph-dir").unwrap_or_else(|| "expand".to_string());
    let (from, to, target, from_mode) = if dir == "collapse" {
        (card, island, mode::UiMode::Island, mode::UiMode::Expanded)
    } else {
        (island, card, mode::UiMode::Expanded, mode::UiMode::Island)
    };
    let motion = mode::motion_for(target);
    let r = anim::rect_at(from, to, motion, ms);
    let radius = morph::morph_frame_radius(r, from.w, to.w, scale);

    let cfg = window::WindowConfig {
        x: r.x,
        y: r.y,
        w: r.w,
        h: r.h,
        visible: true,
    };
    let hwnd = window::create_window(&cfg, Some(bench_proc))?;
    // 退场窗口内窗口仍停在起点矩形、内容是旧形态淡出 → 用真实渲染器画旧形态内容；
    // delay 走完才是纯形状帧。两条路径与运行时逐字对应，截图不是另一套近似。
    let retreat = morph::retreat_ms(target);
    let (pr, content) = if ms < retreat {
        let alpha = morph::fade_out_alpha(ms, target);
        (from, format!("content-fade alpha={alpha:.2}"))
    } else {
        (r, "shape-only".to_string())
    };
    if ms < retreat {
        let surf = window::Surface::new(None, pr.w, pr.h)?;
        let renderer = render::Renderer::new(surf.mem_dc, pr.w, pr.h, scale)?;
        let mut f = demo_frame(vec![0.0; HIST]);
        f.content_alpha = morph::fade_out_alpha(ms, target);
        if from_mode == mode::UiMode::Island {
            f.title.clear();
            f.state.clear();
            f.wifi.clear();
            f.proxy.clear();
            renderer.paint_island(&f)?;
        } else {
            renderer.paint(&f)?;
        }
        window::present_dc(hwnd, surf.mem_dc, pr.x, pr.y, pr.w, pr.h, 255)?;
    } else {
        let painter = render::ShapePainter::new(pr.w, pr.h, scale)?;
        painter.paint(pr.w, pr.h, radius)?;
        window::present_dc(hwnd, painter.dc(), pr.x, pr.y, pr.w, pr.h, 255)?;
    }
    println!(
        "morph-frame dir={dir} t={ms:.0}ms rect=({}, {}, {}, {}) radius={radius:.1} {content}",
        pr.x, pr.y, pr.w, pr.h
    );
    // 停住供截图：默认一直显示（右键退出，与主窗口一致）；
    // `--exit-after MS` 到时自动销毁，截图脚本不必杀进程。
    let hold = parse_flag_value(args, "--exit-after").and_then(|v| v.parse::<u64>().ok());
    if let Some(hold) = hold {
        std::thread::sleep(Duration::from_millis(hold));
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
        return Ok(());
    }
    let mut msg = MSG::default();
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

fn run(args: &[String]) -> Result<()> {
    // P4-4 单实例：第二个进程不建窗，唤起既有窗后立刻退出。
    // 诊断参数（--bench/--selftest/…）在上面的 main 里已各自 return，不受影响。
    let single = instance::SingleInstance::acquire();
    if single.as_ref().map(|s| s.launch()) == Some(instance::Launch::Second) {
        let woken = instance::wake_existing().is_ok();
        eprintln!("[boot] second instance -> wake_existing={woken}, exiting");
        return Ok(());
    }

    let scale = window::dpi_scale();
    let screen = screen_rect();
    let silent = args.iter().any(|a| a == "--silent");
    let mode_override = parse_mode_override(args);
    let dot_override = parse_dot_override(args);
    let island_pos = window::parse_points(args, "--island-pos").first().copied();

    // P4-3：位置/锁定/自启跨重启保持。config.json 与 WebView 版同一文件。
    let stored = config::load();
    let want_locked = stored.locked.unwrap_or(false);
    let want_autostart = stored.autostart.unwrap_or(true);

    log_boot(&format!(
        "boot args={args:?} silent={silent} scale={scale} dpi={} screen={}x{} mode={mode_override:?} config={stored:?}",
        unsafe { GetDpiForSystem() },
        screen.w,
        screen.h
    ));

    // 位置：命令行 --pos 优先（诊断/截图用），否则用可信配置，否则默认锚点。
    let card_w = (mode::CARD_W * scale as f64).round() as i32;
    let card_h = (mode::CARD_H * scale as f64).round() as i32;
    let home = match window::parse_points(args, "--pos").first() {
        Some(p) => *p,
        None => config::usable_home(&stored, screen, card_w, card_h)
            .unwrap_or_else(|| mode::default_home(screen, scale)),
    };
    // 跨屏 DPI 变化时 `on_dpi_changed` 会重算 scale/screen（它是 App 字段，
    // 这里只留注释说明为什么启动时读一次就够）。
    let layout = mode::Layout {
        scale,
        screen,
        home,
        hot: false,
        anchor: mode::IslandAnchor::Center,
        island_pos,
    };
    let card = layout.card();

    let show_after = parse_flag_value(args, "--show-after").is_some();
    let start_visible =
        !silent && mode_override != Some(ModeOverride::Hidden) && !show_after;
    // 静默启动从胶囊态起步：gate 必须先走满 2 s 桌面 settle 才会给出 Expanded，
    // 在此之前 show_on_decide 一律不显窗（P1-3）。非静默直接以展开态冷启动。
    let initial_mode = match mode_override {
        Some(ModeOverride::Island) => mode::UiMode::Island,
        Some(ModeOverride::Expanded) => mode::UiMode::Expanded,
        Some(ModeOverride::Hidden) => mode::UiMode::Hidden,
        None if silent => mode::UiMode::Island,
        None => mode::UiMode::Expanded,
    };
    let initial_rect = layout.target(initial_mode).unwrap_or(card);

    let cfg = window::WindowConfig {
        x: initial_rect.x,
        y: initial_rect.y,
        w: initial_rect.w,
        h: initial_rect.h,
        visible: start_visible,
    };
    let hwnd = window::create_window(&cfg, Some(wndproc))?;
    let ui = Ui::new(Some(hwnd), initial_rect.w, initial_rect.h, scale)?;
    let raw = hwnd.0 as isize;

    // 诊断/截图用：`--exit-after MS` 到点自动退出（不 kill 进程）。
    if let Some(ms) = parse_flag_value(args, "--exit-after").and_then(|v| v.parse::<u64>().ok()) {
        let raw = raw;
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(ms));
            window::post(raw, WM_CLOSE);
        });
    }
    if let Ok(mut g) = PRESENTED.lock() {
        *g = initial_rect;
    }

    let (tx, rx) = mpsc::channel::<netmon::Sample>();
    let probe_state = Arc::new(Mutex::new(None::<probe::ProbePayload>));

    // 1 Hz sampler: same logic as the Tauri app (GetIfEntry2 diff + default route).
    {
        let raw = raw;
        std::thread::spawn(move || {
            let mut sampler = netmon::Sampler::new();
            loop {
                let s = sampler.sample(true);
                if tx.send(s).is_err() {
                    break;
                }
                window::post_sample(raw);
                std::thread::sleep(Duration::from_millis(1000));
            }
        });
    }

    // 探针线程（移植自 src-tauri）：本地状态 2 s 廉价路径（监听表 PID + hosts mtime
    // 门控），外网时延 10 s 一轮，电池下不发外网探测，窗口隐藏时完全停手。
    {
        let state = probe_state.clone();
        let raw = raw;
        std::thread::spawn(move || {
            let mut cached: Option<probe::ProbePayload> = None;
            let mut last_local: Option<Instant> = None;
            let mut last_external: Option<Instant> = None;
            let mut last_port: Option<u16> = None;
            std::thread::sleep(Duration::from_secs(3));
            loop {
                let visible = VISIBLE_FLAG.load(Ordering::Relaxed) != 0;
                let local_due = last_local
                    .map(|t| t.elapsed() >= Duration::from_secs(2))
                    .unwrap_or(true);
                if visible && local_due {
                    let battery = probe::on_battery();
                    let external_due = !battery
                        && last_external
                            .map(|t| t.elapsed() >= Duration::from_secs(10))
                            .unwrap_or(true);
                    let p = if external_due {
                        let p = probe::run_probe();
                        cached = Some(p.clone());
                        last_external = Some(Instant::now());
                        p
                    } else {
                        let mut p = match &cached {
                            Some(c) => {
                                let mut p2 = c.clone();
                                p2.proxy = probe::proxy_state();
                                p2
                            }
                            None => probe::offline_payload(),
                        };
                        if p.proxy.port != last_port {
                            p.via = p.proxy.port.map(|port| {
                                probe::http_connect_probe(port, "github.com", 443, 5000)
                            });
                        }
                        p
                    };
                    let mut p = p;
                    p.battery = battery;
                    last_port = p.proxy.port;
                    if let Ok(mut g) = state.lock() {
                        *g = Some(p);
                    }
                    window::post_sample(raw);
                    last_local = Some(Instant::now());
                }
                std::thread::sleep(Duration::from_secs(if visible { 1 } else { 2 }));
            }
        });
    }

    // Desktop widgets sit in the topmost band, but other topmost windows (the
    // backdrop used in tests, installers, media players) can still land above
    // us. Re-assert WS_EX_TOPMOST every 2 s; it is cheap and never activates.
    {
        let raw = raw;
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(2));
            unsafe {
                let _ = SetWindowPos(
                    HWND(raw as *mut std::ffi::c_void),
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
        });
    }

    // P4-5 DPI 轮询兜底：窗口位置由 ULW 的 pptDst 决定，跨屏拖动可能不发
    // WM_DPICHANGED。2 s 查一次 GetDpiForWindow，变了就交给 UI 线程重建。
    {
        let raw = raw;
        std::thread::spawn(move || {
            let mut last = unsafe { window::dpi_scale_for(HWND(raw as *mut std::ffi::c_void)) };
            loop {
                std::thread::sleep(Duration::from_secs(2));
                let now = unsafe { window::dpi_scale_for(HWND(raw as *mut std::ffi::c_void)) };
                if (now - last).abs() > 0.001 {
                    last = now;
                    window::post_dpi(raw, (now * 96.0).round() as u32);
                }
            }
        });
    }

    let mut app = App {
        ui,
        hwnd,
        scale,
        layout,
        applied: mode::mode_code(initial_mode),
        visible: start_visible,
        hist: [0.0; HIST],
        up_hist: [0.0; HIST],
        unit_mb: true,
        hover: false,
        active: false,
        drag: None,
        rx,
        probe: probe_state,
        battery: false,
        tint: island::TintTween::new(dot_override.unwrap_or(island::DotColor::Green)),
        dot_target: dot_override.unwrap_or(island::DotColor::Green),
        dot_override,
        breathe_on: false,
        timer_on: false,
        breathe_t0: Instant::now(),
        last_tick: Instant::now(),
        title: "正在读取".to_string(),
        state: "—".to_string(),
        wifi: "—".to_string(),
        wifi_ok: false,
        foot_proxy: "未启用".to_string(),
        proxy_tone: card::Tone::Warn,
        detail_proxy: "未启用".to_string(),
        detail_tone: card::Tone::Warn,
        latency: None,
        link_mbps: None,
        down: ("—".to_string(), "MB/s".to_string()),
        up: ("—".to_string(), "MB/s".to_string()),
        last_key: String::new(),
        hover_anim: fx::HoverAnim::new(),
        hover_timer_on: false,
        hover_t0: Instant::now(),
        numbers: number::NumberAnim::new(),
        number_timer_on: false,
        num_t0: Instant::now(),
        num_last: Instant::now(),
        rate_down: 0.0,
        rate_up: 0.0,
        morph: None,
        morph_timer_on: false,
        morph_alpha_step: u8::MAX,
        shapes: None,
        fade: None,
        fade_timer_on: false,
        last_click: None,
        last_click_mode: None,
        click_t0: Instant::now(),
        press: None,
        press_moved: false,
        locked: lock::Lock::new(want_locked),
        hotkey: None,
        autostart_on: false,
        last_shown: initial_mode,
    };
    app.repaint();
    app.present_rect(initial_rect);
    app.sync_timer();
    MODE.store(mode::mode_code(initial_mode), Ordering::Relaxed);
    APP.with(|a| *a.borrow_mut() = Some(app));
    VISIBLE_FLAG.store(start_visible as u8, Ordering::Relaxed);

    // 单实例唤醒消息：UI 线程收到后由 wndproc 转给 App（P4-4）。
    unsafe {
        let id = RegisterWindowMessageW(windows::core::PCWSTR(wide(instance::wake_message_name()).as_ptr()));
        WAKE_MSG.with(|m| m.set(id));
    }

    // P4-2 快捷键：候选序与现版一致，取第一个注册成功的。
    let hotkey = unsafe {
        hotkey::pick(|c| {
            RegisterHotKey(Some(hwnd), HOTKEY_ID, c.mods, c.vk).is_ok()
        })
    };
    log_boot(&format!("boot hotkey={:?}", hotkey.map(|h| h.label)));

    // P4-1 托盘：图标从内嵌 PNG 生成；失败也不影响主窗口（菜单可由快捷键兜底）。
    let tray_ok = match tray::Tray::add(hwnd) {
        Ok(t) => {
            TRAY.with(|slot| *slot.borrow_mut() = Some(t));
            true
        }
        Err(e) => {
            log_line(&format!("tray add failed: {e:?}"));
            false
        }
    };

    // P4-3 自启：首次运行默认开（对齐现版 want_autostart = cfg.autostart.unwrap_or(true)）。
    let autostart_cmd = autostart::ensure(want_autostart);
    let autostart_on = autostart::is_enabled();

    // 状态回填到 App（单实例/托盘/热键/自启都要在 UI 线程可见）。
    APP.with(|a| {
        if let Some(app) = a.borrow_mut().as_mut() {
            app.hotkey = hotkey;
            app.autostart_on = autostart_on;
            if want_locked {
                app.apply_lock(true);
            }
        }
    });
    log_boot(&format!(
        "boot tray={tray_ok} hotkey={:?} locked={want_locked} autostart={autostart_on} cmd={autostart_cmd:?}",
        hotkey.map(|h| h.label)
    ));
    // 首次把状态落盘：即便用户没做任何操作，config.json 也会带上当前锁定/自启。
    APP.with(|a| {
        if let Some(app) = a.borrow_mut().as_mut() {
            app.save_config();
        }
    });

    // 形变线程：apply 把每帧矩形交给 UI 线程呈现；current 读窗口当前矩形。
    anim::start(
        anim::Target {
            apply: Box::new(move |r| {
                if let Ok(mut g) = PENDING.lock() {
                    *g = Some(r);
                }
                window::post(raw, window::WM_APP_GEOMETRY);
            }),
            current: Box::new(|| PRESENTED.lock().ok().map(|g| *g)),
        },
        initial_rect,
    );

    // 诊断：`--show-after MS` 起手隐藏，到点后走真实 apply_mode 显示初始形态
    // （验证"隐藏 -> 恢复首帧无空板"）。不改变默认路径。
    if let Some(ms) = parse_flag_value(args, "--show-after").and_then(|v| v.parse::<u64>().ok()) {
        let raw = raw;
        let m = initial_mode;
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(ms));
            MODE.store(mode::mode_code(m), Ordering::Relaxed);
            window::post(raw, window::WM_APP_MODE);
        });
    }

    // 诊断：`--dpi-test DPI` 起手 1.5 s 后投递一次 DPI 变化（走真实
    // on_dpi_changed 路径，单屏机器也能验证"重建 surface/字号 + 刷新屏幕 DC
    // 缓存"这一段）。参数是目标 DPI（96/144/192…），不是毫秒。
    if let Some(dpi) = parse_flag_value(args, "--dpi-test").and_then(|v| v.parse::<u32>().ok()) {
        let raw = raw;
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1500));
            window::post_dpi(raw, dpi);
        });
    }

    // 诊断：`--morph-demo` 走真实 apply_mode 路径交替两态（截图/几何采样用），
    // 不接形态线程也不改默认行为。
    if args.iter().any(|a| a == "--morph-demo") {
        let raw = raw;
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(900));
            let mut island = true;
            loop {
                std::thread::sleep(Duration::from_millis(2400));
                island = !island;
                let m = if island { mode::UiMode::Island } else { mode::UiMode::Expanded };
                MODE.store(mode::mode_code(m), Ordering::Relaxed);
                window::post(raw, window::WM_APP_MODE);
            }
        });
    }

    // 形态线程：260 ms 前台轮询 → ModeGate → 三态（spec D3 维持 260 ms）。
    if !args.iter().any(|a| a == "--morph-demo") {
        let raw = raw;
        let mut layout = layout;
        std::thread::spawn(move || {
            let warmup = Instant::now();
            let mut last_other = unsafe { GetForegroundWindow() };
            let mut gate = mode::ModeGate::new();
            // --mode=hidden 直接落到 D6 的托盘隐藏路径（测量隐藏态用）。
            if mode_override == Some(ModeOverride::Hidden) {
                gate.set_manual_hidden(true);
            }
            let mut silent_pending = silent && mode_override.is_none();
            let mut anchor = mode::IslandAnchor::Center;
            loop {
                std::thread::sleep(mode::POLL);
                if warmup.elapsed() < Duration::from_millis(1600) {
                    continue;
                }
                let mut fg = unsafe { GetForegroundWindow() };
                let mut class = fg_class(fg);

                // 卡片自己拿到焦点时（个别系统仍会），以它之前的前台窗口为准，
                // 否则会一直保持展开、压在你正在用的软件上。
                if fg.0 as isize == raw {
                    fg = last_other;
                    class = fg_class(fg);
                } else {
                    last_other = fg;
                }

                let kind = mode::classify_fg(&class);
                let current = mode::mode_from_code(MODE.load(Ordering::Relaxed));
                // D6：托盘的"显示/隐藏"每拍同步给 gate——隐藏后全屏路过、
                // 退出全屏、桌面稳定都不得把它抢回来；只有托盘"显示"才清零。
                gate.set_manual_hidden(MANUAL_HIDDEN.load(Ordering::Relaxed) != 0);
                // 跨屏 DPI 变化：UI 线程算好的新布局在这里换掉自己的副本，
                // 否则本线程会按旧缩放重算胶囊/卡片目标，把窗口弹回旧尺寸。
                if let Some(u) = DPI_UPDATE.lock().ok().and_then(|mut g| g.take()) {
                    layout.scale = u.scale;
                    layout.screen = u.screen;
                    layout.home = u.home;
                    anchor = mode::IslandAnchor::Center;
                    layout.anchor = anchor;
                    // 本拍算出的 target 立刻按新缩放对齐：否则这一拍还会用
                    // 旧 layout 的几何发一帧，表现为跨屏瞬间窗口尺寸回弹。
                    if let Some(r) = layout.target(current) {
                        if current != mode::UiMode::Hidden {
                            anim::snap_to(r);
                        }
                    }
                }
                // hover 放大由 UI 线程写 HOT；躲让重算 target 时必须用同一尺寸，
                // 否则 hover 中遇到躲让会把胶囊缩回 144×34。
                layout.hot = HOT.load(Ordering::Relaxed) != 0;

                // 前台窗口压住顶部居中时躲到没被覆盖的一角；只是平移窗口，
                // 不换内容。任务栏/Alt+Tab 覆盖层保持当前挂载点，避免来回跳。
                // --island-pos 诊断把胶囊钉死后不再躲让（测量/截图不被前台变化污染）。
                let next_anchor = if layout.island_pos.is_some() {
                    anchor
                } else if kind == mode::FgKind::App {
                    match fg_window_rect(fg) {
                        Some(rect) => mode::pick_anchor(
                            Some(rect),
                            layout.island_center(),
                            layout.screen,
                        ),
                        // 阴影/离屏辅助窗口拿不到可信矩形：保持当前挂载点
                        None => anchor,
                    }
                } else if kind == mode::FgKind::Desktop {
                    mode::IslandAnchor::Center
                } else {
                    anchor
                };
                if next_anchor != anchor {
                    anchor = next_anchor;
                    layout.anchor = anchor;
                    ANCHOR.store(mode::anchor_code(anchor), Ordering::Relaxed);
                    log_line(&format!("anchor {:?} fg={class}", anchor));
                    if current == mode::UiMode::Island {
                        if let Some(r) = layout.target(current) {
                            anim::animate_to(r, mode::DODGE_MOTION);
                        }
                    }
                }

                let now = Instant::now();
                // 手动展开的 8 秒 pin：pin 期内保持展开，前台切到别的应用即失效。
                let pin = *PIN_UNTIL.lock().unwrap_or_else(|e| e.into_inner());
                let pinned = mode::pinned_here(
                    pin,
                    now,
                    PIN_FG.load(Ordering::Relaxed),
                    fg.0 as isize,
                    kind,
                );
                let mut want = gate.decide(
                    now,
                    mode::FgSnapshot {
                        kind,
                        fullscreen: kind == mode::FgKind::App && fg_is_fullscreen(fg),
                        // 双击收回后压过桌面 2 s settle（对照 MANUAL_ISLAND）。
                        manual_island: MANUAL_ISLAND.load(Ordering::Relaxed) != 0,
                        pinned_here: pinned,
                        already_expanded: current == mode::UiMode::Expanded,
                    },
                );
                if let Some(m) = mode_override {
                    want = match m {
                        ModeOverride::Island => mode::UiMode::Island,
                        ModeOverride::Expanded => mode::UiMode::Expanded,
                        ModeOverride::Hidden => mode::UiMode::Hidden,
                    };
                }
                if want == mode::UiMode::Expanded {
                    silent_pending = false;
                }
                if want != current {
                    log_line(&format!("mode {:?} fg={class}", want));
                    MODE.store(mode::mode_code(want), Ordering::Relaxed);
                    if mode::show_on_decide(want, silent_pending) {
                        window::post(raw, window::WM_APP_MODE);
                    }
                }
            }
        });
    }

    let mut msg = MSG::default();
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

/// 窗口是否可见（探针线程用；跨线程读，避免每次去查 Win32）。
static VISIBLE_FLAG: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

fn fmt_hysteresis(v: f64, mb: &mut bool) -> (String, String) {
    if !v.is_finite() || v < 1024.0 {
        return ("0.0".to_string(), "MB/s".to_string());
    }
    let m = v / 1048576.0;
    if *mb {
        if m < 0.95 {
            *mb = false;
        }
    } else if m > 1.05 {
        *mb = true;
    }
    if *mb {
        (format!("{m:.1}"), "MB/s".to_string())
    } else {
        (format!("{}", (v / 1024.0).round() as i64), "KB/s".to_string())
    }
}

/// `--fonts`：自证 DWrite 进程内 collection 真的包含 Inter 及五个字重。
fn fonts() -> Result<()> {
    use windows::Win32::Graphics::DirectWrite::{DWriteCreateFactory, DWRITE_FACTORY_TYPE_SHARED, IDWriteFactory};
    let dwrite: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
    match font::load_collection(&dwrite) {
        Some(c) => {
            font::dump(&c);
            println!("custom has Inter = {}", font::has_family(&c, "Inter"));
            println!("weights = {:?}", font::family_weights(&c, "Inter"));
        }
        None => println!("load_collection returned None (DWrite 3 unavailable?)"),
    }
    println!("system has Inter = {}", font::system_has_family(&dwrite, "Inter"));
    Ok(())
}

/// 诊断/基准用的样例帧（内容固定，方便和 T1 基线数字对比）。
fn demo_frame(hist: Vec<f64>) -> render::Frame {
    render::Frame {
        title: "以太网 2".to_string(),
        state: "已连接".to_string(),
        down_value: "2.9".to_string(),
        down_unit: "MB/s".to_string(),
        up_value: "148".to_string(),
        up_unit: "KB/s".to_string(),
        wifi: "已连接".to_string(),
        wifi_ok: true,
        proxy: "23 ms".to_string(),
        proxy_tone: card::Tone::Normal,
        details: card::detail_rows(
            Some(23.0),
            2.9 * 1048576.0,
            Some(1000.0),
            148.0 * 1024.0,
            ("23 ms", card::Tone::Normal),
        ),
        hist,
        hover: false,
        hover_fx: fx::HoverFx::IDLE,
        active: false,
        dot: render::DotPaint::default(),
        content_alpha: 1.0,
    }
}

/// Times the paint path (no compositor) and reports process memory.
fn bench() -> Result<()> {
    use windows::Win32::System::ProcessStatus::PROCESS_MEMORY_COUNTERS;
    use windows::Win32::System::ProcessStatus::GetProcessMemoryInfo;
    use windows::Win32::System::Threading::GetCurrentProcess;

    fn ws_mb() -> f64 {
        unsafe {
            let mut c = PROCESS_MEMORY_COUNTERS::default();
            if GetProcessMemoryInfo(
                GetCurrentProcess(),
                &mut c,
                std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            )
            .is_ok()
            {
                c.WorkingSetSize as f64 / 1048576.0
            } else {
                0.0
            }
        }
    }

    println!("startup WorkingSet = {:.1} MB", ws_mb());
    let scale = window::dpi_scale();
    let w = (LOGICAL_W * scale).round() as i32;
    let h = (LOGICAL_H * scale).round() as i32;
    let surface = window::Surface::new(None, w, h)?;
    println!("after DIB          = {:.1} MB", ws_mb());
    let renderer = render::Renderer::new(surface.mem_dc, w, h, scale)?;
    println!("after D2D/DWrite   = {:.1} MB", ws_mb());

    let mut hist = vec![0.0f64; HIST];
    for (i, v) in hist.iter_mut().enumerate() {
        *v = 4.0e5 + (i as f64 * 9.1e4);
    }
    let frame = demo_frame(hist);
    renderer.paint(&frame)?;
    println!(
        "font family        = {} (collection sees Inter = {}, system sees Inter = {}, weights = {:?})",
        renderer.font_family(),
        renderer.dwrite_has_family("Inter"),
        renderer.system_has_family("Inter"),
        renderer.family_weights("Inter")
    );
    println!("after 1st paint    = {:.1} MB", ws_mb());
    renderer.bench_parts(&frame)?;
    renderer.bench_parts(&frame)?;
    let t0 = std::time::Instant::now();
    for _ in 0..120 {
        renderer.paint(&frame)?;
    }
    let per = t0.elapsed().as_secs_f64() * 1000.0 / 120.0;
    println!("paint: {:.3} ms/frame ({} frames)", per, 120);
    println!("after 120 paints   = {:.1} MB", ws_mb());

    // 胶囊态 paint 成本（T3 呼吸灯 15 fps 的预算依据）。
    let iw = (mode::ISLAND_W * scale as f64).round() as i32;
    let ih = (mode::ISLAND_H * scale as f64).round() as i32;
    let island_surface = window::Surface::new(None, iw, ih)?;
    let island_renderer = render::Renderer::new(island_surface.mem_dc, iw, ih, scale)?;
    let mut island_frame = demo_frame(Vec::new());
    island_frame.title.clear();
    island_frame.state.clear();
    island_frame.wifi.clear();
    island_frame.proxy.clear();
    island_frame.details =
        card::detail_rows(None, 0.0, None, 0.0, ("未启用", card::Tone::Warn));
    island_renderer.paint_island(&island_frame)?;
    island_renderer.bench_island_parts(&island_frame)?;
    let t0 = std::time::Instant::now();
    for _ in 0..300 {
        island_renderer.paint_island(&island_frame)?;
    }
    let per = t0.elapsed().as_secs_f64() * 1000.0 / 300.0;
    println!(
        "island paint: {:.3} ms/frame (300 frames, {}x{} physical)",
        per, iw, ih
    );
    println!("after island paint = {:.1} MB", ws_mb());

    // ULW 呈现成本：在真实 layered window 上量（呼吸帧的固定开销）。
    let cfg = window::WindowConfig {
        x: 0,
        y: 0,
        w: iw,
        h: ih,
        visible: true,
    };
    let bench_hwnd = window::create_window(&cfg, Some(bench_proc))?;
    let present_surface = window::Surface::new(Some(bench_hwnd), iw, ih)?;
    let present_renderer = render::Renderer::new(present_surface.mem_dc, iw, ih, scale)?;
    present_renderer.paint_island(&island_frame)?;
    present_surface.present_rect(0, 0, iw, ih)?;
    let t0 = std::time::Instant::now();
    for _ in 0..300 {
        present_surface.present_rect(0, 0, iw, ih)?;
    }
    println!(
        "present only: {:.3} ms/frame (300 frames)",
        t0.elapsed().as_secs_f64() * 1000.0 / 300.0
    );
    let t0 = std::time::Instant::now();
    for _ in 0..300 {
        present_renderer.paint_island(&island_frame)?;
        present_surface.present_rect(0, 0, iw, ih)?;
    }
    println!(
        "island paint+present: {:.3} ms/frame (300 frames)",
        t0.elapsed().as_secs_f64() * 1000.0 / 300.0
    );
    // 形变中间帧（纯形状）成本：每帧按当前矩形重画圆角矩形 + ULW。
    // spec §6 基线表里的「纯形状」项就是它。
    let (bw, bh) = morph::shape_buffer(scale);
    let shape = render::ShapePainter::new(bw, bh, scale)?;
    let from = mode::Layout {
        scale,
        screen: screen_rect(),
        home: (0, 0),
        hot: false,
        anchor: mode::IslandAnchor::Center,
        island_pos: None,
    };
    let island_r = from.target(mode::UiMode::Island).unwrap();
    let card_r = from.target(mode::UiMode::Expanded).unwrap();
    let motion = mode::motion_for(mode::UiMode::Expanded);
    let n = 120;
    let frames: Vec<anim::Rect> = (0..n)
        .map(|i| anim::rect_at(island_r, card_r, motion, i as f64 * 6.0))
        .collect();
    let t0 = std::time::Instant::now();
    for r in &frames {
        let radius = morph::morph_frame_radius(*r, island_r.w, card_r.w, scale);
        shape.paint(r.w, r.h, radius)?;
    }
    println!(
        "morph shape paint: {:.3} ms/frame ({} frames, {}x{} buffer)",
        t0.elapsed().as_secs_f64() * 1000.0 / n as f64,
        n,
        bw,
        bh
    );
    let t0 = std::time::Instant::now();
    for r in &frames {
        window::present_dc(bench_hwnd, shape.dc(), r.x, r.y, r.w, r.h, 255)?;
    }
    println!(
        "morph shape present: {:.3} ms/frame ({} frames)",
        t0.elapsed().as_secs_f64() * 1000.0 / n as f64,
        n
    );
    let t0 = std::time::Instant::now();
    for r in &frames {
        let radius = morph::morph_frame_radius(*r, island_r.w, card_r.w, scale);
        shape.paint(r.w, r.h, radius)?;
        window::present_dc(bench_hwnd, shape.dc(), r.x, r.y, r.w, r.h, 255)?;
    }
    println!(
        "morph shape paint+present: {:.3} ms/frame ({} frames)",
        t0.elapsed().as_secs_f64() * 1000.0 / n as f64,
        n
    );
    let t0 = std::time::Instant::now();
    for _ in 0..50 {
        let _ = probe::proxy_state();
    }
    println!(
        "proxy_state: {:.3} ms/call (50 calls, 2s probe path)",
        t0.elapsed().as_secs_f64() * 1000.0 / 50.0
    );
    unsafe {
        let _ = DestroyWindow(bench_hwnd);
    }
    Ok(())
}

/// Renders one frame into an offscreen DIB and reports the alpha channel.
/// This proves the shape lives in the alpha channel itself, independent of
/// what the compositor does.
fn selftest() -> Result<()> {
    let scale = window::dpi_scale();
    let w = (LOGICAL_W * scale).round() as i32;
    let h = (LOGICAL_H * scale).round() as i32;
    let surface = window::Surface::new(None, w, h)?;
    let renderer = render::Renderer::new(surface.mem_dc, w, h, scale)?;

    let mut hist = vec![0.0f64; HIST];
    for (i, v) in hist.iter_mut().enumerate() {
        *v = if i % 7 == 0 { 3.0e6 } else { 4.0e5 + (i as f64 * 9.1e4) };
    }
    let frame = demo_frame(hist);
    renderer.paint(&frame)?;

    println!(
        "canvas: {w}x{h} physical (scale {scale}, font {}, collection sees Inter = {}, weights = {:?})",
        renderer.font_family(),
        renderer.dwrite_has_family("Inter"),
        renderer.family_weights("Inter")
    );
    println!(
        "alpha: center(240,150)={} corner(3,3)={} outside-samples={:?}",
        surface.alpha_at(w / 2, h / 2),
        surface.alpha_at(3, 3),
        [
            (60, 42),
            (420, 42),
            (60, 258),
            (420, 258),
            (30, 70),
            (450, 70),
            (30, 230),
            (450, 230),
            (0, 0),
            (w - 1, 0),
            (0, h - 1),
            (w - 1, h - 1)
        ]
        .iter()
        .map(|(x, y)| surface.alpha_at(*x, *y))
        .collect::<Vec<_>>()
    );

    // Full-bitmap audit: every pixel strictly outside the squircle must be
    // alpha 0. AA fringe pixels sit within ~1px of the curve, so also report
    // the largest alpha found on pixels that are clearly outside (>=1.02px).
    // 外投影按 CSS 语义被 border-box/窗口裁剪（参考实现窗口=卡片），所以
    // 窗口边缘不会出现投影 alpha；这里审计的是整个 DIB。
    let radius = card::RADIUS * scale;
    let mut outside_any = 0u32;
    let mut outside_far = 0u32;
    let mut max_far_alpha = 0u8;
    for y in 0..h {
        for x in 0..w {
            let d = card::outside_distance(
                x as f32 + 0.5,
                y as f32 + 0.5,
                w as f32,
                h as f32,
                radius,
                card::EXPONENT,
            );
            if d > 0.0 {
                let a = surface.alpha_at(x, y);
                if a > 0 {
                    outside_any += 1;
                    if d >= 1.02 {
                        outside_far += 1;
                        max_far_alpha = max_far_alpha.max(a);
                    }
                }
            }
        }
    }
    println!(
        "outside squircle (d>0, r={radius:.1}px n={}): alpha>0 pixels={outside_any} | at d>=1.02: pixels={outside_far} max_alpha={max_far_alpha}",
        card::EXPONENT
    );

    // 胶囊态：圆角矩形之外必须全透明（AA 边带除外）。
    let iw = (mode::ISLAND_W * scale as f64).round() as i32;
    let ih = (mode::ISLAND_H * scale as f64).round() as i32;
    let island_surface = window::Surface::new(None, iw, ih)?;
    let island_renderer = render::Renderer::new(island_surface.mem_dc, iw, ih, scale)?;
    let mut island_frame = demo_frame(Vec::new());
    island_frame.title.clear();
    island_frame.state.clear();
    island_frame.wifi.clear();
    island_frame.proxy.clear();
    island_frame.details =
        card::detail_rows(None, 0.0, None, 0.0, ("未启用", card::Tone::Warn));
    island_renderer.paint_island(&island_frame)?;
    let radius = island::capsule_radius(iw as f32, ih as f32, scale);
    let mut cap_any = 0u32;
    let mut cap_far = 0u32;
    let mut cap_max = 0u8;
    for y in 0..ih {
        for x in 0..iw {
            let d = rounded_rect_sdf(x as f32 + 0.5, y as f32 + 0.5, iw as f32, ih as f32, radius);
            if d > 0.0 {
                let a = island_surface.alpha_at(x, y);
                if a > 0 {
                    cap_any += 1;
                    if d >= 1.02 {
                        cap_far += 1;
                        cap_max = cap_max.max(a);
                    }
                }
            }
        }
    }
    println!(
        "capsule canvas: {iw}x{ih} physical (scale {scale}, radius {radius:.2}) center={} corner={}",
        island_surface.alpha_at(iw / 2, ih / 2),
        island_surface.alpha_at(2, 2)
    );
    println!(
        "outside capsule (d>0): alpha>0 pixels={cap_any} | at d>=1.02: pixels={cap_far} max_alpha={cap_max}"
    );
    Ok(())
}

/// 圆角矩形有符号距离（负=内部），用于胶囊 alpha 审计。
fn rounded_rect_sdf(px: f32, py: f32, w: f32, h: f32, r: f32) -> f32 {
    let qx = (px - w / 2.0).abs() - (w / 2.0 - r);
    let qy = (py - h / 2.0).abs() - (h / 2.0 - r);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - r
}
