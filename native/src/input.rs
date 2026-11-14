//! 鼠标输入的纯判定：双击（卡片→胶囊）与拖动阈值（拖动 ≠ 点击）。
//!
//! 窗口类没有 `CS_DBLCLKS`（NOACTIVATE 的悬浮窗不需要），所以双击只能自己数：
//! 两次左键抬起在时间窗与位移窗内即算双击。拖动同理——按下到抬起位移超过
//! 阈值才算拖动，否则仍按点击处理（胶囊点一下展开）。
//!
//! 这里不碰 Win32，时间由调用方以毫秒注入，因此可直接单测。

use crate::mode::UiMode;

/// 双击的时间窗（Windows 默认 500 ms）。
pub const DOUBLE_CLICK_MS: u64 = 500;
/// 双击允许的位移（物理 px）：手抖不能算两次不同位置。
pub const DOUBLE_CLICK_PX: i32 = 6;
/// 按下到抬起的位移超过它才算拖动（物理 px）。
pub const DRAG_THRESHOLD_PX: i32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Click {
    pub at_ms: u64,
    pub x: i32,
    pub y: i32,
}

/// 这次抬起是否构成双击（并与上一次点击比较）。返回 true 时调用方应清掉记录，
/// 让连击不会把第三次也算成双击。
pub fn is_double_click(last: Option<Click>, now: Click) -> bool {
    let Some(prev) = last else {
        return false;
    };
    let dt = now.at_ms.saturating_sub(prev.at_ms);
    let dx = (now.x - prev.x).abs();
    let dy = (now.y - prev.y).abs();
    dt <= DOUBLE_CLICK_MS && dx <= DOUBLE_CLICK_PX && dy <= DOUBLE_CLICK_PX
}

/// 位移是否够得上拖动。
pub fn is_drag(start: (i32, i32), cur: (i32, i32)) -> bool {
    (cur.0 - start.0).abs() > DRAG_THRESHOLD_PX || (cur.1 - start.1).abs() > DRAG_THRESHOLD_PX
}

/// 一次左键抬起要执行的动作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClickAction {
    /// 双击卡片：手动收回顶部胶囊。
    CollapseCard,
    /// 单击胶囊：展开并 pin。
    ExpandIsland,
}

/// 点击语义由**手势起点形态**决定：双击的判定看第一击发生在哪一态。
///
/// 第一击在胶囊上会立刻把形变目标改成卡片，若第二击按「当前目标形态」判定，
/// 就会被误判成「卡片双击」而把刚展开的卡片收回去。因此第二击沿用第一击的形态。
pub fn click_action(mode_at_gesture_start: UiMode, dbl: bool) -> Option<ClickAction> {
    match (mode_at_gesture_start, dbl) {
        (UiMode::Expanded, true) => Some(ClickAction::CollapseCard),
        (UiMode::Island, false) => Some(ClickAction::ExpandIsland),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn click(ms: u64, x: i32, y: i32) -> Click {
        Click { at_ms: ms, x, y }
    }

    #[test]
    fn two_quick_clicks_at_same_spot_are_a_double_click() {
        let first = click(1000, 500, 300);
        assert!(is_double_click(Some(first), click(1200, 502, 301)));
        assert!(is_double_click(Some(first), click(1500, 500, 300)), "500 ms 边界内");
    }

    #[test]
    fn slow_or_far_clicks_are_not_double_clicks() {
        let first = click(1000, 500, 300);
        assert!(!is_double_click(Some(first), click(1600, 500, 300)), "超时不算");
        assert!(!is_double_click(Some(first), click(1100, 540, 300)), "位移过大不算");
        assert!(!is_double_click(None, click(1100, 500, 300)), "第一次点击不算");
    }

    #[test]
    fn third_click_after_a_double_click_starts_over() {
        // 调用方在判定成功后清空记录，所以第三次不会连成第二组双击
        let first = click(1000, 500, 300);
        let second = click(1150, 500, 300);
        assert!(is_double_click(Some(first), second));
        assert!(!is_double_click(None, click(1250, 500, 300)));
    }

    #[test]
    fn tiny_jitter_is_still_a_click_not_a_drag() {
        assert!(!is_drag((500, 300), (502, 301)));
        assert!(!is_drag((500, 300), (503, 303)));
        assert!(is_drag((500, 300), (504, 300)));
        assert!(is_drag((500, 300), (500, 320)));
    }

    #[test]
    fn double_click_started_on_island_does_not_collapse_the_card() {
        // 第一击在胶囊上已经展开，第二击沿用起点形态：不再触发收回
        assert_eq!(click_action(UiMode::Island, true), None);
    }

    #[test]
    fn double_click_on_card_collapses_to_island() {
        assert_eq!(
            click_action(UiMode::Expanded, true),
            Some(ClickAction::CollapseCard)
        );
    }

    #[test]
    fn single_click_on_island_expands() {
        assert_eq!(
            click_action(UiMode::Island, false),
            Some(ClickAction::ExpandIsland)
        );
    }

    #[test]
    fn single_click_on_card_is_a_no_op() {
        assert_eq!(click_action(UiMode::Expanded, false), None);
    }
}
