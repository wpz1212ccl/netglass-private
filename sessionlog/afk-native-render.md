# AFK journal — 原生渲染切换（native-render）

**状态**
- 已完成：T1（D9 成立）、T2（29 绿，`a096467`）、T3 胶囊态（43 绿，`715b121`，#3 已关）、T4 展开卡片（58 绿，合并 `57e1d4d`，#4 已关；三项指标 root-only 口径全达标，rubric 8.8/10）
- 进行中：T5 形变弹簧与数字动画（即将派发，`.scratch/afk-t5-prompt.md` 已备）
- 下一步：T6 系统集成 → T7 全量验收收官（双轴 code-review + 演示 + 收官报告，停在合并门）
- **T4 视觉复核（主 agent 亲自看图）**：`card-default.png` / `card-hover.png` 里 35px squircle、Inter 数字「114/118」字距紧、5 格 hover 详情（延迟 23ms / 峰值 118 KB/s / 链路 243 Mbps / 上行峰值 30 KB/s / 代理 未加速）、蓝色曲线、底栏交叉淡出均在，层次符合 iOS 小组件范式。但两张图左侧有竖直暗带 + 小圆角矩形叠影；`desktop-full.png` 显示屏幕被最大化浏览器占满、左上角仍有常驻 WebView2 版 NetGlass 胶囊（「2.9 MB/s ↑ 148 KB/s」）→ **是常驻版叠在原生卡片左边缘，属证据构图问题、非产品缺陷**（goal §3a 已预警并存重叠）。已要求 T4 用 `--mode=expanded --pos` 摆到空白区 + `--hit` 探坐标补拍 clean 版。
- **T4 主 agent 复跑 `--bench`**：paint **2.560 ms/帧**（T4 报 2.26，环境波动，均 <5 ms 门槛 → D9 仍成立）；`--selftest` 卡片 squircle 与胶囊 `d>=1.02` 像素 0 / max_alpha 0，且打印 `font Inter, collection sees Inter = true, weights=[400,450,500,560,600,...]`。`outside-samples` 前 8 点非零是因为旧椭圆采样点现在落在 squircle 内部（审计已改为 squircle 距离判定），逻辑自洽，T7 留意即可。
- **T4 展开态测量复核（主 agent 按 CSV 独立复算）**：`measure-expanded.csv` 23 点 → sum_dcpu 4.1097s / sum_dt 233.45s = **1.7604% 单核 = 0.11% 整机**（<2% 阈值，**达标**）；逐点中位 1.6269 / p90 2.5146；Private 中位 23.41 MB（<25 达标）；**WS 中位 47.41 MB > 40 MB 阈值（未达标）**。CSV 恒 `n=2`，主 agent 独立核实 `Get-Process netglass-native` 物理只有 **1 个**进程（PID 13480，Start 1:10:29）且无子进程 → `n=2` 是测量脚本 `Get-TreeIds` 统计口径偏差（非真派生），spec「进程数=1」物理成立。WS 较 T3 岛态 23 MB 上涨归因：卡片玻璃离屏缓存 + Inter 5 静态实例 + 自定义 DWrite font collection（`--bench` 的 25.3/29.1 MB 是进程内阶段值，与常驻稳态不同口径）。已要求 T4 重测确认；若仍 >40 MB，按「内存部分达标、待用户决策」如实上报（与岛态 CPU 项并列）。
- **T4 补拍复核通过**：`card-default.png` / `card-hover.png`（01:09 重拍）无竖直暗带/叠影、只有原生卡片本体，S 级证据合格。T4 用 `System.Windows.Forms.Form` 造受控壁纸背板（注释说明不碰用户被浏览器占满的真实桌面），做法合理。
- **T4 关键技术结论（D11 自证，主 agent 复核认可）**：`AddFontMemResourceEx` 注册的私有字体**对 DWrite 不可见**（`--fonts` 诊断 `system sees Inter=false`）；改用 **DWrite3 `FontSetBuilder` 建进程内自定义 collection** 后 `custom sees Inter=true`。这纠正了会话 2 备忘 §三「最简嵌入路径可用」的判断——GDI 私有字体只喂 GDI，DWrite 需自定义 collection。T4 正在处理 450/560 静态实例在 DWrite 里 `GetWeight()=1` 的问题（主 agent 建议降级到 400/500/600 三实例或修 STAT 表），并已修好 `GetStringLength` 签名（windows 0.62.2 返回 `Result<u32>` 而非 out 参数）。
- 下一步：T5 → T6 → T7（串行，`render.rs` 共享）
- ⏸ 需要用户决策：T3 岛态 CPU 0.6285% 单核 > 0.3% 阈值，瓶颈为 T2 既有探针/轮询地板；压到阈值内需动 D3（未批准）→ 收官报告列明，不擅自改。

