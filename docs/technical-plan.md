# NetGlass 技术方案

> 版本：v0.1 · 2026-11-12 · 适用机器：Windows 11 build 26300.9818（Home China）、单屏 2560×1600 @150%、100 Mbps PPPoE 出口

## 1. 目标与非目标

**目标**：一个常驻桌面的浅色玻璃小卡片，1 Hz 显示当前出口的真实上下行速度，并在同一屏上给出 Wi-Fi / 宽带 / 加速器三行的状态；点击穿透可锁定；开机自启可开关。

**非目标**：不做加速、限速、防火墙、按进程流量统计（需驱动/ETW）、历史持久化、跨平台。

## 2. 已由实测确定的前提

| 事实 | 影响 |
|---|---|
| 拨号连接不是 NDIS 适配器，`Get-NetAdapterStatistics` 没有它 | 速度统一走 `GetIfEntry2`（80 µs，64 位） |
| 以太网计数器比拨号高 0.56%（下行）/31%（上行） | 不能用底层网卡代替拨号 |
| `WlanQueryInterface` 单次 11.7 ms | Wi-Fi 状态用 `WlanRegisterNotification`，属性 10 s 一次 |
| build 26300 没有壁纸 WorkerW，`0x052C` 失效 | 放弃"图标之下"，改置顶卡片 |
| CSS `backdrop-filter` 模糊不到桌面 | 玻璃必须由 DWM 提供（Mica / 亚克力） |
| 亚克力在拖动/缩放时掉帧 | 窗口固定尺寸、不可缩放 |

## 3. 架构总览

单进程 Tauri 2 应用。Rust 侧负责采集与窗口行为，前端只负责渲染。

```
┌────────────── Rust（后台） ──────────────┐        ┌──── WebView2 ────┐
│ iface::sampler  ──1 Hz──►  event: sample │        │ React 卡片        │
│ wifi::listener  ──事件──► event: wifi    │  IPC   │  ├ SpeedPair      │
│ probe::scheduler ─30 s──► event: probe   │ ─────► │  ├ Sparkline      │
│ proxy::watcher   ──5 s──► event: proxy   │        │  ├ Chips          │
│ window::effects / position / fullscreen  │        │  └ Details        │
│ tray / autostart / shortcut / single     │        └──────────────────┘
└──────────────────────────────────────────┘
```

- **采样线程**：独立 `std::thread`，循环 `sleep(1000ms)` 读计数器，算差分后 `emit`。不放进 tokio，避免与异步任务抢调度。
- **异步任务**：探针、代理探测、全屏检测、配置写盘，用 `tokio`。
- **前端节流**：事件 1 Hz；数字用 `useMotionValue` 驱动 DOM 文本，**不触发 React 重渲染**；曲线每秒只改一次 `d` 属性。

## 4. 目录结构

```
netglass/
├─ src/                        # React + TS
│  ├─ main.tsx  App.tsx  types.ts
│  ├─ components/  Card / Header / SpeedPair / Sparkline / Chips / Details
│  ├─ hooks/       useSample / useProbe / useSpringNumber / useLock
│  └─ styles/      tokens.css / card.css
├─ src-tauri/
│  ├─ tauri.conf.json  capabilities/default.json  Cargo.toml
│  └─ src/
│     ├─ lib.rs  main.rs  events.rs  config.rs
│     ├─ iface/   discover.rs  counter.rs  route.rs
│     ├─ wifi.rs  proxy.rs
│     ├─ probe/   mod.rs  http_connect.rs  socks5.rs  timing.rs
│     └─ window/  effects.rs  position.rs  fullscreen.rs  lock.rs
├─ docs/adr/  GLOSSARY.md
└─ prototype/                  # 一次性原型，交付前删除
```

## 5. 数据模型

```ts
type IfaceKind = 'wifi' | 'dialup' | 'ethernet' | 'tun'
type IfaceState = 'closed' | 'disconnected' | 'up'   // 空状态的三种形态

interface IfaceSample {
  id: string            // 接口索引的稳定化标识（Luid 优先）
  alias: string         // 「宽带连接」
  kind: IfaceKind
  state: IfaceState
  down: number          // B/s
  up: number            // B/s
  linkMbps?: number     // 协商速率
}

interface Sample {
  ts: number
  primaryId: string     // 默认路由所在接口
  total: { down: number; up: number }
  ifaces: IfaceSample[]
}

interface Probe {
  ts: number
  viaProxy: boolean
  ok: boolean
  tcpMs?: number; tlsMs?: number; ttfbMs?: number
  error?: string
}

interface ProxyState {
  systemProxy: boolean
  endpoint?: string          // 127.0.0.1:7897
  accelerators: { name: string; running: boolean; mode?: string }[]
}
```

