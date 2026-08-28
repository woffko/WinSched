[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$TestDirectory,
    [Parameter(Mandatory = $true)]
    [string]$InstallDirectory,
    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory,
    [string]$InteractiveUser
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw "ASSERTION FAILED: $Message" }
}

function Wait-Condition([string]$Description, [scriptblock]$Condition, [int]$TimeoutSeconds = 120) {
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        if (& $Condition) { return }
        Start-Sleep -Milliseconds 250
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Timed out waiting for: $Description"
}

function Invoke-InteractiveAcceptance(
    [string]$Label,
    [string]$ScriptName,
    [string]$ReceiptName,
    [ValidateSet("Limited", "Highest")]
    [string]$RunLevel
) {
    $receipt = Join-Path $OutputDirectory $ReceiptName
    Remove-Item -LiteralPath $receipt -Force -ErrorAction SilentlyContinue
    $taskName = "WinSchedMonitor-$Label-$([Guid]::NewGuid().ToString('N').Substring(0, 10))"
    $powershell = "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe"
    $arguments = @(
        '-NoLogo', '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass',
        '-File', ('"{0}"' -f (Join-Path $TestDirectory $ScriptName)),
        '-InstallDirectory', ('"{0}"' -f $InstallDirectory),
        '-OutputDirectory', ('"{0}"' -f $OutputDirectory)
    ) -join ' '
    $action = New-ScheduledTaskAction -Execute $powershell -Argument $arguments
    $principal = New-ScheduledTaskPrincipal `
        -UserId $InteractiveUser `
        -LogonType Interactive `
        -RunLevel $RunLevel
    $settings = New-ScheduledTaskSettingsSet `
        -AllowStartIfOnBatteries `
        -DontStopIfGoingOnBatteries `
        -ExecutionTimeLimit ([TimeSpan]::FromMinutes(5))
    try {
        Register-ScheduledTask `
            -TaskName $taskName `
            -Action $action `
            -Principal $principal `
            -Settings $settings | Out-Null
        Start-ScheduledTask -TaskName $taskName
        Wait-Condition "$Label receipt" {
            Test-Path -LiteralPath $receipt -PathType Leaf
        } 180
        $result = Get-Content -LiteralPath $receipt -Raw | ConvertFrom-Json
        Assert-True ([string]$result.result -eq "PASS") "$Label acceptance failed"
        return $result
    } finally {
        Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue
    }
}

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
Assert-True ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) `
    "controller must run elevated"
if ([string]::IsNullOrWhiteSpace($InteractiveUser)) {
    $InteractiveUser = (Get-CimInstance Win32_ComputerSystem).UserName
}
Assert-True (-not [string]::IsNullOrWhiteSpace($InteractiveUser)) `
    "interactive user is unavailable"
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null

$ui = Invoke-InteractiveAcceptance `
    "Ui" `
    "process-monitor-ui-acceptance.ps1" `
    "process-monitor-ui-result.json" `
    "Limited"
$tray = Invoke-InteractiveAcceptance `
    "Tray" `
    "tray-ui-acceptance.ps1" `
    "tray-ui-result.json" `
    "Limited"
$handoff = Invoke-InteractiveAcceptance `
    "Rule" `
    "process-monitor-rule-handoff-acceptance.ps1" `
    "process-monitor-rule-handoff-result.json" `
    "Highest"

$result = [ordered]@{
    result = "PASS"
    ui = $ui
    tray = $tray
    rule_handoff = $handoff
}
$resultPath = Join-Path $OutputDirectory "process-monitor-acceptance-result.json"
[IO.File]::WriteAllText(
    $resultPath,
    ([pscustomobject]$result | ConvertTo-Json -Depth 8) + "`n",
    [Text.UTF8Encoding]::new($false)
)
[pscustomobject]$result | ConvertTo-Json -Depth 8
