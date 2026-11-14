//! 托盘图标与弹出菜单（P4-1）：`Shell_NotifyIconW` + `CreatePopupMenu`。
//!
//! 零新增三方 crate（D10）：PNG 解码用系统 WIC，图标用 `CreateIconIndirect`，
//! 菜单用 `CreatePopupMenu`/`AppendMenuW`/`TrackPopupMenu`。菜单的**结构与 ID**
//! 在 `menu.rs`（纯逻辑、可单测），这里只负责把它们画到 Win32 上。

use std::ffi::c_void;

use windows::core::{Result, GUID, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS,
    HGDIOBJ,
};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppBGRA, IWICBitmapSource, IWICImagingFactory,
    WICBitmapInterpolationModeHighQualityCubic, WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_SETVERSION,
    NOTIFYICONDATAW, NOTIFYICON_VERSION_4,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::menu::{self, Item, MenuAction};

/// 托盘回调消息（wparam = 鼠标消息，lparam = 屏幕坐标）。
pub const WM_TRAYICON: u32 = crate::window::WM_APP_TRAY;
/// 托盘图标 id（单图标）。
const TRAY_ID: u32 = 1;
/// tooltip 与现版一致。
const TOOLTIP: &str = "NetGlass";
/// 内嵌的 PNG（从 `src-tauri/icons/tray.png` 复制，32×32 RGBA）。
const TRAY_PNG: &[u8] = include_bytes!("../assets/tray.png");

/// 托盘图标句柄的持有者：退出时 `NIM_DELETE` + `DestroyIcon`，
/// 否则任务栏会留下一个点了没反应的幽灵图标。
pub struct Tray {
    hwnd: HWND,
    icon: HICON,
    added: bool,
    /// 菜单的 owner 窗口：**必须是一个可激活的普通窗口**。
    ///
    /// 主窗口是 `WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW` 的 layered 窗口，
    /// `SetForegroundWindow` 对它无效（系统拒绝激活），于是弹出菜单拿不到
    /// 输入：鼠标点不中、Esc 也关不掉（实测）。菜单的 owner 一旦不可激活，
    /// 整个 `TrackPopupMenu` 就成了死界面。所以这里单独建一个 0×0 的
    /// 隐藏 owner 窗口，只用来承载菜单的激活与消息。
    owner: HWND,
}

impl Tray {
    pub fn add(hwnd: HWND) -> Result<Self> {
        let icon = load_tray_icon().unwrap_or_default();
        let owner = create_menu_owner().unwrap_or_default();
        let mut tray = Self {
            hwnd,
            icon,
            added: false,
            owner,
        };
        let mut data = tray.data();
        data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        data.uCallbackMessage = WM_TRAYICON;
        data.hIcon = icon;
        fill_tip(&mut data.szTip, TOOLTIP);
        tray.added = unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool();
        if tray.added {
            // V4 语义：回调 lparam 是坐标、wparam 是鼠标消息（菜单落点要用）。
            data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            unsafe {
                let _ = Shell_NotifyIconW(NIM_SETVERSION, &data);
            }
        }
        Ok(tray)
    }

