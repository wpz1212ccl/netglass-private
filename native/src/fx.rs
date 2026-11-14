//! Hover 详情时间轴：CSS `cubic-bezier(.6,0,.25,1)` 的纯 Rust 复刻。
//!
//! 对照 `src/index.html`：
//! - `.details` opacity .22s / transform .4s，delay .06s（进场）
//! - `.cell` opacity .2s / transform .42s，错峰 delay .04/.08/.12/.16/.20s
//! - `.foot` 淡出 .07s；`.chart` 淡出 .1s linear
//! - `.inner` translateY(-1px) .56s
//! - `.card:hover` 背景 .34s
//! 离场：details .1s ease、cell delay 0、foot .1s ease delay .12s、
//! chart .24s delay .18s（对照 `.card:not(:hover) ...` 覆盖规则）。
//!
//! 这里只有时间→数值的纯函数，绘制在 render.rs，定时器在 main.rs。

/// 详情卡片错峰进场的格子数（`.cell` × 5）。
pub const CELLS: usize = 5;

/// CSS cubic-bezier(x1,y1,x2,y2)：按 x 求参数，再取 y。
/// 两端 x 导数非负，单调，用二分求根（60 次迭代误差 < 1e-15）。
pub fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let curve = |u: f32| 3.0 * (1.0 - u) * (1.0 - u) * u * x1 + 3.0 * (1.0 - u) * u * u * x2 + u * u * u;
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..60 {
        let u = (lo + hi) / 2.0;
        if curve(u) < x {
            lo = u;
        } else {
            hi = u;
        }
    }
    let u = (lo + hi) / 2.0;
    3.0 * (1.0 - u) * (1.0 - u) * u * y1 + 3.0 * (1.0 - u) * u * u * y2 + u * u * u
}

/// 卡片动效统一曲线（CSS cubic-bezier(.6,0,.25,1)）。
pub fn ease_morph(t: f32) -> f32 {
    cubic_bezier(0.6, 0.0, 0.25, 1.0, t)
}

