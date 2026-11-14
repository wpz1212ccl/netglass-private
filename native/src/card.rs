//! 展开卡片的纯几何/排版逻辑：squircle 轮廓、CSS 对照尺寸、hover 详情文案。
//!
//! 所有数值对照 `src/index.html` 的 `.card` / `.top` / `.main` / `.foot` /
//! `.details` / `.chart`；绘制（D2D）在 render.rs。

/// `border-radius:35px`（逻辑 px）。
pub const RADIUS: f32 = 35.0;
/// `corner-shape:squircle` 用超椭圆指数 4（Lamé 曲线 |x|⁴+|y|⁴=1）。
pub const EXPONENT: f32 = 4.0;

pub const PAD_X: f32 = 22.0;
pub const TOP_Y: f32 = 18.0;
pub const TOP_H: f32 = 16.0;
pub const MAIN_Y: f32 = 56.0;
pub const MAIN_H: f32 = 46.0;
pub const FOOT_Y: f32 = 120.0;
pub const CHART_H: f32 = 56.0;
pub const DETAILS_TOP: f32 = 106.0;
pub const DETAILS_BOTTOM: f32 = 12.0;
pub const DETAILS_GAP_X: f32 = 26.0;
/// `.cell.span2` 的 border-top 前留白：margin-top 3 + padding-top 9。
pub const DETAILS_SPAN_PAD: f32 = 12.0;

/// 右上角 state 的右边界 = 卡片宽 - 22。
pub fn content_right(card_w: f32) -> f32 {
    card_w - PAD_X
}

/// 卡片高度（逻辑 px）——CSS 窗口 320×200。
pub const CARD_H: f32 = 200.0;

/// 左上角超椭圆四分之一：从 (0, r)（左边切线点）到 (r, 0)（上边切线点）。
/// u = cos^(2/n)θ、v = sin^(2/n)θ 保证 uⁿ+vⁿ=1。
pub fn corner_points(r: f32, n: f32, steps: usize) -> Vec<(f32, f32)> {
    let steps = steps.max(1);
    (0..=steps)
        .map(|i| {
            let theta = std::f32::consts::FRAC_PI_2 * (i as f32 / steps as f32);
            // 端点处 cos(π/2) 在 f32 下是 -4e-8，直接 powf 会得到 NaN。
            let c = theta.cos().max(0.0).powf(2.0 / n);
            let s = theta.sin().max(0.0).powf(2.0 / n);
            (r * (1.0 - c), r * (1.0 - s))
        })
        .collect()
}

/// 顺时针闭合轮廓（左上 → 右上 → 右下 → 左下），不含重复的闭合点。
/// 每个角都从"上一条边"的切线点走向"下一条边"的切线点。
pub fn outline(w: f32, h: f32, r: f32, n: f32, steps: usize) -> Vec<(f32, f32)> {
    let c = corner_points(r, n, steps);
    let mut out = Vec::with_capacity(c.len() * 4);
    out.extend(c.iter().map(|&(x, y)| (x, y))); // 左上：左→上
    out.extend(c.iter().rev().map(|&(x, y)| (w - x, y))); // 右上：上→右
    out.extend(c.iter().map(|&(x, y)| (w - x, h - y))); // 右下：右→下
    out.extend(c.iter().rev().map(|&(x, y)| (x, h - y))); // 左下：下→左
    out
}

/// 点到卡片 squircle 的有符号距离（px，正=在形状外）。
/// 直边用圆角矩形 SDF；角落用同一超椭圆的归一化半径近似。
pub fn outside_distance(px: f32, py: f32, w: f32, h: f32, r: f32, n: f32) -> f32 {
    let qx = (px - w / 2.0).abs() - (w / 2.0 - r);
    let qy = (py - h / 2.0).abs() - (h / 2.0 - r);
    match (qx > 0.0, qy > 0.0) {
        (false, false) => qx.max(qy) - r,
        (true, false) => qx - r,
        (false, true) => qy - r,
        (true, true) => {
            let u = qx / r;
            let v = qy / r;
            ((u.powf(n) + v.powf(n)).powf(1.0 / n) - 1.0) * r
        }
    }
}

