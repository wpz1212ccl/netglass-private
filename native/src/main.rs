//! NetGlass native widget spike.
//!
//! A layered window (`WS_EX_LAYERED` + `UpdateLayeredWindow`) whose alpha channel
//! is exactly one ellipse: no rectangular backdrop, no WebView, per-pixel alpha
//! hit-testing for free.
//!
//! Modes:
//!   (no args)              run the widget
//!   --pos X,Y              initial window position (physical px)
//!   --pixels X,Y[;X,Y…]    print screen RGB at physical coordinates
//!   --hit X,Y[;X,Y…]       WindowFromPoint identity at physical coordinates
//!   --selftest             render offscreen, report DIB alpha inside/outside the ellipse

mod netmon;
mod render;
mod window;

use std::cell::RefCell;
use std::net::{SocketAddr, TcpStream};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows::core::Result;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::HiDpi::{GetDpiForSystem, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::WindowsAndMessaging::*;

const LOGICAL_W: f32 = 320.0;
const LOGICAL_H: f32 = 200.0;
const HIST: usize = 60;
// wry/windows-rs does not expose WM_MOUSELEAVE; the value is stable since Win2000.
const WM_MOUSELEAVE: u32 = 0x02A3;

fn main() {
    window::dpi_aware();
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "--dpi") {
        let r = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        println!("SetProcessDpiAwarenessContext(PMv2) -> {r:?} err={:?}", std::io::Error::last_os_error());
        println!("GetDpiForSystem={} SM_CXSCREEN={} SM_CYSCREEN={}", unsafe { GetDpiForSystem() }, unsafe { GetSystemMetrics(SM_CXSCREEN) }, unsafe { GetSystemMetrics(SM_CYSCREEN) });
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

struct App {
    surface: window::Surface,
    renderer: render::Renderer,
    pos: POINT,
    hist: [f64; HIST],
    unit_mb: bool,
    hover: bool,
    drag: Option<(POINT, i32, i32)>,
    rx: Receiver<netmon::Sample>,
    proxy: Arc<Mutex<Option<u16>>>,

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

    fn repaint(&self) {
        let t0 = std::time::Instant::now();
        let _ = self.renderer.paint(&self.frame());
        let t1 = std::time::Instant::now();
        let _ = self.surface.present(self.pos.x, self.pos.y);
        if std::env::var_os("NETGLASS_TRACE").is_some() {
            eprintln!(
                "[paint] draw={:.3}ms present={:.3}ms",
                t0.elapsed().as_secs_f64() * 1000.0,
                t1.elapsed().as_secs_f64() * 1000.0
            );
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
        let p = self.proxy.lock().ok().and_then(|g| *g);
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
        }
    }

    fn content_key(&self) -> String {
        let mut k = format!(
            "{}|{}|{} {}|{} {}|{}|{}|{}",
            self.title, self.state, self.down.0, self.down.1, self.up.0, self.up.1, self.wifi, self.proxy_text, self.hover
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
        WM_MOUSEMOVE => {
            APP.with(|a| {
                if let Some(app) = a.borrow_mut().as_mut() {
                    if let Some((start, wx, wy)) = app.drag {
                        let c = window::cursor_pos();
                        app.pos.x = wx + (c.x - start.x);
                        app.pos.y = wy + (c.y - start.y);
                        let _ = app.surface.present(app.pos.x, app.pos.y);
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
                    }
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            APP.with(|a| {
                if let Some(app) = a.borrow_mut().as_mut() {
                    let c = window::cursor_pos();
                    app.drag = Some((c, app.pos.x, app.pos.y));
                    SetCapture(hwnd);
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            APP.with(|a| {
                if let Some(app) = a.borrow_mut().as_mut() {
                    app.drag = None;
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

fn run(args: &[String]) -> Result<()> {
    let scale = window::dpi_scale();
    eprintln!("[boot] args={args:?} scale={scale} dpi={} screen={}x{}", unsafe { GetDpiForSystem() }, unsafe { GetSystemMetrics(SM_CXSCREEN) }, unsafe { GetSystemMetrics(SM_CYSCREEN) });
    let w = (LOGICAL_W * scale).round() as i32;
    let h = (LOGICAL_H * scale).round() as i32;

    let (px, py);
    if let Some(p) = window::parse_points(args, "--pos").first() {
        px = p.0;
        py = p.1;
    } else {
        let sw = unsafe { GetSystemMetrics(SM_CXSCREEN) };
        let sh = unsafe { GetSystemMetrics(SM_CYSCREEN) };
        px = (sw - w) / 2;
        py = (sh - h) / 2;
    }

    let cfg = window::WindowConfig {
        x: px,
        y: py,
        w,
        h,
    };
    let hwnd = window::create_window(&cfg, Some(wndproc))?;
    let surface = window::Surface::new(Some(hwnd), w, h)?;
    let renderer = render::Renderer::new(surface.mem_dc, w, h, scale)?;

    let (tx, rx) = mpsc::channel::<netmon::Sample>();
    let proxy = Arc::new(Mutex::new(None::<u16>));
    let raw = hwnd.0 as isize;

    // 1 Hz sampler: same logic as the Tauri app (GetIfEntry2 diff + default route).
    {
        let raw = raw;
        std::thread::spawn(move || {
            let mut sampler = netmon::Sampler::new();
            loop {
                let s = sampler.sample();
                if tx.send(s).is_err() {
                    break;
                }
                window::post_sample(raw);
                std::thread::sleep(Duration::from_millis(1000));
            }
        });
    }
    // Proxy port poll: 5x80ms TCP connects every 30s on a worker thread.
    {
        let proxy = proxy.clone();
        let raw = raw;
        std::thread::spawn(move || loop {
            let found = [7897u16, 7890, 1080, 10809, 2080]
                .into_iter()
                .find(|p| {
                    TcpStream::connect_timeout(
                        &SocketAddr::from(([127, 0, 0, 1], *p)),
                        Duration::from_millis(80),
                    )
                    .is_ok()
                });
            if let Ok(mut g) = proxy.lock() {
                *g = found;
            }
            window::post_sample(raw);
            std::thread::sleep(Duration::from_secs(30));
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

    let app = App {
        surface,
        renderer,
        pos: POINT { x: px, y: py },
        hist: [0.0; HIST],
        unit_mb: true,
        hover: false,
        drag: None,
        rx,
        proxy,
        title: "正在读取".to_string(),
        state: "—".to_string(),
        wifi: "Wi-Fi —".to_string(),
        proxy_text: "代理 —".to_string(),
        down: ("—".to_string(), "MB/s".to_string()),
        up: ("—".to_string(), "MB/s".to_string()),
        last_key: String::new(),
    };
    app.repaint();
    APP.with(|a| *a.borrow_mut() = Some(app));

    let mut msg = MSG::default();
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

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
    use windows::Win32::System::ProcessStatus::GetProcessMemoryInfo;
    use windows::Win32::System::Threading::GetCurrentProcess;
    use windows::Win32::System::ProcessStatus::PROCESS_MEMORY_COUNTERS;

    fn ws_mb() -> f64 {
        unsafe {
            let mut c = PROCESS_MEMORY_COUNTERS::default();
            if GetProcessMemoryInfo(GetCurrentProcess(), &mut c, std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32).is_ok() {
                c.WorkingSetSize as f64 / 1048576.0
            } else { 0.0 }
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
