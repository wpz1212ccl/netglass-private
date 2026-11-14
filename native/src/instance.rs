//! 单实例（P4-4）：`CreateMutexW` 判定 + 唤起既有窗。
//!
//! 二次启动不出第二个窗口：拿到互斥体名字后 `ERROR_ALREADY_EXISTS` 就说明
//! 已有实例，于是让既有窗口显示自己，然后本进程直接退出。
//! 判定与唤醒消息名是纯逻辑（单测），Win32 调用在下面。

use windows::core::{Result, PCWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, HANDLE, ERROR_ALREADY_EXISTS,
};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, PostMessageW, RegisterWindowMessageW, ShowWindow, SW_SHOWNA,
};

/// 互斥体名字。用户级单实例（不跨会话）。
pub const MUTEX_NAME: &str = "NetGlassNativeSingleInstance";
/// 唤起既有窗口用的广播消息名（`RegisterWindowMessageW`）。
pub const WAKE_MESSAGE: &str = "NetGlassNativeWake";
/// 窗口类名（与 `window::CLASS_NAME` 相同，`FindWindowW` 用）。
pub const WINDOW_CLASS: &str = "NetGlassNative";

/// 本次启动是第一个实例还是第二个。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Launch {
    First,
    Second,
}

/// `CreateMutexW` 之后 `GetLastError() == ERROR_ALREADY_EXISTS` 的判定。
pub fn decide(already_exists: bool) -> Launch {
    if already_exists {
        Launch::Second
    } else {
        Launch::First
    }
}

/// 第二实例的唤醒消息名（`RegisterWindowMessageW` 的入参）。
pub fn wake_message_name() -> &'static str {
    WAKE_MESSAGE
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 持住互斥体句柄：进程活着就一直持有。
pub struct SingleInstance {
    handle: HANDLE,
    launch: Launch,
}

impl SingleInstance {
    /// `None` = 创建互斥体失败（极少见）；调用方按"第一个实例"继续跑，
    /// 不能因为单实例机制坏了就完全起不来。
    pub fn acquire() -> Option<Self> {
        unsafe {
            // CreateMutexW 的 `Ok` 只表示拿到了句柄；是否"已存在"看 GetLastError。
            let handle = CreateMutexW(None, true, PCWSTR(wide(MUTEX_NAME).as_ptr())).ok()?;
            let launch = decide(GetLastError() == ERROR_ALREADY_EXISTS);
            Some(Self { handle, launch })
        }
    }

    pub fn launch(&self) -> Launch {
        self.launch
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

/// 唤起既有窗：找到类名匹配的窗口，先 `SW_SHOWNA`（不抢焦点地显示），
/// 再投递唤醒消息（UI 线程收到后按当前形态重新落位并置顶）。
pub fn wake_existing() -> Result<()> {
    unsafe {
        let hwnd = FindWindowW(PCWSTR(wide(WINDOW_CLASS).as_ptr()), PCWSTR::null())?;
        let _ = ShowWindow(hwnd, SW_SHOWNA);
        let msg = RegisterWindowMessageW(PCWSTR(wide(WAKE_MESSAGE).as_ptr()));
        if msg != 0 {
            let _ = PostMessageW(Some(hwnd), msg, windows::Win32::Foundation::WPARAM(0), windows::Win32::Foundation::LPARAM(0));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_launch_is_detected_from_the_mutex_error() {
        assert_eq!(decide(true), Launch::Second);
        assert_eq!(decide(false), Launch::First);
    }

    #[test]
    fn mutex_name_is_stable_and_specific() {
        assert_eq!(MUTEX_NAME, "NetGlassNativeSingleInstance");
        assert_ne!(MUTEX_NAME, WINDOW_CLASS, "互斥体名不能和窗口类名混用");
        assert_ne!(MUTEX_NAME, wake_message_name());
    }

    #[test]
    fn wake_message_name_matches_the_window_class_lookup() {
        assert_eq!(wake_message_name(), "NetGlassNativeWake");
        // FindWindowW 必须能找到主窗口：类名要和 window.rs 一致。
        assert_eq!(WINDOW_CLASS, "NetGlassNative");
    }
}
