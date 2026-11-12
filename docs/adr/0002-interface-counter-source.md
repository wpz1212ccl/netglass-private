# 速度只从 GetIfEntry2 读

拨号连接不是 NDIS 适配器 —— `Get-NetAdapterStatistics` 的结果里根本没有「宽带连接」这一行，所以常见的做法（按网卡名轮询 CIM）在这台机器上直接失效。实测 `GetIfEntry2` 对拨号接口（本机索引 47）返回实时、64 位、独立的 InOctets/OutOctets，单次 80 µs；`Get-NetAdapterStatistics` 单次 58 ms 且看不到拨号；底层以太网计数器比拨号高 0.56%（下行）/31%（上行），因为它同时承载封装开销与其他流量。因此所有速度统一由 `GetIfEntry2` 提供。

**Consequences**：启动时按 `IfType == 23 && ConnectionType == DialUp` 自动发现拨号接口，不写死索引（重播后索引会变）。计数器可能因重播或睡眠唤醒回退，必须按"重置"处理而不是算成负速度。Wi-Fi 的信号与连接状态不能轮询（`WlanQueryInterface` 单次 11.7 ms），必须用 `WlanRegisterNotification` 事件驱动。Windows 的 `MIB_IF_ROW2` 布局与文档存在差异，不要在 Rust 里手写偏移量，用 `windows` crate 的生成结构体。
