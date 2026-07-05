# Resize 校验框架 —— L3 GUI 端到端（半自动）
#
# 启动真实构建的 wezterm-gui，在 pane 里跑 fixture（编号行 + 长行 + 中文 +
# 持续 TICK），用 Win32 SetWindowPos 改窗口尺寸（含拖拽风暴），
# 每个阶段用 `wezterm cli get-text` 抓取 pane 内容做不变量检查：
# 丢失 / 重复 / 错序 / 截断。
#
# 用法:
#   .\resize-verify\gui-check.ps1                       # 自动找 target\{release,debug}
#   .\resize-verify\gui-check.ps1 -TargetDir target\release
#
# 前置: cargo build -p wezterm -p wezterm-gui [--release]
# 注意: 会新开一个 wezterm 窗口并在结束时关闭它；请勿在检查期间操作该窗口。

param(
    [string]$TargetDir = "",
    [string]$OutDir = "target\resize-verify\gui"
)

$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = [Text.Encoding]::UTF8
$OutputEncoding = [Text.Encoding]::UTF8

$RepoRoot = Split-Path -Parent $PSScriptRoot

function Find-Binaries {
    param([string]$Hint)
    $candidates = @()
    if ($Hint) { $candidates += (Join-Path $RepoRoot $Hint) }
    $candidates += (Join-Path $RepoRoot "target\release")
    $candidates += (Join-Path $RepoRoot "target\debug")
    foreach ($dir in $candidates) {
        $gui = Join-Path $dir "wezterm-gui.exe"
        # debug 构建的 wezterm.exe cli 在 Windows 上主线程栈溢出(0xC00000FD)；
        # wezterm-bigstack.exe 是 editbin /STACK 调大栈后的副本，优先使用
        $cli = Join-Path $dir "wezterm-bigstack.exe"
        if (-not (Test-Path $cli)) { $cli = Join-Path $dir "wezterm.exe" }
        if ((Test-Path $gui) -and (Test-Path $cli)) {
            return @{ Gui = $gui; Cli = $cli }
        }
    }
    throw "未找到 wezterm-gui.exe / wezterm.exe。请先构建: cargo build -p wezterm -p wezterm-gui --release"
}

$bins = Find-Binaries -Hint $TargetDir
$OutDirFull = Join-Path $RepoRoot $OutDir
New-Item -ItemType Directory -Force $OutDirFull | Out-Null

# ---------------------------------------------------------------------------
# fixture 脚本（在 wezterm pane 内运行）
# ---------------------------------------------------------------------------
$fixturePath = Join-Path $OutDirFull "fixture.ps1"
@'
[Console]::OutputEncoding = [Text.Encoding]::UTF8
$OutputEncoding = [Text.Encoding]::UTF8
1..6 | ForEach-Object { "L{0:d2}|short-{0:d2}" -f $_ }
$long = -join (0..11 | ForEach-Object { "SEG{0:d2}-abcdefghij." -f $_ })
"L07|$long"
$cjk = "中文宽字符换行校验一二三四五六七八九十" * 4
"L08|$cjk"
"L09|TAIL-MARKER"
"@@FIXTURE-READY@@"
1..40 | ForEach-Object { "TICK-{0:d3}" -f $_; Start-Sleep -Milliseconds 300 }
"@@TICKS-DONE@@"
'@ | ForEach-Object {
    # 显式带 BOM：pane 内用 Windows PowerShell 5.1 运行，无 BOM 会把中文按
    # 本地代码页误读
    [IO.File]::WriteAllText($fixturePath, $_, (New-Object Text.UTF8Encoding $true))
}

# 最小化配置，排除用户配置干扰
$configPath = Join-Path $OutDirFull "wezterm-verify.lua"
@'
local wezterm = require 'wezterm'
return {
  check_for_updates = false,
  window_close_confirmation = 'NeverPrompt',
  initial_cols = 80,
  initial_rows = 24,
}
'@ | ForEach-Object {
    [IO.File]::WriteAllText($configPath, $_, (New-Object Text.UTF8Encoding $false))
}