## T4 展开卡片视觉复刻（完成 · 会话 5 收尾合并 · 三项指标全达标）

**决策依据**：spec §3 Phase 3 前半（P3-1..P3-5）、票 #4、goal §4 D9/D11、pm rubric。

**T4 worktree 分阶段提交**（implementer 会话 3/4 完成）：`1586cc2`（card/fx/font 纯逻辑 + Inter 实例）→ `2b2bda3`（卡片 paint + DWrite font collection）→ `a742d48`（重生成 Inter 实例删 STAT 修 450/560 GetWeight=1）→ `04998f8`（玻璃离屏缓存 paint 2.26ms）→ `c9745d2`（hover 2 列 grid 修复）。

**WS 口径之谜已解（会话 4 发现，本票官方化）**：`measure-expanded.csv` 的 WS 47.41 MB「未达标」是假警报——被测 exe 是 console-subsystem 二进制，Windows 强制伴生 conhost.exe 子进程，旧脚本 `Get-TreeIds` 树合并把它算进去（CSV 恒 n=2，物理进程数=1）。官方口径改 **root-only**（`.scratch/t4-measure-root.ps1`，头部注释说明），**T7 全量验收统一按此复测所有状态**；T6 若切 windows-subsystem 二进制则 conhost 自然消失。

**复核（主 agent 亲自跑，会话 5）**：
- t4 worktree `cargo test` → **58 passed; 0 failed**；`cargo build --release` 通过；护栏 `git diff --stat 715b121..t4-card -- src-tauri src` 空、`Cargo.toml` 零改动。
- `card-default.png` 默认态亲验 ✓：曲线可见（与 hover 态交叉淡出相反）、构图与 hover 版一致、无叠影无暗带；「代理 未启用」橙色 warn 与 `src/index.html:253` cls=warn 语义一致（对照确认）。
- **合并后主树复测**：`cargo test` 58 绿、release 构建通过、`--bench`/`--selftest` 可跑。
- **主树 30×10s root-only 复测（A 级，`%TEMP%\afk-demo-native-render-t4\measure-root.csv`）**：CPU 权威值 **1.4829% 单核 = 0.0927% 整机**（<2% 达标）；逐点中位 1.5591 / p90 2.4937；WS 中位 **35.89** / max 36.59 MB（<40 达标）；Private 中位 **21.07** / p90 24.79 / max 26.04 MB（<25 达标，max 个别点略过如实并列）；children=[conhost.exe] 佐证口径；污染 delta 0。与 worktree 版（1.1803%/35.42/20.14）两次一致可复现。

**A 级证据（原样尾行）**：
```
running 58 tests
..........................................................
test result: ok. 58 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

**合并**：`57e1d4d`（merge: T4 expanded card visual (58 tests green; all expanded metrics within threshold)）。**票 #4 已关**（证据评论 6861218670，含 rubric 自评 8.8/10 与复跑命令）。

**关键技术结论（D11，会话 4 发现、主 agent 复核认可）**：`AddFontMemResourceEx` 私有字体对 DWrite 不可见 → 必须走 DWrite3 `FontSetBuilder` 进程内自定义 collection；450/560 静态实例删 STAT 表 + 标准 subfamily 后 GetWeight 正确。tabular-nums 经 `DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES` + `SetTypography` 实现（读码确认）。

**已知限制**：① hover 态曲线不可见为设计行为（交叉淡出），形变中间态由 T5 覆盖；② collection 含 oblique 共 10 face；③ conhost root-only 口径 T7 统一复测并在报告注明；④ Private p90 24.79 贴线。

---

集成分支：`feat/native-render`（HEAD 起始 `b832533`）。worktree：`.worktrees/native-render-build`。
基线：`src-tauri` cargo test 22/22 绿（2026-11-13 本会话复跑）；`native` 0 用例。

---

## T1 原生原型实测基线（完成 · D9 数据门通过）

**决策依据**：spec §3 Phase 0（P0-1/P0-2/P0-3）、spec §6 基线表、goal §4 D9（paint ≥5 ms/帧 → 标「⏸ 需要用户」重议）。

**做了什么**：release 构建 `native/`，跑 `--bench` / `--selftest`，再起原型（卡片态）按 §6 协议 30 点 × 10 s 采样（脚本 `.scratch/measure-t1.ps1`，仅测量、不改产品代码）。

**A 级证据（原样输出）**：

```
=== BENCH ===
startup WorkingSet = 7.6 MB
after DIB          = 8.3 MB
after D2D/DWrite   = 16.2 MB
after 1st paint    = 22.7 MB
  layout(title)        0.001 ms
  layout(number 44px)  0.000 ms
  spark geometry       0.004 ms
