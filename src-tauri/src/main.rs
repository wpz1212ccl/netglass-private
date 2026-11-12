#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod netmon;
mod probe;

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager, RunEvent,
};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

static LOCKED: AtomicBool = AtomicBool::new(false);
static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
static EXPAND_UNTIL: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

#[derive(Default, Serialize, Deserialize)]
struct Config {
    x: Option<i32>,
    y: Option<i32>,
    locked: Option<bool>,
    autostart: Option<bool>,
}

struct TrayState {
    auto_item: CheckMenuItem<tauri::Wry>,
}

fn config_path() -> std::path::PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    std::path::PathBuf::from(base)
        .join("com.netglass.app")
        .join("config.json")
}

fn load_config() -> Config {
    std::fs::read_to_string(config_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_config() {
    let old = load_config();
    let pos = HOME.lock().ok().and_then(|g| *g);
    let cfg = Config {
        locked: Some(LOCKED.load(Ordering::Relaxed)),
        autostart: old.autostart,
        x: pos.map(|p| p.0).or(old.x),
        y: pos.map(|p| p.1).or(old.y),
    };
    if let Some(dir) = config_path().parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(s) = serde_json::to_string_pretty(&cfg) {
        let _ = std::fs::write(config_path(), s);
    }
}


#[derive(Clone, Copy, PartialEq, Debug)]
enum UiMode {
    Expanded,
    Island,
    Hidden,
}

fn mode_code(mode: UiMode) -> u8 {
    match mode {
        UiMode::Expanded => 0,
        UiMode::Island => 1,
        UiMode::Hidden => 2,
    }
}

fn mode_from_code(code: u8) -> UiMode {
    match code {
        1 => UiMode::Island,
        2 => UiMode::Hidden,
        _ => UiMode::Expanded,
    }
}

fn mode_str(mode: UiMode) -> &'static str {
    match mode {
        UiMode::Expanded => "expanded",
        UiMode::Island => "island",
        UiMode::Hidden => "hidden",
    }
}

/// 实际显示形态。三态切换要加淡出动画，必须知道"现在是不是已经在这个形态"，
/// 否则模式轮询会在展开 700ms 后再补一次 hide/show，把动画打成两段。
static MODE: AtomicU8 = AtomicU8::new(0);

/// 展开态在桌面上的固定锚点（物理像素）。只记录"展开且可见"时的位置，
/// 胶囊的位置永远不会写进来——旧实现用"离开展开态时的 outer_position"，
/// 一旦展开是从胶囊位置长出来的，胶囊坐标就会被记成桌面位置。
static HOME: Mutex<Option<(i32, i32)>> = Mutex::new(None);

/// 首次运行没有可用配置时，卡片中心落在屏幕的这个比例位置（对应红框：上部居中）。
const HOME_CX: f64 = 0.50;
const HOME_CY: f64 = 0.30;

fn island_pos(win: &tauri::WebviewWindow) -> (i32, i32) {
    let scale = win.scale_factor().unwrap_or(1.0);
    let sw = unsafe { windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
        windows::Win32::UI::WindowsAndMessaging::SM_CXSCREEN,
    ) };
    let w = (156.0 * scale).round() as i32;
    ((sw - w) / 2, 0)
}

fn card_size(win: &tauri::WebviewWindow) -> (i32, i32) {
    let scale = win.scale_factor().unwrap_or(1.0);
    ((320.0 * scale).round() as i32, (200.0 * scale).round() as i32)
}

fn default_home(win: &tauri::WebviewWindow) -> (i32, i32) {
    let (w, h) = card_size(win);
    match win.current_monitor().ok().flatten() {
        Some(m) => {
            let pos = m.position();
            let size = m.size();
            (
                pos.x + (size.width as f64 * HOME_CX).round() as i32 - w / 2,
                pos.y + (size.height as f64 * HOME_CY).round() as i32 - h / 2,
            )
        }
        None => (0, 0),
    }
}

