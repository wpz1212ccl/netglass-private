# NetGlass 全量性能剖析与轻量化方案

> 扫描时间：2026-11-12 11:28–11:35 (GMT+8) ｜ 环境：Windows 11 build 26300.9818 / 2560×1600 @150% / 16 逻辑核 / 平衡电源 / **AC 供电 94%**
> 扫描范围：全部参与运行的源码（10 个文件，约 100 KB）+ 两个 Cargo.toml + 前端 HTML + 构建产物 + 运行中进程的实测采样
> 结论口径：所有标「实测」的数字来自本机命令输出；所有标「推算」的数字注明计算依据；所有标「目标」的数字是待验证值，不是已达成的效果

---

## 0. 结论先行

**当前交付版的性能瓶颈不在业务逻辑，而在 WebView2 运行时。** `dist/NetGlass.exe` 与 `src-tauri/target/release/netglass.exe` 的 SHA256 完全一致（`79ccc2c4…e888783ea33a`），即现在托盘里挂的是 Tauri + WebView2 版，它会拉起 6 个子进程常驻。

**仓库里已经存在答案**：`native/` 是一个纯 Win32 + Direct2D 实现，两个版本此刻都在本机运行。实测对比：

| 指标（空闲，本机实测） | 当前交付 Tauri/WebView2 | 原生 native | 削减 |
|---|---|---|---|
| 进程树私用内存 | **246.7 MB** | **37.0 MB** | **−85.0%** |
| 进程树工作集 WS | 298.5 MB | 51.4 MB | −82.8% |
| 空闲 CPU（单核口径） | 3.386% | 0.26% | **−92.3%（13×）** |
| 子进程数 | 6 | 0 | −6 |
| 可执行文件体积 | 9,986,048 B | 282,624 B | 35.3× |
| Cargo.lock 包数 | 491 | 16 | 30.7× |
| 定时唤醒次数/天（推算） | 212,709 | 89,280 | −58.0% |

> 内存数字来自按 ParentProcessId 归属的进程树求和，未混入系统里另外 6 个属于其他应用的 WebView2 进程。CPU 数字来自 12 秒窗口 `TotalProcessorTime` 增量、按同一进程树归属统计。

**但 native 版目前是渲染 spike，不是可交付版**：缺托盘、开机自启、单实例、锁定穿透、全屏隐藏、位置记忆、hover 详情、延迟数值、Wi-Fi 射频状态（共 9 项），右键直接 `DestroyWindow`。所以「直接换过去」会丢功能。

**推荐路线：先 C 止血 → 再 A 终态。**
- **C（本周即可见效，零产品风险）**：把 §6 的 P1/P2 改动打到当前 Tauri 版，主要消除 123,429 次/天的前台轮询、隐藏时照跑的渲染、每秒无条件重绘。CPU 与电池立刻改善，内存不变。
- **A（终态）**：把 9 项功能从 Tauri 版移植到 native（都是 Win32 API，无需 WebView），切换交付主线，一次性拿到上表的全部削减。

**备选路线 B（不推荐）**：保留 WebView2，用 `TrySuspend` + 去掉三层常驻合成层 + 事件驱动把 CPU 压下来。`docs/delivery-report.md` §8.2 已承认「除非改为原生渲染，否则压不下去」——246.7 MB 降不了，无法满足「没有任何负载担忧」。

---

## 1. 扫描范围与排除依据

