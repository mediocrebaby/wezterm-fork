# 0001 — 光标 smear（拖尾）动画的实现架构

- 状态：已接受
- 日期：2026-06-27

## 背景

在本 wezterm fork 中实现 Ghostty 风格的光标 smear（拖尾）动画：光标移动时，在旧、新位置之间绘制一个连接两者的四边形过渡形状，并在一小段缓动时间内从拉伸态收缩回正常光标，呈现"拉长再收缩"的观感。术语见 [CONTEXT.md](../../CONTEXT.md)。

实现前对代码库做了核查，确认了若干硬约束，它们共同决定了下面的决策。每条决策都是在多个可行方案中、基于具体理由做出的权衡，且事后改变代价较大，故记录于此。

## 决策

### 1. 渲染架构：独立的光标渲染 pass（在 paint_pane 层）

现状：光标只在 `render_screen_line` 中、且仅当 `stable_line_idx == cursor.y` 的那一行被绘制；逐行渲染时每行不知道其他行的像素 Y。

决策：不在逐行循环里画 smear，而在 `paint_pane`（`wezterm-gui/src/termwindow/render/pane.rs`）层新增一个独立的光标渲染步骤。该层已能算出 pane 内任意 cell 的像素坐标，天然支持跨行 smear，并与既有的闪烁（fade blink）逻辑解耦。

权衡：改动比"仅同行 smear"大，但换来跨行能力与干净的解耦。

### 2. 缓动：复用 ColorEase，拉伸量与位置分离

`ColorEase`（`wezterm-gui/src/colorease.rs`）本质是一个通用的 0→1→0 进度缓动器，`intensity_one_shot()` 返回 `(intensity, next_frame_time)`，且已接好 wezterm 的按需重绘与 `animation_fps`。

决策：复用 `ColorEase`，新增一个 smear 专用缓动状态。其往返曲线（0→峰值→0）用于驱动 **拉伸量**；光标 **中心位置** 另用 `elapsed/total` 的单调 0→1 时间插值驱动。二者解耦（见 CONTEXT.md 中"拉伸量 vs 位置"）。

权衡：避免重写缓动器与帧调度；代价是需要小心不要把往返的 intensity 误用到位置上。

### 3. 实现路线：SDF，单渲染管线加第 6 种 has_color 模式

Ghostty 原生用 GPU fragment shader + SDF（距离场）逐像素判定拖尾区域，以获得平滑边缘。wezterm 是单渲染管线设计，用 `has_color` 标志位在一个 shader 里分流 5 种渲染模式，同时维护 wgpu（`shader.wgsl`）和 glium（`glyph-frag.glsl`/`glyph-vertex.glsl`）两套后端。

决策：走 SDF 路线以贴近 Ghostty 观感，但**不新建独立渲染管线**。在现有单管线中新增第 6 种模式 `IS_CURSOR_SMEAR`，复用现有顶点流与 draw 循环，在 fragment shader 里增加一段 SDF 分支。smear 所需的旧/新矩形坐标与进度 t 通过 smear quad 的顶点属性编码传入，不扩展全局 ShaderUniform。

权衡：SDF 比 CPU 顶点四边形多改 shader（且需 wgpu/glsl 两处同步），但拿到平滑圆角/任意角度；单管线第 6 模式比独立管线侵入小得多，避免在两套后端各建一整套管线及处理绘制顺序。

### 4. 坐标系：存屏幕像素位，滚动时不 smear

光标 y 是 `StableRowIndex`（不随滚动变），而屏幕像素位 = `(stable_row - viewport_top) * cell_height`，`viewport_top` 随滚动变化。

决策：smear 的"旧位置"存上一帧的**实际屏幕像素坐标**。若两帧间视口顶变化（发生了滚动），则不触发 smear、直接瞬移。

权衡：滚动中的光标位移并非用户感知的"光标移动"，按像素位存可避免把滚动误判为移动而产生错误的大 smear。与 Ghostty 取舍一致。

## 起步阶段的非架构性选择（可后续调整，不属本 ADR 的难撤销决策）

- 触发范围：只要光标位置变化即 smear（含逐格打字），不设最小距离阈值。
- 配置：`cursor_smear_duration_ms`（0 = 关闭，默认关闭，兼作开关与时长）为主开关；附带 `cursor_smear_ease`（`EasingFunction`，默认 linear）以提供 ColorEase 必需的缓动曲线，与 `cursor_blink_ease_*` 对称。颜色/拖尾长度暂硬编码。

## 影响的文件

- `config/src/config.rs` — 新增 `cursor_smear_duration_ms`
- `wezterm-gui/src/termwindow/mod.rs` — 新增 smear 缓动状态字段
- `wezterm-gui/src/termwindow/prevcursor.rs` — 记录上一帧光标的屏幕像素位
- `wezterm-gui/src/termwindow/render/pane.rs` — 独立光标渲染 pass
- `wezterm-gui/src/quad.rs` — smear quad 顶点编码（SDF 阶段）
- `wezterm-gui/src/shader.wgsl` + `glyph-frag.glsl`/`glyph-vertex.glsl` — 第 6 种模式与 SDF（SDF 阶段）