/// 大数字基线：CSS `.num` 字号 46 / line-height 46，在 `.main` 顶部 56 处
/// 把自然行高居中到 46px 行盒后，基线 = 行盒顶 + 半行距 + ascent。
pub fn num_baseline(line_height: f32, ascent: f32) -> f32 {
    MAIN_Y + (MAIN_H - line_height) / 2.0 + ascent
}

/// 详情三行的基线（逻辑 px）与 span2 顶线 y。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DetailsLayout {
    pub baselines: [f32; 3],
    pub border_y: f32,
}

/// grid 2 列 × 3 行、align-content:space-between：
/// 第 3 行是 span2（margin-top 3 + padding-top 9 + 文本行）。
pub fn details_layout(line_height: f32, ascent: f32) -> DetailsLayout {
    let span_h = DETAILS_SPAN_PAD + line_height;
    let total = line_height * 2.0 + span_h;
    let gap = ((CARD_H - DETAILS_BOTTOM - DETAILS_TOP) - total) / 2.0;
    let rows = [
        DETAILS_TOP,
        DETAILS_TOP + line_height + gap,
        DETAILS_TOP + (line_height + gap) * 2.0,
    ];
    DetailsLayout {
        baselines: [
            rows[0] + ascent,
            rows[1] + ascent,
            rows[2] + DETAILS_SPAN_PAD + ascent,
        ],
        border_y: rows[2] + 3.0,
    }
}