## 6. 关键流程

### 6.1 接口发现与采样

1. 启动时 `GetIfTable2` 遍历，挑出：`IfType==23 && ConnectionType==DialUp`（拨号）、`IfType==71`（Wi-Fi）、`OperStatus==Up` 的有线、以及名称匹配 TUN 的虚拟网卡。
2. 读取 `GetBestRoute2` / `GetIpForwardTable2` 得默认路由 → `primaryId`（**不按度量值猜**）。
3. 每 1 s 对候选接口 `GetIfEntry2`，`delta = cur - prev`。
4. `delta < 0` 视为计数器重置（重播/唤醒），本拍记 0 并标记 `reset`，下一拍重新建立基线。
5. 注册 `NotifyIpInterfaceChange`，接口增删或重播时重新发现（索引会变）。
6. `emit("net://sample", Sample)`。

### 6.2 Wi-Fi

`WlanRegisterNotification` 只在连接/断开时唤醒；唤醒后 `WlanQueryInterface`（opcode 7）取信号、频段、协商速率，之后每 10 s 刷新一次。射频关闭时 state=`closed`，不产生任何 0 值噪声。

### 6.3 探针

- 周期 30 s（±5 s 抖动），超时 5 s。
- 路径 A（直连）：`TcpStream::connect(target:443)` → 记录 tcpMs → rustls 握手 → 记录 tlsMs → `GET / HTTP/1.1` → 记录 ttfbMs。
- 路径 B（经代理）：仅当检测到 `127.0.0.1:<port>` 在监听时执行。先试 HTTP `CONNECT`，失败再试 SOCKS5 握手；同样测 tcp/tls/ttfb。
- 目标可配置，默认 `github.com:443`。
- 失败不重试（等下一轮），连续 3 次失败 → 代理状态灯转红。
- 吞吐测试只在用户点击"测一下"时执行：拉 256 KB，算 MB/s。

### 6.4 加速器状态

- 系统代理：读 `HKCU\...\Internet Settings` 的 `ProxyEnable` / `ProxyServer`。
- 进程：Watt Toolkit（`Steam++.exe` / `Steam++.Accelerator.exe`）、SakuraCat（`SakuraCat.exe` / `core.exe`）、UU（`uu.exe`），只判断是否存在。
- TUN：扫描适配器名是否含 `wintun` / `mihomo` / `sakura`。
- Clash API（可选）：依次试 `127.0.0.1:9090`、`9097`、`9091` 的 `/version`，带 200 ms 超时；成功才启用 `/traffic`。默认不可用，属静默降级。
- 5 s 一轮，变化才 `emit`。

### 6.5 窗口

- 配置：`transparent: true`、`decorations: false`、`alwaysOnTop: true`、`skipTaskbar: true`、`resizable: false`、`visible: false`。
- **玻璃**：`apply_mica(window, Some(false))`（浅色 Mica）。若观感偏灰，降级为 `apply_acrylic(window, Some((242, 244, 248, 150)))`。CSS 只负责内层高光、描边、阴影，**不承担模糊**。
- 就绪后再 `show()`，避免 WebView2 白闪。
- 拖动：前端 `data-tauri-drag-region`；拖动结束把逻辑坐标写回配置。
- 吸附：距工作区边缘 < 12 px 时对齐。
- 位置恢复：落在不存在的显示器上时，夹到最近显示器的工作区内。
- 多屏：每 2 s 取鼠标所在显示器，变化时按相对位置迁移。
- 全屏：`SHQueryUserNotificationState` 每 2 s 一次，`QUNS_RUNNING_D3D_FULL_SCREEN` / `QUNS_PRESENTATION_MODE` 时隐藏。
- 锁定：全局快捷键 `Ctrl+Alt+L` → `set_ignore_cursor_events(true)`，同时把卡片边缘高光调暗 1.5 s 作为反馈。

### 6.6 启动与退出

- `tauri-plugin-single-instance`：第二次启动唤起已有实例。
- 自启：`tauri-plugin-autostart`（写 HKCU Run），启动后静默驻留，`sleep(10s)` 再显示。
- 关闭按钮不存在；托盘菜单：显示/隐藏、锁定、开机自启（勾选）、退出。
- 托盘图标静态，不随速度重绘。

## 7. 配置

`%APPDATA%\com.netglass.app\config.json`

