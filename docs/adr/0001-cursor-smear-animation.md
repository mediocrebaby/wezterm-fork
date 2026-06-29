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

### 第四阶段（已被第五阶段取代）：静止时恢复光标形状（bar/underline）

> 注：本阶段的"移动画块、静止交还逐行"分工虽解决了静止形状，但实机验证发现两个问题——(1) 移动到位时有可见的 **block→bar 形变跳变**；(2) 反色等光标配色**未体现在拖尾上**。根因是"拖尾"与"光标头部"由两套机制分别绘制，存在交接，观感割裂。故被第五阶段整体取代。以下保留作演进记录。

第三阶段遗留问题：smear 启用后，`render_screen_line` 用 `!smear_enabled` **无条件**跳过逐行光标 quad，光标形状唯一来源变成只会画实心块（`draw_cursor_quad` 硬编码 `filled_box` + `cursor_bg`）的 smear pass。后果是设成 SteadyBar/SteadyUnderline 时，**移动中和静止时都显示成块**。

采用的方案——**移动中由 smear pass 画块状拖尾虚影，静止后把光标交还逐行路径按形状绘制**（"拖尾是虚影、头部尊重形状"）：

- **分工**：逐行路径（`render_screen_line` → `compute_cell_fg_bg` → `cursor_sprite`）已正确处理 bar/underline 形状、反色、`cursor_border_color` 配色、密码锁、IME 合成、未聚焦时 bar→block 等全部既有语义；静止态直接复用，**不在 smear pass 里重新实现形状与配色**（故"完整配色支持"亦随之解决）。
- **时序难点**：逐行渲染先于 `paint_cursor_smear` 执行，需要"是否仍在动画"时本帧 `update()` 尚未运行。解决：新增跨帧状态 `TermWindow::cursor_trail_animating`（`Cell<bool>`），逐行路径读**上一帧**的值决定是否让出光标（`smear_owns_cursor = smear 启用 && 上一帧在动画`）；smear pass 在末尾写入本帧值。代价是静止后延迟一帧才显形状（≈ 一帧，肉眼不可见），且 smear pass 的 `update_next_frame_time` 保证该帧确实被重绘。
- **缓存失效**：`LineQuadCacheKey` 新增 `cursor_smear_animating: bool`，使动画↔静止切换时光标行缓存失效、强制重画，否则静止帧命中旧的"无光标"缓存。
- **临界帧无空窗**：静止那一帧逐行仍按"上一帧在动画"让出了光标，故 smear pass 在 `was_animating && !animating` 时仍画一次块（此时块已收缩贴合目标格，与下一帧的 bar/underline 衔接）；光标隐藏分支同步把 `cursor_trail_animating` 置 `false`，避免重现时形状被持续抑制。

### 第五阶段（已实现）：smear pass 独占光标——拖尾即形状的拉伸

针对第四阶段的"割裂"反馈，确立新原则：**smear 启用时，光标（形状 + 拖尾 + 静止态 + 配色）完全、始终由 smear pass 一处绘制，逐行路径恒不画普通光标**（仅保留 cell 反色）。无交接 → 无 block→bar 跳变；拖尾本身就是光标形状的拉伸 → 浑然一体。

- **形状即几何**：不在 `CursorTrail` 里引入"形状"概念，而由 `cursor_target_rect` 按形状直接返回**缩窄的目标矩形**——bar = 贴左边、宽 `underline_height` 量级的细列；underline = 贴底边的扁行；block = 整格。4 角弹簧照常作用于这个窄 rect，于是 bar 的拖尾是细竖条的拉伸、underline 是扁条的拉伸。`cursor_spring.rs` 零改动。
- **配色统一**：拖尾全程用**目标 cell** 的光标色（移动跨多格也不逐格变色，符合 Neovide 观感）。新增 helper `resolve_cursor_smear_color`，复刻 `compute_cell_fg_bg` 的普通光标分支——归结为「反色 ? 目标 cell 前景色 : `cursor_bg`」（与形状无关）；反色判定沿用 `force_reverse_video_cursor` + `cursor_is_default_color` + 对比度。所需的目标 cell fg/bg 在 `paint_pane` 末尾用一次 `get_lines(row..row+1)` 读取。
- **特殊光标礼让**：IME 预编辑（composition 变宽块）与密码输入（lock glyph）由逐行路径承载专属 glyph，语义上不该有拖尾。smear pass 检测到 `defer_to_per_line`（composing/leader 或 password_input）即 `snap_to` 当前位并**当帧不画**；逐行路径的 `smear_owns_cursor` 对称地排除这两种情形，照常画块/锁。退出后回归 smear 独占。
- **回退第四阶段脚手架**：移除跨帧状态 `cursor_trail_animating` 与缓存键 `cursor_smear_animating`——A 方案光标全程归 smear、逐行恒不画，不再需要跨帧交接与缓存翻转。
- `CursorSmearParams` 扩展 `shape` / `defer_to_per_line` / `cursor_color`；`draw_cursor_quad` 改用传入的 `cursor_color`。

