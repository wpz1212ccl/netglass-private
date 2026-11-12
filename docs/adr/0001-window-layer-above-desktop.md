# 窗口层放在桌面之上，而不是图标之下

需求原话是"不遮蔽桌面图标"，直觉做法是把窗口 SetParent 到壁纸 WorkerW，让图标盖在上面。2026-11 在目标机器（build 26300.9818）实测：Progman 带 `WS_EX_NOREDIRECTIONBITMAP`（Win11 的 raised desktop），`SHELLDLL_DefView` 直接挂在其下；全系统 18 个 WorkerW 都是不可见的 202×56 空壳，没有一个带 DefView；`0x052C` 发送两次均返回成功但不生成新 WorkerW；强行 SetParent 后父句柄正确但窗口永不绘制。因此放弃桌面层，改为**始终置顶的卡片**，用「默认可交互 + 全局快捷键锁定为点击穿透」满足"不挡操作"，用尺寸与吸附位置满足"不遮图标"。

**Considered Options**：桌面层（已证明不可行）；置顶 + 点击穿透（采用）；始终置顶可交互（作为锁定前的默认态）；always-on-bottom（会被 Win+D 隐藏）。

**Consequences**：窗口不会进入 Alt+Tab（toolwindow），Win+D 不会隐藏它，全屏应用会盖住它 —— 因此需要自己做"全屏时自动隐藏"与"锁定状态"两套逻辑；这两个行为都写在代码里，别人看不出原因。