fn is_on_screen(p: (i32, i32), win: &tauri::WebviewWindow) -> bool {
    let (w, h) = card_size(win);
    match win.available_monitors() {
        Ok(monitors) => monitors.iter().any(|m| {
            let mp = m.position();
            let ms = m.size();
            p.0 + w > mp.x
                && p.0 < mp.x + ms.width as i32
                && p.1 + h > mp.y
                && p.1 < mp.y + ms.height as i32
        }),
        Err(_) => true,
    }
}

/// 起始位置：有可信配置就用配置；否则落在默认锚点。
/// (0,0) 是旧版在窗口还没摆好时读到的坏值，直接丢弃——这就是"飞到左上角"的来源。
fn resolve_home(win: &tauri::WebviewWindow) -> (i32, i32) {
    let cfg = load_config();
    if let (Some(x), Some(y)) = (cfg.x, cfg.y) {
        if !(x == 0 && y == 0) && is_on_screen((x, y), win) {
            return (x, y);
        }
    }
    default_home(win)
}

/// 只在"展开且可见"时采样窗口位置，避免把胶囊坐标当成桌面位置。
fn remember_expanded_pos(win: &tauri::WebviewWindow) {
    if mode_from_code(MODE.load(Ordering::Relaxed)) != UiMode::Expanded {
        return;
    }
    if !win.is_visible().unwrap_or(false) {
        return;
    }
    if let (Ok(p), Ok(mut g)) = (win.outer_position(), HOME.lock()) {
        *g = Some((p.x, p.y));
    }
}

/// 圆角形状完全交给 CSS（Chromium 的光栅化器会逐像素抗锯齿）。
///
/// 这里曾经用 `CreateRoundRectRgn` + `SetWindowRgn` 把窗口裁成圆角：
/// GDI 区域是 1-bit 蒙版，没有抗锯齿，28px 圆角在 125%/150% 缩放下
/// 会变成肉眼可见的台阶，胶囊两端也一样。区域裁剪已全部移除。
/// DWM 的玻璃底同理：`DwmEnableBlurBehindWindow` 的区域无法抗锯齿，
/// 保留它会在圆角外露出一块方形模糊，所以玻璃质感改由 CSS 渐变承担。
fn fade_out(win: &tauri::WebviewWindow, app: &tauri::AppHandle) {
    if !win.is_visible().unwrap_or(false) {
        return;
    }
    let _ = app.emit("net-leave", ());
    // 和 index.html 的 .out 过渡时长对齐，等淡出画完再隐藏/改尺寸
    std::thread::sleep(Duration::from_millis(130));
}

fn apply_mode(win: &tauri::WebviewWindow, app: &tauri::AppHandle, mode: UiMode) {
    use tauri::{LogicalSize, PhysicalPosition};
    if MODE.swap(mode_code(mode), Ordering::Relaxed) == mode_code(mode) {
        return;
    }
    fade_out(win, app);
    match mode {
        UiMode::Hidden => {
            let _ = win.hide();
        }
        UiMode::Expanded => {
            // 先藏起来再改尺寸：WebView2 会在隐藏期间完成重绘，
            // 否则切换瞬间会看到旧尺寸画面被拉伸的"双影"。
            let _ = win.hide();
            let _ = win.set_size(LogicalSize::new(320.0, 200.0));
            // 始终回到固定锚点：没有锚点时先算一个，绝不沿用胶囊位置。
            let home = HOME
                .lock()
                .ok()
                .and_then(|g| *g)
                .unwrap_or_else(|| default_home(win));
            let _ = win.set_position(PhysicalPosition::new(home.0, home.1));
            let _ = win.show();
            tune_window(win);
            let _ = app.emit("net-mode", mode_str(mode));
        }
        UiMode::Island => {
            let _ = win.hide();
            let _ = win.set_size(LogicalSize::new(156.0, 38.0));
            let (x, y) = island_pos(win);
            let _ = win.set_position(PhysicalPosition::new(x, y));
            let _ = win.show();
            tune_window(win);
            let _ = app.emit("net-mode", mode_str(mode));
        }
    }
}

fn fg_class(h: windows::Win32::Foundation::HWND) -> String {
    use windows::Win32::UI::WindowsAndMessaging::GetClassNameW;
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(h, &mut buf) }.max(0) as usize;
    String::from_utf16_lossy(&buf[..n])
}