    fn data(&self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: TRAY_ID,
            ..Default::default()
        }
    }

    /// 在光标处弹菜单，返回用户选中的动作（取消 = None）。
    pub fn show_menu(&self, items: &[Item], cursor: POINT) -> Option<MenuAction> {
        unsafe {
            let menu = build_menu(items)?;
            // owner 必须能成为前台，否则菜单收不到输入（见 `owner` 字段注释）。
            // 弹之前先把它设成前台，弹完补一条 WM_NULL（Win32 的既定做法：
            // 否则菜单关掉后前台会悬空，下一次点击被吞）。
            let owner = if self.owner.is_invalid() {
                self.hwnd
            } else {
                self.owner
            };
            // owner 必须能成为前台，否则 TrackPopupMenu 收不到输入：菜单画得
            // 出来，但鼠标点不中、Esc 也关不掉，而 UI 线程被它阻塞 = 整个应用
            // 假死（实测过）。策略（T8 修订，用户反馈"右键菜单消失"）：
            // ① 先尽力抢前台（attach shell 线程 + SetForegroundWindow）；
            // ② 拿不到也**照弹**——TrackPopupMenu 的输入路由到 owner 窗口，
            //    owner 是可激活的普通窗口，绝大多数场景可用；WebView 版
            //    （tauri 托盘）从不预检前台权，行为对齐它。
            // ③ 假死兜底：后台线程 4s 后若菜单仍开着（前台仍未回到 owner），
            //    PostMessageW(WM_CANCELMODE) 强制关闭菜单，UI 线程解锁。
            let got_fg = force_foreground(owner);
            if !got_fg {
                log("tray menu: foreground not acquired; showing menu anyway (cancel-watch armed)");
            }
            // 看门狗：4s 后若本线程仍在 TrackPopupMenu 里（菜单模态标志仍在），
            // 发 WM_CANCELMODE 关菜单——TrackPopupMenu 返回 0，等价"取消"。
            let owner_raw = owner.0 as isize;
            let cancel = std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(4000));
                // 若前台已是 owner，说明菜单正常交互中/已结束，不动。
                let fg = unsafe { GetForegroundWindow() };
                if !fg.is_invalid() && fg.0 as isize == owner_raw {
                    return;
                }
                unsafe {
                    let _ = PostMessageW(
                        Some(HWND(owner_raw as *mut _)),
                        WM_CANCELMODE,
                        WPARAM(0),
                        LPARAM(0),
                    );
                }
            });
            let id = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
                cursor.x,
                cursor.y,
                None,
                owner,
                None,
            );
            let _ = cancel.join();
            let _ = PostMessageW(Some(owner), WM_NULL, WPARAM(0), LPARAM(0));
            let _ = DestroyMenu(menu);
            menu::action_of(id.0 as usize)
        }
    }

    /// 只改 tooltip 之外的状态时用（当前只在退出时删图标）。
    pub fn remove(&mut self) {
        if !self.added {
            return;
        }
        let data = self.data();
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &data);
        }
        self.added = false;
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        self.remove();
        if !self.icon.is_invalid() {
            unsafe {
                let _ = DestroyIcon(self.icon);
            }
        }
        if !self.owner.is_invalid() {
            unsafe {
                let _ = DestroyWindow(self.owner);
            }
        }
    }
}

/// 取得前台权：AttachThreadInput → SetForegroundWindow → detach，返回是否成功。
///
/// 只在"本线程不是前台线程"时 attach，且无论成败都立刻 detach——长时间
/// attach 会把两个消息循环焊在一起（对方卡死我们也跟着卡）。
/// 前台为空（桌面刚失去前台、shell 对话框刚关）时改附到 shell 窗口的线程。
fn force_foreground(owner: HWND) -> bool {
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    unsafe {
        let cur = GetCurrentThreadId();
        let mut fg = GetForegroundWindow();
        let mut fg_thread = if fg.is_invalid() {
            0
        } else {
            GetWindowThreadProcessId(fg, None)
        };
        if fg_thread == 0 {
            fg = GetShellWindow();
            if !fg.is_invalid() {
                fg_thread = GetWindowThreadProcessId(fg, None);
            }
        }
        let attached = fg_thread != 0 && fg_thread != cur;
        if attached {
            let _ = AttachThreadInput(cur, fg_thread, true);
        }
        let mut ok = SetForegroundWindow(owner).as_bool();
        if !ok {
            let _ = BringWindowToTop(owner);
            ok = SetForegroundWindow(owner).as_bool();
        }
        if attached {
            let _ = AttachThreadInput(cur, fg_thread, false);
        }
        // 以"真的成了前台"为准，而不是 SetForegroundWindow 的返回值。
        let now = GetForegroundWindow();
        let got = ok && now == owner;
        if !got {
            log(&format!(
                "tray menu: foreground not acquired (fg class={:?} attached={attached} ret={ok})",
                fg_class(now)
            ));
        }
        got
    }
}

fn fg_class(h: HWND) -> String {
    if h.is_invalid() {
        return String::new();
    }
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(h, &mut buf) }.max(0) as usize;
    String::from_utf16_lossy(&buf[..n])
}

fn log(s: &str) {
    use std::io::Write;
    let p = std::env::temp_dir().join("netglass-log.txt");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
        let _ = writeln!(f, "{s}");
    }
}