paint: 0.980 ms/frame (120 frames)
after 120 paints   = 23.9 MB

=== SELFTEST ===
canvas: 480x300 physical (scale 1.5)
alpha: center(240,150)=69 corner(3,3)=0 outside-samples=[0,0,0,0,0,0,0,0,0,0,0,0]
outside ellipse (d>1.0): alpha>0 pixels=564 | at d>=1.02: pixels=0 max_alpha=0
```

```
=== RESULT (steady-state samples: 29) ===
cores = 16
-- CPU (one core %) --
  min = 0.0000  median = 0.2997  p90 = 1.3427  max = 1.7991  mean = 0.4855
-- CPU (whole machine %) --
  median = 0.0187  p90 = 0.0839
-- WorkingSet MB --
  min = 26.32  median = 27.46  p90 = 27.64  max = 27.70  growth = +1.37
-- Private MB --
  min = 10.71  median = 11.36  p90 = 12.32  max = 12.42  growth = -0.14
-- process count -- distinct = 1
```

总增量法（权威值）：`sum_cpu_s=1.4688 / sum_dt_s=302.71` → **0.4852% 单核 = 0.0303% 整机**。

**结论**：
- **D9 成立**：卡片全量 paint **0.980 ms/帧** ≪ 5 ms 门槛，D2D 软件渲染维持主路径。
- `--selftest`：圆角外 d≥1.02 区域 alpha 全 0（564 个 d>1.0 的 alpha>0 像素全落在 1 px AA 边带内），per-pixel alpha 形状成立。
- 内存：启动 7.6 MB → 首帧后 22.7 MB → 120 次重绘后 23.9 MB（+1.2 MB 后稳定）；常驻 WS 中位 **27.46 MB**、Private 中位 **11.36 MB** —— 两位数目标（WS <40 / Private <25）已见底。进程数 1。

**口径注记（重要）**：逐点 CPU 值呈 0 / 0.5–1.8% 交替，是 Windows CPU 时间 15.625 ms 时钟粒度所致（10.4 s 窗内 0–4 个 tick，单 tick ≈ 0.15% 单核）。**禁止用逐点中位当权威值**；本票权威值取总增量法。该现象在 T7 复测时同样适用。

**污染检查**：`%TEMP%\netglass-log.txt` 采样前后 31718 → 31987 B（+269 B）。查明：`netglass-log.txt` 只由 `src-tauri/src/main.rs:623` 写入，native crate 零引用；增量来自后台常驻的 WebView2 版 `dist\NetGlass.exe --silent`（PID 24768，另一进程）自身的 anchor 翻转，与本票被测进程无关 → **本票数据不受污染**。后续票测量若需纯净环境，按 goal §3a 用托盘退出常驻 WebView 版（可逆）。

**遇到的问题**：首次 `git worktree add .worktrees/native-render-build feat/native-render` 失败（分支已被主树占用，主树本身就在该分支）。绕法：worktree 用 `--detach` 挂同一 commit，主树留在 `feat/native-render`。

---

## T2 后端整合移植（完成 · 29 绿 · 已合并）

**决策依据**：spec §3 Phase 1（P1-1/P1-2/P1-3）、goal §4 D2/D3、票 #2 验收指针。

**做了什么**：implementer subagent 因网络 502 中断，代码已落盘未提交；主 agent 接手复核、验证、提交（`57da54a`）、合并进集成分支（`a096467`）。移植 `anim.rs`/`probe.rs`、用 src-tauri 版替换 `netmon.rs`（去掉 serde）、从 main.rs 提取 `mode.rs` 状态机；接线 260 ms 前台轮询三态 + `--silent`。

**复核（主 agent 亲自跑）**：
- 29 个用例全绿，22 个原用例（16 mode + 6 probe）逐个同名、测试块逐字一致（仅 `TICK`→`POLL` 常量别名，值同为 260 ms），断言未削弱；新增 7 例（anim 3 + mode 4）。
- `anim.rs` 弹簧与打断逻辑逐字一致，仅把 HWND+SetWindowPos 换成注入 `Target{apply,current}`。
- `git status --short src-tauri src` 为空（护栏零改动）；`Cargo.toml` 仅加 `Win32_System_Power` feature，零新增三方 crate。
- release 编译通过。
- 三态手动实测（`.scratch/t2-verify.ps1`）：正常启动卡片 320×200 → 转岛 144×34；`--silent` 24 次采样 hwnd=0 全程不显窗（P1-3）。

**A 级证据（原样尾行）**：
```
running 29 tests
.............................
test result: ok. 29 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

