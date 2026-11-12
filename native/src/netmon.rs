//! 接口枚举、默认路由识别、字节计数差分。
//!
//! 事实依据（本机实测，见 docs/adr/0002）：
//! - 拨号连接不是 NDIS 适配器，Get-NetAdapterStatistics 里没有它；
//!   必须用 GetIfTable2 / GetIfEntry2 才能拿到 64 位计数。
//! - 同一时刻只有一个接口承载默认路由；"总计"只统计它。

use std::collections::HashMap;
use std::time::Instant;

use windows::Win32::NetworkManagement::IpHelper::{
    FreeMibTable, GetIfEntry2, GetIfTable2, GetIpForwardTable2, GetIpInterfaceEntry,
    MIB_IF_ROW2, MIB_IF_TABLE2, MIB_IPFORWARD_TABLE2, MIB_IPINTERFACE_ROW,
};
use windows::Win32::Networking::WinSock::AF_INET;

const IF_TYPE_ETHERNET_CSMACD: u32 = 6;
const IF_TYPE_PPP: u32 = 23;
const IF_TYPE_IEEE80211: u32 = 71;

const IF_OPER_STATUS_UP: i32 = 1;
const NET_IF_CONNECTION_DIALUP: i32 = 1;
const MEDIA_CONNECTED: i32 = 1;

const VIRTUAL_HINTS: &[&str] = &[
    "virtualbox",
    "vmware",
    "hyper-v",
    "vethernet",
    "wsl",
    "loopback",
    "bluetooth",
    "蓝牙",
    "npcap",
    "tap-windows",
    "tap-win32",
    "teredo",
    "isatap",
    "ip-https",
    "6to4",
    "wan miniport",
    "内核调试",
];

#[derive(Clone, Debug)]
pub struct IfaceSample {
    pub id: u32,
    pub alias: String,
    pub kind: &'static str,
    pub state: &'static str,
    pub down: f64,
    pub up: f64,
    pub link_mbps: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct Sample {
    pub ts: u64,
    pub primary_id: Option<u32>,
    pub total_down: f64,
    pub total_up: f64,
    pub ifaces: Vec<IfaceSample>,
}

struct Row {
    index: u32,
    alias: String,
    kind: &'static str,
    up: bool,
    in_octets: u64,
    out_octets: u64,
    link_bps: u64,
}

fn wide_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end]).trim().to_string()
}

fn classify(if_type: u32, conn_type: i32, alias: &str) -> Option<&'static str> {
    let lower = alias.to_lowercase();
    let is_virtual = VIRTUAL_HINTS.iter().any(|h| lower.contains(h));

    // 本机实测：拨号接口的 ConnectionType 报 3（MAXIMUM），不是文档里的 DialUp(1)。
    // 因此只按 IfType=PPP 判定，ConnectionType 不可靠。
    if if_type == IF_TYPE_PPP {
        return Some("dialup");
    }
    let _ = conn_type;
    if is_virtual && if_type != IF_TYPE_PPP {
        if lower.contains("wintun") || lower.contains("mihomo") || lower.contains("sakura") {
            return Some("tun");
        }
        return None;
    }
    match if_type {
        IF_TYPE_IEEE80211 => Some("wifi"),
        IF_TYPE_ETHERNET_CSMACD => Some("ethernet"),
        _ => None,
    }
}

fn enumerate() -> Vec<Row> {
    let mut out = Vec::new();
    unsafe {
        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        if GetIfTable2(&mut table).0 != 0 || table.is_null() {
            return out;
        }
        let t = &*table;
        let rows = std::slice::from_raw_parts(t.Table.as_ptr(), t.NumEntries as usize);
        for row in rows {
            // bit1 = FilterInterface：WFP / QoS / Native WiFi Filter 等轻量过滤层
            // 会被 GetIfTable2 当成独立接口列出来，必须剔除，否则会污染候选列表。
            if row.InterfaceAndOperStatusFlags._bitfield & 0x02 != 0 {
                continue;
            }
            let alias = wide_to_string(&row.Alias);
            // Wi-Fi Direct 虚拟适配器（"本地连接* N"）不是真实出口
            if alias.contains("本地连接*") {
                continue;
            }
            let kind = match classify(row.Type, row.ConnectionType.0, &alias) {
                Some(k) => k,
                None => continue,
            };
            out.push(Row {
                index: row.InterfaceIndex,
                alias,
                kind,
                up: row.OperStatus.0 == IF_OPER_STATUS_UP
                    && row.MediaConnectState.0 == MEDIA_CONNECTED,
                in_octets: row.InOctets,
                out_octets: row.OutOctets,
                link_bps: row.ReceiveLinkSpeed,
            });
        }
        FreeMibTable(table as *const core::ffi::c_void);
    }
    out
}