# ---------------------------------------------------------------------------
# Win32 resize 支持
# ---------------------------------------------------------------------------
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class RV {
    [DllImport("user32.dll")] public static extern bool SetWindowPos(
        IntPtr hWnd, IntPtr after, int x, int y, int cx, int cy, uint flags);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(
        IntPtr hWnd, out RECT rect);
    public struct RECT { public int Left, Top, Right, Bottom; }
}
"@
$SWP = 0x0002 -bor 0x0004 -bor 0x0010  # NOMOVE | NOZORDER | NOACTIVATE

# ---------------------------------------------------------------------------
# 启动 wezterm-gui
# ---------------------------------------------------------------------------
$env:WEZTERM_CONFIG_FILE = $configPath
Write-Host "启动 $($bins.Gui) ..."
# 用 pwsh（若可用）保持与用户环境一致；-NoExit 使 fixture 结束后回到
# 交互 prompt，供「缩窄后输入」阶段使用
$shell = if (Get-Command pwsh -ErrorAction SilentlyContinue) { "pwsh" } else { "powershell" }
$proc = Start-Process -FilePath $bins.Gui -ArgumentList @(
    "start", "--class", "ResizeVerify", "--",
    $shell, "-NoProfile", "-NoExit", "-ExecutionPolicy", "Bypass", "-File", $fixturePath
) -PassThru

