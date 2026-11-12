#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod anim;
mod netmon;
mod probe;

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU8, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

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
/// 从胶囊展开时记住当时的前台窗口：同一窗口保持 pin，
/// 一旦用户切到别的页面/应用就立即收回，不被 8s pin 挡住。
static PIN_FG: AtomicIsize = AtomicIsize::new(0);
/// 用户双击卡片后手动收回胶囊：在点击胶囊重新展开前，不被"桌面=展开"抢回来。
static MANUAL_ISLAND: AtomicBool = AtomicBool::new(false);
/// 手动拖动是否进行中。拖动由独立线程跟随光标移动窗口，
/// 不再使用 HTCAPTION（系统会把拖动区双击解释成最大化/全屏）。
static DRAGGING: AtomicBool = AtomicBool::new(false);

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

/// 前台窗口类别。任务栏、Alt+Tab 覆盖层、托盘溢出面板是 Shell 的临时 UI：
/// 它们会短暂抢走前台，但不代表用户离开了桌面或切换了页面。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FgKind {
    Desktop,
    ShellTransient,
    App,
}

fn classify_fg(class: &str) -> FgKind {
    match class {
        "Progman" | "WorkerW" => FgKind::Desktop,
        "Shell_TrayWnd"
        | "NotifyIconOverflowWindow"
        | "TopLevelWindowForOverflowXamlIsland"
        | "XamlExplorerHostIslandWindow"
        | "Windows.UI.Core.CoreWindow"
        | "TaskSwitcherWnd"
        | "MultitaskingViewFrame" => FgKind::ShellTransient,
        _ => FgKind::App,
    }
}

#[derive(Clone, Copy, Debug)]
struct FgSnapshot {
    kind: FgKind,
    fullscreen: bool,
    manual_island: bool,
    pinned_here: bool,
    already_expanded: bool,
}

/// 桌面连续在前台多久才自动展开。短于这个值的前台变化（点任务栏、
/// Alt+Tab、最小化动画）都会让计时作废，避免胶囊与卡片来回抽动。
const DESKTOP_SETTLE: Duration = Duration::from_secs(2);

/// 前台窗口 → 形态的纯决策状态机。时间由调用方注入，便于按 260ms 轮询节奏单测。
struct ModeGate {
    desktop_since: Option<Instant>,
}

impl ModeGate {
    fn new() -> Self {
        Self {
            desktop_since: None,
        }
    }

    fn decide(&mut self, now: Instant, s: FgSnapshot) -> UiMode {
        match s.kind {
            FgKind::Desktop => {
                self.desktop_since.get_or_insert(now);
            }
            // 任务栏/Alt+Tab 覆盖层只是路过，既不算离开桌面，也不启动计时
            FgKind::ShellTransient => {}
            FgKind::App => self.desktop_since = None,
        }
        if s.kind == FgKind::App && s.fullscreen {
            return UiMode::Hidden;
        }
        if s.manual_island {
            return UiMode::Island;
        }
        let desktop_stable = self
            .desktop_since
            .is_some_and(|t| now.duration_since(t) >= DESKTOP_SETTLE);
        // 2 秒门槛只约束"胶囊→卡片"；已经展开的卡片在桌面上不必先收回再展开
        // （冷启动首帧就是这样）。
        if desktop_stable || s.pinned_here || (s.kind == FgKind::Desktop && s.already_expanded) {
            UiMode::Expanded
        } else {
            UiMode::Island
        }
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
const HOME_CY: f64 = 0.215;

const CARD_W: f64 = 320.0;
const CARD_H: f64 = 200.0;
const ISLAND_W: f64 = 144.0;
const ISLAND_H: f64 = 34.0;
// 悬浮时窗口弹簧放大到 162x42；窗口始终等于可见胶囊，没有透明留白。
const ISLAND_HOT_W: f64 = 162.0;
const ISLAND_HOT_H: f64 = 42.0;
static ISLAND_HOT: AtomicBool = AtomicBool::new(false);
/// 最近一次真正显示过的形态（Hidden 之前的），托盘重新显示时用它还原布局。
static LAST_SHOWN: AtomicU8 = AtomicU8::new(0);

fn card_size(win: &tauri::WebviewWindow) -> (i32, i32) {
    let scale = win.scale_factor().unwrap_or(1.0);
    ((CARD_W * scale).round() as i32, (CARD_H * scale).round() as i32)
}

fn card_rect(win: &tauri::WebviewWindow) -> anim::Rect {
    let (w, h) = card_size(win);
    let (x, y) = HOME
        .lock()
        .ok()
        .and_then(|g| *g)
        .unwrap_or_else(|| default_home(win));
    anim::Rect::new(x, y, w, h)
}

/// 灵动岛挂载点。默认顶部居中；前台窗口压住这个位置时躲到它没盖住的一侧。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum IslandAnchor {
    Center,
    Left,
    Right,
}

static ISLAND_ANCHOR: AtomicU8 = AtomicU8::new(0);

fn anchor_code(anchor: IslandAnchor) -> u8 {
    match anchor {
        IslandAnchor::Center => 0,
        IslandAnchor::Left => 1,
        IslandAnchor::Right => 2,
    }
}

fn island_anchor() -> IslandAnchor {
    match ISLAND_ANCHOR.load(Ordering::Relaxed) {
        1 => IslandAnchor::Left,
        2 => IslandAnchor::Right,
        _ => IslandAnchor::Center,
    }
}

fn screen_rect() -> anim::Rect {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN,
    };
    unsafe {
        anim::Rect::new(
            0,
            0,
            GetSystemMetrics(SM_CXSCREEN),
            GetSystemMetrics(SM_CYSCREEN),
        )
    }
}