| 路径 | 是否纳入 | 依据 |
|---|---|---|
| `src-tauri/src/{main,netmon,probe}.rs` | ✅ 纳入 | 当前交付版后端，经 SHA256 确认 |
| `src/index.html` + `src/fonts/inter-var.woff2` | ✅ 纳入 | 当前交付版前端（`tauri.conf.json` → `frontendDist: ../src`） |
| `src-tauri/{Cargo.toml,tauri.conf.json,capabilities/default.json}` | ✅ 纳入 | 构建与窗口配置 |
| `native/src/{main,netmon,render,window}.rs` + `native/Cargo.toml` | ✅ 纳入 | 正在运行的第二实现，轻量化候选 |
| `dist/NetGlass.exe`、两处 `target/release/*.exe` | ✅ 纳入 | 产物一致性与体积 |
| `prototype/**`（pm2–pm7、probe、visual、window-lab、native） | ❌ 排除 | 一次性原型；`grep` 确认主应用无任何引用 |
| `spike-s1/**` | ❌ 排除 | S1 早期 spike，无引用 |
| `.scratch/**`、`sessionlog/**`、`docs/**` | ⚠️ 仅作证据 | 不参与运行，用于回溯已有实测结论 |
| 各 `target/**` | ❌ 排除 | Rust 构建产物，非源码 |

---

## 2. 阶段一 · 信息采集与解析（结构化字段表）

| 字段 | 值 | 来源 | 口径 |
|---|---|---|---|
| `delivered_binary` | `dist/NetGlass.exe` | 运行中进程 CommandLine | 绝对路径 |
| `delivered_impl` | `src-tauri` (Tauri 2 + WebView2) | SHA256 与 `src-tauri/target/release/netglass.exe` 一致 | 二进制级 |
| `delivered_size_b` | 9,986,048 | `ls -l` | 磁盘字节 |
| `native_size_b` | 282,624 | `ls -l` | 磁盘字节 |
| `tree_priv_mb.delivered` | 246.7 | 进程树 `PrivatePageCount` 求和 | 含 6 子进程 |
| `tree_priv_mb.native` | 37.0 | 同上 | 单进程 |
| `tree_ws_mb.delivered` | 298.5 | 进程树 `WorkingSetSize` 求和 | 含 6 子进程 |
| `tree_ws_mb.native` | 51.4 | 同上 | 单进程 |
| `child_ws_mb.webview2` | 226.7 | 6 个 `msedgewebview2.exe` 求和 | 仅 NetGlass 名下 |
| `idle_cpu_pct_onecore.delivered` | 3.386 | 12 s 窗口 CPU 增量 / 12 s | 单核百分比 |
| `idle_cpu_pct_onecore.native` | 0.26 | 同上 | 单核百分比 |
| `idle_cpu_pct_machine.delivered` | 0.212 | 0.406 s / 12 s / 16 核 | 整机百分比 |
| `idle_cpu_pct_machine.native` | 0.016 | 0.031 s / 12 s / 16 核 | 整机百分比 |
| `logical_cores` | 16 | `Win32_ComputerSystem` | — |
| `doc_idle_cpu_pct` | 0.104 | `docs/delivery-report.md` §3 | 历史 45 s 均值，**主进程口径**，不可与整树 3.386% 直接比较 |
| `doc_tree_priv_mb` | 215 | `dist/README.txt` L29 / 交付报告 §3 | 历史值，本次实测 246.7 MB |
| `power_source` | AC（BatteryStatus=2） | `Win32_Battery` | 电池场景**未实测** |
| `charge_pct` | 94 | `Win32_Battery` | — |

> 口径统一说明：本次报告统一使用「整树 / 单核 %」。历史文档的 0.104% 是主进程单核口径，不含 WebView2 子进程，两者不冲突。

---

## 3. 阶段二 · 唤醒来源结构化清单

> **技能适配说明**：本阶段指定的 phosphor 是「基于 DTrace 的 Ruby 运行时事件采集」，其文档明确「不支持 Windows 或 Linux」，且本项目为 Windows + Rust，无 Ruby 运行时。按专家规则「技能不可用时用自身能力直接产出」，此处改用等价口径：以源码中的定时常数 + Win32 事件 API 为事件源，输出同构的结构化事件表。全部字段可回溯到文件行号。

