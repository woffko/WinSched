[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$AcceptanceScript,
    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory,
    [Parameter(Mandatory = $true)]
    [string]$InteractiveUser,
    [string]$InstallDirectory = (Join-Path ([Environment]::GetFolderPath("ProgramFiles")) "WinSched"),
    [string]$DataDirectory = (Join-Path ([Environment]::GetFolderPath("CommonApplicationData")) "WinSched"),
    [string]$ExpectedVersion = "0.6.0"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$taskName = "WinSchedTrayAcceptance"
$resultPath = Join-Path $OutputDirectory "tray-ui-result.json"
Remove-Item -LiteralPath $resultPath -Force -ErrorAction SilentlyContinue

$arguments = @(
    "-NoProfile",
    "-NonInteractive",
    "-ExecutionPolicy", "Bypass",
    "-File", "`"$AcceptanceScript`"",
    "-InstallDirectory", "`"$InstallDirectory`"",
    "-DataDirectory", "`"$DataDirectory`"",
    "-OutputDirectory", "`"$OutputDirectory`""
)
$acceptanceSource = Get-Content -LiteralPath $AcceptanceScript -Raw
if ($acceptanceSource -match '(?m)\$ExpectedVersion\b') {
    $arguments += @("-ExpectedVersion", "`"$ExpectedVersion`"")
}
$arguments = $arguments -join " "
$action = New-ScheduledTaskAction -Execute "powershell.exe" -Argument $arguments
$principal = New-ScheduledTaskPrincipal `
    -UserId $InteractiveUser `
    -LogonType Interactive `
    -RunLevel Limited
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
    throw "Timed out waiting for interactive tray acceptance result."
} finally {
    Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue
}
