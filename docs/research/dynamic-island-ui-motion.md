# 灵动岛的几何与动效：一手资料调研 + 对 NetGlass 的落地方案

> 调研日期：2026-11。目标读者：实现 NetGlass 展开/收起过渡的人。
> 资料分级：**[官方]** = Apple 一手文档/演讲原文；**[推算]** = 由官方数据算术推出；**[三方]** = 第三方实测或拆解，未获 Apple 确认。

## 0. 结论摘要

1. Apple **公布了**灵动岛的尺寸表、圆角规则（胶囊圆角 = 高度一半，实际由硬件开孔决定）、点按/长按行为、内容边距与"同心圆角"原则，以及"Widget/Live Activity 动画最长 2 秒"的上限。
2. Apple **没有公布**展开/收起动画的时长、`response`、`dampingFraction`、中间帧曲线。所有具体弹簧数值都是第三方测的，不可当规范。
3. 官方对形变的定性描述极明确：弹性、像活体、形变而不是缩放，且**展开前后要保持元素相对位置**、**不要留"额头"**。
4. Web 侧能拿到三个关键原语：`corner-shape: squircle`（Chromium 139+，可动画）、`linear()` 缓动（可用弹簧采样曲线，仍是合成器友好的纯 easing）、圆角 + 内容分层的 transition。
5. NetGlass 的"硬切"根因是 `apply_mode()` 里的 `hide → set_size → set_position → show`（`src-tauri/src/main.rs` 第 134–166 行）：窗口几何只出现一次跳变，CSS 只能在这个跳变**之后**淡入，因此不存在中间帧。

## 1. 几何与视觉

### 1.1 官方尺寸（iPhone，单位 pt）

| 屏幕（竖屏） | Compact leading / trailing（各一侧） | Minimal | Expanded | Limited width |
|---|---|---|---|---|
| 430×932 | 62.33 × 36.67 | 36.67–45 × 36.67 | 408 × 84–160 | 28 × 28 |
| 393×852 | 52.33 × 36.67 | 36.67–45 × 36.67 | 371 × 84–160 | 28 × 28 |

