# NetGlass 常驻资源独立复核（2026-11-13）

> 性质：只读测量，未修改代码、未结束进程。与 `perf-spec-2026-11-12.md` 的实施后结论做交叉核对。
> 本文只收录实测数据与由数据直接推出的归因；未被测量的项单列在 §5，不给预期值。

## 0. 测量环境与方法

| 项 | 值 |
|---|---|
| 主机 | i5-13500H，12 物理核 / 16 逻辑线程，31.75 GB，Iris Xe（驱动 32.0.101.5763） |
| 显示 | 2560x1600 @150%（DPR 1.5） |
| 被测 | `dist\NetGlass.exe --silent`，PID 24768，启动 2026-11-13 09:44:26 |
| WebView2 | 157.0.4341.46，UDD `%LOCALAPPDATA%\com.netglass.app\EBWebView` |
| 进程树 | 24768 → 24812(browser) → 24840 crashpad / 25072 gpu-process / 25080 network / 25112 storage / 25232 renderer |
| 采样时窗口态 | 胶囊态，实测 HWND 67308 |

测量分三路并行，命令原文见各节：

1. **时间序列**：30 点 × 10 s（290.4 s），字段为每进程 CPU 累计秒、WorkingSet、PrivateMemory、HandleCount、Threads。
2. **GPU 归因**：`\GPU Engine(*)\Utilization Percentage`、`\GPU Process Memory(*)\Dedicated/Shared Usage`，2 s × 30；线程级上下文切换两次快照。
3. **单变量对照**：用 `msedge.exe --app` + 独立 `--user-data-dir` 加载两个本地页面（A 静态 / B 仅加 66 ms 呼吸灯，逐字复刻 `src/index.html` 的 `breatheTick`），各测 191 s CPU。

口径：**单核 % = CPU 秒增量 ÷ 采样秒数 × 100**；整机 % = 单核 % ÷ 16。

## 1. 实测数据

### 1.1 进程构成与稳态资源（19:49:18 快照）

| PID | 角色 | WorkingSet | Private | 累计 CPU | 生命周期均占单核 |
|---|---|---|---|---|---|
| 24768 | 主进程 | 13.85 MB | 8.64 MB | 224.609 s | 0.619% |
| 24812 | browser | 68.49 MB | 71.38 MB | 76.234 s | 0.210% |
| 24840 | crashpad-handler | 2.23 MB | 2.98 MB | 0.203 s | 0.001% |
| 25072 | gpu-process | 53.32 MB | 82.35 MB | 911.516 s | 2.512% |
| 25080 | network service | 15.63 MB | 11.73 MB | 1.203 s | 0.003% |
| 25112 | storage service | 4.86 MB | 7.64 MB | 0.188 s | 0.001% |
| 25232 | renderer | 54.21 MB | 44.96 MB | 723.312 s | 1.993% |
| **合计** | 7 进程 | **212.59 MB** | **229.68 MB** | **1937.265 s** | **5.338%** |

主进程附加指标：线程 29–30、句柄 384（30 点全程恒定）、GDI 对象 21、USER 对象 37。

### 1.2 5 分钟窗口 CPU（30 点，单核 %）

| 进程 | min | mean | max |
|---|---|---|---|
| gpu-process 25072 | 1.25% | **2.915%** | 6.25% |
| renderer 25232 | 1.10% | **2.494%** | 4.53% |
| 主进程 24768 | 0.00% | 1.078% | 3.60% |
| browser 24812 | 0.00% | 0.248% | 1.41% |
| network / storage / crashpad | 0.00% | ≈0% | 0.31% |
| **全树合计** | — | **6.73%** | — |

同一窗口内另一次 61.2 s 差分复核：gpu 3.33%、renderer 3.33%、主进程 1.05%、browser 0.31%，合计 8.02%。

### 1.3 10 小时 04 分生命周期

运行 36,292 s。各进程累计 CPU ÷ 运行秒数 = 全树平均 **5.338% 单核**（= 整机 0.334%）。
其中 gpu-process 2.512% 与 renderer 1.993% 合计 4.505%，占全树 84.4%。

### 1.4 无泄漏验证

| 指标 | 首点 | 末点 | Δ |
|---|---|---|---|
| 合计 PrivateMemory | 229.71 MB | 229.68 MB | **−0.03 MB** |
| 合计 WorkingSet | 204.36 MB | 212.59 MB | **+8.23 MB** |
| 合计句柄 | 4343 | 4345 | +2 |
| 主进程句柄 | 384 | 384 | 0 |

