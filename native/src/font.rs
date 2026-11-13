//! Inter 字体嵌入（D11）。
//!
//! woff2 已离线转成 5 个静态实例（wght 400/450/500/560/600，family 统一为
//! "Inter"）。`AddFontMemResourceEx` 只登记 GDI 私有字体表，DWrite 看不到；
//! 因此用 DWrite 3 的 FontSetBuilder 建进程内 collection，再把 collection
//! 传给 `CreateTextFormat`，字重按 OS/2 usWeightClass 匹配。


pub static INTER_400: &[u8] = include_bytes!("../assets/inter-400.ttf");
pub static INTER_450: &[u8] = include_bytes!("../assets/inter-450.ttf");
pub static INTER_500: &[u8] = include_bytes!("../assets/inter-500.ttf");
pub static INTER_560: &[u8] = include_bytes!("../assets/inter-560.ttf");
pub static INTER_600: &[u8] = include_bytes!("../assets/inter-600.ttf");

pub const ASSETS: [&[u8]; 5] = [INTER_400, INTER_450, INTER_500, INTER_560, INTER_600];
pub const NAMES: [&str; 5] = [
    "inter-400.ttf",
    "inter-450.ttf",
    "inter-500.ttf",
    "inter-560.ttf",
    "inter-600.ttf",
];

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

/// DWrite 的系统字体集合里是否能看到 `family`。
/// AddFontMemResourceEx 注册的是 GDI 私有字体集合；这里用于自证 DWrite
/// 是否也命中了它（否则 CreateTextFormat 会静默回退 Segoe UI）。
pub fn system_has_family(dwrite: &windows::Win32::Graphics::DirectWrite::IDWriteFactory, family: &str) -> bool {
    use windows::core::PCWSTR;
    use windows::core::BOOL;
    use windows::Win32::Graphics::DirectWrite::IDWriteFontCollection;
    let mut collection: Option<IDWriteFontCollection> = None;
    if unsafe { dwrite.GetSystemFontCollection(&mut collection, false) }.is_err() {
        return false;
    }
    let Some(collection) = collection else {
        return false;
    };
    let name: Vec<u16> = family.encode_utf16().chain(std::iter::once(0)).collect();
    let mut index = 0u32;
    let mut exists = BOOL(0);
    unsafe {
        collection
            .FindFamilyName(PCWSTR(name.as_ptr()), &mut index, &mut exists)
            .is_ok()
            && exists.as_bool()
    }
}

/// 把 5 个静态实例写到 `%TEMP%/netglass-fonts/` 并用 DWrite 的 FontSet API
/// 建一个进程内 collection。
///
/// 为什么不用 `AddFontMemResourceEx`：它只登记 GDI 的进程私有字体表，
/// DWrite 的系统 collection 看不到（`FindFamilyName("Inter")` 为 false，
/// CreateTextFormat 会静默回退 Segoe UI）。DWrite 3 的 FontSetBuilder 是
/// 官方支持的内存/文件字体集合路径，AddFontFile 需要磁盘路径，所以这里
/// 落一份 335 KB 的缓存文件（内容不变则不重写）。
pub fn load_collection(
    dwrite: &windows::Win32::Graphics::DirectWrite::IDWriteFactory,
) -> Option<windows::Win32::Graphics::DirectWrite::IDWriteFontCollection1> {
    use std::os::windows::ffi::OsStrExt;

    use windows::core::{Interface, PCWSTR};
    use windows::Win32::Graphics::DirectWrite::{
        IDWriteFactory5, IDWriteFontSetBuilder2,
    };

    let factory5: IDWriteFactory5 = dwrite.cast().ok()?;
    let dir = std::env::temp_dir().join("netglass-fonts");
    std::fs::create_dir_all(&dir).ok()?;
    let builder = unsafe { factory5.CreateFontSetBuilder() }.ok()?;
    let builder2: IDWriteFontSetBuilder2 = builder.cast().ok()?;
    for (name, bytes) in NAMES.iter().zip(ASSETS) {
        let path = dir.join(name);
        let stale = std::fs::read(&path).map(|b| b != bytes).unwrap_or(true);
        if stale {
            std::fs::write(&path, bytes).ok()?;
        }
        let w: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        unsafe { builder2.AddFontFile(PCWSTR(w.as_ptr())) }.ok()?;
    }
    let set = unsafe { builder.CreateFontSet() }.ok()?;
    unsafe { factory5.CreateFontCollectionFromFontSet(&set) }.ok()
}

