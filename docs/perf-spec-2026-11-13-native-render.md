# NetGlass 原生渲染切换规格（2026-11-13 · 实施级）

> 状态：**v1.0 待实施**。D7 已由用户批准（2026-11-13，"内存两位数 + UI 保持不变"），重开 perf-spec-2026-11-12 §5 的"不切 native（14:40 决策）"。决策记录见 `docs/adr/0005`。
> 前置阅读：`perf-2026-11-13-resource-audit.md`（内存下界证据）、`perf-spec-2026-11-12.md`（上一轮全部决策）、`docs/adr/0004`。
> 本文行号以当前工作区（分支 `全面性能优化`）为准。

## 0. 目标与硬约束

- **目标**：常驻内存降到两位数——单进程 WS **< 40 MB**（stretch < 25 MB）；岛态 CPU **< 0.3% 单核**、展开 **< 2%**；无持续合成器出帧（功耗受益）。
- **视觉基线不变**：以当前 WebView2 版为像素级对照，每个 Phase 过 `pm-acceptance-rubric.md` **≥ 8/10**。CSS 中的每个数值（渐变 stop、字号字重、间距、动效曲线时长）都是移植对照表，见 §3 各项。
- **行为基线不变**：本地状态 ≤ 2–3 s 反映、260 ms 前台轮询（D3 维持未批准）、隐藏时采样连续、DC 静态呼吸点 + DC 停外网探针、双击/拖动/躲让/Alt+Tab 行为照旧。
- **口径纪律沿用** perf-spec §6：单核 % / 整机 % 必标禁混用；中位 + p90；`%TEMP%\netglass-log.txt` 行数判污染。

## 1. 为什么是原生（数据链，非估算）

| 证据 | 来源 |
|---|---|
| 内存 82.8%（176.0 MB）在 browser+gpu+renderer 固定三件套，页面内容增量仅 ≈1.1 MB（噪声内） | audit §1.7 / §4 |
| 同一页面仅加呼吸灯，内存 +1.13 MB、CPU gpu ×20.9 / renderer ×94 | audit §1.7 |
| 隐藏态 TrySuspend 89 MB 只在不可见时成立；`MemoryUsageTargetLevel`/`--single-process` 已否决 | ADR 0004 |
| → WebView2 内任何优化都动不了 ~200 MB 地板；两位数必须去掉常驻 WebView2 | 本轮 D7 |

## 2. 架构

**单进程 Win32 layered window + Direct2D（软件渲染）+ UpdateLayeredWindow + DirectWrite。** 合成交给系统 DWM（本就在运行，参照 dwm 0.59% 单核），进程树从 7 个变 **1 个**。

模块划分（`native/` 目录内演进，`src-tauri/` 保持不动直至 Phase 5 切换）：

| 模块 | 来源 | 说明 |
|---|---|---|
| `window.rs` | 已有 | layered window、per-pixel-alpha DIB、`present()`；**ULW 的 pptDst+psize 可逐帧改位置与尺寸**——形变不走 SetWindowPos，无 WebView resize 阻塞问题 |
| `render.rs` | 已有（扩展） | Frame→paint；卡片视觉参数已对齐 CSS（玻璃渐变 stop、ink 三级色、曲线渐变）。扩展：胶囊 Frame、呼吸灯、hover 详情态、squircle 路径几何 |
| `anim.rs` | **从 src-tauri/src/anim.rs 原样移植** | SetWindowPos 弹簧是纯 Rust，零 WebView 依赖；移植后 apply 改为"渲染形状→ULW present" |
| `netmon.rs` | **用 src-tauri 版替换 native 版** | src-tauri 版含 read_row 差分缓存、60 s/5 s 缓存、隐藏态跳过载荷构建；native 版是旧拷贝 |
| `probe.rs` | **从 src-tauri 原样移植** | 纯 Rust（windows crate），含加速器 2 s 廉价路径、hosts mtime 门控、DC 停外网探针 |
| `mode.rs` | 从 src-tauri/src/main.rs 提取 | ModeGate / FgKind / pick_anchor / 三态切换 + **22 个单测随迁** |
| `tray.rs` | 新增（纯 Win32） | Shell_NotifyIconW + CreatePopupMenu；不引入 tray-icon crate（免 muda/crossbeam 依赖） |
| `main.rs` | 重写 | 窗口创建、消息循环、线程编排、`--silent`、单实例（CreateMutexW） |

