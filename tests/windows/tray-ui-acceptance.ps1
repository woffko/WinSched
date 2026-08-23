[CmdletBinding()]
param(
    [string]$InstallDirectory = "$env:ProgramData\WinSched",
    [string]$DataDirectory = $InstallDirectory,
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
public static class WinSchedMouse {
    [DllImport("user32.dll")]
    public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extraInfo);
    public const uint LEFT_DOWN = 0x0002;
    public const uint LEFT_UP = 0x0004;
}
"@

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) {
        throw "ASSERTION FAILED: $Message"
    }
}

function Wait-Condition(
    [string]$Description,
    [scriptblock]$Condition,
    [int]$TimeoutSeconds = 30
) {
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        if (& $Condition) {
            return
        }
        Start-Sleep -Milliseconds 250
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Timed out waiting for: $Description"
}

function Read-Status {
        $path = Join-Path $DataDirectory "status.json"
    if (-not (Test-Path -LiteralPath $path)) {
        return $null
    }
    try {
        return Get-Content -LiteralPath $path -Raw | ConvertFrom-Json
    } catch {
        return $null
    }
}

function Get-AllAutomationElements {
    return [System.Windows.Automation.AutomationElement]::RootElement.FindAll(
        [System.Windows.Automation.TreeScope]::Descendants,
        [System.Windows.Automation.Condition]::TrueCondition
    )
}

function Find-ButtonLike([string]$NamePattern) {
    foreach ($element in Get-AllAutomationElements) {
        try {
            if ($element.Current.Name -like $NamePattern -and
                $element.Current.ControlType -eq [System.Windows.Automation.ControlType]::Button) {
                return $element
            }
        } catch {
            continue
        }
    }
    return $null
}

function Find-ElementByExactName([string]$Name) {
    foreach ($element in Get-AllAutomationElements) {
        try {
            if ($element.Current.Name -eq $Name) {
                return $element
            }
        } catch {
            continue
        }
    }
    return $null
}

function Find-MenuItem([string]$Name) {
    foreach ($element in Get-AllAutomationElements) {
        try {
            if ($element.Current.Name -eq $Name -and
                $element.Current.ControlType -eq [System.Windows.Automation.ControlType]::MenuItem) {
                return $element
            }
        } catch {
            continue
        }
    }
    return $null
}

function Invoke-AutomationElement($Element) {
    $pattern = $null
    if ($Element.TryGetCurrentPattern(
        [System.Windows.Automation.InvokePattern]::Pattern,
        [ref]$pattern
    )) {
        $pattern.Invoke()
        return
    }

    $point = $Element.GetClickablePoint()
    [System.Windows.Forms.Cursor]::Position = [System.Drawing.Point]::new(
        [int][Math]::Round($point.X),
        [int][Math]::Round($point.Y)
    )
    [WinSchedMouse]::mouse_event([WinSchedMouse]::LEFT_DOWN, 0, 0, 0, [UIntPtr]::Zero)
    [WinSchedMouse]::mouse_event([WinSchedMouse]::LEFT_UP, 0, 0, 0, [UIntPtr]::Zero)
}

function Find-TrayIcon {
    $icon = Find-ButtonLike "WinSched:*"
    if ($icon) {
        return $icon
    }
    $chevron = Find-ButtonLike "Show hidden icons*"
    if ($chevron) {
        Invoke-AutomationElement $chevron
        Start-Sleep -Milliseconds 500
        return Find-ButtonLike "WinSched:*"
    }
    return $null
}

function Open-TrayMenu {
    $icon = Find-TrayIcon
    Assert-True ($null -ne $icon) "WinSched notification-area icon was not found through UI Automation"
    Invoke-AutomationElement $icon
    Start-Sleep -Milliseconds 400
}

function Invoke-MenuItem([string]$Name) {
    Open-TrayMenu
    $item = Find-MenuItem $Name
    Assert-True ($null -ne $item) "tray menu item '$Name' was not found"
    Assert-True $item.Current.IsEnabled "tray menu item '$Name' is disabled"
    Invoke-AutomationElement $item
}

