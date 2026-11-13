//! 全局快捷键（P4-2）：候选序与锁定菜单文案。
//!
//! 对照 `src-tauri/src/main.rs:876-884`：`Ctrl+Alt+L` 可能被别的程序占用，
//! 按候选顺序注册，取第一个成功的；菜单里显示**实际注册到**的那个。

use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT,
};

pub struct Hotkey {
    /// 菜单里显示的名字（与现版完全一致）。
    pub label: &'static str,
    pub mods: HOT_KEY_MODIFIERS,
    pub vk: u32,
}

/// VK 码（Win32 固定值，不依赖 crate 常量名）。
const VK_L: u32 = 0x4C;
const VK_N: u32 = 0x4E;
const VK_F9: u32 = 0x78;

/// 候选序与现版逐字一致。MOD_NOREPEAT 对齐现版"只在按下时切换一次"的语义。
pub static CANDIDATES: [Hotkey; 4] = [
    Hotkey {
        label: "Ctrl+Alt+L",
        mods: HOT_KEY_MODIFIERS(MOD_CONTROL.0 | MOD_ALT.0 | MOD_NOREPEAT.0),
        vk: VK_L,
    },
    Hotkey {
        label: "Ctrl+Alt+N",
        mods: HOT_KEY_MODIFIERS(MOD_CONTROL.0 | MOD_ALT.0 | MOD_NOREPEAT.0),
        vk: VK_N,
    },
    Hotkey {
        label: "Ctrl+Shift+L",
        mods: HOT_KEY_MODIFIERS(MOD_CONTROL.0 | MOD_SHIFT.0 | MOD_NOREPEAT.0),
        vk: VK_L,
    },
    Hotkey {
        label: "Ctrl+Alt+F9",
        mods: HOT_KEY_MODIFIERS(MOD_CONTROL.0 | MOD_ALT.0 | MOD_NOREPEAT.0),
        vk: VK_F9,
    },
];

/// 按候选序取第一个注册成功的。`try_register` 返回是否成功（Win32 侧是
/// `RegisterHotKey` 的结果；单测里注入假实现）。
pub fn pick<F>(mut try_register: F) -> Option<&'static Hotkey>
where
    F: FnMut(&Hotkey) -> bool,
{
    CANDIDATES.iter().find(|c| try_register(c))
}

/// 托盘菜单的锁定项文案（含实际注册到的快捷键名）。
pub fn lock_label(hotkey: Option<&Hotkey>) -> String {
    match hotkey {
        Some(h) => format!("锁定 / 解锁（{}）", h.label),
        None => "锁定 / 解锁（快捷键不可用）".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_keep_the_webview_order() {
        let labels: Vec<&str> = CANDIDATES.iter().map(|c| c.label).collect();
        assert_eq!(
            labels,
            vec!["Ctrl+Alt+L", "Ctrl+Alt+N", "Ctrl+Shift+L", "Ctrl+Alt+F9"]
        );
    }

    #[test]
    fn every_candidate_has_ctrl_and_no_repeat() {
        for c in CANDIDATES.iter() {
            assert_ne!(c.mods.0 & MOD_CONTROL.0, 0, "{}", c.label);
            assert_ne!(c.mods.0 & MOD_NOREPEAT.0, 0, "{}", c.label);
        }
    }

    #[test]
    fn pick_takes_the_first_that_registers() {
        // 只让第二个成功 → 选中 Ctrl+Alt+N（不是"最想要"的 L）。
        let picked = pick(|c| c.label == "Ctrl+Alt+N").expect("second candidate");
        assert_eq!(picked.label, "Ctrl+Alt+N");
    }

    #[test]
    fn pick_stops_trying_after_success() {
        let mut tried = 0;
        let picked = pick(|_| {
            tried += 1;
            true
        });
        assert_eq!(picked.map(|h| h.label), Some("Ctrl+Alt+L"));
        assert_eq!(tried, 1, "成功之后不该再试后面的候选");
    }

    #[test]
    fn pick_returns_none_when_all_are_taken() {
        assert!(pick(|_| false).is_none());
    }

    #[test]
    fn lock_label_names_the_registered_hotkey() {
        let h = pick(|c| c.label == "Ctrl+Shift+L").unwrap();
        assert_eq!(lock_label(Some(h)), "锁定 / 解锁（Ctrl+Shift+L）");
        assert_eq!(lock_label(None), "锁定 / 解锁（快捷键不可用）");
    }
}
