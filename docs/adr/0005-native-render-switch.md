# 切换原生渲染，WebView2 退出常驻

用户目标升级为常驻内存两位数（2026-11-13，D7 批准）。独立复核已证明 WebView2 的 browser+gpu+renderer 固定三件套占内存 82.8%（176.0 MB），页面内容增量仅 ≈1.1 MB——WebView2 架构内无论怎么优化都动不了 ~200 MB 地板，因此重开 2026-11-12 14:40 的"不切 native"决策：常驻渲染切换为单进程 Win32 layered window + Direct2D（软件渲染，避免 d3d11/UMD ~28 MB 进程）+ DirectWrite，合成交给系统 DWM。进程树 7 → 1。`native/` 原型（D2D 渲染层、脏检查、弹簧可直接移植的纯 Rust 后端）为起点；`anim.rs`/`probe.rs`/`netmon.rs`/ModeGate 连同 22 个单测随迁，托盘/自启/快捷键/单实例纯 Win32 零新增 crate；D6（托盘隐藏持久化）顺带落地。视觉与行为基线不变：CSS 数值作像素级对照表，rubric ≥8/10，260 ms 轮询与三态行为照旧。

**Consequences**：目标是 WS < 40 MB（stretch < 25 MB）、岛态 CPU < 0.3%、进程数 = 1。已否决方案清单（ADR 0004）继续有效；旧否决"固定大画布"的理由（常驻合成成本）不适用于 layered window，但形变仍选窗口跟随 + UpdateLayeredWindow 逐帧改尺寸（中间帧只重绘纯形状，成本更低）。软件渲染为主路径的前提是 Phase 0 bench 实测 paint < 5 ms/帧，超阈值回 D9 重议。`src-tauri/` 保持不动直至 Phase 5 验收全绿后打 tag `webview-final` 再切换部署，可整体回退。实施细节见 `docs/perf-spec-2026-11-13-native-render.md`。
