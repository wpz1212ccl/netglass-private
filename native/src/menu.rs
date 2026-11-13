//! 托盘菜单的**模型**（纯逻辑）：项 ID、文案、勾选态。
//!
//! 渲染成 `CreatePopupMenu` + `Shell_NotifyIconW` 在 `tray.rs`；这里只保证
//! 菜单结构与现版一致（显示/隐藏、锁定/解锁+快捷键名、开机自启√、分隔、退出），
//! 因此顺序/ID/文案都能单测。

use crate::hotkey::{lock_label, Hotkey};

/// 菜单项 ID（`TrackPopupMenu(TPM_RETURNCMD)` 的返回值）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MenuAction {
    ToggleVisible,
    ToggleLock,
    ToggleAutostart,
    Quit,
}

/// ID 编码：100 起，与 Win32 菜单的 `wID` 一一对应。
pub const ID_TOGGLE: usize = 100;
pub const ID_LOCK: usize = 101;
pub const ID_AUTOSTART: usize = 102;
pub const ID_QUIT: usize = 103;

pub fn id_of(action: MenuAction) -> usize {
    match action {
        MenuAction::ToggleVisible => ID_TOGGLE,
        MenuAction::ToggleLock => ID_LOCK,
        MenuAction::ToggleAutostart => ID_AUTOSTART,
        MenuAction::Quit => ID_QUIT,
    }
}

/// `TrackPopupMenu` 返回 0（用户点空白处取消）时不要误判成某个动作。
pub fn action_of(id: usize) -> Option<MenuAction> {
    match id {
        ID_TOGGLE => Some(MenuAction::ToggleVisible),
        ID_LOCK => Some(MenuAction::ToggleLock),
        ID_AUTOSTART => Some(MenuAction::ToggleAutostart),
        ID_QUIT => Some(MenuAction::Quit),
        _ => None,
    }
}

/// 一项菜单：文案 + 是否勾选 + 是否分隔线前的最后一项（分隔由调用方插）。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Item {
    pub action: Option<MenuAction>,
    pub label: String,
    pub checked: bool,
}

/// 与现版同序：显示/隐藏、锁定/解锁（含快捷键名）、开机自启（勾选）、分隔、退出。
pub fn items(hotkey: Option<&Hotkey>, autostart: bool) -> Vec<Item> {
    vec![
        Item {
            action: Some(MenuAction::ToggleVisible),
            label: "显示 / 隐藏".to_string(),
            checked: false,
        },
        Item {
            action: Some(MenuAction::ToggleLock),
            label: lock_label(hotkey),
            checked: false,
        },
        Item {
            action: Some(MenuAction::ToggleAutostart),
            label: "开机自启".to_string(),
            checked: autostart,
        },
        Item {
            action: None, // 分隔线
            label: String::new(),
            checked: false,
        },
        Item {
            action: Some(MenuAction::Quit),
            label: "退出".to_string(),
            checked: false,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotkey::pick;

    #[test]
    fn menu_matches_the_webview_structure() {
        let h = pick(|_| true).unwrap();
        let it = items(Some(h), true);
        let labels: Vec<&str> = it.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(
            labels,
            vec!["显示 / 隐藏", "锁定 / 解锁（Ctrl+Alt+L）", "开机自启", "", "退出"]
        );
        assert!(it[2].checked, "开机自启勾选态跟随实际状态");
        assert!(it[3].action.is_none(), "第 4 项是分隔线");
    }

    #[test]
    fn autostart_check_follows_the_argument() {
        let h = pick(|_| true).unwrap();
        assert!(!items(Some(h), false)[2].checked);
    }

    #[test]
    fn lock_label_degrades_when_no_hotkey_registered() {
        let it = items(None, false);
        assert_eq!(it[1].label, "锁定 / 解锁（快捷键不可用）");
    }

    #[test]
    fn ids_roundtrip_and_zero_is_cancel() {
        for a in [
            MenuAction::ToggleVisible,
            MenuAction::ToggleLock,
            MenuAction::ToggleAutostart,
            MenuAction::Quit,
        ] {
            assert_eq!(action_of(id_of(a)), Some(a));
        }
        assert_eq!(action_of(0), None, "TrackPopupMenu 返回 0 表示取消");
        assert_eq!(action_of(999), None);
    }

}