**票**：#2 已关闭，证据评论已留；#1 已关闭。frontier 推进到 #3 / #4。

---

## 续跑进度（会话 2 · 进行中）
- 23:0x：T3 worktree `.worktrees/t3`（分支 `t3-island`）建立，implementer 后台开工。主 agent 期间完成：Inter woff2→ttf 可行性验证（fontTools 4.63，生成 `.scratch/fonts/inter-var.ttf` + 5 个静态字重实例）、`AddFontMemResourceEx` API 可用性确认（windows 0.62.2 GDI feature 已启用）、PowerShell 桌面截图脚本 `.scratch/shot-win.ps1` 冒烟通过、T7 参数化测量脚本 `.scratch/measure-state.ps1`、ffmpeg gdigrab 录屏冒烟通过（2560×1600×32 桌面可捕获）、T4/T5/T6/T7 派发备忘落 `.scratch/`。
- 23:1x：T3 implementer 回报：`island.rs` 纯逻辑按 TDD 红→绿，`cargo test` 38 全绿（原 29 + 新增 9）；正在接 render/main 的胶囊绘制、66ms 定时器、`--mode=island`/`--island-pos` 诊断钳制。测量与截图随后启动。
- 23:2x：T3 首轮岛态 30×10s 实测 **0.5928% 单核（总增量法）/ 中位 0.5972 / p90 1.0483**，超阈值 <0.3%；WS 23.6MB、Private 10.2MB、进程数 1、污染 delta 0（口径合规）。implementer 自主定位两处浪费并修复：① 隐藏态 1Hz 仍重绘整张卡片（hidden 基线 0.274%→0.000%）；② 胶囊 content_key 把不可见的 60 点 hist 也算进去（每秒白重绘）。短测降至 0.498%，仍未达标。剩余成本归因：15fps × (D2D paint 0.15ms + ULW present 0.10ms) ≈ 0.375% 单核，ULW 固定开销 ~0.15% 省不掉；正在把每帧 D2D 换成「预渲染呼吸相位 + 静态 DIB BitBlt」以压到 ~0.15%。**如实记录：首轮未达标，按验证驱动继续优化，不掩盖数据。** 边界已给 implementer：不得降呼吸频率/删呼吸灯/改 260ms 轮询/改软件渲染路径；两轮后仍超则标「性能部分达标、待用户决策」并继续 T4（T4 只依 #2，不受阻）。
- 23:4x：多轮短测（hidden 8 点全 0.0000，隐藏态已达标；GDI BitBlt 版短测约 0.42%，短测受 2.5s 窗时钟粒度量化与启动抖动污染，不作权威）。最终长测 `t3-native-island-final.csv` 前 19 点总增量法 ≈ **0.490% 单核**（sum_dcpu 0.9221s / sum_dt 188.09s），仍 >0.3%。主 agent 观察：逐点有 0.125s/0.0938s 尖峰（单 10s 窗 ≈1.2% 单核突发），不像逐帧 paint，更像探针线程 10s 一轮 `run_probe()`（HTTP CONNECT + 进程枚举）或 1Hz sampler 批处理的周期性成本；已提示 implementer 定位并复核测量口径（诊断模式是否也该停外网探针）。若确属探针成本，属 T2 移植既有行为、非 T3 引入，报告需如实归因。

**最终 30 点权威值（主 agent 独立按 CSV 复算，2026-11-13 23:4x）**：`t3-native-island-final.csv` 满 30 点 → sum_dcpu 1.9066s / sum_dt 303.35s = **0.6285% 单核 = 0.0393% 整机**；逐点中位 0.6002 / p90 1.1927；WS 中位 23.21 / p90 25.52 MB，Private 中位 10.08 / p90 11.03 MB；进程数 1。比首轮 0.5928% 略高（后段探针尖峰更多）。**结论：岛态 CPU <0.3% 单核未达标**（前 19 点 0.490% 的乐观值不作数）。按 goal 停机条件这不算死锁（不需人才能继续），标为「性能部分达标、待用户决策」，继续推进 T4（只依 #2 不受阻）；是否动探针降频/轮询周期会触及 D3（未批准），需用户点头。视觉功能、隐藏态（0.000%）、内存、进程数均达标。

