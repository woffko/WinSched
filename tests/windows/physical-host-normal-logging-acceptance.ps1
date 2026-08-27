[CmdletBinding()]
param(
    [ValidateRange(60, 300)]
    [int]$DurationSeconds = 75,
    [string]$DataDirectory = "$env:ProgramData\WinSched",
    [string]$ResultPath = "$env:PUBLIC\WinSchedV051Host\normal-logging-result.json"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw "ASSERTION FAILED: $Message" }
}

function Wait-Condition([string]$Description, [scriptblock]$Condition, [int]$TimeoutSeconds = 60) {
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        if (& $Condition) { return }
        Start-Sleep -Milliseconds 250
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Timed out waiting for: $Description"
}

function Read-Status {
    return Get-Content -LiteralPath (Join-Path $DataDirectory "status.json") -Raw |
        ConvertFrom-Json
}

function Set-FileAtomically([string]$Path, [byte[]]$Bytes) {
    $directory = Split-Path -Parent $Path
    $temporaryPath = Join-Path $directory (
        ".{0}.normal-log-{1}.tmp" -f (Split-Path -Leaf $Path), [Guid]::NewGuid().ToString("N")
    )
    $replacementBackup = "$temporaryPath.backup"
    try {
        [IO.File]::WriteAllBytes($temporaryPath, $Bytes)
        [IO.File]::Replace($temporaryPath, $Path, $replacementBackup, $true)
    } finally {
        foreach ($cleanupPath in @($temporaryPath, $replacementBackup)) {
            Remove-Item -LiteralPath $cleanupPath -Force -ErrorAction SilentlyContinue
        }
    }
}

function New-NormalLoggingConfig([string]$Text) {
    $schema = [regex]::Match($Text, '(?m)^\s*schema_version\s*=\s*(?<value>\d+)\s*$')
    Assert-True $schema.Success "config schema is missing"
    $sectionPattern = '(?ms)^\s*\[logging\]\s*(?<body>.*?)(?=^\s*\[|\z)'
    $section = [regex]::Match($Text, $sectionPattern)
    Assert-True $section.Success "logging section is missing"
    $body = $section.Groups['body'].Value
    if ([int]$schema.Groups['value'].Value -ge 5) {
        Assert-True ([regex]::IsMatch($body, '(?m)^\s*level\s*=\s*"(?:off|normal|trace)"\s*$')) `
            "schema-5 logging.level is missing"
        $updatedBody = [regex]::Replace(
            $body,
            '(?m)^\s*level\s*=\s*"(?:off|normal|trace)"\s*$',
            'level = "normal"',
            1
        )
    } else {
        Assert-True ([regex]::IsMatch($body, '(?m)^\s*enabled\s*=\s*(true|false)\s*$')) `
            "legacy logging.enabled is missing"
        $updatedBody = [regex]::Replace(
            $body,
            '(?m)^\s*enabled\s*=\s*(true|false)\s*$',
            'enabled = true',
            1
        )
    }
    return $Text.Substring(0, $section.Index) +
        "[logging]`r`n$updatedBody" +
        $Text.Substring($section.Index + $section.Length)
}

function Get-ProcessSnapshot([int]$ProcessId) {
    $process = Get-CimInstance Win32_Process -Filter "ProcessId=$ProcessId"
    Assert-True ($null -ne $process) "service process is missing"
    return [pscustomobject]@{
        kernel_time_100ns = [uint64]$process.KernelModeTime
        user_time_100ns = [uint64]$process.UserModeTime
        write_operations = [uint64]$process.WriteOperationCount
        write_bytes = [uint64]$process.WriteTransferCount
        working_set_bytes = [uint64]$process.WorkingSetSize
    }
}

function Get-Delta($Before, $After) {
    Assert-True ([uint64]$After -ge [uint64]$Before) "monotonic counter regressed"
    return ([uint64]$After - [uint64]$Before)
}

