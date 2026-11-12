//! 代理路径探测与加速器状态。
//!
//! 代理没有自己的网卡，也不一定暴露 API，所以唯一可靠的办法是
//! "走代理端口主动测一次"：CONNECT 隧道建立的时间就是这条路的质量。
//! 代理没开时不做任何测量，界面直接显示"未启用"。

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use serde::Serialize;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};

/// 常见本地代理端口，按顺序探测
const PROXY_PORTS: &[u16] = &[7897, 7890, 1080, 10809, 2080];

#[derive(Clone, Debug, Serialize)]
pub struct ProbeResult {
    pub ok: bool,
    pub ms: Option<f64>,
    pub target: String,
    pub error: Option<String>,
}

impl ProbeResult {
    fn fail(target: &str, why: &str) -> Self {
        Self {
            ok: false,
            ms: None,
            target: target.to_string(),
            error: Some(why.to_string()),
        }
    }
    fn ok(target: &str, ms: f64) -> Self {
        Self {
            ok: true,
            ms: Some((ms * 10.0).round() / 10.0),
            target: target.to_string(),
            error: None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ProxyState {
    pub port: Option<u16>,
    pub accelerators: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProbePayload {
    pub direct: ProbeResult,
    pub proxy: ProxyState,
    pub via: Option<ProbeResult>,
}

use std::sync::Mutex;

/// 目标地址解析一次就缓存：DNS 是探针里最贵的一步，
/// 每次重新解析会在主进程里造成几十毫秒的 CPU 尖峰。
static DNS_CACHE: Mutex<Vec<(String, SocketAddr)>> = Mutex::new(Vec::new());

fn resolve(host: &str, port: u16) -> Option<SocketAddr> {
    let key = format!("{host}:{port}");
    if let Ok(cache) = DNS_CACHE.lock() {
        if let Some((_, addr)) = cache.iter().find(|(k, _)| k == &key) {
            return Some(*addr);
        }
    }
    let addr = (host, port).to_socket_addrs().ok()?.next()?;
    if let Ok(mut cache) = DNS_CACHE.lock() {
        cache.push((key, addr));
    }
    Some(addr)
}

/// 直连 TCP 握手时延
pub fn tcp_probe(host: &str, port: u16, timeout_ms: u64) -> ProbeResult {
    let target = format!("{host}:{port}");
    let addr = match resolve(host, port) {
        Some(a) => a,
        None => return ProbeResult::fail(&target, "dns"),
    };
    let start = Instant::now();
    match TcpStream::connect_timeout(&addr, Duration::from_millis(timeout_ms)) {
        Ok(_) => ProbeResult::ok(&target, start.elapsed().as_secs_f64() * 1000.0),
        Err(_) => ProbeResult::fail(&target, "tcp"),
    }
}

pub fn port_listening(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(80)).is_ok()
}

/// 经本地代理端口建立 CONNECT 隧道，耗时即这条代理路径的时延
pub fn http_connect_probe(
    proxy_port: u16,
    host: &str,
    target_port: u16,
    timeout_ms: u64,
) -> ProbeResult {
    let target = format!("{host}:{target_port}");
    let addr = SocketAddr::from(([127, 0, 0, 1], proxy_port));
    let start = Instant::now();
    let mut s = match TcpStream::connect_timeout(&addr, Duration::from_millis(800)) {
        Ok(s) => s,
        Err(_) => return ProbeResult::fail(&target, "proxy-closed"),
    };
    let _ = s.set_read_timeout(Some(Duration::from_millis(timeout_ms)));
    let req = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n");
    if s.write_all(req.as_bytes()).is_err() {
        return ProbeResult::fail(&target, "write");
    }
    let mut buf = [0u8; 96];
    match s.read(&mut buf) {
        Ok(n) if n > 0 => {
            let head = String::from_utf8_lossy(&buf[..n]);
            if head.starts_with("HTTP/1.1 200") || head.starts_with("HTTP/1.0 200") {
                ProbeResult::ok(&target, start.elapsed().as_secs_f64() * 1000.0)
            } else {
                ProbeResult::fail(&target, head.lines().next().unwrap_or("bad-response"))
            }
        }
        _ => ProbeResult::fail(&target, "timeout"),
    }
}

fn running_processes() -> Vec<String> {
    let mut out = Vec::new();
    unsafe {
        let snap = match CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            Ok(h) => h,
            Err(_) => return out,
        };
        let mut e = PROCESSENTRY32W::default();
        e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snap, &mut e).is_ok() {
            loop {
                let end = e
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(e.szExeFile.len());
                out.push(String::from_utf16_lossy(&e.szExeFile[..end]).to_lowercase());
                if Process32NextW(snap, &mut e).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    out
}

/// 加速器进程枚举要走一次全量快照（~15 ms），60 秒一次足够。
static ACC_CACHE: Mutex<Option<(Instant, Vec<String>)>> = Mutex::new(None);

fn accelerators_cached() -> Vec<String> {
    if let Ok(guard) = ACC_CACHE.lock() {
        if let Some((t, v)) = guard.as_ref() {
            if t.elapsed().as_secs() < 60 {
                return v.clone();
            }
        }
    }

    let procs = running_processes();
    const KNOWN: &[(&str, &str)] = &[
        ("sakuracat.exe", "SakuraCat"),
        ("steam++.exe", "Watt"),
        ("steam++.accelerator.exe", "Watt"),
        ("uu.exe", "UU"),
        ("uuaccelerator.exe", "UU"),
    ];
    let mut out: Vec<String> = Vec::new();
    for (exe, label) in KNOWN {
        if procs.iter().any(|p| p == exe) && !out.iter().any(|a| a == label) {
            out.push((*label).to_string());
        }
    }

    if let Ok(mut guard) = ACC_CACHE.lock() {
        *guard = Some((Instant::now(), out.clone()));
    }
    out
}

pub fn proxy_state() -> ProxyState {
    let port = PROXY_PORTS.iter().copied().find(|p| port_listening(*p));
    ProxyState {
        port,
        accelerators: accelerators_cached(),
    }
}

pub fn run_probe() -> ProbePayload {
    // 直连时延用一个国内可达目标，避免"被墙"把延迟变成红线；
    // 代理路径则测真实用途（github），这才是有意义的对比。
    let direct = tcp_probe("www.baidu.com", 443, 3000);
    let proxy = proxy_state();
    let via = proxy
        .port
        .map(|p| http_connect_probe(p, "github.com", 443, 5000));
    ProbePayload {
        direct,
        proxy,
        via,
    }
}
