//! 数字弹簧：把前端 `Spring(k=175, c=21)` 原样移植为原生插值（P3-7）。
//!
//! 对照 `src/index.html`：
//! - `Spring.step(dt)`：`v += (-k*(x-t) - c*v)*dt; x += v*dt`，k=175、c=21
//! - `frame()`：dt 上限 0.05 s、30 fps 渲染上限、`animUntil = kick + 300ms`、
//!   settled = `|x-t|<1 && |v|<1`，落定即吸附并停表
//! - `onSample()`：`changed = |total - t| > max(1024, t*0.01)` 才起摆；
//!   胶囊态直接吸附、不跑动画
//! - `renderValues()`：`Math.max(0, x)`
//!
//! 这里只有时间→数值的纯逻辑，重绘与定时器在 main.rs。

/// 弹簧刚度（前端 `const k = 175`）。
pub const K: f64 = 175.0;
/// 阻尼（前端 `const c = 21`）。
pub const C: f64 = 21.0;
/// `frame()` 里 `Math.min((now - last) / 1000, 0.05)`。
pub const MAX_DT: f64 = 0.05;
/// 30 fps 渲染上限（`if (now - lastRender >= 30)`）。
pub const RENDER_MIN_MS: f64 = 30.0;
/// `kick()` 里的 `animUntil = last + 300`。
pub const ANIM_WINDOW_MS: f64 = 300.0;
/// 起摆阈值的绝对下限：1 KB/s。
pub const MIN_DELTA: f64 = 1024.0;
/// 起摆阈值的相对比例：1%。
pub const REL_DELTA: f64 = 0.01;
/// settled 阈值：位置差与速度都小于 1。
pub const SETTLED: f64 = 1.0;

/// 一条阻尼弹簧（位置 x / 速度 v / 目标 t）。
#[derive(Clone, Copy, Debug)]
pub struct Spring {
    pub x: f64,
    pub v: f64,
    pub t: f64,
}

impl Spring {
    pub fn new(v: f64) -> Self {
        Self { x: v, v: 0.0, t: v }
    }

    /// 与前端 `step(dt)` 逐字一致；dt 上限 0.05 s（前端也在外面钳过）。
    pub fn step(&mut self, dt: f64) {
        let dt = dt.clamp(0.0, MAX_DT);
        self.v += (-K * (self.x - self.t) - C * self.v) * dt;
        self.x += self.v * dt;
    }

    /// 吸附到目标并清零速度（`sDown.x = sDown.t`）。
    pub fn snap(&mut self) {
        self.x = self.t;
        self.v = 0.0;
    }

    pub fn settled(&self) -> bool {
        (self.x - self.t).abs() < SETTLED && self.v.abs() < SETTLED
    }
}

/// 一次 tick 的结果：这一帧要不要重绘、动画是否收尾。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tick {
    pub render: bool,
    pub finished: bool,
}

/// 下行/上行两条弹簧 + 30 fps 渲染闸 + 300 ms 动画窗口。
pub struct NumberAnim {
    down: Spring,
    up: Spring,
    running: bool,
    anim_until: f64,
    last_render: f64,
}

impl Default for NumberAnim {
    fn default() -> Self {
        Self::new()
    }
}

impl NumberAnim {
    pub fn new() -> Self {
        Self {
            down: Spring::new(0.0),
            up: Spring::new(0.0),
            running: false,
            anim_until: 0.0,
            last_render: f64::NEG_INFINITY,
        }
    }

    pub fn running(&self) -> bool {
        self.running
    }

    /// 起摆阈值：`|total - t| > max(1024, t*0.01)`。
    pub fn changed(&self, total: f64) -> bool {
        (total - self.down.t).abs() > MIN_DELTA.max(self.down.t * REL_DELTA)
    }