fn fg_is_fullscreen(fg: windows::Win32::Foundation::HWND) -> bool {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, GetWindowLongW, GetWindowRect, GWL_STYLE, SM_CXSCREEN, SM_CYSCREEN,
        WS_CAPTION, WS_MAXIMIZE,
    };
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

fn toggle_lock(app: &tauri::AppHandle) {
    let next = !LOCKED.load(Ordering::Relaxed);
    LOCKED.store(next, Ordering::Relaxed);
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.set_ignore_cursor_events(next);
    }
    let _ = app.emit("net-lock", next);
}

fn toggle_visible(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        if w.is_visible().unwrap_or(false) {
            let _ = app.emit("net-leave", ());
            // 托盘回调跑在事件循环线程上，淡出等待放到后台线程，别卡住菜单
            let h = app.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(130));
                if let Some(w) = h.get_webview_window("main") {
                    let _ = w.hide();
                }
            });
        } else {
            let _ = w.show();
            tune_window(&w);
            // 全屏隐藏时不会改窗口尺寸：按当前宽度还原"隐藏前是什么形态"，
            // 否则可能把展开内容塞进胶囊大小里。
            let mode = match mode_from_code(MODE.load(Ordering::Relaxed)) {
                UiMode::Hidden => {
                    let logical_w = w
                        .inner_size()
                        .map(|s| s.width as f64 / w.scale_factor().unwrap_or(1.0))
                        .unwrap_or(0.0);
                    if logical_w < 200.0 {
                        UiMode::Island
                    } else {
                        UiMode::Expanded
                    }
                }
                m => m,
            };
            let _ = app.emit("net-mode", mode_str(mode));
        }
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
        let _ = writeln!(f, "{s}");
    }
}

/// 让 DWM 把窗口裁成圆角（否则 CSS 圆角外会露出方形背景），并移出 Alt+Tab。
fn tune_window(win: &tauri::WebviewWindow) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_WINDOW_CORNER_PREFERENCE,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    };

    let Ok(raw) = win.hwnd() else { return };
    let hwnd = HWND(raw.0 as _);
    let pref: i32 = 2; // DWMWCP_ROUND

    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &pref as *const _ as *const core::ffi::c_void,
            4,
        );
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        // 去掉 WS_EX_WINDOWEDGE：它会在窗口四周留 1–2px 深色发丝边。
        // 加上 WS_EX_NOACTIVATE：点卡片（拖动/展开）不会抢走前台软件的焦点。
        let next = (style | WS_EX_TOOLWINDOW.0 as isize | WS_EX_NOACTIVATE.0 as isize) & !0x100;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, next);

        // DWM 边框颜色设为 COLOR_NONE，彻底去掉四边的深色 2px 框
        let none: u32 = 0xFFFF_FFFE;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            &none as *const _ as *const core::ffi::c_void,
            4,
        );
    }
}

#[tauri::command]
fn expand_temporarily(app: tauri::AppHandle) {
    if let Ok(mut g) = EXPAND_UNTIL.lock() {
        *g = Some(std::time::Instant::now() + Duration::from_secs(8));
    }
    if let Some(w) = app.get_webview_window("main") {
        apply_mode(&w, &app, UiMode::Expanded);
    }
}

#[tauri::command]
fn app_ready(app: tauri::AppHandle) {
    // WebView2 冷启动首帧约 1 秒；按进程启动时间对齐到 1050ms 再显示，
    // 否则会看到一段纯色空板。
    let h = app.clone();
    std::thread::spawn(move || {
        let elapsed = START.get().map(|t| t.elapsed()).unwrap_or_default();
        let wait = Duration::from_millis(1050).saturating_sub(elapsed);
        std::thread::sleep(wait);
        if let Some(w) = h.get_webview_window("main") {
            let _ = w.show();
            // show 之后 Tauri 会重写 exstyle，必须在这里补一次
            tune_window(&w);
            let _ = h.emit("net-mode", mode_str(mode_from_code(MODE.load(Ordering::Relaxed))));
        }
        let _ = h.emit("net-lock", LOCKED.load(Ordering::Relaxed));
    });
}

