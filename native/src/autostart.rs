//! 开机自启（P4-3）：HKCU `...\CurrentVersion\Run` + `--silent`。
//!
//! 与现版 `tauri_plugin_autostart` 写的位置/值名/命令行逐字一致：
//! 值名 `NetGlass`，数据 `"<exe 路径>" --silent`。D10 要求零新增 crate，
//! 所以直接用 `RegCreateKeyExW`/`RegSetValueExW`/`RegDeleteValueW`。
//!
//! 命令行格式与判定是纯函数（单测）；注册表调用在 `Registry` 里。

use windows::core::PCWSTR;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegQueryValueExW, RegSetValueExW,
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
};

/// 现版 tauri_plugin_autostart 用的值名（= productName）。
pub const VALUE_NAME: &str = "NetGlass";
pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// 自启动参数：与现版 `Some(vec!["--silent"])` 一致。
pub const AUTOSTART_ARG: &str = "--silent";

/// 写进注册表的数据：路径始终加引号（路径含空格），后跟 `--silent`。
pub fn command_line(exe: &str) -> String {
    format!("\"{exe}\" {AUTOSTART_ARG}")
}

/// 启动时是否需要补写：想开且当前没开（对照 `want_autostart && !al.is_enabled()`）。
/// 已经开着就不动——尤其不能把用户已有的自启项改指向别的 exe。
pub fn should_enable(want: bool, enabled: bool) -> bool {
    want && !enabled
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub struct Registry {
    key: HKEY,
    value: &'static str,
}

impl Registry {
    /// 打开（必要时创建）HKCU Run 键，操作正式值名。
    pub fn open() -> Option<Self> {
        Self::open_named(VALUE_NAME)
    }

    /// 打开 Run 键，操作指定值名（自检用临时值名，不碰用户的 NetGlass）。
    pub fn open_named(value: &'static str) -> Option<Self> {
        unsafe {
            let mut key = HKEY::default();
            let r = RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(wide(RUN_KEY).as_ptr()),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE | KEY_QUERY_VALUE,
                None,
                &mut key,
                None,
            );
            (r == ERROR_SUCCESS).then_some(Self { key, value })
        }
    }

    /// 读当前值（不存在返回 None）。
    pub fn read(&self) -> Option<String> {
        unsafe {
            let mut ty = REG_SZ;
            let mut len = 0u32;
            let r = RegQueryValueExW(
                self.key,
                PCWSTR(wide(self.value).as_ptr()),
                None,
                Some(&mut ty),
                None,
                Some(&mut len),
            );
            if r != ERROR_SUCCESS || len < 2 {
                return None;
            }
            let mut buf = vec![0u8; len as usize];
            let mut len2 = len;
            let r = RegQueryValueExW(
                self.key,
                PCWSTR(wide(self.value).as_ptr()),
                None,
                Some(&mut ty),
                Some(buf.as_mut_ptr()),
                Some(&mut len2),
            );
            if r != ERROR_SUCCESS {
                return None;
            }
            let u16s: Vec<u16> = buf[..len2 as usize]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .take_while(|c| *c != 0)
                .collect();
            Some(String::from_utf16_lossy(&u16s))
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.read().is_some()
    }

    pub fn set(&self, value: &str) -> bool {
        let mut data: Vec<u16> = value.encode_utf16().collect();
        data.push(0);
        let bytes: Vec<u8> = data.iter().flat_map(|u| u.to_le_bytes()).collect();
        unsafe {
            RegSetValueExW(self.key, PCWSTR(wide(self.value).as_ptr()), None, REG_SZ, Some(&bytes)) == ERROR_SUCCESS
        }
    }

    pub fn clear(&self) -> bool {
        unsafe {
            let r = RegDeleteValueW(self.key, PCWSTR(wide(self.value).as_ptr()));
            r == ERROR_SUCCESS || r == ERROR_FILE_NOT_FOUND
        }
    }
}

impl Drop for Registry {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.key);
        }
    }
}

/// 现进程 exe 的自启命令行（自启开关与诊断共用）。
pub fn current_command_line() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    Some(command_line(&exe.to_string_lossy()))
}

/// 启动时的自启处理：只补写、不改指向。
pub fn ensure(want: bool) -> Option<String> {
    let reg = Registry::open()?;
    if should_enable(want, reg.is_enabled()) {
        if let Some(cmd) = current_command_line() {
            if reg.set(&cmd) {
                return Some(cmd);
            }
        }
    }
    reg.read()
}

/// 托盘「开机自启」开关：写/删 HKCU Run，返回是否达到目标状态。
pub fn set_enabled(on: bool) -> bool {
    let Some(reg) = Registry::open() else {
        return false;
    };
    if on {
        let Some(cmd) = current_command_line() else {
            return false;
        };
        reg.set(&cmd)
    } else {
        reg.clear() && !reg.is_enabled()
    }
}

/// 当前是否已写入自启（菜单勾选态）。
pub fn is_enabled() -> bool {
    Registry::open().map(|r| r.is_enabled()).unwrap_or(false)
}

/// `--autostart-selftest`：在**临时值名**上跑一遍 写→读→删，证明注册表
/// 通路可用，绝不碰用户真实的 `NetGlass` 自启项。
pub fn selftest() -> bool {
    const TEMP_VALUE: &str = "NetGlassT6Selftest";
    let Some(reg) = Registry::open_named(TEMP_VALUE) else {
        println!("autostart selftest: open Run key FAILED");
        return false;
    };
    let _ = reg.clear();
    let want = command_line(r"D:\tmp\NetGlass.exe");
    let wrote = reg.set(&want);
    let read = reg.read();
    let cleared = reg.clear();
    let gone = reg.read().is_none();
    println!(
        "autostart selftest: value={TEMP_VALUE} write={wrote} read={read:?} cleared={cleared} gone={gone}"
    );
    wrote && read.as_deref() == Some(want.as_str()) && cleared && gone
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_line_quotes_the_path_and_adds_silent() {
        assert_eq!(
            command_line(r"D:\code\vibe coding\桌面网络监看器\dist\NetGlass.exe"),
            "\"D:\\code\\vibe coding\\桌面网络监看器\\dist\\NetGlass.exe\" --silent"
        );
    }

    #[test]
    fn enable_only_when_missing() {
        assert!(should_enable(true, false), "想开且没开 → 补写");
        assert!(!should_enable(true, true), "已经开着 → 不碰（别改指向）");
        assert!(!should_enable(false, false), "不想开 → 不写");
        assert!(!should_enable(false, true), "不想开也不主动删（保持现版语义）");
    }

    #[test]
    fn run_key_and_value_name_match_the_webview_version() {
        assert_eq!(RUN_KEY, r"Software\Microsoft\Windows\CurrentVersion\Run");
        assert_eq!(VALUE_NAME, "NetGlass");
    }
}
