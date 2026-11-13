//! 代理路径探测与加速器状态。
//!
//! 代理没有自己的网卡，也不一定暴露 API，所以唯一可靠的办法是
//! "走代理端口主动测一次"：CONNECT 隧道建立的时间就是这条路的质量。
//! 代理没开时不做任何测量，界面直接显示"未启用"。
//!
//! 加速器分两级状态："在运行"（进程存在）≠"在加速"。
//! Watt 的 Hosts 加速模式不监听任何端口，只往 hosts 写加速块；
//! System/PAC 模式才会在本机挂监听端口。

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, ERROR_INSUFFICIENT_BUFFER, NO_ERROR};
use windows::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
};
use windows::Win32::Networking::WinSock::AF_INET;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};

/// 常见本地代理端口，按顺序探测
const PROXY_PORTS: &[u16] = &[7897, 7890, 1080, 10809, 2080];

#[derive(Clone, Debug)]
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

#[derive(Clone, Debug)]
pub struct ProxyState {
    pub port: Option<u16>,
    /// 检测到在运行的加速器（进程存在）
    pub accelerators: Vec<String>,
    /// 检测到正在加速的加速器
    pub accelerating: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ProbePayload {
    pub direct: ProbeResult,
    pub proxy: ProxyState,
    pub via: Option<ProbeResult>,
    pub battery: bool,
}

/// 电池供电时不发外网探测，避免把网卡从省电态拉起。
/// 电源状态未知时按 AC 处理，不牺牲功能。
pub fn on_battery() -> bool {
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    let mut s = SYSTEM_POWER_STATUS::default();
    unsafe { GetSystemPowerStatus(&mut s).is_ok() && s.ACLineStatus == 0 }
}

use std::sync::Mutex;

/// 目标地址解析一次就缓存：DNS 是探针里最贵的一步，
/// 每次重新解析会在主进程里造成几十毫秒的 CPU 尖峰。
/// TTL 10 分钟：CDN 换 IP 后不至于陈旧到重启。
static DNS_CACHE: Mutex<Vec<(String, SocketAddr, Instant)>> = Mutex::new(Vec::new());
const DNS_TTL: Duration = Duration::from_secs(600);

fn resolve(host: &str, port: u16) -> Option<SocketAddr> {
    let key = format!("{host}:{port}");
    if let Ok(cache) = DNS_CACHE.lock() {
        if let Some((_, addr, _)) = cache
            .iter()
            .find(|(k, _, t)| k == &key && t.elapsed() < DNS_TTL)
        {
            return Some(*addr);
        }
    }
    let addr = (host, port).to_socket_addrs().ok()?.next()?;
    if let Ok(mut cache) = DNS_CACHE.lock() {
        cache.retain(|(k, _, _)| k != &key);
        cache.push((key, addr, Instant::now()));
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

fn running_processes() -> Vec<(String, u32)> {
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
                out.push((
                    String::from_utf16_lossy(&e.szExeFile[..end]).to_lowercase(),
                    e.th32ProcessID,
                ));
                if Process32NextW(snap, &mut e).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    out
}

/// 已知加速器：进程名（小写）→ 显示名。
/// SakuraCat 的代理端口由其 Clash 内核 core.exe 监听（resources/binary/win32/x64），
/// 不含内核进程就永远看不到"SakuraCat 加速中"。
const KNOWN: &[(&str, &str)] = &[
    ("sakuracat.exe", "SakuraCat"),
    ("core.exe", "SakuraCat"),
    ("steam++.exe", "Watt"),
    ("steam++.accelerator.exe", "Watt"),
    ("uu.exe", "UU"),
    ("uuaccelerator.exe", "UU"),
];

/// Watt 的 Hosts 加速模式不改端口、不挂代理，只在 hosts 里写
/// "# Steam++ Start ... # Steam++ End" 块；块内有未注释的条目才算在加速。
/// 停止加速后 Watt 会撤块；块内被注释掉的历史条目（Watt 卸载残留）不算。
fn hosts_block_active(text: &str) -> bool {
    let mut in_block = false;
    for line in text.lines() {
        let t = line.trim();
        if t.contains("Steam++ Start") || t.contains("Steam++ Hosts Start") {
            in_block = true;
            continue;
        }
        if t.contains("Steam++ End") || t.contains("Steam++ Hosts End") {
            in_block = false;
            continue;
        }
        if in_block && !t.is_empty() && !t.starts_with('#') {
            return true;
        }
    }
    false
}

fn watt_hosts_accelerating() -> bool {
    let path = "C:\\Windows\\System32\\drivers\\etc\\hosts";
    // mtime 没变就不重读文件：Watt 写/撤块才会改 mtime
    let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    if let Ok(mut g) = HOSTS_STATE.lock() {
        if g.0 == mtime {
            return g.1;
        }
        let active = std::fs::read_to_string(path)
            .map(|t| hosts_block_active(&t))
            .unwrap_or(false);
        *g = (mtime, active);
        return active;
    }
    false
}

static HOSTS_STATE: Mutex<(Option<std::time::SystemTime>, bool)> = Mutex::new((None, false));

/// 这些 PID 名下是否有 LISTEN 状态的 TCP 端口（System/PAC 加速模式的信号）。
/// 只取 PID 列表，归属用定向 exe 查询，不做全量进程快照。
fn listener_pids() -> Vec<u32> {
    unsafe {
        let mut size: u32 = 0;
        let ret = GetExtendedTcpTable(
            None,
            &mut size,
            false,
            AF_INET.0 as u32,
            TCP_TABLE_OWNER_PID_LISTENER,
            0,
        );
        if ret != ERROR_INSUFFICIENT_BUFFER.0 {
            return Vec::new();
        }
        // u32 缓冲保证 MIB_TCPTABLE_OWNER_PID 的 4 字节对齐（u8 缓冲严格意义是 UB）
        let mut buf = vec![0u32; (size as usize + 3) / 4];
        let ret = GetExtendedTcpTable(
            Some(buf.as_mut_ptr() as *mut core::ffi::c_void),
            &mut size,
            false,
            AF_INET.0 as u32,
            TCP_TABLE_OWNER_PID_LISTENER,
            0,
        );
        if ret != NO_ERROR.0 {
            return Vec::new();
        }
        let table = buf.as_ptr() as *const MIB_TCPTABLE_OWNER_PID;
        let rows =
            std::slice::from_raw_parts((*table).table.as_ptr(), (*table).dwNumEntries as usize);
        let mut pids: Vec<u32> = rows.iter().map(|r| r.dwOwningPid).collect();
        pids.sort_unstable();
        pids.dedup();
        pids
    }
}

/// 定向查单个 PID 的 exe 名（小写）。比全量快照便宜两个数量级。
fn exe_name(pid: u32) -> Option<String> {
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 260];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            h,
            windows::Win32::System::Threading::PROCESS_NAME_FORMAT(0),
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(h);
        if !ok {
            return None;
        }
        let s = String::from_utf16_lossy(&buf[..len as usize]);
        Some(s.rsplit('\\').next().unwrap_or(&s).to_lowercase())
    }
}

/// 「在运行」展示用全量快照 10s 兜底；加速判定走 2s 廉价路径，不受它滞后。
static PROC_CACHE: Mutex<(Option<Instant>, Vec<(String, u32)>)> = Mutex::new((None, Vec::new()));

fn processes_cached() -> Vec<(String, u32)> {
    if let Ok(mut g) = PROC_CACHE.lock() {
        if g.0.map(|t| t.elapsed() < Duration::from_secs(10)).unwrap_or(false) {
            return g.1.clone();
        }
        let procs = running_processes();
        *g = (Some(Instant::now()), procs.clone());
        return procs;
    }
    running_processes()
}

/// (运行中, 正在加速)。加速判定 2s 廉价路径：监听表 PID 定向查 exe 名 +
/// hosts mtime 门控，不依赖进程缓存（新启动加速器 ≤2-3s 反映）；
/// 「在运行」展示用 10s 兜底快照，加速中的标签立即并入。
fn accelerator_state() -> (Vec<String>, Vec<String>) {
    let listener_names: Vec<String> = listener_pids().iter().filter_map(|p| exe_name(*p)).collect();
    let mut accelerating: Vec<String> = Vec::new();
    for (exe, label) in KNOWN {
        if listener_names.iter().any(|n| n == exe) && !accelerating.iter().any(|a| a == label) {
            accelerating.push((*label).to_string());
        }
    }
    if watt_hosts_accelerating() && !accelerating.iter().any(|a| a == "Watt") {
        accelerating.push("Watt".to_string());
    }

    let procs = processes_cached();
    let mut running: Vec<String> = Vec::new();
    for (exe, label) in KNOWN {
        if (procs.iter().any(|(p, _)| p == exe) || accelerating.iter().any(|a| a == label))
            && !running.iter().any(|a| a == label)
        {
            running.push((*label).to_string());
        }
    }
    (running, accelerating)
}

pub fn proxy_state() -> ProxyState {
    let port = PROXY_PORTS.iter().copied().find(|p| port_listening(*p));
    let (accelerators, accelerating) = accelerator_state();
    ProxyState {
        port,
        accelerators,
        accelerating,
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
        battery: on_battery(),
    }
}

/// 从未成功外网探测时的基础载荷（电池冷启动 / 启动初期）：
/// 直连时延显示"暂停"，本地代理/加速器状态现算。
pub fn offline_payload() -> ProbePayload {
    ProbePayload {
        direct: ProbeResult::fail("www.baidu.com:443", "paused"),
        proxy: proxy_state(),
        via: None,
        battery: on_battery(),
    }
}

#[cfg(test)]
mod tests {
    use super::hosts_block_active;

    #[test]
    fn active_block_is_accelerating() {
        let text = "# junk\n# Steam++ Start\n127.0.0.1 github.com\n# Steam++ End\n";
        assert!(hosts_block_active(text));
    }

    #[test]
    fn commented_entries_do_not_count() {
        // Watt 停止/卸载后残留的注释条目不算加速
        let text = "# Steam++ Start\n#127.0.0.1 github.com\n# Steam++ End\n";
        assert!(!hosts_block_active(text));
    }

    #[test]
    fn no_block_is_not_accelerating() {
        assert!(!hosts_block_active("127.0.0.1 localhost\n"));
    }

    #[test]
    fn empty_block_is_not_accelerating() {
        assert!(!hosts_block_active("# Steam++ Start\n# Steam++ End\n"));
    }

    #[test]
    fn entry_after_end_marker_does_not_count() {
        let text = "# Steam++ Start\n# Steam++ End\n127.0.0.1 github.com\n";
        assert!(!hosts_block_active(text));
    }

    #[test]
    fn hosts_variant_marker_matches() {
        let text = "# Steam++ Hosts Start\n127.0.0.1 github.com\n# Steam++ Hosts End\n";
        assert!(hosts_block_active(text));
    }
}
