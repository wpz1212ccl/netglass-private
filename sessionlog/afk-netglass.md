# AFK journal — NetGlass

**状态**
- 已完成：T1 窗口、T2 数据层、T3 探针、T4 系统集成
- 进行中：T5 PM 验收（第 2 次）
- 下一步：按 PM 意见返工 → 演示视频 → 收官报告

---

## T1 窗口 spike（完成）
- Tauri 2 + 静态 HTML；浅色 Mica。证据 `prototype/s1/shot.png`、`s1-log.txt`。
- 修：DWM 圆角偏好、WS_EX_TOOLWINDOW、首帧后 show。

## T2 数据层（完成）
- `GetIfTable2` / `GetIfEntry2`。证据 `prototype/s1/t5.png`、`%TEMP%\netglass-dump.txt`。
- 四个实测坑：
  1. 拨号 ConnectionType=3（MAXIMUM），只能按 IfType=23 判定。
  2. WFP/QoS 过滤层会被当成独立接口，用 bit1(FilterInterface) 剔除。
  3. 路由 Metric 不含接口度量，必须加 `GetIpInterfaceEntry().Metric`。
  4. Wi-Fi 连通性看 `MediaConnectState`，不能只看 OperStatus。

## T3 探针（完成）
- CONNECT 隧道时延（github.com:443）+ 直连时延（www.baidu.com:443）+ 端口/进程检测。
- 证据 `prototype/s1/t6.png`（延迟 26 ms）。

## T4 系统集成（完成）
- 单实例、自启（HKCU Run，指向 dist）、Ctrl+Alt+L 锁定、托盘四项、位置记忆、退出保存。
- 证据：`exstyle=0x40198 toolwindow=True`；第二次启动后进程数仍为 1。

## T5 PM 验收（完成）

六轮：7.4 → 6.7 → 7.7 → 7.1 → **8.1 及格**。

- 第 1 次取证失效（截图被 DPI 虚拟化裁成 22×22），中止；`card.ps1` 改为 DPI-aware。
- 快捷键失效根因：Ctrl+Alt+L 被系统占用（日志 `HotKey already registered`），改候选回退，实际 **Ctrl+Alt+N**。
- 修过的真问题：
  - hover 叠字 → 非对称时序（进入底栏先退，离开详情先退）。
  - 启动空板 → `font-display:block` 在字体就绪前不渲染文字；改 `swap` + 按进程启动时间对齐 1050ms 再 show。
  - 锁定态被每秒采样覆写 → 前端持有 locked，状态位常驻「已连接 · 已锁定」。
  - CPU → 接口表 60s 重建 + 每秒 GetIfEntry2；DNS/进程枚举 60s 缓存；置顶 60s。0.364%→0.104%。
  - 整树 CPU 30%→6% → 数字弹簧只在变化 >1% 时动，最长 300ms、30fps 上限；空闲每秒只改一次文字。
  - 四边深框 → show 后重设 exstyle + `DWMWA_BORDER_COLOR = COLOR_NONE`。
  - 曲线贴底 → 基线上抬 8px；小字对比 → 单位 60%、详情 64%。
- 材质结论：Win11 的 DWM 背景（DWMSBT_TRANSIENTWINDOW）按设计只采样壁纸；BLURBEHIND 会把卡片压成深灰（白底 RGB≈50）。浅色玻璃 = DWM 材质 + 低白覆盖（16%→6%）+ 噪点 0.07。
- 第 6 轮 8.1 及格。保留意见：整树内存 225MB 是 WebView2 平台成本；详情 10.5px 对比 3.3:1；曲线端头被圆角切 7px。

## T6 演示视频（完成）

- `NetGlass-demo.mp4`：54s / 1920×1200 / 4.2MB，Edge TTS 六段旁白（zh-CN-XiaoxiaoNeural）。
- 录制用 MPEG-TS + ffmpeg `-t 54` 自终止——强杀会丢尾部 5 秒。
- 抽帧确认：12s 下载 2.6MB/s；22s 悬停详情（13ms / 10.9MB/s）；38s 已锁定。

## T7 形态返工（2026-11-12 下午）

- 用户否决椭圆：要苹果"组件式圆角矩形"（iOS 小组件 / macOS 控制中心），椭圆方案不采纳。
- 根因（矩形底）：wry 的透明窗口用 DWM blur-behind，按**整个窗口矩形**采样，圆角/椭圆外会露出浅色矩形。
  - 解法：`SetWindowRgn` + `DwmEnableBlurBehindWindow(DWM_BB_BLURREGION)` 用**同一块圆角矩形区域**同时约束窗口形状与玻璃底；形状外不渲染、不拦截点击。
- 形态：展开 320×200、28pt 圆角玻璃卡片；灵动岛改为真苹果样式（黑胶囊 + 白字 + 绿点），顶部居中贴边（y=0）。
- 其它修复：`WS_EX_NOACTIVATE` 不抢焦点；watcher 在"焦点落在自己身上"时回退到上一个前台窗口；Island↔Expanded 先 hide 再 resize 再 show，消除旧画面拉伸残影；展开瞬间冻结 hover，等 pointermove 再恢复。
- 事故：夜间子代理为测透明度启动了全屏 WinForms 壁纸/白底窗体（标题 "NetGlass PoC Backdrop"），覆盖了用户桌面；已结束其进程与子代理，脚本改名为 `*.disabled`，PoC 二进制已挪开。
