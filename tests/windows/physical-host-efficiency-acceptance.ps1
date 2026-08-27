[CmdletBinding()]
param(
    [ValidateRange(10, 300)]
    [int]$DurationSeconds = 30,
    [string]$DataDirectory = "$env:ProgramData\WinSched",
    [string]$ResultPath = "$env:PUBLIC\WinSchedV051Host\logging-off-efficiency-result.json"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw "ASSERTION FAILED: $Message" }
}

function Read-Status {
    return Get-Content -LiteralPath (Join-Path $DataDirectory "status.json") -Raw |
        ConvertFrom-Json
}

function Get-ProcessSnapshot([int]$ProcessId) {
    $process = Get-CimInstance Win32_Process -Filter "ProcessId=$ProcessId"
    Assert-True ($null -ne $process) "service process is missing"
    return [pscustomobject]@{
        pid = [int]$process.ProcessId
        kernel_time_100ns = [uint64]$process.KernelModeTime
        user_time_100ns = [uint64]$process.UserModeTime
        read_operations = [uint64]$process.ReadOperationCount
        write_operations = [uint64]$process.WriteOperationCount
        read_bytes = [uint64]$process.ReadTransferCount
        write_bytes = [uint64]$process.WriteTransferCount
        working_set_bytes = [uint64]$process.WorkingSetSize
        handles = [uint64]$process.HandleCount
        threads = [uint64]$process.ThreadCount
    }
}

function Get-LogSnapshot {
    return @(
        Get-ChildItem -LiteralPath $DataDirectory -Filter "winsched.log*" -File `
            -ErrorAction SilentlyContinue |
            Where-Object Name -Match '^winsched\.log(?:\.\d+)?$' |
            Sort-Object Name |
            ForEach-Object {
                [pscustomobject]@{
                    name = $_.Name
                    length = [uint64]$_.Length
                    sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
                }
            }
    )
}

function Get-UnsignedDelta($Before, $After) {
    $left = [uint64]$Before
    $right = [uint64]$After
    Assert-True ($right -ge $left) "monotonic counter regressed"
    return [uint64]($right - $left)
}

$resultDirectory = Split-Path -Parent $ResultPath
New-Item -ItemType Directory -Path $resultDirectory -Force | Out-Null
$beforeStatus = Read-Status
Assert-True ([int]$beforeStatus.schema_version -eq 5) "status schema 5 is required"
Assert-True ([string]$beforeStatus.applied_logging.level -eq "off") `
    "logging must be Off for this acceptance"
Assert-True ($null -eq $beforeStatus.last_error) "service reports an error before measurement"
Assert-True ($null -ne $beforeStatus.telemetry) "controller telemetry is missing"
$servicePid = [int]$beforeStatus.service_pid
$beforeProcess = Get-ProcessSnapshot $servicePid
$beforeLogs = @(Get-LogSnapshot)
$stopwatch = [Diagnostics.Stopwatch]::StartNew()
Start-Sleep -Seconds $DurationSeconds
$stopwatch.Stop()
$afterStatus = Read-Status
$afterProcess = Get-ProcessSnapshot $servicePid
$afterLogs = @(Get-LogSnapshot)

Assert-True ([int]$afterStatus.service_pid -eq $servicePid) "service restarted during measurement"
Assert-True ([string]$afterStatus.applied_logging.level -eq "off") "logging level changed"
Assert-True ($null -eq $afterStatus.last_error) "service reports an error after measurement"
$beforeLogJson = $beforeLogs | ConvertTo-Json -Depth 4 -Compress
$afterLogJson = $afterLogs | ConvertTo-Json -Depth 4 -Compress
Assert-True ($beforeLogJson -ceq $afterLogJson) "Off service log files changed"

$duration = $stopwatch.Elapsed.TotalSeconds
$cpuTimeDelta = (Get-UnsignedDelta $beforeProcess.kernel_time_100ns $afterProcess.kernel_time_100ns) +
    (Get-UnsignedDelta $beforeProcess.user_time_100ns $afterProcess.user_time_100ns)
$logicalProcessors = [Environment]::ProcessorCount
$oneCorePercent = 100.0 * [double]$cpuTimeDelta / 10000000.0 / $duration
$machinePercent = $oneCorePercent / [Math]::Max(1, $logicalProcessors)
$writeOperations = Get-UnsignedDelta $beforeProcess.write_operations $afterProcess.write_operations
$writeBytes = Get-UnsignedDelta $beforeProcess.write_bytes $afterProcess.write_bytes
$readOperations = Get-UnsignedDelta $beforeProcess.read_operations $afterProcess.read_operations
$readBytes = Get-UnsignedDelta $beforeProcess.read_bytes $afterProcess.read_bytes
$logging = $afterStatus.telemetry.logging
$evaluation = $afterStatus.telemetry.evaluation

Assert-True ([uint64]$logging.records_written -eq 0) "Off telemetry counted file log records"
Assert-True ([uint64]$logging.bytes_written -eq 0) "Off telemetry counted file log bytes"
Assert-True ([uint64]$logging.write_errors -eq 0) "file logger reports write errors"
Assert-True ([uint64]$evaluation.completed_total -gt 0) "controller evaluation telemetry is empty"

$result = [ordered]@{
    result = "PASS"
    duration_seconds = $duration
    service_pid_unchanged = $true
    logical_processors = $logicalProcessors
    cpu = [ordered]@{
        time_100ns = $cpuTimeDelta
        one_core_percent = $oneCorePercent
        machine_capacity_percent = $machinePercent
    }
    io = [ordered]@{
        read_operations = $readOperations
        read_operations_per_second = [double]$readOperations / $duration
        read_bytes = $readBytes
        read_bytes_per_second = [double]$readBytes / $duration
        write_operations = $writeOperations
        write_operations_per_second = [double]$writeOperations / $duration
        write_bytes = $writeBytes
        write_bytes_per_second = [double]$writeBytes / $duration
    }
    process = [ordered]@{
        working_set_mib = [double]$afterProcess.working_set_bytes / 1MB
        handles = $afterProcess.handles
        threads = $afterProcess.threads
    }
    controller = [ordered]@{
        evaluations_completed_total = [uint64]$evaluation.completed_total
        rolling_mean_us = [uint64]$evaluation.rolling_mean_us
        rolling_p95_us = [uint64]$evaluation.rolling_p95_us
        rolling_max_us = [uint64]$evaluation.rolling_max_us
        last_scanned_processes = [int]$evaluation.last_scanned_processes
        last_eligible_processes = [int]$evaluation.last_eligible_processes
        last_decisions = [int]$evaluation.last_decisions
        status_writes = [uint64]$logging.status_writes
    }
    logging = [ordered]@{
        level = "off"
        records_written = [uint64]$logging.records_written
        bytes_written = [uint64]$logging.bytes_written
        write_errors = [uint64]$logging.write_errors
        existing_files_byte_stable = $true
    }
}
[IO.File]::WriteAllText(
    $ResultPath,
    ([pscustomobject]$result | ConvertTo-Json -Depth 8) + "`n",
    [Text.UTF8Encoding]::new($false)
)
[pscustomobject]$result | ConvertTo-Json -Depth 8