/// 默认挂载位置（顶部居中、基础尺寸）。只用来判断"这个位置有没有被压住"，
/// 不受悬浮放大和当前躲让位置影响。
fn island_center_rect(win: &tauri::WebviewWindow) -> anim::Rect {
    let scale = win.scale_factor().unwrap_or(1.0);
    let sw = screen_rect().w;
    let w = (ISLAND_W * scale).round() as i32;
    let h = (ISLAND_H * scale).round() as i32;
    anim::Rect::new((sw - w) / 2, 0, w, h)
}

/// 前台窗口矩形与"顶部居中"相交时选一个角落：
/// 哪侧没被窗口覆盖就躲哪侧；两侧都塞不下时躲空间大的那边，
/// 完全贴满（最大化）则躲左上——右上永远是窗口的最小化/关闭按钮。
fn pick_anchor(
    window: Option<anim::Rect>,
    island: anim::Rect,
    screen: anim::Rect,
) -> IslandAnchor {
    let Some(window) = window else {
        return IslandAnchor::Center;
    };
    let right = window.x + window.w;
    let bottom = window.y + window.h;
    let overlaps = window.x < island.x + island.w
        && right > island.x
        && window.y < island.y + island.h
        && bottom > island.y;
    if !overlaps {
        return IslandAnchor::Center;
    }
    let screen_right = screen.x + screen.w;
    let free_left = window.x.clamp(screen.x, screen_right) - screen.x;
    let free_right = screen_right - right.clamp(screen.x, screen_right);
    let fits_left = free_left >= island.w;
    let fits_right = free_right >= island.w;
    match (fits_left, fits_right) {
        (true, false) => IslandAnchor::Left,
        (false, true) => IslandAnchor::Right,
        (true, true) if free_right > free_left => IslandAnchor::Right,
        (true, true) => IslandAnchor::Left,
        (false, false) if free_right > free_left => IslandAnchor::Right,
        (false, false) => IslandAnchor::Left,
    }
}

fn island_rect(win: &tauri::WebviewWindow, hot: bool) -> anim::Rect {
    let scale = win.scale_factor().unwrap_or(1.0);
    let sw = screen_rect().w;
    let (lw, lh) = if hot {
        (ISLAND_HOT_W, ISLAND_HOT_H)
    } else {
        (ISLAND_W, ISLAND_H)
    };
    let w = (lw * scale).round() as i32;
    let h = (lh * scale).round() as i32;
    let edge = (8.0 * scale).round() as i32;
    let x = match island_anchor() {
        IslandAnchor::Center => (sw - w) / 2,
        IslandAnchor::Left => edge,
        IslandAnchor::Right => sw - w - edge,
    };
    anim::Rect::new(x, 0, w, h)
}

fn target_rect(win: &tauri::WebviewWindow, mode: UiMode) -> Option<anim::Rect> {
    match mode {
        UiMode::Expanded => Some(card_rect(win)),
        UiMode::Island => Some(island_rect(win, ISLAND_HOT.load(Ordering::Relaxed))),
        UiMode::Hidden => None,
    }
}

