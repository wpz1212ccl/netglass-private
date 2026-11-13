//! 形态状态机与几何布局（从 `src-tauri/src/main.rs` 提取）。
//!
//! 这里只放纯决策逻辑：时间由调用方注入，屏幕/位置/缩放由调用方传入，
//! 因此 260 ms 前台轮询的整套行为都能在 `cargo test` 里逐拍断言。
//! Win32 查询（GetForegroundWindow / GetWindowRect / 显示器枚举）留在 main.rs。

use std::time::{Duration, Instant};

use crate::anim;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum UiMode {
    Expanded,
    Island,
    Hidden,
}

pub fn mode_code(mode: UiMode) -> u8 {
    match mode {
        UiMode::Expanded => 0,
        UiMode::Island => 1,
        UiMode::Hidden => 2,
    }
}

pub fn mode_from_code(code: u8) -> UiMode {
    match code {
        1 => UiMode::Island,
        2 => UiMode::Hidden,
        _ => UiMode::Expanded,
    }
}

/// 前台窗口类别。任务栏、Alt+Tab 覆盖层、托盘溢出面板是 Shell 的临时 UI：
/// 它们会短暂抢走前台，但不代表用户离开了桌面或切换了页面。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FgKind {
    Desktop,
    ShellTransient,
    App,
}

