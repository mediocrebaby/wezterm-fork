# Real-interaction render check: narrow the window by DRAGGING its right
# border with synthesized mouse input (triggers WM_ENTERSIZEMOVE ->
# live-resize path), then type via SendKeys (real keyboard event chain,
# IME involved). Screenshots + get-text at each step.
#
# Do NOT touch mouse/keyboard while this runs (~30s).

param(
    [string]$TargetDir = "target\debug",
    [string]$OutDir = "target\resize-verify\render-real"
)

$ErrorActionPreference = "Stop"
$RepoRoot = Split-Path -Parent $PSScriptRoot

# wezterm-gui-fix.exe: 当 wezterm-gui.exe 被正在运行的实例锁定无法覆盖时,
# 从 target\debug\deps\wezterm_gui.exe 复制出来的新构建
$gui = Join-Path $RepoRoot "$TargetDir\wezterm-gui-fix.exe"
if (-not (Test-Path $gui)) { $gui = Join-Path $RepoRoot "$TargetDir\wezterm-gui.exe" }
$cli = Join-Path $RepoRoot "$TargetDir\wezterm-bigstack.exe"
if (-not (Test-Path $cli)) { $cli = Join-Path $RepoRoot "$TargetDir\wezterm.exe" }
if (-not ((Test-Path $gui) -and (Test-Path $cli))) { throw "binaries not found in $TargetDir" }

$OutDirFull = Join-Path $RepoRoot $OutDir
New-Item -ItemType Directory -Force $OutDirFull | Out-Null

$fixturePath = Join-Path $OutDirFull "fixture.ps1"
@'
1..30 | ForEach-Object { "ROW-{0:d2}-" -f $_ + ("x" * 66) }
"@@FIXTURE-READY@@"
'@ | Set-Content -Encoding utf8 $fixturePath

