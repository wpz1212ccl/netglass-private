//! NetGlass native widget.
//!
//! A layered window (`WS_EX_LAYERED` + `UpdateLayeredWindow`) whose alpha channel
//! is exactly the shape: no rectangular backdrop, no WebView, per-pixel alpha
//! hit-testing for free.
//!
//! 形态由 `mode.rs` 的 260 ms 前台轮询状态机决定（expanded / island / hidden），
//! 形变由 `anim.rs` 的弹簧线程推进，每帧通过 ULW 的 pptDst+psize 落位。
//!
//! Modes:
//!   (no args)              run the widget
//!   --silent               自启动：桌面稳定到展开前不显窗
//!   --pos X,Y              initial window position (physical px)
//!   --pixels X,Y[;X,Y…]    print screen RGB at physical coordinates
//!   --hit X,Y[;X,Y…]       WindowFromPoint identity at physical coordinates
//!   --selftest             render offscreen, report DIB alpha inside/outside the shape

mod anim;
mod mode;
mod netmon;
mod probe;
mod render;
mod window;

use std::cell::RefCell;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::core::Result;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
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

/// 实际显示形态。apply_mode 维护；形态线程写、UI 线程读。
static MODE: AtomicU8 = AtomicU8::new(0);

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
    unit_mb: bool,
    hover: bool,
    drag: Option<(POINT, i32, i32)>,
    rx: Receiver<netmon::Sample>,
    probe: Arc<Mutex<Option<probe::ProbePayload>>>,

    title: String,
    state: String,
    wifi: String,
    proxy_text: String,
    down: (String, String),
    up: (String, String),
    last_key: String,
}

impl App {
    fn frame(&self) -> render::Frame {
        render::Frame {
            title: self.title.clone(),
            state: self.state.clone(),
            down_value: self.down.0.clone(),
            down_unit: self.down.1.clone(),
            up_value: self.up.0.clone(),
            up_unit: self.up.1.clone(),
            wifi: self.wifi.clone(),
            proxy: self.proxy_text.clone(),
            hist: self.hist.to_vec(),
            hover: self.hover,
        }
    }

