//! 弹簧驱动的窗口几何动画（灵动岛 <-> 卡片）。
//!
//! 形变的对象就是窗口矩形本身：过渡期间页面内容全部淡出，窗口里只剩圆角
//! 背景，所以这里用独立线程直接 SetWindowPos，不去和 WebView2 自己的动画
//! 抢主线程。窗口宽度/中心/高度各自一条阻尼弹簧，目标可随时打断重定向，
//! 位置与速度连续，因此快速点击/切前台不会看到跳变。

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowRect, SetWindowPos, SWP_NOACTIVATE, SWP_NOZORDER,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }
    fn cx(self) -> f64 {
        self.x as f64 + self.w as f64 / 2.0
    }
}

/// response = 无阻尼周期（秒级），damping = 阻尼比。
/// damping < 1 才有回弹；越大越"稳重"。
/// delay_ms = 起步前的停顿：制造"先慢后快"的节奏（内容先退场，形状再起步）。
#[derive(Clone, Copy, Debug)]
pub struct Motion {
    pub response: f64,
    pub damping: f64,
    pub delay_ms: u64,
}

impl Motion {
    pub const fn new(response: f64, damping: f64) -> Self {
        Self {
            response,
            damping,
            delay_ms: 0,
        }
    }
    pub const fn delayed(response: f64, damping: f64, delay_ms: u64) -> Self {
        Self {
            response,
            damping,
            delay_ms,
        }
    }
}

struct Command {
    rect: Rect,
    motion: Motion,
    snap: bool,
}

static TX: OnceLock<Sender<Command>> = OnceLock::new();

pub fn start(raw: isize, initial: Rect) {
    let (tx, rx) = mpsc::channel();
    let _ = TX.set(tx);
    let _ = std::thread::Builder::new()
        .name("netglass-geometry".into())
        .spawn(move || run(raw, initial, rx));
}

pub fn animate_to(rect: Rect, motion: Motion) {
    if let Some(tx) = TX.get() {
        let _ = tx.send(Command { rect, motion, snap: false });
    }
}

/// 不播动画，直接把窗口摆到目标（显示隐藏窗口、启动时用）。
pub fn snap_to(rect: Rect) {
    if let Some(tx) = TX.get() {
        let _ = tx.send(Command { rect, motion: Motion::new(0.3, 1.0), snap: true });
    }
}

#[derive(Clone, Copy)]
struct Spring {
    pos: f64,
    vel: f64,
    target: f64,
}

impl Spring {
    fn new(v: f64) -> Self {
        Self { pos: v, vel: 0.0, target: v }
    }
    fn set(&mut self, v: f64) {
        self.pos = v;
        self.vel = 0.0;
        self.target = v;
    }
    fn retarget(&mut self, v: f64) {
        self.target = v;
    }
    fn step(&mut self, dt: f64, omega: f64, zeta: f64) {
        let acc = -omega * omega * (self.pos - self.target) - 2.0 * zeta * omega * self.vel;
        self.vel += acc * dt;
        self.pos += self.vel * dt;
    }
    fn settled(&self) -> bool {
        (self.pos - self.target).abs() < 0.6 && self.vel.abs() < 12.0
    }
}

struct Geometry {
    cx: Spring,
    w: Spring,
    y: Spring,
    h: Spring,
}