## 实现进度

### 第一阶段（已实现，分支 `feature/cursor-smear-animation`）

目标：打通"平滑移动"的端到端链路并可见，先不做 SDF 拖尾形状。

- 配置 `cursor_smear_duration_ms` / `cursor_smear_ease`；默认关闭。
- `CursorSmearState`（`prevcursor.rs`）：按屏幕像素位检测移动、锁定动画起点、滚动时抑制（返回 Jump）。
- 独立光标 pass `paint_cursor_smear`（`pane.rs`）：在逐行渲染后、不走行缓存，复用既有 `filled_rectangle` 绘制光标块；位置在旧/新像素位间按时间线性插值。
- `render_screen_line` 在 smear 启用时跳过逐行光标 quad（仍保留光标下 cell 的反色）。

第一阶段的务实简化（待后续阶段补全）：

1. 独立 pass 只绘制**实心块状光标**，使用 `cursor_bg` 着色；尚未支持 bar/underline 形状，也未复用 `compute_cell_fg_bg` 的完整光标配色（反色仍由逐行路径计算）。
2. 只做**位置插值**，未做拉伸量（stretch）与拖尾几何；`ColorEase` 已接入并驱动重绘调度，其 intensity 预留给 SDF 阶段做拉伸。

第一阶段已通过 `cargo build -p wezterm-gui` 编译验证（环境补装 Strawberry Perl 以构建 vendored OpenSSL）。

### 第二阶段（已实现）：SDF 拖尾

- 顶点扩展（`quad.rs`）：新增 `smear_a`/`smear_b` 两个 `vec4` 字段，同步 wgpu `ATTRIBS`、glium `implement_vertex!`、`BoxedQuad` 往返。其他渲染模式忽略该字段。
- 第 6 种模式 `IS_CURSOR_SMEAR = 5.0`：`shader.wgsl` 与 `glyph-frag.glsl`/`glyph-vertex.glsl` 两套后端同步实现 `sdf_box` + `sdf_convex_quad` + `sdf_cursor_smear`，按片元在 quad-local [0,1] 空间的 SDF 距离做抗锯齿着色。
- smear pass（`pane.rs::draw_cursor_smear_sdf`）：发射一个覆盖「旧 ∪ 新矩形」并集包围盒（外扩抗锯齿边距）的 quad，把旧/新中心、半尺寸、progress、edge_blur 编码进顶点（均为 quad-local 归一化坐标，分辨率无关）。静止（无动画）时仍走纯色块 `filled_rectangle`。

实现细节澄清（相对 ADR 决策 #2）：SDF 采用「旧矩形 4 角按 progress 向新矩形对应角插值」的凸四边形表述，**位置与拉伸由同一个单调 progress（0→1）统一驱动**——拖尾在 progress 增大时自然从满拉伸收缩到目标块，无需 ColorEase 的往返 intensity 曲线单独驱动拉伸。`ColorEase`（`cursor_smear_ease_state`）现仅用于重绘调度。

### 第三阶段（已实现）：改用 Neovide 式 4 角弹簧——取代 SDF

实机验证后，SDF 扫掠拖尾（即便加了锥形收窄）仍达不到 Neovide 的生动观感。用户明确以 **Neovide** 为参照标准，故切换实现路线，**整体取代决策 #2、#3 的 ColorEase + SDF 方案**：

- 路线：移植 Neovide 的 `cursor_renderer`——光标用 **4 个角**表示，每个角的 X/Y 各用**临界阻尼弹簧**（`zeta=1`，`omega=4/animation_length`）独立缓动到目标；按"角方向 vs 运动方向"的点积给 4 角排名，前缘角动画时长短（先到）、后缘角时长长（滞后），由此把光标四边形**拉伸**成拖尾。
- 渲染：把 4 个动画角连成一个**填充四边形**（`set_position_quad` + 纯色模式），**不再用 SDF**。底层顶点本就支持任意坐标，新增 `set_position_quad` 抽象即可。
- 新增 `cursor_spring.rs`（`Spring`/`Corner`/`CursorTrail`）、`quad.rs::set_position_quad`、`config.cursor_trail_size`（默认 0.7，仿 Neovide）。`cursor_smear_duration_ms` 改作弹簧 `animation_length`。
- **回退了 SDF 全部脚手架**：`shader.wgsl`/`glyph-*.glsl` 的 SDF 代码、`IS_CURSOR_SMEAR` 模式、顶点 `smear_a`/`smear_b` 字段、`ColorEase` smear 状态全部移除，避免遗留死代码。

弹簧的滚动/可见性处理沿用决策 #4：滚动或光标隐藏时 `CursorTrail::snap_to`（无动画瞬移）。

### 待办

- 光标形状（bar/underline）与完整配色支持（当前 trail 只画块状、用 `cursor_bg`）。
- 实机调参：`cursor_trail_size`、`cursor_smear_duration_ms`、角排名映射曲线。
- **运行期视觉验证**：已编译通过；4 角弹簧拖尾的观感、跨行/对角移动表现需在真实窗口确认。
