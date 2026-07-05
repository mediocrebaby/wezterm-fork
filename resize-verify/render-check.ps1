# Resize render check: reproduce user's steps with user's cursor config,
# capture screenshots (PrintWindow) + get-text at each step, so the
# screenshot can be compared against the model content to isolate
# renderer-level mismatches (which get-text can never show).
#
# Usage: .\resize-verify\render-check.ps1 [-TargetDir target\debug]

param(
    [string]$TargetDir = "target\debug",
    [string]$OutDir = "target\resize-verify\render"
)

$ErrorActionPreference = "Stop"
$RepoRoot = Split-Path -Parent $PSScriptRoot

$gui = Join-Path $RepoRoot "$TargetDir\wezterm-gui.exe"
$cli = Join-Path $RepoRoot "$TargetDir\wezterm-bigstack.exe"
if (-not (Test-Path $cli)) { $cli = Join-Path $RepoRoot "$TargetDir\wezterm.exe" }
if (-not ((Test-Path $gui) -and (Test-Path $cli))) { throw "binaries not found in $TargetDir" }

$OutDirFull = Join-Path $RepoRoot $OutDir
New-Item -ItemType Directory -Force $OutDirFull | Out-Null

# fixture: wide output then interactive prompt (pwsh -NoExit)
$fixturePath = Join-Path $OutDirFull "fixture.ps1"
@'
1..30 | ForEach-Object { "ROW-{0:d2}-" -f $_ + ("x" * 66) }
"@@FIXTURE-READY@@"
'@ | Set-Content -Encoding utf8 $fixturePath

# config mirroring the user's cursor/render settings
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
    [DllImport("user32.dll")] public static extern bool SetWindowPos(
        IntPtr hWnd, IntPtr after, int x, int y, int cx, int cy, uint flags);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(
        IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool PrintWindow(
        IntPtr hwnd, IntPtr hdc, uint flags);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr value);
    public struct RECT { public int Left, Top, Right, Bottom; }
}
"@
# PerMonitorV2: GetWindowRect/PrintWindow must operate in physical pixels,
# otherwise on scaled displays (e.g. 150%) the bitmap crops the window bottom.
[RC]::SetProcessDpiAwarenessContext([IntPtr]::new(-4)) | Out-Null
Add-Type -AssemblyName System.Drawing
$SWP = 0x0002 -bor 0x0004 -bor 0x0010

function Save-Shot {
    param([IntPtr]$Hwnd, [string]$Name)
    $rect = New-Object 'RC+RECT'
    [RC]::GetWindowRect($Hwnd, [ref]$rect) | Out-Null
    $w = $rect.Right - $rect.Left; $h = $rect.Bottom - $rect.Top
    $bmp = New-Object Drawing.Bitmap $w, $h
    $g = [Drawing.Graphics]::FromImage($bmp)
    $hdc = $g.GetHdc()
    # PW_RENDERFULLCONTENT = 2 (needed for GPU-composited windows)
    [RC]::PrintWindow($Hwnd, $hdc, 2) | Out-Null
    $g.ReleaseHdc($hdc)
    $g.Dispose()
    $bmp.Save((Join-Path $OutDirFull "$Name.png"))
    $bmp.Dispose()
}

$env:WEZTERM_CONFIG_FILE = $configPath
$proc = Start-Process -FilePath $gui -ArgumentList @(
    "start", "--class", "RenderCheck", "--",
    "pwsh", "-NoProfile", "-NoExit", "-ExecutionPolicy", "Bypass", "-File", $fixturePath
) -PassThru

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
    $rect = New-Object 'RC+RECT'
    [RC]::GetWindowRect($hwnd, [ref]$rect) | Out-Null
    $w0 = $rect.Right - $rect.Left; $h0 = $rect.Bottom - $rect.Top

    # drag-narrow to minimum, small steps like a real drag
    for ($w = $w0; $w -ge [int]($w0 * 0.30); $w -= 30) {
        [RC]::SetWindowPos($hwnd, [IntPtr]::Zero, 0, 0, $w, $h0, $SWP) | Out-Null
        Start-Sleep -Milliseconds 25
    }
    Start-Sleep -Milliseconds 1200
    Save-Shot $hwnd "1-after-narrow"
    Get-PaneText | Set-Content -Encoding utf8 (Join-Path $OutDirFull "1-after-narrow.txt")

    # type first half of the command
    foreach ($ch in "echo INPUT-".ToCharArray()) {
        & $cli cli send-text --no-paste "$ch" | Out-Null
        Start-Sleep -Milliseconds 90
    }
    Start-Sleep -Milliseconds 400
    Save-Shot $hwnd "2-mid-typing"
    Get-PaneText | Set-Content -Encoding utf8 (Join-Path $OutDirFull "2-mid-typing.txt")

    # finish typing
    foreach ($ch in "MARKER-OK".ToCharArray()) {
        & $cli cli send-text --no-paste "$ch" | Out-Null
        Start-Sleep -Milliseconds 90
    }
    Start-Sleep -Milliseconds 400
    Save-Shot $hwnd "3-typed-full"
    Get-PaneText | Set-Content -Encoding utf8 (Join-Path $OutDirFull "3-typed-full.txt")

    & $cli cli send-text --no-paste "`r" | Out-Null
    Start-Sleep -Milliseconds 1500
    Save-Shot $hwnd "4-after-enter"
    Get-PaneText | Set-Content -Encoding utf8 (Join-Path $OutDirFull "4-after-enter.txt")

    Write-Host "artifacts in $OutDirFull"
}
finally {
    if ($proc -and -not $proc.HasExited) { $proc.Kill() }
    Remove-Item Env:\WEZTERM_UNIX_SOCKET -ErrorAction SilentlyContinue
    Remove-Item Env:\WEZTERM_CONFIG_FILE -ErrorAction SilentlyContinue
}