/// collection 里是否存在 `family`（用于自证 DWrite 真的命中 Inter）。
pub fn has_family(
    collection: &windows::Win32::Graphics::DirectWrite::IDWriteFontCollection,
    family: &str,
) -> bool {
    use windows::core::{BOOL, PCWSTR};
    let name: Vec<u16> = family.encode_utf16().chain(std::iter::once(0)).collect();
    let mut index = 0u32;
    let mut exists = BOOL(0);
    unsafe {
        collection
            .FindFamilyName(PCWSTR(name.as_ptr()), &mut index, &mut exists)
            .is_ok()
            && exists.as_bool()
    }
}

/// family 下所有字重（诊断用；应得到 400/450/500/560/600）。
pub fn family_weights(
    collection: &windows::Win32::Graphics::DirectWrite::IDWriteFontCollection,
    family: &str,
) -> Vec<i32> {
    use windows::core::{BOOL, PCWSTR};
    let name: Vec<u16> = family.encode_utf16().chain(std::iter::once(0)).collect();
    let mut index = 0u32;
    let mut exists = BOOL(0);
    let Ok(()) = (unsafe {
        collection.FindFamilyName(PCWSTR(name.as_ptr()), &mut index, &mut exists)
    }) else {
        return Vec::new();
    };
    if !exists.as_bool() {
        return Vec::new();
    }
    let Ok(fam) = (unsafe { collection.GetFontFamily(index) }) else {
        return Vec::new();
    };
    let count = unsafe { fam.GetFontCount() };
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count {
        if let Ok(font) = unsafe { fam.GetFont(i) } {
            out.push(unsafe { font.GetWeight() }.0);
        }
    }
    out
}

/// 诊断：打印 collection 里的 family / 字重 / face 名（`--fonts`）。
pub fn dump(collection: &windows::Win32::Graphics::DirectWrite::IDWriteFontCollection) {
    use windows::Win32::Graphics::DirectWrite::IDWriteLocalizedStrings;

    fn strings(s: &IDWriteLocalizedStrings) -> Vec<String> {
        let n = unsafe { s.GetCount() };
        let mut out = Vec::new();
        for i in 0..n {
            let Ok(len) = (unsafe { s.GetStringLength(i) }) else {
                continue;
            };
            let mut buf = vec![0u16; len as usize + 1];
            if unsafe { s.GetString(i, &mut buf) }.is_ok() {
                out.push(String::from_utf16_lossy(&buf[..len as usize]));
            }
        }
        out
    }

    let families = unsafe { collection.GetFontFamilyCount() };
    println!("collection families = {families}");
    for fi in 0..families {
        let Ok(fam) = (unsafe { collection.GetFontFamily(fi) }) else {
            continue;
        };
        let Ok(names) = (unsafe { fam.GetFamilyNames() }) else {
            continue;
        };
        let count = unsafe { fam.GetFontCount() };
        println!("family[{fi}] names={:?} fonts={count}", strings(&names));
        for i in 0..count {
            let Ok(font) = (unsafe { fam.GetFont(i) }) else {
                continue;
            };
            let face = unsafe { font.GetFaceNames() }.ok().map(|s| strings(&s)).unwrap_or_default();
            println!(
                "  font[{i}] weight={} stretch={:?} style={:?} face={face:?}",
                unsafe { font.GetWeight() }.0,
                unsafe { font.GetStretch() },
                unsafe { font.GetStyle() },
            );
        }
    }
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
