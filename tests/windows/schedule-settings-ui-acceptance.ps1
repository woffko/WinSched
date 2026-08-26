[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$AcceptanceScript,
    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory,
    [Parameter(Mandatory = $true)]
    [string]$InteractiveUser,
    [string]$InstallDirectory = "$env:ProgramFiles\WinSched",
    [string]$DataDirectory = "$env:ProgramData\WinSched",
    [string]$ResultFileName = "settings-ui-result.json"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$taskName = "WinSchedSettingsUiAcceptance"
$resultPath = Join-Path $OutputDirectory $ResultFileName
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
Remove-Item -LiteralPath $resultPath -Force -ErrorAction SilentlyContinue

$quote = [char]34
$arguments = @(
    "-NoProfile"
    "-NonInteractive"
    "-ExecutionPolicy Bypass"
    "-File $quote$AcceptanceScript$quote"
    "-OutputDirectory $quote$OutputDirectory$quote"
    "-InstallDirectory $quote$InstallDirectory$quote"
    "-DataDirectory $quote$DataDirectory$quote"
)
$acceptanceSource = Get-Content -LiteralPath $AcceptanceScript -Raw
if ($acceptanceSource -match '(?m)\$ResultFileName\b') {
    $arguments += "-ResultFileName $quote$ResultFileName$quote"
}
$arguments = $arguments -join " "
$action = New-ScheduledTaskAction -Execute "powershell.exe" -Argument $arguments
$principal = New-ScheduledTaskPrincipal `
    -UserId $InteractiveUser `
    -LogonType Interactive `
    -RunLevel Highest
$settings = New-ScheduledTaskSettingsSet `
    -AllowStartIfOnBatteries `
    -DontStopIfGoingOnBatteries `
    -ExecutionTimeLimit ([TimeSpan]::FromMinutes(8))

Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue
Register-ScheduledTask `
    -TaskName $taskName `
    -Action $action `
    -Principal $principal `
    -Settings $settings | Out-Null
Start-ScheduledTask -TaskName $taskName

$deadline = [DateTime]::UtcNow.AddMinutes(7)
try {
    do {
        if (Test-Path -LiteralPath $resultPath -PathType Leaf) {
            try {
                $result = Get-Content -LiteralPath $resultPath -Raw | ConvertFrom-Json
                if ($result.cleanup_completed -eq $true) {
                    $result | ConvertTo-Json -Depth 8
                    if ($result.result -ne "PASS") {
                        exit 1
                    }
                    exit 0
                }
            } catch {
                # The interactive writer may be replacing the JSON; retry until it is complete.
            }
        }
        Start-Sleep -Milliseconds 500
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Timed out waiting for interactive settings UI acceptance cleanup."
} finally {
    Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue
}