pub fn classify_fg(class: &str) -> FgKind {
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
pub struct FgSnapshot {
    pub kind: FgKind,
    pub fullscreen: bool,
    pub manual_island: bool,
    pub pinned_here: bool,
    pub already_expanded: bool,
}

/// 桌面连续在前台多久才自动展开。短于这个值的前台变化（点任务栏、
/// Alt+Tab、最小化动画）都会让计时作废，避免胶囊与卡片来回抽动。
pub const DESKTOP_SETTLE: Duration = Duration::from_secs(2);

/// 前台轮询的真实节奏（260 ms，D3 维持未批准）。
pub const POLL: Duration = Duration::from_millis(260);

/// 前台窗口 → 形态的纯决策状态机。时间由调用方注入，便于按 260ms 轮询节奏单测。
pub struct ModeGate {
    desktop_since: Option<Instant>,
}

impl ModeGate {
    pub fn new() -> Self {
        Self {
            desktop_since: None,
        }
    }

    pub fn decide(&mut self, now: Instant, s: FgSnapshot) -> UiMode {
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

/// 灵动岛挂载点。默认顶部居中；前台窗口压住这个位置时躲到它没盖住的一侧。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IslandAnchor {
    Center,
    Left,
    Right,
}

// 逻辑像素（DIP）尺寸，全部按 scale 放大成物理像素后落窗。
pub const CARD_W: f64 = 320.0;
pub const CARD_H: f64 = 200.0;
pub const ISLAND_W: f64 = 144.0;
pub const ISLAND_H: f64 = 34.0;
// 悬浮时窗口弹簧放大到 162x42；窗口始终等于可见胶囊，没有透明留白。
pub const ISLAND_HOT_W: f64 = 162.0;
pub const ISLAND_HOT_H: f64 = 42.0;
/// 首次运行没有可用配置时，卡片中心落在屏幕的这个比例位置（上部居中）。
pub const HOME_CX: f64 = 0.50;
pub const HOME_CY: f64 = 0.215;
/// 躲让时离屏幕左右边缘的间距。
const EDGE: f64 = 8.0;

/// 一处几何上下文：缩放、屏幕矩形、桌面锚点、胶囊悬浮态与挂载点。
/// 所有方法都是纯函数，窗口/显示器查询由调用方在 main.rs 里做完再传进来。
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub scale: f32,
    pub screen: anim::Rect,
    pub home: (i32, i32),
    pub hot: bool,
    pub anchor: IslandAnchor,
}

impl Layout {
    pub fn card(&self) -> anim::Rect {
        let w = (CARD_W * self.scale as f64).round() as i32;
        let h = (CARD_H * self.scale as f64).round() as i32;
        anim::Rect::new(self.home.0, self.home.1, w, h)
    }

    /// 默认挂载位置（顶部居中、基础尺寸）。只用来判断"这个位置有没有被压住"，
    /// 不受悬浮放大和当前躲让位置影响。
    pub fn island_center(&self) -> anim::Rect {
        let w = (ISLAND_W * self.scale as f64).round() as i32;
        let h = (ISLAND_H * self.scale as f64).round() as i32;
        anim::Rect::new((self.screen.w - w) / 2, self.screen.y, w, h)
    }

    pub fn island(&self) -> anim::Rect {
        let (lw, lh) = if self.hot {
            (ISLAND_HOT_W, ISLAND_HOT_H)
        } else {
            (ISLAND_W, ISLAND_H)
        };
        let w = (lw * self.scale as f64).round() as i32;
        let h = (lh * self.scale as f64).round() as i32;
        let edge = (EDGE * self.scale as f64).round() as i32;
        let x = match self.anchor {
            IslandAnchor::Center => (self.screen.w - w) / 2,
            IslandAnchor::Left => self.screen.x + edge,
            IslandAnchor::Right => self.screen.x + self.screen.w - w - edge,
        };
        anim::Rect::new(x, self.screen.y, w, h)
    }

    pub fn target(&self, mode: UiMode) -> Option<anim::Rect> {
        match mode {
            UiMode::Expanded => Some(self.card()),
            UiMode::Island => Some(self.island()),
            UiMode::Hidden => None,
        }
    }
}

/// 默认桌面锚点：卡片中心落在显示器左上角加 (HOME_CX, HOME_CY) 比例的位移处。
pub fn default_home(monitor: anim::Rect, scale: f32) -> (i32, i32) {
    let w = (CARD_W * scale as f64).round() as i32;
    let h = (CARD_H * scale as f64).round() as i32;
    (
        monitor.x + (monitor.w as f64 * HOME_CX).round() as i32 - w / 2,
        monitor.y + (monitor.h as f64 * HOME_CY).round() as i32 - h / 2,
    )
}

/// 前台窗口矩形与"顶部居中"相交时选一个角落：
/// 哪侧没被窗口覆盖就躲哪侧；两侧都塞不下时躲空间大的那边，
/// 完全贴满（最大化）则躲左上——右上永远是窗口的最小化/关闭按钮。
pub fn pick_anchor(
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

/// 躲到屏幕角落：大位移纯平移。阻尼取高值，回弹再好看也不能冲出屏幕边缘。
pub const DODGE_MOTION: anim::Motion = anim::Motion::new(0.34, 0.92);

/// 展开：略软、带回弹（damping < 1）；收回：更快、几乎不过冲。
pub fn motion_for(mode: UiMode) -> anim::Motion {
    match mode {
        UiMode::Expanded => anim::Motion::delayed(0.42, 0.54, 90),
        UiMode::Island => anim::Motion::delayed(0.34, 0.62, 50),
        UiMode::Hidden => anim::Motion::new(0.28, 1.0),
    }
}

/// `--silent` 自启动：桌面稳定到展开之前不显窗（P1-3）。
/// 一旦显示过，`silent_pending` 由调用方清零，之后三态照常。
pub fn show_on_decide(mode: UiMode, silent_pending: bool) -> bool {
    match mode {
        UiMode::Hidden => false,
        UiMode::Expanded => true,
        UiMode::Island => !silent_pending,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 前台轮询的真实节奏
    const TICK: Duration = POLL;

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

    // ---- 以下为 T2 新增：布局与 --silent 显隐决策 ----

    fn layout(anchor: IslandAnchor, hot: bool) -> Layout {
        Layout {
            scale: 1.5,
            screen: rect(0, 0, 2560, 1600),
            home: (1000, 300),
            hot,
            anchor,
        }
    }

    #[test]
    fn card_rect_is_home_at_logical_size() {
        let l = layout(IslandAnchor::Center, false);
        assert_eq!(l.card(), rect(1000, 300, 480, 300));
        assert_eq!(l.target(UiMode::Expanded), Some(rect(1000, 300, 480, 300)));
        assert_eq!(l.target(UiMode::Hidden), None);
    }

    #[test]
    fn island_rect_follows_anchor_and_hot_size() {
        let center = layout(IslandAnchor::Center, false);
        assert_eq!(center.island(), rect((2560 - 216) / 2, 0, 216, 51));
        assert_eq!(center.island_center(), center.island());

        let left = layout(IslandAnchor::Left, false);
        assert_eq!(left.island(), rect(12, 0, 216, 51));

        let right = layout(IslandAnchor::Right, false);
        assert_eq!(right.island(), rect(2560 - 216 - 12, 0, 216, 51));

        let hot = layout(IslandAnchor::Center, true);
        assert_eq!(hot.island(), rect((2560 - 243) / 2, 0, 243, 63));
    }

    #[test]
    fn default_home_puts_card_center_at_fraction() {
        // 2560x1600 @1.5 => 卡片 480x300；中心落在 (1280, 344)
        assert_eq!(default_home(rect(0, 0, 2560, 1600), 1.5), (1040, 194));
        // 副屏有原点偏移时锚点跟着偏移
        assert_eq!(default_home(rect(2560, 0, 2560, 1600), 1.5), (3600, 194));
    }

    #[test]
    fn silent_start_hides_until_expanded() {
        assert!(!show_on_decide(UiMode::Hidden, false));
        assert!(!show_on_decide(UiMode::Island, true), "静默启动不先露胶囊");
        assert!(show_on_decide(UiMode::Expanded, true), "桌面稳定 2s 才展开");
        assert!(show_on_decide(UiMode::Island, false), "显示过后三态照常");
    }
}
