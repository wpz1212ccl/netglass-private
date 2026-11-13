//! Inter 字体嵌入（D11）。
//!
//! woff2 已离线转成 5 个静态实例（wght 400/450/500/560/600，family 统一为
//! "Inter"），用 `AddFontMemResourceEx` 注册到进程私有字体集合，DWrite 的
//! `CreateTextFormat("Inter", weight)` 就能按字重匹配。字体数据由系统复制，
//! 句柄保留到进程结束即可（不再 Remove）。

use std::ffi::c_void;
use std::sync::OnceLock;

use windows::Win32::Graphics::Gdi::AddFontMemResourceEx;

pub static INTER_400: &[u8] = include_bytes!("../assets/inter-400.ttf");
pub static INTER_450: &[u8] = include_bytes!("../assets/inter-450.ttf");
pub static INTER_500: &[u8] = include_bytes!("../assets/inter-500.ttf");
pub static INTER_560: &[u8] = include_bytes!("../assets/inter-560.ttf");
pub static INTER_600: &[u8] = include_bytes!("../assets/inter-600.ttf");

pub const ASSETS: [&[u8]; 5] = [INTER_400, INTER_450, INTER_500, INTER_560, INTER_600];

fn u16be(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*b.get(o)?, *b.get(o + 1)?]))
}

fn u32be(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes([
        *b.get(o)?,
        *b.get(o + 1)?,
        *b.get(o + 2)?,
        *b.get(o + 3)?,
    ]))
}

/// sfnt 魔数：0x00010000 / "true" / "OTTO"（后两者是可选的兼容写法）。
pub fn is_sfnt(ttf: &[u8]) -> bool {
    ttf.len() >= 4 && (&ttf[..4] == &[0u8, 1, 0, 0][..] || &ttf[..4] == b"true" || &ttf[..4] == b"OTTO")
}

/// 从 name 表读 family name（nameID=1，Windows UTF-16BE 记录）。
pub fn family_name(ttf: &[u8]) -> Option<String> {
    if !is_sfnt(ttf) {
        return None;
    }
    let num_tables = u16be(ttf, 4)? as usize;
    let mut name_off = None;
    for i in 0..num_tables {
        let rec = 12 + i * 16;
        if &ttf.get(rec..rec + 4)?[..] == b"name" {
            name_off = Some(u32be(ttf, rec + 8)? as usize);
            break;
        }
    }
    let off = name_off?;
    let count = u16be(ttf, off + 2)? as usize;
    let string_off = u16be(ttf, off + 4)? as usize;
    for i in 0..count {
        let rec = off + 6 + i * 12;
        let platform = u16be(ttf, rec)?;
        let encoding = u16be(ttf, rec + 2)?;
        let name_id = u16be(ttf, rec + 6)?;
        let len = u16be(ttf, rec + 8)? as usize;
        let str_off = u16be(ttf, rec + 10)? as usize;
        if name_id != 1 || platform != 3 || encoding != 1 {
            continue;
        }
        let start = off + string_off + str_off;
        let bytes = ttf.get(start..start + len)?;
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        return Some(String::from_utf16_lossy(&units));
    }
    None
}

/// 进程内只注册一次（多次 `Renderer::new` 不重复添加字体面）。
/// 返回是否至少成功注册了一个实例。
pub fn ensure_installed() -> bool {
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| {
        let mut any = false;
        for bytes in ASSETS {
            let mut added = 0u32;
            let handle = unsafe {
                AddFontMemResourceEx(
                    bytes.as_ptr() as *const c_void,
                    bytes.len() as u32,
                    None,
                    &added,
                )
            };
            if !handle.0.is_null() {
                any = true;
                // 句柄故意不释放：字体要活到进程退出（HANDLE 无 Drop）。
                let _ = handle;
            }
        }
        any
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_assets_are_ttf_with_inter_family() {
        for (i, bytes) in ASSETS.iter().enumerate() {
            assert!(is_sfnt(bytes), "asset {i} 不是 sfnt/ttf");
            assert!(bytes.len() > 20_000, "asset {i} 太小：{} B", bytes.len());
            assert_eq!(
                family_name(bytes).as_deref(),
                Some("Inter"),
                "asset {i} 的 name 表 family 必须是 Inter"
            );
        }
    }

    #[test]
    fn weight_classes_are_the_five_static_instances() {
        // OS/2.usWeightClass 在 OS/2 表偏移 4 处。
        let expected = [400u16, 450, 500, 560, 600];
        for (bytes, want) in ASSETS.iter().zip(expected) {
            let num = u16be(bytes, 4).unwrap() as usize;
            let mut os2 = None;
            for i in 0..num {
                let rec = 12 + i * 16;
                if &bytes[rec..rec + 4] == b"OS/2" {
                    os2 = Some(u32be(bytes, rec + 8).unwrap() as usize);
                }
            }
            let off = os2.expect("OS/2 表");
            assert_eq!(u16be(bytes, off + 4), Some(want), "字重类");
        }
    }

    #[test]
    fn rejects_non_font_bytes() {
        assert!(!is_sfnt(b"not a font"));
        assert_eq!(family_name(b"not a font"), None);
    }
}