| event_id | 事件源 | 周期常数 | 代码位置 | 次数/天（推算） | 类别 | 可否事件驱动 |
|---|---|---|---|---|---|---|
| `E01` | 前台窗口轮询线程 | 700 ms | `src-tauri/src/main.rs:386` | 123,429 | 定时轮询 | ✅ `SetWinEventHook(EVENT_SYSTEM_FOREGROUND)` |
| `E02` | 计数器采样 / IPC emit | 1000 ms | `src-tauri/src/main.rs:449-458` | 86,400 | 定时轮询 | ⚠️ 主读数必须保留；空闲可降频 |
| `E03` | 代理探针（TCP/CONNECT） | 30 s | `src-tauri/src/main.rs:461-469`、`probe.rs:200-213` | 2,880 | 网络唤醒 | ✅ 电池时暂停 / 事件触发 |
| `E04` | 接口表重枚举 | 60 s | `src-tauri/src/netmon.rs:277-293` | 1,440 | 定时轮询 | ✅ `NotifyIpInterfaceChange` |
| `E05` | 默认路由核对 | 5 s | `src-tauri/src/netmon.rs:309-317` | 17,280 | 定时轮询 | ✅ `NotifyRouteChange2` |
| `E06` | 进程快照 / 加速器 | 60 s 缓存，30 s 轮次检查 | `probe.rs:159-190` | 2,880 次检查 | 定时轮询 | ✅ 缓存已有，检查归并无成本 |
| `E07` | native 采样 post | 1000 ms | `native/src/main.rs:256-267` | 86,400 | 定时轮询 | ⚠️ 同上 |
| `E08` | native 代理端口扫描 | 30 s | `native/src/main.rs:272-288` | 2,880 | 网络唤醒 | ✅ 电池时暂停 |
| `E09` | 前端 rAF 弹簧 | 变化 >1% 时 30 fps / 最长 300 ms | `src/index.html:302-334,358` | 有流量时持续 | 渲染 | ✅ 脏检查 |
| `E10` | WebView2 合成器 | 刷新率 | `src/index.html:38,51-54`（常驻层） | 5,184,000 帧（60 fps 理论上限，推算） | 渲染 | ✅ 去掉常驻合成层 / `TrySuspend` |
| `E11` | 启动期调试落盘 | 每次启动 | `src-tauri/src/main.rs:290` (`netmon::dump`) | 1/次 | 磁盘 I/O | ✅ 用 `cfg(debug_assertions)` 门控 |
| `E12` | 模式切换日志 | 每次切换 | `src-tauri/src/main.rs:157-167,430` | 不定 | 磁盘 I/O | ✅ 同上 |

**推算依据**：次数 = 86,400 s ÷ 周期；`E01` = 86400/0.7 = 123,429（四舍五入）。合计 Tauri 版 212,709 次/天，native 版 89,280 次/天（仅 E07+E08），降幅 58.0%。**E09/E10 未计入合计**，因为它们不是定时唤醒而是渲染负载，量级上远超定时唤醒。

**对电池最关键的三条**：`E10`（持续合成）、`E01`（无意义轮询）、`E03/E08`（每 30 秒把网卡从省电态拉起）。

---

## 4. 全量问题清单（按文件:行号）

严重度：🔴 决定性 / 🟠 高 / 🟡 中 / ⚪ 低

### A. 架构级

| # | 位置 | 问题 | 影响 | 严重度 |
|---|---|---|---|---|
| A1 | `src-tauri/Cargo.toml:12`、`tauri.conf.json:11-25` | Tauri 2 + WebView2 透明窗口，拉起 6 个 `msedgewebview2.exe` | 实测整树私用 246.7 MB，其中子进程 WS 226.7 MB；这是内存主因 | 🔴 |
| A2 | `src/index.html:38` `will-change:transform`；`:51-54` `.grain` 用 SVG `feTurbulence` + `mix-blend-mode:overlay`；`:30-35` 五重 `box-shadow`（含 2 个 inset）；`:40-49` 两个 radial-gradient 伪元素 | 透明窗口下强制独立合成层 + 分形噪声滤镜 + 混合模式，每一层都参与合成 | WebView2 GPU 进程持续工作，实测整树空闲 CPU 3.386%（单核），是 Rust 侧的 13× | 🔴 |
| A3 | `Cargo.lock` 491 包 vs 16 包；exe 9.99 MB vs 276 KB | 依赖规模 | 冷启动加载、磁盘占用、常驻 DLL 映射 | 🟠 |
| A4 | `native/src/main.rs:210-213` | native 版功能缺口 9 项，右键直接销毁窗口 | 无法直接替换交付版，是切主线的**唯一实质障碍** | 🔴 |

