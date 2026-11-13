//! 锁定状态（P4-2）：快捷键 / 托盘菜单切换，跨重启保持。
//!
//! 语义对照现版 `set_ignore_cursor_events(true)`：锁定后鼠标事件穿透窗口，
//! 点击落到下面的桌面/应用上；窗口仍然可见、仍然刷新数字。
//!
//! 原生实现用 `WS_EX_TRANSPARENT`（命中测试直接跳过本窗口），而不是在
//! wndproc 里吞消息——吞消息只挡得住已经命中的事件，拖动/悬停判定仍会
//! 抢走光标形状；`WS_EX_TRANSPARENT` 是系统级的"这里没有窗口"。
//! 取舍：`WS_EX_TRANSPARENT` 同样挡掉右键退出，所以锁定期间必须靠托盘菜单
//! 或快捷键解锁（现版锁定后也只能从托盘解锁，行为一致）。

/// 锁定状态的唯一来源（UI 线程读写，纯值语义便于单测）。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Lock {
    on: bool,
}

impl Lock {
    pub fn new(on: bool) -> Self {
        Self { on }
    }

    pub fn is_on(self) -> bool {
        self.on
    }

    /// 切换并返回新状态（托盘菜单与快捷键共用同一条路径）。
    pub fn toggle(&mut self) -> bool {
        self.on = !self.on;
        self.on
    }

}

/// 状态行文案：现版前端把 `已锁定` 拼在连接状态后面（`index.html:478/654`）。
pub fn state_text(conn: &str, locked: bool) -> String {
    if locked {
        format!("{conn} · 已锁定")
    } else {
        conn.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_flips_and_reports_the_new_state() {
        let mut l = Lock::default();
        assert!(!l.is_on());
        assert!(l.toggle());
        assert!(l.is_on());
        assert!(!l.toggle());
        assert!(!l.is_on());
    }

    #[test]
    fn state_line_matches_the_webview_text() {
        assert_eq!(state_text("已连接", false), "已连接");
        assert_eq!(state_text("已连接", true), "已连接 · 已锁定");
    }
}