WorkingSet 的 +8.23 MB 不是单调增长：S01–S06 稳定在 204.36–204.54 MB，S07 单点阶跃至 212.91 MB，其后 22 个点在 212.27–213.66 MB（±1.4 MB）内波动。阶跃来源为 network service（10.01 → 15.73 MB）与 browser（65.42 → 68.30 MB），其余进程无对应跳变。

### 1.5 GPU 与显存

- 胶囊态窗口实测：`GetWindowRect` **144 × 34 物理像素**（DPR 1.5，折合 96 × 22.67 DIP）；`EXSTYLE=0x08040198` → TOPMOST + TOOLWINDOW + NOACTIVATE，`WS_EX_LAYERED = false`。
- 全树 GPU 引擎利用率 30 点：均值 **0.631%**、峰值 0.688%、最低 0.585%。
- 同一时段 dwm.exe 均值 **0.590%**（0.534–0.632%）。比值 1.07。
- 引擎实例数：仅 gpu-process 25072 有 14 个实例，且只有 `eng0 | 3d` 非零；renderer / browser / 主进程 / utility 的实例数均为 **0**（即无 GPU 引擎分配）。
- 显存（`GPU Process Memory`）：gpu-process 25072 Shared 均值 **29.43 MB**、峰值 29.45 MB，Dedicated **0 MB**（核显）。dwm 参照 Shared 2722.6 MB。其余 NetGlass 进程无该计数器实例。

### 1.6 上下文切换（71.0 s 窗口，线程级汇总）

| 进程 | 线程数 | 次/秒 |
|---|---|---|
| gpu-process 25072 | 23 | **525.6** |
| renderer 25232 | 29 | **342.1** |
| 主进程 24768 | 30 → 29 | 339.1（含线程退出偏差） |
| browser 24812 | 58 → 57 | 39.0 |
| network / storage | 22 / 11 | 0.14 / 0.11 |
| dwm.exe 参照 | 20 | 947.1 |

### 1.7 单变量对照实验（呼吸灯）

同一台机、同一 Edge 内核，A 页无脚本无动画，B 页仅增加与 `index.html:447-458` 逐字相同的 `setInterval(breatheTick, 66)`。两页内存与 CPU 均为 11 进程同口径：

| 指标 | A（静态） | B（呼吸灯） | Δ |
|---|---|---|---|
| gpu-process 均值 | 0.16% | **3.24%** | **+3.08 pp**（×20.9） |
| renderer 均值 | 0.02% | **1.88%** | **+1.86 pp** |
| gpu + renderer | 0.18% | **5.12%** | **+4.94 pp** |
| 全部进程 WorkingSet | 720.59 MB | 721.72 MB | +1.13 MB |
| 全部进程 Private | 395.86 MB | 395.28 MB | −0.58 MB |
| gpu-process WorkingSet | 168.61 MB | 172.78 MB | +4.17 MB |
| renderer WorkingSet 合计 | 231.93 MB | 227.78 MB | −4.15 MB |

复现批次（同方法）：

| 批次 | A gpu/renderer | B gpu/renderer |
|---|---|---|
| A7/B7 | 0.16% / 0.02% | 3.24% / 1.88% |
| A6/B6 | 0.23% / 0.09% | 2.06% / 1.20% |
| A3/B3 | 0.00% / 0.01% | 2.92% / 1.84% |

B 页在窗口被系统判定遮挡时（`CalculateNativeWinOcclusion` 生效），测得 gpu 0.00–0.10%、renderer 0.15–0.44%。

## 2. 与 2026-11-12 实施后基线的对照

| 指标 | 11-12 实施后记录 | 11-13 本次实测 | 判定 |
|---|---|---|---|
| 岛态单核 CPU | 中位 5.2%（4.1–7.2%） | 5 分钟均值 6.73%；10 h 均值 5.34% | 落在同一区间 |
| 岛态 WorkingSet | ~192 MB | 212.59 MB | 高 20.6 MB，差额主要在 browser(+3.0) 与 network(+5.6) 的阶跃，另 11-12 未逐进程分解 |
| 无泄漏 | 3 min +0.41 MB | 10 h Private −0.03 MB、句柄 +2 | 一致 |
| GPU 进程份额 | 10.5%（10 s 差分，当时的 15fps 版） | 2.915%（30 点均值） | 口径不同（11-12 为差分瞬时，本次为 30 点稳态） |
| renderer 份额 | 9.8%（同上口径） | 2.494%（同上） | 同上 |