### B. 唤醒与功耗（电池场景）

| # | 位置 | 问题 | 影响 | 严重度 |
|---|---|---|---|---|
| B1 | `src-tauri/src/main.rs:381-435` | 700 ms 前台轮询，每次 `GetForegroundWindow` + `GetClassNameW` + `String::from_utf16_lossy` 分配 | 123,429 次/天唤醒，纯浪费；可用 WinEvent hook 完全消除 | 🔴 |
| B2 | `src-tauri/src/main.rs:93-95` `UiMode::Hidden → win.hide()` | **只隐藏窗口，采样/探针/前台轮询/渲染全部照跑** | 「挂托盘隐藏一整天」的负载和显示时几乎一样；这是用户诉求里最致命的一条 | 🔴 |
| B3 | 全代码库 | 无 `WM_POWERBROADCAST` / `PBT_POWERSETTINGCHANGE` / `GetSystemPowerStatus` | 插电与电池行为完全相同，无法按需降频 | 🟠 |
| B4 | `src-tauri/src/main.rs:461-469`、`probe.rs:193-207` | 每 30 s 直连 TCP（3 s 超时）+ 5 端口 × 80 ms 扫描 + 可能 CONNECT（5 s 读超时） | 每轮最多 8.4 s 阻塞窗口 + 网卡唤醒 2,880 次/天 | 🟠 |
| B5 | `src-tauri/src/main.rs:290`、`netmon.rs:187-226` | 生产路径每次启动 `dump()`：全表枚举 + 写 `%TEMP%` 文件 | 启动期磁盘 I/O，SSD 唤醒 | 🟡 |
| B6 | `src-tauri/src/main.rs:157-167,430` | `log_line()` 每次模式切换 open/append/close 文件 | 频繁切换时反复磁盘 I/O | 🟡 |
| B7 | `src/index.html:302-334,358-362` | 变化 >1% 即启动最长 300 ms @30 fps 的 rAF 动画 | 有流量时持续 30 fps 重绘；空闲已做 1% 阈值，尚可 | 🟡 |

### C. 分配、IPC 与绘制

| # | 位置 | 问题 | 影响 | 严重度 |
|---|---|---|---|---|
| C1 | `src-tauri/src/main.rs:455` → `src/index.html:341-376` | 每秒把完整 `Sample`（全部 ifaces 的 String + f64）序列化成 JSON 跨进程 IPC，前端再解析并约 15 次 `textContent` 赋值 | 即使速率恒为 0 也照发；双重分配 + DOM 抖动 | 🟠 |
| C2 | `native/src/main.rs:107-121` | `refresh()` **无条件** `repaint()`；采样线程与代理线程都 `post_sample` | 空闲时数据完全相同却每秒全量 D2D 绘制 + 位图上传 | 🟠 |
| C3 | `native/src/main.rs:87-100` | `frame()` 每次克隆 9 个 String + `hist.to_vec()` | 每帧堆分配 | 🟡 |
| C4 | `native/src/render.rs:265-275,289,304-318` | 同一文本 `measure()` 与 `draw_text()` 各建一次 `IDWriteTextLayout`（重复 shaping）；`:398,419` 每帧新建 2 个 `ID2D1PathGeometry`（60 点贝塞尔） | 每帧重复 DirectWrite 排版 + COM 对象创建 | 🟠 |
| C5 | `native/src/window.rs:97-109` | 每次 `present()` 都 `GetDC(None)` / `ReleaseDC` + `UpdateLayeredWindow` 上传整张 DIB | 150% DPI 下 DIB = 480×300×4 = 576,000 B（562.5 KiB，推算），每秒一次 | 🟡 |
| C6 | `src/index.html:250-271,354-356,373-374` | 每秒新建 3 个数组、4 次 `Math.max(...hist)` 展开、hist 的 push/shift 搬移 | 短生命周期分配，量小但可归零 | ⚪ |
| C7 | `src-tauri/src/netmon.rs:323-324`（`native/src/netmon.rs:323-324` 同） | 每秒为每个接口 `m.alias.clone()`，且 `cached.iter().find()` 线性查找 | 每秒字符串克隆 | ⚪ |
| C8 | `probe.rs:64` `DNS_CACHE` 只增不减 | 仅 2 个固定 host，上界 2 | **无泄漏风险**，仅记录 | ⚪ |