function Read-SharedLines([string]$Path) {
    $stream = [IO.File]::Open(
        $Path,
        [IO.FileMode]::Open,
        [IO.FileAccess]::Read,
        ([IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete)
    )
    try {
        $reader = [IO.StreamReader]::new($stream, [Text.Encoding]::UTF8, $true, 4096, $true)
        try { $text = $reader.ReadToEnd() } finally { $reader.Dispose() }
    } finally {
        $stream.Dispose()
    }
    return @([regex]::Split($text, '\r?\n'))
}

function Read-NewLogEvents([uint64]$StartedUnixMs) {
    $events = New-Object System.Collections.ArrayList
    foreach ($file in @(Get-ChildItem -LiteralPath $DataDirectory -Filter "winsched.log*" -File |
        Where-Object Name -Match '^winsched\.log(?:\.\d+)?$')) {
        foreach ($line in @(Read-SharedLines $file.FullName)) {
            if ([string]::IsNullOrWhiteSpace($line) -or $line -notmatch '"timestamp_ms"') { continue }
            try {
                $event = $line | ConvertFrom-Json
                if ([uint64]$event.timestamp_ms -ge $StartedUnixMs) {
                    [void]$events.Add($event)
                }
            } catch {
            }
        }
    }
    return @($events)
}

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
Assert-True ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) `
    "normal logging acceptance requires elevation"

$configPath = Join-Path $DataDirectory "winsched.toml"
$originalBytes = [IO.File]::ReadAllBytes($configPath)
$originalHash = (Get-FileHash $configPath -Algorithm SHA256).Hash.ToLowerInvariant()
$originalText = [Text.UTF8Encoding]::new($false, $true).GetString($originalBytes)
$beforeStatus = Read-Status
Assert-True ([string]$beforeStatus.applied_logging.level -eq "off") "initial logging is not Off"
$servicePid = [int]$beforeStatus.service_pid
$baselineSequence = [uint64]$beforeStatus.config_reload_sequence
$normalText = New-NormalLoggingConfig $originalText
$result = $null
$cleanupErrors = New-Object System.Collections.ArrayList

try {
    Set-FileAtomically $configPath ([Text.UTF8Encoding]::new($false).GetBytes($normalText))
    Wait-Condition "Normal logging reload receipt" {
        $status = Read-Status
        [int]$status.service_pid -eq $servicePid -and
            [uint64]$status.config_reload_sequence -gt $baselineSequence -and
            [string]$status.config_reload_result -eq "reloaded" -and
            [string]$status.applied_logging.level -eq "normal"
    }
    $normalStatus = Read-Status
    $beforeProcess = Get-ProcessSnapshot $servicePid
    $beforeLogging = $normalStatus.telemetry.logging
    $startedUnixMs = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    $stopwatch = [Diagnostics.Stopwatch]::StartNew()
    Start-Sleep -Seconds $DurationSeconds
    $stopwatch.Stop()
    $afterStatus = Read-Status
    $afterProcess = Get-ProcessSnapshot $servicePid
    $events = @(Read-NewLogEvents $startedUnixMs)
    $summaries = @($events | Where-Object event -eq "decision_summary")
    $periodicSummaries = @($summaries | Where-Object flush_reason -eq "periodic")
    $rawDecisions = @($events | Where-Object event -eq "decision")
    $rawNoOps = @($rawDecisions | Where-Object {
        $actionJson = $_.action | ConvertTo-Json -Compress
        $actionJson -match '^\{"Keep"' -or $actionJson -eq '"Ignore"'
    })
    $duration = $stopwatch.Elapsed.TotalSeconds
    $cpuDelta = (Get-Delta $beforeProcess.kernel_time_100ns $afterProcess.kernel_time_100ns) +
        (Get-Delta $beforeProcess.user_time_100ns $afterProcess.user_time_100ns)
    $recordsDelta = Get-Delta $beforeLogging.records_written $afterStatus.telemetry.logging.records_written
    $bytesDelta = Get-Delta $beforeLogging.bytes_written $afterStatus.telemetry.logging.bytes_written

    Assert-True ([int]$afterStatus.service_pid -eq $servicePid) "service restarted"
    Assert-True ($null -eq $afterStatus.last_error) "service reports an error"
    Assert-True ($periodicSummaries.Count -ge 1) "Normal emitted no complete minute summary"
    Assert-True ($rawNoOps.Count -eq 0) "Normal emitted a raw no-op decision"
    Assert-True ($recordsDelta -gt 0 -and $recordsDelta -lt 200) "Normal record count is unbounded"
    Assert-True ([uint64]$afterStatus.telemetry.logging.write_errors -eq 0) "logger reports errors"

    $oneCorePercent = 100.0 * [double]$cpuDelta / 10000000.0 / $duration
    $result = [ordered]@{
        result = "PASS"
        duration_seconds = $duration
        service_pid_unchanged = $true
        cpu = [ordered]@{
            one_core_percent = $oneCorePercent
            machine_capacity_percent = $oneCorePercent / [Math]::Max(1, [Environment]::ProcessorCount)
        }
        process_io = [ordered]@{
            write_operations = Get-Delta $beforeProcess.write_operations $afterProcess.write_operations
            write_bytes = Get-Delta $beforeProcess.write_bytes $afterProcess.write_bytes
        }
        working_set_mib = [double]$afterProcess.working_set_bytes / 1MB
        logging = [ordered]@{
            records = $recordsDelta
            bytes = $bytesDelta
            records_per_second = [double]$recordsDelta / $duration
            bytes_per_second = [double]$bytesDelta / $duration
            decision_summaries = $summaries.Count
            periodic_summaries = $periodicSummaries.Count
            raw_mutation_decisions = $rawDecisions.Count
            raw_noop_decisions = $rawNoOps.Count
            write_errors = [uint64]$afterStatus.telemetry.logging.write_errors
        }
        evaluation = $afterStatus.telemetry.evaluation
        original_config_sha256 = $originalHash
    }
} catch {
    $result = [ordered]@{
        result = "FAIL"
        error = $_.Exception.ToString()
        script_stack = $_.ScriptStackTrace
        original_config_sha256 = $originalHash
    }
} finally {
    try {
        $restoreStatus = Read-Status
        $restoreSequence = [uint64]$restoreStatus.config_reload_sequence
        Set-FileAtomically $configPath $originalBytes
        Wait-Condition "Off logging restore receipt" {
            $status = Read-Status
            [int]$status.service_pid -eq $servicePid -and
                [uint64]$status.config_reload_sequence -gt $restoreSequence -and
                [string]$status.applied_logging.level -eq "off"
        }
        Assert-True (
            (Get-FileHash $configPath -Algorithm SHA256).Hash.ToLowerInvariant() -eq $originalHash
        ) "original config bytes were not restored"
    } catch {
        [void]$cleanupErrors.Add($_.Exception.ToString())
    }
}

if ($null -eq $result) {
    $result = [ordered]@{ result = "FAIL"; error = "measurement did not complete" }
}
$result["config_restored"] = $cleanupErrors.Count -eq 0
$result["cleanup_errors"] = @($cleanupErrors)
if ($cleanupErrors.Count -gt 0) { $result["result"] = "FAIL" }
[IO.File]::WriteAllText(
    $ResultPath,
    ([pscustomobject]$result | ConvertTo-Json -Depth 9) + "`n",
    [Text.UTF8Encoding]::new($false)
)
[pscustomobject]$result | ConvertTo-Json -Depth 9
if ($result.result -ne "PASS") { exit 1 }