const HOVER_MOTION: anim::Motion = anim::Motion::new(0.30, 0.55);

/// 躲到屏幕角落：大位移纯平移。阻尼取高值，回弹再好看也不能冲出屏幕边缘。
const DODGE_MOTION: anim::Motion = anim::Motion::new(0.34, 0.92);

/// 展开：略软、带回弹（damping < 1）；收回：更快、几乎不过冲。
fn motion_for(mode: UiMode) -> anim::Motion {
    match mode {
        UiMode::Expanded => anim::Motion::delayed(0.42, 0.54, 90),
        UiMode::Island => anim::Motion::delayed(0.34, 0.62, 50),
        UiMode::Hidden => anim::Motion::new(0.28, 1.0),
    }
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

/// 只在"展开且已落定"时采样窗口位置：形变途中尺寸不是 320x200，
/// 这时读到的是胶囊/中间帧坐标，不能当成桌面锚点。
fn remember_expanded_pos(win: &tauri::WebviewWindow) {
    if mode_from_code(MODE.load(Ordering::Relaxed)) != UiMode::Expanded {
        return;
    }
    if !win.is_visible().unwrap_or(false) {
        return;
    }
    let (cw, ch) = card_size(win);
    let settled = win
        .inner_size()
        .map(|s| (s.width as i32 - cw).abs() <= 2 && (s.height as i32 - ch).abs() <= 2)
        .unwrap_or(false);
    if !settled {
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
    // 和 index.html 的 .out 过渡时长对齐，等淡出画完再隐藏
    std::thread::sleep(Duration::from_millis(220));
}

fn apply_mode(win: &tauri::WebviewWindow, app: &tauri::AppHandle, mode: UiMode) {
    use tauri::{LogicalSize, PhysicalPosition};
    let code = mode_code(mode);
    if MODE.swap(code, Ordering::Relaxed) == code {
        return;
    }
    if mode != UiMode::Hidden {
        LAST_SHOWN.store(code, Ordering::Relaxed);
    }

    match mode {
        UiMode::Hidden => {
            ISLAND_HOT.store(false, Ordering::Relaxed);
            fade_out(win, app);
            let _ = win.hide();
            set_webview_active(app, false);
        }
        UiMode::Expanded | UiMode::Island => {
            if mode != UiMode::Island {
                ISLAND_HOT.store(false, Ordering::Relaxed);
            }
            let Some(target) = target_rect(win, mode) else { return };
            if !win.is_visible().unwrap_or(false) {
                // 从隐藏态直接显示：先把窗口摆到目标矩形，再用 CSS 淡入
                let (lw, lh) = if mode == UiMode::Island {
                    (ISLAND_W as f32, ISLAND_H as f32)
                } else {
                    (CARD_W as f32, CARD_H as f32)
                };
                let _ = win.set_size(LogicalSize::new(lw, lh));
                let _ = win.set_position(PhysicalPosition::new(target.x, target.y));
                anim::snap_to(target);
                set_webview_active(app, true);
                let _ = win.show();
                tune_window(win);
                let _ = app.emit("net-mode", mode_str(mode));
            } else {
                // 内容先退场 -> 窗口矩形弹簧形变 -> 内容再进场（见 net-mode 延迟）
                let _ = app.emit("net-morph", mode_str(mode));
                anim::animate_to(target, motion_for(mode));
                let app2 = app.clone();
                let delay = if mode == UiMode::Expanded { 360 } else { 250 };
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(delay));
                    // 期间如果又切了形态，这一帧的内容事件就作废
                    if MODE.load(Ordering::Relaxed) == code {
                        let _ = app2.emit("net-mode", mode_str(mode));
                    }
                });
            }
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

/// 前台窗口矩形（物理像素）。最小化窗口的坐标是 -32000，阴影/辅助窗口
/// 可能整块离屏或只有几十像素宽——这些都不能代表用户正在看的界面，返回 None。
fn fg_window_rect(fg: windows::Win32::Foundation::HWND) -> Option<anim::Rect> {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowRect, IsIconic};
    if unsafe { IsIconic(fg) }.as_bool() {
        return None;
    }
    let mut r = RECT::default();
    unsafe { GetWindowRect(fg, &mut r).ok()? };
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

fn toggle_lock(app: &tauri::AppHandle) {
    let next = !LOCKED.load(Ordering::Relaxed);
    LOCKED.store(next, Ordering::Relaxed);
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.set_ignore_cursor_events(next);
    }
    let _ = app.emit("net-lock", next);
}



/// 采样/探针线程共用：窗口不可见时前端没有渲染的必要。
fn window_visible(app: &tauri::AppHandle) -> bool {
    app.get_webview_window("main")
        .and_then(|w| w.is_visible().ok())
        .unwrap_or(false)
}

/// 隐藏时同步 WebView2 IsVisible 并 TrySuspend：Tauri 的 hide() 只做 SW_HIDE，
/// controller 的 IsVisible 恒 TRUE，渲染管线会一直空转（MS 官方要求隐藏时关）。
/// 显示时先 Resume 再可见，避免恢复首帧空板。
fn set_webview_active(app: &tauri::AppHandle, active: bool) {
    let Some(w) = app.get_webview_window("main") else { return };
    let _ = w.with_webview(move |pv| unsafe {
        use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2_3;
        use windows::core::Interface;
        let _ = pv.controller().SetIsVisible(active);
        let Ok(wv) = pv.controller().CoreWebView2() else { return };
        let Ok(wv3) = wv.cast::<ICoreWebView2_3>() else { return };
        if active {
            let mut suspended = windows::core::BOOL::default();
            if wv3.IsSuspended(&mut suspended).is_ok() && suspended.as_bool() {
                let _ = wv3.Resume();
            }
        } else {
            let handler = webview2_com::TrySuspendCompletedHandler::create(Box::new(
                |_r: windows::core::Result<()>, _s: bool| Ok(()),
            ));
            let _ = wv3.TrySuspend(&handler);
        }
    });
}

fn toggle_visible(app: &tauri::AppHandle) {
    use tauri::{LogicalSize, PhysicalPosition};
    if let Some(w) = app.get_webview_window("main") {
        if w.is_visible().unwrap_or(false) {
            ISLAND_HOT.store(false, Ordering::Relaxed);
            remember_expanded_pos(&w);
            let _ = app.emit("net-leave", ());
            // 托盘回调跑在事件循环线程上，淡出等待放到后台线程，别卡住菜单
            let h = app.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(220));
                if let Some(w) = h.get_webview_window("main") {
                    let _ = w.hide();
                    set_webview_active(&h, false);
                }
            });
        } else {
            // 全屏隐藏期间窗口可能停在半形变尺寸：按"最后显示过的形态"
            // 先把矩形摆正，再用 CSS 淡入。
            let mode = mode_from_code(LAST_SHOWN.load(Ordering::Relaxed));
            if let Some(target) = target_rect(&w, mode) {
                let (lw, lh) = if mode == UiMode::Island {
                    (ISLAND_W as f32, ISLAND_H as f32)
                } else {
                    (CARD_W as f32, CARD_H as f32)
                };
                let _ = w.set_size(LogicalSize::new(lw, lh));
                let _ = w.set_position(PhysicalPosition::new(target.x, target.y));
                anim::snap_to(target);
            }
            set_webview_active(app, true);
            let _ = w.show();
            tune_window(&w);
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
        // 常驻进程无界追加会慢慢长大；超 256KB 清空重来
        if f.metadata().map(|m| m.len() > 256 * 1024).unwrap_or(false) {
            let _ = f.set_len(0);
        }
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
        // 去掉 WS_EX_WINDOWEDGE（1–2px 深色发丝边），加上 NOACTIVATE
        // （点卡片不抢前台焦点）与 TOOLWINDOW（不进 Alt+Tab）。
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

/// 鼠标进入/离开胶囊：窗口做一次小幅弹簧放大。
#[tauri::command]
fn island_hover(app: tauri::AppHandle, hot: bool) {
    ISLAND_HOT.store(hot, Ordering::Relaxed);
    if mode_from_code(MODE.load(Ordering::Relaxed)) != UiMode::Island {
        return;
    }
    if let Some(w) = app.get_webview_window("main") {
        if w.is_visible().unwrap_or(false) {
            anim::animate_to(island_rect(&w, hot), HOVER_MOTION);
        }
    }
}

/// 双击卡片：手动收回顶部胶囊（并解除 pin）。
#[tauri::command]
fn collapse_to_island(app: tauri::AppHandle) {
    MANUAL_ISLAND.store(true, Ordering::Relaxed);
    if let Ok(mut g) = EXPAND_UNTIL.lock() {
        *g = None;
    }
    if let Some(w) = app.get_webview_window("main") {
        remember_expanded_pos(&w);
        apply_mode(&w, &app, UiMode::Island);
    }
}

/// 自绘拖动：按下卡片后由独立线程按光标位移移动窗口，松手即停。
#[tauri::command]
fn begin_drag(app: tauri::AppHandle) {
    use windows::Win32::Foundation::{HWND, POINT};
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetCursorPos, SetWindowPos, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER,
    };
    if mode_from_code(MODE.load(Ordering::Relaxed)) != UiMode::Expanded {
        return;
    }
    let Some(w) = app.get_webview_window("main") else { return };
    let (cw, ch) = card_size(&w);
    let settled = w
        .inner_size()
        .map(|s| (s.width as i32 - cw).abs() <= 4 && (s.height as i32 - ch).abs() <= 4)
        .unwrap_or(false);
    if !settled || DRAGGING.swap(true, Ordering::SeqCst) {
        return;
    }
    let (Ok(raw), Ok(pos)) = (w.hwnd(), w.outer_position()) else {
        DRAGGING.store(false, Ordering::SeqCst);
        return;
    };
    let raw = raw.0 as isize;
    std::thread::spawn(move || {
        let hwnd = HWND(raw as *mut std::ffi::c_void);
        let mut start = POINT::default();
        let _ = unsafe { GetCursorPos(&mut start) };
        let (mut lx, mut ly) = (pos.x, pos.y);
        loop {
            let down = unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) } as u16 & 0x8000 != 0;
            if !down {
                break;
            }
            let mut c = POINT::default();
            if unsafe { GetCursorPos(&mut c) }.is_err() {
                break;
            }
            let x = pos.x + (c.x - start.x);
            let y = pos.y + (c.y - start.y);
            if x != lx || y != ly {
                let _ = unsafe {
                    SetWindowPos(
                        hwnd,
                        None,
                        x,
                        y,
                        0,
                        0,
                        SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                    )
                };
                lx = x;
                ly = y;
            }
            std::thread::sleep(Duration::from_millis(8));
        }
        DRAGGING.store(false, Ordering::SeqCst);
    });
}

