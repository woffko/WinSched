[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$AcceptanceScript,
    [Parameter(Mandatory = $true)]
    [string]$SetupPath,
    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory,
    [Parameter(Mandatory = $true)]
    [string]$InteractiveUser,
    [string]$ExpectedConfigMarker = ""
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$taskName = "WinSchedGuiInstallerAcceptance"
$resultPath = Join-Path $OutputDirectory "gui-installer-result.json"
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
Remove-Item -LiteralPath $resultPath -Force -ErrorAction SilentlyContinue
$quote = [char]34
$arguments = "-NoProfile -NonInteractive -ExecutionPolicy Bypass -File $quote$AcceptanceScript$quote -SetupPath $quote$SetupPath$quote -OutputDirectory $quote$OutputDirectory$quote -ExpectedConfigMarker $quote$ExpectedConfigMarker$quote"
$action = New-ScheduledTaskAction -Execute "powershell.exe" -Argument $arguments
$principal = New-ScheduledTaskPrincipal `
    -UserId $InteractiveUser `
    -LogonType Interactive `
    -RunLevel Highest
$settings = New-ScheduledTaskSettingsSet `
    -AllowStartIfOnBatteries `
    -DontStopIfGoingOnBatteries `
    -ExecutionTimeLimit ([TimeSpan]::FromMinutes(5))

Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue
Register-ScheduledTask `
    -TaskName $taskName `
    -Action $action `
    -Principal $principal `
    -Settings $settings | Out-Null
Start-ScheduledTask -TaskName $taskName

$deadline = [DateTime]::UtcNow.AddMinutes(4)
try {
    do {
        if (Test-Path -LiteralPath $resultPath -PathType Leaf) {
            $result = Get-Content -LiteralPath $resultPath -Raw | ConvertFrom-Json
            $result | ConvertTo-Json -Depth 6
            if ($result.result -ne "PASS") {
                exit 1
            }
            exit 0
        }
        Start-Sleep -Milliseconds 500
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Timed out waiting for GUI installer acceptance result."
} finally {
    Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue
}