集成件全部纯 Win32，零新增三方 crate：自启 = HKCU `...\CurrentVersion\Run`（RegSetValueW）、快捷键 = RegisterHotKey/WM_HOTKEY、单实例 = CreateMutexW、托盘图标 = 嵌入 PNG → CreateIcon。

## 3. 实施项（分 Phase，每项含验证）

### Phase 0 · 原型实测（不改产品代码）

| # | 项 | 验证 |
|---|---|---|
| P0-1 | `cargo run --release -- --bench`：记录 startup/after-DIB/after-D2D/after-1st-paint WS 与 paint ms/frame | 数字填入 §6 基线表；paint > 5 ms/帧（卡片全量）则软件渲染假设需重审 |
| P0-2 | `--selftest`：alpha 通道审计（圆角外必须全 0） | outside pixels = 0 |
| P0-3 | 跑 `native`（卡片态）按 §6 协议测 30×10 s CPU/内存 | 与 bench 数字互证 |

### Phase 1 · 代码整合（零视觉，`cargo test` 先行）

| # | 项 | 验证 |
|---|---|---|
| P1-1 | 移植 `anim.rs` / `probe.rs`、替换 `netmon.rs`、提取 `mode.rs`（含 22 测） | `cargo test` 全绿（原 22 + native 7 不丢） |
| P1-2 | 接线：mode 线程 260 ms 轮询 → hide/island/expanded → 渲染分支 | 单测 + 手动三态切换 |
| P1-3 | `--silent`（自启动不显窗，桌面稳定 2 s 后展开） | 冷启动路径日志 |

### Phase 2 · 胶囊态（岛）复刻【视觉验收：胶囊 rubric】

| # | 项 | 对照（src/index.html） |
|---|---|---|
| P2-1 | 144×34 物理窗（DPR 1.5 → 96×22.67 DIP），21 px 圆角胶囊玻璃 | `.card.collapsed`（:164-174） |
| P2-2 | 呼吸灯：66 ms 正弦表（2 s 周期，opacity 1−0.45s、scale 1−0.2s），DC 静态点、隐藏/形变/展开暂停，三态色绿 #34C759 / 橙 #FF9F0A / 红 #FF3B30，色变 0.4 s 插值过渡 | `breatheTick`（:447-465）、`.mdot`（:182-187） |
| P2-3 | 迷你数字：15 px/600 + 10 px 单位 + 10 px 上行，tabular-nums，1 Hz 差分更新 | `#mDown/#mUnit/#mUp`（:188-191） |
| P2-4 | hover 弹簧放大 162×42（HOVER_MOTION 0.30/0.55），active 微暗 | `island_hover`、`.hot` |
| P2-5 | 锚点躲让：pick_anchor + DODGE_MOTION（0.34/0.92）平移；全屏 App 隐藏 | mode.rs 随迁 |
| P2-6 | 隐藏：SW_HIDE + 停 emit；**D6 顺带落地**——托盘隐藏持久化，全屏路过不重显 | D6（上轮批准下轮实施） |

验证：岛态 30×10 s **< 0.3% 单核**；呼吸观感 rubric ≥ 8/10；三态色闭环（开/停加速器 ≤ 3 s 变色）。

### Phase 3 · 展开卡片复刻【视觉验收：卡片 + 形变 rubric】

| # | 项 | 对照 |
|---|---|---|
| P3-1 | 35 px squircle：D2D 路径几何（连续曲率，超椭圆近似），缓存几何；形变中间帧可用普通圆角矩形降级（中间帧本就无文字、300 ms 级） | `corner-shape:squircle`（:33） |
| P3-2 | 玻璃层：172° 主渐变 + 双 radial 高光 + 内发丝 0.5 px 双描边 + 双层投影；投影用离屏模糊位图缓存（一次性光栅） | `.card` + `::before/::after`（:34-62） |
| P3-3 | 文本：**Inter ttf 嵌入**（woff2→ttf 转换）+ DWrite 自定义字体 collection；正文回退链不变；等宽数字 font-feature tabular-nums | @font-face（:7-11）、.num（:86-87） |
| P3-4 | hover 详情：5 格错峰进场（0.04 s 步进）+ 底栏/曲线交叉淡出 + 内容上浮 1 px——插值时间轴复刻 CSS 的 cubic-bezier(.6,0,.25,1) | `.details/.cell`（:114-140） |
| P3-5 | 曲线：D2D path geometry，60 点平滑 + 纵轴下限 64 KB/s + 渐变描边/填充；1 Hz 差分重建（复用 content_key 思路） | `build()`（:347-368） |
| P3-6 | 形变：anim.rs 弹簧驱动逐帧"纯形状重绘→ULW(psize=当前矩形)"；内容退场/进场时间轴对齐（250/360 ms 延迟、.out 220 ms 淡出） | `apply_mode`（main.rs:429-481）、`.morphing` |
| P3-7 | 数字弹簧：前端 Spring(k=175,c=21) 移植为原生插值，30 fps 上限、1% 阈值起摆 | `frame()/kick()`（:399-431） |

