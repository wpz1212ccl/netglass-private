# docs 索引

- `spec.md` — 首版交付 spec（历史基线）
- `technical-plan.md` — 首版技术方案（历史）
- `delivery-report.md` — 交付报告与位置锚点
- `pm-acceptance-rubric.md` — PM 验收 rubric
- `performance-review.md` — 2026-11-12 性能复核（取代旧两份审核）
- `perf-spec-2026-11-12.md` — 性能优化实施级 spec（v1.2，已实施，含决策 D1–D6 与三态复测）
- `perf-2026-11-13-resource-audit.md` — 2026-11-13 常驻资源独立复核（只读实测，与 11-12 基线交叉核对，含未测项清单）
- `perf-spec-2026-11-13-native-render.md` — 原生渲染切换实施级 spec（D7 已批，Phase 0–5，目标单进程 WS < 40 MB）
- `../native/` — 原生渲染 crate（单进程 Win32 + D2D 软件渲染 + DWrite，Tauri 版回退路径保留不动）。模块：`mode/anim/morph/number`（状态机与弹簧）、`render/card/fx/island`（D2D 绘制）、`font`（Inter 嵌入，DWrite3 FontSetBuilder）、`netmon/probe`（采样与探针）、`tray/menu/hotkey/autostart/config/instance`（纯 Win32 系统集成）、`window`（layered window/ULW）、`input`（双击/拖动判定）。assets：Inter 5 静态实例 ttf + 托盘 PNG。诊断参数：`--bench/--selftest/--hit/--pixels/--fonts/--mode/--island-pos/--dot/--icon/--tray-menu/--autostart-selftest/--config/--dpi-test/--morph-demo/--show-after/--exit-after/--silent`（仅显式传入生效）。
- `perf-check.ps1` — 常驻性能测量脚本（整树口径）
- `perf-2026-11-12/` — 测量 CSV 原件（含基线与实施后三态终测）
- `research/dynamic-island-ui-motion.md` — 灵动岛动效一手调研
- `adr/` — 决策记录 0001–0005
- `agents/` — 协作纪律（issue tracker / triage labels / domain）