/// 接口自身的度量值。路由表里的 Metric 不含它，必须相加才是系统真正比较的值。
fn iface_metric(idx: u32) -> u32 {
    unsafe {
        let mut row = MIB_IPINTERFACE_ROW::default();
        row.Family = AF_INET;
        row.InterfaceIndex = idx;
        if GetIpInterfaceEntry(&mut row).0 == 0 {
            row.Metric
        } else {
            0
        }
    }
}

/// 默认路由指向的接口索引：只在前缀长度 0、且接口确实已连通的候选里选有效度量最小的。
pub fn primary_index(connected: &[u32]) -> Option<u32> {
    unsafe {
        let mut table: *mut MIB_IPFORWARD_TABLE2 = std::ptr::null_mut();
        if GetIpForwardTable2(AF_INET, &mut table).0 != 0 || table.is_null() {
            return None;
        }
        let t = &*table;
        let rows = std::slice::from_raw_parts(t.Table.as_ptr(), t.NumEntries as usize);
        let mut best: Option<(u32, u32)> = None;
        for row in rows {
            if row.DestinationPrefix.PrefixLength != 0 || row.Loopback {
                continue;
            }
            let idx = row.InterfaceIndex;
            if idx == 0 || !connected.contains(&idx) {
                continue;
            }
            let m = row.Metric.saturating_add(iface_metric(idx));
            best = match best {
                Some((bm, _)) if bm <= m => best,
                _ => Some((m, idx)),
            };
        }
        FreeMibTable(table as *const core::ffi::c_void);
        best.map(|(_, idx)| idx)
    }
}

/// 调试用：把所有接口行原样写进临时目录，便于核对状态字段。
pub fn dump() {
    let mut out = String::new();
    unsafe {
        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        if GetIfTable2(&mut table).0 != 0 || table.is_null() {
            return;
        }
        let t = &*table;
        let rows = std::slice::from_raw_parts(t.Table.as_ptr(), t.NumEntries as usize);
        for row in rows {
            out.push_str(&format!(
                "idx={:<3} type={:<3} conn={:<2} oper={:<2} media={:<2} alias={}\n",
                row.InterfaceIndex,
                row.Type,
                row.ConnectionType.0,
                row.OperStatus.0,
                row.MediaConnectState.0,
                wide_to_string(&row.Alias),
            ));
        }
        FreeMibTable(table as *const core::ffi::c_void);

        out.push_str("\n--- default routes ---\n");
        let mut rtable: *mut MIB_IPFORWARD_TABLE2 = std::ptr::null_mut();
        if GetIpForwardTable2(AF_INET, &mut rtable).0 == 0 && !rtable.is_null() {
            let rt = &*rtable;
            let rrows = std::slice::from_raw_parts(rt.Table.as_ptr(), rt.NumEntries as usize);
            for r in rrows {
                if r.DestinationPrefix.PrefixLength == 0 {
                    out.push_str(&format!(
                        "ifindex={:<3} metric={:<5} loopback={}\n",
                        r.InterfaceIndex, r.Metric, r.Loopback
                    ));
                }
            }
            FreeMibTable(rtable as *const core::ffi::c_void);
        }
    }
    let _ = std::fs::write(std::env::temp_dir().join("netglass-dump.txt"), out);
}