$configPath = Join-Path $OutDirFull "wezterm-render.lua"
@'
local wezterm = require 'wezterm'
return {
  check_for_updates = false,
  window_close_confirmation = 'NeverPrompt',
  initial_cols = 100,
  initial_rows = 30,
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
public static class RC {
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
[RC]::SetProcessDpiAwarenessContext([IntPtr]::new(-4)) | Out-Null
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms

$MOUSEEVENTF_LEFTDOWN = 0x0002
$MOUSEEVENTF_LEFTUP = 0x0004

function Save-Shot {
    param([IntPtr]$Hwnd, [string]$Name)
    $rect = New-Object 'RC+RECT'
    [RC]::GetWindowRect($Hwnd, [ref]$rect) | Out-Null
    $w = $rect.Right - $rect.Left; $h = $rect.Bottom - $rect.Top
    $bmp = New-Object Drawing.Bitmap $w, $h
    $g = [Drawing.Graphics]::FromImage($bmp)
    $hdc = $g.GetHdc()
    [RC]::PrintWindow($Hwnd, $hdc, 2) | Out-Null
    $g.ReleaseHdc($hdc)
    $g.Dispose()
    $bmp.Save((Join-Path $OutDirFull "$Name.png"))
    $bmp.Dispose()
}

$env:WEZTERM_CONFIG_FILE = $configPath
# 抓模型层 resize 序列（Screen::resize 的 debug 日志走 stderr）
$env:WEZTERM_LOG = "wezterm_term::screen=debug"
$proc = Start-Process -FilePath $gui -ArgumentList @(
    "start", "--class", "RenderCheckReal", "--",
    "pwsh", "-NoProfile", "-NoExit", "-ExecutionPolicy", "Bypass", "-File", $fixturePath
) -PassThru -RedirectStandardError (Join-Path $OutDirFull "gui-stderr.log")

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
        $t = & $cli cli get-text 2>$null
        if ($LASTEXITCODE -ne 0) { throw "get-text failed" }
        return ($t -join "`n")
    }

    $deadline = (Get-Date).AddSeconds(25)
    while ((Get-PaneText) -notmatch "@@FIXTURE-READY@@") {
        if ((Get-Date) -gt $deadline) { throw "fixture timeout" }
        Start-Sleep -Milliseconds 400
    }
    Start-Sleep -Milliseconds 1500

    [RC]::SetForegroundWindow($hwnd) | Out-Null
    Start-Sleep -Milliseconds 300

    # --- real drag on the right border: WM_ENTERSIZEMOVE -> live resize ---
    $rect = New-Object 'RC+RECT'
    [RC]::GetWindowRect($hwnd, [ref]$rect) | Out-Null
    $w0 = $rect.Right - $rect.Left
    $midY = [int](($rect.Top + $rect.Bottom) / 2)
    $edgeX = $rect.Right - 3
    $targetX = $rect.Left + [int]($w0 * 0.30)

    # drag 1: narrow to minimum
    [RC]::SetCursorPos($edgeX, $midY) | Out-Null
    Start-Sleep -Milliseconds 150
    [RC]::mouse_event($MOUSEEVENTF_LEFTDOWN, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 150
    for ($x = $edgeX; $x -gt $targetX; $x -= 25) {
        [RC]::SetCursorPos($x, $midY) | Out-Null
        Start-Sleep -Milliseconds 15
    }
    [RC]::SetCursorPos($targetX, $midY) | Out-Null
    Start-Sleep -Milliseconds 150
    [RC]::mouse_event($MOUSEEVENTF_LEFTUP, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 1000

    Save-Shot $hwnd "1-after-narrow"
    Get-PaneText | Set-Content -Encoding utf8 (Join-Path $OutDirFull "1-after-narrow.txt")

    # drag 2: widen back to a comfortable width (user's actual step)
    $rect2 = New-Object 'RC+RECT'
    [RC]::GetWindowRect($hwnd, [ref]$rect2) | Out-Null
    $midY2 = [int](($rect2.Top + $rect2.Bottom) / 2)
    $edgeX2 = $rect2.Right - 3
    $widenX = $rect2.Left + [int]($w0 * 0.70)
    [RC]::SetCursorPos($edgeX2, $midY2) | Out-Null
    Start-Sleep -Milliseconds 150
    [RC]::mouse_event($MOUSEEVENTF_LEFTDOWN, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 150
    for ($x = $edgeX2; $x -lt $widenX; $x += 25) {
        [RC]::SetCursorPos($x, $midY2) | Out-Null
        Start-Sleep -Milliseconds 15
    }
    [RC]::SetCursorPos($widenX, $midY2) | Out-Null
    Start-Sleep -Milliseconds 150
    [RC]::mouse_event($MOUSEEVENTF_LEFTUP, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 1000

    Save-Shot $hwnd "1b-after-widen"
    Get-PaneText | Set-Content -Encoding utf8 (Join-Path $OutDirFull "1b-after-widen.txt")

    # --- type into the pane. send-text bypasses the host IME (SendKeys with
    # a CJK IME active gets composed into hanzi, corrupting the test text;
    # IME-chain positioning was verified manually/by screenshot) ---
    [RC]::SetForegroundWindow($hwnd) | Out-Null
    Start-Sleep -Milliseconds 300
    foreach ($ch in "echo INPUT-MARKER-OK".ToCharArray()) {
        & $cli cli send-text --no-paste "$ch" | Out-Null
        Start-Sleep -Milliseconds 90
    }
    Start-Sleep -Milliseconds 500
    Save-Shot $hwnd "2-typed"
    Get-PaneText | Set-Content -Encoding utf8 (Join-Path $OutDirFull "2-typed.txt")

    & $cli cli send-text --no-paste "`r" | Out-Null
    Start-Sleep -Milliseconds 1500
    Save-Shot $hwnd "3-after-enter"
    Get-PaneText | Set-Content -Encoding utf8 (Join-Path $OutDirFull "3-after-enter.txt")

    Write-Host "artifacts in $OutDirFull"
}
finally {
    if ($proc -and -not $proc.HasExited) { $proc.Kill() }
    Remove-Item Env:\WEZTERM_UNIX_SOCKET -ErrorAction SilentlyContinue
    Remove-Item Env:\WEZTERM_CONFIG_FILE -ErrorAction SilentlyContinue
}
