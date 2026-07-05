# Resize 校验框架

针对已知问题「窗口尺寸变换后，原先已展示的内容出现异样」的三层校验框架。
目标：让任何修复尝试都能**快速感知**是否生效、**自动归因**到出问题的层、
并给出下一步**处置路径**。

## 快速使用

```powershell
# 日常快速校验（headless，~1 分钟）
.\resize-verify\verify.ps1

# 修复已知问题后，检查特征测试是否转绿
.\resize-verify\verify.ps1 -KnownIssues

# 完整校验（需先 cargo build -p wezterm -p wezterm-gui --release）
.\resize-verify\verify.ps1 -Gui
```

## 三层结构

| 层 | 载体 | 覆盖范围 | 耗时 |
|---|---|---|---|
| L1 模型层 | `term/src/test/resize_verify.rs`（`cargo test -p wezterm-term resize_verify`） | `Screen::resize` / `rewrap_lines` / `TerminalState::resize`，conpty quirks 开/关两条路径 | 秒级 |
| L2 ConPTY 交互层 | `resize-verify/probe`（`cargo run -p resize-probe`） | 真实 ConPTY 重放流量 × wezterm 模型 rewrap 的合成结果，含拖拽风暴；另含**交互输入场景**（真实 powershell + PSReadLine，逐字符输入与风暴交错，检查输入行叠影/命令执行完整性） | ~40s |
| L3 GUI 端到端 | `resize-verify/gui-check.ps1` | 真实 wezterm-gui 窗口 + Win32 resize + `wezterm cli get-text` 抓屏 | ~1min，半自动 |

所有层共享同一套**不变量**（对同一形状的 fixture：编号短行、超长 ASCII 行、
超长中文宽字符行、TAIL 标记、滚动的 TICK 行）：

1. **丢失** —— resize 后 payload 消失；
2. **重复** —— payload / TICK 出现多于一次（重放错位的典型症状）；
3. **错序** —— 相对顺序改变；
4. **断裂/截断** —— 内容还在但逻辑行拼不回去（wrapped 标记丢失或错置）。

## 处置决策树

```
L1 FAIL ──► 模型层回归（与 ConPTY / GUI 无关）
│           修 term/src/screen.rs (resize / rewrap_lines)、
│           terminalstate/mod.rs (光标折算)、performer.rs (wrapped 标记)。
│           violation 分类直接指明症状；失败输出附逻辑行 dump。
│
L1 PASS, L2 FAIL ──► ConPTY 交互层
│           看 target/resize-verify/report.txt 找到出问题的阶段，
│           再到 raw-stream.bin 里对应 @@STAGE:xxx@@ 区间还原 ConPTY
│           当时重放的转义序列。嫌疑：term 的 is_conpty 分支
│           （resize_preserves_scrollback / makes_sense_to_wrap 启发式）、
│           mux/src/localpane.rs 中 pty 与模型 resize 的先后时序。
│
L1/L2 PASS, L3 FAIL ──► GUI 链路
│           wezterm-gui/src/termwindow/resize.rs（rows/cols 计算、
│           live-resize 分支）、事件风暴节流。
│           看 target/resize-verify/gui/report.txt + screen-*.txt。
│
全部 PASS 但肉眼仍见异样 ──► 渲染层（模型内容正确、画面错误）
            quad/glyph/shape 缓存失效问题（quad_generation 等），
            需人工截图对照，不在本框架自动覆盖范围内。
```

## known-issue 特征测试

`#[ignore = "known-issue: ..."]` 标记的测试描述**期望达到但当前未达到**的
行为，平时不计入回归（保持 CI 绿）。用下面命令单独运行：

```powershell
cargo test -p wezterm-term resize_verify -- --ignored
```

某个特征测试**转绿 = 对应已知问题被修复**，此时应摘除其 `#[ignore]`
将其纳入常规回归（`verify.ps1 -KnownIssues` 会自动提示）。

## 基线（2026-07-05，Windows 11 26200，主症状已修复）

**根因（2026-07-05 定位并修复）**：`term/src/screen.rs` `rewrap_lines`
的「列 0 特例」条件过宽（原为 `x == 0 && y > 0`）。流式输出 `\r\n`
之后光标本就停在独立空行的列 0，却被该特例误判为「wrapped 逻辑行
折算落到下一行开头」而拉回上一行：缩窄时下一行输出**原地覆盖**最后
一条已有输出（丢失），放宽时光标被放到上一行 `x = 新cols` 处，下次
打印触发 autowrap 造成拼行（`[TICK-004 <pad> TICK-005]`），覆盖残留
即错序。三类症状同源；bug 在 plain/conpty 共用路径，ConPTY 只是放大
场景（拖拽 + 滚动输出）。修复：特例增加 `num_lines > 0` 条件（x 是
新宽度的**非零**整数倍才触发）。关键证据：L2 raw-stream.bin 证明
ConPTY 全程只转发纯线性流、无任何重绘/定位序列 —— 症状纯属模型层。

