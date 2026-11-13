//! Direct2D + DirectWrite rendering into a premultiplied-ARGB DIB.
//!
//! Why this stack (see report): Direct2D's DC render target writes straight into
//! the 32bpp DIB that `UpdateLayeredWindow` consumes, keeps per-pixel alpha,
//! antialiases geometry in software and lets DirectWrite shape CJK + Latin with
//! grayscale AA (ClearType is unusable on a transparent surface).

use std::cell::RefCell;

use windows::core::{Interface, PCWSTR, Result};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{AlphaBlend, BitBlt, HDC, BLENDFUNCTION, SRCCOPY};
use windows_numerics::{Matrix3x2, Vector2};

use crate::card;
use crate::font;
use crate::fx;
use crate::island;
use crate::window;

/// 呼吸灯一帧的绘制参数（颜色/光晕/透明度/缩放由 main.rs 的相位表算好）。
#[derive(Clone, Copy, Debug)]
pub struct DotPaint {
    pub rgb: (u8, u8, u8),
    pub glow: f32,
    pub alpha: f32,
    pub scale: f32,
    /// 呼吸相位索引（0..island::BREATHE_PHASES），用于挑预渲染帧。
    pub phase: u8,
}

impl Default for DotPaint {
    fn default() -> Self {
        Self {
            rgb: island::DotColor::Green.rgb(),
            glow: island::DotColor::Green.glow(),
            alpha: 1.0,
            scale: 1.0,
            phase: 0,
        }
    }
}

/// Every text/value the widget shows for one frame.
pub struct Frame {
    pub title: String,
    pub state: String,
    pub down_value: String,
    pub down_unit: String,
    pub up_value: String,
    pub up_unit: String,
    /// 底栏 Wi-Fi 值（已连接/未连接/无）。
    pub wifi: String,
    pub wifi_ok: bool,
    /// 底栏代理值（`Watt 加速中` / `23 ms` / `未启用` …）。
    pub proxy: String,
    pub proxy_tone: card::Tone,
    /// hover 详情 5 格。
    pub details: [card::DetailRow; 5],
    pub hist: Vec<f64>,
    /// 岛态 hover / 展开态 hover 布尔（T3 胶囊提亮仍用它）。
    pub hover: bool,
    /// 展开态 hover 时间轴（详情错峰/底栏曲线淡出/内容上浮）。
    pub hover_fx: fx::HoverFx,
    /// 岛态按下（`.card.collapsed:active`，玻璃压暗一档）。
    pub active: bool,
    pub dot: DotPaint,
}