#[tauri::command]
fn expand_temporarily(app: tauri::AppHandle) {
    MANUAL_ISLAND.store(false, Ordering::Relaxed);
    if let Ok(mut g) = EXPAND_UNTIL.lock() {
        *g = Some(std::time::Instant::now() + Duration::from_secs(8));
    }
    unsafe {
        PIN_FG.store(
            windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow().0 as isize,
            Ordering::Relaxed,
        );
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
        .invoke_handler(tauri::generate_handler![
            app_ready,
            expand_temporarily,
            collapse_to_island,
            begin_drag,
            island_hover
        ])
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

            if let (Ok(raw), Ok(size)) = (win.hwnd(), win.inner_size()) {
                let (w, h) = if size.width > 0 && size.height > 0 {
                    (size.width as i32, size.height as i32)
                } else {
                    card_size(&win)
                };
                anim::start(raw.0 as isize, anim::Rect::new(home.0, home.1, w, h));
            }

            if want_locked {
                LOCKED.store(true, Ordering::Relaxed);
            }
            if LOCKED.load(Ordering::Relaxed) {
                let _ = win.set_ignore_cursor_events(true);
            }

            tune_window(&win);
            #[cfg(debug_assertions)]
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
                let mut gate = ModeGate::new();
                loop {
                    std::thread::sleep(Duration::from_millis(260));
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

                    let kind = classify_fg(&class);
                    let pinned = EXPAND_UNTIL
                        .lock()
                        .ok()
                        .and_then(|g| *g)
                        .map(|t| t > std::time::Instant::now())
                        .unwrap_or(false);
                    let pinned_here = pinned
                        && (kind != FgKind::App
                            || (PIN_FG.load(Ordering::Relaxed) != 0
                                && fg.0 as isize == PIN_FG.load(Ordering::Relaxed)));

                    // MODE 由 apply_mode 维护，expand_temporarily 也会改，
                    // 用共享状态判断才不会在展开后再补一次切换动画。
                    let current = mode_from_code(MODE.load(Ordering::Relaxed));

                    // 前台窗口压住顶部居中时躲到没被覆盖的一角；只是平移窗口，
                    // 不换内容。任务栏/Alt+Tab 覆盖层保持当前挂载点，避免来回跳。
                    let next_anchor = if kind == FgKind::App {
                        match fg_window_rect(fg) {
                            Some(rect) => {
                                pick_anchor(Some(rect), island_center_rect(&w), screen_rect())
                            }
                            // 阴影/离屏辅助窗口拿不到可信矩形：保持当前挂载点，
                            // 不要被它拽回中间再弹出去
                            None => island_anchor(),
                        }
                    } else if kind == FgKind::Desktop {
                        IslandAnchor::Center
                    } else {
                        island_anchor()
                    };
                    if next_anchor != island_anchor() {
                        ISLAND_ANCHOR.store(anchor_code(next_anchor), Ordering::Relaxed);
                        log_line(&format!("anchor {:?} fg={class}", next_anchor));
                        if current == UiMode::Island && w.is_visible().unwrap_or(false) {
                            anim::animate_to(
                                island_rect(&w, ISLAND_HOT.load(Ordering::Relaxed)),
                                DODGE_MOTION,
                            );
                        }
                    }

                    let want = gate.decide(
                        std::time::Instant::now(),
                        FgSnapshot {
                            kind,
                            fullscreen: kind == FgKind::App && fg_is_fullscreen(fg),
                            manual_island: MANUAL_ISLAND.load(Ordering::Relaxed),
                            pinned_here,
                            already_expanded: current == UiMode::Expanded,
                        },
                    );
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
                        set_webview_active(&fb, true);
                        let _ = w.show();
                        // 与 app_ready 路径对齐：补 exstyle 与内容形态
                        tune_window(&w);
                        let _ = fb.emit("net-mode", mode_str(mode_from_code(MODE.load(Ordering::Relaxed))));
                    }
                }
            });

            // 采样线程。窗口隐藏（托盘隐藏 / 全屏应用）时前端根本不画，
            // 照样每秒跨进程序列化整份 Sample 只会白烧 WebView 的 CPU；
            // 采样本身保留，计数差分与隐藏前的基线不断档。
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                let mut sampler = netmon::Sampler::new();
                sampler.sample(true);
                std::thread::sleep(Duration::from_millis(1000));
                loop {
                    let vis = window_visible(&handle);
                    let s = sampler.sample(vis);
                    if vis {
                        let _ = handle.emit("net-sample", &s);
                    }
                    std::thread::sleep(Duration::from_millis(1000));
                }
            });

            // 探针线程，两种节奏：
            //   本地状态（代理端口/加速器/加速中）2s 一刷——全是本机调用、零外网开销，
            //     开/停代理、切换网络 ≤3s 反映到呼吸灯和状态行；
            //   外网时延（直连握手）10s 一轮，电池下不做，避免把网卡从省电态拉起。
            // 经代理的 CONNECT 探测只在端口出现/更换或外网轮次时重测，
            // 不让本地代理每 2s 替我们连一次 github。
            // 隐藏时不做任何探测，重新显示后立即补上。
            let probe_handle = app.handle().clone();
            std::thread::spawn(move || {
                let mut cached: Option<probe::ProbePayload> = None;
                let mut last_local: Option<Instant> = None;
                let mut last_external: Option<Instant> = None;
                let mut last_port: Option<u16> = None;
                std::thread::sleep(Duration::from_secs(3));
                loop {
                    let visible = window_visible(&probe_handle);
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
                                p.via = p
                                    .proxy
                                    .port
                                    .map(|port| probe::http_connect_probe(port, "github.com", 443, 5000));
                            }
                            p
                        };
                        let mut p = p;
                        p.battery = battery;
                        last_port = p.proxy.port;
                        let _ = probe_handle.emit("net-probe", &p);
                        last_local = Some(Instant::now());
                    }
                    std::thread::sleep(Duration::from_secs(if visible { 1 } else { 2 }));
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

