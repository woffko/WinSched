[CmdletBinding()]
param(
    [string]$InstallDirectory = "$env:ProgramData\WinSched",
    [string]$Configuration = (Join-Path $PSScriptRoot "winsched.toml"),
    [switch]$NoStart,
    [switch]$NoTrayAutostart,
    [switch]$NoTrayLaunch
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Assert-Administrator {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw "WinSched installation requires an elevated Administrator session."
    }
}

function Assert-File([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "Required package file is missing: $Path"
    }
}

function Wait-ServiceAbsent {
    $deadline = [DateTime]::UtcNow.AddSeconds(15)
    while ((Get-Service -Name "WinSched" -ErrorAction SilentlyContinue) -and
           [DateTime]::UtcNow -lt $deadline) {
        Start-Sleep -Milliseconds 250
    }
    if (Get-Service -Name "WinSched" -ErrorAction SilentlyContinue) {
        throw "The previous WinSched service did not disappear before the timeout."
    }
}

function New-Shortcut(
    [string]$Path,
    [string]$Target,
    [string]$Arguments,
    [string]$WorkingDirectory,
    [string]$IconLocation
) {
    $shell = New-Object -ComObject WScript.Shell
    $shortcut = $shell.CreateShortcut($Path)
    $shortcut.TargetPath = $Target
    $shortcut.Arguments = $Arguments
    $shortcut.WorkingDirectory = $WorkingDirectory
    $shortcut.IconLocation = $IconLocation
    $shortcut.Save()
}