try {
    # 等窗口出现
    $deadline = (Get-Date).AddSeconds(30)
    while ($proc.MainWindowHandle -eq 0) {
        if ((Get-Date) -gt $deadline) { throw "等待 wezterm 窗口出现超时" }
        Start-Sleep -Milliseconds 200
        $proc.Refresh()
    }
    $hwnd = $proc.MainWindowHandle

    # 定位该实例的 gui socket，避免连到别的 wezterm
    $sockDir = Join-Path $env:USERPROFILE ".local\share\wezterm"
    $sock = $null
    $deadline = (Get-Date).AddSeconds(15)
    while (-not $sock) {
        $exact = Join-Path $sockDir "gui-sock-$($proc.Id)"
        if (Test-Path $exact) { $sock = $exact; break }
        $glob = Get-ChildItem $sockDir -Filter "gui-sock-*" -ErrorAction SilentlyContinue |
            Where-Object { $_.LastWriteTime -gt $proc.StartTime } |
            Sort-Object LastWriteTime | Select-Object -Last 1
        if ($glob) { $sock = $glob.FullName; break }
        if ((Get-Date) -gt $deadline) { throw "未找到 gui-sock（$sockDir）" }
        Start-Sleep -Milliseconds 300
    }
    $env:WEZTERM_UNIX_SOCKET = $sock
    Write-Host "已连接 socket: $sock"

    function Get-PaneText {
        # 优先带 scrollback；旧版本 cli 不支持 --start-line 时退回视口
        $text = & $bins.Cli cli get-text --start-line -1000 2>$null
        if ($LASTEXITCODE -ne 0) { $text = & $bins.Cli cli get-text }
        if ($LASTEXITCODE -ne 0) { throw "wezterm cli get-text 失败" }
        return ($text -join "`n")
    }

    # 等 fixture 就绪
    $deadline = (Get-Date).AddSeconds(20)
    while ((Get-PaneText) -notmatch "@@FIXTURE-READY@@") {
        if ((Get-Date) -gt $deadline) { throw "等待 fixture 就绪超时" }
        Start-Sleep -Milliseconds 400
    }
    Write-Host "fixture 就绪，开始 resize 序列"

    # ---------------------------------------------------------------------
    # 不变量检查
    # ---------------------------------------------------------------------
    $payloads = @()
    1..6 | ForEach-Object { $payloads += ("L{0:d2}|short-{0:d2}" -f $_) }
    $payloads += ("L07|" + (-join (0..11 | ForEach-Object { "SEG{0:d2}-abcdefghij." -f $_ })))
    $payloads += ("L08|" + ("中文宽字符换行校验一二三四五六七八九十" * 4))
    $payloads += "L09|TAIL-MARKER"

    function Test-Invariants {
        param([string]$Text, [string]$Stage)
        $violations = @()
        $lines = $Text -split "`r?`n"
        # 软换行的物理行在 get-text 输出里是独立行；拼接后检查 payload 完整性
        $joined = ($lines | ForEach-Object { $_.TrimEnd() }) -join ""
        $lastPos = -1
        foreach ($p in $payloads) {
            $hits = [regex]::Matches($joined, [regex]::Escape($p))
            if ($hits.Count -eq 0) {
                $head = $p.Substring(0, [Math]::Min(8, $p.Length))
                if ($joined.Contains($head)) { $violations += "断裂/截断: $p" }
                else { $violations += "丢失: $p" }
            } elseif ($hits.Count -gt 1) {
                $violations += "重复 $($hits.Count) 次: $p"
            } else {
                if ($hits[0].Index -lt $lastPos) { $violations += "错序: $p" }
                $lastPos = $hits[0].Index
            }
        }
        # TICK 行不重复、不回退
        $seen = @{}; $last = -1
        foreach ($m in [regex]::Matches($Text, 'TICK-(\d{3})')) {
            $n = [int]$m.Groups[1].Value
            if ($seen.ContainsKey($n)) { $violations += ("重复: TICK-{0:d3}" -f $n) }
            else {
                if ($n -lt $last) { $violations += ("错序: TICK-{0:d3}" -f $n) }
                $seen[$n] = $true; $last = $n
            }
        }
        return $violations
    }

    $report = @()
    $allOk = $true

    function Invoke-Stage {
        param([string]$Stage, [int]$W, [int]$H)
        if ($W -gt 0) {
            [RV]::SetWindowPos($hwnd, [IntPtr]::Zero, 0, 0, $W, $H, $SWP) | Out-Null
        }
        Start-Sleep -Milliseconds 800
        $text = Get-PaneText
        $v = Test-Invariants -Text $text -Stage $Stage
        $verdict = if ($v.Count -eq 0) { "PASS" } else { $script:allOk = $false; "FAIL" }
        Write-Host "[$verdict] $Stage (违规 $($v.Count) 项)"
        $script:report += "===== 阶段 [$Stage] $verdict ====="
        $v | ForEach-Object { $script:report += "  违规: $_" }
        if ($v.Count -gt 0) {
            $safe = ($Stage -replace '[^\w]', '_')
            $text | Set-Content -Encoding utf8 (Join-Path $OutDirFull "screen-$safe.txt")
        }
    }

    $rect = New-Object 'RV+RECT'
    [RV]::GetWindowRect($hwnd, [ref]$rect) | Out-Null
    $w0 = $rect.Right - $rect.Left; $h0 = $rect.Bottom - $rect.Top

    Invoke-Stage "初始(resize 前)" 0 0
    Invoke-Stage "缩窄" ([int]($w0 * 0.55)) $h0
    Invoke-Stage "放宽" ([int]($w0 * 1.4)) $h0
    Invoke-Stage "变矮" ([int]($w0 * 1.4)) ([int]($h0 * 0.5))
    Invoke-Stage "变高" ([int]($w0 * 1.4)) ([int]($h0 * 1.3))

    # 拖拽风暴：连续小步长
    for ($w = [int]($w0 * 1.4); $w -ge [int]($w0 * 0.5); $w -= 40) {
        [RV]::SetWindowPos($hwnd, [IntPtr]::Zero, 0, 0, $w, $h0, $SWP) | Out-Null
        Start-Sleep -Milliseconds 20
    }
    for ($w = [int]($w0 * 0.5); $w -le $w0; $w += 40) {
        [RV]::SetWindowPos($hwnd, [IntPtr]::Zero, 0, 0, $w, $h0, $SWP) | Out-Null
        Start-Sleep -Milliseconds 20
    }
    Invoke-Stage "风暴后回到原尺寸" $w0 $h0

    # ------------------------------------------------------------------
    # 输入阶段（用户步骤复现）：等 fixture 结束回到交互 prompt，
    # 缩到最窄后逐字符输入，检查模型内容中输入前缀保持连续。
    # get-text 反映的是模型内容 —— 此阶段 PASS 而肉眼仍见字符错位，
    # 即可断定问题在渲染层（画面与模型不一致）。
    # ------------------------------------------------------------------
    $deadline = (Get-Date).AddSeconds(40)
    while ((Get-PaneText) -notmatch "@@TICKS-DONE@@") {
        if ((Get-Date) -gt $deadline) { throw "等待 fixture 结束（TICKS-DONE）超时" }
        Start-Sleep -Milliseconds 500
    }
    Start-Sleep -Milliseconds 1500   # 等交互 prompt 出现

    [RV]::SetWindowPos($hwnd, [IntPtr]::Zero, 0, 0, [int]($w0 * 0.35), $h0, $SWP) | Out-Null
    Start-Sleep -Milliseconds 1000

    $typedCmd = "echo INPUT-MARKER-OK"
    foreach ($ch in $typedCmd.ToCharArray()) {
        & $bins.Cli cli send-text --no-paste "$ch" | Out-Null
        Start-Sleep -Milliseconds 100
    }
    Start-Sleep -Milliseconds 800
    $inputViolations = @()
    # 字符间允许空白（窄宽下输入行会软换行），但次序必须连续、无穿插
    $loosePattern = ($typedCmd.ToCharArray() | ForEach-Object { [regex]::Escape($_) }) -join '\s*'
    $text = Get-PaneText
    if ($text -notmatch $loosePattern) {
        $inputViolations += "输入前缀断裂: 模型内容中不存在连续的 [$typedCmd]"
    }
    & $bins.Cli cli send-text --no-paste "`r" | Out-Null
    Start-Sleep -Milliseconds 1500
    $text = Get-PaneText
    $hits = [regex]::Matches($text, "INPUT-MARKER-OK").Count
    if ($hits -lt 2) {
        $inputViolations += "命令执行后 INPUT-MARKER-OK 出现 $hits 次(期望 >=2: 回显+输出，输入被错乱执行)"
    }
    $verdict = if ($inputViolations.Count -eq 0) { "PASS" } else { $allOk = $false; "FAIL" }
    Write-Host "[$verdict] 缩窄后输入(模型内容) (违规 $($inputViolations.Count) 项)"
    $report += "===== 阶段 [缩窄后输入(模型内容)] $verdict ====="
    $inputViolations | ForEach-Object { $report += "  违规: $_" }
    if ($inputViolations.Count -gt 0) {
        $text | Set-Content -Encoding utf8 (Join-Path $OutDirFull "screen-input.txt")
    }

    Invoke-Stage "输入后回到原尺寸" $w0 $h0

    $report | Set-Content -Encoding utf8 (Join-Path $OutDirFull "report.txt")
    Write-Host "详细报告: $(Join-Path $OutDirFull 'report.txt')"
    if ($allOk) {
        Write-Host "== L3 结果: 全部通过 =="
        exit 0
    } else {
        Write-Host "== L3 结果: 存在违规（GUI 端到端链路检出内容异样）=="
        exit 1
    }
}
finally {
    if ($proc -and -not $proc.HasExited) { $proc.Kill() }
    Remove-Item Env:\WEZTERM_UNIX_SOCKET -ErrorAction SilentlyContinue
    Remove-Item Env:\WEZTERM_CONFIG_FILE -ErrorAction SilentlyContinue
}
