# Resize 校验框架 —— 一键运行器
#
# 用法:
#   .\resize-verify\verify.ps1              # L1 + L2（快速，纯 headless）
#   .\resize-verify\verify.ps1 -KnownIssues # 额外检查 known-issue 特征测试是否转绿
#   .\resize-verify\verify.ps1 -Gui         # 额外跑 L3（需要已构建的 wezterm-gui）
#
# 退出码: 0 = 已运行层级全部通过; 1 = 有层级失败
# 失败时按「处置决策树」输出归因结论，详见 resize-verify\README.md

param(
    [switch]$KnownIssues,
    [switch]$Gui,
    [string]$TargetDir = ""
)

$ErrorActionPreference = "Continue"
[Console]::OutputEncoding = [Text.Encoding]::UTF8
$RepoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $RepoRoot

$results = [ordered]@{}

Write-Host "`n========== L1 模型层 (term crate 单测) ==========" -ForegroundColor Cyan
cargo test -q -p wezterm-term resize_verify 2>&1 | Where-Object { $_ -match "^test |test result" }
$results["L1"] = ($LASTEXITCODE -eq 0)

if ($KnownIssues) {
    Write-Host "`n========== L1 known-issue 特征测试 ==========" -ForegroundColor Cyan
    $out = cargo test -q -p wezterm-term resize_verify -- --ignored 2>&1
    $out | Where-Object { $_ -match "^test |test result" }
    if ($LASTEXITCODE -eq 0) {
        Write-Host ">>> 所有 known-issue 特征测试已转绿！" -ForegroundColor Green
        Write-Host ">>> 请将 term\src\test\resize_verify.rs 中对应测试的 #[ignore] 摘除，纳入常规回归。" -ForegroundColor Green
    } else {
        $green = $out | Where-Object { $_ -match "^test .+ \.\.\. ok$" }
        if ($green) {
            Write-Host ">>> 部分特征测试转绿（下列测试对应的已知问题可能已修复，请摘除其 #[ignore]）:" -ForegroundColor Green
            $green | ForEach-Object { Write-Host "    $_" -ForegroundColor Green }
        } else {
            Write-Host ">>> known-issue 均仍为红（符合未修复时的预期基线）"
        }
    }
}

Write-Host "`n========== L2 ConPTY 交互层 (resize-probe) ==========" -ForegroundColor Cyan
cargo run -q -p resize-probe 2>&1 | Where-Object { $_ -notmatch "^\s*$" -and $_ -notmatch "warning|-->" }
$results["L2"] = ($LASTEXITCODE -eq 0)

if ($Gui) {
    Write-Host "`n========== L3 GUI 端到端 (gui-check) ==========" -ForegroundColor Cyan
    & (Join-Path $PSScriptRoot "gui-check.ps1") -TargetDir $TargetDir
    $results["L3"] = ($LASTEXITCODE -eq 0)
}

# ---------------------------------------------------------------------------
# 汇总 + 处置决策树
# ---------------------------------------------------------------------------
Write-Host "`n==================== 汇总 ====================" -ForegroundColor Cyan
foreach ($k in $results.Keys) {
    $tag = if ($results[$k]) { "PASS" } else { "FAIL" }
    $color = if ($results[$k]) { "Green" } else { "Red" }
    Write-Host ("  {0}: {1}" -f $k, $tag) -ForegroundColor $color
}

$fail = @($results.Keys | Where-Object { -not $results[$_] })
if ($fail.Count -eq 0) {
    Write-Host "`n已运行层级全部通过。" -ForegroundColor Green
    if (-not $Gui) {
        Write-Host "提示: L1/L2 无法覆盖 GUI 链路（termwindow/renderer），若用户仍反馈异样，请运行 -Gui。"
    }
    exit 0
}

Write-Host "`n---------- 归因（处置决策树） ----------" -ForegroundColor Yellow
if (-not $results["L1"]) {
    Write-Host @"
[L1 FAIL] 问题在 wezterm 自身的 rewrap 模型（与 ConPTY/GUI 无关）:
  · term\src\screen.rs        Screen::resize / rewrap_lines
  · term\src\terminalstate\mod.rs  TerminalState::resize（光标折算）
  · term\src\terminalstate\performer.rs  wrapped 标记逻辑
  处置: 先修模型层并让 L1 转绿，再看 L2/L3。失败输出中的violation分类
  （丢失/重复/错序/断裂）直接指向 rewrap 的哪一步弄坏了行。
"@
} elseif (($results.Contains("L2")) -and (-not $results["L2"])) {
    Write-Host @"
[L1 PASS, L2 FAIL] 问题在 ConPTY 交互层（重放流量 x 模型 rewrap 的合成）:
  · 看 target\resize-verify\report.txt 定位出问题的阶段
  · 用 target\resize-verify\raw-stream.bin 中 @@STAGE:xxx@@ 标记之间的
    原始转义序列，还原 ConPTY 在该阶段实际重放了什么
  · 嫌疑代码: term 的 is_conpty 分支（resize_preserves_scrollback、
    makes_sense_to_wrap）、mux\src\localpane.rs resize 先后顺序
  处置: 判断是「ConPTY 重放覆盖错行」还是「quirk 启发式误判」，
  修改后重跑 L2；注意同时保持 L1 全绿。
"@
} elseif ($results.Contains("L3") -and (-not $results["L3"])) {
    Write-Host @"
[L1/L2 PASS, L3 FAIL] 问题在 GUI 链路（模型之上）:
  · wezterm-gui\src\termwindow\resize.rs  resize/apply_dimensions
    （rows/cols 计算、live-resize 跳过缩放重算的分支）
  · 事件风暴节流、webgpu/renderer 的尺寸同步
  · 看 target\resize-verify\gui\report.txt 与 screen-*.txt
  处置: 若 get-text 内容正常但屏幕显示异样，则问题在渲染层
  （quad/glyph 缓存失效），需要人工截图对照。
"@
}
exit 1
