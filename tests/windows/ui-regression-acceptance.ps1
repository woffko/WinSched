[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$TestDirectory,
    [Parameter(Mandatory = $true)]
    [string]$InteractiveUser,
    [Parameter(Mandatory = $true)]
    [string]$OutputRoot,
    [string]$InstallDirectory = "$env:ProgramFiles\WinSched",
    [string]$DataDirectory = "$env:ProgramData\WinSched"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Assert-Stage([string]$Name) {
    if ($LASTEXITCODE -ne 0) {
        throw "$Name failed with exit code $LASTEXITCODE"
    }
}

function Invoke-TrayStage(
    [string]$AcceptanceScript,
    [string]$OutputDirectory,
    [string]$Name
) {
    & powershell.exe `
        -NoLogo `
        -NoProfile `
        -NonInteractive `
        -ExecutionPolicy Bypass `
        -File (Join-Path $TestDirectory "schedule-tray-ui-acceptance.ps1") `
        -AcceptanceScript (Join-Path $TestDirectory $AcceptanceScript) `
        -OutputDirectory $OutputDirectory `
        -InteractiveUser $InteractiveUser `
        -InstallDirectory $InstallDirectory `
        -DataDirectory $DataDirectory `
        -ExpectedVersion "0.6.0"
    Assert-Stage $Name
}

function Invoke-SettingsStage(
    [string]$AcceptanceScript,
    [string]$OutputDirectory,
    [string]$Name,
    [string]$ResultFileName
) {
    $arguments = @(
        '-NoLogo',
        '-NoProfile',
        '-NonInteractive',
        '-ExecutionPolicy',
        'Bypass',
        '-File',
        (Join-Path $TestDirectory "schedule-settings-ui-acceptance.ps1"),
        '-AcceptanceScript',
        (Join-Path $TestDirectory $AcceptanceScript),
        '-OutputDirectory',
        $OutputDirectory,
        '-InteractiveUser',
        $InteractiveUser,
        '-InstallDirectory',
        $InstallDirectory,
        '-DataDirectory',
        $DataDirectory
    )
    if (-not [string]::IsNullOrWhiteSpace($ResultFileName)) {
        $arguments += @('-ResultFileName', $ResultFileName)
    }
    & powershell.exe @arguments
    Assert-Stage $Name
}

New-Item -ItemType Directory -Path $OutputRoot -Force | Out-Null
$trayControl = Join-Path $OutputRoot "tray-control"
$trayStatus = Join-Path $OutputRoot "tray-status"
$settings = Join-Path $OutputRoot "settings"
$diagnostics = Join-Path $OutputRoot "diagnostics"

Write-Host "UI stage: tray controls"
Invoke-TrayStage "tray-ui-acceptance.ps1" $trayControl "tray control acceptance"

Write-Host "UI stage: tray About and status"
Invoke-TrayStage "tray-responsiveness-smoke.ps1" $trayStatus "tray status acceptance"

Write-Host "UI stage: Settings controls and tooltips"
Invoke-SettingsStage `
    "settings-ui-acceptance.ps1" `
    $settings `
    "Settings acceptance" `
    ""

Write-Host "UI stage: Diagnostics and controller efficiency"
Invoke-SettingsStage `
    "diagnostics-ui-acceptance.ps1" `
    $diagnostics `
    "Diagnostics acceptance" `
    "diagnostics-ui-result.json"

$result = [ordered]@{
    result = "PASS"
    version = "0.6.0"
    tray_controls = "PASS"
    tray_about_and_status = "PASS"
    settings_controls_and_tooltips = "PASS"
    diagnostics_and_self_observability = "PASS"
    output_directories = [ordered]@{
        tray_controls = $trayControl
        tray_status = $trayStatus
        settings = $settings
        diagnostics = $diagnostics
    }
}
$resultPath = Join-Path $OutputRoot "ui-regression-result.json"
[IO.File]::WriteAllText(
    $resultPath,
    ([pscustomobject]$result | ConvertTo-Json -Depth 5) + "`n",
    [Text.UTF8Encoding]::new($false)
)
[pscustomobject]$result | ConvertTo-Json -Depth 5
