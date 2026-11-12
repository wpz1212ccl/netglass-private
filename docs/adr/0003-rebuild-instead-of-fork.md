# 不复用 TrafficMonitor，从零构建

46.9k★ 的 TrafficMonitor 是功能最接近的现成项目（可选监控连接、鼠标穿透、开机自启、背景透明），但它是 MFC/DirectComposition 的 C++、Anti-996 许可、界面停留在 2010 年代、完全没有代理感知，且存在网卡列表刷新的历史缺陷。其余候选都不对路：NetSpeedTray 是任务栏插件而非悬浮窗，Clash-Float 只有约一千行、仅覆盖代理一半，neohtop 是 CPU/内存面板。没有任何项目同时具备"逐接口 + PPPoE + 代理探针 + 亚克力 + 桌面级窗口"。因此从零用 Tauri 2 构建，把 Clash-Float 仅作为读取 mihomo API 的思路参考。

**Consequences**：需要自己实现窗口层、计数采集、探针与自启四件事；换来的是许可干净、界面可控、且不必在 C++ 里逆向一套陌生的渲染管线。
