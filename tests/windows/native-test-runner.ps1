[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$TestDirectory,
    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory,
    [string]$InteractiveUser,
    [string[]]$TestNames,
    [string]$ServiceName = "WinSched",
    [switch]$Worker,
    [string]$ManifestPath,
    [string]$WorkerResultPath
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw "ASSERTION FAILED: $Message" }
}

function Wait-Condition([string]$Description, [scriptblock]$Condition, [int]$TimeoutSeconds) {
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        if (& $Condition) { return }
        Start-Sleep -Milliseconds 250
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Timed out waiting for: $Description"
}

function Write-Json([string]$Path, $Value) {
    $parent = Split-Path -Parent $Path
    New-Item -ItemType Directory -Path $parent -Force | Out-Null
    $temporaryPath = Join-Path $parent (
        ".{0}.{1}.{2}.tmp" -f `
            (Split-Path -Leaf $Path), `
            $PID, `
            [Guid]::NewGuid().ToString("N")
    )
    $replacementBackup = "$temporaryPath.backup"
    [IO.File]::WriteAllText(
        $temporaryPath,
        ([pscustomobject]$Value | ConvertTo-Json -Depth 8) + "`n",
        [Text.UTF8Encoding]::new($false)
    )
    try {
        if (Test-Path -LiteralPath $Path -PathType Leaf) {
            [IO.File]::Replace($temporaryPath, $Path, $replacementBackup, $true)
        } else {
            [IO.File]::Move($temporaryPath, $Path)
        }
    } finally {
        foreach ($cleanupPath in @($temporaryPath, $replacementBackup)) {
            Remove-Item -LiteralPath $cleanupPath -Force -ErrorAction SilentlyContinue
        }
    }
}

function Read-SharedUtf8([string]$Path) {
    $stream = [IO.File]::Open(
        $Path,
        [IO.FileMode]::Open,
        [IO.FileAccess]::Read,
        ([IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete)
    )
    try {
        $reader = [IO.StreamReader]::new(
            $stream,
            [Text.Encoding]::UTF8,
            $true,
            4096,
            $true
        )
        try { return $reader.ReadToEnd() } finally { $reader.Dispose() }
    } finally {
        $stream.Dispose()
    }
}

function Invoke-Worker {
    Assert-True ([Diagnostics.Process]::GetCurrentProcess().SessionId -gt 0) `
        "native tests must run in an interactive session"
    Assert-True (Test-Path -LiteralPath $ManifestPath -PathType Leaf) `
        "native-test manifest is missing"
    $entries = New-Object System.Collections.ArrayList
    $totalPassed = 0
    $totalFailed = 0
    $failed = $false
    $selectedNames = @(Get-Content -LiteralPath $ManifestPath | Where-Object {
        -not [string]::IsNullOrWhiteSpace($_)
    })
    foreach ($manifestName in $selectedNames) {
        $name = [string]$manifestName
        if ([string]::IsNullOrWhiteSpace($name)) { continue }
        Assert-True ($name -match '^[A-Za-z0-9_.-]+\.exe$') "invalid native-test filename"
        $path = Join-Path $TestDirectory $name
        Assert-True (Test-Path -LiteralPath $path -PathType Leaf) "native-test binary is missing: $name"
        $output = (& $path --test-threads=1 2>&1 | Out-String)
        $exitCode = $LASTEXITCODE
        [IO.File]::WriteAllText(
            (Join-Path $OutputDirectory "$name.log"),
            $output,
            [Text.UTF8Encoding]::new($false)
        )
        $summary = [regex]::Match(
            $output,
            'test result: (?<result>ok|FAILED)\. (?<passed>\d+) passed; (?<failed>\d+) failed;'
        )
        $passed = if ($summary.Success) { [int]$summary.Groups['passed'].Value } else { 0 }
        $failures = if ($summary.Success) { [int]$summary.Groups['failed'].Value } else { 1 }
        $entryPassed = $exitCode -eq 0 -and $summary.Success -and
            $summary.Groups['result'].Value -eq 'ok' -and $failures -eq 0
        [void]$entries.Add([ordered]@{
            name = $name
            result = if ($entryPassed) { "PASS" } else { "FAIL" }
            exit_code = $exitCode
            passed = $passed
            failed = $failures
        })
        $totalPassed += $passed
        $totalFailed += $failures
        if (-not $entryPassed) { $failed = $true }
    }
    $result = [ordered]@{
        result = if ($failed) { "FAIL" } else { "PASS" }
        interactive_session = [Diagnostics.Process]::GetCurrentProcess().SessionId
        executables = @($entries)
        total_passed = $totalPassed
        total_failed = $totalFailed
    }
    Write-Json $WorkerResultPath $result
    [pscustomobject]$result | ConvertTo-Json -Depth 8
    if ($failed) { exit 1 }
    return
}

if ($Worker) {
    try {
        Invoke-Worker
    } catch {
        $failure = [ordered]@{
            result = "FAIL"
            interactive_session = [Diagnostics.Process]::GetCurrentProcess().SessionId
            executables = @()
            total_passed = 0
            total_failed = 1
            infrastructure_error = $_.Exception.ToString()
            script_stack = $_.ScriptStackTrace
        }
        if (-not [string]::IsNullOrWhiteSpace($WorkerResultPath)) {
            Write-Json $WorkerResultPath $failure
        }
        [pscustomobject]$failure | ConvertTo-Json -Depth 8
        exit 1
    }
    return
}

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
Assert-True ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) `
    "native-test controller requires an elevated shell"
Assert-True (Test-Path -LiteralPath $TestDirectory -PathType Container) `
    "native-test directory is missing"
