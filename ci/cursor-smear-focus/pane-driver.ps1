[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$SignalDir,

    [Parameter(Mandatory = $true)]
    [string]$OpenPath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Write-Marker {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Name
    )

    $path = Join-Path $SignalDir $Name
    Set-Content -LiteralPath $path -Value (Get-Date).ToString('o') -NoNewline
}

function Emit-CursorJump {
    param(
        [Parameter(Mandatory = $true)]
        [int]$FromRow,
        [Parameter(Mandatory = $true)]
        [int]$FromCol,
        [Parameter(Mandatory = $true)]
        [int]$ToRow,
        [Parameter(Mandatory = $true)]
        [int]$ToCol
    )

    $esc = [char]27
    [Console]::Out.Write("${esc}[${FromRow};${FromCol}H")
    [Console]::Out.Flush()
    Start-Sleep -Milliseconds 40
    [Console]::Out.Write("${esc}[${ToRow};${ToCol}H")
    [Console]::Out.Flush()
}

function Wait-For-Signal {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Name,
        [int]$TimeoutSeconds = 20
    )

    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    $path = Join-Path $SignalDir $Name
    while ((Get-Date) -lt $deadline) {
        if (Test-Path -LiteralPath $path) {
            return
        }
        Start-Sleep -Milliseconds 50
    }

    throw "Timed out waiting for signal $Name"
}

New-Item -ItemType Directory -Force -Path $SignalDir | Out-Null

$esc = [char]27
[Console]::Out.Write("${esc}[2J${esc}[H${esc}[?25h")
[Console]::Out.Flush()
Write-Marker -Name 'driver-ready.txt'

Start-Sleep -Milliseconds 700

Emit-CursorJump -FromRow 8 -FromCol 5 -ToRow 8 -ToCol 35
Write-Marker -Name 'phase-a-triggered.txt'

Start-Sleep -Milliseconds 120

Start-Process -FilePath $OpenPath | Out-Null
Write-Marker -Name 'phase-b-requested.txt'

Wait-For-Signal -Name 'phase-c.signal'
Emit-CursorJump -FromRow 12 -FromCol 35 -ToRow 12 -ToCol 8
Write-Marker -Name 'phase-c-triggered.txt'

Wait-For-Signal -Name 'done.signal'