#[cfg(test)]
mod tests {
    use super::*;

    // 前台轮询的真实节奏
    const TICK: Duration = Duration::from_millis(260);

    fn snap(kind: FgKind) -> FgSnapshot {
        FgSnapshot {
            kind,
            fullscreen: false,
            manual_island: false,
            pinned_here: false,
            already_expanded: false,
        }
    }

    fn run(gate: &mut ModeGate, t0: Instant, classes: &[&str]) -> Vec<UiMode> {
        classes
            .iter()
            .enumerate()
            .map(|(i, c)| {
                gate.decide(
                    t0 + TICK * i as u32,
                    FgSnapshot {
                        already_expanded: false,
                        ..snap(classify_fg(c))
                    },
                )
            })
            .collect()
    }

    // 复现日志里的真实序列：点击任务栏图标 / Alt+Tab 时，
    // Shell_TrayWnd、Alt+Tab 覆盖层会短暂出现在前台。
    #[test]
    fn taskbar_click_and_alt_tab_never_expand() {
        let t0 = Instant::now();
        let mut g = ModeGate::new();
        let modes = run(
            &mut g,
            t0,
            &[
                "Chrome_WidgetWin_1",
                "Chrome_WidgetWin_1",
                "Shell_TrayWnd",
                "Chrome_WidgetWin_1",
                "XamlExplorerHostIslandWindow",
                "Chrome_WidgetWin_1",
            ],
        );
        assert!(
            modes.iter().all(|m| *m == UiMode::Island),
            "灵动岛不应在切换页面时展开成卡片：{modes:?}"
        );
    }