/// 只读单个接口的计数器。比 GetIfTable2 重枚举 47 条便宜得多（~80 µs）。
fn read_row(index: u32) -> Option<(u64, u64, bool, u64)> {
    unsafe {
        let mut row = MIB_IF_ROW2::default();
        row.InterfaceIndex = index;
        if GetIfEntry2(&mut row).0 != 0 {
            return None;
        }
        let is_up = row.OperStatus.0 == IF_OPER_STATUS_UP
            && row.MediaConnectState.0 == MEDIA_CONNECTED;
        Some((
            row.InOctets,
            row.OutOctets,
            is_up,
            row.ReceiveLinkSpeed,
        ))
    }
}

struct Cached {
    index: u32,
    alias: String,
    kind: &'static str,
}

pub struct Sampler {
    prev: HashMap<u32, (u64, u64, Instant)>,
    cached: Vec<Cached>,
    last_discover: Option<Instant>,
    primary: Option<u32>,
    last_primary: Option<Instant>,
}

impl Sampler {
    pub fn new() -> Self {
        Self {
            prev: HashMap::new(),
            cached: Vec::new(),
            last_discover: None,
            primary: None,
            last_primary: None,
        }
    }

    pub fn sample(&mut self) -> Sample {
        let now = Instant::now();

        // 接口表 60 秒重建一次：接口增删/重播远比这慢，
        // 每秒重枚举整张表是空闲 CPU 的主要来源。
        let need_discover = self.cached.is_empty()
            || self
                .last_discover
                .map(|t| now.duration_since(t).as_secs_f64() > 60.0)
                .unwrap_or(true);
        if need_discover {
            self.cached = enumerate()
                .into_iter()
                .map(|r| Cached {
                    index: r.index,
                    alias: r.alias,
                    kind: r.kind,
                })
                .collect();
            self.last_discover = Some(now);
            self.last_primary = None;
        }

        let mut rows: Vec<(u32, u64, u64, bool, u64)> = Vec::with_capacity(self.cached.len());
        let mut lost = false;
        for c in &self.cached {
            match read_row(c.index) {
                Some((i, o, is_up, link)) => rows.push((c.index, i, o, is_up, link)),
                None => lost = true,
            }
        }
        if lost {
            self.cached.clear();
            self.last_discover = None;
        }

        // 默认路由 5 秒核对一次
        if self
            .last_primary
            .map(|t| now.duration_since(t).as_secs_f64() > 5.0)
            .unwrap_or(true)
        {
            let connected: Vec<u32> = rows.iter().filter(|r| r.3).map(|r| r.0).collect();
            self.primary = primary_index(&connected);
            self.last_primary = Some(now);
        }
        let primary = self.primary;

        let mut ifaces = Vec::with_capacity(rows.len());
        for (idx, in_octets, out_octets, is_up, link_bps) in rows {
            let (alias, kind) = match self.cached.iter().find(|c| c.index == idx) {
                Some(m) => (m.alias.clone(), m.kind),
                None => (String::new(), "ethernet"),
            };

            let (down, up) = match self.prev.get(&idx) {
                Some((pi, po, pt)) => {
                    let secs = now.duration_since(*pt).as_secs_f64().max(0.001);
                    let backward = in_octets < *pi || out_octets < *po;
                    if backward || secs > 10.0 {
                        (0.0, 0.0)
                    } else {
                        (
                            (in_octets - *pi) as f64 / secs,
                            (out_octets - *po) as f64 / secs,
                        )
                    }
                }
                None => (0.0, 0.0),
            };

            self.prev.insert(idx, (in_octets, out_octets, now));

            ifaces.push(IfaceSample {
                id: idx,
                alias,
                kind,
                state: if is_up { "up" } else { "down" },
                down,
                up,
                link_mbps: if link_bps > 0 {
                    Some(link_bps as f64 / 1_000_000.0)
                } else {
                    None
                },
            });
        }

        let live: Vec<u32> = ifaces.iter().map(|i| i.id).collect();
        self.prev.retain(|k, _| live.contains(k));

        let (total_down, total_up) = match primary {
            Some(pid) => ifaces
                .iter()
                .find(|i| i.id == pid)
                .map(|i| (i.down, i.up))
                .unwrap_or((0.0, 0.0)),
            None => (0.0, 0.0),
        };

        Sample {
            ts: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            primary_id: primary,
            total_down,
            total_up,
            ifaces,
        }
    }
}
