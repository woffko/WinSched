[CmdletBinding()]
param(
    [string]$InstallDirectory = "$env:ProgramFiles\WinSched",
    [string]$DataDirectory = "$env:ProgramData\WinSched",
    [ValidateRange(10, 300)]
    [int]$DurationSeconds = 30,
    [string]$ResultPath = "$env:PUBLIC\WinSchedDisabledIdle\disabled-idle-result.json"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Assert-True([bool]$Condition, [string]$Code) {
    if (-not $Condition) { throw $Code }
}

function Wait-Condition([string]$Code, [scriptblock]$Condition, [int]$TimeoutSeconds = 45) {
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        if (& $Condition) { return }
        Start-Sleep -Milliseconds 250
    } while ([DateTime]::UtcNow -lt $deadline)
    throw $Code
}

function Read-Status {
    $path = Join-Path $DataDirectory "status.json"
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { return $null }
    try { return Get-Content -LiteralPath $path -Raw | ConvertFrom-Json } catch { return $null }
}

function Get-ProcessSnapshot([int]$ProcessId) {
    $process = Get-CimInstance Win32_Process -Filter "ProcessId=$ProcessId"
    Assert-True ($null -ne $process) "DISABLED_IDLE_SERVICE_PROCESS_MISSING"
    return [pscustomobject]@{
        kernel_time_100ns = [uint64]$process.KernelModeTime
        user_time_100ns = [uint64]$process.UserModeTime
        write_operations = [uint64]$process.WriteOperationCount
        write_bytes = [uint64]$process.WriteTransferCount
    }
}

function Get-Delta($Before, $After) {
    $left = [uint64]$Before
    $right = [uint64]$After
    Assert-True ($right -ge $left) "DISABLED_IDLE_COUNTER_REGRESSION"
    return [uint64]($right - $left)
}

function Set-Scheduling([bool]$Enabled, [string]$ServiceBinary, [int]$ServicePid) {
    $command = if ($Enabled) { "enable" } else { "disable" }
    $status = Read-Status
    Assert-True ($null -ne $status -and [int]$status.service_pid -eq $ServicePid) `
        "DISABLED_IDLE_SERVICE_RESTARTED"
    if ([bool]$status.scheduling_enabled -ne $Enabled) {
        & $ServiceBinary $command | Out-Null
        Assert-True ($LASTEXITCODE -eq 0) "DISABLED_IDLE_CONTROL_COMMAND_FAILED"
    }
    Wait-Condition "DISABLED_IDLE_CONTROL_RECEIPT_TIMEOUT" {
        $current = Read-Status
        $null -ne $current -and
            [int]$current.service_pid -eq $ServicePid -and
            [bool]$current.scheduling_enabled -eq $Enabled -and
            $null -eq $current.last_error
    }
}

function Write-Result($Value) {
    $parent = Split-Path -Parent $ResultPath
    if (-not [string]::IsNullOrWhiteSpace($parent)) {
        New-Item -ItemType Directory -Path $parent -Force | Out-Null
    }
    [IO.File]::WriteAllText(
        $ResultPath,
        ([pscustomobject]$Value | ConvertTo-Json -Depth 8) + "`n",
        [Text.UTF8Encoding]::new($false)
    )
}

$serviceBinary = Join-Path $InstallDirectory "winsched-service.exe"
$initialScheduling = $null
$servicePid = 0
$mainError = $null
$restoreError = $null
$measurement = $null