    #[test]
    fn taskbar_focus_for_seconds_never_expands() {
        let t0 = Instant::now();
        let mut g = ModeGate::new();
        let modes = run(&mut g, t0, &["Shell_TrayWnd"; 20]);
        assert!(modes.iter().all(|m| *m == UiMode::Island), "{modes:?}");
    }

    #[test]
    fn taskbar_is_not_desktop() {
        assert_eq!(classify_fg("Shell_TrayWnd"), FgKind::ShellTransient);
        assert_eq!(classify_fg("Progman"), FgKind::Desktop);
        assert_eq!(classify_fg("Chrome_WidgetWin_1"), FgKind::App);
    }

    #[test]
    fn alt_tab_overlay_is_not_a_fullscreen_app() {
        let mut g = ModeGate::new();
        let m = g.decide(
            Instant::now(),
            FgSnapshot {
                kind: classify_fg("XamlExplorerHostIslandWindow"),
                fullscreen: true,
                ..snap(FgKind::App)
            },
        );
        assert_ne!(m, UiMode::Hidden, "Alt+Tab 覆盖层不应让胶囊消失");
    }

    #[test]
    fn desktop_must_settle_two_seconds() {
        let t0 = Instant::now();
        let mut g = ModeGate::new();
        let modes = run(&mut g, t0, &["Progman"; 9]);
        assert!(
            modes[..8].iter().all(|m| *m == UiMode::Island),
            "桌面不足 2 秒不应展开：{modes:?}"
        );
        assert_eq!(modes[8], UiMode::Expanded, "桌面持续 2 秒后才展开");
    }