/// 菜单 owner：0×0 的隐藏普通窗口（可激活，但不显示）。
fn create_menu_owner() -> Result<HWND> {
    unsafe {
        let hinstance = GetModuleHandleW(None)?;
        let cls = wide("NetGlassTrayOwner");
        let wc = WNDCLASSW {
            style: WNDCLASS_STYLES(0),
            lpfnWndProc: Some(menu_owner_proc),
            hInstance: windows::Win32::Foundation::HINSTANCE(hinstance.0),
            lpszClassName: PCWSTR(cls.as_ptr()),
            ..Default::default()
        };
        let _ = RegisterClassW(&wc);
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            PCWSTR(cls.as_ptr()),
            PCWSTR(wide("NetGlass tray menu owner").as_ptr()),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(windows::Win32::Foundation::HINSTANCE(hinstance.0)),
            None,
        )
    }
}

unsafe extern "system" fn menu_owner_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    DefWindowProcW(hwnd, msg, wp, lp)
}

/// 按 `menu::items` 建一个真实的 Win32 弹出菜单（托盘菜单与自证诊断共用）。
pub fn build_menu(items: &[Item]) -> Option<HMENU> {
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        for it in items {
            match it.action {
                None => {
                    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                }
                Some(a) => {
                    let mut flags = MF_STRING;
                    if it.checked {
                        flags |= MF_CHECKED;
                    }
                    let label = wide(&it.label);
                    let _ = AppendMenuW(menu, flags, menu::id_of(a), PCWSTR(label.as_ptr()));
                }
            }
        }
        Some(menu)
    }
}

/// `--tray-menu`：把托盘菜单真的建出来，再逐项读回 Win32 的文案/ID/勾选态。
/// 托盘图标点不到时，这是"菜单结构与现版一致"的可验证证据。
pub fn dump_menu(items: &[Item]) {
    unsafe {
        let Some(menu) = build_menu(items) else {
            println!("tray menu: CreatePopupMenu FAILED");
            return;
        };
        let count = GetMenuItemCount(Some(menu));
        println!("tray menu: items={count}");
        for i in 0..count {
            let mut buf = [0u16; 256];
            let n = GetMenuStringW(menu, i as u32, Some(&mut buf), MF_BYPOSITION);
            let label = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
            let id = GetMenuItemID(menu, i);
            let state = GetMenuState(menu, i as u32, MF_BYPOSITION);
            println!(
                "  [{i}] id={id} checked={} sep={} label={label:?}",
                state & MF_CHECKED.0 != 0,
                state & MF_SEPARATOR.0 != 0
            );
        }
        let _ = DestroyMenu(menu);
    }
}

fn fill_tip(buf: &mut [u16; 128], text: &str) {
    for (dst, src) in buf.iter_mut().zip(text.encode_utf16()) {
        *dst = src;
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 从内嵌 PNG 生成托盘图标（WIC 解码 → 32bppBGRA → CreateIconIndirect）。
pub fn load_tray_icon() -> Option<HICON> {
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16);
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let stream = factory.CreateStream().ok()?;
        stream.InitializeFromMemory(TRAY_PNG).ok()?;
        let decoder = factory
            .CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)
            .ok()?;
        let frame = decoder.GetFrame(0).ok()?;
        let conv = factory.CreateFormatConverter().ok()?;
        conv.Initialize(
            &frame,
            &GUID_WICPixelFormat32bppBGRA,
            windows::Win32::Graphics::Imaging::WICBitmapDitherTypeNone,
            None,
            0.0,
            windows::Win32::Graphics::Imaging::WICBitmapPaletteTypeCustom,
        )
        .ok()?;
        // 缩放到系统小图标尺寸（150% DPI 下是 24px），否则托盘里会被拉伸。
        let scaler = factory.CreateBitmapScaler().ok()?;
        scaler
            .Initialize(&conv, size as u32, size as u32, WICBitmapInterpolationModeHighQualityCubic)
            .ok()?;
        let src: IWICBitmapSource = scaler.into();

        let stride = (size * 4) as u32;
        let mut buf = vec![0u8; (stride * size as u32) as usize];
        src.CopyPixels(std::ptr::null(), stride, &mut buf).ok()?;

        // 颜色位图：直接把我们拷出来的 BGRA 当 DIB 内容。
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: 0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let color = CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        std::ptr::copy_nonoverlapping(buf.as_ptr(), bits as *mut u8, buf.len());
        // 掩码位图：单色全 0（alpha 由颜色位图提供，掩码不参与）。
        let mask = CreateBitmap(size, size, 1, 1, None);
        let info = ICONINFO {
            fIcon: true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&info).ok();
        let _ = DeleteObject(HGDIOBJ(color.0));
        let _ = DeleteObject(HGDIOBJ(mask.0));
        icon
    }
}

