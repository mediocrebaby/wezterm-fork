# ls-check: verify that PREVIOUSLY PRINTED output survives narrow->widen.
#
# Steps (mirrors the user's report):
#   1. start wezterm-gui, run `ls` in a file-rich directory (repo root)
#   2. drag the right border to minimum width (real mouse, live-resize)
#   3. drag it back out to the original width
#   4. after each drag: screenshot + `cli get-text`, then assert that every
#      baseline output line still exists exactly once, in order (whitespace-
#      insensitive so soft-wrap changes don't false-positive)
#
# Exit 0 = all stages pass; 1 = content violations found.
# Do NOT touch mouse/keyboard while this runs (~40s).

param(
    [string]$TargetDir = "target\debug",
    [string]$OutDir = "target\resize-verify\ls-check",
    # 用户真实环境模式：加载用户的 wezterm 配置与 pwsh profile
    # （oh-my-posh prompt + lsd 图标输出），复现用户实际使用形态
    [switch]$UserEnv,
    # 显式指定 gui 可执行文件（绕开被运行中实例锁定的路径）
    [string]$GuiPath = "",
    # 放宽后追加滚轮向上/回底检查（截图供人工比对渲染与模型）
    [switch]$ScrollCheck
)

$ErrorActionPreference = "Stop"
$RepoRoot = Split-Path -Parent $PSScriptRoot

# prefer the freshly linked copy when wezterm-gui.exe is locked by a live instance
if ($GuiPath) {
    $gui = $GuiPath
} else {
    $gui = Join-Path $RepoRoot "$TargetDir\wezterm-gui-fix.exe"
    if (-not (Test-Path $gui)) { $gui = Join-Path $RepoRoot "$TargetDir\wezterm-gui.exe" }
}
$cli = Join-Path $RepoRoot "$TargetDir\wezterm-bigstack.exe"
if (-not (Test-Path $cli)) { $cli = Join-Path $RepoRoot "$TargetDir\wezterm.exe" }
if (-not ((Test-Path $gui) -and (Test-Path $cli))) { throw "binaries not found in $TargetDir" }

$OutDirFull = Join-Path $RepoRoot $OutDir
New-Item -ItemType Directory -Force $OutDirFull | Out-Null

$configPath = Join-Path $OutDirFull "wezterm-ls.lua"
@'
local wezterm = require 'wezterm'
return {
  check_for_updates = false,
  window_close_confirmation = 'NeverPrompt',
  initial_cols = 100,
  initial_rows = 34,
  front_end = 'WebGpu',
  webgpu_power_preference = 'HighPerformance',
  max_fps = 120,
  default_cursor_style = 'SteadyBar',
  force_reverse_video_cursor = true,
  cursor_smear_duration_ms = 150,
  cursor_thickness = 2,
  cursor_trail_size = 1.0,
}
'@ | Set-Content -Encoding ascii $configPath

Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class LC {
    [DllImport("user32.dll")] public static extern bool GetWindowRect(
        IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool PrintWindow(
        IntPtr hwnd, IntPtr hdc, uint flags);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr value);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(
        uint flags, int dx, int dy, uint data, UIntPtr extra);
    public struct RECT { public int Left, Top, Right, Bottom; }
}
"@
[LC]::SetProcessDpiAwarenessContext([IntPtr]::new(-4)) | Out-Null
Add-Type -AssemblyName System.Drawing

$MOUSEEVENTF_LEFTDOWN = 0x0002
$MOUSEEVENTF_LEFTUP = 0x0004
$MOUSEEVENTF_WHEEL = 0x0800

