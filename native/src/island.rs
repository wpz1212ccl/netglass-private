//! 胶囊态（岛）的纯逻辑：呼吸相位、三态色、颜色插值、玻璃亮度。
//!
//! 这里只放不依赖 Win32/D2D 的决策，便于单测；绘制与定时器在 render.rs / main.rs。
//! 数值全部对照 `src/index.html` 的 `.mdot` / `breatheTick` / `.pillbg`。

use std::time::Duration;

/// 呼吸灯三态色（对照 `.mdot` / `.mdot.acc` / `.mdot.acc.cat`）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DotColor {
    Green,
    Orange,
    Red,
}

impl DotColor {
    pub fn rgb(self) -> (u8, u8, u8) {
        match self {
            DotColor::Green => (0x34, 0xC7, 0x59),
            DotColor::Orange => (0xFF, 0x9F, 0x0A),
            DotColor::Red => (0xFF, 0x3B, 0x30),
        }
    }

    /// box-shadow 光晕强度：绿 .35 / 橙 .5 / 红 .5。
    pub fn glow(self) -> f32 {
        match self {
            DotColor::Green => 0.35,
            DotColor::Orange => 0.5,
            DotColor::Red => 0.5,
        }
    }
}

/// 加速器状态 → 呼吸灯颜色。SakuraCat 优先（红），其它加速器橙，无加速绿。
pub fn dot_color(accelerating: &[String]) -> DotColor {
    if accelerating.iter().any(|a| a == "SakuraCat") {
        DotColor::Red
    } else if !accelerating.is_empty() {
        DotColor::Orange
    } else {
        DotColor::Green
    }
}

/// 2 s 呼吸周期：t 归一化后 s = 0.5 - 0.5cos(2πt)，
/// opacity = 1 - 0.45s，scale = 1 - 0.2s（对照 breatheTick）。
pub fn breathe_at(elapsed_ms: f64) -> (f32, f32) {
    let t = elapsed_ms.rem_euclid(2000.0) / 2000.0;
    let s = 0.5 - 0.5 * (t * std::f64::consts::TAU).cos();
    ((1.0 - 0.45 * s) as f32, (1.0 - 0.2 * s) as f32)
}

/// 预渲染的呼吸相位帧数：66 ms 一拍、2 s 周期约 30 拍，32 帧足够平滑。
pub const BREATHE_PHASES: u8 = 32;

/// 当前相位索引（0..BREATHE_PHASES），用于挑预渲染帧。
pub fn breathe_phase(elapsed_ms: f64) -> u8 {
    let t = elapsed_ms.rem_euclid(2000.0) / 2000.0;
    ((t * BREATHE_PHASES as f64) as u8).min(BREATHE_PHASES - 1)
}

/// 呼吸只在岛态、非电池、非隐藏、非形变时运行；其余情况回到静态点。
pub fn should_breathe(island: bool, battery: bool, hidden: bool, morph: bool) -> bool {
    island && !battery && !hidden && !morph
}

/// 当前呼吸相位表（66 ms 驱动 ≤15 fps）。
pub const BREATHE_TICK: Duration = Duration::from_millis(66);
/// 三态色插值时长（CSS transition .4s）。
pub const COLOR_FADE_SECS: f64 = 0.4;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Tint {
    pub rgb: (u8, u8, u8),
    pub glow: f32,
}

impl Tint {
    pub fn of(color: DotColor) -> Self {
        Self {
            rgb: color.rgb(),
            glow: color.glow(),
        }
    }
}

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    let t = t.clamp(0.0, 1.0);
    (a as f32 + (b as f32 - a as f32) * t).round().clamp(0.0, 255.0) as u8
}

/// 三态色 0.4 s 插值；中途改目标从当前插值继续，不跳变。
#[derive(Clone, Copy, Debug)]
pub struct TintTween {
    from: Tint,
    to: Tint,
    t: f32,
}

impl TintTween {
    pub fn new(color: DotColor) -> Self {
        let tint = Tint::of(color);
        Self {
            from: tint,
            to: tint,
            t: 1.0,
        }
    }

    pub fn set(&mut self, color: DotColor) {
        let target = Tint::of(color);
        let current = self.current();
        if target == current {
            self.from = target;
            self.to = target;
            self.t = 1.0;
            return;
        }
        self.from = current;
        self.to = target;
        self.t = 0.0;
    }

    pub fn step(&mut self, dt_secs: f64) {
        if self.t < 1.0 {
            self.t = (self.t + (dt_secs / COLOR_FADE_SECS) as f32).min(1.0);
        }
    }

    pub fn current(&self) -> Tint {
        let t = self.t.clamp(0.0, 1.0);
        Tint {
            rgb: (
                lerp_u8(self.from.rgb.0, self.to.rgb.0, t),
                lerp_u8(self.from.rgb.1, self.to.rgb.1, t),
                lerp_u8(self.from.rgb.2, self.to.rgb.2, t),
            ),
            glow: self.from.glow + (self.to.glow - self.from.glow) * t,
        }
    }

    pub fn settled(&self) -> bool {
        self.t >= 1.0
    }
}