/// 速率格式化（与前端 `fmtPlain` 一致）：<1 KB/s 显示 0.0 MB/s。
pub fn format_rate(v: f64) -> (String, String) {
    if !v.is_finite() || v < 1024.0 {
        return ("0.0".to_string(), "MB/s".to_string());
    }
    let mb = v / 1048576.0;
    if mb >= 1.0 {
        (format!("{mb:.1}"), "MB/s".to_string())
    } else {
        (format!("{}", (v / 1024.0).round() as i64), "KB/s".to_string())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tone {
    Normal,
    Warn,
    Error,
}

#[derive(Clone, PartialEq, Debug)]
pub struct DetailRow {
    pub label: &'static str,
    pub value: String,
    pub tone: Tone,
    pub span2: bool,
}

/// 代理状态（从 `probe::ProbePayload` 归一化，便于纯逻辑单测）。
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ProxyStatus<'a> {
    /// 加速器正在加速（`accelerating[0]`）。
    Accelerating(&'a str),
    /// 代理隧道可用，时延 ms。
    Via(f64),
    /// 电池/未探测：显示"暂停"。
    Paused,
    /// 代理端口在监听但隧道不通。
    Blocked,
    /// 加速器在运行但未加速。
    NotAccelerating(&'a str),
    /// 未启用代理/加速器。
    Off,
}

/// 底栏 `.foot` 的代理值（`Watt 加速中` / `23 ms` / `暂停` / `不通` / `Watt 未加速` / `未启用`）。
pub fn proxy_foot(s: ProxyStatus<'_>) -> (String, Tone) {
    match s {
        ProxyStatus::Accelerating(a) => (format!("{a} 加速中"), Tone::Normal),
        ProxyStatus::Via(ms) => (format!("{} ms", ms.round() as i64), Tone::Normal),
        ProxyStatus::Paused => ("暂停".to_string(), Tone::Warn),
        ProxyStatus::Blocked => ("不通".to_string(), Tone::Error),
        ProxyStatus::NotAccelerating(a) => (format!("{a} 未加速"), Tone::Normal),
        ProxyStatus::Off => ("未启用".to_string(), Tone::Warn),
    }
}

/// 详情第 5 格的代理值（`加速中` / `23 ms` / `暂停` / `不通` / `未加速` / `未启用`）。
pub fn proxy_detail(s: ProxyStatus<'_>) -> (String, Tone) {
    match s {
        ProxyStatus::Accelerating(_) => ("加速中".to_string(), Tone::Normal),
        ProxyStatus::Via(ms) => (format!("{} ms", ms.round() as i64), Tone::Normal),
        ProxyStatus::Paused => ("暂停".to_string(), Tone::Warn),
        ProxyStatus::Blocked => ("不通".to_string(), Tone::Error),
        ProxyStatus::NotAccelerating(_) => ("未加速".to_string(), Tone::Normal),
        ProxyStatus::Off => ("未启用".to_string(), Tone::Warn),
    }
}

/// 详情 5 格（DOM 顺序）：延迟、峰值、链路、上行峰值、代理（span2）。
pub fn detail_rows(
    latency_ms: Option<f64>,
    peak_down: f64,
    link_mbps: Option<f64>,
    peak_up: f64,
    proxy: (&str, Tone),
) -> [DetailRow; 5] {
    let rate = |v: f64| {
        let (n, u) = format_rate(v);
        format!("{n} {u}")
    };
    [
        DetailRow {
            label: "延迟",
            value: latency_ms
                .map(|ms| format!("{} ms", ms.round() as i64))
                .unwrap_or_else(|| "—".to_string()),
            tone: Tone::Normal,
            span2: false,
        },
        DetailRow {
            label: "峰值",
            value: rate(peak_down),
            tone: Tone::Normal,
            span2: false,
        },
        DetailRow {
            label: "链路",
            value: link_mbps
                .map(|mbps| format!("{mbps:.0} Mbps"))
                .unwrap_or_else(|| "—".to_string()),
            tone: Tone::Normal,
            span2: false,
        },
        DetailRow {
            label: "上行峰值",
            value: rate(peak_up),
            tone: Tone::Normal,
            span2: false,
        },
        DetailRow {
            label: "代理",
            value: proxy.0.to_string(),
            tone: proxy.1,
            span2: true,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn squircle_corner_hits_tangent_points_and_is_monotonic() {
        let pts = corner_points(35.0, 4.0, 16);
        assert_eq!(pts.len(), 17);
        assert!(close(pts[0].0, 0.0) && close(pts[0].1, 35.0), "左边切线点");
        let last = pts[pts.len() - 1];
        assert!(close(last.0, 35.0) && close(last.1, 0.0), "上边切线点");
        for w in pts.windows(2) {
            assert!(w[1].0 >= w[0].0 - 1e-4, "x 单调递增");
            assert!(w[1].1 <= w[0].1 + 1e-4, "y 单调递减");
        }
        // 超椭圆 n=4 在 45° 处的半径是 r·(1-2^-1/4) ≈ 0.1591r。
        let mid = pts[8];
        assert!(close(mid.0, 35.0 * (1.0 - 0.5f32.powf(0.25))));
        assert!(close(mid.1, mid.0));
    }

    #[test]
    fn outline_stays_inside_card_and_closes() {
        let (w, h, r) = (320.0, 200.0, 35.0);
        let pts = outline(w, h, r, 4.0, 8);
        assert_eq!(pts.len(), 36);
        for (x, y) in &pts {
            assert!(*x >= -1e-4 && *x <= w + 1e-4, "x={x} 越界");
            assert!(*y >= -1e-4 && *y <= h + 1e-4, "y={y} 越界");
        }
        assert!(close(pts[0].0, 0.0) && close(pts[0].1, r), "从左上切点开始");
        let last = pts[pts.len() - 1];
        assert!(close(last.0, 0.0) && close(last.1, h - r), "左下切点收尾");
    }

    #[test]
    fn outside_distance_matches_squircle() {
        let (w, h, r, n) = (320.0, 200.0, 35.0, 4.0);
        // 中心与直边内侧为负。
        assert!(outside_distance(w / 2.0, h / 2.0, w, h, r, n) < 0.0);
        assert!(outside_distance(w / 2.0, 0.5, w, h, r, n) < 0.0);
        assert!(close(outside_distance(w / 2.0, 0.0, w, h, r, n), 0.0));
        // 外角点：超椭圆半径 (2)^(1/4)·r - r。
        let corner = (2.0f32.powf(0.25) - 1.0) * r;
        assert!(close(outside_distance(0.0, 0.0, w, h, r, n), corner));
        // 曲线 45° 点（距左上角 (0.1591r, 0.1591r)）在边界上。
        let m = r * (1.0 - 0.5f32.powf(0.25));
        assert!(close(outside_distance(m, m, w, h, r, n), 0.0));
        // 圆角矩形 SDF 会把这个点判在内部，squircle 不会——这正是要区分的。
        assert!(outside_distance(m, m, w, h, r, 2.0) > 1.0);
    }

    #[test]
    fn num_baseline_centers_natural_line_in_46px_box() {
        // Inter 46px 自然行高 ≈ 55.7、ascent ≈ 44.6。
        let b = num_baseline(55.7, 44.6);
        assert!(close(b, 56.0 + (46.0 - 55.7) / 2.0 + 44.6), "baseline={b}");
    }

    #[test]
    fn details_rows_fill_the_grid_bottom() {
        let l = details_layout(12.7, 9.7);
        assert!(close(l.baselines[0], 115.7));
        // 第三行文本底部正好落在 top:106 + bottom:12 的容器底。
        let row3_top = l.baselines[2] - 9.7 - DETAILS_SPAN_PAD;
        assert!(close(row3_top + DETAILS_SPAN_PAD + 12.7, 200.0 - 12.0));
        assert!(close(l.border_y, row3_top + 3.0));
        assert!(l.baselines[0] < l.baselines[1] && l.baselines[1] < l.baselines[2]);
    }

    #[test]
    fn detail_rows_match_css_values() {
        let rows = detail_rows(
            Some(23.4),
            2.9 * 1048576.0,
            Some(1000.0),
            148.0 * 1024.0,
            ("加速中", Tone::Normal),
        );
        assert_eq!(rows[0].value, "23 ms");
        assert_eq!(rows[1].value, "2.9 MB/s");
        assert_eq!(rows[2].value, "1000 Mbps");
        assert_eq!(rows[3].value, "148 KB/s");
        assert_eq!(rows[4].value, "加速中");
        assert!(rows[4].span2);

        let empty = detail_rows(None, 0.0, None, 0.0, ("未启用", Tone::Warn));
        assert_eq!(empty[0].value, "—");
        assert_eq!(empty[1].value, "0.0 MB/s");
        assert_eq!(empty[2].value, "—");
        assert_eq!(empty[4].tone, Tone::Warn);
    }

    #[test]
    fn proxy_status_maps_to_foot_and_detail_text() {
        assert_eq!(
            proxy_foot(ProxyStatus::Accelerating("Watt")),
            ("Watt 加速中".to_string(), Tone::Normal)
        );
        assert_eq!(proxy_foot(ProxyStatus::Via(23.4)).0, "23 ms");
        assert_eq!(proxy_foot(ProxyStatus::Paused), ("暂停".to_string(), Tone::Warn));
        assert_eq!(proxy_foot(ProxyStatus::Blocked), ("不通".to_string(), Tone::Error));
        assert_eq!(
            proxy_foot(ProxyStatus::NotAccelerating("Watt")).0,
            "Watt 未加速"
        );
        assert_eq!(proxy_foot(ProxyStatus::Off), ("未启用".to_string(), Tone::Warn));

        assert_eq!(proxy_detail(ProxyStatus::Accelerating("Watt")).0, "加速中");
        assert_eq!(proxy_detail(ProxyStatus::Via(7.6)).0, "8 ms");
        assert_eq!(proxy_detail(ProxyStatus::Blocked).1, Tone::Error);
        assert_eq!(proxy_detail(ProxyStatus::NotAccelerating("Watt")).0, "未加速");
    }

    #[test]
    fn format_rate_matches_frontend_fmt_plain() {
        assert_eq!(format_rate(0.0), ("0.0".to_string(), "MB/s".to_string()));
        assert_eq!(format_rate(148.0 * 1024.0).0, "148");
        assert_eq!(format_rate(2.94 * 1048576.0).0, "2.9");
    }
}
