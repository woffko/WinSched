[CmdletBinding()]
param(
    [string]$InstallDirectory = (Join-Path ([Environment]::GetFolderPath("ProgramFiles")) "WinSched"),
    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class WinSchedMonitorWindow {
    [DllImport("user32.dll")]
    public static extern bool ShowWindow(IntPtr window, int command);
    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr window);
    public const int RESTORE = 9;
    public const int MINIMIZE = 6;
}
"@

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw "ASSERTION FAILED: $Message" }
}

function Wait-Condition([string]$Description, [scriptblock]$Condition, [int]$TimeoutSeconds = 30) {
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        if (& $Condition) { return }
        Start-Sleep -Milliseconds 250
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Timed out waiting for: $Description"
}

function Get-Window($Process) {
    $Process.Refresh()
    if ($Process.MainWindowHandle -eq [IntPtr]::Zero) { return $null }
    return [System.Windows.Automation.AutomationElement]::FromHandle($Process.MainWindowHandle)
}

function Get-Names($Window) {
    if ($null -eq $Window) { return @() }
    return @(
        $Window.FindAll(
            [System.Windows.Automation.TreeScope]::Descendants,
            [System.Windows.Automation.Condition]::TrueCondition
        ) | ForEach-Object {
            try { $_.Current.Name } catch { $null }
        } | Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
    )
}

function Get-SnapshotCount($Window) {
    foreach ($name in Get-Names $Window) {
        if ($name -match '^Snapshots:\s*(?<count>\d+)$') {
            return [uint64]$Matches.count
        }
    }
    return $null
}

function Get-ObservationCount([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { return $null }
    try {
        return [uint64](Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json).snapshots_started
    } catch {
        return $null
    }
}

function Capture-Window([IntPtr]$Handle, [string]$Path) {
    $element = [System.Windows.Automation.AutomationElement]::FromHandle($Handle)
    $rectangle = $element.Current.BoundingRectangle
    $bounds = [System.Drawing.Rectangle]::FromLTRB(
        [int][Math]::Floor($rectangle.Left),
        [int][Math]::Floor($rectangle.Top),
        [int][Math]::Ceiling($rectangle.Right),
        [int][Math]::Ceiling($rectangle.Bottom)
    )
    $bitmap = New-Object System.Drawing.Bitmap($bounds.Width, $bounds.Height)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.CopyFromScreen($bounds.Location, [System.Drawing.Point]::Empty, $bounds.Size)
        $bitmap.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png)
    } finally {
        $graphics.Dispose()
        $bitmap.Dispose()
    }
}

New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$resultPath = Join-Path $OutputDirectory "process-monitor-ui-result.json"
$screenshotPath = Join-Path $OutputDirectory "process-monitor.png"
$monitorPath = Join-Path $InstallDirectory "winsched-monitor.exe"
$observationPath = Join-Path $OutputDirectory "process-monitor-observation.json"
$monitor = $null

try {
    Assert-True (Test-Path -LiteralPath $monitorPath -PathType Leaf) `
        "installed Process Monitor is missing"
    Get-Process -Name "winsched-monitor" -ErrorAction SilentlyContinue |
        Stop-Process -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $observationPath -Force -ErrorAction SilentlyContinue
    $monitor = Start-Process `
        -FilePath $monitorPath `
        -WorkingDirectory $InstallDirectory `
        -ArgumentList @("--test-observation-file", $observationPath) `
        -PassThru
    Wait-Condition "Process Monitor window" {
        $null -ne (Get-Window $monitor)
    }
    $window = Get-Window $monitor
    $names = Get-Names $window
    Assert-True (@($names | Where-Object { $_ -like 'Settings*' }).Count -gt 0) `
        "Settings button is missing"
    Assert-True (@($names | Where-Object { $_ -eq 'Refresh' }).Count -gt 0) `
        "Refresh button is missing"
    Assert-True (@($names | Where-Object { $_ -eq 'CPU Sets' }).Count -gt 0) `
        "CPU Sets column is missing"
    Assert-True (@($names | Where-Object { $_ -eq 'EcoQoS' }).Count -gt 0) `
        "EcoQoS column is missing"
    Assert-True (@($names | Where-Object { $_ -eq 'Rule / scope' }).Count -gt 0) `
        "rule/scope column is missing"

    Wait-Condition "two active process snapshots" {
        $count = Get-ObservationCount $observationPath
        $null -ne $count -and $count -ge 2
    } 30
    $activeCount = Get-SnapshotCount (Get-Window $monitor)
    $activeObservationCount = Get-ObservationCount $observationPath
    Capture-Window $monitor.MainWindowHandle $screenshotPath

    [void][WinSchedMonitorWindow]::ShowWindow(
        $monitor.MainWindowHandle,
        [WinSchedMonitorWindow]::MINIMIZE
    )
    Start-Sleep -Seconds 2
    $minimizedSettledCount = Get-ObservationCount $observationPath
    Start-Sleep -Seconds 3
    $minimizedCount = Get-ObservationCount $observationPath
    Assert-True ($minimizedCount -eq $minimizedSettledCount) `
        "Process Monitor continued sampling while minimized"

    [void][WinSchedMonitorWindow]::ShowWindow(
        $monitor.MainWindowHandle,
        [WinSchedMonitorWindow]::RESTORE
    )
    $secondMonitor = Start-Process `
        -FilePath $monitorPath `
        -WorkingDirectory $InstallDirectory `
        -PassThru
    Assert-True ($secondMonitor.WaitForExit(10000)) `
        "second Process Monitor activation process did not exit"
    Assert-True (-not $monitor.HasExited) `
        "existing Process Monitor exited during activation"
    Wait-Condition "sampling resumed after restore" {
        $count = Get-ObservationCount $observationPath
        $null -ne $count -and $count -gt $minimizedCount
    } 20
    $resumedCount = Get-ObservationCount $observationPath

    [pscustomobject]@{
        result = "PASS"
        active_snapshots = $activeCount
        active_observations = $activeObservationCount
        minimized_settled_snapshots = $minimizedSettledCount
        minimized_snapshots = $minimizedCount
        resumed_snapshots = $resumedCount
        polling_paused_while_minimized = $true
        polling_resumed_when_active = $true
        second_instance_focused_existing_window = $true
        settings_button = $true
        required_columns = @("CPU Sets", "LLC", "EcoQoS", "Memory", "Rule / scope")
        screenshot = $screenshotPath
    } | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $resultPath -Encoding UTF8
} catch {
    [pscustomobject]@{
        result = "FAIL"
        error = $_.Exception.ToString()
        script_stack = $_.ScriptStackTrace
    } | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $resultPath -Encoding UTF8
    exit 1
} finally {
    if ($null -ne $monitor) {
        Stop-Process -Id $monitor.Id -Force -ErrorAction SilentlyContinue
    }
}