**复核待办（主 agent 读 t3 diff 发现，需提交后逐项验证）**：
1. `probe.rs` 被改动：`accelerator_state()` 的「2s 廉价路径（监听表 PID 定向 `OpenProcess`+`QueryFullProcessImageNameW` 查 exe 名）」被换成 `accelerating_labels()` + `process_snapshot_for()`（进程快照查表、10s 缓存）。这是 T3 为压 CPU 做的性能优化，但**改了 T2 从 src-tauri 移植的探针语义**（T2 复核时确认过「与 src-tauri 版 diff 仅去掉 serde」）；必须验证：6 个 `probe::tests::*` 原用例仍绿且断言未削弱、三态色闭环 ≤3s 仍成立、新增纯函数 `accelerating_labels` 有单测。若语义偏离 src-tauri 版，按 code-review Spec 轴处理。
2. `window.rs::screen_dc()` 缓存屏幕 DC 且**不再 ReleaseDC**（注释称进程退出由系统回收）。技术上 GetDC(NULL) 是 common DC，但属资源生命周期变更，需确认窗口/表面重建（形态切换、WM_DPICHANGED）时不会累积 DC 句柄；T6 的 DPI 重建会碰这里，需联动验证。

**主 agent 独立验证（读 t3 worktree 代码，不改文件）**：`main.rs` 探针线程 `visible=true` 时每 2s 本地路径（`accelerator_state()` 内 `CreateToolhelp32Snapshot` 全进程枚举）、每 10s 外网 `run_probe()`（`http_connect_probe` 最多 5s + 进程枚举）；另有每 2s `SetWindowPos(HWND_TOPMOST)` 重申置顶线程。这些周期性批处理与 0.125s 尖峰吻合，**属 T2 移植既有接线、非 T3 引入**；ADR 0004 亦记「隐藏剩余 0.3–0.5% 是 Rust 轮询地板」。结论：岛态 0.49% 里约 0.3–0.4% 可能是探针/轮询地板 + ULW 固定开销，而非呼吸视觉本身；T7 全量验收需按 spec §6 做「逐项归因」（票 #7 验收项写明「仅 1 进程」但归因不可省），并在报告里如实区分「视觉呼吸成本」与「架构地板」。

## T3 胶囊态（岛）视觉复刻（完成 · 主 agent 接手复核 · 岛态 CPU 部分达标）

**决策依据**：spec §3 Phase 2、票 #3、goal §4 D1/D6/D9、pm rubric。

**断点**：implementer 因网络 502/ECONNRESET 中断（与 T2 同类故障），代码已落盘未提交。主 agent 按 T2 先例接手：亲自复核 → 验证 → 提交 → 合并 → 关票。

