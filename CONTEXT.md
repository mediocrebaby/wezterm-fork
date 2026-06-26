# Context — wezterm-fork

本仓库是 wezterm 的 fork。本文件只是一份术语表，记录本 fork 引入或重新定义的领域语言，**不含任何实现细节**。

## 光标动态效果（Cursor Animation）

围绕"在 wezterm 中实现 Ghostty 风格光标动态效果"这一工作引入的术语。

- **Smear（拖尾）**：光标从旧 cell 位置移动到新 cell 位置的过程中，绘制一个连接旧、新两个光标方块的四边形过渡形状，并在一小段缓动时间内从拉伸态收缩回正常光标形状。这是本 fork 要实现的招牌效果，对应 Ghostty 的 "animated cursor"。Smear **不是**多段半透明残影（motion trail / ghost），也**不是**闪烁淡入淡出（fade blink）。

- **旧位置 / 新位置（prev pos / target pos）**：光标移动事件中，光标移动前所在的 cell（旧位置）与移动后所在的 cell（新位置）。Smear 是这两个位置之间的几何过渡。注意：旧位置与新位置可能不在同一行（跨行移动）。

- **Fade blink（闪烁淡入淡出）**：光标闪烁时颜色平滑淡入淡出的效果。这是 wezterm 已有的特性，**不在**本次 smear 工作范围内，二者是相互独立的光标特性，不要混为一谈。

- **光标渲染 pass（cursor render pass）**：独立于逐行文本渲染、专门负责绘制光标本体及其 smear 的渲染步骤。与 fade blink 等既有逻辑解耦。

- **拉伸量 vs 位置（stretch vs position）**：smear 动画里两个独立的量。**拉伸量**描述拖尾被拉得多长，呈 0→峰值→0 的往返曲线（光标启动时拉长、到位时收缩回正常）。**位置**描述光标中心从旧位置走到新位置的进度，呈 0→1 单调推进，不折返。二者由不同曲线驱动，不可混用同一个进度值。

- **SDF smear**：用有符号距离场（signed distance field）在 fragment shader 中按像素判断是否落在拖尾区域的渲染方式，以获得平滑边缘/圆角。对应 Ghostty 原生观感。本 fork 的 SDF 不走独立渲染管线，而是作为主渲染管线的一种新 `has_color` 模式（`IS_CURSOR_SMEAR`）存在。