结论：本次测量未发现与 11-12 结论冲突的数据；岛态总量与已记录的 4.1–7.2% 区间重合。

## 3. 根因（数据链）

### 3.1 CPU：呼吸灯是唯一被单变量验证的成本源

```
对照实验：去掉动画   0.18 pp
对照实验：仅加动画   5.12 pp   （Δ +4.94 pp）
NetGlass 实测全树    5.34 pp   （10 h 均值）
```

三者同量级。数据可确认的是：**在页面上只增加该 `setInterval` 呼吸灯，gpu-process 与 renderer 的 CPU 分别上升约 20 倍与 90 倍；内存变化在噪声内（+1.13 MB）**。

不可从现有数据推出的是：5.34 pp 中呼吸灯与其他因素（玻璃渐变、字体、SVG 曲线、1 Hz 数字更新）的精确占比——对照页的绘制面积为 213×133 CSS，而 NetGlass 胶囊实测仅 96×22.67 DIP，两者面积不可等价，因此不做百分比拆分。

### 3.2 内存：由 WebView2 进程模型决定，与页面复杂度无关

- 6 个 WebView2 进程合计 199.9 MB（212.59 − 13.85 主进程），其中 browser 68.49 + gpu 53.32 + renderer 54.21 = 176.0 MB。
- 对照实验证明：同一页面仅增加动画，内存变化 +1.13 MB（噪声量级），renderer 合计甚至 −4.15 MB。
- 对照实验同时证明：一个**无脚本、无动画、无自定义字体**的 Chromium 页面，仍拉起 11 个进程、720.59 MB（Edge app 模式，含 5 个 renderer、Extractor、SearchIndexer），说明进程与外壳成本不随页面内容线性变化。

### 3.3 GPU 利用率 0.63% 但 gpu-process 吃 2.9% CPU

实测事实：

- 全树 GPU 引擎利用率 0.631%，与系统 dwm 0.590% 同量级；
- 但 gpu-process 的 CPU 为 2.915%，上下文切换 525.6 次/秒（dwm 947.1 次/秒）；
- 其 14 个 GPU 引擎实例中仅 1 个 3D 引擎非零，videodecode / copy / videoprocessing / legacyoverlay 全为 0。

数据直接给出的是：**该进程的 CPU 消耗不体现在 GPU 引擎利用率上，而体现在每秒约 526 次上下文切换的提交/调度路径**。现有测量无法进一步拆分驱动提交、D3D 调用与线程调度各占多少（§5 第 6 项）。

### 3.4 与既有决策的关系（数据/记录事实，非评价）

本次实测涉及的三条路径在 `perf-spec-2026-11-12.md` / `adr/0004` 中已有记录：

| 路径 | 既有记录 | 本次实测是否支持该记录 |
|---|---|---|
| 呼吸灯 120 fps CSS → JS 15 fps | 基线 21–23% 降至 5.2%；剩余为"已批准的 15 fps 呼吸合成成本" | 支持：当前 5.34%，动画增量 4.94 pp |
| `MemoryUsageTargetLevel` | **否决**：会把内存换出到磁盘，胶囊态 1 Hz 脚本立刻换回，形成换出/换回 churn | 本次未实测该项，无法新增数据 |
| `--single-process` | **否决**：沙箱消失，且无独立渲染进程可回收，TrySuspend 失效 | 本次数据支持"独立进程是回收前提"：隐藏态 89 MB 的既有记录来自 renderer 独立回收 |
| 隐藏 IsVisible(false) + TrySuspend | 已实施；隐藏 WS 344 → 89 MB | 本次未复测隐藏态 |

## 4. 数据支持的资源分布（当前无改动）

| 分类 | 内存 | 占比 | 说明 |
|---|---|---|---|
| WebView2 固定三件套 | 176.0 MB | 82.8% | browser + gpu + renderer |
| WebView2 辅助进程 | 22.7 MB | 10.7% | network 15.63 + storage 4.86 + crashpad 2.23 |
| Rust 后端 | 13.85 MB | 6.5% | 主进程 |
| 页面内容增量（动画） | ≈1.1 MB | 0.5% | 对照实验 Δ，处于噪声内 |

CPU 分布（10 h 均值）：

| 分类 | 单核 % | 占比 |
|---|---|---|
| gpu-process | 2.512% | 47.1% |
| renderer | 1.993% | 37.3% |
| Rust 后端 | 0.619% | 11.6% |
| browser | 0.210% | 3.9% |
| utility ×3 | 0.004% | 0.1% |