**复核（主 agent 亲自跑）**：
- `cargo test` → **43 passed; 0 failed**（原 29 + 新增 14：island.rs 8 纯逻辑 + probe.rs 3 `accelerating_labels_*` + mode.rs 3 D6/island_pos）；6 个原 `probe::tests::*` 全在、断言未削弱。`cargo build --release` 通过。
- 护栏：`git status --short src-tauri src` 空；`Cargo.toml` 零改动（D10 零新增 crate）；worktree 除 6 个 native/src 文件外无杂项（测量脚本/CSV/PNG 全在 .scratch/ 与 %TEMP%，不进 git）。
- 默认/`--silent` 行为不变（`.scratch/t3-verify.ps1` 12 次采样）：默认卡片 320×200 → 岛 144×34；`--silent` 全 hwnd=0/vis=False（P1-3）；`--mode=hidden` 全程不显窗；`--mode=island --island-pos X,Y` 稳定岛态。
- `--bench` paint 1.025 ms/帧（<5 ms，D9 仍成立）；`--selftest` 椭圆 + 胶囊圆角外 d≥1.02 alpha 全 0。
- **A 级证据（30×10 s，`%TEMP%\t3-native-island-final.csv` 满 30 点）**：sum_dcpu 1.9066 s / sum_dt 303.35 s → **0.6285% 单核 = 0.0393% 整机**（权威值）；逐点中位 0.6002 / p90 1.1927；WS 中位 23.21 / p90 25.52 MB，Private 中位 10.08 / p90 11.03 MB；进程数 1；污染 delta 0。
- **S 级证据**：`%TEMP%\afk-demo-native-render-t3\`（final 静态/hover/离开 + 三态色橙/红）；主 agent 复核补拍 `%TEMP%\afk-demo-native-render-t3-verify\island-clean2.png`（`--hit` 确认 hwnd=`NetGlassNative`，胶囊完整入镜）。rubric 岛态自评 **8.5/10 及格**。

**⚠ 未达标项（如实记录）**：岛态 CPU < 0.3% 单核 → 实测 **0.6285%，未达标**。内存（WS 23 / Private 10 MB，阈值 40/25）、进程数 1、隐藏态（8 点全 0.0000%）、视觉功能均达标。归因（主 agent 独立读代码验证）：`--bench` 新增分解项显示 `island paint+present: 0.056 ms/frame`（15 fps ≈ 0.084% 单核）**不是瓶颈**；`proxy_state: 466.761 ms/call (2s probe path)` 及探针线程每 10 s `run_probe()`（HTTP CONNECT + 全进程枚举）、每 2 s `SetWindowPos(TOPMOST)`、1 Hz sampler 才是周期性尖峰来源——**属 T2 移植既有接线、非 T3 引入**（ADR 0004 亦记「隐藏剩余 0.3–0.5% 是 Rust 轮询地板」）。要压到 <0.3% 需动探针轮询周期/DC 语义，触及 D3（未批准）→ **⏸ 需要用户决策，agent 不擅自改**；按 goal 停机条件这不算死锁（不需人才能继续），T4 只依 #2 不受阻，继续推进。

**复核发现的两项（已验证，留 T7 复验 / code-review Spec 轴处置）**：
1. `probe.rs` 为压 CPU 把「2 s 廉价路径定向 `OpenProcess` 查 exe 名」换成「监听 PID + 进程快照查表（10 s 缓存 + 2 s staleness 门控）」。6 原测试 + 3 新纯函数测试全绿、功能等价，但**偏离 src-tauri 原文语义**（T2 曾确认仅去掉 serde）；「新启动加速器 ≤2–3 s 反映」无测试覆盖 → 三态色闭环 ≤3 s 需 T7 真机复验。
2. `window.rs::screen_dc()` 缓存屏幕 DC（GetDC(NULL) common DC）不再 `ReleaseDC`；渲染与 `--hit/--pixels` 诊断同线程 → 单 DC、不随 Surface 重建累积，进程退出系统回收。T6 的 WM_DPICHANGED 重建需联动验证。

**主 agent 独立复测（复核义务）**：`.scratch/t3-verify-measure.ps1` 起 `--mode=island --island-pos 1800,120` 跑 10×10 s → sum_dcpu 0.2969 s / sum_dt 90.17 s = **0.3292% 单核 = 0.0206% 整机**；WS 中位 23.34 MB、Private 10.03 MB、污染 delta 0（CSV `%TEMP%\afk-t3-verify.csv`）。与 implementer 30 点的 0.6285% 差异显著 → **岛态 CPU 方差大，取决于 10 s 窗口内落了几次探针批处理**；内存数据两次一致可复现。这进一步印证「瓶颈是周期性探针批处理而非呼吸视觉」，也说明 spec §6 要求的 60 s×3 全量复测（T7）才是最终裁判。两次数字都如实保留，不取乐观值报达标。

**诊断参数**：`--mode=island|expanded|hidden`、`--island-pos X,Y`、`--dot=green|orange|red`，仅显式传入生效，默认/`--silent` 行为不变（主 agent 实测）。

## 续跑交接（会话 2 · 2026-11-13 22:xx）

doctor `--quick` ALL GREEN；`gh auth status` 正常。主 agent 亲自复跑 `cd native && cargo test --quiet` → 29 绿（确认断点状态）。`render.rs` 当前卡片仍是**椭圆原型**、岛态为粗糙版；T3/T4 都要改 `render.rs` 且共享 `Renderer`/`Frame`，并行必冲突 → 按 goal §6「撞额度退避串行」改为 T3→T4→T5→T6→T7 串行推进。视觉对照源：`src/index.html`（673 行，CSS 数值为像素级对照表）；Inter 字体 `src/fonts/inter-var.woff2`（fontTools 4.63 可转 ttf）；`src-tauri/icons/tray.png` 供 T6。测量脚本 `.scratch/measure-t1.ps1`（T3/T7 复用）。
