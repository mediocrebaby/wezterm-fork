[CmdletBinding()]
param(
    [string]$WezTermGuiPath,
    [string]$OutputDir,
    [string]$OpenPath,
    [int]$PhaseDelayMs = 140,
    [switch]$LeaveGuiRunning
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms

Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;

public static class CursorSmearHarnessNative {
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct POINT {
        public int X;
        public int Y;
    }

    [DllImport("user32.dll")]
    public static extern bool GetClientRect(IntPtr hWnd, out RECT lpRect);

    [DllImport("user32.dll")]
    public static extern bool GetWindowRect(IntPtr hWnd, out RECT lpRect);

    [DllImport("user32.dll")]
    public static extern bool ClientToScreen(IntPtr hWnd, ref POINT lpPoint);

    [DllImport("user32.dll")]
    public static extern bool PrintWindow(IntPtr hwnd, IntPtr hdcBlt, uint nFlags);

    [DllImport("user32.dll")]
    public static extern bool ShowWindowAsync(IntPtr hWnd, int nCmdShow);

    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr hWnd);

    [DllImport("user32.dll")]
    public static extern IntPtr GetForegroundWindow();
}
"@

function New-OutputDirectory {
    param(
        [string]$BasePath
    )

    if ($BasePath) {
        New-Item -ItemType Directory -Force -Path $BasePath | Out-Null
        return (Resolve-Path $BasePath).Path
    }

    $timestamp = Get-Date -Format 'yyyyMMdd-HHmmss'
    $path = Join-Path $scriptRoot "..\..\target\cursor-smear-focus-harness\$timestamp"
    New-Item -ItemType Directory -Force -Path $path | Out-Null
    (Resolve-Path $path).Path
}

function Write-ResultFile {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,
        [Parameter(Mandatory = $true)]
        [object]$Value
    )

    $Value | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $Path
}

function Wait-ForFile {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,
        [int]$TimeoutSeconds = 15
    )

    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        if (Test-Path -LiteralPath $Path) {
            return
        }
        Start-Sleep -Milliseconds 50
    }

    throw "Timed out waiting for file $Path"
}

function Get-FocusEvents {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path
    )

    if (-not (Test-Path -LiteralPath $Path)) {
        return ,@()
    }

    $events = @()
    foreach ($line in Get-Content -LiteralPath $Path) {
        if ([string]::IsNullOrWhiteSpace($line)) {
            continue
        }
        $events += ($line | ConvertFrom-Json)
    }
    return ,$events
}

function Wait-FocusEvent {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,
        [Parameter(Mandatory = $true)]
        [bool]$Focused,
        [int]$AfterCount = 0,
        [int]$TimeoutSeconds = 15
    )

    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        $events = Get-FocusEvents -Path $Path
        for ($i = $AfterCount; $i -lt $events.Count; $i++) {
            if ([bool]$events[$i].focused -eq $Focused) {
                return @{
                    Event = $events[$i]
                    Count = $events.Count
                }
            }
        }
        Start-Sleep -Milliseconds 50
    }

    throw "Timed out waiting for focus=$Focused in $Path"
}

function Wait-MainWindowHandle {
    param(
        [Parameter(Mandatory = $true)]
        [System.Diagnostics.Process]$Process,
        [int]$TimeoutSeconds = 15
    )

    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        $Process.Refresh()
        if ($Process.MainWindowHandle -ne 0) {
            return [IntPtr]$Process.MainWindowHandle
        }
        Start-Sleep -Milliseconds 100
    }

    throw "Timed out waiting for a main window from PID $($Process.Id)"
}

function Get-ClientRectangle {
    param(
        [Parameter(Mandatory = $true)]
        [IntPtr]$Hwnd
    )

    $clientRect = New-Object CursorSmearHarnessNative+RECT
    if (-not [CursorSmearHarnessNative]::GetClientRect($Hwnd, [ref]$clientRect)) {
        throw "GetClientRect failed for HWND $Hwnd"
    }

    $origin = New-Object CursorSmearHarnessNative+POINT
    if (-not [CursorSmearHarnessNative]::ClientToScreen($Hwnd, [ref]$origin)) {
        throw "ClientToScreen failed for HWND $Hwnd"
    }

    [pscustomobject]@{
        X = $origin.X
        Y = $origin.Y
        Width = $clientRect.Right - $clientRect.Left
        Height = $clientRect.Bottom - $clientRect.Top
    }
}