### D. 长跑正确性

| # | 位置 | 问题 | 影响 | 严重度 |
|---|---|---|---|---|
| D1 | `native/src/render.rs:463-467` | `D2D1_LAYER_PARAMETERS.geometricMask = ManuallyDrop::new(Some(self.mask.clone()))`，`PushLayer` 后未见配对的 `ManuallyDrop::drop` | `clone()` 每帧 AddRef；泄漏的是**引用计数**而非新对象（同一个 mask），不会撑大内存，但属正确性缺陷。**待确认**：需核对 windows-rs 0.62 ABI 约定并实测 24 h 曲线 | 🟡 待确认 |
| D2 | `docs/delivery-report.md:54` | 「24 小时长跑 ⚠️ 未跑满；内存 10 分钟无增长趋势」 | 缺长周期证据，无法保证挂一天无累积 | 🟠 |
| D3 | `native/src/main.rs:109` `try_recv` 排空 | 若采样快于 UI 会积压；当前 1 Hz 不成立 | 无实际问题 | ⚪ |

---

## 5. 阶段三 · 分级优化方案（最小改动优先）

> 按 ponytail 口径：先给能跑通的最短路径，不引入新框架、不新增依赖、不重构架构。每条标注改动规模。

### P0 · 决定方向（需你拍板，见 §8）

| 路线 | 内容 | 收益 | 代价 |
|---|---|---|---|
| **A（终态，推荐）** | 把 9 项功能移植到 `native/`，切换交付主线，归档 `src-tauri/` | 实测 −85.0% 内存 / −92.3% CPU / −6 子进程 | 需重写托盘、自启、锁定等 Win32 逻辑；视觉从「圆角矩形 + Inter」变为「椭圆 + Segoe UI Variable」 |
| **B** | 保留 WebView2，`TrySuspend` + 去合成层 + 事件驱动 | CPU 可降 | **内存 246.7 MB 降不下来**，不满足诉求 |
| **C（第一步，立即做）** | 只打 P1/P2 的零风险补丁 | CPU 与电池明显改善 | 内存不变 |

### P1 · 电池与唤醒（改动小、收益大）

| # | 改动 | 位置 | 规模 |
|---|---|---|---|
| P1-1 | 用 `SetWinEventHook(EVENT_SYSTEM_FOREGROUND)` 替换 700 ms 轮询循环 | `src-tauri/src/main.rs:381-435` | ~30 行替换 |
| P1-2 | 窗口 Hidden / 锁定时：停止 emit、停止采样渲染、探针暂停；恢复时补一次 | `main.rs:93-95,449-469` | ~15 行 |
| P1-3 | `WM_POWERBROADCAST` + `RegisterPowerSettingNotification(GUID_ACDC_POWER_SOURCE)`，DC 时采样 1 Hz→0.5 Hz、探针 30 s→暂停、hist 60→30 点 | `wndproc` / 采样循环 | ~25 行 |
| P1-4 | `NotifyIpInterfaceChange` / `NotifyRouteChange2` 替换 60 s / 5 s 轮询 | `netmon.rs:277-317` | ~20 行 |
| P1-5 | 代理探针改为「端口状态变化时才测」，或 DC 时降到 5 min | `probe.rs:200-213` | ~10 行 |
| P1-6 | `netmon::dump()` 与 `log_line()` 加 `#[cfg(debug_assertions)]` | `main.rs:157-167,290` | 2 行 |

