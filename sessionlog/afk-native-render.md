# AFK journal — 原生渲染切换（native-render）

**状态**
- 已完成：T1 原型实测基线（D9 数据门 → 成立）
- 进行中：T2 后端整合移植
- 下一步：T3/T4 并行（同依 #2）→ T5 → T6 → T7

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