来源：HIG Live Activities → Specifications（[官方](https://developer.apple.com/design/human-interface-guidelines/live-activities)）。注意 393 机型展开宽 371 = 屏宽 − 22（左右各 11 留白），430 机型 408 = 430 − 22，**即展开态左右各留 11pt 屏边距**。[推算]

### 1.2 灵动岛本体宽度 ≈ 126pt，高度 36.67pt

HIG 不给"岛"的总宽，只给每侧内容宽。由两侧内容宽反推硬件开孔：230 − 52.33×2 = 125.34，250 − 62.33×2 = 125.34，两种机型一致 → **开孔约 126pt 宽、36.67pt 高**。[推算] 第三方对 iPhone 14 Pro 开孔的实测亦为约 126pt（≈20.7mm / 378px @460ppi）（[三方](https://www.ifanr.com/1679814)）。

旧版 HIG 页面曾有一张"Dynamic Island width"表（compact/minimal 230 / 250，expanded 371 / 408），中文二手笔记完整转载了该表，并附有"灵动岛使用 44 点圆角"的说法（[三方转载](https://mmmnote.com/article/7e6/12/article-8e84709e431df199.shtml)）。当前（2026-10 版）HIG 已重构、我无法在该页复核 44pt 这一项，故按未确认处理；230/250 与上面的反推自洽，可信度高。

### 1.3 圆角规则

- **胶囊态：圆角 = 高度一半**（36.67/2 ≈ 18.33pt）。这是胶囊的几何定义，HIG 用"concentric"而不是数字来表达：内嵌圆角形状与外轮廓**同心**，四周边距相等。
- **连续曲率**：Apple 平台默认圆角是 continuous corner curve（超椭圆/squircle），不是圆弧。HIG 正文谈的是"圆角形状与 TrueDepth 摄像头相符"，核心诉求是圆角与外轮廓同心、贴合。
- **展开态圆角**：Apple 未给数值；只能从"concentric"原则推：内容容器圆角 ≈ 外圆角 − 内边距。NetGlass 现用 35px（320×200）属于合理量级。

### 1.4 与屏幕顶部的间距

官方没有"距顶部 X pt"的规范，只有两处间接约束：状态栏区域需要可读；紧凑态内容要"snug against the sensor region"（紧贴摄像头区）。真机上岛体上边缘并非紧贴屏幕物理边缘，而是留出约一个状态栏文字的高度。NetGlass 目前 `y = 0` 完全贴顶，可以考虑留 6–8px，视觉上更像"悬浮的岛"而不是"贴边的条"。

### 1.5 背景、描边、材质

- **[官方]** "Live Activities in the Dynamic Island use a black opaque background"，而且 compact / minimal / expanded **都不能自定义背景色**；唯一可定制的视觉钩子是 `keylineTint`（给岛外围的 key line 着色）与内容配色。
- 换言之：NetGlass 的"中性灰胶囊"本身就是对官方配色的偏离（合理的偏离——桌面浅色环境里纯黑太突兀），但既然偏离，就要自己保证对比度与描边。
- 分隔内容时不要画到边缘：用 inset 形状或一条细线（[官方 HIG](https://developer.apple.com/design/human-interface-guidelines/live-activities) / WWDC23 原文 "instead of drawing all the way to the edge of the container, place content inside an inset shape, or use a separator line"）。

### 1.6 内容安全区、字号、留白

- **[官方]** 使用"large, heavier-weight text — a medium weight or higher"，小字慎用；紧凑态要"as narrow as possible with no wasted space"。
- **[官方]** 同心边距：让内容视觉质心落在圆角内壁里；实操技巧是"把内容模糊一下，看模糊后的形状是否与外边框同心"（WWDC23 原文）。
- **[官方]** 展开态不要留"额头"（顶部空白把摄像头凸显出来），要"hug the sensor as tightly as possible and wrap content all the way around it"。
- **[官方]** 展开高度是 84–160 的区间，并明确"avoid a height right on the edge between them"——即别做成 84 或 160 这种边界值。
- 开发者可通过 `contentMargins(_:for:)` 覆盖默认内容边距（[官方](https://developer.apple.com/documentation/widgetkit/dynamicisland)）；这些边距没有公开数值。

## 2. 动效机制

### 2.1 触发方式（官方行为）

- 紧凑/极简态：**轻点** = 打开 App；**长按** = 展开成 expanded（[官方 HIG](https://developer.apple.com/design/human-interface-guidelines/live-activities)）。NetGlass 用"单击展开"，这是桌面鼠标场景的合理改写，不属于官方行为。
- 没有 Live Activity 时，岛本身对点按也有反馈（轻微形变），这是系统级细节，第三方描述为"Q 弹"（[三方](https://quan.ithome.com/content/sharedetail?id=159171)）。

### 2.2 形变方向与锚点

- **[官方定性]** "an organic shape shift"、"designed to feel like a living organism, with a deliberate elasticity"、"bloom into a larger Expanded view"（WWDC23 原文）。关键词是 **bloom / shape shift**，不是 scale。
- **锚点**：以灵动岛本体（顶部居中）为基准向**左右 + 向下**张开；水平方向两侧对称扩张，垂直方向只向下长（上方是屏幕边界）。这一点从 371/408 的居中宽度即可推出。[推算]
- **[官方]** "When laying out your expanded view, try and maintain the relative placement of things between the two views. That way there's a coherence between them."（WWDC23）——紧凑态里的信息在展开态应出现在相近位置，而不是全部换掉重排。这条对 NetGlass 尤其重要：胶囊里的"↓ 数值"应大致落在展开卡片主数字的位置附近。

### 2.3 内容过渡

- **[官方]** 默认内容更新使用 content-replace 过渡：文本是"blurred content transitions"（模糊淡入淡出），增删视图是 fade in/out（[WidgetKit 文档](https://developer.apple.com/documentation/widgetkit/animating-data-updates-in-widgets-and-live-activities)）。
- **[官方]** HIG 建议：布局变化时"preserve as much of the existing layout as possible by animating existing elements to their new positions rather than removing and animating them back in"；不可避免的遮挡则"animate out … then re-animate them in at a new position"。
- **[三方观察]** 展开时状态栏的时间/信号图标会随上层内容覆盖而**模糊淡出**，岛底部会加一层模糊投影暗示层级（[三方拆解](https://www.sohu.com/a/657491690_121124780)、[三方拆解 2](https://m.163.com/dy/article/HTMET0P905169V9M.html)）。这条对桌面版没有直接对应物（状态栏不属于我们），但"底层内容先模糊退场、上层内容后进场"的顺序可以照搬。

### 2.4 时长与弹簧参数

**[官方能确认的]**
- 动画上限：**"Animations in widgets and Live Activities have a maximum duration of two seconds."**（[官方](https://developer.apple.com/documentation/widgetkit/animating-data-updates-in-widgets-and-live-activities)）——这是天花板，不是推荐值。
- 弹簧参数语义：`response` = "The stiffness of the spring, defined as an approximate duration in seconds"；`dampingFraction` = 阻尼占临界阻尼的比例（1 = 无过冲，<1 有过冲）（[官方](https://developer.apple.com/documentation/swiftui/animation/spring(response:dampingfraction:blendduration:))）。
- 手感的调参方法："大多数动画应该使用阻尼值 1"、"80% 阻尼"是有弹性的例子；并且**不要用固定时长去截断弹簧**（WWDC18 *Designing Fluid Interfaces* 原文，[官方](https://developer.apple.com/videos/play/wwdc2018/803/)）。
- HIG Motion：反馈类动画要"brief and precise"，让用户可以随时打断（[官方](https://developer.apple.com/design/human-interface-guidelines/motion)）。

**[没有官方数值]** 岛的展开/收起用了什么 response / dampingFraction、总时长多少。第三方网页复刻常取 700–800ms 并叠 `scaleX(1.04)` 过冲（[三方](https://zhuanlan.zhihu.com/p/566585348)），这个时长明显偏慢（是逐帧手调动画的产物，不是系统行为）。**不要拿它当基准**；按 WWDC18 的"弹簧不设时长"原则 + HIG 的 2s 上限，0.3–0.45s 区间、damping 0.8–0.9 是与 Apple 手感一致的安全区。[推算]

### 2.5 被覆盖 / 被打断

- **[官方]** iOS 上岛收起由系统驱动（Live Activity 结束、alert 结束、用户点开 App），动画是同一个形变的**反向**，不是淡出重建。
- **[官方]** "Let people cancel motion. As much as possible, don't make people wait for an animation to complete before they can do anything"——反向动画必须能从当前帧接上，不能等前一段播完。
- Windows 侧没有"被遮挡"的系统事件，NetGlass 现在是 700ms 轮询前台窗口（`main.rs` 第 542 行起）。这意味着收回总是滞后 0–700ms，观感上比"硬切"更伤：先卡一下，再硬切。

## 3. 迁移到 Web/CSS 的要点

### 3.1 三个原语

1. **`corner-shape`**：`border-radius` 决定角区域大小，`corner-shape` 决定区域内是什么曲线，`squircle` = `superellipse(2)`，可动画，Chrome 139+ 支持，必须配合非零圆角才生效（[MDN](https://developer.mozilla.org/en-US/docs/Web/CSS/corner-shape)）。NetGlass 已在用，方向正确；注意它**不是 Baseline**，WebView2 内核版本过旧时会静默回退成圆弧（视觉上略"硬"，可接受）。
2. **`transition` + 只动合成器属性**：`opacity` / `transform` 走合成器；`border-radius`、`background`、`box-shadow`、`width/height` 都会触发重绘（大模糊阴影尤其贵）。`will-change` 只在确有掉帧时短期打开，长期挂着会常驻显存（[MDN](https://developer.mozilla.org/en-US/docs/Web/CSS/will-change)）。
3. **`linear()` 缓动**：可以让 CSS 直接播放**采样自真实弹簧**的曲线（允许 0–1 之外的值来过冲），无需 JS 逐帧算（[三方，质量高](https://www.joshwcomeau.com/animation/linear-timing-function/)；生成器见 MDN 引用的 Jake Archibald linear() generator）。这比 `cubic-bezier` 更接近 Apple 的弹簧手感，且仍是纯 easing，合成器照旧。

### 3.2 关键判断：形状由"窗口矩形"提供，CSS 只管内容

窗口本身就是可见形状（透明窗口 + 圆角元素铺满）。所以：

- **不要**在 CSS 里把 320×200 的卡片 `scale(0.4875, 0.19)` 压成胶囊——非等比缩放会把圆角压成椭圆，永远得不到真胶囊。这是最容易走错的一步。
- **要**让窗口矩形自己从 156×38 长到 320×200，元素依旧 `inset: 0`，圆角从 18→28 用 CSS transition 跟随。形变 100% 由窗口承担，没有非等比缩放。
- CSS 侧需要 JS 驱动的只有：`border-radius`（可用 transition 免 JS）、内容分层切换（class 切换即可）、以及"过渡进行中"标志位（暂停图表重绘）。**几何不需要 JS**——这也是这套方案的主要好处。

### 3.3 性能注意

- 逐帧改窗口尺寸会让 WebView2 每帧重排重绘。320×200 量级成本很低，但要：过渡期间**停止 SVG 曲线重绘**与数字滚动（现有 `frame()` 的 rAF 已可条件短路），避免主线程被数据刷新抢占。
- 过渡期间**不要动 `box-shadow` 的 blur 半径**（大模糊每帧重算最贵）；用两层阴影交叉淡入，或干脆保持外阴影不变。
- 窗口尺寸用**物理像素整数**逐帧设置，`x = round((screenW - w)/2)`，避免逻辑→物理取整造成的 1px 抖动。
- 若一帧窗口 resize 超过 ~8ms，退到 30fps 也比"硬切"好；但先测再降。

## 4. 事实与推测对照

| 结论 | 级别 | 依据 |
|---|---|---|
| Compact 每侧 52.33/62.33 × 36.67；Expanded 371/408 × 84–160 | 官方 | HIG 规格表 |
| 岛体 ≈126 × 36.67pt，开孔宽度两代一致 | 推算 | 230−2×52.33 = 250−2×62.33 = 125.34 |
| 胶囊圆角 = 高/2（18.33pt），展开圆角未公布 | 推算 | 胶囊几何定义 + HIG 未给数值 |
| 展开态圆角 44pt | 三方（未复核） | 旧版 HIG 中文二手转载 |
| 岛背景为黑色不透明，且不可自定义 | 官方 | HIG Choosing colors |
| 轻点开 App、长按展开 | 官方 | HIG Anatomy / Expanded |
| 形变是"bloom / shape shift"，带弹性，非缩放 | 官方（定性） | WWDC23-10194 |
| 展开前后保持元素相对位置、避免"额头" | 官方 | WWDC23-10194、HIG |
| 动画最长 2 秒 | 官方 | WidgetKit 文档 Note |
| 展开/收起的 response、dampingFraction、时长 | **无官方数据** | 只有第三方观察与复刻 |
| 内容用模糊淡入淡出、状态栏图标模糊退场 | 官方（blur 过渡）+ 三方（观察） | WidgetKit 文档；中文拆解 |
| `corner-shape: squircle` 可动画 | 三方（MDN） | MDN corner-shape |

## 5. 对 NetGlass 的落地建议

### 5.1 320×200 ↔ 156×38 怎么过渡

**建议：窗口矩形由 Rust 逐帧插值，CSS 只做圆角与内容。（能直接做）**

- 删掉 `apply_mode()` 里的 `hide → set_size → set_position → show`：这是"没有中间帧"的唯一原因。改为不隐藏窗口，用一个 ~360ms 的循环，每 ~16ms 用同一缓动函数把 `(x, y, w, h)` 从当前值插到目标值，`set_size(PhysicalSize)` + `set_position(PhysicalPosition)` 一起调。
- **水平方向天然对称**：`HOME_CX = 0.50`，展开态窗口中心恰好是屏幕水平中点；胶囊也是居中。所以每帧 `x = HOME.x + round(w/2) - round(w/2)` 的本质就是 `x = round((screenW - w)/2)`，宽从 156→320 左右对称张开，无需额外处理。
- **纵向是唯一的位移轴**：胶囊上边在 y=0、高 38；展开卡片上边在 y≈0.215×屏高 − 100（150% 缩放、1600 高时约 194 物理像素）、高 200。也就是"上边下移 129、下边下移 291"。建议 **h 用弹簧、y 用更晚更缓的曲线**（y 延迟 ~40ms 起步、ease-in-out 走完 360ms），让前 100ms 看起来是"从顶部顶端往下长出来"，而不是"整块先飞下去再变大"。这是最接近灵动岛观感的一处调参。
- 若 `HOME` 后续允许用户自由放置（不再恒为中心），则水平方向也要插值，此时仍以"胶囊中心 → 目标矩形中心"为锚，`x` 与 `w` 用同一曲线，避免宽度和位置错帧。
- 过渡开始时发一个 `net-morph`（带方向与时长），前端立刻切 class 做圆角/内容过渡；**不要再用 130ms 淡出等待**。
- 反向（收起）用同一函数、更短时长即可天然可打断：从当前矩形出发，不做"等前一段播完"。

**备选（不推荐，成本高）**：窗口固定 320×200，胶囊画在窗口顶部中央，只平移窗口 + CSS 变形。好处是不用逐帧改尺寸；坏处是空透明区域会吃掉鼠标点击，需要给 HWND 挂钩 `WM_NCHITTEST` 返回 `HTTRANSPARENT`（要 subclass 窗口过程，容易与 Tauri 的窗口管理打架）。除非实测 resize 逐帧明显卡，否则别走这条。

**只能近似**：Apple 的形变是单层视图的连续 morph，内容跟着一起拉伸、模糊；我们是"窗口矩形 + 内容独立淡入"，中间帧里内容不会被拉伸。视觉上够用，但严格说不是同一种机制。

### 5.2 具体数值（可直接照抄的参数）

| 项 | 展开 | 收起 |
|---|---|---|
| 窗口矩形 | 360ms | 280ms |
| 曲线（Rust 与 CSS 用同一条） | 采样弹簧 `response 0.36 / dampingFraction 0.86`（约 2–3% 过冲），或近似 `cubic-bezier(.22,1.06,.24,1)` | `response 0.26 / dampingFraction 0.95`（无过冲），或 `cubic-bezier(.4,0,.2,1)` |
| `border-radius` | 18 → 28，360ms，同上曲线 | 28 → 18，280ms |
| `corner-shape` | squircle 全程不变（胶囊在 squircle 下仍是胶囊，因为 r = h/2） | 同 |
| 外层阴影 | 全程保持不变，过渡结束后再切到目标阴影 | 同 |
| 胶囊内容（`.mini`） | 0–110ms 淡出，同时 scale 1 → .96 | 160–300ms 淡入（延迟 = 收起时长的 55%） |
| 卡片内容（`.inner`/`.foot`/`.chart`） | 延迟 100ms，180ms 淡入 + `translateY(6px → 0)` | 0–120ms 淡出（先退，别和胶囊抢屏） |
| 图表重绘 | morph 期间暂停 | 同 |

- 时长都能落到 HIG 的 2s 上限之内且远低于上限；`0.36s` 这个量级与 iOS 交互反馈的常见区间一致，且足够快，不违反"brief and precise"。[推算]
- Rust 侧必须实现**同一个**缓动（建议直接写阻尼弹簧解析解或 cubic-bezier 求值），否则窗口形状和圆角会互相错开 1–2 帧，肉眼可见。
- 关于"被其它窗口覆盖时收回"：把 700ms 轮询改成 ~250ms，或在触发时用 280ms 收起（比展开快），并且**从当前矩形开始**。判定条件建议从"前台是否全屏"扩成"前台窗口矩形与本窗口相交面积 > 60% 且持续 ~500ms"，否则浏览器最大化一点就收，仍然突兀。

### 5.3 内容淡出淡入

- 用**两层并存**而不是同一批元素改字号：`.mini`（胶囊内容）与 `.inner`（卡片内容）各自 `opacity/transform` 过渡，靠 class 切换。文本重排不可能有中间帧，这是唯一稳的做法。
- 顺序：胶囊内容先退（0–110ms）→ 卡片内容后进（100–280ms），中间有 ~150ms 只有形状没有字，正好覆盖窗口长高的过程。Apple 的"底层模糊退场、上层进场"就是这个节奏。
- 卡片内容进场用 `translateY(6px)` + `opacity`，位移方向朝下（与生长方向一致），不要用缩放；整体 `scale` 会让 46px 的数字发虚。
- **[官方原则]** 保持相对位置：胶囊里的 `↓ 12.4 MB/s` 建议落在展开卡片主数字附近（居中偏左），而不是从角落飞过去。

### 5.4 灰色胶囊配色

- 底色：`#2C2C2E` 96% 不透明（或保留现有 `#606066 → #3C3C42` 纵向渐变，但把顶端的 96 灰压暗一档，白字对比更稳）。
- 描边：`inset 0 0 0 .5px rgba(255,255,255,.14)`，替代 Apple 的 keyline；外阴影 `0 6px 16px -10px rgba(0,0,0,.45)`，别太强，胶囊贴顶时阴影会压到状态区。
- 文字：主数值 `#FFF` 600 字重；单位 `rgba(255,255,255,.62)`；上行数值 `.55`。对比度目标 ≥ 4.5:1（3C3C42 上白字约 8:1，安全）。
- 状态点保留 `#34C759`，但在灰底上把发光阴影去掉或降到 `.35`，否则灰底会显得脏。
- 独有约束：Apple 岛是纯黑因此自带"挖孔"的错觉；NetGlass 是灰的，圆角与描边必须干净，`corner-shape: squircle` 已足够，不要再叠内发光。

### 5.5 能直接做 vs 只能近似

**能直接做**
- 胶囊 = 高度一半的圆角；卡片 = 连续曲率 squircle（Chromium 139+）。
- 顶部居中锚点、水平对称展开、纯生长（不做非等比缩放）。
- 窗口几何逐帧动画 + 可打断的反向过渡。
- 圆角/阴影/内容的过渡；内容两层交叉淡入淡出。
- 过渡中暂停数据重绘，保证形状动画独占帧预算。

**只能近似**
- Apple 的"内容随形状一起形变"：我们中间帧里内容是淡入淡出，不会拉伸。
- 弹簧物理的**窗口**动画：受 DWM 合成 + WebView2 重排影响，逐帧 resize 可能到不了 60fps；必要时用 30fps 换取稳定。
- 状态栏元素随岛展开而模糊退场：屏幕状态栏不归我们管，只能对本窗口自己的底层内容做模糊。
- 44pt 展开圆角之类的"官方数值"：没有可靠一手数据，按 concentric 原则自定（现 28px 合理）。

## 6. 来源

- Apple HIG · Live Activities：https://developer.apple.com/design/human-interface-guidelines/live-activities
- Apple HIG · Motion：https://developer.apple.com/design/human-interface-guidelines/motion
- WWDC23-10194 *Design dynamic Live Activities*：https://developer.apple.com/videos/play/wwdc2023/10194/
- WWDC18-803 *Designing Fluid Interfaces*：https://developer.apple.com/videos/play/wwdc2018/803/
- WidgetKit · *Animating data updates in widgets and Live Activities*：https://developer.apple.com/documentation/widgetkit/animating-data-updates-in-widgets-and-live-activities
- SwiftUI · `Animation.spring(response:dampingFraction:blendDuration:)`：https://developer.apple.com/documentation/swiftui/animation/spring(response:dampingfraction:blendduration:)
- WidgetKit · `DynamicIsland` / `contentMargins(_:for:)`：https://developer.apple.com/documentation/widgetkit/dynamicisland
- MDN · `corner-shape`：https://developer.mozilla.org/en-US/docs/Web/CSS/corner-shape
- MDN · `linear()` easing：https://developer.mozilla.org/en-US/docs/Web/CSS/easing-function/linear
- MDN · `will-change`：https://developer.mozilla.org/en-US/docs/Web/CSS/will-change
- Josh Comeau · *Springs and Bounces in Native CSS*：https://www.joshwcomeau.com/animation/linear-timing-function/
- 知乎 · 灵动岛 Web 动画实践：https://zhuanlan.zhihu.com/p/566585348
- 搜狐 · 灵动岛动画细节拆解：https://www.sohu.com/a/657491690_121124780
- 网易 · 设计师必看：灵动岛动画细节：https://m.163.com/dy/article/HTMET0P905169V9M.html
- 少数派 · Chan Karunamuni 与灵动岛设计：https://sspai.com/a/z262mz
- 爱范儿 · 灵动岛开孔约 126pt / 20.7mm：https://www.ifanr.com/1679814
- mmmnote · iOS 16 灵动岛尺寸规范笔记（HIG 旧表转载）：https://mmmnote.com/article/7e6/12/article-8e84709e431df199.shtml