### P2 · 单次开销（可独立合入）

| # | 改动 | 位置 | 规模 |
|---|---|---|---|
| P2-1 | **渲染脏检查**：签名不变就跳过整帧 | `native/src/main.rs:107-121` | ~8 行（代码见下） |
| P2-2 | `present()` 的屏幕 DC 缓存进 `Surface` | `native/src/window.rs:79-112` | ~6 行 |
| P2-3 | 合并 `measure()`/`draw_text()` 的重复 TextLayout；PathGeometry 复用 | `native/src/render.rs:265-318,398-441` | ~25 行 |
| P2-4 | `frame()` 的 9 次 String clone 改 `&str`/`Cow` | `native/src/main.rs:87-100` | ~15 行 |
| P2-5 | emit 改为「变化才发」且只发前端需要的字段 | `main.rs:455` + `index.html:341-376` | ~15 行 |
| P2-6 | 去掉 `will-change` / `feTurbulence` 噪点 / `mix-blend-mode`（仅 B/C 路线需要） | `src/index.html:38,51-54` | 删 4 行 |
| P2-7 | release profile 加 `strip = true`、`lto = "fat"`、`codegen-units = 1` | `native/Cargo.toml:31-35` | 3 行（减体积，非内存） |

**P2-1 最小可用补丁（可直接粘贴）** —— 空闲时绝大部分秒的渲染输入完全相同，直接省掉整帧 D2D 绘制 + 576 KB 位图上传：

```rust
// App 增加字段： last_sig: String （初始化 String::new()）
fn refresh(&mut self) {
    let mut last = None;
    while let Ok(s) = self.rx.try_recv() { last = Some(s); }
    if let Some(s) = last { self.apply(&s); }
    let p = self.proxy.lock().ok().and_then(|g| *g);
    self.proxy_text = match p {
        Some(port) => format!("代理 :{port}"),
        None => "代理 未启用".to_string(),
    };

    // 渲染输入没变就不画：空闲时 1 Hz 的 sample 内容完全相同
    let sig = format!("{}|{}|{}|{}|{}|{}|{:?}",
        self.title, self.state, self.wifi, self.proxy_text,
        self.down.0, self.up.0, self.hist);
    if sig == self.last_sig { return; }
    self.last_sig = sig;
    self.repaint();
}
```

**P1-3 电源档位建议**（阈值是「目标/待确认」，需你定）：

| 状态 | 采样 | 探针 | 曲线点数 | 隐藏时 |
|---|---|---|---|---|
| AC 供电 + 卡片可见 | 1 Hz | 30 s | 60 | — |
| DC 电池 + 卡片可见 | 0.5 Hz | 暂停（或 5 min） | 30 | — |
| DC 电池 + 卡片隐藏/锁定 | 停止 | 停止 | 冻结 | 完全不重绘 |
| 全屏应用 / 系统睡眠 | 停止 | 停止 | 冻结 | 完全不重绘 |

> 「停止」指不做 `GetIfEntry2`、不做 IPC、不做 D2D/`UpdateLayeredWindow`；只保留消息循环等待 WinEvent 唤醒。这是「一整天挂托盘」应达到的空转状态。

---

## 6. 验收断言（BDD 风格，可复跑）