/// CSS `ease`（cubic-bezier(.25,.1,.25,1)），离场淡出用。
pub fn ease(t: f32) -> f32 {
    cubic_bezier(0.25, 0.1, 0.25, 1.0, t)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellFx {
    pub alpha: f32,
    /// 相对最终位置的垂直偏移（逻辑 px，正=向下）。
    pub dy: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HoverFx {
    pub details_alpha: f32,
    pub details_dy: f32,
    pub cells: [CellFx; CELLS],
    pub foot_alpha: f32,
    pub chart_alpha: f32,
    pub inner_dy: f32,
    /// 0 = 普通玻璃，1 = `.card:hover` 提亮后的玻璃。
    pub glass: f32,
}

impl HoverFx {
    /// 未悬停：详情透明、底栏/曲线可见、内容不动。
    pub const IDLE: Self = Self {
        details_alpha: 0.0,
        details_dy: 4.0,
        cells: [CellFx { alpha: 0.0, dy: 5.0 }; CELLS],
        foot_alpha: 1.0,
        chart_alpha: 1.0,
        inner_dy: 0.0,
        glass: 0.0,
    };

    /// 悬停完成态：详情全显、底栏/曲线淡出、内容上浮 1px。
    pub const HOVER: Self = Self {
        details_alpha: 1.0,
        details_dy: 0.0,
        cells: [CellFx { alpha: 1.0, dy: 0.0 }; CELLS],
        foot_alpha: 0.0,
        chart_alpha: 0.0,
        inner_dy: -1.0,
        glass: 1.0,
    };
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// 一个通道在 elapsed 时刻的进度：0 表示还没开始，1 表示结束。
fn channel(elapsed_ms: f64, delay_ms: f64, dur_ms: f64, curve: fn(f32) -> f32) -> f32 {
    if elapsed_ms <= delay_ms {
        return 0.0;
    }
    let t = ((elapsed_ms - delay_ms) / dur_ms).min(1.0) as f32;
    curve(t)
}

/// 进场总时长：最后一个 cell 的 transform 在 0.20s 延迟后跑 0.42s。
pub const ENTER_MS: f64 = 620.0;
/// 离场总时长：`.inner` 的 .56s 最长。
pub const LEAVE_MS: f64 = 560.0;

/// 一次 hover 进/出场动画。开始瞬间记录当前值，中途反向也不跳变
/// （与 CSS transition 从当前计算值继续的行为一致）。
#[derive(Clone, Copy, Debug)]
pub struct HoverAnim {
    from: HoverFx,
    start_ms: f64,
    entering: bool,
    active: bool,
}

impl Default for HoverAnim {
    fn default() -> Self {
        Self::new()
    }
}

impl HoverAnim {
    pub fn new() -> Self {
        Self {
            from: HoverFx::IDLE,
            start_ms: 0.0,
            entering: false,
            active: false,
        }
    }

    pub fn begin(&mut self, now_ms: f64, entering: bool) {
        self.from = self.at(now_ms);
        self.start_ms = now_ms;
        self.entering = entering;
        self.active = true;
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn reset(&mut self) {
        self.from = HoverFx::IDLE;
        self.active = false;
    }

    pub fn duration(&self) -> f64 {
        if self.entering {
            ENTER_MS
        } else {
            LEAVE_MS
        }
    }

    pub fn done(&self, now_ms: f64) -> bool {
        !self.active || now_ms - self.start_ms >= self.duration()
    }

    /// 结束并落在最终状态（定时器收尾时调用）。
    pub fn finish(&mut self) {
        self.from = if self.entering {
            HoverFx::HOVER
        } else {
            HoverFx::IDLE
        };
        self.active = false;
    }

    pub fn at(&self, now_ms: f64) -> HoverFx {
        if !self.active {
            return self.from;
        }
        let e = now_ms - self.start_ms;
        // (目标, cell 延迟, cell transform 时长, details 延迟, details transform 时长,
        //  foot 延迟, foot 时长, chart 延迟, chart 时长)
        let (target, cell_delay, cell_dur, details_delay, details_dur, foot_delay, foot_dur,
             chart_delay, chart_dur) = if self.entering
        {
            (HoverFx::HOVER, 40.0, 420.0, 60.0, 400.0, 0.0, 70.0, 0.0, 100.0)
        } else {
            (HoverFx::IDLE, 0.0, 420.0, 0.0, 100.0, 120.0, 100.0, 180.0, 240.0)
        };
        let curve = ease_morph as fn(f32) -> f32;
        let f = self.from;
        let mut out = target;

        let d_alpha_dur = if self.entering { 220.0 } else { 100.0 };
        let d_curve = if self.entering {
            ease_morph as fn(f32) -> f32
        } else {
            ease as fn(f32) -> f32
        };
        let d = channel(e, details_delay, d_alpha_dur, d_curve);
        out.details_alpha = lerp(f.details_alpha, target.details_alpha, d);
        let d = channel(e, details_delay, details_dur, curve);
        out.details_dy = lerp(f.details_dy, target.details_dy, d);

        let c_alpha_curve = ease_morph as fn(f32) -> f32;
        for (i, cell) in out.cells.iter_mut().enumerate() {
            let delay = if self.entering {
                cell_delay * (i as f64 + 1.0)
            } else {
                cell_delay
            };
            let a = channel(e, delay, 200.0, c_alpha_curve);
            cell.alpha = lerp(f.cells[i].alpha, target.cells[i].alpha, a);
            let d = channel(e, delay, cell_dur, curve);
            cell.dy = lerp(f.cells[i].dy, target.cells[i].dy, d);
        }

        let foot_curve = ease as fn(f32) -> f32;
        let a = channel(e, foot_delay, foot_dur, foot_curve);
        out.foot_alpha = lerp(f.foot_alpha, target.foot_alpha, a);

        let chart_curve = if self.entering {
            linear as fn(f32) -> f32
        } else {
            ease_morph as fn(f32) -> f32
        };
        let a = channel(e, chart_delay, chart_dur, chart_curve);
        out.chart_alpha = lerp(f.chart_alpha, target.chart_alpha, a);

        let i = channel(e, 0.0, 560.0, curve);
        out.inner_dy = lerp(f.inner_dy, target.inner_dy, i);

        let g = channel(e, 0.0, 340.0, curve);
        out.glass = lerp(f.glass, target.glass, g);

        out
    }
}

fn linear(t: f32) -> f32 {
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn cubic_bezier_matches_css_reference() {
        // 参考值由独立实现（二分求 x 根）算出。
        let cases = [
            (0.0, 0.0),
            (0.1, 0.010788),
            (0.25, 0.091499),
            (0.5, 0.655681),
            (0.75, 0.949601),
            (0.9, 0.993397),
            (1.0, 1.0),
        ];
        for (x, y) in cases {
            let got = ease_morph(x);
            assert!(close(got, y), "x={x} got={got} want={y}");
        }
        assert!(close(ease(0.5), 0.802403), "CSS ease 中点");
    }

    #[test]
    fn hover_enter_staggers_cells() {
        let mut a = HoverAnim::new();
        a.begin(0.0, true);

        let t0 = a.at(0.0);
        assert_eq!(t0, HoverFx::IDLE, "开始瞬间不跳变");

        // 39ms：第 1 格还没到 40ms 延迟。
        let t39 = a.at(39.0);
        assert_eq!(t39.cells[0].alpha, 0.0);
        assert_eq!(t39.cells[4].alpha, 0.0);
        // 底栏/曲线立即开始淡出。
        assert!(t39.foot_alpha < 1.0);
        assert!(t39.chart_alpha < 1.0);

        // 100ms：第 1 格在动，第 5 格还没开始（延迟 200ms）。
        let t100 = a.at(100.0);
        assert!(t100.cells[0].alpha > 0.0 && t100.cells[0].alpha < 1.0);
        assert_eq!(t100.cells[4].alpha, 0.0, "0.04s 步进：第 5 格 0.2s 才起");
        assert!(t100.details_alpha > 0.0, "详情容器 0.06s 起");
        assert!(t100.inner_dy < 0.0, "内容开始上浮");

        let end = a.at(ENTER_MS);
        assert!(close(end.details_alpha, 1.0));
        assert!(close(end.details_dy, 0.0));
        for c in end.cells {
            assert!(close(c.alpha, 1.0) && close(c.dy, 0.0));
        }
        assert!(close(end.foot_alpha, 0.0));
        assert!(close(end.chart_alpha, 0.0));
        assert!(close(end.inner_dy, -1.0));
        assert!(close(end.glass, 1.0));
        assert!(a.done(ENTER_MS));
    }

    #[test]
    fn hover_leave_is_continuous_and_quick() {
        let mut a = HoverAnim::new();
        a.begin(0.0, true);
        let mid = a.at(300.0);
        a.begin(300.0, false);
        assert_eq!(a.at(300.0), mid, "反向瞬间从当前值继续");

        // 离场：详情 0.1s 先退，底栏 0.12s 延迟后才回来。
        let t50 = a.at(350.0);
        assert!(t50.details_alpha < mid.details_alpha);
        assert!(t50.foot_alpha <= 0.001, "底栏仍在延迟期");
        let t250 = a.at(550.0);
        assert!(t250.foot_alpha > 0.5, "底栏 0.12s 后淡回");
        assert!(t250.chart_alpha > 0.0);

        let end = a.at(300.0 + LEAVE_MS);
        assert!(close(end.details_alpha, 0.0));
        assert!(close(end.foot_alpha, 1.0));
        assert!(close(end.chart_alpha, 1.0));
        assert!(close(end.inner_dy, 0.0));
        assert!(close(end.glass, 0.0));
        assert!(a.done(300.0 + LEAVE_MS));
    }

    #[test]
    fn cell_delays_are_40ms_apart() {
        let mut a = HoverAnim::new();
        a.begin(0.0, true);
        // 第 i 格在 40*(i+1)ms 之前 alpha 必须为 0。
        for i in 0..CELLS {
            let before = a.at(40.0 * i as f64 + 39.0);
            assert_eq!(before.cells[i].alpha, 0.0, "cell {i} 不应提前进场");
            let after = a.at(40.0 * i as f64 + 41.0);
            assert!(after.cells[i].alpha > 0.0, "cell {i} 应开始进场");
        }
    }
}
