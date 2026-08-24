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

Write-Host "final stage: service and adaptive acceptance"
& (Join-Path $TestDirectory "full-acceptance.ps1") `
    -PackageDirectory $PackageDirectory `
    -InteractiveUser $InteractiveUser

Write-Host "final stage: bounded logging acceptance"
& (Join-Path $TestDirectory "logging-acceptance.ps1")

Write-Host "final stage: lifecycle acceptance"
& (Join-Path $TestDirectory "lifecycle-acceptance.ps1") `
    -PackageDirectory $PackageDirectory `
    -InteractiveUser $InteractiveUser

Write-Host "final stage: tray UI acceptance"
& powershell.exe `
    -NoProfile `
    -NonInteractive `
    -ExecutionPolicy Bypass `
    -File (Join-Path $PublicUiDirectory "schedule-tray-ui-acceptance.ps1") `
    -AcceptanceScript (Join-Path $PublicUiDirectory "tray-ui-acceptance.ps1") `
    -OutputDirectory (Join-Path $PublicUiDirectory "output") `
    -InteractiveUser $InteractiveUser
if ($LASTEXITCODE -ne 0) {
    throw "tray UI acceptance failed with exit code $LASTEXITCODE"
}

[pscustomobject]@{
    result = "PASS"
    package = Split-Path -Leaf $PackageDirectory
    service_adaptive = "PASS"
    bounded_logging = "PASS"
    lifecycle = "PASS"
    tray_ui = "PASS"
} | ConvertTo-Json -Depth 4