    /// 1 Hz 采样（对照 `onSample`）。返回 true 表示刚起摆、需要开 16 ms 定时器。
    ///
    /// `island` 为真时对齐前端胶囊态：直接吸附，不跑动画。
    pub fn on_sample(&mut self, down: f64, up: f64, island: bool, now_ms: f64) -> bool {
        let changed = self.changed(down);
        self.down.t = down;
        self.up.t = up;
        if island {
            self.snap();
            return false;
        }
        if !changed {
            self.snap();
            return false;
        }
        if self.running {
            return false;
        }
        // kick()：只有从静止起摆才重置窗口与渲染基准
        self.running = true;
        self.anim_until = now_ms + ANIM_WINDOW_MS;
        self.last_render = f64::NEG_INFINITY;
        true
    }

    /// 推进一帧（对照 `frame(now)`）。dt 由调用方按真实经过时间给出。
    pub fn tick(&mut self, now_ms: f64, dt: f64) -> Tick {
        if !self.running {
            return Tick {
                render: false,
                finished: true,
            };
        }
        self.down.step(dt);
        self.up.step(dt);
        let render = now_ms - self.last_render >= RENDER_MIN_MS;
        if render {
            self.last_render = now_ms;
        }
        let settled = self.down.settled() && self.up.settled();
        if settled || now_ms >= self.anim_until {
            self.snap();
            self.running = false;
            return Tick {
                render: true,
                finished: true,
            };
        }
        Tick {
            render,
            finished: false,
        }
    }

    /// 立即吸附（胶囊态、net-mode 内容进场、隐藏恢复首帧都用它）。
    pub fn snap(&mut self) {
        self.down.snap();
        self.up.snap();
        self.running = false;
    }

