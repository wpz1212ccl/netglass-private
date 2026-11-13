//! `%APPDATA%\com.netglass.app\config.json` 的读写（P4-3）。
//!
//! 与 WebView 版**逐字互通**：同一路径、同一字段名、同一 `serde_json`
//! pretty 排版。D10 要求零新增 crate，所以这里手写一个只认这四个字段的
//! 小解析器——未知字段照 JSON 规则跳过（WebView 版将来加字段不会读崩），
//! 输出格式对齐 `serde_json::to_string_pretty`。

use std::path::{Path, PathBuf};

use crate::anim;

/// 与 `src-tauri/src/main.rs:32-38` 的 `Config` 一一对应。
/// 全部 `Option`：文件里缺失/null 都表示"没有用户决定"，由调用方取默认值。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Config {
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub locked: Option<bool>,
    pub autostart: Option<bool>,
}

/// `%APPDATA%\com.netglass.app\config.json`。
pub fn path() -> PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    PathBuf::from(base)
        .join("com.netglass.app")
        .join("config.json")
}

pub fn load() -> Config {
    load_at(&path())
}

pub fn save(cfg: &Config) {
    save_at(&path(), cfg)
}

pub fn load_at(path: &Path) -> Config {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| parse(&s))
        .unwrap_or_default()
}

pub fn save_at(path: &Path, cfg: &Config) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, to_pretty(cfg));
}

/// 与 `serde_json::to_string_pretty` 同排版（两空格缩进、`": "`、无尾换行）。
pub fn to_pretty(c: &Config) -> String {
    fn opt_i(v: Option<i32>) -> String {
        v.map(|n| n.to_string()).unwrap_or_else(|| "null".to_string())
    }
    fn opt_b(v: Option<bool>) -> String {
        v.map(|b| b.to_string()).unwrap_or_else(|| "null".to_string())
    }
    format!(
        "{{\n  \"x\": {},\n  \"y\": {},\n  \"locked\": {},\n  \"autostart\": {}\n}}",
        opt_i(c.x),
        opt_i(c.y),
        opt_b(c.locked),
        opt_b(c.autostart)
    )
}

/// 只解析扁平对象里的四个已知字段；未知键按 JSON 语法跳过。
pub fn parse(s: &str) -> Option<Config> {
    let mut p = Parser {
        b: s.as_bytes(),
        i: 0,
    };
    p.ws();
    p.eat(b'{')?;
    let mut cfg = Config::default();
    p.ws();
    if p.eat(b'}').is_some() {
        return p.finish(&cfg);
    }
    loop {
        p.ws();
        let key = p.string()?;
        p.ws();
        p.eat(b':')?;
        p.ws();
        match key.as_str() {
            "x" => cfg.x = p.opt_int()?,
            "y" => cfg.y = p.opt_int()?,
            "locked" => cfg.locked = p.opt_bool()?,
            "autostart" => cfg.autostart = p.opt_bool()?,
            _ => p.skip_value()?,
        }
        p.ws();
        if p.eat(b',').is_some() {
            continue;
        }
        p.eat(b'}')?;
        return p.finish(&cfg);
    }
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Parser<'a> {
    fn finish(&mut self, cfg: &Config) -> Option<Config> {
        self.ws();
        if self.i != self.b.len() {
            return None;
        }
        Some(cfg.clone())
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn eat(&mut self, ch: u8) -> Option<()> {
        if self.peek()? == ch {
            self.i += 1;
            Some(())
        } else {
            None
        }
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.i += 1;
        }
    }

    fn string(&mut self) -> Option<String> {
        self.eat(b'"')?;
        let mut out = String::new();
        loop {
            let c = self.peek()?;
            self.i += 1;
            match c {
                b'"' => return Some(out),
                b'\\' => {
                    let e = self.peek()?;
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'n' => out.push('\n'),
                        b't' => out.push('\t'),
                        b'r' => out.push('\r'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        // \uXXXX：配置里不该出现，但别把整个文件判死。
                        b'u' => {
                            let hex = self.b.get(self.i..self.i + 4)?;
                            let s = std::str::from_utf8(hex).ok()?;
                            let v = u16::from_str_radix(s, 16).ok()?;
                            out.push(char::from_u32(v as u32)?);
                            self.i += 4;
                        }
                        _ => return None,
                    }
                }
                _ => {
                    // 非 ASCII 键名按字节原样收，最后统一校验 UTF-8。
                    let start = self.i - 1;
                    let mut end = self.i;
                    while let Some(n) = self.peek() {
                        if n == b'"' || n == b'\\' {
                            break;
                        }
                        self.i += 1;
                        end = self.i;
                    }
                    out.push_str(std::str::from_utf8(&self.b[start..end]).ok()?);
                }
            }
        }
    }

    fn opt_int(&mut self) -> Option<Option<i32>> {
        if self.peek()? == b'n' {
            self.literal(b"null")?;
            return Some(None);
        }
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            self.i += 1;
        }
        if self.i == start || (self.i == start + 1 && self.b[start] == b'-') {
            return None;
        }
        let s = std::str::from_utf8(&self.b[start..self.i]).ok()?;
        s.parse::<i32>().ok().map(Some)
    }

    fn opt_bool(&mut self) -> Option<Option<bool>> {
        match self.peek()? {
            b'n' => {
                self.literal(b"null")?;
                Some(None)
            }
            b't' => {
                self.literal(b"true")?;
                Some(Some(true))
            }
            b'f' => {
                self.literal(b"false")?;
                Some(Some(false))
            }
            _ => None,
        }
    }

    fn literal(&mut self, lit: &[u8]) -> Option<()> {
        if self.b.get(self.i..self.i + lit.len())? == lit {
            self.i += lit.len();
            Some(())
        } else {
            None
        }
    }

    /// 未知字段：按 JSON 语法整段跳过（字符串/数字/字面量/嵌套对象数组）。
    fn skip_value(&mut self) -> Option<()> {
        match self.peek()? {
            b'"' => self.string().map(|_| ()),
            b'{' | b'[' => {
                let mut depth = 0usize;
                loop {
                    let c = self.peek()?;
                    if c == b'"' {
                        self.string()?;
                        continue;
                    }
                    self.i += 1;
                    match c {
                        b'{' | b'[' => depth += 1,
                        b'}' | b']' => {
                            depth -= 1;
                            if depth == 0 {
                                return Some(());
                            }
                        }
                        _ => {}
                    }
                }
            }
            b't' => self.literal(b"true"),
            b'f' => self.literal(b"false"),
            b'n' => self.literal(b"null"),
            _ => {
                let start = self.i;
                while matches!(self.peek(), Some(c) if c.is_ascii_digit() || matches!(c, b'-' | b'+' | b'.' | b'e' | b'E')) {
                    self.i += 1;
                }
                if self.i == start {
                    None
                } else {
                    Some(())
                }
            }
        }
    }
}