impl Geometry {
    fn from_rect(r: Rect) -> Self {
        Self {
            cx: Spring::new(r.cx()),
            w: Spring::new(r.w as f64),
            y: Spring::new(r.y as f64),
            h: Spring::new(r.h as f64),
        }
    }
    fn set(&mut self, r: Rect) {
        self.cx.set(r.cx());
        self.w.set(r.w as f64);
        self.y.set(r.y as f64);
        self.h.set(r.h as f64);
    }
    fn retarget(&mut self, r: Rect) {
        self.cx.retarget(r.cx());
        self.w.retarget(r.w as f64);
        self.y.retarget(r.y as f64);
        self.h.retarget(r.h as f64);
    }
    fn step(&mut self, dt: f64, m: Motion) {
        let omega = std::f64::consts::TAU / m.response.max(0.05);
        let zeta = m.damping.clamp(0.2, 1.2);
        // 上边缘比形状略慢半拍：展开时先向下长、再整体落位，
        // 收起时先压高度、最后把顶边收回，避免"整块矩形平移"的呆板感。
        let (oy, zy) = (
            std::f64::consts::TAU / (m.response * 1.18).max(0.05),
            (m.damping + 0.04).clamp(0.2, 1.2),
        );
        self.cx.step(dt, omega, zeta);
        self.w.step(dt, omega, zeta);
        self.y.step(dt, oy, zy);
        self.h.step(dt, omega, zeta);
    }
    fn settled(&self) -> bool {
        self.cx.settled() && self.w.settled() && self.y.settled() && self.h.settled()
    }
    fn rect(&self) -> Rect {
        let w = self.w.pos.round().max(1.0) as i32;
        let h = self.h.pos.round().max(1.0) as i32;
        let x = (self.cx.pos - self.w.pos / 2.0).round() as i32;
        Rect::new(x, self.y.pos.round() as i32, w, h)
    }
}

fn hwnd_of(raw: isize) -> HWND {
    HWND(raw as *mut std::ffi::c_void)
}

fn window_rect(raw: isize) -> Option<Rect> {
    let mut r = RECT::default();
    unsafe { GetWindowRect(hwnd_of(raw), &mut r).ok()? };
    Some(Rect::new(r.left, r.top, r.right - r.left, r.bottom - r.top))
}

fn apply(raw: isize, r: Rect, last: &mut Rect) {
    if *last == r {
        return;
    }
    unsafe {
        let _ = SetWindowPos(
            hwnd_of(raw),
            None,
            r.x,
            r.y,
            r.w,
            r.h,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
    *last = r;
}

fn run(raw: isize, initial: Rect, rx: Receiver<Command>) {
    let mut geom = Geometry::from_rect(initial);
    let mut motion = Motion::new(0.36, 0.86);
    let mut target = initial;
    let mut last = window_rect(raw).unwrap_or(initial);
    let mut idle = true;
    let mut last_tick = Instant::now();
    let mut delay_until: Option<Instant> = None;

    loop {
        let cmd = if idle {
            // 空闲时零开销阻塞，不再周期性唤醒
            match rx.recv() {
                Ok(c) => Some(c),
                Err(_) => return,
            }
        } else {
            match rx.recv_timeout(Duration::from_millis(8)) {
                Ok(c) => Some(c),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        };

        if let Some(c) = cmd {
            let was_idle = idle;
            if was_idle {
                // 用户可能刚拖动过窗口：以真实矩形重新起算，避免从旧位置弹过去
                if let Some(actual) = window_rect(raw) {
                    geom = Geometry::from_rect(actual);
                    last = actual;
                }
            }
            motion = c.motion;
            target = c.rect;
            if c.snap {
                geom.set(c.rect);
                apply(raw, geom.rect(), &mut last);
                idle = true;
                delay_until = None;
                continue;
            }
            // 打断时保留当前速度，实现"从当前帧自然接上"
            geom.retarget(c.rect);
            idle = false;
            // 只有从静止开始的完整动作才吃 delay；打断/接力立即起步
            let delay = if was_idle { c.motion.delay_ms } else { 0 };
            delay_until = if delay > 0 {
                Some(Instant::now() + Duration::from_millis(delay))
            } else {
                None
            };
            last_tick = delay_until.unwrap_or_else(Instant::now);
        }

        if idle {
            continue;
        }

        let now = Instant::now();
        if let Some(until) = delay_until {
            if now < until {
                last_tick = now;
                continue;
            }
            delay_until = None;
            last_tick = until;
        }
        // SetWindowPos 可能被 WebView2 的 resize 阻塞到 20–30ms；
        // 物理用固定 4ms 子步，保证大 dt 下也不会把回弹"积"成过阻尼。
        let dt = (now - last_tick).as_secs_f64().clamp(0.001, 0.05);
        last_tick = now;
        let mut remain = dt;
        while remain > 0.0 {
            let h = remain.min(0.004);
            geom.step(h, motion);
            remain -= h;
        }
        if geom.settled() {
            geom.set(target);
            apply(raw, geom.rect(), &mut last);
            idle = true;
        } else {
            apply(raw, geom.rect(), &mut last);
        }
    }
}
