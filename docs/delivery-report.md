# NetGlass 交付报告

> 交付时间：2026-11-12 凌晨。执行方式：AFK（无人值守），按 `docs/spec.md` 与 4 份 ADR 实施。
> 验收口径：产品经理子代理按 `docs/pm-acceptance-rubric.md` 打分，**≥8.0/10 及格**。

## 1. 交付物

| 文件 | 说明 |
|---|---|
| `dist\NetGlass.exe` | 便携版，双击即用，无需安装、无需管理员权限 |
| `NetGlass-demo.mp4` | 演示视频（旁白 + 实机录屏，见第 6 节） |
| `docs\spec.md` | 功能规格（界面、数据、探针、系统行为、验收清单） |
| `docs\technical-plan.md` | 技术方案（架构、模块、风险） |
| `docs\adr\0001-0003` | 窗口层、计数源、不复用 TrafficMonitor 的决策记录 |
| `docs\pm-acceptance-rubric.md` | PM 评分表 |
| `sessionlog\afk-netglass.md` | 实施日志（含每个坑的实测证据） |
| `prototype\` | 视觉原型与本机探针脚本（可删） |

## 2. 运行

1. 双击 `dist\NetGlass.exe`。首次启动会在 `HKCU\...\Run` 注册开机自启（托盘可关）。
2. 卡片出现在屏幕中央偏右，置顶、不占任务栏、不进 Alt+Tab。
3. 托盘图标：**左键**显示/隐藏；**右键**菜单：显示 / 隐藏、锁定 / 解锁（会显示当前快捷键）、开机自启、退出。
4. 锁定快捷键：**Ctrl+Alt+N**。本机 Ctrl+Alt+L 已被其他程序占用，程序会自动回退并在菜单里显示实际生效的键。
5. 配置与位置：`%APPDATA%\com.netglass.app\config.json`，退出时写回。

## 3. 实测数据

| 指标 | 数值 | 方法 |
|---|---|---|
| 空闲 CPU | **0.104%**（单核，45s 均值；改前 0.364%） | `Get-Process.TotalProcessorTime` |
| 常驻内存 | 主进程 30 MB；**全树私用 215 MB / WS 481 MB**（含 6 个 WebView2 子进程） | 进程树求和 |
| 启动到可见 | **1095 ms**，首帧即完整内容（无空板） | 90ms 连拍 |
| 采样开销 | 80 µs/次 @1Hz（接口表 60s 重建） | ADR 0002 |
| 材质 | DWM 浅色背景（DWMSBT_TRANSIENTWINDOW）+ 白覆盖 16%→6% + 噪点 0.07 | 见 §8 限制 1 |

## 4. 验收清单（对照 spec §14）

| # | 项目 | 状态 | 证据 |
|---|---|---|---|
| 1 | 1.5s 内出现，无白闪/黑块 | ✅ | 1095ms 可见，首帧即完整（font-display: swap + 1050ms 对齐 show） |
| 2 | 下载读数与资源监视器偏差 ≤5% | ⚠️ 待实测 | 方法见第 5 节 |
| 3 | 上行读数正确 | ✅ | 实机观察 |
| 4 | 断开宽带 3s 内归零/切换 | ⚠️ 未实测 | 会中断当前网络，留给用户手测 |
| 5 | Wi-Fi 射频关闭显示"已关闭" | ❌ 显示"未连接" | 未接 WlanAPI 射频状态，见限制 |
| 6 | 拖动 + 边缘吸附 | ⚠️ 拖动可用，吸附未做 | 见限制 |
| 7 | 重启恢复位置与锁定 | ✅ | config.json 持久化 |
| 8 | Win+D 后卡片仍可见 | ✅ | 置顶 + toolwindow |
| 9 | 全屏应用自动隐藏 | ✅ | `SHQueryUserNotificationState`，只认 D3D 全屏与演示模式 || 10 | 锁定后点击穿透 | ✅ | `WS_EX_TRANSPARENT`，PM 已验 |
| 11 | 开关代理 5s 内状态变化 | ⚠️ 未实测 | 探针与端口检测已实现，未真机开关代理 |
| 12 | 断网恢复无负值 | ⚠️ 未实测 | 计数器回退按重置处理（代码保证） |
| 13 | 睡眠唤醒首屏合理 | ⚠️ 未实测 | 单调时钟 + 间隔 >10s 丢弃（代码保证） |
| 14 | 150% / 200% 缩放 | ⚠️ 150% 已验证 | 200% 未实测 |
| 15 | 24 小时长跑 | ⚠️ 未跑满 | 内存 10 分钟无增长趋势 |

## 5. 精度对照方法（可复跑）

```powershell
# 1) 限速下载
curl.exe --limit-rate 8M --max-time 20 -o NUL https://mirrors.aliyun.com/ubuntu-releases/24.04/ubuntu-24.04-desktop-amd64.iso
# 2) 同时用 Get-NetAdapterStatistics 读"以太网 3"的 Rx 字节增量 / 10s
# 3) 与卡片显示值比较：拨号计数器应略低于以太网（封装开销），偏差在 5% 内
```

## 6. 演示视频

- 文件：`NetGlass-demo.mp4`（1920×1200，H.264 + AAC，54 秒，4.2 MB）
- 旁白：Edge TTS `zh-CN-XiaoxiaoNeural`，6 段；画面：实时速度（6.3 MB/s 限速下载）→ 悬停详情（延迟 13 ms / 峰值 10.9 MB/s）→ 代理状态 → Ctrl+Alt+N 锁定（右上"已连接 · 已锁定"）→ 收尾
- 复现：`.scratch\record-demo.ps1`（MPEG-TS 录制，ffmpeg 自终止）、`.scratch\mux-demo.ps1`（拼旁白 + 压 1920 宽）

## 7. PM 验收

**最终 8.1 / 10（及格）**。共 6 轮，取证在 `prototype\pm2` … `pm7`。

| 轮次 | 分数 | 主要结论 |
|---|---|---|
| 1 | — | 取证失效（截图被 DPI 虚拟化裁成 22×22），中止 |
| 2 | 7.4 | hover 叠字、Ctrl+Alt+L 被占用、详情节奏、曲线贴底、玻璃像白板 |
| 3 | 6.7 | 新增 CPU 尖峰、锁定文字被采样覆写、材质透射仅 17% |
| 4 | 7.7 | 启动 0.9s 空板、边缘灰环、小字对比不足、离开悬停空窗 |
| 5 | 7.1 | 全树 CPU 30%、四边 2px 深框、整树内存 225 MB |
| **6** | **8.1** | **及格**：整树 CPU 30%→6%、深框消除、叠影消除、曲线抬底、状态位常驻、对比度提升 |

PM 剩余扣分（不阻断交付）：整树内存 ~225 MB（WebView2 平台成本）、详情标签 10.5px 3.3:1、玻璃外圈发丝、hover 底行跨列、曲线端头被圆角切 7px。

## 8. 已知限制

1. **玻璃的边界**：Windows 11 的 DWM 背景（Mica / DWMSBT_TRANSIENTWINDOW）按设计只采样壁纸，不采样窗口——压在别的窗口上时颜色不随其变化。传统 BLURBEHIND 实测会把卡片压成深灰（白底上 RGB≈50），视觉不可接受。macOS 小组件同样是壁纸级材质，因此接受这一平台约束。
2. **内存**：WebView2 进程树私用约 215 MB（主进程仅 30 MB），高于 spec 的 150 MB 目标；这是 WebView2 的运行时成本，除非改为原生渲染，否则压不下去。
3. **Wi-Fi 射频关闭**显示"未连接"而非"已关闭"——需要 WlanAPI 射频查询。
4. **边缘吸附**未实现（12px 吸附），拖动与位置记忆可用。
5. **无历史持久化**——按 spec 决定，只有内存 60 秒曲线。
6. **探针口径**是 CONNECT 隧道时延，不是 TLS 首字节或吞吐。
7. **仅便携版**，未打 NSIS 安装包；自启路径指向 `dist`，移动目录后需在托盘里关开一次自启。
8. 本机 **Ctrl+Alt+L 被占用**，实际快捷键为 Ctrl+Alt+N，托盘菜单会显示。

## 9. 复跑命令

```powershell
# 构建
cd "D:\code\vibe coding\桌面网络监看器\src-tauri"
cargo build --release
copy target\release\netglass.exe ..\dist\NetGlass.exe

# 截图（DPI-aware）
powershell -ExecutionPolicy Bypass -File "..\.scratch\card.ps1" -Out "..\prototype\s1\shot.png"

# 录演示
powershell -ExecutionPolicy Bypass -File "..\.scratch\record-demo.ps1"
```

## 10. 决策回溯

| 决策 | 文档 |
|---|---|
| 窗口放在桌面之上，而非图标之下 | `docs/adr/0001` |
| 速度只从 GetIfEntry2 读 | `docs/adr/0002` |
| 不复用 TrafficMonitor，从零构建 | `docs/adr/0003` |
| 浅色玻璃取 Mica，不取亚克力 | `sessionlog/afk-netglass.md`「材质实验」 |
| hover 不改变窗口尺寸 | `docs/spec.md` §4.1 |