/// 可信的桌面锚点：两个坐标都在、不是旧版读到的 (0,0)、且卡片与屏幕有交集。
/// 对照 `src-tauri/src/main.rs::resolve_home` + `is_on_screen`。
pub fn usable_home(cfg: &Config, screen: anim::Rect, card_w: i32, card_h: i32) -> Option<(i32, i32)> {
    let (x, y) = (cfg.x?, cfg.y?);
    if x == 0 && y == 0 {
        return None;
    }
    let on_screen = x + card_w > screen.x
        && x < screen.x + screen.w
        && y + card_h > screen.y
        && y < screen.y + screen.h;
    if on_screen {
        Some((x, y))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 用户机器上 WebView 版实际写出的文件内容（只读对照，独立于本实现）。
    const WEBVIEW_FILE: &str = "{\n  \"x\": 1041,\n  \"y\": 196,\n  \"locked\": false,\n  \"autostart\": null\n}";

    fn screen() -> anim::Rect {
        anim::Rect::new(0, 0, 2560, 1600)
    }

    #[test]
    fn parses_the_webview_file_verbatim() {
        let c = parse(WEBVIEW_FILE).expect("real config must parse");
        assert_eq!(
            c,
            Config {
                x: Some(1041),
                y: Some(196),
                locked: Some(false),
                autostart: None
            }
        );
    }

    #[test]
    fn pretty_output_matches_serde_json_layout() {
        let c = Config {
            x: Some(1041),
            y: Some(196),
            locked: Some(false),
            autostart: None,
        };
        assert_eq!(to_pretty(&c), WEBVIEW_FILE);
    }

    #[test]
    fn roundtrip_keeps_every_field() {
        for c in [
            Config::default(),
            Config {
                x: Some(-12),
                y: Some(7),
                locked: Some(true),
                autostart: Some(true),
            },
            Config {
                x: None,
                y: None,
                locked: None,
                autostart: Some(false),
            },
        ] {
            assert_eq!(parse(&to_pretty(&c)), Some(c));
        }
    }

    #[test]
    fn tolerates_unknown_fields_and_any_order() {
        let s = r#"{ "autostart": true, "future": {"nested": [1, 2, {"k": "v"}]}, "locked": null, "x": 5, "y": 6 }"#;
        let c = parse(s).expect("unknown fields must be skipped");
        assert_eq!(
            c,
            Config {
                x: Some(5),
                y: Some(6),
                locked: None,
                autostart: Some(true)
            }
        );
    }

    #[test]
    fn missing_fields_are_none() {
        let c = parse("{}").expect("empty object is valid");
        assert_eq!(c, Config::default());
    }

    #[test]
    fn rejects_malformed_input() {
        for s in ["", "{", "{\"x\": }", "{\"x\": 1,}", "not json", "{\"x\": 1.5}"] {
            assert!(parse(s).is_none(), "should reject {s:?}");
        }
    }

    #[test]
    fn accepts_trusted_home_only() {
        let on = Config {
            x: Some(1000),
            y: Some(200),
            ..Default::default()
        };
        assert_eq!(usable_home(&on, screen(), 480, 300), Some((1000, 200)));

        // (0,0) 是旧版窗口未摆好时读到的坏值，直接丢弃。
        let origin = Config {
            x: Some(0),
            y: Some(0),
            ..Default::default()
        };
        assert_eq!(usable_home(&origin, screen(), 480, 300), None);

        // 完全离屏（比如换过显示器）同样丢弃。
        let off = Config {
            x: Some(5000),
            y: Some(200),
            ..Default::default()
        };
        assert_eq!(usable_home(&off, screen(), 480, 300), None);

        // 只有一个坐标也不可信。
        let half = Config {
            x: Some(100),
            ..Default::default()
        };
        assert_eq!(usable_home(&half, screen(), 480, 300), None);
    }

    #[test]
    fn save_then_load_roundtrips_through_disk() {
        let dir = std::env::temp_dir().join("netglass-t6-config-test");
        let path = dir.join("config.json");
        let c = Config {
            x: Some(64),
            y: Some(128),
            locked: Some(true),
            autostart: Some(false),
        };
        save_at(&path, &c);
        assert_eq!(load_at(&path), c);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