function Send-Wheel {
    param([IntPtr]$Hwnd, [int]$Notches)  # 正=向上
    $rect = New-Object 'LC+RECT'
    [LC]::GetWindowRect($Hwnd, [ref]$rect) | Out-Null
    [LC]::SetCursorPos([int](($rect.Left + $rect.Right) / 2), [int](($rect.Top + $rect.Bottom) / 2)) | Out-Null
    Start-Sleep -Milliseconds 100
    # WHEEL delta 是有符号值塞进 uint32：-120 = 0xFFFFFF88
    $step = if ($Notches -gt 0) { [uint32]120 } else { [uint32]4294967176 }
    for ($i = 0; $i -lt [Math]::Abs($Notches); $i++) {
        [LC]::mouse_event($MOUSEEVENTF_WHEEL, 0, 0, $step, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 120
    }
    Start-Sleep -Milliseconds 400
}

function Save-Shot {
    param([IntPtr]$Hwnd, [string]$Name)
    $rect = New-Object 'LC+RECT'
    [LC]::GetWindowRect($Hwnd, [ref]$rect) | Out-Null
    $w = $rect.Right - $rect.Left; $h = $rect.Bottom - $rect.Top
    $bmp = New-Object Drawing.Bitmap $w, $h
    $g = [Drawing.Graphics]::FromImage($bmp)
    $hdc = $g.GetHdc()
    [LC]::PrintWindow($Hwnd, $hdc, 2) | Out-Null
    $g.ReleaseHdc($hdc)
    $g.Dispose()
    $bmp.Save((Join-Path $OutDirFull "$Name.png"))
    $bmp.Dispose()
}

function Invoke-Drag {
    param([IntPtr]$Hwnd, [int]$TargetX)
    $rect = New-Object 'LC+RECT'
    [LC]::GetWindowRect($Hwnd, [ref]$rect) | Out-Null
    $midY = [int](($rect.Top + $rect.Bottom) / 2)
    $x = $rect.Right - 3
    [LC]::SetCursorPos($x, $midY) | Out-Null
    Start-Sleep -Milliseconds 150
    [LC]::mouse_event($MOUSEEVENTF_LEFTDOWN, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 150
    $step = if ($TargetX -gt $x) { 25 } else { -25 }
    while ([Math]::Abs($TargetX - $x) -gt 25) {
        $x += $step
        [LC]::SetCursorPos($x, $midY) | Out-Null
        Start-Sleep -Milliseconds 15
    }
    [LC]::SetCursorPos($TargetX, $midY) | Out-Null
    Start-Sleep -Milliseconds 150
    [LC]::mouse_event($MOUSEEVENTF_LEFTUP, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 1200
}

# whitespace-insensitive pattern: non-blank chars of the line joined by \s*
function Get-LoosePattern {
    param([string]$Line)
    $chars = ($Line -replace '\s', '').ToCharArray()
    return (($chars | ForEach-Object { [regex]::Escape($_) }) -join '\s*')
}

if ($UserEnv) {
    # 用户配置 + 默认 shell（pwsh 带 profile: oh-my-posh + lsd 别名），
    # cwd 用家目录（文件多、含图标丰富的条目）
    Remove-Item Env:\WEZTERM_CONFIG_FILE -ErrorAction SilentlyContinue
    $startArgs = @("start", "--class", "LsCheckUser", "--cwd", $env:USERPROFILE)
} else {
    $env:WEZTERM_CONFIG_FILE = $configPath
    $startArgs = @(
        "start", "--class", "LsCheck", "--cwd", $RepoRoot, "--",
        "pwsh", "-NoProfile", "-NoLogo", "-NoExit"
    )
}
$proc = Start-Process -FilePath $gui -ArgumentList $startArgs -PassThru `
    -RedirectStandardError (Join-Path $OutDirFull "gui-stderr.log")

try {
    $deadline = (Get-Date).AddSeconds(30)
    while ($proc.MainWindowHandle -eq 0) {
        if ((Get-Date) -gt $deadline) { throw "window timeout" }
        Start-Sleep -Milliseconds 200
        $proc.Refresh()
    }
    $hwnd = $proc.MainWindowHandle

    $sockDir = Join-Path $env:USERPROFILE ".local\share\wezterm"
    $sock = $null
    $deadline = (Get-Date).AddSeconds(15)
    while (-not $sock) {
        $exact = Join-Path $sockDir "gui-sock-$($proc.Id)"
        if (Test-Path $exact) { $sock = $exact; break }
        if ((Get-Date) -gt $deadline) { throw "gui-sock timeout" }
        Start-Sleep -Milliseconds 300
    }
    $env:WEZTERM_UNIX_SOCKET = $sock

    function Get-PaneText {
        # 视口内容（用于等待 prompt 等交互判定）
        $t = & $cli cli get-text 2>$null
        if ($LASTEXITCODE -ne 0) { throw "get-text failed" }
        return ($t -join "`n")
    }

    function Get-BufferText {
        # 含 scrollback 的完整缓冲（用于内容完整性校验——缩窄后行数
        # 膨胀，内容滚进 scrollback 是正常现象而非丢失）
        $t = & $cli cli get-text --start-line -4000 2>$null
        if ($LASTEXITCODE -ne 0) { return (Get-PaneText) }
        return ($t -join "`n")
    }

    # wait for the interactive prompt
    # UserEnv: oh-my-posh(robbyrussell) 的箭头 prompt；否则裸 pwsh 的 PS >
    $promptPattern = if ($UserEnv) { [string][char]0x2192 + "|" + [string][char]0x279C } else { "^PS .*>" }
    $deadline = (Get-Date).AddSeconds(25)
    while ((Get-PaneText) -notmatch $promptPattern) {
        if ((Get-Date) -gt $deadline) { throw "prompt timeout" }
        Start-Sleep -Milliseconds 400
    }
    Start-Sleep -Milliseconds 800

    # run `ls` (pwsh: Get-ChildItem) in the repo root -- real, wide output
    & $cli cli send-text --no-paste "ls" | Out-Null
    Start-Sleep -Milliseconds 200
    & $cli cli send-text --no-paste "`r" | Out-Null

    # wait until output settles (two identical reads in a row)
    $prev = ""
    $deadline = (Get-Date).AddSeconds(20)
    while ($true) {
        Start-Sleep -Milliseconds 700
        $cur = Get-PaneText
        # 输出结束的标志：内容稳定，且最后一个非空行是新的 prompt
        $lastLine = (($cur -split "`n") | Where-Object { $_.Trim() } | Select-Object -Last 1)
        if ($cur -eq $prev -and $lastLine -match $promptPattern) { break }
        if ((Get-Date) -gt $deadline) { throw "ls output timeout" }
        $prev = $cur
    }

    [LC]::SetForegroundWindow($hwnd) | Out-Null
    Start-Sleep -Milliseconds 300

    $baselineText = Get-BufferText
    $baselineText | Set-Content -Encoding utf8 (Join-Path $OutDirFull "0-baseline.txt")
    Save-Shot $hwnd "0-baseline"

    # baseline payloads: non-blank output lines (>= 6 non-space chars)
    $payloads = @()
    foreach ($line in ($baselineText -split "`n")) {
        $t = $line.Trim()
        if (($t -replace '\s', '').Length -ge 6) { $payloads += $t }
    }
    Write-Host "基线内容行: $($payloads.Count) 行"
    $vBase = ($baselineText -replace '\s', '')
    # 基线视口的最后 5 个非空行（含 prompt）——放宽回原尺寸后这些行
    # 必须仍显示在视口内
    $vpTail = @((Get-PaneText) -split "`n" | Where-Object { $_.Trim() } | Select-Object -Last 5)

    $rect0 = New-Object 'LC+RECT'
    [LC]::GetWindowRect($hwnd, [ref]$rect0) | Out-Null
    $w0 = $rect0.Right - $rect0.Left

    $allOk = $true
    $report = @()

    function Test-Stage {
        param([string]$Stage, [bool]$CheckViewport = $true)
        $text = Get-BufferText
        $text | Set-Content -Encoding utf8 (Join-Path $OutDirFull "$Stage.txt")
        Save-Shot $hwnd $Stage
        $violations = @()
        # 判定 1（丢失/乱序/拼行）：整个缓冲去空白归一化后，基线全文
        # 必须作为连续子串保留 —— rewrap 只改变换行位置，不改变字符流。
        $vNow = ($text -replace '\s', '')
        if (-not $vNow.Contains($script:vBase)) {
            # 定位第一处对不上的基线行，方便归因
            foreach ($p in $script:payloads) {
                $pn = ($p -replace '\s', '')
                if (-not $vNow.Contains($pn)) {
                    $violations += "内容损坏: 基线行 [$p] 在缓冲中不再连续存在"
                }
            }
            if ($violations.Count -eq 0) {
                $violations += "内容乱序/拼行: 每一行单独可寻，但整体顺序被破坏"
            }
        }
        # 判定 2（重复/残影）：缓冲字符量对基线的膨胀不应超过少量
        # prompt 重绘的量级
        $growth = $vNow.Length - $script:vBase.Length
        if ($growth -gt 200) {
            $violations += "内容膨胀 $growth 字符（疑似重复/残影行）"
        }
        # 判定 3（视口显示）：基线视口尾部的输出行必须仍显示在视口内。
        # 旧 bug 形态：缩→放后视口被垫行占据、内容整体被顶进 scrollback
        # —— 缓冲完整但用户看到「输出不见了」。
        # 只在「回到原尺寸」的阶段检查：缩窄状态下内容 wrap 膨胀超过
        # 视口高度、早期行滚出视口，是任何终端的正常行为。
        $vpRaw = Get-PaneText
        $vpRaw | Set-Content -Encoding utf8 (Join-Path $OutDirFull "$Stage-viewport.txt")
        if ($CheckViewport) {
            $vpNow = ($vpRaw -replace '\s', '')
            foreach ($p in $script:vpTail) {
                if (-not $vpNow.Contains(($p -replace '\s', ''))) {
                    $violations += "视口显示: 基线尾部行 [$p] 被顶出视口"
                }
            }
        }
        $verdict = if ($violations.Count -eq 0) { "PASS" } else { $script:allOk = $false; "FAIL" }
        Write-Host "[$verdict] $Stage (违规 $($violations.Count) 项)"
        $script:report += "===== [$Stage] $verdict ====="
        $violations | ForEach-Object { $script:report += "  违规: $_" }
    }

    # 1. drag-narrow to ~30% width（缩窄状态只查内容完整性，内容滚出
    #    视口是正常行为）
    Invoke-Drag $hwnd ($rect0.Left + [int]($w0 * 0.30))
    Test-Stage "1-narrow" $false

    # 2. drag back out to the original width（回原尺寸后内容必须回到视口）
    Invoke-Drag $hwnd ($rect0.Left + $w0)
    Test-Stage "2-widen" $true

    # 3.（可选）滚动检查：滚轮向上翻 scrollback，截图供比对渲染与模型；
    #    再滚回底部确认视口恢复
    if ($ScrollCheck) {
        Send-Wheel $hwnd 8
        Save-Shot $hwnd "3-scrolled-up"
        Get-BufferText | Set-Content -Encoding utf8 (Join-Path $OutDirFull "3-scrolled-up-buffer.txt")
        Send-Wheel $hwnd -12
        Save-Shot $hwnd "4-scrolled-back"
        Get-PaneText | Set-Content -Encoding utf8 (Join-Path $OutDirFull "4-scrolled-back-viewport.txt")
    }

    $report | Set-Content -Encoding utf8 (Join-Path $OutDirFull "report.txt")
    if ($allOk) {
        Write-Host "== ls-check: 全部通过（已输出内容在缩窄/放宽后完好）=="
        exit 0
    } else {
        Write-Host "== ls-check: 检出内容异样，详见 $OutDirFull\report.txt 及截图 =="
        exit 1
    }
}
finally {
    if ($proc -and -not $proc.HasExited) { $proc.Kill() }
    Remove-Item Env:\WEZTERM_UNIX_SOCKET -ErrorAction SilentlyContinue
    Remove-Item Env:\WEZTERM_CONFIG_FILE -ErrorAction SilentlyContinue
}