fn c(r: u8, g: u8, b: u8, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a,
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn vadd(a: Vector2, b: Vector2) -> Vector2 {
    Vector2::new(a.X + b.X, a.Y + b.Y)
}

fn vsub(a: Vector2, b: Vector2) -> Vector2 {
    Vector2::new(a.X - b.X, a.Y - b.Y)
}

fn vmul(a: Vector2, k: f32) -> Vector2 {
    Vector2::new(a.X * k, a.Y * k)
}

fn vlen(a: Vector2) -> f32 {
    (a.X * a.X + a.Y * a.Y).sqrt()
}

pub struct Renderer {
    factory: ID2D1Factory,
    dwrite: IDWriteFactory,
    rt: ID2D1DCRenderTarget,
    caps: ID2D1StrokeStyle,
    scale: f32,
    w: f32,
    h: f32,
    /// 主 DIB 的 DC（rt 平时绑它；画静态层时临时改绑，再切回来）。
    mem_dc: HDC,
    family: &'static str,
    /// 进程内 Inter collection；text formats 引用它，必须活到 Renderer drop。
    collection: Option<IDWriteFontCollection>,

    f_title: IDWriteTextFormat,
    f_state: IDWriteTextFormat,
    f_num: IDWriteTextFormat,
    f_unit: IDWriteTextFormat,
    f_up: IDWriteTextFormat,
    f_upunit: IDWriteTextFormat,
    f_foot: IDWriteTextFormat,
    f_foot_b: IDWriteTextFormat,
    f_cell: IDWriteTextFormat,
    f_cell_val: IDWriteTextFormat,
    f_mdown: IDWriteTextFormat,
    f_munit: IDWriteTextFormat,
    f_mup: IDWriteTextFormat,
    /// tabular-nums：等宽数字，1 Hz 更新时布局不抖。
    typo: IDWriteTypography,

    b_ink: ID2D1SolidColorBrush,
    b_ink2: ID2D1SolidColorBrush,
    b_ink3: ID2D1SolidColorBrush,
    b_ink6: ID2D1SolidColorBrush,
    b_ink55: ID2D1SolidColorBrush,
    b_hair72: ID2D1SolidColorBrush,
    b_hair85: ID2D1SolidColorBrush,
    b_dot: ID2D1SolidColorBrush,
    b_base: ID2D1SolidColorBrush,
    b_warn: ID2D1SolidColorBrush,
    b_err: ID2D1SolidColorBrush,
    /// 9 档玻璃亮度（0=普通，8=`.card:hover`），按 hover 时间轴量化选择。
    glass: Vec<ID2D1LinearGradientBrush>,
    b_hl1: ID2D1RadialGradientBrush,
    b_hl2: ID2D1RadialGradientBrush,
    b_blue: ID2D1RadialGradientBrush,
    b_bottom: ID2D1LinearGradientBrush,

    /// 35px squircle 路径（缓存，形状不变就不重建）。
    card_path: ID2D1PathGeometry,
    card_rim: ID2D1PathGeometry,
    icon_wifi: ID2D1PathGeometry,
    icon_wifi_slash: ID2D1PathGeometry,
    icon_bolt: ID2D1PathGeometry,

    pill_normal: Pill,
    pill_hot: Pill,
    pill_active: Pill,
    /// 胶囊静态层：玻璃 + 文字 + 发丝画进一张独立 DIB（内容/悬停变化才重画）。
    /// 呼吸帧用 GDI BitBlt 把它整块拷回主 DIB，不再走 D2D。
    island_static: RefCell<Option<IslandStatic>>,
    /// 呼吸点预渲染：32 个相位帧排成一条位图，呼吸帧只 AlphaBlend 一格。
    dot_tiles: RefCell<Option<DotTiles>>,
    /// 曲线离屏缓存：hist 变了才重画一次，hover 淡出只改合成透明度。
    chart: RefCell<Option<ChartCache>>,
}

struct IslandStatic {
    key: String,
    dot_cx: f32,
    surface: window::Surface,
}

struct DotTiles {
    key: String,
    tile: i32,
    surface: window::Surface,
}

struct ChartCache {
    key: String,
    target: ID2D1BitmapRenderTarget,
    bitmap: ID2D1Bitmap,
    line: ID2D1LinearGradientBrush,
    area: ID2D1LinearGradientBrush,
    base: ID2D1SolidColorBrush,
}

/// `.pillbg` 的三层材质：172° 主渐变 + 左上/右上两团径向高光。
/// 亮度滤镜（hover 1.05 / active .965）在 CSS 里是整层 RGB 乘法，
/// 这里预生成三套 stop，切换时直接换笔刷，避免逐帧重建。
struct Pill {
    base: ID2D1LinearGradientBrush,
    r1: ID2D1RadialGradientBrush,
    r2: ID2D1RadialGradientBrush,
}

impl Renderer {
    pub fn new(mem_dc: HDC, w: i32, h: i32, scale: f32) -> Result<Self> {
        let factory: ID2D1Factory = unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
        let dwrite: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };

        // SOFTWARE keeps the Intel UMD + d3d11 out of the process (~28 MB), at
        // the cost of a slower rasterizer; NETGLASS_RT=hw switches to the GPU path.
        let rt_type = match std::env::var("NETGLASS_RT").as_deref() {
            Ok("hw") => D2D1_RENDER_TARGET_TYPE_DEFAULT,
            _ => D2D1_RENDER_TARGET_TYPE_SOFTWARE,
        };
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: rt_type,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            // 96 dpi => 1 DIP == 1 physical pixel; layout is scaled by `scale` by hand.
            dpiX: 96.0,
            dpiY: 96.0,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        let rt = unsafe { factory.CreateDCRenderTarget(&props)? };
        let rect = windows::Win32::Foundation::RECT {
            left: 0,
            top: 0,
            right: w,
            bottom: h,
        };
        unsafe { rt.BindDC(mem_dc, &rect)? };

        let caps_props = D2D1_STROKE_STYLE_PROPERTIES {
            startCap: D2D1_CAP_STYLE_ROUND,
            endCap: D2D1_CAP_STYLE_ROUND,
            dashCap: D2D1_CAP_STYLE_ROUND,
            lineJoin: D2D1_LINE_JOIN_ROUND,
            miterLimit: 2.0,
            dashStyle: D2D1_DASH_STYLE_SOLID,
            dashOffset: 0.0,
        };
        let caps = unsafe { factory.CreateStrokeStyle(&caps_props, None)? };

        let s = scale;
        // D11：Inter 静态实例（400/450/500/560/600）建成进程内 DWrite collection。
        // 建 collection 失败时退回原型用的 Segoe UI Variable（并记日志）。
        let (collection, family): (Option<IDWriteFontCollection>, &'static str) =
            match font::load_collection(&dwrite)
                .and_then(|c| c.cast::<IDWriteFontCollection>().ok())
            {
                Some(c) => (Some(c), "Inter"),
                None => (None, "Segoe UI Variable Text"),
            };
        let coll = collection.as_ref();

        let f_title = Self::fmt(&dwrite, coll, family, DWRITE_FONT_WEIGHT_SEMI_BOLD, 13.0 * s)?;
        let f_state = Self::fmt(&dwrite, coll, family, DWRITE_FONT_WEIGHT(450), 11.0 * s)?;
        let f_num = Self::fmt(&dwrite, coll, family, DWRITE_FONT_WEIGHT_SEMI_BOLD, 46.0 * s)?;
        let f_unit = Self::fmt(&dwrite, coll, family, DWRITE_FONT_WEIGHT_MEDIUM, 13.0 * s)?;
        let f_up = Self::fmt(&dwrite, coll, family, DWRITE_FONT_WEIGHT(560), 14.0 * s)?;
        let f_upunit = Self::fmt(&dwrite, coll, family, DWRITE_FONT_WEIGHT_MEDIUM, 11.0 * s)?;
        let f_foot = Self::fmt(&dwrite, coll, family, DWRITE_FONT_WEIGHT_MEDIUM, 11.0 * s)?;
        let f_foot_b = Self::fmt(&dwrite, coll, family, DWRITE_FONT_WEIGHT_SEMI_BOLD, 11.0 * s)?;
        let f_cell = Self::fmt(&dwrite, coll, family, DWRITE_FONT_WEIGHT(450), 10.5 * s)?;
        let f_cell_val = Self::fmt(&dwrite, coll, family, DWRITE_FONT_WEIGHT(550), 10.5 * s)?;
        // 胶囊迷你数字：15px/600 + 10px 单位 + 10px 上行（#mDown/#mUnit/.mup）。
        let f_mdown = Self::fmt(&dwrite, coll, family, DWRITE_FONT_WEIGHT_SEMI_BOLD, 15.0 * s)?;
        let f_munit = Self::fmt(&dwrite, coll, family, DWRITE_FONT_WEIGHT_NORMAL, 10.0 * s)?;
        let f_mup = Self::fmt(&dwrite, coll, family, DWRITE_FONT_WEIGHT_NORMAL, 10.0 * s)?;

        let typo = unsafe { dwrite.CreateTypography()? };
        unsafe {
            typo.AddFontFeature(DWRITE_FONT_FEATURE {
                nameTag: DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES,
                parameter: 1,
            })?;
        }

        let b_ink = unsafe { rt.CreateSolidColorBrush(&c(18, 21, 26, 0.92), None)? };
        let b_ink2 = unsafe { rt.CreateSolidColorBrush(&c(18, 21, 26, 0.64), None)? };
        let b_ink3 = unsafe { rt.CreateSolidColorBrush(&c(18, 21, 26, 0.50), None)? };
        let b_ink6 = unsafe { rt.CreateSolidColorBrush(&c(18, 21, 26, 0.60), None)? };
        let b_ink55 = unsafe { rt.CreateSolidColorBrush(&c(18, 21, 26, 0.55), None)? };
        let b_hair72 = unsafe { rt.CreateSolidColorBrush(&c(255, 255, 255, 0.72), None)? };
        let b_hair85 = unsafe { rt.CreateSolidColorBrush(&c(255, 255, 255, 0.85), None)? };
        let b_dot = unsafe { rt.CreateSolidColorBrush(&c(52, 199, 89, 1.0), None)? };
        let b_base = unsafe { rt.CreateSolidColorBrush(&c(18, 21, 26, 0.10), None)? };
        let b_warn = unsafe { rt.CreateSolidColorBrush(&c(196, 106, 0, 1.0), None)? };
        let b_err = unsafe { rt.CreateSolidColorBrush(&c(255, 59, 48, 1.0), None)? };

        let pill_normal = Self::pill(&rt, w as f32, h as f32, island::glass_brightness(false, false))?;
        let pill_hot = Self::pill(&rt, w as f32, h as f32, island::glass_brightness(true, false))?;
        let pill_active = Self::pill(&rt, w as f32, h as f32, island::glass_brightness(false, true))?;

        // `.card` 的 172° 玻璃：普通/悬停两套 stop 之间插 9 档，hover 时逐帧量化。
        let (gs, ge) = Self::linear_172(w as f32, h as f32);
        let mut glass = Vec::with_capacity(9);
        for i in 0..=8 {
            let t = i as f32 / 8.0;
            let stops = Self::glass_stops(t);
            glass.push(Self::grad(&rt, &stops, gs, ge)?);
        }

        let hl1 = [
            D2D1_GRADIENT_STOP { position: 0.0, color: c(255, 255, 255, 0.28) },
            D2D1_GRADIENT_STOP { position: 0.58, color: c(255, 255, 255, 0.0) },
            D2D1_GRADIENT_STOP { position: 1.0, color: c(255, 255, 255, 0.0) },
        ];
        let b_hl1 = Self::radial(
            &rt,
            &hl1,
            Vector2::new(w as f32 * 0.18, -(h as f32) * 0.12),
            w as f32 * 1.2,
            h as f32 * 0.9,
        )?;
        let hl2 = [
            D2D1_GRADIENT_STOP { position: 0.0, color: c(255, 255, 255, 0.16) },
            D2D1_GRADIENT_STOP { position: 0.62, color: c(255, 255, 255, 0.0) },
            D2D1_GRADIENT_STOP { position: 1.0, color: c(255, 255, 255, 0.0) },
        ];
        let b_hl2 = Self::radial(
            &rt,
            &hl2,
            Vector2::new(w as f32, 0.0),
            w as f32 * 0.9,
            h as f32 * 0.7,
        )?;
        let blue = [
            D2D1_GRADIENT_STOP { position: 0.0, color: c(10, 132, 255, 0.045) },
            D2D1_GRADIENT_STOP { position: 0.62, color: c(10, 132, 255, 0.0) },
            D2D1_GRADIENT_STOP { position: 1.0, color: c(10, 132, 255, 0.0) },
        ];
        let b_blue = Self::radial(
            &rt,
            &blue,
            Vector2::new(w as f32 * 0.06, h as f32 * 1.08),
            w as f32 * 0.7,
            h as f32 * 0.5,
        )?;
        let bottom = [
            D2D1_GRADIENT_STOP { position: 0.72, color: c(18, 28, 48, 0.0) },
            D2D1_GRADIENT_STOP { position: 1.0, color: c(18, 28, 48, 0.035) },
        ];
        let b_bottom = Self::grad(
            &rt,
            &bottom,
            Vector2::new(0.0, 0.0),
            Vector2::new(0.0, h as f32),
        )?;

        let radius = card::RADIUS * s;
        let steps = 8;
        let card_path = Self::squircle_geometry(
            &factory,
            0.0,
            0.0,
            w as f32,
            h as f32,
            radius,
            card::EXPONENT,
            steps,
        )?;
        let inset = 0.25 * s;
        let card_rim = Self::squircle_geometry(
            &factory,
            inset,
            inset,
            w as f32 - inset * 2.0,
            h as f32 - inset * 2.0,
            (radius - inset).max(0.0),
            card::EXPONENT,
            steps,
        )?;
        let icon_scale = 11.0 * s / 16.0;
        let (icon_wifi, icon_wifi_slash) = Self::wifi_geometries(&factory, icon_scale)?;
        let icon_bolt = Self::bolt_geometry(&factory, icon_scale)?;

        Ok(Self {
            factory,
            dwrite,
            rt,
            caps,
            scale: s,
            w: w as f32,
            h: h as f32,
            mem_dc,
            family,
            collection,
            f_title,
            f_state,
            f_num,
            f_unit,
            f_up,
            f_upunit,
            f_foot,
            f_foot_b,
            f_cell,
            f_cell_val,
            f_mdown,
            f_munit,
            f_mup,
            typo,
            b_ink,
            b_ink2,
            b_ink3,
            b_ink6,
            b_ink55,
            b_hair72,
            b_hair85,
            b_dot,
            b_base,
            b_warn,
            b_err,
            glass,
            b_hl1,
            b_hl2,
            b_blue,
            b_bottom,
            card_path,
            card_rim,
            icon_wifi,
            icon_wifi_slash,
            icon_bolt,
            pill_normal,
            pill_hot,
            pill_active,
            island_static: RefCell::new(None),
            dot_tiles: RefCell::new(None),
            chart: RefCell::new(None),
        })
    }

    pub fn font_family(&self) -> &'static str {
        self.family
    }

    /// 自证：进程内 collection 是否真的能看到 Inter（否则会静默回退）。
    pub fn dwrite_has_family(&self, family: &str) -> bool {
        self.collection
            .as_ref()
            .map(|c| font::has_family(c, family))
            .unwrap_or(false)
    }

    /// 系统 collection 是否看到（预期 false：AddFontMemResourceEx 不喂 DWrite）。
    pub fn system_has_family(&self, family: &str) -> bool {
        font::system_has_family(&self.dwrite, family)
    }

    /// 诊断：collection 里 Inter 的字重列表。
    pub fn family_weights(&self, family: &str) -> Vec<i32> {
        self.collection
            .as_ref()
            .map(|c| font::family_weights(c, family))
            .unwrap_or_default()
    }

    fn fmt(
        dwrite: &IDWriteFactory,
        collection: Option<&IDWriteFontCollection>,
        family: &str,
        weight: DWRITE_FONT_WEIGHT,
        size: f32,
    ) -> Result<IDWriteTextFormat> {
        let fam = wide(family);
        let loc = wide("zh-CN");
        let f = unsafe {
            dwrite.CreateTextFormat(
                PCWSTR(fam.as_ptr()),
                collection,
                weight,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                PCWSTR(loc.as_ptr()),
            )?
        };
        unsafe {
            f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING)?;
            f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
        }
        Ok(f)
    }

    fn grad(
        rt: &ID2D1RenderTarget,
        stops: &[D2D1_GRADIENT_STOP],
        start: Vector2,
        end: Vector2,
    ) -> Result<ID2D1LinearGradientBrush> {
        let collection = unsafe { rt.CreateGradientStopCollection(stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)? };
        let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
            startPoint: start,
            endPoint: end,
        };
        unsafe { rt.CreateLinearGradientBrush(&props, None, &collection) }
    }

    fn radial(
        rt: &ID2D1RenderTarget,
        stops: &[D2D1_GRADIENT_STOP],
        center: Vector2,
        radius_x: f32,
        radius_y: f32,
    ) -> Result<ID2D1RadialGradientBrush> {
        let collection = unsafe { rt.CreateGradientStopCollection(stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)? };
        let props = D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES {
            center,
            gradientOriginOffset: Vector2::new(0.0, 0.0),
            radiusX: radius_x,
            radiusY: radius_y,
        };
        unsafe { rt.CreateRadialGradientBrush(&props, None, &collection) }
    }

    /// CSS linear-gradient(172deg)：方向从正上方顺时针转 172°，
    /// 渐变线过中心、长度 = |w·sinθ| + |h·cosθ|。
    fn linear_172(w: f32, h: f32) -> (Vector2, Vector2) {
        let theta = 172.0f32.to_radians();
        let dir = Vector2::new(theta.sin(), -theta.cos());
        let len = w * dir.X.abs() + h * dir.Y.abs();
        let center = Vector2::new(w / 2.0, h / 2.0);
        let half = Vector2::new(dir.X * len / 2.0, dir.Y * len / 2.0);
        (
            Vector2::new(center.X - half.X, center.Y - half.Y),
            Vector2::new(center.X + half.X, center.Y + half.Y),
        )
    }

    /// 普通 ↔ hover 玻璃 stop 插值（t=0 普通，t=1 hover）。
    fn glass_stops(t: f32) -> [D2D1_GRADIENT_STOP; 3] {
        let a = |normal: f32, hover: f32| normal + (hover - normal) * t;
        [
            D2D1_GRADIENT_STOP {
                position: 0.0,
                color: c(255, 255, 255, a(0.36, 0.44)),
            },
            D2D1_GRADIENT_STOP {
                position: 0.46,
                color: c(252, 253, 255, a(0.22, 0.28)),
            },
            D2D1_GRADIENT_STOP {
                position: 1.0,
                color: c(244, 247, 252, a(0.26, 0.32)),
            },
        ]
    }

    fn stop(pos: f32, r: u8, g: u8, b: u8, a: f32, brightness: f32) -> D2D1_GRADIENT_STOP {
        let m = |v: u8| (v as f32 * brightness).round().clamp(0.0, 255.0) as u8;
        D2D1_GRADIENT_STOP {
            position: pos,
            color: c(m(r), m(g), m(b), a),
        }
    }

    /// `.pillbg` 的三层渐变（色相压中性灰 + 两团白高光），按亮度滤镜预生成。
    fn pill(rt: &ID2D1RenderTarget, w: f32, h: f32, brightness: f32) -> Result<Pill> {
        let (start, end) = Self::linear_172(w, h);
        let base_stops = [
            Self::stop(0.0, 243, 245, 248, 0.36, brightness),
            Self::stop(0.46, 230, 233, 239, 0.22, brightness),
            Self::stop(1.0, 221, 225, 232, 0.26, brightness),
        ];
        let r1_stops = [
            Self::stop(0.0, 255, 255, 255, 0.28, brightness),
            Self::stop(0.58, 255, 255, 255, 0.0, brightness),
            Self::stop(1.0, 255, 255, 255, 0.0, brightness),
        ];
        let r2_stops = [
            Self::stop(0.0, 255, 255, 255, 0.16, brightness),
            Self::stop(0.62, 255, 255, 255, 0.0, brightness),
            Self::stop(1.0, 255, 255, 255, 0.0, brightness),
        ];
        Ok(Pill {
            base: Self::grad(rt, &base_stops, start, end)?,
            r1: Self::radial(rt, &r1_stops, Vector2::new(w * 0.18, -h * 0.12), w * 1.2, h * 0.9)?,
            r2: Self::radial(rt, &r2_stops, Vector2::new(w, 0.0), w * 0.9, h * 0.7)?,
        })
    }

    /// 35px 连续曲率卡片轮廓：四角超椭圆（分段三次贝塞尔），四边直线。
    fn squircle_geometry(
        factory: &ID2D1Factory,
        x0: f32,
        y0: f32,
        w: f32,
        h: f32,
        r: f32,
        n: f32,
        steps: usize,
    ) -> Result<ID2D1PathGeometry> {
        let pts = card::outline(w, h, r, n, steps);
        let m = steps + 1;
        let chains: [&[(f32, f32)]; 4] = [
            &pts[0..m],
            &pts[m..2 * m],
            &pts[2 * m..3 * m],
            &pts[3 * m..4 * m],
        ];
        let to_v = |(x, y): (f32, f32)| Vector2::new(x0 + x, y0 + y);
        let tl: Vec<Vector2> = chains[0].iter().map(|&p| to_v(p)).collect();
        let tr: Vec<Vector2> = chains[1].iter().map(|&p| to_v(p)).collect();
        let br: Vec<Vector2> = chains[2].iter().map(|&p| to_v(p)).collect();
        let bl: Vec<Vector2> = chains[3].iter().map(|&p| to_v(p)).collect();

        let geo = unsafe { factory.CreatePathGeometry()? };
        let sink = unsafe { geo.Open()? };
        unsafe {
            sink.BeginFigure(tl[0], D2D1_FIGURE_BEGIN_FILLED);
            Self::add_corner(&sink, &tl, Vector2::new(0.0, -1.0), Vector2::new(1.0, 0.0));
            sink.AddLine(tr[0]);
            Self::add_corner(&sink, &tr, Vector2::new(1.0, 0.0), Vector2::new(0.0, 1.0));
            sink.AddLine(br[0]);
            Self::add_corner(&sink, &br, Vector2::new(0.0, 1.0), Vector2::new(-1.0, 0.0));
            sink.AddLine(bl[0]);
            Self::add_corner(&sink, &bl, Vector2::new(-1.0, 0.0), Vector2::new(0.0, -1.0));
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            sink.Close()?;
        }
        Ok(geo)
    }

    /// 把一条角点链拟合成 C1 连续的三次贝塞尔；端点切线显式给定，
    /// 保证与直边相接处没有折角。
    fn add_corner(
        sink: &ID2D1GeometrySink,
        pts: &[Vector2],
        start_dir: Vector2,
        end_dir: Vector2,
    ) {
        let n = pts.len();
        let mut tangents = Vec::with_capacity(n);
        let start_len = vlen(vsub(pts[1], pts[0]));
        let end_len = vlen(vsub(pts[n - 1], pts[n - 2]));
        for i in 0..n {
            let t = if i == 0 {
                vmul(start_dir, start_len)
            } else if i == n - 1 {
                vmul(end_dir, end_len)
            } else {
                vmul(vsub(pts[i + 1], pts[i - 1]), 0.5)
            };
            tangents.push(t);
        }
        for i in 0..n - 1 {
            let seg = D2D1_BEZIER_SEGMENT {
                point1: vadd(pts[i], vmul(tangents[i], 1.0 / 3.0)),
                point2: vsub(pts[i + 1], vmul(tangents[i + 1], 1.0 / 3.0)),
                point3: pts[i + 1],
            };
            unsafe {
                sink.AddBezier(&seg);
            }
        }
    }

    /// Wi-Fi 图标（viewBox 16 → 11px）：两道弧 + 斜杠。
    fn wifi_geometries(
        factory: &ID2D1Factory,
        s: f32,
    ) -> Result<(ID2D1PathGeometry, ID2D1PathGeometry)> {
        let arcs = unsafe { factory.CreatePathGeometry()? };
        let sink = unsafe { arcs.Open()? };
        unsafe {
            sink.BeginFigure(Vector2::new(2.0 * s, 6.2 * s), D2D1_FIGURE_BEGIN_HOLLOW);
            sink.AddArc(&D2D1_ARC_SEGMENT {
                point: Vector2::new(14.0 * s, 6.2 * s),
                size: D2D_SIZE_F { width: 9.0 * s, height: 9.0 * s },
                rotationAngle: 0.0,
                sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                arcSize: D2D1_ARC_SIZE_SMALL,
            });
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            sink.BeginFigure(Vector2::new(4.4 * s, 8.7 * s), D2D1_FIGURE_BEGIN_HOLLOW);
            sink.AddArc(&D2D1_ARC_SEGMENT {
                point: Vector2::new(11.6 * s, 8.7 * s),
                size: D2D_SIZE_F { width: 5.4 * s, height: 5.4 * s },
                rotationAngle: 0.0,
                sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                arcSize: D2D1_ARC_SIZE_SMALL,
            });
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            sink.Close()?;
        }
        let slash = unsafe { factory.CreatePathGeometry()? };
        let sink = unsafe { slash.Open()? };
        unsafe {
            sink.BeginFigure(Vector2::new(2.6 * s, 13.4 * s), D2D1_FIGURE_BEGIN_HOLLOW);
            sink.AddLine(Vector2::new(13.4 * s, 2.6 * s));
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            sink.Close()?;
        }
        Ok((arcs, slash))
    }

    /// 代理闪电图标（viewBox 16 → 11px）。
    fn bolt_geometry(factory: &ID2D1Factory, s: f32) -> Result<ID2D1PathGeometry> {
        let pts = [
            (8.9, 1.9),
            (4.4, 8.9),
            (7.4, 8.9),
            (6.7, 14.1),
            (11.6, 6.7),
            (8.4, 6.7),
        ];
        let geo = unsafe { factory.CreatePathGeometry()? };
        let sink = unsafe { geo.Open()? };
        unsafe {
            sink.BeginFigure(Vector2::new(pts[0].0 * s, pts[0].1 * s), D2D1_FIGURE_BEGIN_HOLLOW);
            for p in &pts[1..] {
                sink.AddLine(Vector2::new(p.0 * s, p.1 * s));
            }
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            sink.Close()?;
        }
        Ok(geo)
    }

    fn p(&self, logical: f32) -> f32 {
        logical * self.scale
    }

    fn layout(&self, text: &str, fmt: &IDWriteTextFormat) -> Result<IDWriteTextLayout> {
        let w = wide(text);
        unsafe {
            self.dwrite.CreateTextLayout(
                &w[..w.len() - 1],
                fmt,
                self.p(600.0),
                self.p(160.0),
            )
        }
    }

    /// 单行文本的 (自然行高, 基线)。
    fn line_metrics(&self, text: &str, fmt: &IDWriteTextFormat) -> (f32, f32) {
        let Ok(lay) = self.layout(text, fmt) else {
            return (0.0, 0.0);
        };
        let mut lm = [DWRITE_LINE_METRICS::default(); 1];
        let mut n = 0u32;
        unsafe {
            let _ = lay.GetLineMetrics(Some(&mut lm), &mut n);
        }
        if n == 0 {
            (0.0, 0.0)
        } else {
            (lm[0].height, lm[0].baseline)
        }
    }

    /// tabular-nums（等宽数字）+ 可选字距（DIP，已乘 scale）。
    fn layout_feat(
        &self,
        text: &str,
        fmt: &IDWriteTextFormat,
        tabular: bool,
        tracking: f32,
    ) -> Result<IDWriteTextLayout> {
        let w = wide(text);
        let lay = unsafe {
            self.dwrite
                .CreateTextLayout(&w[..w.len() - 1], fmt, self.p(600.0), self.p(160.0))?
        };
        if tabular || tracking != 0.0 {
            let range = DWRITE_TEXT_RANGE {
                startPosition: 0,
                length: (w.len() - 1) as u32,
            };
            if let Ok(l1) = lay.cast::<IDWriteTextLayout1>() {
                unsafe {
                    if tabular {
                        let _ = l1.SetTypography(&self.typo, range);
                    }
                    if tracking != 0.0 {
                        let _ = l1.SetCharacterSpacing(0.0, tracking, 0.0, range);
                    }
                }
            }
        }
        Ok(lay)
    }

    fn measure_feat(
        &self,
        text: &str,
        fmt: &IDWriteTextFormat,
        tabular: bool,
        tracking: f32,
    ) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        match self.layout_feat(text, fmt, tabular, tracking) {
            Ok(lay) => {
                let mut m = DWRITE_TEXT_METRICS::default();
                unsafe {
                    let _ = lay.GetMetrics(&mut m);
                }
                m.widthIncludingTrailingWhitespace
            }
            Err(_) => 0.0,
        }
    }

    /// 按基线画单行文本，返回宽度。
    fn draw_baseline(
        &self,
        text: &str,
        fmt: &IDWriteTextFormat,
        x: f32,
        baseline: f32,
        brush: &ID2D1Brush,
        tabular: bool,
        tracking: f32,
    ) -> Result<f32> {
        if text.is_empty() {
            return Ok(0.0);
        }
        let lay = self.layout_feat(text, fmt, tabular, tracking)?;
        let mut lm = [DWRITE_LINE_METRICS::default(); 1];
        let mut n = 0u32;
        let mut m = DWRITE_TEXT_METRICS::default();
        unsafe {
            lay.GetLineMetrics(Some(&mut lm), &mut n)?;
            lay.GetMetrics(&mut m)?;
            self.rt.DrawTextLayout(
                Vector2::new(x, baseline - lm[0].baseline),
                &lay,
                brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
        Ok(m.widthIncludingTrailingWhitespace)
    }

    /// 单行文本按视觉行高垂直居中（对照 flex align-items:center）。
    fn draw_text_vcentered(
        &self,
        rt: &ID2D1RenderTarget,
        text: &str,
        fmt: &IDWriteTextFormat,
        x: f32,
        cy: f32,
        brush: &ID2D1Brush,
        tabular: bool,
        tracking: f32,
    ) -> Result<f32> {
        if text.is_empty() {
            return Ok(0.0);
        }
        let lay = self.layout_feat(text, fmt, tabular, tracking)?;
        let mut m = DWRITE_TEXT_METRICS::default();
        unsafe {
            lay.GetMetrics(&mut m)?;
        }
        unsafe {
            rt.DrawTextLayout(
                Vector2::new(x, cy - m.height / 2.0),
                &lay,
                brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
        Ok(m.widthIncludingTrailingWhitespace)
    }

    fn set_alpha(brush: &ID2D1Brush, a: f32) {
        unsafe {
            brush.SetOpacity(a.clamp(0.0, 1.0));
        }
    }

    fn tone_brush(&self, tone: card::Tone) -> &ID2D1SolidColorBrush {
        match tone {
            card::Tone::Normal => &self.b_ink2,
            card::Tone::Warn => &self.b_warn,
            card::Tone::Error => &self.b_err,
        }
    }

    /// 呼吸灯：实心圆 + 6 层同心圆近似 box-shadow 光晕。
    fn draw_dot(&self, cx: f32, cy: f32, dot: &DotPaint) -> Result<()> {
        let alpha = dot.alpha.clamp(0.0, 1.0);
        let (r, g, b) = dot.rgb;
        let core = self.p(3.0) * dot.scale;
        let blur = if dot.glow > 0.4 { 6.0 } else { 5.0 };
        let outer = core + self.p(blur);
        let ring_alpha = dot.glow * alpha * 0.22;
        unsafe {
            for i in (0..6).rev() {
                let t = (i as f32 + 1.0) / 6.0;
                let rr = core + (outer - core) * t;
                self.b_dot.SetColor(&c(r, g, b, ring_alpha));
                self.rt.FillEllipse(
                    &D2D1_ELLIPSE {
                        point: Vector2::new(cx, cy),
                        radiusX: rr,
                        radiusY: rr,
                    },
                    &self.b_dot,
                );
            }
            self.b_dot.SetColor(&c(r, g, b, alpha));
            self.rt.FillEllipse(
                &D2D1_ELLIPSE {
                    point: Vector2::new(cx, cy),
                    radiusX: core,
                    radiusY: core,
                },
                &self.b_dot,
            );
        }
        Ok(())
    }

    /// `.glyph` 地球：13px 方框、viewBox 16、stroke 1.7。
    fn draw_globe(&self, cx: f32, cy: f32) -> Result<()> {
        let r = self.p(4.55);
        let stroke = self.p(1.38);
        unsafe {
            let e = D2D1_ELLIPSE {
                point: Vector2::new(cx, cy),
                radiusX: r,
                radiusY: r,
            };
            self.rt.DrawEllipse(&e, &self.b_ink2, stroke, &self.caps);
            self.rt.DrawLine(
                Vector2::new(cx - r, cy),
                Vector2::new(cx + r, cy),
                &self.b_ink2,
                stroke,
                &self.caps,
            );
            let meridian = D2D1_ELLIPSE {
                point: Vector2::new(cx, cy),
                radiusX: r * 0.44,
                radiusY: r,
            };
            self.rt.DrawEllipse(&meridian, &self.b_ink2, stroke, &self.caps);
        }
        Ok(())
    }

    /// 上行箭头（viewBox 12 → 9px），底边落在基线上。
    fn draw_up_arrow(&self, x: f32, baseline: f32) -> Result<()> {
        let s = self.p(0.75);
        let top = baseline - self.p(9.0);
        let stroke = self.p(1.5);
        let px = |v: f32| x + v * s;
        let py = |v: f32| top + v * s;
        unsafe {
            self.rt.DrawLine(
                Vector2::new(px(6.0), py(9.6)),
                Vector2::new(px(6.0), py(3.0)),
                &self.b_ink3,
                stroke,
                &self.caps,
            );
            self.rt.DrawLine(
                Vector2::new(px(3.2), py(5.6)),
                Vector2::new(px(6.0), py(2.8)),
                &self.b_ink3,
                stroke,
                &self.caps,
            );
            self.rt.DrawLine(
                Vector2::new(px(6.0), py(2.8)),
                Vector2::new(px(8.8), py(5.6)),
                &self.b_ink3,
                stroke,
                &self.caps,
            );
        }
        Ok(())
    }

    fn draw_wifi(&self, x: f32, cy: f32) -> Result<()> {
        let s = 11.0 * self.scale / 16.0;
        let top = cy - self.p(5.5);
        unsafe {
            self.rt.SetTransform(&Matrix3x2::translation(x, top));
            self.rt.DrawGeometry(&self.icon_wifi, &self.b_ink3, 1.8 * s, &self.caps);
            self.rt.DrawGeometry(&self.icon_wifi_slash, &self.b_ink3, 1.5 * s, &self.caps);
            self.rt.SetTransform(&Matrix3x2::identity());
            self.b_dot.SetColor(&c(18, 21, 26, 0.50));
            self.rt.FillEllipse(
                &D2D1_ELLIPSE {
                    point: Vector2::new(x + 8.0 * s, top + 11.9 * s),
                    radiusX: 0.9 * s,
                    radiusY: 0.9 * s,
                },
                &self.b_dot,
            );
        }
        Ok(())
    }

    fn draw_bolt(&self, x: f32, cy: f32) -> Result<()> {
        let s = 11.0 * self.scale / 16.0;
        let top = cy - self.p(5.5);
        unsafe {
            self.rt.SetTransform(&Matrix3x2::translation(x, top));
            self.rt.DrawGeometry(&self.icon_bolt, &self.b_ink3, 1.8 * s, &self.caps);
            self.rt.SetTransform(&Matrix3x2::identity());
        }
        Ok(())
    }

    /// 60 点 3 点平滑曲线 + 填充区域；纵轴下限 64 KB/s（对照 `build()`）。
    fn spark_geometry(
        &self,
        values: &[f64],
        left: f32,
        right: f32,
        top: f32,
        base: f32,
        area_bottom: f32,
    ) -> Result<(ID2D1PathGeometry, ID2D1PathGeometry)> {
        let n = values.len();
        let max = values.iter().cloned().fold(0.0f64, f64::max).max(65536.0) * 1.18;
        let pts: Vec<Vector2> = (0..n)
            .map(|i| {
                let a = values[i.saturating_sub(1)];
                let b = values[(i + 1).min(n - 1)];
                let v = (a + values[i] * 2.0 + b) / 4.0;
                let x = left + (i as f32 / (n - 1) as f32) * (right - left);
                let y = base - (v / max) as f32 * (base - top);
                Vector2::new(x, y)
            })
            .collect();

        let line = unsafe { self.factory.CreatePathGeometry()? };
        {
            let sink = unsafe { line.Open()? };
            unsafe {
                sink.BeginFigure(pts[0], D2D1_FIGURE_BEGIN_HOLLOW);
                for i in 1..pts.len() - 1 {
                    let mid = Vector2::new(
                        (pts[i].X + pts[i + 1].X) / 2.0,
                        (pts[i].Y + pts[i + 1].Y) / 2.0,
                    );
                    sink.AddQuadraticBezier(&D2D1_QUADRATIC_BEZIER_SEGMENT {
                        point1: pts[i],
                        point2: mid,
                    });
                }
                sink.AddLine(pts[n - 1]);
                sink.EndFigure(D2D1_FIGURE_END_OPEN);
                sink.Close()?;
            }
        }

        let area = unsafe { self.factory.CreatePathGeometry()? };
        {
            let sink = unsafe { area.Open()? };
            unsafe {
                sink.BeginFigure(pts[0], D2D1_FIGURE_BEGIN_FILLED);
                for i in 1..pts.len() - 1 {
                    let mid = Vector2::new(
                        (pts[i].X + pts[i + 1].X) / 2.0,
                        (pts[i].Y + pts[i + 1].Y) / 2.0,
                    );
                    sink.AddQuadraticBezier(&D2D1_QUADRATIC_BEZIER_SEGMENT {
                        point1: pts[i],
                        point2: mid,
                    });
                }
                sink.AddLine(pts[n - 1]);
                sink.AddLine(Vector2::new(right, area_bottom));
                sink.AddLine(Vector2::new(left, area_bottom));
                sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                sink.Close()?;
            }
        }
        Ok((line, area))
    }

    fn chart_size(&self) -> (f32, f32) {
        (self.w - self.p(card::PAD_X) * 2.0, self.p(card::CHART_H))
    }

    /// 曲线坐标映射：viewBox 320×66 → 卡片上的 276×56（preserveAspectRatio=none）。
    fn chart_rect(&self) -> (f32, f32, f32, f32) {
        let left = self.p(card::PAD_X);
        let top = self.p(card::CARD_H - card::CHART_H);
        let (cw, ch) = self.chart_size();
        (left, top, cw, ch)
    }

    fn chart_key(f: &Frame) -> String {
        use std::fmt::Write as _;
        let mut k = String::new();
        if f.hist.iter().cloned().fold(0.0f64, f64::max) < 1024.0 {
            k.push('f');
        } else {
            for v in &f.hist {
                let _ = write!(k, "{v:.0},");
            }
        }
        k
    }

    fn new_chart_cache(&self) -> Result<ChartCache> {
        let (cw, ch) = self.chart_size();
        let size = D2D_SIZE_F {
            width: cw,
            height: ch,
        };
        let target = unsafe {
            self.rt.CreateCompatibleRenderTarget(
                Some(&size),
                None,
                None,
                D2D1_COMPATIBLE_RENDER_TARGET_OPTIONS_NONE,
            )?
        };
        let line_stops = [
            D2D1_GRADIENT_STOP { position: 0.0, color: c(10, 132, 255, 0.35) },
            D2D1_GRADIENT_STOP { position: 0.70, color: c(10, 132, 255, 1.0) },
            D2D1_GRADIENT_STOP { position: 1.0, color: c(0, 102, 214, 1.0) },
        ];
        let line = Self::grad(&target, &line_stops, Vector2::new(0.0, 0.0), Vector2::new(cw, 0.0))?;
        let area_stops = [
            D2D1_GRADIENT_STOP { position: 0.0, color: c(10, 132, 255, 0.30) },
            D2D1_GRADIENT_STOP { position: 0.55, color: c(10, 132, 255, 0.09) },
            D2D1_GRADIENT_STOP { position: 1.0, color: c(10, 132, 255, 0.0) },
        ];
        let area = Self::grad(&target, &area_stops, Vector2::new(0.0, 0.0), Vector2::new(0.0, ch))?;
        let base = unsafe { target.CreateSolidColorBrush(&c(18, 21, 26, 0.10), None)? };
        let bitmap = unsafe { target.GetBitmap()? };
        Ok(ChartCache {
            key: String::new(),
            target,
            bitmap,
            line,
            area,
            base,
        })
    }

    fn build_chart(&self, cache: &mut ChartCache, f: &Frame) -> Result<()> {
        let (cw, ch) = self.chart_size();
        let vy = |v: f32| v / 66.0 * ch;
        let top = vy(17.0);
        let base = vy(57.0);
        let target = &cache.target;
        unsafe {
            target.BeginDraw();
            target.Clear(Some(&c(0, 0, 0, 0.0)));
            target.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        }
        let flat = f.hist.iter().cloned().fold(0.0f64, f64::max) < 1024.0;
        if flat {
            unsafe {
                target.DrawLine(
                    Vector2::new(0.0, vy(57.6)),
                    Vector2::new(cw, vy(57.6)),
                    &cache.base,
                    self.p(0.6),
                    None,
                );
            }
        } else {
            let (line, area) = self.spark_geometry(&f.hist, 0.0, cw, top, base, ch)?;
            unsafe {
                cache.area.SetStartPoint(Vector2::new(0.0, 0.0));
                cache.area.SetEndPoint(Vector2::new(0.0, ch));
                target.FillGeometry(&area, &cache.area, None);
                cache.line.SetStartPoint(Vector2::new(0.0, 0.0));
                cache.line.SetEndPoint(Vector2::new(cw, 0.0));
                target.DrawGeometry(&line, &cache.line, self.p(1.5), &self.caps);
            }
        }
        unsafe {
            target.EndDraw(None, None)?;
        }
        cache.bitmap = unsafe { target.GetBitmap()? };
        Ok(())
    }

    fn ensure_chart(&self, f: &Frame) -> Result<()> {
        let key = Self::chart_key(f);
        {
            let cache = self.chart.borrow();
            if let Some(c) = cache.as_ref() {
                if c.key == key {
                    return Ok(());
                }
            }
        }
        let mut cache = self.chart.borrow_mut();
        if cache.is_none() {
            *cache = Some(self.new_chart_cache()?);
        }
        let c = cache.as_mut().unwrap();
        self.build_chart(c, f)?;
        c.key = key;
        Ok(())
    }

    /// 曲线 + 左右 26px 渐隐 mask。曲线先画进离屏位图，再用逐段 opacity 合成，
    /// 1 Hz 重建一次，hover 淡出只改每段透明度。
    fn draw_chart(&self, f: &Frame) -> Result<()> {
        let alpha = f.hover_fx.chart_alpha;
        if alpha <= 0.002 {
            return Ok(());
        }
        self.ensure_chart(f)?;
        let cache = self.chart.borrow();
        let c = cache.as_ref().unwrap();
        let (left, top, cw, ch) = self.chart_rect();
        let dest = D2D_RECT_F {
            left,
            top,
            right: left + cw,
            bottom: top + ch,
        };
        let fade = (26.0 * self.scale).round().max(1.0);
        let band = 2.0f32;
        let mut x = 0.0f32;
        while x < fade {
            let x1 = (x + band).min(fade);
            let mask = ((x + x1) / 2.0 / fade).clamp(0.0, 1.0);
            let clip = D2D_RECT_F {
                left: left + x,
                top,
                right: left + x1,
                bottom: top + ch,
            };
            unsafe {
                self.rt.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_ALIASED);
                self.rt.DrawBitmap(
                    &c.bitmap,
                    Some(&dest),
                    alpha * mask,
                    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                    None,
                );
                self.rt.PopAxisAlignedClip();
            }
            x = x1;
        }
        let mut x = 0.0f32;
        while x < fade {
            let x1 = (x + band).min(fade);
            let mask = ((x + x1) / 2.0 / fade).clamp(0.0, 1.0);
            let clip = D2D_RECT_F {
                left: left + cw - fade + x,
                top,
                right: left + cw - fade + x1,
                bottom: top + ch,
            };
            unsafe {
                self.rt.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_ALIASED);
                self.rt.DrawBitmap(
                    &c.bitmap,
                    Some(&dest),
                    alpha * mask,
                    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                    None,
                );
                self.rt.PopAxisAlignedClip();
            }
            x = x1;
        }
        let clip = D2D_RECT_F {
            left: left + fade,
            top,
            right: left + cw - fade,
            bottom: top + ch,
        };
        unsafe {
            self.rt.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_ALIASED);
            self.rt.DrawBitmap(
                &c.bitmap,
                Some(&dest),
                alpha,
                D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                None,
            );
            self.rt.PopAxisAlignedClip();
        }
        Ok(())
    }

    fn fit_feat(
        &self,
        text: &str,
        fmt: &IDWriteTextFormat,
        max_w: f32,
        tracking: f32,
    ) -> String {
        if self.measure_feat(text, fmt, false, tracking) <= max_w {
            return text.to_string();
        }
        let mut chars: Vec<char> = text.chars().collect();
        while !chars.is_empty() {
            chars.pop();
            let candidate: String = chars.iter().collect::<String>() + "…";
            if self.measure_feat(&candidate, fmt, false, tracking) <= max_w {
                return candidate;
            }
        }
        String::new()
    }

    /// 展开卡片：squircle 玻璃 + 排版 + 曲线 + hover 详情。
    pub fn paint(&self, f: &Frame) -> Result<()> {
        let w = self.w;
        let fx = f.hover_fx;

        unsafe {
            self.rt.BeginDraw();
            self.rt.Clear(Some(&c(0, 0, 0, 0.0)));
            self.rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            self.rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        }

        // 1. 玻璃：172° 主渐变 + 两团 radial 高光 + 底部蓝/暗角。
        let mix = (fx.glass.clamp(0.0, 1.0) * 8.0).round() as usize;
        unsafe {
            self.rt.FillGeometry(&self.card_path, &self.glass[mix], None);
            self.rt.FillGeometry(&self.card_path, &self.b_hl1, None);
            self.rt.FillGeometry(&self.card_path, &self.b_hl2, None);
            self.rt.FillGeometry(&self.card_path, &self.b_blue, None);
            self.rt.FillGeometry(&self.card_path, &self.b_bottom, None);
        }

        // 2. 内发丝：0.5px 白 .72 描边 + 顶部 0.5px 白 .85 高光（inset 在形状内）。
        unsafe {
            self.rt
                .DrawGeometry(&self.card_rim, &self.b_hair72, self.p(0.5), None);
            self.rt.DrawLine(
                Vector2::new(self.p(card::RADIUS), self.p(0.25)),
                Vector2::new(w - self.p(card::RADIUS), self.p(0.25)),
                &self.b_hair85,
                self.p(0.5),
                None,
            );
        }

        // 3. 曲线（在 .inner 下面）。
        self.draw_chart(f)?;

        // 4. 内容：整体随 hover 上浮 1px。
        let dy = self.p(fx.inner_dy);
        let left = self.p(card::PAD_X);
        let right = w - self.p(card::PAD_X);

        // .top：地球 + 标题 + 状态
        let cy = self.p(card::TOP_Y + card::TOP_H / 2.0) + dy;
        self.draw_globe(left + self.p(6.5), cy)?;
        let state_w = self.measure_feat(&f.state, &self.f_state, false, self.p(0.022));
        let title_x = left + self.p(19.0);
        let title_max = (right - state_w - self.p(6.0) - title_x).min(self.p(150.0));
        let title = self.fit_feat(&f.title, &self.f_title, title_max, self.p(-0.156));
        self.draw_text_vcentered(
            &self.rt,
            &title,
            &self.f_title,
            title_x,
            cy,
            &self.b_ink,
            false,
            self.p(-0.156),
        )?;
        self.draw_text_vcentered(
            &self.rt,
            &f.state,
            &self.f_state,
            right - state_w,
            cy,
            &self.b_ink6,
            false,
            self.p(0.022),
        )?;

        // .main：46px 大数字 + 单位 + 右侧上行。
        let (lh_num, asc_num) = self.line_metrics("0", &self.f_num);
        let num_base = self.p(card::MAIN_Y) + (self.p(card::MAIN_H) - lh_num) / 2.0 + asc_num + dy;
        let nw = self.draw_baseline(
            &f.down_value,
            &self.f_num,
            left,
            num_base,
            &self.b_ink,
            true,
            self.p(-1.932),
        )?;
        self.draw_baseline(
            &f.down_unit,
            &self.f_unit,
            left + nw + self.p(5.0),
            num_base,
            &self.b_ink3,
            false,
            self.p(-0.065),
        )?;

        let (lh_up, asc_up) = self.line_metrics("0", &self.f_up);
        let up_base = self.p(card::MAIN_Y + card::MAIN_H - 2.0) - (lh_up - asc_up) + dy;
        let u2_w = self.measure_feat(&f.up_unit, &self.f_upunit, false, 0.0);
        let u2_x = right - u2_w;
        self.draw_baseline(
            &f.up_unit,
            &self.f_upunit,
            u2_x,
            up_base,
            &self.b_ink3,
            false,
            0.0,
        )?;
        let up_w = self.measure_feat(&f.up_value, &self.f_up, true, 0.0);
        let up_x = u2_x - self.p(4.0) - up_w;
        self.draw_baseline(
            &f.up_value,
            &self.f_up,
            up_x,
            up_base,
            &self.b_ink2,
            true,
            0.0,
        )?;
        self.draw_up_arrow(up_x - self.p(3.0) - self.p(9.0), up_base)?;

        // .foot：Wi-Fi + 代理（hover 时淡出）。
        let foot_alpha = fx.foot_alpha;
        if foot_alpha > 0.002 {
            let (lh_foot, _) = self.line_metrics("Wi-Fi", &self.f_foot);
            let fcy = self.p(card::FOOT_Y) + lh_foot / 2.0 + dy;
            for b in [&self.b_ink2, &self.b_ink3, &self.b_warn, &self.b_err] {
                Self::set_alpha(b, foot_alpha);
            }
            self.draw_wifi(left, fcy)?;
            let mut x = left + self.p(15.0);
            let w1 = self.draw_text_vcentered(
                &self.rt,
                "Wi-Fi",
                &self.f_foot,
                x,
                fcy,
                &self.b_ink2,
                false,
                self.p(0.044),
            )?;
            x += w1 + self.p(3.0);
            let wifi_fmt = if f.wifi_ok { &self.f_foot_b } else { &self.f_foot };
            x += self.draw_text_vcentered(
                &self.rt,
                &f.wifi,
                wifi_fmt,
                x,
                fcy,
                &self.b_ink2,
                false,
                0.0,
            )?;
            x += self.p(14.0);
            self.draw_bolt(x, fcy)?;
            x += self.p(15.0);
            let w2 = self.draw_text_vcentered(
                &self.rt,
                "代理",
                &self.f_foot,
                x,
                fcy,
                &self.b_ink2,
                false,
                self.p(0.044),
            )?;
            x += w2 + self.p(3.0);
            self.draw_text_vcentered(
                &self.rt,
                &f.proxy,
                &self.f_foot_b,
                x,
                fcy,
                self.tone_brush(f.proxy_tone),
                false,
                0.0,
            )?;
            for b in [&self.b_ink2, &self.b_ink3, &self.b_warn, &self.b_err] {
                Self::set_alpha(b, 1.0);
            }
        }

        // .details：5 格错峰进场（3 行 × 2 列，第 5 格 span2）。
        let container_alpha = fx.details_alpha;
        if container_alpha > 0.002 {
            let (lh_cell, asc_cell) = self.line_metrics("延迟", &self.f_cell);
            let layout = card::details_layout(lh_cell / self.scale, asc_cell / self.scale);
            let row_base = |i: usize| self.p(layout.baselines[i]) + dy + self.p(fx.details_dy);
            let cells = fx.cells;
            for (i, row) in [(0usize, 0usize), (1, 0), (2, 1), (3, 1), (4, 2)] {
                let cell = &f.details[i];
                let alpha = container_alpha * cells[i].alpha;
                if alpha <= 0.002 {
                    continue;
                }
                let y = row_base(row) + self.p(cells[i].dy);
                let value_w = self.measure_feat(&cell.value, &self.f_cell_val, true, self.p(-0.053));
                let label_brush = &self.b_ink2;
                let value_brush = self.tone_brush(cell.tone);
                Self::set_alpha(label_brush, alpha);
                Self::set_alpha(value_brush, alpha);
                self.draw_baseline(&cell.label, &self.f_cell, left, y, label_brush, false, 0.0)?;
                self.draw_baseline(
                    &cell.value,
                    &self.f_cell_val,
                    right - value_w,
                    y,
                    value_brush,
                    true,
                    self.p(-0.053),
                )?;
                Self::set_alpha(label_brush, 1.0);
                Self::set_alpha(value_brush, 1.0);
            }
            // span2 顶线（.5px rgba(18,21,26,.075)）。
            let a = container_alpha * cells[4].alpha;
            if a > 0.002 {
                let y = self.p(layout.border_y) + dy + self.p(fx.details_dy + cells[4].dy);
                Self::set_alpha(&self.b_base, a * 0.75);
                unsafe {
                    self.rt.DrawLine(
                        Vector2::new(left, y),
                        Vector2::new(right, y),
                        &self.b_base,
                        self.p(0.5),
                        None,
                    );
                }
                Self::set_alpha(&self.b_base, 1.0);
            }
        }

        unsafe {
            self.rt.EndDraw(None, None)?;
        }
        Ok(())
    }

    /// 胶囊态（T3）：144×34 DIP 玻璃胶囊 + 呼吸灯 + 迷你数字。
    /// 静态层（玻璃/文字/发丝）画进独立 DIB；呼吸帧只做两次 GDI 拷贝：
    /// BitBlt 静态层 → 主 DIB，AlphaBlend 预渲染的呼吸点相位帧。
    /// 这样 15 fps 呼吸不再每帧触发 D2D 的 BeginDraw/EndDraw 与全表面渐变重绘。
    pub fn paint_island(&self, f: &Frame) -> Result<()> {
        let (static_dc, dot_cx) = self.ensure_island_static(f)?;
        self.ensure_dot_tiles(&f.dot)?;
        let (tile, strip_dc, phase) = {
            let cache = self.dot_tiles.borrow();
            let t = cache.as_ref().unwrap();
            (
                t.tile,
                t.surface.mem_dc,
                f.dot.phase.min(island::BREATHE_PHASES - 1),
            )
        };
        unsafe {
            let _ = BitBlt(
                self.mem_dc,
                0,
                0,
                self.w as i32,
                self.h as i32,
                Some(static_dc),
                0,
                0,
                SRCCOPY,
            );
            let left = (dot_cx - tile as f32 / 2.0).round() as i32;
            let top = (self.h / 2.0 - tile as f32 / 2.0).round() as i32;
            let blend = BLENDFUNCTION {
                BlendOp: 0, // AC_SRC_OVER
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: 1, // AC_SRC_ALPHA
            };
            let _ = AlphaBlend(
                self.mem_dc,
                left,
                top,
                tile,
                tile,
                strip_dc,
                phase as i32 * tile,
                0,
                tile,
                tile,
                blend,
            );
        }
        Ok(())
    }

    fn island_static_key(f: &Frame) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}",
            f.down_value, f.down_unit, f.up_value, f.up_unit, f.hover, f.active
        )
    }

    /// 内容/悬停/按下没变时复用上一张静态 DIB；变了才重画一次。
    fn ensure_island_static(&self, f: &Frame) -> Result<(HDC, f32)> {
        let key = Self::island_static_key(f);
        if let Some(c) = self.island_static.borrow().as_ref() {
            if c.key == key {
                return Ok((c.surface.mem_dc, c.dot_cx));
            }
        }
        if self.island_static.borrow().is_none() {
            *self.island_static.borrow_mut() = Some(IslandStatic {
                key: String::new(),
                dot_cx: 0.0,
                surface: window::Surface::new(None, self.w as i32, self.h as i32)?,
            });
        }
        let mut cache = self.island_static.borrow_mut();
        let st = cache.as_mut().unwrap();
        let dot_cx = self.redraw_island_static(st, f)?;
        st.key = key;
        st.dot_cx = dot_cx;
        Ok((st.surface.mem_dc, dot_cx))
    }

    /// 画静态层（玻璃 + 迷你数字 + 内发丝），返回呼吸灯圆心 x。
    fn redraw_island_static(&self, st: &mut IslandStatic, f: &Frame) -> Result<f32> {
        let w = self.w;
        let h = self.h;
        let radius = island::capsule_radius(w, h, self.scale);
        let body = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F {
                left: 0.0,
                top: 0.0,
                right: w,
                bottom: h,
            },
            radiusX: radius,
            radiusY: radius,
        };
        let pill = if f.active {
            &self.pill_active
        } else if f.hover {
            &self.pill_hot
        } else {
            &self.pill_normal
        };
        let rect = RECT {
            left: 0,
            top: 0,
            right: self.w as i32,
            bottom: self.h as i32,
        };
        unsafe {
            self.rt.BindDC(st.surface.mem_dc, &rect)?;
        }
        let result = (|| -> Result<f32> {
            unsafe {
                self.rt.BeginDraw();
                self.rt.Clear(Some(&c(0, 0, 0, 0.0)));
                self.rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
                self.rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
                self.rt.FillRoundedRectangle(&body, &pill.base);
                self.rt.FillRoundedRectangle(&body, &pill.r1);
                self.rt.FillRoundedRectangle(&body, &pill.r2);
            }

            // 迷你一行：点 + 5px + 下行数字 + 单位 + 上行，整体水平居中。
            let gap = self.p(5.0);
            let up = format!("↑ {} {}", f.up_value, f.up_unit);
            let down_w = self.measure_feat(&f.down_value, &self.f_mdown, true, -0.3 * self.scale);
            let unit_w = self.measure_feat(&f.down_unit, &self.f_munit, false, 0.0);
            let up_w = self.measure_feat(&up, &self.f_mup, true, 0.0);
            let total = self.p(6.0) + gap + down_w + gap + unit_w + gap + up_w;
            let mut x = (w - total) / 2.0;
            let cy = h / 2.0;
            let dot_cx = x + self.p(3.0);
            x += self.p(6.0) + gap;
            self.draw_text_vcentered(
                &self.rt,
                &f.down_value,
                &self.f_mdown,
                x,
                cy,
                &self.b_ink,
                true,
                -0.3 * self.scale,
            )?;
            x += down_w + gap;
            self.draw_text_vcentered(
                &self.rt,
                &f.down_unit,
                &self.f_munit,
                x,
                cy,
                &self.b_ink6,
                false,
                0.0,
            )?;
            x += unit_w + gap;
            self.draw_text_vcentered(&self.rt, &up, &self.f_mup, x, cy, &self.b_ink55, true, 0.0)?;

            // 内发丝：0.5px 白 .72 描边 + 顶部 0.5px 白 .85 高光。
            let inset = self.p(0.25);
            let rim = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: inset,
                    top: inset,
                    right: w - inset,
                    bottom: h - inset,
                },
                radiusX: (radius - inset).max(0.0),
                radiusY: (radius - inset).max(0.0),
            };
            unsafe {
                self.rt
                    .DrawRoundedRectangle(&rim, &self.b_hair72, self.p(0.5), None);
                self.rt.DrawLine(
                    Vector2::new(radius, inset),
                    Vector2::new(w - radius, inset),
                    &self.b_hair85,
                    self.p(0.5),
                    None,
                );
                self.rt.EndDraw(None, None)?;
            }
            Ok(dot_cx)
        })();
        unsafe {
            self.rt.BindDC(self.mem_dc, &rect)?;
        }
        result
    }

    /// 预渲染 32 个呼吸相位帧（颜色/光晕变了才重画），存成一条位图。
    fn ensure_dot_tiles(&self, dot: &DotPaint) -> Result<()> {
        let key = format!("{},{},{},{:.3}", dot.rgb.0, dot.rgb.1, dot.rgb.2, dot.glow);
        let blur = if dot.glow > 0.4 { 6.0 } else { 5.0 };
        let tile = ((3.0 + blur) * 2.0 * self.scale).ceil() as i32 + 2;
        {
            let cache = self.dot_tiles.borrow();
            if let Some(t) = cache.as_ref() {
                if t.key == key && t.tile == tile {
                    return Ok(());
                }
            }
        }
        let mut cache = self.dot_tiles.borrow_mut();
        let resize = cache.as_ref().map(|t| t.tile != tile).unwrap_or(true);
        if resize {
            *cache = Some(DotTiles {
                key: String::new(),
                tile,
                surface: window::Surface::new(
                    None,
                    tile * island::BREATHE_PHASES as i32,
                    tile,
                )?,
            });
        }
        let t = cache.as_mut().unwrap();
        let rect = RECT {
            left: 0,
            top: 0,
            right: tile * island::BREATHE_PHASES as i32,
            bottom: tile,
        };
        unsafe {
            self.rt.BindDC(t.surface.mem_dc, &rect)?;
        }
        let result = (|| -> Result<()> {
            unsafe {
                self.rt.BeginDraw();
                self.rt.Clear(Some(&c(0, 0, 0, 0.0)));
                self.rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            }
            for i in 0..island::BREATHE_PHASES {
                let (alpha, scale) = island::breathe_at(
                    i as f64 / island::BREATHE_PHASES as f64 * 2000.0,
                );
                let m = Matrix3x2::translation((i as i32 * tile) as f32, 0.0);
                unsafe {
                    self.rt.SetTransform(&m);
                }
                self.draw_dot(
                    tile as f32 / 2.0,
                    tile as f32 / 2.0,
                    &DotPaint {
                        rgb: dot.rgb,
                        glow: dot.glow,
                        alpha,
                        scale,
                        phase: i,
                    },
                )?;
            }
            unsafe {
                self.rt.SetTransform(&Matrix3x2::identity());
                self.rt.EndDraw(None, None)?;
            }
            Ok(())
        })();
        unsafe {
            self.rt.BindDC(self.mem_dc, &rect)?;
        }
        t.key = key;
        result
    }

    /// 岛态绘制成本分解（--bench）：GDI 静态层 BitBlt + 呼吸点 AlphaBlend。
    pub fn bench_island_parts(&self, f: &Frame) -> Result<()> {
        let (static_dc, dot_cx) = self.ensure_island_static(f)?;
        self.ensure_dot_tiles(&f.dot)?;
        let (tile, strip_dc) = {
            let cache = self.dot_tiles.borrow();
            let t = cache.as_ref().unwrap();
            (t.tile, t.surface.mem_dc)
        };
        let n = 300;
        let left = (dot_cx - tile as f32 / 2.0).round() as i32;
        let top = (self.h / 2.0 - tile as f32 / 2.0).round() as i32;
        let blend = BLENDFUNCTION {
            BlendOp: 0,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: 1,
        };
        let t = std::time::Instant::now();
        for i in 0..n {
            unsafe {
                let _ = BitBlt(
                    self.mem_dc,
                    0,
                    0,
                    self.w as i32,
                    self.h as i32,
                    Some(static_dc),
                    0,
                    0,
                    SRCCOPY,
                );
                let _ = AlphaBlend(
                    self.mem_dc,
                    left,
                    top,
                    tile,
                    tile,
                    strip_dc,
                    (i % island::BREATHE_PHASES as usize) as i32 * tile,
                    0,
                    tile,
                    tile,
                    blend,
                );
            }
        }
        eprintln!(
            "  gdi blit+tile        {:.4} ms",
            t.elapsed().as_secs_f64() * 1000.0 / n as f64
        );
        Ok(())
    }

    /// Micro-benchmark hook used by `--bench` to attribute paint cost.
    pub fn bench_parts(&self, f: &Frame) -> Result<()> {
        let t = std::time::Instant::now();
        for _ in 0..200 { let _ = self.layout(&f.title, &self.f_title)?; }
        eprintln!("  layout(title)        {:.3} ms", t.elapsed().as_secs_f64() * 1000.0 / 200.0);
        let t = std::time::Instant::now();
        for _ in 0..200 { let _ = self.layout("2.9", &self.f_num)?; }
        eprintln!("  layout(number 46px)  {:.3} ms", t.elapsed().as_secs_f64() * 1000.0 / 200.0);
        let t = std::time::Instant::now();
        let (cw, ch) = self.chart_size();
        for _ in 0..200 { let _ = self.spark_geometry(&f.hist, 0.0, cw, 8.0, ch - 10.0, ch)?; }
        eprintln!("  spark geometry       {:.3} ms", t.elapsed().as_secs_f64() * 1000.0 / 200.0);
        Ok(())
    }

    pub fn dimensions(&self) -> (f32, f32) {
        (self.w, self.h)
    }
}
