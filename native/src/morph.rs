//! 形变时间轴（P3-6）：内容退场 → 形状形变 → 内容进场。
//!
//! 对照 `src/index.html`：
//! - `net-morph`：内容立刻开始退场（`.morphing .inner/.mini` opacity .12s）
//! - 形状弹簧 `motion_for`：展开 0.42/0.54 delay 90 ms、收回 0.34/0.62 delay 50 ms
//!   ——delay 期间窗口还没动，正好让文字先淡出
//! - `net-mode`：**展开 360 ms / 收回 250 ms** 后内容进场
//! - `.card.out`：隐藏/显示 220 ms 淡出淡入
//!
//! 这里只有时间→数值的纯函数，重绘在 main.rs。

use crate::anim;
use crate::mode::UiMode;

/// 内容进场时长（`.inner`/`.mini` opacity .26s）。
pub const CONTENT_IN_MS: f64 = 260.0;
/// 隐藏/显示淡出淡入（`.card.out` opacity .2s，窗口侧按 220 ms 对齐）。
pub const FADE_MS: f64 = 220.0;
/// 内容进场的尺寸门槛：形变误差低于 1.5% 才把正文画回来。
/// 原生没有 WebView 的中间尺寸重排，正文只能按目标尺寸绘制；所以进场时
/// 顺带把最后这点残余几何吸附掉（≤1.5% ≈ 7 px，一帧的位移量，肉眼无感），
/// 既保证文字不被拉变形，也保证不会把 DIB 边界外的像素读出来。
pub const CONTENT_ENTER_ERR: f64 = 0.015;

/// `net-mode` 延迟：展开 360 ms、收回 250 ms。
pub fn enter_delay_ms(target: UiMode) -> f64 {
    match target {
        UiMode::Expanded => 360.0,
        _ => 250.0,
    }
}

/// 内容退场窗口 = 弹簧的 delay（展开 90 ms、收回 50 ms）。
///
/// CSS 是 `.morphing .inner { opacity: 0 }` 的 .12s 过渡，而弹簧靠 `delay_ms`
/// 制造"先慢后快"——两段本来就是同一段停顿。原生把退场压进 delay 窗口，
/// 好处是形状起步那一刻文字正好归零：既没有"文字还在动"的拖影，
/// 也不需要为两段动画各开一个定时器。
pub fn retreat_ms(target: UiMode) -> f64 {
    crate::mode::motion_for(target).delay_ms as f64
}

/// 退场不透明度：1 → 0，线性走完 delay 窗口。
pub fn fade_out_alpha(t_ms: f64, target: UiMode) -> f32 {
    let window = retreat_ms(target);
    if t_ms <= 0.0 || window <= 0.0 {
        return 1.0;
    }
    if t_ms >= window {
        return 0.0;
    }
    (1.0 - t_ms / window) as f32
}

/// 相对尺寸误差：当前矩形离目标还有多远（按面积差的最大边归一）。
pub fn size_error(cur: anim::Rect, target: anim::Rect) -> f64 {
    let dw = (cur.w - target.w).abs() as f64 / target.w.max(1) as f64;
    let dh = (cur.h - target.h).abs() as f64 / target.h.max(1) as f64;
    dw.max(dh)
}

/// 正文是否该回来了：时间过 `enter_delay` **且** 尺寸误差 ≤3%。
/// 两者都满足前，中间帧只有形状、没有文字（`.morphing` 语义）。
pub fn content_ready(t_ms: f64, target: UiMode, cur: anim::Rect, to: anim::Rect) -> bool {
    t_ms >= enter_delay_ms(target) && size_error(cur, to) <= CONTENT_ENTER_ERR
}

/// 进场不透明度：`elapsed_ms` 是**进场起点之后**的毫秒数（几何已落定那一刻）。
/// 按 CSS cubic-bezier(.6,0,.25,1) 在 260 ms 内从 0 升到 1。
pub fn content_in_alpha(elapsed_ms: f64) -> f32 {
    if elapsed_ms <= 0.0 {
        return 0.0;
    }
    let k = (elapsed_ms / CONTENT_IN_MS).clamp(0.0, 1.0) as f32;
    crate::fx::ease_morph(k)
}