    #[test]
    fn desktop_interrupted_resets_timer() {
        let t0 = Instant::now();
        let mut g = ModeGate::new();
        let modes = run(
            &mut g,
            t0,
            &[
                "Progman",
                "Progman",
                "Progman",
                "Chrome_WidgetWin_1",
                "Progman",
                "Progman",
                "Progman",
                "Progman",
                "Progman",
                "Progman",
                "Progman",
                "Progman",
                "Progman",
            ],
        );
        assert!(
            modes[..12].iter().all(|m| *m == UiMode::Island),
            "中途切到应用后必须重新计时：{modes:?}"
        );
        assert_eq!(modes[12], UiMode::Expanded);
    }

    #[test]
    fn shell_transient_keeps_expanded_desktop() {
        let t0 = Instant::now();
        let mut g = ModeGate::new();
        let modes = run(
            &mut g,
            t0,
            &[
                "Progman",
                "Progman",
                "Progman",
                "Progman",
                "Progman",
                "Progman",
                "Progman",
                "Progman",
                "Progman",
                "Shell_TrayWnd",
                "XamlExplorerHostIslandWindow",
                "Chrome_WidgetWin_1",
            ],
        );
        assert_eq!(modes[8], UiMode::Expanded);
        assert_eq!(modes[9], UiMode::Expanded, "点任务栏不应把卡片打回胶囊");
        assert_eq!(modes[10], UiMode::Expanded);
        assert_eq!(modes[11], UiMode::Island, "切到真实应用才收回");
    }