if ([string]::IsNullOrWhiteSpace($InteractiveUser)) {
    $InteractiveUser = (Get-CimInstance Win32_ComputerSystem).UserName
}
Assert-True (-not [string]::IsNullOrWhiteSpace($InteractiveUser)) `
    "an interactive user is required"

New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$runId = [Guid]::NewGuid().ToString("N").Substring(0, 12)
$taskName = "WinSchedNativeTests-$runId"
$manifest = Join-Path $OutputDirectory "native-tests-$runId.txt"
$workerResult = Join-Path $OutputDirectory "native-tests-worker-$runId.json"
$resultPath = Join-Path $OutputDirectory "native-tests-result.json"
$zeroTestExecutablesSkipped = New-Object System.Collections.ArrayList
$names = @(
    if ($null -ne $TestNames -and @($TestNames).Count -gt 0) {
        @($TestNames)
    } else {
        foreach ($file in @(Get-ChildItem -LiteralPath $TestDirectory -Filter *.exe -File |
            Sort-Object Name)) {
            $listOutput = (& $file.FullName --list 2>&1 | Out-String)
            Assert-True ($LASTEXITCODE -eq 0) "could not enumerate native tests: $($file.Name)"
            $testCount = [regex]::Matches($listOutput, '(?m): test\r?$').Count
            if ($testCount -gt 0) {
                $file.Name
            } else {
                [void]$zeroTestExecutablesSkipped.Add($file.Name)
            }
        }
    }
)
Assert-True ($names.Count -gt 0) "no native-test executables were selected"
[IO.File]::WriteAllLines($manifest, $names, [Text.UTF8Encoding]::new($false))

$service = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
$serviceWasRunning = $null -ne $service -and $service.Status -ne "Stopped"
$taskRegistered = $false
$mainError = $null
$cleanupErrors = New-Object System.Collections.ArrayList
$workerReceipt = $null

try {
    if ($serviceWasRunning) {
        Stop-Service -Name $ServiceName -Force
        Wait-Condition "$ServiceName service stopped" {
            (Get-Service -Name $ServiceName).Status -eq "Stopped"
        } 30
    }
    Remove-Item -LiteralPath $workerResult -Force -ErrorAction SilentlyContinue
    $powershell = "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe"
    $arguments = @(
        '-NoLogo', '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass',
        '-File', ('"{0}"' -f $PSCommandPath),
        '-Worker',
        '-TestDirectory', ('"{0}"' -f $TestDirectory),
        '-OutputDirectory', ('"{0}"' -f $OutputDirectory),
        '-ManifestPath', ('"{0}"' -f $manifest),
        '-WorkerResultPath', ('"{0}"' -f $workerResult)
    ) -join ' '
    $action = New-ScheduledTaskAction -Execute $powershell -Argument $arguments
    $taskPrincipal = New-ScheduledTaskPrincipal `
        -UserId $InteractiveUser `
        -LogonType Interactive `
        -RunLevel Highest
    $settings = New-ScheduledTaskSettingsSet `
        -AllowStartIfOnBatteries `
        -DontStopIfGoingOnBatteries `
        -ExecutionTimeLimit ([TimeSpan]::FromMinutes(20))
    Register-ScheduledTask `
        -TaskName $taskName `
        -Action $action `
        -Principal $taskPrincipal `
        -Settings $settings | Out-Null
    $taskRegistered = $true
    Start-ScheduledTask -TaskName $taskName
    $workerStartedAt = [DateTime]::UtcNow
    Write-Host ("native tests: task scheduled with {0} executable(s)" -f $names.Count)
    $deadline = $workerStartedAt.AddSeconds(900)
    do {
        if (Test-Path -LiteralPath $workerResult -PathType Leaf) { break }
        if (([DateTime]::UtcNow - $workerStartedAt).TotalSeconds -ge 15) {
            $task = Get-ScheduledTask -TaskName $taskName
            $taskInfo = $task | Get-ScheduledTaskInfo
            if ($task.State -eq "Ready" -and $taskInfo.LastRunTime.Year -le 2000) {
                throw "interactive native-test task did not start"
            }
            if ($task.State -eq "Ready" -and
                -not (Test-Path -LiteralPath $workerResult -PathType Leaf)) {
                throw "interactive native-test worker exited without a receipt: $($taskInfo.LastTaskResult)"
            }
        }
        Start-Sleep -Milliseconds 500
    } while ([DateTime]::UtcNow -lt $deadline)
    Assert-True (Test-Path -LiteralPath $workerResult -PathType Leaf) `
        "timed out waiting for interactive native-test worker receipt"
    $script:nativeWorkerReceipt = $null
    Wait-Condition "complete native-test worker JSON" {
        try {
            $script:nativeWorkerReceipt = Read-SharedUtf8 $workerResult |
                ConvertFrom-Json
            return $null -ne $script:nativeWorkerReceipt
        } catch {
            return $false
        }
    } 5
    $workerReceipt = $script:nativeWorkerReceipt
    Assert-True ([string]$workerReceipt.result -eq "PASS") "one or more native tests failed"
} catch {
    $mainError = $_.Exception.ToString()
} finally {
    if ($taskRegistered) {
        Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue
    }
    if ($serviceWasRunning) {
        try {
            Start-Service -Name $ServiceName
            Wait-Condition "$ServiceName service restored" {
                (Get-Service -Name $ServiceName).Status -eq "Running"
            } 45
        } catch {
            [void]$cleanupErrors.Add($_.Exception.Message)
        }
    }
}

$passed = $null -eq $mainError -and $cleanupErrors.Count -eq 0
$result = [ordered]@{
    result = if ($passed) { "PASS" } else { "FAIL" }
    worker = $workerReceipt
    zero_test_executables_skipped = @($zeroTestExecutablesSkipped)
    service_was_running = $serviceWasRunning
    service_state_restored = $cleanupErrors.Count -eq 0
    cleanup_errors = @($cleanupErrors)
    error = $mainError
}
Write-Json $resultPath $result
Remove-Item -LiteralPath $manifest -Force -ErrorAction SilentlyContinue
[pscustomobject]$result | ConvertTo-Json -Depth 10
if (-not $passed) { exit 1 }