function Start-TrayLimited([string]$TrayPath, [string]$WorkingDirectory) {
    $taskName = "WinSchedTrayLaunch-$([Guid]::NewGuid().ToString('N'))"
    $userId = [Security.Principal.WindowsIdentity]::GetCurrent().Name
    $escapedTray = $TrayPath.Replace("'", "''")
    $escapedWorkingDirectory = $WorkingDirectory.Replace("'", "''")
    $arguments = "-NoProfile -NonInteractive -Command `"Start-Process -FilePath '$escapedTray' -WorkingDirectory '$escapedWorkingDirectory'`""
    $action = New-ScheduledTaskAction -Execute "powershell.exe" -Argument $arguments
    $principal = New-ScheduledTaskPrincipal `
        -UserId $userId `
        -LogonType Interactive `
        -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet `
        -AllowStartIfOnBatteries `
        -DontStopIfGoingOnBatteries `
        -ExecutionTimeLimit ([TimeSpan]::FromMinutes(1))
    try {
        Register-ScheduledTask `
            -TaskName $taskName `
            -Action $action `
            -Principal $principal `
            -Settings $settings | Out-Null
        Start-ScheduledTask -TaskName $taskName
        $deadline = [DateTime]::UtcNow.AddSeconds(15)
        while (-not (Get-Process -Name "winsched-tray" -ErrorAction SilentlyContinue) -and
               [DateTime]::UtcNow -lt $deadline) {
            Start-Sleep -Milliseconds 250
        }
        if (-not (Get-Process -Name "winsched-tray" -ErrorAction SilentlyContinue)) {
            throw "The limited-user tray process did not start before the timeout."
        }
    } finally {
        Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue
    }
}

Assert-Administrator

$serviceSource = Join-Path $PSScriptRoot "winsched-service.exe"
$traySource = Join-Path $PSScriptRoot "winsched-tray.exe"
$settingsSource = Join-Path $PSScriptRoot "winsched-settings.exe"
$cliSource = Join-Path $PSScriptRoot "winsched.exe"
$uninstallSource = Join-Path $PSScriptRoot "uninstall.ps1"
$installedConfig = Join-Path $InstallDirectory "winsched.toml"
$configurationWasExplicit = $PSBoundParameters.ContainsKey("Configuration")
$effectiveConfiguration = if ((Test-Path -LiteralPath $installedConfig -PathType Leaf) -and
    -not $configurationWasExplicit) {
    $installedConfig
} else {
    $Configuration
}
Assert-File $serviceSource
Assert-File $traySource
Assert-File $settingsSource
Assert-File $cliSource
Assert-File $uninstallSource
Assert-File $effectiveConfiguration

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
    throw "A running WinSched user application did not exit before the update timeout."
}

$existing = Get-Service -Name "WinSched" -ErrorAction SilentlyContinue
if ($existing) {
    if ($existing.Status -ne [System.ServiceProcess.ServiceControllerStatus]::Stopped) {
        & sc.exe stop WinSched | Out-Null
        $existing.WaitForStatus(
            [System.ServiceProcess.ServiceControllerStatus]::Stopped,
            [TimeSpan]::FromSeconds(20)
        )
    }
    $existing.Dispose()
    & sc.exe delete WinSched | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to remove the previous WinSched service (sc.exe exit $LASTEXITCODE)."
    }
    Wait-ServiceAbsent
}

New-Item -ItemType Directory -Path $InstallDirectory -Force | Out-Null
Copy-Item -LiteralPath $traySource -Destination (Join-Path $InstallDirectory "winsched-tray.exe") -Force
Copy-Item -LiteralPath $settingsSource -Destination (Join-Path $InstallDirectory "winsched-settings.exe") -Force
Copy-Item -LiteralPath $cliSource -Destination (Join-Path $InstallDirectory "winsched.exe") -Force
Copy-Item -LiteralPath $uninstallSource -Destination (Join-Path $InstallDirectory "uninstall.ps1") -Force

$installArguments = @("install", "--config", $effectiveConfiguration, "--allow-auto")
if (-not $NoStart) {
    $installArguments += "--start"
}
& $serviceSource @installArguments
if ($LASTEXITCODE -ne 0) {
    throw "winsched-service install failed with exit code $LASTEXITCODE."
}

$installedTray = Join-Path $InstallDirectory "winsched-tray.exe"
$installedSettings = Join-Path $InstallDirectory "winsched-settings.exe"
$commonStartup = [Environment]::GetFolderPath("CommonStartup")
$startupShortcut = Join-Path $commonStartup "WinSched Tray.lnk"
if ($NoTrayAutostart) {
    Remove-Item -LiteralPath $startupShortcut -Force -ErrorAction SilentlyContinue
} else {
    New-Shortcut $startupShortcut $installedTray "" $InstallDirectory "$installedTray,0"
}

$programs = Join-Path $env:ProgramData "Microsoft\Windows\Start Menu\Programs\WinSched"
New-Item -ItemType Directory -Path $programs -Force | Out-Null
New-Shortcut (Join-Path $programs "WinSched Tray.lnk") $installedTray "" $InstallDirectory "$installedTray,0"
New-Shortcut (Join-Path $programs "WinSched Settings.lnk") $installedSettings "" $InstallDirectory "$installedSettings,0"
New-Shortcut `
    (Join-Path $programs "WinSched Configuration (Advanced).lnk") `
    "$env:SystemRoot\System32\notepad.exe" `
    "`"$(Join-Path $InstallDirectory 'winsched.toml')`"" `
    $InstallDirectory `
    "$env:SystemRoot\System32\notepad.exe,0"
New-Shortcut `
    (Join-Path $programs "Uninstall WinSched.lnk") `
    "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe" `
    "-NoProfile -ExecutionPolicy Bypass -File `"$(Join-Path $InstallDirectory 'uninstall.ps1')`"" `
    $InstallDirectory `
    "$env:SystemRoot\System32\shell32.dll,131"

if (-not $NoTrayLaunch) {
    Start-TrayLimited $installedTray $InstallDirectory
}

Write-Host "WinSched installed in $InstallDirectory"
Write-Host "Service: Automatic; tray autostart: $(-not $NoTrayAutostart)"
Write-Host "Configuration: $installedConfig"