/// `.card.collapsed.hot .pillbg` brightness(1.05)；`:active` brightness(.965)。
/// active 优先：按下时给即时反馈，比悬停高光更重要。
pub fn glass_brightness(hot: bool, active: bool) -> f32 {
    if active {
        0.965
    } else if hot {
        1.05
    } else {
        1.0
    }
}

/// 胶囊圆角：CSS 21px 在 34px 高上会被按比例钳到 h/2（真胶囊），
/// 悬浮 42px 高时正好是 21px。
pub fn capsule_radius(w: f32, h: f32, scale: f32) -> f32 {
    let _ = w;
    (21.0 * scale).min(h / 2.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn dot_color_prefers_sakuracat() {
        assert_eq!(dot_color(&[]), DotColor::Green);
        assert_eq!(dot_color(&names(&["Watt"])), DotColor::Orange);
        assert_eq!(dot_color(&names(&["SakuraCat"])), DotColor::Red);
        assert_eq!(
            dot_color(&names(&["Watt", "SakuraCat"])),
            DotColor::Red,
            "SakuraCat 优先级最高"
        );
    }

    #[test]
    fn breathe_phase_matches_css_table() {
        let (o0, s0) = breathe_at(0.0);
        assert!(close(o0, 1.0) && close(s0, 1.0), "t=0 静态满亮");

        let (o500, s500) = breathe_at(500.0);
        assert!(close(o500, 0.775) && close(s500, 0.9), "t=0.25: {o500},{s500}");

        let (o1000, s1000) = breathe_at(1000.0);
        assert!(close(o1000, 0.55) && close(s1000, 0.8), "t=0.5: {o1000},{s1000}");

        let (o1500, s1500) = breathe_at(1500.0);
        assert!(close(o1500, 0.775) && close(s1500, 0.9));

        let (o2000, s2000) = breathe_at(2000.0);
        assert!(close(o2000, 1.0) && close(s2000, 1.0), "2 s 周期回到起点");
        let (o2500, s2500) = breathe_at(2500.0);
        assert!(close(o2500, 0.775) && close(s2500, 0.9), "相位按 2 s 取模");
    }

    #[test]
    fn breathe_phase_maps_cycle_to_tiles() {
        assert_eq!(breathe_phase(0.0), 0);
        assert_eq!(breathe_phase(1000.0), 16, "半周期");
        assert_eq!(breathe_phase(1999.0), 31, "末帧不越界");
        assert_eq!(breathe_phase(2000.0), 0, "2 s 回到首帧");
        assert_eq!(breathe_phase(2500.0), 8, "相位按 2 s 取模");
    }

    #[test]
    fn breathe_only_when_visible_island_on_ac() {
        assert!(should_breathe(true, false, false, false));
        assert!(!should_breathe(false, false, false, false), "展开/隐藏不呼吸");
        assert!(!should_breathe(true, true, false, false), "电池供电静态点");
        assert!(!should_breathe(true, false, true, false), "隐藏时暂停");
        assert!(!should_breathe(true, false, false, true), "形变期间暂停");
    }

    #[test]
    fn tint_tween_interpolates_over_400ms() {
        let mut tw = TintTween::new(DotColor::Green);
        assert_eq!(tw.current(), Tint::of(DotColor::Green));
        assert!(tw.settled());

        tw.set(DotColor::Red);
        assert!(!tw.settled());
        tw.step(0.2);
        let mid = tw.current();
        assert_eq!(mid.rgb, (154, 129, 69), "绿→红中点");
        assert!(close(mid.glow, 0.425));

        tw.step(0.2);
        assert_eq!(tw.current(), Tint::of(DotColor::Red));
        assert!(tw.settled());
    }

    #[test]
    fn tint_tween_retarget_continues_from_current() {
        let mut tw = TintTween::new(DotColor::Green);
        tw.set(DotColor::Red);
        tw.step(0.2);
        let before = tw.current();
        tw.set(DotColor::Green);
        assert_eq!(tw.current(), before, "改目标瞬间不跳色");
        assert!(!tw.settled(), "中途改目标重新计时");
        tw.step(0.2);
        // 从当前插值(154,129,69)往绿色走 50%
        assert_eq!(tw.current().rgb, (103, 164, 79));
        tw.step(0.2);
        assert_eq!(tw.current(), Tint::of(DotColor::Green));
    }

    #[test]
    fn glass_brightness_follows_css() {
        assert!(close(glass_brightness(false, false), 1.0));
        assert!(close(glass_brightness(true, false), 1.05), "hover 提亮 5%");
        assert!(close(glass_brightness(false, true), 0.965), "按下微暗 3.5%");
        assert!(close(glass_brightness(true, true), 0.965), "active 优先");
    }

    #[test]
    fn capsule_radius_clamps_to_half_height() {
        assert!(close(capsule_radius(216.0, 51.0, 1.5), 25.5));
        assert!(close(capsule_radius(243.0, 63.0, 1.5), 31.5));
        assert!(close(capsule_radius(144.0, 34.0, 1.0), 17.0), "21px 被钳到 h/2");
        assert!(close(capsule_radius(162.0, 42.0, 1.0), 21.0));
        assert!(close(capsule_radius(200.0, 34.0, 1.0), 17.0));
    }
}