| 检查 | 结果 | 说明 |
|---|---|---|
| L1 常规 10 项 | 全绿 | 含交错风暴（conpty/plain）与光标空行回归守卫（`resize_verify_cursor_blank_line_not_pulled_up`、`resize_verify_interleaved_storm_*`，即本次修复的锁定测试） |
| L1 特征测试 `resize_verify_conpty_space_at_wrap_boundary` | 红（预期） | 已知问题③：conpty 的 `makes_sense_to_wrap` 启发式（performer.rs:183）导致行尾空格处的真实软换行不被标记，放宽后逻辑行拼不回去（`"abcd "` + `"efgh"` 无法合并）。独立问题，尚未修复 |
| L2 单步 5 阶段 | 绿 | 情报：本机 ConPTY 对纯流式输出在单次 resize 时**不主动重放整屏**（各阶段仅收到 TICK 流量） |
| L2 风暴阶段 | 绿（修复后 3 连跑全过） | 修复前 3 轮稳定全红（TICK 错序/新旧内容拼行，即用户所见「旧内容异样」）；修复后连跑 3 次全绿 |
| L3 | 未跑 | 需要已构建的 wezterm-gui；建议构建后跑一次做端到端确认 |

## 「输入错乱」（缩窄→放宽→输入）—— 已修复（2026-07-05）

用户步骤：宽输出（ls）→ 拖拽缩到最窄 → 放宽 → 在 prompt 输入 →
字符上屏位置错乱。复现/验证手段：

- headless：L2 探针 `narrow_then_type_scenario`（多步版 + GUI 重放版）
- 端到端：`render-check-real.ps1`（真实鼠标拖拽边框 + 截图 vs get-text 对照）

根因：**ConPTY 的 conhost 没有 scrollback（缓冲高度=视口高度），
resize 时只对视口内的行 reflow，顶部放不下的行被不可逆丢弃**，且
resize 后几乎不重放——它假定客户端做了与它完全相同的 reflow
（Windows Terminal 与 conhost 共享 TextBuffer::Reflow 所以无此问题）。
wezterm 对完整 scrollback 做可逆 rewrap + conpty 垫行策略，「缩→放」
后与 conhost 布局失步，conhost 的视口相对 CUP 落到错误的行。

修复（term/src/screen.rs）：
1. `Screen::reflow_viewport_conpty` —— conpty 下宽度变化只 reflow
   「缓冲末尾 rows 行」（视口区），scrollback 冻结；视口顶行与
   scrollback 的 wrapped 边界断开；顶部滚出的行保留进 scrollback
   （conhost 丢弃，我们保留是纯收益）；底部垫空行到视口高度。
2. 仅高度变化时保持「光标相对视口顶行号」（conhost ResizeWithReflow
   的 cursorHeightInViewport 语义）。
3. **pre-prune 对 conpty 跳过**——conhost 固定缓冲里光标后的空白行
   一直存在并参与下次 reflow，删掉会让视口窗口滑进 scrollback 失步。

语义代价：conpty 下跨视口边界的逻辑行在边界处 wrapped 标记断开
（与 conhost/Windows Terminal 行为一致），校验的「断裂」判定已加
物理行连接文本复核豁免。

调试工具：`WEZTERM_RAW_DUMP=<path>` 环境变量使 mux（每 pane 追加
`<path>.paneN`）与 wezterm_term::Terminal::advance_bytes 落盘原始
pty 字节流。GUI 侧真相：窗口层会把拖拽事件**合并**为「缩窄终点+
放宽终点」两跳（WEZTERM_LOG=wezterm_term::screen=debug 可证）。

## ls-check：已输出内容的缩放显示校验

`.\resize-verify\ls-check.ps1`（半自动，~40s，运行期间勿动鼠标键盘）：
真实 `ls`（pwsh Get-ChildItem，仓库根）→ 鼠标拖窄到 30% → 拖宽回原尺寸，
每阶段截图 + get-text，三重判定：

1. **内容完整**：整缓冲（含 scrollback）去空白归一化后，基线全文必须
   是连续子串（丢失/乱序/拼行都会破坏它，且对软换行变化不敏感）；
2. **无膨胀**：缓冲字符量膨胀 >200 即疑似重复/残影行；
3. **视口显示**：基线视口尾部 5 行必须仍显示在视口内（旧 bug 形态：
   缓冲完整但内容被垫行顶出视口 → 用户看到「输出不见了」）。

已做双向标定（2026-07-05）：含 conpty reflow 修复的构建全绿；
修复前构建在「拖宽回原尺寸」阶段稳定红（判定 3 检出尾部行被顶出
视口）——校验器不误报、不漏报。

## 注意事项

- **L3 必须用 release 构建的 wezterm.exe（CLI）**：debug 构建的
  `wezterm cli get-text` 在 Windows 上 main 线程稳定栈溢出
  （0xC00000FD），与被测行为无关，属工具链障碍。gui 可用 debug，
  但 CLI 要 release（Find-Binaries 优先找 target\release）。

## 产物位置

- `target/resize-verify/report.txt` —— L2 各阶段逻辑行 dump 与违规明细
- `target/resize-verify/raw-stream.bin` —— L2 原始字节流（含阶段标记），
  归因「ConPTY 到底重放了什么」的第一手证据
- `target/resize-verify/screen-*.txt` —— L2 失败阶段的物理行快照
- `target/resize-verify/gui/` —— L3 的 report / 屏幕快照 / fixture / 临时配置

## 已知问题编号对照（见会话分析）

- **①** ConPTY 与 wezterm 双重 reflow 竞争（根本矛盾，L2/L3 捕获）
- **②** `resize_preserves_scrollback = is_conpty` 垫空行策略（screen.rs；
  纯模型层表现良好，症状需 ConPTY 重放参与，归 L2/L3）
- **③** `makes_sense_to_wrap` 启发式误伤真实软换行（performer.rs:183；
  L1 特征测试已锁定）