/// 形状进度 0→1（0 = 卡片 squircle，1 = 胶囊）：按窗口宽度在两端之间插值，
/// 与弹簧的过冲同步（过冲时 >1，圆角继续朝胶囊收敛）。
pub fn shape_progress(from_w: i32, to_w: i32, w: i32) -> f32 {
    let span = (to_w - from_w) as f32;
    if span.abs() < 1.0 {
        return 1.0;
    }
    ((w - from_w) as f32 / span).clamp(0.0, 1.4)
}

/// 形变中的圆角半径（物理 px）：卡片 35 px squircle ↔ 胶囊 h/2。
/// 中间帧用普通圆角矩形降级（P3-1 允许），半径按进度插值。
pub fn morph_radius(t: f32, w: f32, h: f32, scale: f32) -> f32 {
    let card_r = crate::card::RADIUS * scale;
    let cap_r = (21.0 * scale).min(h / 2.0);
    let r = card_r + (cap_r - card_r) * t.clamp(0.0, 1.0);
    r.min(w / 2.0).min(h / 2.0)
}

/// 形变帧缓冲尺寸：卡片是最大形态，弹簧过冲实测 +7%（expand 480→514），
/// 留 16% 余量再 +8 px 兜住 AA 边带。
pub fn shape_buffer(scale: f32) -> (i32, i32) {
    let w = (crate::mode::CARD_W * scale as f64).round() as i32;
    let h = (crate::mode::CARD_H * scale as f64).round() as i32;
    (
        (w as f64 * 1.16).round() as i32 + 8,
        (h as f64 * 1.16).round() as i32 + 8,
    )
}

