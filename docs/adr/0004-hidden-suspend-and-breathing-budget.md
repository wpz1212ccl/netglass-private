# 隐藏即挂起 WebView2，呼吸灯给帧预算

岛态稳态成本 95% 在 WebView2 GPU+renderer：120Hz 屏上透明置顶窗的无限呼吸动画让合成器永久逐帧出图（实测 21–23% 单核）。隐藏时 Tauri `hide()` 只做 `SW_HIDE`，controller 的 `IsVisible` 恒 TRUE，渲染管线继续空转。因此：隐藏路径 `put_IsVisible(false)` + `ICoreWebView2_3::TrySuspend`，显示路径先 `Resume` 再 show；呼吸灯改 JS 驱动 ≤15fps、DC 静态点、展开/隐藏/形变暂停；`.mdot` 加 `will-change` 独立合成层。实测隐藏 0.3–0.5% 单核、WS 344→89MB；岛中位 5.2%（−75%）。

**Consequences**：TrySuspend 收益主显现在 working set，验收必须 WS+私用双口径；其前提是 `IsVisible=false`，变可见自动 resume。`MemoryUsageTargetLevel` 否决（换出到磁盘、1Hz 脚本立刻换回，电池场景反向优化）；`--single-process` 否决（沙箱消失、无独立渲染进程可回收，TrySuspend 失效）；apply 节流 60Hz 否决（120Hz 屏手感优先）；固定大画布否决（常驻成本换瞬时成本）。岛态剩余 ~5% 是已批准的 15fps 呼吸合成成本；隐藏剩余 0.3–0.5% 是 Rust 260ms 轮询地板（D3 未批准）。托盘隐藏被全屏路过覆盖为既有行为，记 D6 待修。决策与数据见 `docs/perf-spec-2026-11-12.md`。