```json
{
  "position": { "x": 1280, "y": 40 },
  "monitor": "\\\\.\\DISPLAY1",
  "locked": false,
  "autostart": true,
  "hideOnFullscreen": true,
  "probeTarget": "github.com:443",
  "probeIntervalSec": 30,
  "startDelaySec": 10
}
```

写入去抖 500 ms，避免拖动时频繁写盘。

## 8. UI 规格

- 卡片 320×200，圆角 24，内边距 18；迷你档 220×120（MVP-3 再做）。
- 层级：状态行（11 px）→ 大数字（40 px / 600 / `tabular-nums`）→ 状态胶囊（9.5 px）→ 曲线（底部 78 px 出血）。
- hover：高度 200→268（420 ms，`cubic-bezier(.16,1.02,.32,1)`），详情层淡入上移 6 px。
- 数字：`useSpring`（stiffness 180 / damping 22）从旧值滚到新值；采样 1 Hz，显示做 EMA(α=0.35) 平滑，峰值保持 1.2 s。
- 空状态三态：`closed` 灰点 + "已关闭"；`disconnected` 空心点 + "未连接"；`up` 且速率为 0 → 实心点 + "空闲"。
- 托盘/图标资源用 SVG 转 ICO，浅色卡片配深色描边图标。

## 9. 依赖

**Rust**：`tauri 2`、`tauri-plugin-autostart`、`tauri-plugin-global-shortcut`、`tauri-plugin-single-instance`、`window-vibrancy`、`windows`（`Win32_NetworkManagement_IpHelper`、`Win32_NetworkManagement_Ndis`、`Win32_Networking_WinSock`、`Win32_UI_WindowsAndMessaging`、`Win32_Graphics_Dwm`、`Win32_System_Threading`）、`tokio`、`tokio-rustls`、`serde`、`serde_json`。

**前端**：`react`、`react-dom`、`framer-motion`、`@tauri-apps/api`、`vite`、`typescript`。不引 UI 框架，样式手写 CSS 变量。

## 10. 风险与对策

| 风险 | 概率 | 对策 |
|---|---|---|
| 浅色 Mica 在透明窗口上仍是灰调 / 不透 | 中 | 降级到亚克力 + 自定义浅色 tint；两者都在 S1 里先验证 |
| 亚克力拖动掉帧 | 中 | 固定尺寸 + 拖动结束才重绘；S1 实测，必要时改 Mica |
| WebView2 启动白闪 | 高 | `visible:false` + 前端 `ready` 后 show |
| 重播后接口索引变化 | 高 | `NotifyIpInterfaceChange` 重新发现，不缓存索引 |
| 计数器重置导致负速度 | 中 | 负差分按重置处理 |
| 全屏误判（最大化窗口 / 视频网站非全屏） | 中 | 用 `SHQueryUserNotificationState`，不靠几何判断 |
| 探针被墙导致长期失败 | 高 | 失败只影响状态灯，不影响主数字；目标可配 |
| 无签名 exe 被杀软误报 | 中 | MVP 阶段接受；发布前再考虑签名 |
| 4K@200% 下 320 逻辑像素太小 | 中 | 坐标一律用逻辑像素，`scaleFactor` 参与换算 |

## 11. 里程碑与验收

**S1 技术验证（半天，先做）**
一个 200 行以内的 Tauri 窗口：透明 + 浅色 Mica + 置顶 + 拖动 + 一个假数字。
验收：截图确认是浅色玻璃而非灰块/黑块；拖动不掉帧。

**MVP-1（可运行）**
接口发现 + 1 Hz 采样 + 真实数字 + 曲线 + 拖动/吸附/记忆位置。
验收：下载一个文件，卡片读数与任务管理器/实际速度差 ≤ 5%；拔掉网络后 3 秒内进入空状态。

**MVP-2（完整）**
探针 + 加速器状态 + 托盘 + 自启开关 + 锁定穿透 + 全屏隐藏 + 单实例。
验收：开关 SakuraCat，代理行 3 秒内变状态；锁定后点击卡片能选中桌面图标；Win+D 后卡片仍在；重启电脑后 10 秒出现。

**MVP-3（打磨）**
迷你档、吸附动画、断线闪烁、详情层信息补全、设置面板。

## 12. 不做的事（明确记录）

- 不做按进程/按应用的流量统计（需内核驱动或 ETW 提权）。
- 不做流量历史与月度统计（v1 无持久化）。
- 不做 Clash 节点切换、订阅管理（只读）。
- 不做跨平台。