| ID | Given / When / Then | 阈值 | 阈值性质 |
|---|---|---|---|
| `A1` | Given 原生版运行 24 h，When 比较首尾，Then 进程树私用内存增长 | < 10 MB | 目标，待验证 |
| `A2` | Given 空闲 5 min，When 统计整树 CPU，Then 单核占比 | < 0.5% | 目标（实测 native 现值 0.26% 已达标） |
| `A3` | Given 任一时刻，Then 进程树私用内存 | < 60 MB | 目标（实测 native 现值 37.0 MB 已达标） |
| `A4` | Given 卡片隐藏，When 观察 60 s，Then 定时器唤醒次数 | = 0（仅 WinEvent 唤醒） | 目标 |
| `A5` | Given DC 电池，Then 探针触发间隔 | ≥ 300 s | 目标，待你确认 |
| `A6` | Given 空闲网络，When 观察 60 s，Then `refresh()` 实际重绘帧数 | ≤ 状态变化次数 | 目标（P2-1 后） |
| `A7` | Then 子进程数 | = 0 | 实测 native 已达标 |
| `A8` | Given 24 h 长跑，Then `D2D1_LAYER_PARAMETERS` 引用计数无单调增长 | 无增长 | 待确认（D1） |

复跑脚本：`docs/perf-check.ps1`（同目录），例：
```powershell
powershell -ExecutionPolicy Bypass -File "docs\perf-check.ps1" -Minutes 1440 -IntervalSec 60
```
脚本按父进程归属采样整树 WS / 私用 / CPU 增量 / 电池状态，输出 CSV，并在结束时打印 A1–A3、A7 的 PASS/FAIL。

两点实现约束，均已实测验证：
- **脚本刻意全 ASCII**：Windows PowerShell 5.1 用系统 ANSI 代码页解析无 BOM 的 `.ps1`，中文注释会吞掉引号导致解析失败（本机实跑复现）；PowerShell 7 的静态语法检查发现不了这个问题。
- **样本 < 3 时跳过断言**：短窗口捕获的是启动/退出 CPU 尖峰，不是空闲稳态。本机一次 1 分钟试跑中目标进程中途退出、仅剩 1 个样本，读数为 0.625%（单核），而 12 秒稳态实测是 0.26%——若据此判 FAIL 会得出错误结论。脚本现在遇到这种情况只提示 `INSUFFICIENT DATA`，不出 PASS/FAIL。

---

## 7. 待确认项（不编造，需你拍板或实测）

| # | 事项 | 类型 |
|---|---|---|
| Q1 | 是否接受切换到 `native/` 作为唯一主线？9 项功能中哪些可裁剪（尤其 hover 详情、延迟数值、Wi-Fi 射频状态）？ | 需你决定 |
| Q2 | 是否接受视觉变化：椭圆轮廓 + Segoe UI Variable（当前交付版是圆角矩形 + Inter）？ | 需你决定 |
| Q3 | 电池模式采样降到 0.5 Hz（读数 2 s 一跳）是否可接受？ | 需你决定 |
| Q4 | 代理探针在电池时是否允许暂停？ | 需你决定 |
| Q5 | DC 放电实测曲线：当前 `BatteryStatus=2`（AC 供电）、94%，**未做不插电实测** | 待实测 |
| Q6 | 24 h 长跑内存曲线：交付报告标注「未跑满，仅 10 分钟无增长」 | 待实测 |
| Q7 | `render.rs:463` 的 `ManuallyDrop` 引用计数是否确未配对（需核对 windows-rs 0.62 ABI + 长跑观测） | 待确认 |
| Q8 | 历史文档 215 MB 与本次 246.7 MB 的差异，可能来自运行时长/WebView2 版本；本轮以本次实测为准 | 已标注口径 |

---

## 8. 下一步

本轮按你的要求做**全量扫描与诊断**，没有改动任何运行中的代码（避免破坏当前交付版）。

- 若选 **C**：我可以直接把 P1-1 / P1-2 / P1-3 / P1-6 / P2-1 / P2-7 打进当前分支并重新构建实测对比。
- 若选 **A**：我先补 native 的托盘 + 自启 + 单实例 + 锁定 + 位置记忆，做一个功能对等的轻量版，再跑一次同样的进程树采样出对照表。
- 若先要证据：运行 `docs/perf-check.ps1 -Minutes 60` 拿 1 小时基线，再决定。

---

*本报告由 AI 生成，代码行号与实测数据可在上述路径复核；结论与阈值建议请结合实际使用场景确认后实施。*
