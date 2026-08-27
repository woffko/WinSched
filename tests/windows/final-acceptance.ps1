[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$PackageDirectory,
    [Parameter(Mandatory = $true)]
    [string]$TestDirectory,
    [Parameter(Mandatory = $true)]
    [string]$InteractiveUser,
    [Parameter(Mandatory = $true)]
    [string]$PublicUiDirectory
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$uiOutputRoot = Join-Path $PublicUiDirectory "output"
$trayControlOutput = Join-Path $uiOutputRoot "tray-control"
$trayV05Output = Join-Path $uiOutputRoot "tray-v05"
$settingsOutput = Join-Path $uiOutputRoot "settings"
$diagnosticsOutput = Join-Path $uiOutputRoot "diagnostics"
$installDirectory = Join-Path ([Environment]::GetFolderPath("ProgramFiles")) "WinSched"
$dataDirectory = Join-Path ([Environment]::GetFolderPath("CommonApplicationData")) "WinSched"

Write-Host "final stage: service and adaptive acceptance"
& (Join-Path $TestDirectory "full-acceptance.ps1") `
    -PackageDirectory $PackageDirectory `
    -InteractiveUser $InteractiveUser

Write-Host "final stage: bounded logging acceptance"
& (Join-Path $TestDirectory "logging-acceptance.ps1") `
    -InteractiveUser $InteractiveUser

Write-Host "final stage: tray UI acceptance"
& powershell.exe `
    -NoProfile `
    -NonInteractive `
    -ExecutionPolicy Bypass `
    -File (Join-Path $PublicUiDirectory "schedule-tray-ui-acceptance.ps1") `
    -AcceptanceScript (Join-Path $PublicUiDirectory "tray-ui-acceptance.ps1") `
    -OutputDirectory $trayControlOutput `
    -InteractiveUser $InteractiveUser `
    -InstallDirectory $installDirectory `
    -DataDirectory $dataDirectory `
    -ExpectedVersion "0.5.1"
if ($LASTEXITCODE -ne 0) {
    throw "tray UI acceptance failed with exit code $LASTEXITCODE"
}

Write-Host "final stage: v0.5.1 tray About and background status smoke"
& powershell.exe `
    -NoProfile `
    -NonInteractive `
    -ExecutionPolicy Bypass `
    -File (Join-Path $PublicUiDirectory "schedule-tray-ui-acceptance.ps1") `
    -AcceptanceScript (Join-Path $PublicUiDirectory "tray-responsiveness-smoke.ps1") `
    -OutputDirectory $trayV05Output `
    -InteractiveUser $InteractiveUser `
    -InstallDirectory $installDirectory `
    -DataDirectory $dataDirectory `
    -ExpectedVersion "0.5.1"
if ($LASTEXITCODE -ne 0) {
    throw "v0.5.1 tray smoke failed with exit code $LASTEXITCODE"
}

Write-Host "final stage: v0.5.1 settings UI acceptance"
& powershell.exe `
    -NoProfile `
    -NonInteractive `
    -ExecutionPolicy Bypass `
    -File (Join-Path $PublicUiDirectory "schedule-settings-ui-acceptance.ps1") `
    -AcceptanceScript (Join-Path $PublicUiDirectory "settings-ui-acceptance.ps1") `
    -OutputDirectory $settingsOutput `
    -InteractiveUser $InteractiveUser `
    -InstallDirectory $installDirectory `
    -DataDirectory $dataDirectory
if ($LASTEXITCODE -ne 0) {
    throw "settings UI acceptance failed with exit code $LASTEXITCODE"
}

Write-Host "final stage: passive diagnostics UI acceptance"
& powershell.exe `
    -NoProfile `
    -NonInteractive `
    -ExecutionPolicy Bypass `
    -File (Join-Path $PublicUiDirectory "schedule-settings-ui-acceptance.ps1") `
    -AcceptanceScript (Join-Path $PublicUiDirectory "diagnostics-ui-acceptance.ps1") `
    -OutputDirectory $diagnosticsOutput `
    -InteractiveUser $InteractiveUser `
    -InstallDirectory $installDirectory `
    -DataDirectory $dataDirectory `
    -ResultFileName "diagnostics-ui-result.json"
if ($LASTEXITCODE -ne 0) {
    throw "diagnostics UI acceptance failed with exit code $LASTEXITCODE"
}

[pscustomobject]@{
    result = "PASS"
    package = Split-Path -Leaf $PackageDirectory
    service_adaptive = "PASS"
    responsiveness = "PASS"
    bounded_logging = "PASS"
    tray_ui = "PASS"
    tray_v05_about_background = "PASS"
    settings_ui = "PASS"
    diagnostics_ui = "PASS"
    ui_output_directories = [ordered]@{
        tray_control = $trayControlOutput
        tray_v05 = $trayV05Output
        settings = $settingsOutput
        diagnostics = $diagnosticsOutput
    }
} | ConvertTo-Json -Depth 4