function Capture-Screen([string]$Path) {
    $bounds = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
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
$resultPath = Join-Path $OutputDirectory "tray-ui-result.json"
$screenshotPath = Join-Path $OutputDirectory "tray-menu.png"
$iconPath = Join-Path $OutputDirectory "embedded-icon.png"
$trayPath = Join-Path $InstallDirectory "winsched-tray.exe"

try {
    Assert-True (Test-Path -LiteralPath $trayPath -PathType Leaf) "installed tray executable missing"
    Wait-Condition "WinSched service running" {
        (Get-Service -Name "WinSched" -ErrorAction SilentlyContinue).Status -eq "Running"
    }

    $existingTrayProcesses = @(
        Get-Process -Name "winsched-tray" -ErrorAction SilentlyContinue |
            Where-Object { $_.Path -eq $trayPath }
    )
    $existingTrayProcesses | Stop-Process -Force -ErrorAction SilentlyContinue
    Wait-Condition "previous tray process exited" {
        @(
            Get-Process -Name "winsched-tray" -ErrorAction SilentlyContinue |
                Where-Object { $_.Path -eq $trayPath }
        ).Count -eq 0
    } 15
    $originalTray = Start-Process `
        -FilePath $trayPath `
        -WorkingDirectory $InstallDirectory `
        -PassThru
    Wait-Condition "tray process in interactive session" {
        @(
            Get-Process -Name "winsched-tray" -ErrorAction SilentlyContinue |
                Where-Object { $_.Path -eq $trayPath -and $_.SessionId -eq [Diagnostics.Process]::GetCurrentProcess().SessionId }
        ).Count -eq 1
    }

    $secondTray = Start-Process `
        -FilePath $trayPath `
        -WorkingDirectory $InstallDirectory `
        -PassThru
    Assert-True ($secondTray.WaitForExit(10000)) "second tray instance did not exit"
    Assert-True (-not $originalTray.HasExited) "original tray exited during single-instance test"
    $trayProcesses = @(
        Get-Process -Name "winsched-tray" -ErrorAction SilentlyContinue |
            Where-Object { $_.Path -eq $trayPath -and $_.SessionId -eq [Diagnostics.Process]::GetCurrentProcess().SessionId }
    )
    Assert-True ($trayProcesses.Count -eq 1) "single-instance guard allowed multiple tray processes"
    Assert-True ($trayProcesses[0].Id -eq $originalTray.Id) "single-instance guard replaced the original tray"
    Assert-True `
        ($null -eq (Find-ElementByExactName "winsched-tray.exe - Entry Point Not Found")) `
        "obsolete TaskDialogIndirect loader error is still visible"

    $embeddedIcon = [System.Drawing.Icon]::ExtractAssociatedIcon($trayPath)
    Assert-True ($null -ne $embeddedIcon) "tray executable has no associated icon"
    $iconBitmap = $embeddedIcon.ToBitmap()
    try {
        $iconBitmap.Save($iconPath, [System.Drawing.Imaging.ImageFormat]::Png)
    } finally {
        $iconBitmap.Dispose()
        $embeddedIcon.Dispose()
    }
    Assert-True ((Get-Item -LiteralPath $iconPath).Length -gt 100) "extracted tray icon is empty"

    Wait-Condition "WinSched tray icon discoverable" { $null -ne (Find-TrayIcon) }
    Open-TrayMenu
    $expected = @(
        "Disable Scheduling",
        "Stop Service",
        "Mode: Auto",
        "Settings...",
        "Open Configuration (Advanced)",
        "Open Logs",
        "Refresh Status",
        "Exit Tray"
    )
    foreach ($name in $expected) {
        Assert-True ($null -ne (Find-MenuItem $name)) "expected tray menu item missing: $name"
    }
    Capture-Screen $screenshotPath

    $notepadBefore = @(
        Get-Process -Name "Notepad" -ErrorAction SilentlyContinue |
            Select-Object -ExpandProperty Id
    )
    $openConfig = Find-MenuItem "Open Configuration (Advanced)"
    Assert-True ($null -ne $openConfig) "advanced configuration item disappeared"
    Invoke-AutomationElement $openConfig
    Wait-Condition "configuration opened in Notepad" {
        @(
            Get-Process -Name "Notepad" -ErrorAction SilentlyContinue |
                Where-Object { $notepadBefore -notcontains $_.Id }
        ).Count -gt 0
    } 15
    $configNotepad = @(
        Get-Process -Name "Notepad" -ErrorAction SilentlyContinue |
            Where-Object { $notepadBefore -notcontains $_.Id }
    )
    $configNotepad | Stop-Process -Force -ErrorAction SilentlyContinue

    $notepadBefore = @(
        Get-Process -Name "Notepad" -ErrorAction SilentlyContinue |
            Select-Object -ExpandProperty Id
    )
    Invoke-MenuItem "Open Logs"
    Wait-Condition "log opened in Notepad" {
        @(
            Get-Process -Name "Notepad" -ErrorAction SilentlyContinue |
                Where-Object { $notepadBefore -notcontains $_.Id }
        ).Count -gt 0
    } 15
    $logNotepad = @(
        Get-Process -Name "Notepad" -ErrorAction SilentlyContinue |
            Where-Object { $notepadBefore -notcontains $_.Id }
    )
    $logNotepad | Stop-Process -Force -ErrorAction SilentlyContinue

    Invoke-MenuItem "Refresh Status"
    Assert-True (-not $originalTray.HasExited) "Refresh Status terminated the tray"

    Invoke-MenuItem "Disable Scheduling"
    Wait-Condition "tray disabled scheduling" {
        $status = Read-Status
        $status -and -not $status.scheduling_enabled -and $status.managed_processes -eq 0
    }

    Invoke-MenuItem "Stop Service"
    Wait-Condition "tray stopped service" {
        (Get-Service -Name "WinSched").Status -eq "Stopped"
    }
    Start-Sleep -Seconds 2
    Invoke-MenuItem "Start Service"
    Wait-Condition "tray started service" {
        (Get-Service -Name "WinSched").Status -eq "Running"
    }
    Wait-Condition "disabled state survived tray service restart" {
        $status = Read-Status
        $status -and -not $status.scheduling_enabled -and $status.phase -eq "disabled"
    }
    Start-Sleep -Seconds 2
    Invoke-MenuItem "Enable Scheduling"
    Wait-Condition "tray enabled scheduling" {
        $status = Read-Status
        $status -and $status.scheduling_enabled -and $status.phase -eq "running"
    }

    Invoke-MenuItem "Exit Tray"
    Assert-True ($originalTray.WaitForExit(10000)) "Exit Tray did not terminate the tray process"
    $finalTray = Start-Process `
        -FilePath $trayPath `
        -WorkingDirectory $InstallDirectory `
        -PassThru
    Wait-Condition "tray restarted after Exit test" {
        -not $finalTray.HasExited -and $null -ne (Find-TrayIcon)
    }

    [pscustomobject]@{
        result = "PASS"
        session_id = [Diagnostics.Process]::GetCurrentProcess().SessionId
        tray_pid = $finalTray.Id
        managed_processes = (Read-Status).managed_processes
        menu_items = $expected
        screenshot = $screenshotPath
        extracted_icon = $iconPath
    } | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $resultPath -Encoding UTF8
} catch {
    [pscustomobject]@{
        result = "FAIL"
        session_id = [Diagnostics.Process]::GetCurrentProcess().SessionId
        error = $_.Exception.ToString()
        script_stack = $_.ScriptStackTrace
    } | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $resultPath -Encoding UTF8
    exit 1
}
