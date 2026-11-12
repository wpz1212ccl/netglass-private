//! Direct2D + DirectWrite rendering into a premultiplied-ARGB DIB.
//!
//! Why this stack (see report): Direct2D's DC render target writes straight into
//! the 32bpp DIB that `UpdateLayeredWindow` consumes, keeps per-pixel alpha,
//! antialiases geometry in software and lets DirectWrite shape CJK + Latin with
//! grayscale AA (ClearType is unusable on a transparent surface).

use windows::core::{PCWSTR, Result};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::HDC;
use windows_numerics::Vector2;

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

    f_title: IDWriteTextFormat,
    f_state: IDWriteTextFormat,
    f_num: IDWriteTextFormat,
    f_unit: IDWriteTextFormat,
    f_up: IDWriteTextFormat,
    f_upunit: IDWriteTextFormat,
    f_foot: IDWriteTextFormat,

    b_ink: ID2D1SolidColorBrush,
    b_ink2: ID2D1SolidColorBrush,
    b_ink3: ID2D1SolidColorBrush,
    b_white: ID2D1SolidColorBrush,
    b_base: ID2D1SolidColorBrush,
    b_glass: ID2D1LinearGradientBrush,
    b_glass_hover: ID2D1LinearGradientBrush,
    b_line: ID2D1LinearGradientBrush,
    b_area: ID2D1LinearGradientBrush,
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

        let b_ink = unsafe { rt.CreateSolidColorBrush(&c(18, 21, 26, 0.92), None)? };
        let b_ink2 = unsafe { rt.CreateSolidColorBrush(&c(18, 21, 26, 0.64), None)? };
        let b_ink3 = unsafe { rt.CreateSolidColorBrush(&c(18, 21, 26, 0.50), None)? };
        let b_white = unsafe { rt.CreateSolidColorBrush(&c(255, 255, 255, 0.90), None)? };
        let b_base = unsafe { rt.CreateSolidColorBrush(&c(18, 21, 26, 0.10), None)? };

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
            f_title,
            f_state,
            f_num,
            f_unit,
            f_up,
            f_upunit,
            f_foot,
            b_ink,
            b_ink2,
            b_ink3,
            b_white,
            b_base,
            b_glass,
            b_glass_hover,
            b_line,
            b_area,
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
        rt: &ID2D1DCRenderTarget,
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