function Get-WindowRectangle {
    param(
        [Parameter(Mandatory = $true)]
        [IntPtr]$Hwnd
    )

    $windowRect = New-Object CursorSmearHarnessNative+RECT
    if (-not [CursorSmearHarnessNative]::GetWindowRect($Hwnd, [ref]$windowRect)) {
        throw "GetWindowRect failed for HWND $Hwnd"
    }

    [pscustomobject]@{
        X = $windowRect.Left
        Y = $windowRect.Top
        Width = $windowRect.Right - $windowRect.Left
        Height = $windowRect.Bottom - $windowRect.Top
    }
}

function Capture-BitmapFromScreen {
    param(
        [Parameter(Mandatory = $true)]
        [object]$Rect
    )

    $bitmap = New-Object System.Drawing.Bitmap $Rect.Width, $Rect.Height
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.CopyFromScreen(
            [System.Drawing.Point]::new($Rect.X, $Rect.Y),
            [System.Drawing.Point]::new(0, 0),
            [System.Drawing.Size]::new($Rect.Width, $Rect.Height)
        )
        return $bitmap
    }
    finally {
        $graphics.Dispose()
    }
}

function Capture-BitmapViaPrintWindow {
    param(
        [Parameter(Mandatory = $true)]
        [IntPtr]$Hwnd
    )

    $windowRect = Get-WindowRectangle -Hwnd $Hwnd
    $clientRect = Get-ClientRectangle -Hwnd $Hwnd
    $fullBitmap = New-Object System.Drawing.Bitmap $windowRect.Width, $windowRect.Height
    $graphics = [System.Drawing.Graphics]::FromImage($fullBitmap)
    $hdc = $graphics.GetHdc()

    try {
        $ok = [CursorSmearHarnessNative]::PrintWindow($Hwnd, $hdc, 2)
    }
    finally {
        $graphics.ReleaseHdc($hdc)
        $graphics.Dispose()
    }

    if (-not $ok) {
        $fullBitmap.Dispose()
        return $null
    }

    $cropX = $clientRect.X - $windowRect.X
    $cropY = $clientRect.Y - $windowRect.Y
    $cropped = New-Object System.Drawing.Bitmap $clientRect.Width, $clientRect.Height
    $cropGraphics = [System.Drawing.Graphics]::FromImage($cropped)

    try {
        $cropGraphics.DrawImage(
            $fullBitmap,
            [System.Drawing.Rectangle]::new(0, 0, $clientRect.Width, $clientRect.Height),
            [System.Drawing.Rectangle]::new($cropX, $cropY, $clientRect.Width, $clientRect.Height),
            [System.Drawing.GraphicsUnit]::Pixel
        )
        return $cropped
    }
    finally {
        $cropGraphics.Dispose()
        $fullBitmap.Dispose()
    }
}

function Get-BrightPixelBounds {
    param(
        [Parameter(Mandatory = $true)]
        [System.Drawing.Bitmap]$Bitmap,
        [int]$Threshold = 220,
        [int]$IgnoreTopPixels = 32
    )

    $minX = $Bitmap.Width
    $minY = $Bitmap.Height
    $maxX = -1
    $maxY = -1
    $count = 0

    for ($y = $IgnoreTopPixels; $y -lt $Bitmap.Height; $y++) {
        for ($x = 0; $x -lt $Bitmap.Width; $x++) {
            $pixel = $Bitmap.GetPixel($x, $y)
            if ($pixel.R -ge $Threshold -and $pixel.G -ge $Threshold -and $pixel.B -ge $Threshold) {
                $count++
                if ($x -lt $minX) { $minX = $x }
                if ($y -lt $minY) { $minY = $y }
                if ($x -gt $maxX) { $maxX = $x }
                if ($y -gt $maxY) { $maxY = $y }
            }
        }
    }

    if ($count -eq 0) {
        return [pscustomobject]@{
            BrightPixelCount = 0
            Left = $null
            Top = $null
            Right = $null
            Bottom = $null
            Width = 0
            Height = 0
        }
    }

    [pscustomobject]@{
        BrightPixelCount = $count
        Left = $minX
        Top = $minY
        Right = $maxX
        Bottom = $maxY
        Width = $maxX - $minX + 1
        Height = $maxY - $minY + 1
    }
}