验证：展开 30×10 s **< 2% 单核**；形变/展开/胶囊三态 rubric 各 ≥ 8/10；双击、拖动、躲让、Alt+Tab、恢复首帧行为回归。

### Phase 4 · 系统集成

| # | 项 | 验证 |
|---|---|---|
| P4-1 | 托盘：左键单击显/隐、菜单（显示/隐藏、锁定/解锁+快捷键名、开机自启√、退出）；PNG 图标 | 菜单全功能手动回归 |
| P4-2 | Ctrl+Alt+L（候选序同现版）RegisterHotKey | 锁定/解锁 + 状态行 |
| P4-3 | 自启 HKCU Run 键 + `--silent`；config.json 读写（x/y/locked/autostart）与现版兼容 | 位置/锁定跨重启保持 |
| P4-4 | 单实例 CreateMutexW；第二启动唤起既有窗 | 双击 exe 不出第二实例 |
| P4-5 | WM_DPICHANGED：跨 DPI 屏移动重建 surface/字号；多显示器主屏口径限制沿用并记录 | 单屏 150% 验证即可 |

### Phase 5 · 验收与切换部署

| # | 项 | 验证 |
|---|---|---|
| P5-1 | 全量验收（§6 表逐行）+ 24 h 泄漏观察 | WS+Private 双口径 < 10 MB 增长 |
| P5-2 | 打 tag `webview-final`；`dist\NetGlass.exe` 替换为原生产物；自启动路径/参数不变 | 重启后行为一致 |
| P5-3 | ADR 0005 状态更新 + sessionlog + 本 spec 标记已实施 | — |

## 4. 明确不动

- 视觉数值（见 §3 对照表）；260 ms 轮询；2 s 桌面 settle；8 s pin；呼吸 2 s 周期 15 fps；DC 行为。
- `src-tauri/` 与 `src/` 代码在 Phase 1–4 期间零改动（并行可回退）；已否决方案清单（ADR 0004）全部继续有效。
- 旧否决"固定大画布 + CSS 画胶囊"在 WebView 语境的理由（常驻合成成本）**不适用**于原生 layered window（无逐帧合成器成本，per-pixel alpha 免费命中测试）；但本 spec 仍选**窗口跟随可见矩形 + ULW 尺寸参数**，因为形变期间只重绘纯形状，成本更低且与现版行为一致。

## 5. 决策点（D7 已批；其余点头才实施）

| # | 取舍 | 推荐 | 不批准的代价 |
|---|---|---|---|
| D7 | 重开"不切 native"，切原生渲染 | **已批准（2026-11-13）** | 内存锁死 ~200 MB |
| D8 | 形变走"窗口跟随 + ULW 逐帧改尺寸"（非固定大画布） | 批准（随 D7 默认） | 固定大画布需覆盖卡片/胶囊/躲让并集，命中与躲让逻辑复杂化 |
| D9 | D2D **软件渲染**为主路径（省 d3d11/UMD ~28 MB），`NETGLASS_RT=hw` 留作 A/B | 批准（Phase 0 数据若 paint < 5 ms/帧） | 硬件路径多 ~28 MB，两位数目标风险 |
| D10 | 托盘/自启/快捷键/单实例纯 Win32，零新增 crate | 批准（随 D7 默认） | tray-icon crate 引入 muda 等依赖 |
| D11 | Inter 字体 ttf 嵌入（换掉原型的 Segoe UI Variable） | 批准（rubric U2 要求字体一致） | 字观感差异 → rubric 扣分 |
| D12 | D6（托盘隐藏持久化）随原生版直接实现 | 批准（上轮已批） | 隐藏测量继续被路过行为污染 |

