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
    pub wifi: String,
    pub proxy: String,
    pub hist: Vec<f64>,
    pub hover: bool,
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

    f_title: IDWriteTextFormat,
    f_state: IDWriteTextFormat,
    f_num: IDWriteTextFormat,
    f_unit: IDWriteTextFormat,
    f_up: IDWriteTextFormat,
    f_upunit: IDWriteTextFormat,
    f_foot: IDWriteTextFormat,
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
    b_white: ID2D1SolidColorBrush,
    b_hair72: ID2D1SolidColorBrush,
    b_hair85: ID2D1SolidColorBrush,
    b_dot: ID2D1SolidColorBrush,
    b_base: ID2D1SolidColorBrush,
    b_glass: ID2D1LinearGradientBrush,
    b_glass_hover: ID2D1LinearGradientBrush,
    b_line: ID2D1LinearGradientBrush,
    b_area: ID2D1LinearGradientBrush,
    pill_normal: Pill,
    pill_hot: Pill,
    pill_active: Pill,
    /// 胶囊静态层：玻璃 + 文字 + 发丝画进一张独立 DIB（内容/悬停变化才重画）。
    /// 呼吸帧用 GDI BitBlt 把它整块拷回主 DIB，不再走 D2D。
    island_static: RefCell<Option<IslandStatic>>,
    /// 呼吸点预渲染：32 个相位帧排成一条位图，呼吸帧只 AlphaBlend 一格。
    dot_tiles: RefCell<Option<DotTiles>>,
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
        let display = "Segoe UI Variable Display";
        let text = "Segoe UI Variable Text";

        let f_title = Self::fmt(&dwrite, text, DWRITE_FONT_WEIGHT_SEMI_BOLD, 13.0 * s)?;
        let f_state = Self::fmt(&dwrite, text, DWRITE_FONT_WEIGHT_NORMAL, 11.0 * s)?;
        let f_num = Self::fmt(&dwrite, display, DWRITE_FONT_WEIGHT_SEMI_BOLD, 44.0 * s)?;
        let f_unit = Self::fmt(&dwrite, text, DWRITE_FONT_WEIGHT_MEDIUM, 13.0 * s)?;
        let f_up = Self::fmt(&dwrite, text, DWRITE_FONT_WEIGHT_SEMI_BOLD, 14.0 * s)?;
        let f_upunit = Self::fmt(&dwrite, text, DWRITE_FONT_WEIGHT_MEDIUM, 11.0 * s)?;
        let f_foot = Self::fmt(&dwrite, text, DWRITE_FONT_WEIGHT_MEDIUM, 11.0 * s)?;
        // 胶囊迷你数字：15px/600 + 10px 单位 + 10px 上行（#mDown/#mUnit/.mup）。
        let f_mdown = Self::fmt(&dwrite, display, DWRITE_FONT_WEIGHT_SEMI_BOLD, 15.0 * s)?;
        let f_munit = Self::fmt(&dwrite, text, DWRITE_FONT_WEIGHT_NORMAL, 10.0 * s)?;
        let f_mup = Self::fmt(&dwrite, text, DWRITE_FONT_WEIGHT_NORMAL, 10.0 * s)?;

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
        let b_white = unsafe { rt.CreateSolidColorBrush(&c(255, 255, 255, 0.90), None)? };
        let b_hair72 = unsafe { rt.CreateSolidColorBrush(&c(255, 255, 255, 0.72), None)? };
        let b_hair85 = unsafe { rt.CreateSolidColorBrush(&c(255, 255, 255, 0.85), None)? };
        let b_dot = unsafe { rt.CreateSolidColorBrush(&c(52, 199, 89, 1.0), None)? };
        let b_base = unsafe { rt.CreateSolidColorBrush(&c(18, 21, 26, 0.10), None)? };

        let pill_normal = Self::pill(&rt, w as f32, h as f32, island::glass_brightness(false, false))?;
        let pill_hot = Self::pill(&rt, w as f32, h as f32, island::glass_brightness(true, false))?;
        let pill_active = Self::pill(&rt, w as f32, h as f32, island::glass_brightness(false, true))?;

        let glass_stops = [
            D2D1_GRADIENT_STOP {
                position: 0.0,
                color: c(255, 255, 255, 0.34),
            },
            D2D1_GRADIENT_STOP {
                position: 1.0,
                color: c(246, 248, 252, 0.20),
            },
        ];
        let b_glass = Self::grad(&rt, &glass_stops, Vector2::new(0.0, 0.0), Vector2::new(0.0, h as f32))?;

        let hover_stops = [
            D2D1_GRADIENT_STOP {
                position: 0.0,
                color: c(255, 255, 255, 0.42),
            },
            D2D1_GRADIENT_STOP {
                position: 1.0,
                color: c(246, 248, 252, 0.26),
            },
        ];
        let b_glass_hover = Self::grad(&rt, &hover_stops, Vector2::new(0.0, 0.0), Vector2::new(0.0, h as f32))?;

        let line_stops = [
            D2D1_GRADIENT_STOP {
                position: 0.0,
                color: c(10, 132, 255, 0.35),
            },
            D2D1_GRADIENT_STOP {
                position: 0.70,
                color: c(10, 132, 255, 1.0),
            },
            D2D1_GRADIENT_STOP {
                position: 1.0,
                color: c(0, 102, 214, 1.0),
            },
        ];
        let b_line = Self::grad(&rt, &line_stops, Vector2::new(0.0, 0.0), Vector2::new(w as f32, 0.0))?;

        let area_stops = [
            D2D1_GRADIENT_STOP {
                position: 0.0,
                color: c(10, 132, 255, 0.30),
            },
            D2D1_GRADIENT_STOP {
                position: 0.55,
                color: c(10, 132, 255, 0.09),
            },
            D2D1_GRADIENT_STOP {
                position: 1.0,
                color: c(10, 132, 255, 0.0),
            },
        ];
        let b_area = Self::grad(&rt, &area_stops, Vector2::new(0.0, 0.0), Vector2::new(0.0, h as f32))?;

        Ok(Self {
            factory,
            dwrite,
            rt,
            caps,
            scale: s,
            w: w as f32,
            h: h as f32,
            mem_dc,
            f_title,
            f_state,
            f_num,
            f_unit,
            f_up,
            f_upunit,
            f_foot,
            f_mdown,
            f_munit,
            f_mup,
            typo,
            b_ink,
            b_ink2,
            b_ink3,
            b_ink6,
            b_ink55,
            b_white,
            b_hair72,
            b_hair85,
            b_dot,
            b_base,
            b_glass,
            b_glass_hover,
            b_line,
            b_area,
            pill_normal,
            pill_hot,
            pill_active,
            island_static: RefCell::new(None),
            dot_tiles: RefCell::new(None),
        })
    }

    fn fmt(dwrite: &IDWriteFactory, family: &str, weight: DWRITE_FONT_WEIGHT, size: f32) -> Result<IDWriteTextFormat> {
        let fam = wide(family);
        let loc = wide("zh-CN");
        let f = unsafe {
            dwrite.CreateTextFormat(
                PCWSTR(fam.as_ptr()),
                None,
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

    /// Draws `text` with its baseline at `baseline_y`, left edge at `x`. Returns advance width.
    fn draw_text(
        &self,
        text: &str,
        fmt: &IDWriteTextFormat,
        x: f32,
        baseline_y: f32,
        brush: &ID2D1Brush,
    ) -> Result<f32> {
        if text.is_empty() {
            return Ok(0.0);
        }
        let lay = self.layout(text, fmt)?;
        let mut m = DWRITE_TEXT_METRICS::default();
        let mut lm = [DWRITE_LINE_METRICS::default(); 1];
        let mut n = 0u32;
        unsafe {
            lay.GetMetrics(&mut m)?;
            lay.GetLineMetrics(Some(&mut lm), &mut n)?;
        }
        let origin = Vector2::new(x, baseline_y - lm[0].baseline);
        unsafe {
            self.rt.DrawTextLayout(origin, &lay, brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
        }
        Ok(m.widthIncludingTrailingWhitespace)
    }

    fn measure(&self, text: &str, fmt: &IDWriteTextFormat) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        match self.layout(text, fmt) {
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

    /// 胶囊数字用：tabular-nums（等宽数字）+ 可选字距。
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

    fn draw_globe(&self, cx: f32, cy: f32) -> Result<()> {
        let r = self.p(6.3);
        unsafe {
            let e = D2D1_ELLIPSE {
                point: Vector2::new(cx, cy),
                radiusX: r,
                radiusY: r,
            };
            self.rt.DrawEllipse(&e, &self.b_ink2, self.p(1.15), &self.caps);
            self.rt.DrawLine(
                Vector2::new(cx - r, cy),
                Vector2::new(cx + r, cy),
                &self.b_ink2,
                self.p(1.1),
                &self.caps,
            );
            let meridian = D2D1_ELLIPSE {
                point: Vector2::new(cx, cy),
                radiusX: r * 0.44,
                radiusY: r,
            };
            self.rt.DrawEllipse(&meridian, &self.b_ink2, self.p(1.1), &self.caps);
        }
        Ok(())
    }

    fn draw_up_arrow(&self, x: f32, baseline_y: f32) -> Result<()> {
        let w = self.p(9.0);
        let top = baseline_y - self.p(8.6);
        let mid = x + w / 2.0;
        unsafe {
            self.rt.DrawLine(
                Vector2::new(mid, baseline_y - self.p(0.6)),
                Vector2::new(mid, top),
                &self.b_ink3,
                self.p(1.6),
                &self.caps,
            );
            self.rt.DrawLine(
                Vector2::new(mid - w * 0.32, top + w * 0.32),
                Vector2::new(mid, top),
                &self.b_ink3,
                self.p(1.6),
                &self.caps,
            );
            self.rt.DrawLine(
                Vector2::new(mid, top),
                Vector2::new(mid + w * 0.32, top + w * 0.32),
                &self.b_ink3,
                self.p(1.6),
                &self.caps,
            );
        }
        Ok(())
    }

    fn spark_geometry(
        &self,
        values: &[f64],
        left: f32,
        right: f32,
        top: f32,
        base: f32,
    ) -> Result<(ID2D1PathGeometry, ID2D1PathGeometry)> {
        let n = values.len();
        // 纵轴下限 64 KB/s，并做 3 点平滑：低速时不要把几 KB 的噪声放大成满屏山丘。
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
                sink.AddLine(Vector2::new(right, base));
                sink.AddLine(Vector2::new(left, base));
                sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                sink.Close()?;
            }
        }
        Ok((line, area))
    }

    pub fn paint(&self, f: &Frame) -> Result<()> {
        let w = self.w;
        let h = self.h;

        unsafe {
            self.rt.BeginDraw();
            self.rt.Clear(Some(&c(0, 0, 0, 0.0)));
            self.rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            self.rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);

            // 1. The glass body IS the ellipse: one antialiased fill, so the
            //    alpha channel is exactly the shape and nothing else paints
            //    outside it (verified by --selftest, which audits every pixel).
            let body = D2D1_ELLIPSE {
                point: Vector2::new(w / 2.0, h / 2.0),
                radiusX: w / 2.0,
                radiusY: h / 2.0,
            };
            let glass: &ID2D1LinearGradientBrush = if f.hover { &self.b_glass_hover } else { &self.b_glass };
            self.rt.FillEllipse(&body, glass);

            // 2. Text block.
            let left = self.p(54.0);
            let right = w - self.p(54.0);
            let title_base = self.p(54.0);
            let num_base = self.p(116.0);
            let foot_base = self.p(146.0);

            self.draw_globe(left + self.p(6.3), title_base - self.p(4.6))?;
            let x = left + self.p(19.0);
            let title = self.fit(&f.title, &self.f_title, right - x - self.p(64.0));
            let _ = self.draw_text(&title, &self.f_title, x, title_base, &self.b_ink)?;

            let sw = self.measure(&f.state, &self.f_state);
            self.draw_text(&f.state, &self.f_state, right - sw, title_base, &self.b_ink2)?;

            let nw = self.draw_text(&f.down_value, &self.f_num, left, num_base, &self.b_ink)?;
            self.draw_text(
                &f.down_unit,
                &self.f_unit,
                left + nw + self.p(6.0),
                num_base - self.p(1.0),
                &self.b_ink3,
            )?;

            let uw = self.measure(&f.up_value, &self.f_up);
            let uuw = self.measure(&f.up_unit, &self.f_upunit);
            let unit_x = right - uuw;
            self.draw_text(&f.up_unit, &self.f_upunit, unit_x, num_base - self.p(1.0), &self.b_ink3)?;
            let val_x = unit_x - self.p(3.0) - uw;
            self.draw_text(&f.up_value, &self.f_up, val_x, num_base - self.p(1.0), &self.b_ink2)?;
            self.draw_up_arrow(val_x - self.p(12.5), num_base - self.p(1.0))?;

            // 3. Foot row: Wi-Fi + proxy.
            let fw = self.draw_text(&f.wifi, &self.f_foot, left, foot_base, &self.b_ink2)?;
            self.draw_text(&f.proxy, &self.f_foot, left + fw + self.p(14.0), foot_base, &self.b_ink2)?;

            // 4. Sparkline.
            let cl = self.p(92.0);
            let cr = w - self.p(92.0);
            let ctop = self.p(154.0);
            let cbase = self.p(186.0);
            let flat = f.hist.iter().cloned().fold(0.0f64, f64::max) < 1024.0;
            if flat {
                self.rt.DrawLine(
                    Vector2::new(cl, cbase),
                    Vector2::new(cr, cbase),
                    &self.b_base,
                    self.p(0.7),
                    None,
                );
            } else {
                let (line, area) = self.spark_geometry(&f.hist, cl, cr, ctop + self.p(2.0), cbase)?;
                self.b_area.SetStartPoint(Vector2::new(0.0, ctop));
                self.b_area.SetEndPoint(Vector2::new(0.0, cbase));
                self.rt.FillGeometry(&area, &self.b_area, None);
                self.b_line.SetStartPoint(Vector2::new(cl, 0.0));
                self.b_line.SetEndPoint(Vector2::new(cr, 0.0));
                self.rt.DrawGeometry(&line, &self.b_line, self.p(1.5), &self.caps);
            }

            // 5. Hairline rim: 0.5 logical px of white, sits on the AA edge.
            let rim = D2D1_ELLIPSE {
                point: Vector2::new(w / 2.0, h / 2.0),
                radiusX: w / 2.0 - self.p(0.6),
                radiusY: h / 2.0 - self.p(0.6),
            };
            self.rt.DrawEllipse(&rim, &self.b_white, self.p(0.6), None);

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
        eprintln!("  layout(number 44px)  {:.3} ms", t.elapsed().as_secs_f64() * 1000.0 / 200.0);
        let t = std::time::Instant::now();
        for _ in 0..200 { let _ = self.spark_geometry(&f.hist, 0.0, 300.0, 0.0, 40.0)?; }
        eprintln!("  spark geometry       {:.3} ms", t.elapsed().as_secs_f64() * 1000.0 / 200.0);
        Ok(())
    }

    /// Truncates with an ellipsis so a long interface alias can never paint
    /// past the ellipse.
    fn fit(&self, text: &str, fmt: &IDWriteTextFormat, max_w: f32) -> String {
        if self.measure(text, fmt) <= max_w {
            return text.to_string();
        }
        let mut chars: Vec<char> = text.chars().collect();
        while !chars.is_empty() {
            chars.pop();
            let candidate: String = chars.iter().collect::<String>() + "…";
            if self.measure(&candidate, fmt) <= max_w {
                return candidate;
            }
        }
        String::new()
    }

    pub fn dimensions(&self) -> (f32, f32) {
        (self.w, self.h)
    }
}
