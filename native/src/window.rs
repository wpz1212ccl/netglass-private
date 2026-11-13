//! Win32 plumbing: DPI awareness, the layered window, its DIB surface, and the
//! diagnostic probes used by the verification runs (`--hit`, `--pixels`).

use std::ffi::c_void;

use windows::core::{Result, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, GetPixel, ReleaseDC,
    SelectObject, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, DIB_RGB_COLORS, HBRUSH, HBITMAP,
    HDC, HGDIOBJ,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    GetDpiForSystem, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::*;

const CLASS_NAME: &str = "NetGlassNative";

pub fn dpi_aware() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

pub fn dpi_scale() -> f32 {
    unsafe { GetDpiForSystem() as f32 / 96.0 }
}

/// 屏幕 DC 缓存：ULW 每帧都要一个屏幕 DC，反复 GetDC/ReleaseDC 在 15 fps
/// 呼吸路径上是可见开销。进程退出时由系统回收，不需要手动释放。
fn screen_dc() -> HDC {
    use std::cell::Cell;
    thread_local! {
        static CACHE: Cell<isize> = const { Cell::new(0) };
    }
    CACHE.with(|c| {
        let v = c.get();
        if v != 0 {
            return HDC(v as *mut c_void);
        }
        let dc = unsafe { GetDC(None) };
        c.set(dc.0 as isize);
        dc
    })
}

/// A 32bpp premultiplied-ARGB DIB with its memory DC -- the surface that both
/// Direct2D (as a DC render target) and UpdateLayeredWindow consume.
pub struct Surface {
    pub hwnd: Option<HWND>,
    pub mem_dc: HDC,
    pub dib: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    pub w: i32,
    pub h: i32,
}

unsafe impl Send for Surface {}

impl Surface {
    pub fn new(hwnd: Option<HWND>, w: i32, h: i32) -> Result<Self> {
        unsafe {
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    // negative => top-down rows, matching D2D's expectations
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: 0, // BI_RGB
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let dib = CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0)?;
            let mem_dc = CreateCompatibleDC(None);
            let old = SelectObject(mem_dc, HGDIOBJ(dib.0));
            // Start fully transparent: nothing outside the ellipse may ever be opaque.
            std::ptr::write_bytes(bits as *mut u8, 0, (w * h * 4) as usize);
            Ok(Self {
                hwnd,
                mem_dc,
                dib,
                old,
                bits: bits as *mut u8,
                w,
                h,
            })
        }
    }

    pub fn present(&self, x: i32, y: i32) -> Result<()> {
        self.present_rect(x, y, self.w, self.h)
    }

    /// ULW 的 pptDst + psize 可以逐帧改位置与尺寸，形变因此不需要 SetWindowPos，
    /// 也不会和窗口 resize 抢主线程（spec §2 D8）。源 DIB 仍是整张画布，
    /// 中间帧靠 ULW 缩放降级——T5 才换成"纯形状重绘 + 重建 DIB 尺寸"。
    pub fn present_rect(&self, x: i32, y: i32, w: i32, h: i32) -> Result<()> {
        let hwnd = match self.hwnd {
            Some(h) => h,
            None => return Ok(()),
        };
        unsafe {
            let dst = POINT { x, y };
            let size = SIZE {
                cx: w.max(1),
                cy: h.max(1),
            };
            let src = POINT { x: 0, y: 0 };
            let blend = BLENDFUNCTION {
                BlendOp: 0, // AC_SRC_OVER
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: 1, // AC_SRC_ALPHA
            };
            // 每帧 GetDC/ReleaseDC 是可见开销（15 fps 呼吸）；缓存屏幕 DC。
            let screen = screen_dc();
            let r = UpdateLayeredWindow(
                hwnd,
                Some(screen),
                Some(&dst),
                Some(&size),
                Some(self.mem_dc),
                Some(&src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
            r
        }
    }

    /// Reads the alpha byte straight out of the DIB (BGRA, top-down).
    pub fn alpha_at(&self, x: i32, y: i32) -> u8 {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return 0;
        }
        unsafe { *self.bits.add(((y * self.w + x) * 4 + 3) as usize) }
    }

    pub fn bgra_at(&self, x: i32, y: i32) -> (u8, u8, u8, u8) {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return (0, 0, 0, 0);
        }
        unsafe {
            let p = self.bits.add(((y * self.w + x) * 4) as usize);
            (*p, *p.add(1), *p.add(2), *p.add(3))
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            let _ = SelectObject(self.mem_dc, self.old);
            let _ = DeleteObject(HGDIOBJ(self.dib.0));
            let _ = DeleteDC(self.mem_dc);
        }
    }
}