function Save-PhaseCapture {
    param(
        [Parameter(Mandatory = $true)]
        [IntPtr]$Hwnd,
        [Parameter(Mandatory = $true)]
        [string]$Name,
        [Parameter(Mandatory = $true)]
        [string]$OutputDir
    )

    $pngPath = Join-Path $OutputDir "$Name.png"

    $bitmap = Capture-BitmapViaPrintWindow -Hwnd $Hwnd
    $captureMode = 'print-window'

    if ($null -eq $bitmap) {
        $bitmap = Capture-BitmapFromScreen -Rect (Get-ClientRectangle -Hwnd $Hwnd)
        $captureMode = 'copy-from-screen'
    }

    $bounds = Get-BrightPixelBounds -Bitmap $bitmap
    if ($bounds.BrightPixelCount -eq 0 -and $captureMode -eq 'print-window') {
        $bitmap.Dispose()
        $bitmap = Capture-BitmapFromScreen -Rect (Get-ClientRectangle -Hwnd $Hwnd)
        $captureMode = 'copy-from-screen'
        $bounds = Get-BrightPixelBounds -Bitmap $bitmap
    }

    try {
        $bitmap.Save($pngPath, [System.Drawing.Imaging.ImageFormat]::Png)
    }
    finally {
        $bitmap.Dispose()
    }

    [pscustomobject]@{
        Phase = $Name
        CaptureMode = $captureMode
        ImagePath = $pngPath
        Bounds = $bounds
    }
}

function Focus-WezTermWindow {
    param(
        [Parameter(Mandatory = $true)]
        [IntPtr]$Hwnd,
        [Parameter(Mandatory = $true)]
        [int]$ProcessId
    )

    [CursorSmearHarnessNative]::ShowWindowAsync($Hwnd, 9) | Out-Null
    $shell = New-Object -ComObject WScript.Shell
    $null = $shell.AppActivate($ProcessId)
    [CursorSmearHarnessNative]::SetForegroundWindow($Hwnd) | Out-Null
    Start-Sleep -Milliseconds 120

    if ([CursorSmearHarnessNative]::GetForegroundWindow() -eq $Hwnd) {
        return
    }

    $shell.SendKeys('%')
    Start-Sleep -Milliseconds 40
    [CursorSmearHarnessNative]::SetForegroundWindow($Hwnd) | Out-Null
    Start-Sleep -Milliseconds 120

    if ([CursorSmearHarnessNative]::GetForegroundWindow() -ne $Hwnd) {
        throw "Failed to restore focus to WezTerm window"
    }
}

function Get-HarnessSummary {
    param(
        [Parameter(Mandatory = $true)]
        [object]$PhaseA,
        [Parameter(Mandatory = $true)]
        [object]$PhaseB,
        [Parameter(Mandatory = $true)]
        [object]$PhaseC
    )

    $aWidth = [int]$PhaseA.Bounds.Width
    $bWidth = [int]$PhaseB.Bounds.Width
    $cWidth = [int]$PhaseC.Bounds.Width

    $smearWidthThreshold = 120
    $resetWidthThreshold = 120

    $phaseAOk = $PhaseA.Bounds.BrightPixelCount -gt 0 -and $aWidth -ge $smearWidthThreshold
    $phaseBOk = $PhaseB.Bounds.BrightPixelCount -gt 0 -and $bWidth -le $resetWidthThreshold
    $phaseCOk = $PhaseC.Bounds.BrightPixelCount -gt 0 -and $cWidth -ge $smearWidthThreshold

    [pscustomobject]@{
        Passed = ($phaseAOk -and $phaseBOk -and $phaseCOk)
        Checks = [pscustomobject]@{
            phase_a_has_active_smear = $phaseAOk
            phase_b_reset_after_focus_loss = $phaseBOk
            phase_c_smear_returns_after_refocus = $phaseCOk
        }
        Thresholds = [pscustomobject]@{
            smear_width = $smearWidthThreshold
            reset_width = $resetWidthThreshold
        }
        Widths = [pscustomobject]@{
            phase_a = $aWidth
            phase_b = $bWidth
            phase_c = $cWidth
        }
    }
}

$scriptRoot = if ($PSScriptRoot) { $PSScriptRoot } else { Split-Path -Parent $PSCommandPath }
$repoRoot = (Resolve-Path (Join-Path $scriptRoot '..\..')).Path

if (-not $WezTermGuiPath) {
    $WezTermGuiPath = Join-Path $repoRoot 'target\debug\wezterm-gui.exe'
}
if (-not $OpenPath) {
    $OpenPath = $repoRoot
}

$WezTermGuiPath = (Resolve-Path $WezTermGuiPath).Path
$OpenPath = (Resolve-Path $OpenPath).Path
$OutputDir = New-OutputDirectory -BasePath $OutputDir
$focusLog = Join-Path $OutputDir 'focus-events.jsonl'
$stdoutLog = Join-Path $OutputDir 'wezterm-stdout.log'
$stderrLog = Join-Path $OutputDir 'wezterm-stderr.log'
$resultPath = Join-Path $OutputDir 'result.json'
$signalDir = Join-Path $OutputDir 'signals'