fn main() {
    let _ = START.set(std::time::Instant::now());
    let cfg = load_config();
    let mut builder = tauri::Builder::default();

    // 单实例必须最先注册：第二次启动只唤起已有窗口
    builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.show();
            let _ = w.set_focus();
        }
    }));

    builder = builder.plugin(tauri_plugin_autostart::init(
        tauri_plugin_autostart::MacosLauncher::LaunchAgent,
        Some(vec!["--silent"]),
    ));

    builder = builder.plugin(
        tauri_plugin_global_shortcut::Builder::new()
            .with_handler(|app, _shortcut, event| {
                if event.state() == ShortcutState::Pressed {
                    toggle_lock(app);
                }
            })
            .build(),
    );

    let want_autostart = cfg.autostart.unwrap_or(true);
    let want_locked = cfg.locked.unwrap_or(false);

    let app = builder
        .invoke_handler(tauri::generate_handler![app_ready, expand_temporarily])
        .setup(move |app| {
            let win = app.get_webview_window("main").expect("main window missing");

            // Win11 上：DWM 背景只采样壁纸（压在任何窗口上都一样，但观感是浅色玻璃）；
            // 传统 BLURBEHIND 会糊出深灰底，不适合浅色卡片。取前者。
            // 不挂系统背景：胶囊完全由 CSS 画，才能做到"几乎看不见"，
            // 也不会在胶囊外露出一圈方形底。
            let _ = win.set_background_color(Some(tauri::window::Color(0, 0, 0, 0)));

            // 展开态的唯一锚点：可信配置 or 默认上部居中。启动时就落位，
            // 之后每次展开都回到这里，不再依赖"离开时的窗口位置"。
            let home = resolve_home(&win);
            if let Ok(mut g) = HOME.lock() {
                *g = Some(home);
            }
            let _ = win.set_position(tauri::PhysicalPosition::new(home.0, home.1));

            if want_locked {
                LOCKED.store(true, Ordering::Relaxed);
            }
            if LOCKED.load(Ordering::Relaxed) {
                let _ = win.set_ignore_cursor_events(true);
            }

            tune_window(&win);
            netmon::dump();

            // Ctrl+Alt+L 可能被别的程序占用，按候选顺序取第一个能注册的
            let mut hotkey = String::new();
            for c in ["Ctrl+Alt+L", "Ctrl+Alt+N", "Ctrl+Shift+L", "Ctrl+Alt+F9"] {
                if app.global_shortcut().register(c).is_ok() {
                    hotkey = c.to_string();
                    break;
                }
            }
            log_line(&format!("hotkey={hotkey}"));

            // 托盘
            let lock_label = if hotkey.is_empty() {
                "锁定 / 解锁（快捷键不可用）".to_string()
            } else {
                format!("锁定 / 解锁（{}）", hotkey)
            };
            let show_item = MenuItem::with_id(app, "toggle", "显示 / 隐藏", true, None::<&str>)?;
            let lock_item =
                MenuItem::with_id(app, "lock", lock_label.as_str(), true, None::<&str>)?;
            let auto_item =
                CheckMenuItem::with_id(app, "auto", "开机自启", true, want_autostart, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let sep = PredefinedMenuItem::separator(app)?;
            let menu = Menu::with_items(
                app,
                &[&show_item, &lock_item, &auto_item, &sep, &quit_item],
            )?;

            let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?;

            TrayIconBuilder::with_id("netglass")
                .icon(icon)
                .tooltip("NetGlass")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "toggle" => toggle_visible(app),
                    "lock" => toggle_lock(app),
                    "auto" => {
                        let al = app.autolaunch();
                        let now = al.is_enabled().unwrap_or(false);
                        if now {
                            let _ = al.disable();
                        } else {
                            let _ = al.enable();
                        }
                        if let Some(st) = app.try_state::<TrayState>() {
                            let _ = st.auto_item.set_checked(!now);
                        }
                        let mut cfg = load_config();
                        cfg.autostart = Some(!now);
                        if let Some((x, y)) = HOME.lock().ok().and_then(|g| *g) {
                            cfg.x = Some(x);
                            cfg.y = Some(y);
                        }
                        if let Some(dir) = config_path().parent() {
                            let _ = std::fs::create_dir_all(dir);
                        }
                        if let Ok(s) = serde_json::to_string_pretty(&cfg) {
                            let _ = std::fs::write(config_path(), s);
                        }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        toggle_visible(tray.app_handle());
                    }
                })
                .build(app)?;

            app.manage(TrayState { auto_item });

            // 首次运行时按配置开启自启（默认开）
            let al = app.autolaunch();
            if want_autostart && !al.is_enabled().unwrap_or(false) {
                let _ = al.enable();
            }

            // 不再周期性重申置顶：那会把卡片抬到你正在用的软件之上，
            // 表现为"每隔一会儿闪一下"。保持创建时的置顶层级即可。

            // 三种形态：
            //   桌面无遮挡 → 圆角玻璃卡片（用户放置的位置）
            //   别的窗口在前台 → 屏幕顶部居中的小胶囊
            //   前台是全屏应用 → 完全隐藏
            let mode_handle = app.handle().clone();
            std::thread::spawn(move || {
                use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
                let warmup = std::time::Instant::now();
                let mut last_other = unsafe { GetForegroundWindow() };
                loop {
                    std::thread::sleep(Duration::from_millis(700));
                    if warmup.elapsed() < Duration::from_millis(1600) {
                        continue;
                    }
                    let Some(w) = mode_handle.get_webview_window("main") else { continue };
                    let mut fg = unsafe { GetForegroundWindow() };
                    let mut class = fg_class(fg);

                    // 卡片自己拿到焦点时（个别系统仍会），以它之前的前台窗口为准，
                    // 否则会一直保持展开、压在你正在用的软件上。
                    let ours = class == "Tauri Window"
                        || w.hwnd()
                            .map(|h| h.0 as isize == fg.0 as isize)
                            .unwrap_or(false);
                    if ours {
                        fg = last_other;
                        class = fg_class(fg);
                    } else {
                        last_other = fg;
                    }

                    let desktop = matches!(
                        class.as_str(),
                        "Progman" | "WorkerW" | "Shell_TrayWnd" | "NotifyIconOverflowWindow"
                    );
                    let pinned = EXPAND_UNTIL
                        .lock()
                        .ok()
                        .and_then(|g| *g)
                        .map(|t| t > std::time::Instant::now())
                        .unwrap_or(false);

                    let want = if !desktop && fg_is_fullscreen(fg) {
                        UiMode::Hidden
                    } else if desktop || pinned {
                        UiMode::Expanded
                    } else {
                        UiMode::Island
                    };

                    // MODE 由 apply_mode 维护，expand_temporarily 也会改，
                    // 用共享状态判断才不会在展开后再补一次切换动画。
                    let current = mode_from_code(MODE.load(Ordering::Relaxed));
                    if want != current {
                        if current == UiMode::Expanded {
                            remember_expanded_pos(&w);
                        }
                        log_line(&format!("mode {:?} fg={class}", want));
                        apply_mode(&w, &mode_handle, want);
                    }
                }
            });

            // 兜底：前端若在 3 秒内没报到，强制显示，避免永远看不见
            let fb = app.handle().clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(3));
                if let Some(w) = fb.get_webview_window("main") {
                    if !w.is_visible().unwrap_or(false) {
                        let _ = w.show();
                    }
                }
            });

            // 采样线程
            let handle = app.handle().clone();            std::thread::spawn(move || {
                let mut sampler = netmon::Sampler::new();
                sampler.sample();
                std::thread::sleep(Duration::from_millis(1000));
                loop {
                    let s = sampler.sample();
                    let _ = handle.emit("net-sample", &s);
                    std::thread::sleep(Duration::from_millis(1000));
                }
            });

            // 探针线程
            let probe_handle = app.handle().clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(3));
                loop {
                    let p = probe::run_probe();
                    let _ = probe_handle.emit("net-probe", &p);
                    std::thread::sleep(Duration::from_secs(30));
                }
            });

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|app, event| {
        if let RunEvent::Exit = event {
            if let Some(w) = app.get_webview_window("main") {
                remember_expanded_pos(&w);
            }
            save_config();
        }
    });
}