    fn current_mode(&self) -> mode::UiMode {
        mode::mode_from_code(self.applied)
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
        let _ = self.ui.surface.present_rect(r.x, r.y, r.w, r.h);
        if let Ok(mut g) = PRESENTED.lock() {
            *g = r;
        }
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
        if self.applied == code && self.visible {
            return;
        }
        self.applied = code;
        MODE.store(code, Ordering::Relaxed);

        if m == mode::UiMode::Hidden {
            if self.visible {
                unsafe {
                    let _ = ShowWindow(self.hwnd, SW_HIDE);
                }
                self.visible = false;
                VISIBLE_FLAG.store(0, Ordering::Relaxed);
            }
            return;
        }

        let Some(target) = self.layout.target(m) else {
            return;
        };
        self.ensure_ui(target);
        self.repaint();
        if !self.visible {
            // 首次显示 / 从隐藏态回来：先把矩形摆正再露出来，避免空板。
            self.present_rect(target);
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOWNA);
            }
            self.visible = true;
            VISIBLE_FLAG.store(1, Ordering::Relaxed);
            anim::snap_to(target);
        } else {
            // 内容重画完 → 窗口矩形弹簧形变（逐帧 ULW 改尺寸）。
            anim::animate_to(target, mode::motion_for(m));
        }
    }

    fn refresh(&mut self) {
        let mut last = None;
        while let Ok(s) = self.rx.try_recv() {
            last = Some(s);
        }
        if let Some(s) = last {
            self.apply(&s);
        }
        let p = self
            .probe
            .lock()
            .ok()
            .and_then(|g| g.as_ref().and_then(|p| p.proxy.port));
        self.proxy_text = match p {
            Some(port) => format!("代理 :{port}"),
            None => "代理 未启用".to_string(),
        };
        // 1 Hz ticks with nothing new on screen (no traffic at all) would only
        // burn a composite; skip them.
        let key = self.content_key();
        if key != self.last_key {
            self.last_key = key;
            self.repaint();
            let r = *PRESENTED.lock().unwrap_or_else(|e| e.into_inner());
            self.present_rect(r);
        }
    }

    fn content_key(&self) -> String {
        let mut k = format!(
            "{}|{}|{} {}|{} {}|{}|{}|{}",
            self.title,
            self.state,
            self.down.0,
            self.down.1,
            self.up.0,
            self.up.1,
            self.wifi,
            self.proxy_text,
            self.hover
        );
        for v in self.hist.iter() {
            k.push(',');
            k.push_str(&format!("{v:.0}"));
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
        self.wifi = match s.ifaces.iter().find(|i| i.kind == "wifi") {
            None => "Wi-Fi 无",
            Some(i) if i.state == "up" => "Wi-Fi 已连接",
            Some(_) => "Wi-Fi 未连接",
        }
        .to_string();
        self.down = fmt_hysteresis(s.total_down, &mut self.unit_mb);
        self.up = fmt_plain(s.total_up);
        self.hist.rotate_left(1);
        self.hist[HIST - 1] = s.total_down;
    }
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
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
                        app.present_rect(r);
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
                        let r = anim::Rect::new(wx + (c.x - start.x), wy + (c.y - start.y), app.ui.w, app.ui.h);
                        app.present_rect(r);
                    } else if !app.hover {
                        app.hover = true;
                        let mut tme = TRACKMOUSEEVENT {
                            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE,
                            hwndTrack: hwnd,
                            dwHoverTime: 0,
                        };
                        let _ = TrackMouseEvent(&mut tme);
                        app.repaint();
                        let r = *PRESENTED.lock().unwrap_or_else(|e| e.into_inner());
                        app.present_rect(r);
                    }
                }
            });
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            APP.with(|a| {
                if let Some(app) = a.borrow_mut().as_mut() {
                    if app.hover {
                        app.hover = false;
                        app.repaint();
                        let r = *PRESENTED.lock().unwrap_or_else(|e| e.into_inner());
                        app.present_rect(r);
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
                    SetCapture(hwnd);
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            APP.with(|a| {
                if let Some(app) = a.borrow_mut().as_mut() {
                    app.drag = None;
                    // 拖动落点成为新的桌面锚点（展开态才有意义）
                    if app.current_mode() == mode::UiMode::Expanded {
                        let cur = *PRESENTED.lock().unwrap_or_else(|e| e.into_inner());
                        app.layout.home = (cur.x, cur.y);
                    }
                }
            });
            let _ = ReleaseCapture();
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            // 右键退出：NOACTIVATE 窗口不抢焦点，别留下无法关闭的孤儿进程
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
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

fn run(args: &[String]) -> Result<()> {
    let scale = window::dpi_scale();
    let screen = screen_rect();
    let silent = args.iter().any(|a| a == "--silent");
    eprintln!(
        "[boot] args={args:?} silent={silent} scale={scale} dpi={} screen={}x{}",
        unsafe { GetDpiForSystem() },
        screen.w,
        screen.h
    );

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
    };
    let card = layout.card();

    let cfg = window::WindowConfig {
        x: card.x,
        y: card.y,
        w: card.w,
        h: card.h,
        visible: !silent,
    };
    let hwnd = window::create_window(&cfg, Some(wndproc))?;
    let ui = Ui::new(Some(hwnd), card.w, card.h, scale)?;
    let raw = hwnd.0 as isize;
    if let Ok(mut g) = PRESENTED.lock() {
        *g = card;
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

    // 静默启动从胶囊态起步：gate 必须先走满 2 s 桌面 settle 才会给出 Expanded，
    // 在此之前 show_on_decide 一律不显窗（P1-3）。非静默直接以展开态冷启动。
    let initial_mode = if silent {
        mode::UiMode::Island
    } else {
        mode::UiMode::Expanded
    };

    let app = App {
        ui,
        hwnd,
        scale,
        layout,
        applied: mode::mode_code(initial_mode),
        visible: !silent,
        hist: [0.0; HIST],
        unit_mb: true,
        hover: false,
        drag: None,
        rx,
        probe: probe_state,
        title: "正在读取".to_string(),
        state: "—".to_string(),
        wifi: "Wi-Fi —".to_string(),
        proxy_text: "代理 —".to_string(),
        down: ("—".to_string(), "MB/s".to_string()),
        up: ("—".to_string(), "MB/s".to_string()),
        last_key: String::new(),
    };
    app.repaint();
    app.present_rect(card);
    MODE.store(mode::mode_code(initial_mode), Ordering::Relaxed);
    APP.with(|a| *a.borrow_mut() = Some(app));
    VISIBLE_FLAG.store(if silent { 0 } else { 1 }, Ordering::Relaxed);

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
        card,
    );

    // 形态线程：260 ms 前台轮询 → ModeGate → 三态（spec D3 维持 260 ms）。
    {
        let raw = raw;
        let mut layout = layout;
        std::thread::spawn(move || {
            let warmup = Instant::now();
            let mut last_other = unsafe { GetForegroundWindow() };
            let mut gate = mode::ModeGate::new();
            let mut silent_pending = silent;
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

                // 前台窗口压住顶部居中时躲到没被覆盖的一角；只是平移窗口，
                // 不换内容。任务栏/Alt+Tab 覆盖层保持当前挂载点，避免来回跳。
                let next_anchor = if kind == mode::FgKind::App {
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
                    log_line(&format!("anchor {:?} fg={class}", anchor));
                    if current == mode::UiMode::Island {
                        if let Some(r) = layout.target(current) {
                            anim::animate_to(r, mode::DODGE_MOTION);
                        }
                    }
                }

                let want = gate.decide(
                    Instant::now(),
                    mode::FgSnapshot {
                        kind,
                        fullscreen: kind == mode::FgKind::App && fg_is_fullscreen(fg),
                        // 点击/双击/躲让的交互接线在 T5；这里只跑状态机本身。
                        manual_island: false,
                        pinned_here: false,
                        already_expanded: current == mode::UiMode::Expanded,
                    },
                );
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

fn fmt_plain(v: f64) -> (String, String) {
    if !v.is_finite() || v < 1024.0 {
        return ("0.0".to_string(), "MB/s".to_string());
    }
    let m = v / 1048576.0;
    if m >= 1.0 {
        (format!("{m:.1}"), "MB/s".to_string())
    } else {
        (format!("{}", (v / 1024.0).round() as i64), "KB/s".to_string())
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
    let frame = render::Frame {
        title: "以太网 2".to_string(),
        state: "已连接".to_string(),
        down_value: "2.9".to_string(),
        down_unit: "MB/s".to_string(),
        up_value: "148".to_string(),
        up_unit: "KB/s".to_string(),
        wifi: "Wi-Fi 已连接".to_string(),
        proxy: "代理 :7897".to_string(),
        hist,
        hover: false,
    };
    renderer.paint(&frame)?;
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
    let frame = render::Frame {
        title: "以太网 2".to_string(),
        state: "已连接".to_string(),
        down_value: "2.9".to_string(),
        down_unit: "MB/s".to_string(),
        up_value: "148".to_string(),
        up_unit: "KB/s".to_string(),
        wifi: "Wi-Fi 已连接".to_string(),
        proxy: "代理 :7897".to_string(),
        hist,
        hover: false,
    };
    renderer.paint(&frame)?;

    println!("canvas: {w}x{h} physical (scale {scale})");
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

    // Full-bitmap audit: every pixel strictly outside the ellipse must be
    // alpha 0. AA fringe pixels sit within ~1px of the curve, so also report
    // the largest alpha found on pixels that are clearly outside (>=1.02).
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    let (rx, ry) = (w as f32 / 2.0, h as f32 / 2.0);
    let mut outside_any = 0u32;
    let mut outside_far = 0u32;
    let mut max_far_alpha = 0u8;
    for y in 0..h {
        for x in 0..w {
            let nx = (x as f32 + 0.5 - cx) / rx;
            let ny = (y as f32 + 0.5 - cy) / ry;
            let d = nx * nx + ny * ny;
            if d > 1.0 {
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
        "outside ellipse (d>1.0): alpha>0 pixels={outside_any} | at d>=1.02: pixels={outside_far} max_alpha={max_far_alpha}"
    );
    Ok(())
}