New-Item -ItemType Directory -Force -Path $signalDir | Out-Null
New-Item -ItemType File -Force -Path $focusLog | Out-Null

$oldFocusLog = $env:WEZTERM_SMEAR_FOCUS_LOG

$result = [ordered]@{
    output_dir = $OutputDir
    wezterm_gui_path = $WezTermGuiPath
    open_path = $OpenPath
    focus_log = $focusLog
    phases = @{}
    summary = $null
}

$guiProcess = $null

try {
    $env:WEZTERM_SMEAR_FOCUS_LOG = $focusLog

    $arguments = @(
        '--attach-parent-console',
        '--config-file', (Join-Path $scriptRoot 'wezterm.lua'),
        'start',
        '--always-new-process',
        '--position', '40,40',
        '--cwd', $repoRoot,
        'pwsh.exe',
        '-NoLogo',
        '-NoProfile',
        '-NonInteractive',
        '-ExecutionPolicy', 'Bypass',
        '-File', (Join-Path $scriptRoot 'pane-driver.ps1'),
        '-SignalDir', $signalDir,
        '-OpenPath', $OpenPath
    )

    $guiProcess = Start-Process -FilePath $WezTermGuiPath `
        -ArgumentList $arguments `
        -WorkingDirectory $repoRoot `
        -PassThru `
        -RedirectStandardOutput $stdoutLog `
        -RedirectStandardError $stderrLog

    $hwnd = Wait-MainWindowHandle -Process $guiProcess
    $initialFocusCount = (Get-FocusEvents -Path $focusLog).Count

    Wait-ForFile -Path (Join-Path $signalDir 'phase-a-triggered.txt')
    Start-Sleep -Milliseconds $PhaseDelayMs
    $phaseA = Save-PhaseCapture -Hwnd $hwnd -Name 'phase-a' -OutputDir $OutputDir
    $result['phases']['phase_a'] = $phaseA

    $phaseBWait = Wait-FocusEvent -Path $focusLog -Focused $false -AfterCount $initialFocusCount
    Start-Sleep -Milliseconds $PhaseDelayMs
    $phaseB = Save-PhaseCapture -Hwnd $hwnd -Name 'phase-b' -OutputDir $OutputDir
    $result['phases']['phase_b'] = $phaseB

    Focus-WezTermWindow -Hwnd $hwnd -ProcessId $guiProcess.Id
    $phaseCStartCount = $phaseBWait.Count
    Wait-FocusEvent -Path $focusLog -Focused $true -AfterCount $phaseCStartCount | Out-Null
    New-Item -ItemType File -Force -Path (Join-Path $signalDir 'phase-c.signal') | Out-Null
    Wait-ForFile -Path (Join-Path $signalDir 'phase-c-triggered.txt')
    Start-Sleep -Milliseconds $PhaseDelayMs
    $phaseC = Save-PhaseCapture -Hwnd $hwnd -Name 'phase-c' -OutputDir $OutputDir
    $result['phases']['phase_c'] = $phaseC

    $summary = Get-HarnessSummary -PhaseA $phaseA -PhaseB $phaseB -PhaseC $phaseC
    $result['summary'] = $summary
    Write-ResultFile -Path $resultPath -Value $result

    if (-not $LeaveGuiRunning) {
        New-Item -ItemType File -Force -Path (Join-Path $signalDir 'done.signal') | Out-Null
        Start-Sleep -Milliseconds 150
        if (-not $guiProcess.HasExited) {
            $null = $guiProcess.CloseMainWindow()
            Start-Sleep -Seconds 1
        }
        if (-not $guiProcess.HasExited) {
            Stop-Process -Id $guiProcess.Id -Force
        }
    }

    if (-not $summary.Passed) {
        throw "Cursor smear harness detected a regression. See $resultPath"
    }

    Write-Host "Cursor smear focus harness passed. Artifacts: $OutputDir"
}
catch {
    $result['error'] = $_.Exception.Message
    Write-ResultFile -Path $resultPath -Value $result

    if ($guiProcess -and -not $LeaveGuiRunning -and -not $guiProcess.HasExited) {
        try {
            New-Item -ItemType File -Force -Path (Join-Path $signalDir 'done.signal') | Out-Null
            Start-Sleep -Milliseconds 150
            Stop-Process -Id $guiProcess.Id -Force
        }
        catch {
        }
    }

    throw
}
finally {
    $env:WEZTERM_SMEAR_FOCUS_LOG = $oldFocusLog
}