    #[test]
    fn manual_expand_is_immediate() {
        let mut g = ModeGate::new();
        let m = g.decide(
            Instant::now(),
            FgSnapshot {
                pinned_here: true,
                ..snap(FgKind::App)
            },
        );
        assert_eq!(m, UiMode::Expanded, "点击灵动岛必须立即展开");
    }

    #[test]
    fn manual_collapse_survives_desktop() {
        let t0 = Instant::now();
        let mut g = ModeGate::new();
        for i in 0..20 {
            let m = g.decide(
                t0 + TICK * i,
                FgSnapshot {
                    manual_island: true,
                    ..snap(FgKind::Desktop)
                },
            );
            assert_eq!(m, UiMode::Island, "双击收回后不应被桌面抢回去");
        }
    }

    #[test]
    fn startup_expanded_on_desktop_stays_expanded() {
        let t0 = Instant::now();
        let mut g = ModeGate::new();
        for i in 0..8 {
            let m = g.decide(
                t0 + TICK * i,
                FgSnapshot {
                    already_expanded: true,
                    ..snap(FgKind::Desktop)
                },
            );
            assert_eq!(m, UiMode::Expanded, "冷启动已展开时不应先收回再弹出");
        }
    }

    fn rect(x: i32, y: i32, w: i32, h: i32) -> anim::Rect {
        anim::Rect::new(x, y, w, h)
    }

    #[test]
    fn island_stays_centered_when_top_is_free() {
        let screen = rect(0, 0, 2560, 1600);
        let island = rect(1208, 0, 144, 51);
        // 窗口没顶到屏幕顶部：岛下面就是壁纸，不需要躲
        assert_eq!(
            pick_anchor(Some(rect(100, 300, 1200, 900)), island, screen),
            IslandAnchor::Center
        );
        assert_eq!(pick_anchor(None, island, screen), IslandAnchor::Center);
    }

    #[test]
    fn maximized_window_dodges_left_by_default() {
        // 两侧都没有空位时选左上：右上角是窗口的最小化/关闭按钮
        let screen = rect(0, 0, 2560, 1600);
        let island = rect(1208, 0, 144, 51);
        assert_eq!(
            pick_anchor(Some(rect(-8, -8, 2576, 1616)), island, screen),
            IslandAnchor::Left
        );
    }

    #[test]
    fn window_on_left_dodges_right() {
        let screen = rect(0, 0, 2560, 1600);
        let island = rect(1208, 0, 144, 51);
        assert_eq!(
            pick_anchor(Some(rect(0, 0, 1700, 1200)), island, screen),
            IslandAnchor::Right
        );
    }

    #[test]
    fn window_on_right_dodges_left() {
        let screen = rect(0, 0, 2560, 1600);
        let island = rect(1208, 0, 144, 51);
        assert_eq!(
            pick_anchor(Some(rect(860, 0, 1700, 1200)), island, screen),
            IslandAnchor::Left
        );
    }

    #[test]
    fn narrow_free_strips_still_pick_the_larger_side() {
        let screen = rect(0, 0, 2560, 1600);
        let island = rect(1208, 0, 144, 51);
        // 两侧空位都塞不下 144px，选空得多的那一侧
        assert_eq!(
            pick_anchor(Some(rect(20, 0, 2480, 1200)), island, screen),
            IslandAnchor::Right
        );
        assert_eq!(
            pick_anchor(Some(rect(60, 0, 2480, 1200)), island, screen),
            IslandAnchor::Left
        );
    }

    #[test]
    fn fullscreen_app_hides_and_returns() {
        let t0 = Instant::now();
        let mut g = ModeGate::new();
        let hidden = g.decide(
            t0,
            FgSnapshot {
                fullscreen: true,
                ..snap(FgKind::App)
            },
        );
        assert_eq!(hidden, UiMode::Hidden);
        let back = g.decide(
            t0 + TICK,
            FgSnapshot {
                fullscreen: false,
                ..snap(FgKind::App)
            },
        );
        assert_eq!(back, UiMode::Island);
    }
}