try {
    Assert-True (Test-Path -LiteralPath $serviceBinary -PathType Leaf) `
        "DISABLED_IDLE_SERVICE_BINARY_MISSING"
    $service = Get-Service -Name "WinSched" -ErrorAction SilentlyContinue
    Assert-True ($null -ne $service -and $service.Status -eq "Running") `
        "DISABLED_IDLE_SERVICE_NOT_RUNNING"
    $initial = Read-Status
    Assert-True ($null -ne $initial -and [int]$initial.schema_version -eq 5) `
        "DISABLED_IDLE_STATUS_SCHEMA"
    Assert-True ($null -eq $initial.last_error) "DISABLED_IDLE_INITIAL_SERVICE_ERROR"
    $servicePid = [int]$initial.service_pid
    $initialScheduling = [bool]$initial.scheduling_enabled

    Set-Scheduling $false $serviceBinary $servicePid
    Start-Sleep -Seconds 2
    $beforeStatus = Read-Status
    $beforeProcess = Get-ProcessSnapshot $servicePid
    $stopwatch = [Diagnostics.Stopwatch]::StartNew()
    Start-Sleep -Seconds $DurationSeconds
    $stopwatch.Stop()
    $afterProcess = Get-ProcessSnapshot $servicePid
    $afterStatus = Read-Status

    Assert-True ([int]$afterStatus.service_pid -eq $servicePid) `
        "DISABLED_IDLE_SERVICE_RESTARTED"
    Assert-True (-not [bool]$afterStatus.scheduling_enabled) `
        "DISABLED_IDLE_SCHEDULING_STATE_DRIFT"
    Assert-True ([string]$afterStatus.phase -eq "disabled") "DISABLED_IDLE_PHASE_DRIFT"
    Assert-True ($null -eq $afterStatus.last_error) "DISABLED_IDLE_SERVICE_ERROR"

    $cpuTime = (Get-Delta $beforeProcess.kernel_time_100ns $afterProcess.kernel_time_100ns) +
        (Get-Delta $beforeProcess.user_time_100ns $afterProcess.user_time_100ns)
    $duration = $stopwatch.Elapsed.TotalSeconds
    $oneCorePercent = 100.0 * [double]$cpuTime / 10000000.0 / $duration
    $writeOperations = Get-Delta $beforeProcess.write_operations $afterProcess.write_operations
    $writeBytes = Get-Delta $beforeProcess.write_bytes $afterProcess.write_bytes
    $evaluationDelta = Get-Delta `
        $beforeStatus.telemetry.evaluation.completed_total `
        $afterStatus.telemetry.evaluation.completed_total
    $statusWriteDelta = Get-Delta `
        $beforeStatus.telemetry.logging.status_writes `
        $afterStatus.telemetry.logging.status_writes

    Assert-True ($evaluationDelta -eq 0) "DISABLED_IDLE_POLICY_EVALUATED"
    Assert-True ($oneCorePercent -lt 5.0) "DISABLED_IDLE_CPU_SPIN"
    Assert-True (($writeOperations / $duration) -lt 5.0) "DISABLED_IDLE_WRITE_SPIN"
    Assert-True ($statusWriteDelta -le ([Math]::Ceiling($duration / 10.0) + 2)) `
        "DISABLED_IDLE_STATUS_HEARTBEAT_EXCESS"

    $measurement = [ordered]@{
        duration_seconds = $duration
        cpu_one_core_percent = $oneCorePercent
        write_operations = $writeOperations
        write_operations_per_second = [double]$writeOperations / $duration
        write_bytes = $writeBytes
        evaluations_completed = $evaluationDelta
        status_writes = $statusWriteDelta
        service_pid_unchanged = $true
    }
} catch {
    $mainError = [string]$_.Exception.Message
} finally {
    if ($null -ne $initialScheduling -and $servicePid -gt 0) {
        try {
            Set-Scheduling $initialScheduling $serviceBinary $servicePid
        } catch {
            $restoreError = [string]$_.Exception.Message
        }
    }
}

$result = [ordered]@{
    result = if ($null -eq $mainError -and $null -eq $restoreError) { "PASS" } else { "FAIL" }
    error = $mainError
    restore_error = $restoreError
    initial_scheduling_restored = $null -eq $restoreError -and $null -ne $initialScheduling
    measurement = $measurement
}
Write-Result $result
[pscustomobject]$result | ConvertTo-Json -Depth 8
if ([string]$result.result -ne "PASS") { exit 1 }