### 第六阶段（已实现）：宽字符（CJK）光标宽度

第五阶段「smear 独占光标」遗漏了一个场景：`cursor_target_rect` 把 Block/Underline 的矩形宽度硬编码为单个 `cell_width`，但宽字符（中文等 double-width 字形）占 **2 格**。后果是光标停在中文上时拖尾只覆盖**左半格**（「只映照一半」），英文（单格宽）正常。Bar 不受影响（贴左边缘的细竖条，宽度是 `thickness`，与字形宽度无关）。

根因是 smear pass 未对齐逐行路径早已确立的宽字符语义——逐行 `render_screen_line` 的 `cursor_range` 用 `cursor_cell.width()`（宽字符=2、窄字符=1）算光标列跨度，smear pass 缺了这一步。本阶段是把 smear 对齐到该既有语义的**遗漏场景修补**，非新决策（故不另立 ADR）。

- **取宽度**：复用 `paint_pane` 末尾**已有的那次** `get_lines(row..row+1)`（原本只为解析光标配色读目标 cell），顺带取 `cell.width()`，`None`（光标落在行尾外的空白等）回退 1 格——与逐行 `unwrap_or(1)` 一致。零额外 IO，宽度与配色源自同一个 cell，天然一致。`cursor_target_rect` 保持纯函数（只依赖 `params`）。
- **加宽**：经 `CursorSmearParams.cell_cols: usize` 传入；`cursor_target_rect` 的 Block/Underline 宽度改为 `cell_width * cell_cols`，Bar 不变。4 角弹簧照常作用于这个加宽的 rect，中英文间移动时宽度在 1↔2 格间平滑过渡（retarget 保留角的当前位作为新残差，故连续）。
- **右半格无需特判**：wezterm 的 cursor model 把 `cursor.x` 定在宽字符**起始列**；`visible_cells()` 用 `skip_width` 跳过被占据的后续物理 slot，故 `get_cell(cursor.x)` 在起始列返回 `width()==2`、在被跳过的右半列返回 `None`（回退 1）。与逐行路径同样不需要右半格分支。
- **IME 不冲突**：逐行 `cursor_range` 的 `composition_width` 分支由 IME 预编辑触发，而此时 smear `defer_to_per_line` 已礼让、不会执行到 `cursor_target_rect`，故 smear 只需复刻 `cell.width()` 那一支。

### 待办

- 实机调参：`cursor_trail_size`、`cursor_smear_duration_ms`、角排名映射曲线；以及 bar/underline 的 `thickness`（现取 `underline_height`，与原生描边 bar 粗细可能略有差异）。
- **运行期视觉验证**：已编译通过；需在真实窗口确认——(1) SteadyBar/SteadyUnderline 静止时是细条形状、移动时拖尾也是该形状的拉伸且**无 block→bar 跳变**；(2) `force_reverse_video_cursor` 下反色已体现在拖尾上；(3) 中文拼音输入（IME 预编辑）与密码输入时光标正常（变宽块 / 锁形），无双重光标、无残留拖尾；(4) **Neovim Normal 模式下光标停在中文上 block 完整覆盖 2 格**（不再只盖半格）、停在英文上仍是 1 格、中英文间移动时拖尾宽度平滑过渡；`SteadyUnderline` 停在中文上下划线为 2 格长。