pub struct WindowConfig {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    /// false 时创建即隐藏（`--silent` 自启动不显窗）。
    pub visible: bool,
}

pub fn create_window(cfg: &WindowConfig, proc: WNDPROC) -> Result<HWND> {
    unsafe {
        let hinstance = GetModuleHandleW(None)?;
        let cls = wide(CLASS_NAME);
        let wc = WNDCLASSW {
            style: WNDCLASS_STYLES(0),
            lpfnWndProc: proc,
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: windows::Win32::Foundation::HINSTANCE(hinstance.0),
            hIcon: HICON::default(),
            hCursor: HCURSOR::default(),
            hbrBackground: HBRUSH::default(),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: PCWSTR(cls.as_ptr()),
        };
        let _ = RegisterClassW(&wc);
        let style = if cfg.visible {
            WS_POPUP | WS_VISIBLE
        } else {
            WS_POPUP
        };
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            PCWSTR(cls.as_ptr()),
            PCWSTR(wide("NetGlass Native").as_ptr()),
            style,
            cfg.x,
            cfg.y,
            cfg.w,
            cfg.h,
            None,
            None,
            Some(windows::Win32::Foundation::HINSTANCE(hinstance.0)),
            None,
        )
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn window_rect(hwnd: HWND) -> RECT {
    unsafe {
        let mut r = RECT::default();
        let _ = GetWindowRect(hwnd, &mut r);
        r
    }
}

pub fn cursor_pos() -> POINT {
    unsafe {
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        p
    }
}

pub fn set_pos(hwnd: HWND, x: i32, y: i32) {
    unsafe {
        let _ = SetWindowPos(hwnd, None, x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

/// WindowFromPoint + identity, for the hit-test evidence.
pub fn describe_at(x: i32, y: i32) -> String {
    unsafe {
        let hwnd = WindowFromPoint(POINT { x, y });
        if hwnd.is_invalid() {
            return format!("({x},{y}) hwnd=null");
        }
        let mut cls = [0u16; 256];
        let n = GetClassNameW(hwnd, &mut cls);
        let class = String::from_utf16_lossy(&cls[..n.max(0) as usize]);
        let mut title = [0u16; 256];
        let tn = GetWindowTextW(hwnd, &mut title);
        let title = String::from_utf16_lossy(&title[..tn.max(0) as usize]);
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        format!(
            "({x},{y}) hwnd=0x{:X} class={class:?} pid={pid} title={title:?}",
            hwnd.0 as usize
        )
    }
}

/// Screen pixel in physical coordinates (process is per-monitor DPI aware).
pub fn screen_pixel(x: i32, y: i32) -> (u8, u8, u8) {
    unsafe {
        let dc = GetDC(None);
        let c = GetPixel(dc, x, y);
        ReleaseDC(None, dc);
        // COLORREF is 0x00BBGGRR
        let v = c.0;
        ((v & 0xFF) as u8, ((v >> 8) & 0xFF) as u8, ((v >> 16) & 0xFF) as u8)
    }
}

pub fn parse_points(args: &[String], flag: &str) -> Vec<(i32, i32)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == flag && i + 1 < args.len() {
            for pair in args[i + 1].split(';') {
                let mut it = pair.split(',');
                if let (Some(a), Some(b)) = (it.next(), it.next()) {
                    if let (Ok(x), Ok(y)) = (a.trim().parse(), b.trim().parse()) {
                        out.push((x, y));
                    }
                }
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    out
}

pub const WM_APP_SAMPLE: u32 = 0x8000 + 1;
/// 形态线程 → UI 线程：形态变了（wparam = mode_code）。
pub const WM_APP_MODE: u32 = 0x8000 + 2;
/// 几何线程 → UI 线程：形变中间帧（矩形从 `pending_geometry` 取）。
pub const WM_APP_GEOMETRY: u32 = 0x8000 + 3;

pub fn post(raw: isize, msg: u32) {
    unsafe {
        let _ = PostMessageW(Some(HWND(raw as *mut c_void)), msg, WPARAM(0), LPARAM(0));
    }
}

pub fn post_sample(raw: isize) {
    post(raw, WM_APP_SAMPLE);
}
