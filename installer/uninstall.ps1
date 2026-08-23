[CmdletBinding()]
param(
    [string]$InstallDirectory = "$env:ProgramData\WinSched",
    [switch]$PurgeData
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw "WinSched removal requires an elevated Administrator session."
}

function Wait-ServiceAbsent {
    $deadline = [DateTime]::UtcNow.AddSeconds(20)
    while ((Get-Service -Name "WinSched" -ErrorAction SilentlyContinue) -and
           [DateTime]::UtcNow -lt $deadline) {
        Start-Sleep -Milliseconds 250
    }
    if (Get-Service -Name "WinSched" -ErrorAction SilentlyContinue) {
        throw "WinSched service did not disappear before the removal timeout."
    }
}

$trayDeadline = [DateTime]::UtcNow.AddSeconds(15)
do {
    $trayProcesses = @(
        Get-Process -Name "winsched-tray", "winsched-settings" -ErrorAction SilentlyContinue
    )
    foreach ($trayProcess in $trayProcesses) {
        Stop-Process -Id $trayProcess.Id -Force -ErrorAction SilentlyContinue
    }
    if ($trayProcesses.Count -ne 0) {
        Start-Sleep -Milliseconds 250
    }
} while ($trayProcesses.Count -ne 0 -and [DateTime]::UtcNow -lt $trayDeadline)
if (Get-Process -Name "winsched-tray", "winsched-settings" -ErrorAction SilentlyContinue) {
    throw "A running WinSched user application did not exit before removal."
}

$service = Get-Service -Name "WinSched" -ErrorAction SilentlyContinue
if ($service) {
    if ($service.Status -ne [System.ServiceProcess.ServiceControllerStatus]::Stopped) {
        $service.Stop()
        $service.WaitForStatus(
            [System.ServiceProcess.ServiceControllerStatus]::Stopped,
            [TimeSpan]::FromSeconds(20)
        )
    }
    $service.Dispose()
    & sc.exe delete WinSched | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to delete WinSched service (sc.exe exit $LASTEXITCODE)."
    }
    Wait-ServiceAbsent
}

$startupShortcut = Join-Path ([Environment]::GetFolderPath("CommonStartup")) "WinSched Tray.lnk"
Remove-Item -LiteralPath $startupShortcut -Force -ErrorAction SilentlyContinue
$programs = Join-Path $env:ProgramData "Microsoft\Windows\Start Menu\Programs\WinSched"
Remove-Item -LiteralPath $programs -Recurse -Force -ErrorAction SilentlyContinue

if ($PurgeData) {
    if (Test-Path -LiteralPath $InstallDirectory) {
        Remove-Item -LiteralPath $InstallDirectory -Recurse -Force
    }
    if (Test-Path -LiteralPath $InstallDirectory) {
        throw "WinSched data directory still exists after purge."
    }
    Write-Host "WinSched and its data were removed."
} else {
    @(
        "winsched-service.exe",
        "winsched-tray.exe",
        "winsched-settings.exe",
        "winsched.exe"
    ) | ForEach-Object {
        $path = Join-Path $InstallDirectory $_
        if (Test-Path -LiteralPath $path) {
            Remove-Item -LiteralPath $path -Force
        }
        if (Test-Path -LiteralPath $path) {
            throw "Installed binary still exists after removal: $path"
        }
    }
    Write-Host "WinSched was removed. Configuration and logs remain in $InstallDirectory"
}