/// 诊断用：`--icon` 打印图标加载结果（托盘不可见时自证 PNG 解码可用）。
pub fn dump_icon() {
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) };
    match load_tray_icon() {
        Some(h) => println!(
            "tray icon: HICON=0x{:X} size={size}px png={} bytes",
            h.0 as usize,
            TRAY_PNG.len()
        ),
        None => println!("tray icon: load FAILED (png={} bytes)", TRAY_PNG.len()),
    }
    let _ = GUID::default();
}

/// 左键/右键的判定（V4 回调里 wparam 低字是鼠标消息）。
///
/// **NOTIFYICON_VERSION_4 语义（T8 修订，用户反馈"右键托盘无反应"）**：
/// V4 下托盘回调不再发 `WM_LBUTTONUP`/`WM_RBUTTONUP`——
/// 选择/激活发 `NIN_SELECT`，右键（上下文菜单）发 `WM_CONTEXTMENU`。
/// 只认旧消息 = 右键永远不进弹菜单分支、左键显/隐失效。
pub fn mouse_message(wp: usize) -> u32 {
    (wp & 0xFFFF) as u32
}

pub fn is_left_up(msg: u32) -> bool {
    // NIN_SELECT 在 Shell 模块（V4 选择/激活通知）。
    const NIN_SELECT: u32 = 1024u32;
    msg == WM_LBUTTONUP || msg == NIN_SELECT
}

pub fn is_right_up(msg: u32) -> bool {
    msg == WM_RBUTTONUP || msg == WM_CONTEXTMENU
}

/// V4 回调里 lparam 是**屏幕坐标**（低字 x、高字 y，按有符号 16 位打包）。
pub fn point_from_lparam(lp: isize) -> POINT {
    let v = lp as u32;
    let x = (v & 0xFFFF) as u16 as i16 as i32;
    let y = ((v >> 16) & 0xFFFF) as u16 as i16 as i32;
    POINT { x, y }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_png_is_a_real_png() {
        assert_eq!(&TRAY_PNG[..8], b"\x89PNG\r\n\x1a\n");
        assert!(TRAY_PNG.len() > 100);
    }

    #[test]
    fn callback_unpacks_into_a_screen_point() {
        // x=100, y=200 打包成 lparam。
        let lp = ((200u32 << 16) | 100u32) as isize;
        let p = point_from_lparam(lp);
        assert_eq!((p.x, p.y), (100, 200));
        // 负坐标（副屏在左）按有符号 16 位解释。
        let lp = ((200u32 << 16) | 0xFFF6u32) as isize;
        assert_eq!(point_from_lparam(lp).x, -10);
    }

    #[test]
    fn v4_mouse_message_is_the_low_word() {
        assert_eq!(mouse_message(WM_LBUTTONUP as usize), WM_LBUTTONUP);
        assert!(is_left_up(WM_LBUTTONUP));
        assert!(is_right_up(WM_RBUTTONUP));
        assert!(!is_left_up(WM_RBUTTONDOWN));
        // V4 托盘语义：选择 = NIN_SELECT，右键 = WM_CONTEXTMENU（旧消息在
        // V4 下不会到达）。
        const NIN_SELECT: u32 = 1024u32;
        assert!(is_left_up(NIN_SELECT), "V4 left/keyboard select");
        assert!(is_right_up(WM_CONTEXTMENU), "V4 right-click context menu");
        assert!(!is_right_up(NIN_SELECT));
        assert!(!is_left_up(WM_CONTEXTMENU));
    }

    #[test]
    fn tray_callback_message_does_not_collide_with_existing_ones() {
        for other in [
            crate::window::WM_APP_SAMPLE,
            crate::window::WM_APP_MODE,
            crate::window::WM_APP_GEOMETRY,
        ] {
            assert_ne!(WM_TRAYICON, other);
        }
    }
}