/// 形变中间帧的几何：从当前矩形推形状进度，再算圆角。
pub fn morph_frame_radius(r: anim::Rect, from_w: i32, to_w: i32, scale: f32) -> f32 {
    morph_radius(
        shape_progress(from_w, to_w, r.w),
        r.w as f32,
        r.h as f32,
        scale,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(w: i32, h: i32) -> anim::Rect {
        anim::Rect::new(0, 0, w, h)
    }

    #[test]
    fn enter_delay_matches_frontend_events() {
        assert_eq!(enter_delay_ms(UiMode::Expanded), 360.0);
        assert_eq!(enter_delay_ms(UiMode::Island), 250.0);
    }

    #[test]
    fn content_retreats_exactly_over_the_spring_delay() {
        // 展开：delay 90 ms，退场在形状起步那一刻刚好归零
        assert_eq!(fade_out_alpha(0.0, UiMode::Expanded), 1.0);
        assert!((fade_out_alpha(45.0, UiMode::Expanded) - 0.5).abs() < 0.01);
        assert_eq!(fade_out_alpha(90.0, UiMode::Expanded), 0.0);
        // 收回：delay 50 ms
        assert_eq!(retreat_ms(UiMode::Island), 50.0);
        assert_eq!(retreat_ms(UiMode::Expanded), 90.0);
        assert_eq!(fade_out_alpha(50.0, UiMode::Island), 0.0);
        assert_eq!(fade_out_alpha(400.0, UiMode::Expanded), 0.0);
    }

    #[test]
    fn content_fades_in_over_260ms_from_zero() {
        assert_eq!(content_in_alpha(0.0), 0.0);
        assert!(content_in_alpha(40.0) > 0.0, "进场立刻开始，不是先等再跳");
        assert!(content_in_alpha(40.0) < 0.35, "前段要慢（先慢后快曲线）");
        assert!(content_in_alpha(130.0) > 0.5);
        assert!(content_in_alpha(CONTENT_IN_MS) > 0.99);
        assert_eq!(content_in_alpha(5000.0), 1.0);
    }

    #[test]
    fn midframes_have_no_text() {
        // 退场画完之后、尺寸还没落定之前：只有形状
        let to = rect(480, 300);
        assert!(!content_ready(150.0, UiMode::Expanded, rect(300, 190), to));
        assert!(!content_ready(90.0, UiMode::Expanded, rect(217, 52), to), "形状刚起步");
        assert!(!content_ready(300.0, UiMode::Expanded, rect(400, 250), to));
        // 时间到了但尺寸还差 10%：仍然不进场
        assert!(!content_ready(400.0, UiMode::Expanded, rect(432, 270), to));
        // 还差 3% 也不进场（超出门槛）
        assert!(!content_ready(400.0, UiMode::Expanded, rect(466, 291), to));
        // 两个条件都满足才进场（误差 1.2%）
        assert!(content_ready(400.0, UiMode::Expanded, rect(474, 296), to));
    }

    #[test]
    fn island_enters_earlier_than_expand() {
        let to = rect(216, 51);
        // 250 ms 时胶囊已可进场（尺寸也基本到位）
        assert!(content_ready(251.0, UiMode::Island, rect(215, 51), to));
        // 同一时刻卡片还不行：时间不够
        assert!(!content_ready(251.0, UiMode::Expanded, rect(474, 296), rect(480, 300)));
    }

    #[test]
    fn size_error_is_relative_to_target() {
        assert!((size_error(rect(240, 150), rect(480, 300)) - 0.5).abs() < 1e-9);
        assert_eq!(size_error(rect(480, 300), rect(480, 300)), 0.0);
        // 过冲也是误差
        assert!(size_error(rect(511, 320), rect(480, 300)) > 0.06);
    }

    #[test]
    fn shape_progress_tracks_window_width() {
        // 216 → 480：中点附近进度 ≈ 0.5
        assert!((shape_progress(216, 480, 348) - 0.5).abs() < 0.01);
        assert_eq!(shape_progress(216, 480, 216), 0.0);
        assert_eq!(shape_progress(216, 480, 480), 1.0);
        // 弹簧过冲：进度可以 > 1，但不会失控
        assert!(shape_progress(216, 480, 520) > 1.0);
        assert!(shape_progress(216, 480, 520) <= 1.4);
    }

    #[test]
    fn radius_interpolates_between_squircle_and_capsule() {
        let scale = 1.5;
        // 卡片 480×300：35 DIP → 52.5 px
        assert!((morph_radius(0.0, 480.0, 300.0, scale) - 52.5).abs() < 0.01);
        // 胶囊 216×51：21 DIP 会被 h/2 钳到 25.5 px
        assert!((morph_radius(1.0, 216.0, 51.0, scale) - 25.5).abs() < 0.01);
        // 中间帧半径落在两端之间
        let mid = morph_radius(0.5, 348.0, 175.0, scale);
        assert!(mid > 25.5 && mid < 52.5, "{mid}");
    }

    #[test]
    fn shape_buffer_fits_spring_overshoot() {
        // 展开实测最大 514x332（480x300 的 +7%），缓冲必须装得下
        let (w, h) = shape_buffer(1.5);
        assert!(w >= 514 && h >= 332, "缓冲装不下过冲：{w}x{h}");
        // 缩放 1.0 时按比例缩小，仍然够用
        let (w1, h1) = shape_buffer(1.0);
        assert!(w1 >= 344 && h1 >= 224, "{w1}x{h1}");
    }

    #[test]
    fn morph_frame_radius_uses_current_rect() {
        // 收回：480 → 216，当前 348 宽时半径已在中间
        let r = morph_frame_radius(anim::Rect::new(0, 0, 348, 175), 480, 216, 1.5);
        assert!(r > 25.5 && r < 52.5, "{r}");
        // 终点就是胶囊半径
        let r = morph_frame_radius(anim::Rect::new(0, 0, 216, 51), 480, 216, 1.5);
        assert!((r - 25.5).abs() < 0.01);
    }
}
