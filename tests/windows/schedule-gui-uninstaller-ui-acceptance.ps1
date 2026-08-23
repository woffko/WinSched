[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$AcceptanceScript,
    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory,
    [Parameter(Mandatory = $true)]
    [string]$InteractiveUser,
    [ValidateSet("Preserve", "Purge")]
    [string]$PurgeChoice = "Preserve",
    [string]$InstallDirectory = "$env:ProgramFiles\WinSched",
    [string]$DataDirectory = "$env:ProgramData\WinSched"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Quote-TaskArgument([string]$Value) {
    if ($Value.Contains('"')) {
        throw "Scheduled-task argument contains an unsupported quote character."
    }
    return '"' + $Value + '"'
}

function Write-SchedulerFailure([string]$Path, [string]$Message) {
    [pscustomobject][ordered]@{
        result = "FAIL"
        scenario = $PurgeChoice
        error = $Message
        source = "schedule-gui-uninstaller-ui-acceptance.ps1"
    } | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $Path -Encoding UTF8
}

$scenario = $PurgeChoice.ToLowerInvariant()
$taskName = "WinSchedGuiUninstallerAcceptance-$PurgeChoice"
$resultPath = Join-Path $OutputDirectory "gui-uninstaller-$scenario-result.json"
$taskRegistered = $false
$result = $null
$failure = $null
$exitCode = 1

New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
Remove-Item -LiteralPath $resultPath -Force -ErrorAction SilentlyContinue

try {
    if (-not (Test-Path -LiteralPath $AcceptanceScript -PathType Leaf)) {
        throw "GUI uninstaller acceptance script is missing: $AcceptanceScript"
    }

    $existingTask = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
    if ($null -ne $existingTask -and $existingTask.State -eq "Running") {
        throw "Scheduled task '$taskName' is already running."
    }
    if ($null -ne $existingTask) {
        Unregister-ScheduledTask -TaskName $taskName -Confirm:$false
    }

    $arguments = @(
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy", "Bypass",
        "-File", (Quote-TaskArgument $AcceptanceScript),
        "-OutputDirectory", (Quote-TaskArgument $OutputDirectory),
        "-PurgeChoice", $PurgeChoice,
        "-InstallDirectory", (Quote-TaskArgument $InstallDirectory),
        "-DataDirectory", (Quote-TaskArgument $DataDirectory)
    ) -join " "

    $action = New-ScheduledTaskAction -Execute "powershell.exe" -Argument $arguments
    $principal = New-ScheduledTaskPrincipal `
        -UserId $InteractiveUser `
        -LogonType Interactive `
        -RunLevel Highest
    $settings = New-ScheduledTaskSettingsSet `
        -AllowStartIfOnBatteries `
        -DontStopIfGoingOnBatteries `
        -ExecutionTimeLimit ([TimeSpan]::FromMinutes(6))

    Register-ScheduledTask `
        -TaskName $taskName `
        -Action $action `
        -Principal $principal `
        -Settings $settings | Out-Null
    $taskRegistered = $true
    Start-ScheduledTask -TaskName $taskName

    $deadline = [DateTime]::UtcNow.AddMinutes(5)
    do {
        if (Test-Path -LiteralPath $resultPath -PathType Leaf) {
            try {
                $result = Get-Content -LiteralPath $resultPath -Raw | ConvertFrom-Json
                break
            } catch {
                $result = $null
            }
        }
        Start-Sleep -Milliseconds 500
    } while ([DateTime]::UtcNow -lt $deadline)

    if ($null -eq $result) {
        throw "Timed out waiting for GUI uninstaller $PurgeChoice acceptance result."
    }
    if ($result.scenario -ne $PurgeChoice) {
        throw "Acceptance result scenario '$($result.scenario)' does not match '$PurgeChoice'."
    }
    if ($result.result -ne "PASS") {
        throw "GUI uninstaller $PurgeChoice acceptance reported FAIL."
    }
    $exitCode = 0
} catch {
    $failure = $_.Exception.ToString()
    $exitCode = 1
    if ($null -eq $result) {
        Write-SchedulerFailure $resultPath $failure
    }
} finally {
    if ($taskRegistered) {
        $exitDeadline = [DateTime]::UtcNow.AddSeconds(10)
        do {
            $task = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
            if ($null -eq $task -or $task.State -ne "Running") {
                break
            }
            Start-Sleep -Milliseconds 250
        } while ([DateTime]::UtcNow -lt $exitDeadline)

        $task = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
        if ($null -ne $task -and $task.State -eq "Running") {
            if ($null -eq $result -and
                -not (Test-Path -LiteralPath $resultPath -PathType Leaf)) {
                Write-SchedulerFailure $resultPath "Acceptance task did not exit after writing no result."
            }
            Stop-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
        }
        Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue
    }
}

if ($null -eq $result -and (Test-Path -LiteralPath $resultPath -PathType Leaf)) {
    try {
        $result = Get-Content -LiteralPath $resultPath -Raw | ConvertFrom-Json
    } catch {
    }
}
if ($null -ne $result) {
    $result | ConvertTo-Json -Depth 6
}
if ($null -ne $failure) {
    [Console]::Error.WriteLine($failure)
}
exit $exitCode