    /// 当前显示值：`Math.max(0, x)`。
    pub fn values(&self) -> (f64, f64) {
        (self.down.x.max(0.0), self.up.x.max(0.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1 MB/s，避免每次写 1048576。
    const MB: f64 = 1048576.0;

    #[test]
    fn spring_converges_to_target_without_overshoot_runaway() {
        let mut s = Spring::new(0.0);
        s.t = 5.0 * MB;
        for _ in 0..200 {
            s.step(0.016);
        }
        assert!(s.settled(), "弹簧应收敛：x={} v={}", s.x, s.v);
        assert!((s.x - 5.0 * MB).abs() < 1.0);
    }

    #[test]
    fn spring_keeps_velocity_when_target_changes() {
        // 中途改目标：速度不清零，位置连续（快速连点/切前台不跳变）
        let mut s = Spring::new(0.0);
        s.t = 5.0 * MB;
        for _ in 0..6 {
            s.step(0.016);
        }
        let v_before = s.v;
        assert!(v_before > 0.0, "应当已经起步：v={v_before}");
        s.t = 1.0 * MB;
        s.step(0.016);
        assert!(s.v != 0.0, "重定向不应清零速度");
        assert!(
            (s.v - v_before).abs() < v_before.abs(),
            "一帧之内速度不该突变：{v_before} -> {}",
            s.v
        );
    }

    #[test]
    fn small_change_under_one_percent_does_not_animate() {
        let mut n = NumberAnim::new();
        // 目标 1 MB/s，1% = 10485 B/s，阈值取 max(1024, 10485)
        // 首次采样从 0 起摆（对照前端 onSample：t 初值 0，changed 为真就 kick）
        assert!(n.on_sample(MB, 0.0, false, 0.0), "首次采样同样起摆");
        n.snap();
        assert!(!n.running());
        assert_eq!(n.values(), (MB, 0.0));

        let small = MB + 5000.0;
        assert!(!n.changed(small), "0.5% 变化在 1% 阈值内");
        assert!(!n.on_sample(small, 0.0, false, 1000.0));
        assert!(!n.running());
        assert_eq!(n.values().0, small, "小变化直接吸附到新值");

        let big = MB + 20_000.0;
        assert!(n.changed(big), "2% 变化超过 1% 阈值");
        assert!(n.on_sample(big, 0.0, false, 2000.0), "起摆并要求开定时器");
        assert!(n.running());
    }

    #[test]
    fn zero_traffic_never_starts_an_animation() {
        // 空闲桌面：每秒都是 0，不该有任何出帧（对齐前端 changed=false 分支）
        let mut n = NumberAnim::new();
        for i in 0..5 {
            assert!(!n.on_sample(0.0, 0.0, false, i as f64 * 1000.0));
            assert!(!n.running());
        }
        assert_eq!(n.values(), (0.0, 0.0));
    }

    #[test]
    fn absolute_threshold_dominates_near_zero() {
        let mut n = NumberAnim::new();
        n.on_sample(0.0, 0.0, false, 0.0);
        // 目标 0 时 1% 也是 0，靠 1024 B/s 的绝对下限兜住
        assert!(!n.changed(1000.0), "1 KB/s 以下不算变化");
        assert!(n.changed(1025.0), "超过 1 KB/s 才算变化");
    }

    #[test]
    fn render_is_capped_at_thirty_fps() {
        let mut n = NumberAnim::new();
        assert!(n.on_sample(5.0 * MB, 0.0, false, 0.0));
        // 16 ms 一帧（≈60 Hz 定时器）时，只有每两帧渲染一次
        let mut renders = 0;
        for i in 0..10 {
            let now = i as f64 * 16.0;
            if n.tick(now, 0.016).render {
                renders += 1;
            }
        }
        assert!(
            (4..=6).contains(&renders),
            "10 帧 16 ms 内应只渲染 4–6 次（30 fps 上限），实际 {renders}"
        );
    }

    #[test]
    fn animation_stops_after_three_hundred_ms_window() {
        let mut n = NumberAnim::new();
        assert!(n.on_sample(5.0 * MB, MB, false, 0.0));
        let mut now = 0.0;
        let mut finished_at = None;
        while now < 400.0 {
            let t = n.tick(now, 0.016);
            if t.finished {
                finished_at = Some(now);
                break;
            }
            now += 16.0;
        }
        let at = finished_at.expect("300 ms 窗口到点必须收尾");
        assert!(at >= 288.0 && at <= 304.0, "应在 300 ms 附近收尾：{at}");
        assert!(!n.running());
        let (d, u) = n.values();
        assert_eq!((d, u), (5.0 * MB, MB), "收尾时吸附到目标值");
    }

    #[test]
    fn island_mode_snaps_without_animation() {
        let mut n = NumberAnim::new();
        n.on_sample(MB, 0.0, false, 0.0);
        assert!(!n.on_sample(9.0 * MB, MB, true, 1000.0), "胶囊态不起摆");
        assert!(!n.running());
        assert_eq!(n.values(), (9.0 * MB, MB), "胶囊态直接改文字");
    }

    #[test]
    fn values_never_go_negative() {
        let mut n = NumberAnim::new();
        n.on_sample(2.0 * MB, 0.0, false, 0.0);
        // 目标回到 0：欠阻尼会冲过零点，显示值必须钳在 0
        n.on_sample(0.0, 0.0, false, 1000.0);
        assert!(n.running());
        let mut saw_negative_spring = false;
        let mut now = 1000.0;
        for _ in 0..40 {
            n.tick(now, 0.016);
            let (d, _) = n.values();
            assert!(d >= 0.0, "显示值不得为负：{d}");
            if d == 0.0 {
                saw_negative_spring = true;
            }
            now += 16.0;
        }
        assert!(saw_negative_spring, "回落过程应当经过 0 并钳住");
    }

    #[test]
    fn retarget_mid_flight_keeps_rendering_until_window_ends() {
        let mut n = NumberAnim::new();
        n.on_sample(4.0 * MB, 0.0, false, 0.0);
        for i in 0..5 {
            n.tick(i as f64 * 16.0, 0.016);
        }
        // 动画途中又来一帧更大的采样：继续跑，但窗口不续期（前端同语义）
        assert!(!n.on_sample(8.0 * MB, 0.0, false, 80.0), "已在跑就不再 kick");
        assert!(n.running());
        let t = n.tick(320.0, 0.016);
        assert!(t.finished, "窗口到点仍要收尾");
        assert_eq!(n.values().0, 8.0 * MB, "收尾吸附到最新目标");
    }
}