## 6. 验收协议

口径与判据同 perf-spec §6（中位 + p90、log 污染检测、单核 % 标注）。阈值按原生版重设：

| 场景（60 s×3） | 主指标 | 阈值 |
|---|---|---|
| 岛（可见） | CPU、逐项归因（仅 1 进程） | **< 0.3% 单核**（stretch < 0.2%） |
| 展开 | CPU | **< 2% 单核** |
| 隐藏（含 D6 持久化） | CPU | **< 0.3% 单核** |
| 内存 | WS + Private 双口径 | **WS < 40 MB / Private < 25 MB**；10 min / 24 h 增长 < 10 MB |
| 进程数 | — | **= 1**（无 msedgewebview2 子进程、无 crashpad） |
| 启动 | 进程启动→卡片可见 | < 1.5 s 无白闪（无 WebView 冷启动，预期 ~0.3 s） |
| 视觉 | rubric | 胶囊 / 展开 / 形变三态各 ≥ 8/10，附截图对照 |
| 行为 | — | `cargo test` 全绿；三态色闭环 ≤ 3 s；Alt+Tab/任务栏/双击/拖动/躲让/隐藏→恢复首帧 |

Phase 0 基线表（2026-11-13 实测回填，release 构建，屏 2560×1600@150% DPR 1.5，16 逻辑核）：

| 项 | 值 |
|---|---|
| startup WS | 7.6 MB |
| after DIB / after D2D / after 1st paint | 8.3 / 16.2 / 22.7 MB |
| paint ms/frame（卡片全量 / 胶囊 / 纯形状） | **0.980** / 待测（T3） / 待测（T5） |
| paint 分解（title / 数字 44px / 曲线几何） | 0.001 / 0.000 / 0.004 ms |
| 120 次全量重绘后 WS | 23.9 MB（较首帧 +1.2 MB） |
| `--selftest` 圆角外 alpha | d≥1.02 像素 0 个、max_alpha 0（d>1.0 的 564 个全为 AA 边带） |
| 原型常驻（**卡片态**）30×10 s 单核 % | **0.485**（总增量法 1.4688 s / 303 s）；整机 0.030% |
| 原型常驻 WS / Private 中位 | 27.46 / 11.36 MB（p90 27.64 / 12.32） |
| 岛态 30×10 s 单核 % | 待测（T3） |

**D9 判定：成立。** 卡片全量 paint 0.980 ms/帧 ≪ 5 ms/帧门槛，软件渲染（D2D）路线维持主路径，`NETGLASS_RT=hw` 仅作 A/B。

> 口径注记：逐点 CPU 值呈 0 / 0.5–1.8% 交替，是 Windows 进程 CPU 时间的 15.625 ms 时钟粒度所致（10.4 s 采样窗内仅 0–4 个 tick，单 tick ≈ 0.15% 单核）；**权威值取总增量法**，逐点中位/p90 仅作离散度参考。原型无呼吸灯、卡片态 1 Hz 重绘，非岛态目标场景。

## 7. 实施顺序与回滚

1. Phase 0 → 数据回填 §6，paint 成本超阈值即回到 D9 重议。
2. Phase 1–4 每项独立 commit（`native:` 前缀），可单独 revert；期间交付版仍是 WebView2 exe，用户侧零风险。
3. Phase 5 切换前打 tag `webview-final`；切换后任何不达标 → 整体回退 tag 重新部署，原生版继续在分支迭代。
4. 测量脚本：`docs/perf-check.ps1` 的 native 阈值（A2 < 0.5%、A3 < 60 MB）自本 spec 起恢复为回归信号。

## 8. 附录

- 证据链：`perf-2026-11-13-resource-audit.md` §1.7/§3.2/§4；`performance-review.md` §5（"私用内存降不到 100 MB 以下"为切换动机）。
- 原型工具：`native --bench / --selftest / --hit / --pixels`（window.rs/main.rs 内置诊断）。
- 原型已对齐的视觉参数：玻璃渐变 stop、ink 三级色（.92/.64/.50）、曲线渐变（0.35→1.0→#0066D6）、字号字重表（render.rs:96-104）。
- 决策记录：`docs/adr/0005-native-render-switch.md`。