## 5. 未被测量的项（不写入结论）

| # | 项 | 状态 | 缺少的测量手段 |
|---|---|---|---|
| 1 | `MemoryUsageTargetLevel` 在本项目的实际收益 | 未测 | 需改代码后在胶囊态 A/B；既有 ADR 已给出否决理由，但无本地数字 |
| 2 | 胶囊态脱离 WebView2（原生绘制）的内存下界 | 未测 | 需原型实现后测量 |
| 3 | 不透明窗口（`transparent:false`）对 GPU/CPU 的影响 | 未测 | 需改 `tauri.conf.json` 后 A/B |
| 4 | 玻璃层（多层 radial-gradient + mask + box-shadow）的独立成本 | 未测 | 对照实验只做了"动画"单变量，未做"玻璃"单变量 |
| 5 | 内核态 / 用户态 CPU 拆分 | 未测 | 需 `Get-Counter '\Process(*)\% User Time'` |
| 6 | gpu-process 的 CPU 在驱动提交 / D3D 调用 / 调度的分布 | 未测 | 需 ETW 或 WPA 采样 |
| 7 | 页面内部指标（JS 堆、DOM 节点、Layout、RecalcStyle、合成层数） | 拿不到 | NetGlass 未开远程调试端口：命令行无 `--remote-debugging-port`，`netstat` 无 9000–9999 及本树 PID 监听，`DevToolsActivePort` 不存在，三作用域 `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` 未设置，HKCU/HKLM/WOW6432Node 的 WebView2 策略键均不存在 |
| 8 | 隐藏 / 展开两态的本次复测 | 未做 | 采样窗口内窗口保持胶囊态 |
| 9 | 功耗（W） | 未测 | 需 `powercfg /batteryreport` 与放电实测 |

## 6. 复测协议（可复用）

```powershell
# 1) 整树内存
(Get-Process msedgewebview2 | Measure-Object WorkingSet64 -Sum).Sum / 1MB
# 2) 分进程 CPU（30 点 × 10 s）
Get-Process -Id 24768,24812,24840,25072,25080,25112,25232 | Select Id,CPU,WorkingSet64,PrivateMemorySize64,HandleCount
# 3) GPU 引擎（按 pid 聚合）
Get-Counter '\GPU Engine(*)\Utilization Percentage' -SampleInterval 2 -MaxSamples 30
# 4) 显存
Get-Counter '\GPU Process Memory(*)\Shared Usage' -SampleInterval 2 -MaxSamples 30
# 5) 上下文切换（线程级，两次快照相减）
Get-CimInstance Win32_PerfRawData_PerfProc_Thread |
  Where-Object { $ids -contains $_.IDProcess } |
  Select-Object IDProcess, ContextSwitchesPersec
```

判据（沿用 `perf-spec-2026-11-12.md` §6）：单核% 与整机% 必须标注、禁止混用；采样前后比对 `%TEMP%\netglass-log.txt` 行数判断污染；报中位 + p90；10 min / 24 h 增长 <10 MB 视为无泄漏。

## 7. 测量偏差与局限

1. 对照实验使用 `msedge.exe` app 模式，非 `msedgewebview2.exe`；其绝对内存（720 MB）受 Edge 外壳、5 个 renderer、后台服务影响，**只有 A↔B 的差值可用于推断页面增量**。
2. 对照实验窗口为 320×200 物理像素（213×133 CSS），NetGlass 胶囊为 144×34 物理像素（96×22.67 DIP），绘制面积不同，CPU 绝对值不可直接换算。
3. 对照实验为顺序测量（A、B 各自独立实例），非同时运行；采样间隔 10.3–10.7 s，已按实际间隔折算。
4. 对照实验期间机器上存在用户自开的 Edge 实例（17 进程），带来额外内存压力，可能改变实验实例的 WS 裁剪时点；CPU 按独立 `--user-data-dir` 逐进程统计，未混入。
5. Windows 进程 CPU 时间为 15.6 ms 粒度；A 页 renderer 191 s 内仅 31 ms，其 0.02% 不宜按比例解读，B 页 3.594 s 远大于粒度。
6. 时间序列为每点瞬时读取，30 点内已有 ±1 MB 级抖动；WorkingSet 受系统裁剪影响，故同时给 PrivateMemory。
7. `\Process(*)\Context Switches/sec` 在本机不可用（Process 计数器集仅 28 项），改用线程级 WMI 汇总；24768 与 24812 在窗口内有线程退出，其次/秒含偏差。
