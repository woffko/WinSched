[CmdletBinding()]
param(
    [string]$InstallDirectory = (Join-Path ([Environment]::GetFolderPath("ProgramFiles")) "WinSched"),
    [string]$DataDirectory = (Join-Path ([Environment]::GetFolderPath("CommonApplicationData")) "WinSched"),
    [string]$ResultPath = (Join-Path $env:PUBLIC "WinSchedHostABBA\host-abba-result.json"),
    [ValidateRange(1, 120)]
    [int]$MeasurementSeconds = 120,
    [ValidateRange(0, 600)]
    [int]$SettleSeconds = 60,
    [ValidateRange(1, 100)]
    [int]$AttemptsPerPhase = 10,
    [ValidateRange(10, 250)]
    [int]$TaskbarTimeoutMs = 50,
    [ValidateSet("off", "normal")]
    [string]$LoggingLevelDuringTest = "off",
    [ValidateSet("firefox_taskbar_restore")]
    [string]$Scenario = "firefox_taskbar_restore",
    [string]$PilotResultPath = (Join-Path $env:PUBLIC `
        "WinSchedV051Host\passive-pilot-result.json"),
    [switch]$KeepWindowOnError,
    [switch]$ObserverSelfTest,
    [switch]$PassivePilot,
    [switch]$SelfTest
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Throw-Abba([string]$Code) {
    throw $Code
}

function Assert-True([bool]$Condition, [string]$Code) {
    if (-not $Condition) {
        Throw-Abba $Code
    }
}

function Get-SafeUnexpectedError([System.Management.Automation.ErrorRecord]$Record) {
    $type = $Record.Exception.GetType().Name
    $message = [string]$Record.Exception.Message
    $message = [regex]::Replace(
        $message,
        '(?i)(?:\\\\\?\\)?[A-Z]:\\[^\r\n"]+',
        '<path>'
    )
    $message = [regex]::Replace($message, '(?i)\\\\wsl[^\s"]+', '<wsl-path>')
    $message = [regex]::Replace($message, '\b\d{4,10}\b', '<number>')
    if ($message.Length -gt 512) {
        $message = $message.Substring(0, 512)
    }
    return "${type}: $message"
}

function Wait-Condition(
    [string]$Code,
    [scriptblock]$Condition,
    [int]$TimeoutSeconds = 30
) {
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        if (& $Condition) {
            return
        }
        Start-Sleep -Milliseconds 200
    } while ([DateTime]::UtcNow -lt $deadline)
    Throw-Abba $Code
}

function Get-Sha256([string]$Path) {
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Write-Utf8NoBom([string]$Path, [string]$Text) {
    $encoding = New-Object System.Text.UTF8Encoding($false)
    [IO.File]::WriteAllText($Path, $Text, $encoding)
}

function Set-FileAtomically([string]$Path, [byte[]]$Bytes, [string]$RunId) {
    $directory = Split-Path -Parent $Path
    $temporaryPath = Join-Path $directory (
        ".{0}.host-abba-{1}.tmp" -f (Split-Path -Leaf $Path), $RunId
    )
    $replacementBackup = "$temporaryPath.backup"
    try {
        [IO.File]::WriteAllBytes($temporaryPath, $Bytes)
        [IO.File]::Replace($temporaryPath, $Path, $replacementBackup, $true)
    } finally {
        foreach ($cleanupPath in @($temporaryPath, $replacementBackup)) {
            if (Test-Path -LiteralPath $cleanupPath) {
                Remove-Item -LiteralPath $cleanupPath -Force -ErrorAction SilentlyContinue
            }
        }
    }
}

function Set-Utf8FileAtomically([string]$Path, [string]$Text, [string]$RunId) {
    $encoding = New-Object System.Text.UTF8Encoding($false)
    Set-FileAtomically $Path $encoding.GetBytes($Text) $RunId
}

function Get-Percentile([object[]]$Values, [ValidateRange(1, 100)][int]$Percentile) {
    $numbers = @($Values | ForEach-Object { [double]$_ } | Sort-Object)
    if ($numbers.Count -eq 0) {
        return $null
    }
    $rank = [int][Math]::Ceiling($numbers.Count * ($Percentile / 100.0))
    $index = [Math]::Max(0, [Math]::Min($numbers.Count - 1, $rank - 1))
    return [double]$numbers[$index]
}

function Get-MetricSummary([object[]]$Values) {
    $numbers = @($Values | ForEach-Object { [double]$_ })
    if ($numbers.Count -eq 0) {
        return [ordered]@{
            samples = 0
            p50 = $null
            p95 = $null
            maximum = $null
        }
    }
    return [ordered]@{
        samples = $numbers.Count
        p50 = Get-Percentile $numbers 50
        p95 = Get-Percentile $numbers 95
        maximum = [double](($numbers | Measure-Object -Maximum).Maximum)
    }
}

function Get-PropertyValue($Object, [string]$Name, $Default = $null) {
    if ($null -eq $Object) {
        return $Default
    }
    $property = $Object.PSObject.Properties[$Name]
    if ($null -eq $property) {
        return $Default
    }
    return $property.Value
}

function Get-LoggingMode($Status) {
    $logging = Get-PropertyValue $Status "applied_logging"
    if ($null -eq $logging) {
        return $null
    }
    $level = Get-PropertyValue $logging "level"
    if ($null -ne $level) {
        return ([string]$level).ToLowerInvariant()
    }
    $enabled = Get-PropertyValue $logging "enabled"
    if ($null -ne $enabled) {
        if ([bool]$enabled) {
            return "normal"
        }
        return "off"
    }
    return $null
}

function Read-Status {
    if (-not (Test-Path -LiteralPath $script:statusPath -PathType Leaf)) {
        return $null
    }
    try {
        return Get-Content -LiteralPath $script:statusPath -Raw | ConvertFrom-Json
    } catch {
        return $null
    }
}

function Wait-ConfigReceipt(
    [uint64]$AfterSequence,
    [string]$ExpectedLoggingMode,
    [int]$ExpectedServicePid,
    [string]$Code
) {
    Wait-Condition $Code {
        $status = Read-Status
        $null -ne $status -and
            [int]$status.service_pid -eq $ExpectedServicePid -and
            [uint64]$status.config_reload_sequence -gt $AfterSequence -and
            [string]$status.config_reload_result -eq "reloaded" -and
            $null -eq $status.config_reload_error -and
            (Get-LoggingMode $status) -eq $ExpectedLoggingMode
    } 30
    return (Read-Status)
}

function Wait-SchedulingState(
    [bool]$Enabled,
    [int]$ExpectedServicePid,
    [string]$Code
) {
    Wait-Condition $Code {
        $status = Read-Status
        if ($null -eq $status -or [int]$status.service_pid -ne $ExpectedServicePid) {
            return $false
        }
        if ([bool]$status.scheduling_enabled -ne $Enabled) {
            return $false
        }
        if ($Enabled) {
            return [string]$status.phase -eq "running" -and $null -eq $status.last_error
        }
        $background = Get-PropertyValue $status "background_efficiency"
        $backgroundManaged = Get-PropertyValue $background "managed_processes" 0
        return [string]$status.phase -eq "disabled" -and
            [int]$status.managed_processes -eq 0 -and
            [int]$backgroundManaged -eq 0 -and
            $null -eq $status.last_error
    } 45
    return (Read-Status)
}

function Set-SchedulingState([bool]$Enabled, [int]$ExpectedServicePid) {
    $status = Read-Status
    Assert-True ($null -ne $status) "ABBA_STATUS_UNAVAILABLE"
    Assert-True ([int]$status.service_pid -eq $ExpectedServicePid) "ABBA_SERVICE_RESTARTED"
    if ([bool]$status.scheduling_enabled -ne $Enabled) {
        $command = if ($Enabled) { "enable" } else { "disable" }
        & $script:serviceBinary $command | Out-Null
        Assert-True ($LASTEXITCODE -eq 0) "ABBA_SCHEDULING_COMMAND_FAILED"
    }
    return Wait-SchedulingState $Enabled $ExpectedServicePid "ABBA_SCHEDULING_STATE_TIMEOUT"
}

function New-TestConfigurationText(
    [string]$Original,
    [string]$ObserverImage,
    [string]$MarkerImage,
    [ValidateSet("off", "normal")][string]$LoggingMode
) {
    Assert-True (-not [regex]::IsMatch(
        $Original,
        '(?im)^\s*image\s*=\s*"winsched-abba-(?:observer|marker)-[0-9a-f]+\.exe"\s*$'
    )) "ABBA_ORPHAN_TEMP_RULES_DETECTED"
    foreach ($image in @($ObserverImage, $MarkerImage)) {
        Assert-True (-not [regex]::IsMatch(
            $Original,
            ('(?im)^\s*image\s*=\s*"{0}"\s*$' -f [regex]::Escape($image))
        )) "ABBA_TEMP_RULE_ALREADY_PRESENT"
    }

    $schemaMatch = [regex]::Match($Original, '(?m)^\s*schema_version\s*=\s*(\d+)\s*$')
    Assert-True $schemaMatch.Success "ABBA_CONFIG_SCHEMA_MISSING"
    $schemaVersion = [int]$schemaMatch.Groups[1].Value
    $loggingMatch = [regex]::Match(
        $Original,
        '(?ms)^\s*\[logging\]\s*\r?\n.*?(?=^\s*\[|\z)'
    )
    Assert-True $loggingMatch.Success "ABBA_LOGGING_BLOCK_MISSING"
    $loggingBlock = $loggingMatch.Value
    $levelPattern = '(?m)^(?<prefix>\s*level\s*=\s*)"(?:off|normal|trace)"(?<suffix>\s*(?:#.*)?)$'
    $enabledPattern = '(?m)^(?<prefix>\s*enabled\s*=\s*)(?:true|false)(?<suffix>\s*(?:#.*)?)$'
    $levelMatches = [regex]::Matches($loggingBlock, $levelPattern)
    $enabledMatches = [regex]::Matches($loggingBlock, $enabledPattern)
    Assert-True (-not ($levelMatches.Count -gt 0 -and $enabledMatches.Count -gt 0)) `
        "ABBA_CONFLICTING_LOGGING_FIELDS"
    if ($levelMatches.Count -eq 1) {
        $replacement = '${prefix}"' + $LoggingMode + '"${suffix}'
        $newLoggingBlock = [regex]::Replace($loggingBlock, $levelPattern, $replacement, 1)
    } elseif ($enabledMatches.Count -eq 1) {
        $enabledText = if ($LoggingMode -eq "off") { "false" } else { "true" }
        $replacement = '${prefix}' + $enabledText + '${suffix}'
        $newLoggingBlock = [regex]::Replace($loggingBlock, $enabledPattern, $replacement, 1)
    } elseif ($levelMatches.Count -eq 0 -and $enabledMatches.Count -eq 0) {
        $line = if ($schemaVersion -ge 5) {
            'level = "' + $LoggingMode + '"'
        } else {
            if ($LoggingMode -eq "off") { "enabled = false" } else { "enabled = true" }
        }
        $newLoggingBlock = [regex]::Replace(
            $loggingBlock,
            '(?m)^(\s*\[logging\]\s*)$',
            ('$1' + "`r`n" + $line),
            1
        )
    } else {
        Throw-Abba "ABBA_DUPLICATE_LOGGING_FIELD"
    }

    $result = $Original.Substring(0, $loggingMatch.Index) +
        $newLoggingBlock +
        $Original.Substring($loggingMatch.Index + $loggingMatch.Length)
    $emptyInlineRulesPattern = '(?m)^\s*rules\s*=\s*\[\s*\]\s*(?:#.*)?\r?\n?'
    $emptyInlineRules = [regex]::Matches($result, $emptyInlineRulesPattern)
    $anyInlineRules = [regex]::Matches($result, '(?m)^\s*rules\s*=')
    $arrayRules = [regex]::Matches($result, '(?m)^\s*\[\[rules\]\]\s*$')
    Assert-True (-not ($anyInlineRules.Count -gt 0 -and $arrayRules.Count -gt 0)) `
        "ABBA_CONFLICTING_RULE_REPRESENTATIONS"
    if ($anyInlineRules.Count -gt 0) {
        Assert-True ($anyInlineRules.Count -eq 1 -and $emptyInlineRules.Count -eq 1) `
            "ABBA_NONEMPTY_INLINE_RULES_UNSUPPORTED"
        $result = [regex]::Replace($result, $emptyInlineRulesPattern, '', 1)
    }
    $newline = if ($Original.Contains("`r`n")) { "`r`n" } else { "`n" }
    if (-not $result.EndsWith("`n")) {
        $result += $newline
    }
    $result += $newline +
        "# Temporary WinSched physical-host ABBA observer exclusions." + $newline +
        "[[rules]]" + $newline +
        ('image = "{0}"' -f $ObserverImage) + $newline +
        'mode = "off"' + $newline +
        'profile = "balanced"' + $newline + $newline +
        "[[rules]]" + $newline +
        ('image = "{0}"' -f $MarkerImage) + $newline +
        'mode = "off"' + $newline +
        'profile = "balanced"' + $newline
    return $result
}

function Get-ProcessSnapshot([int]$ProcessId) {
    $process = Get-Process -Id $ProcessId -ErrorAction Stop
    $process.Refresh()
    $cim = $null
    try {
        $cim = Get-CimInstance Win32_Process -Filter ("ProcessId={0}" -f $ProcessId) `
            -ErrorAction Stop
    } catch {
        $cim = $null
    }
    return [pscustomobject]@{
        start_time_ticks = $process.StartTime.ToUniversalTime().Ticks
        cpu_time_100ns = [uint64]$process.TotalProcessorTime.Ticks
        working_set_bytes = [uint64]$process.WorkingSet64
        private_bytes = [uint64]$process.PrivateMemorySize64
        handles = [uint64]$process.HandleCount
        threads = [uint64]$process.Threads.Count
        read_operations = if ($null -ne $cim) { [uint64]$cim.ReadOperationCount } else { $null }
        write_operations = if ($null -ne $cim) { [uint64]$cim.WriteOperationCount } else { $null }
        read_bytes = if ($null -ne $cim) { [uint64]$cim.ReadTransferCount } else { $null }
        write_bytes = if ($null -ne $cim) { [uint64]$cim.WriteTransferCount } else { $null }
    }
}

function Get-ProcessSample([int]$ProcessId, [long]$ExpectedStartTicks) {
    $process = Get-Process -Id $ProcessId -ErrorAction Stop
    $process.Refresh()
    Assert-True ($process.StartTime.ToUniversalTime().Ticks -eq $ExpectedStartTicks) `
        "ABBA_PROCESS_ID_REUSED"
    return [pscustomobject]@{
        working_set_bytes = [uint64]$process.WorkingSet64
        private_bytes = [uint64]$process.PrivateMemorySize64
        handles = [uint64]$process.HandleCount
        threads = [uint64]$process.Threads.Count
    }
}

function Get-UnsignedDelta($Before, $After) {
    if ($null -eq $Before -or $null -eq $After) {
        return $null
    }
    $left = [uint64]$Before
    $right = [uint64]$After
    if ($right -lt $left) {
        return $null
    }
    return [uint64]($right - $left)
}

function Get-ProcessMetrics(
    $Before,
    $After,
    [object[]]$Samples,
    [double]$DurationSeconds,
    [int]$LogicalProcessors
) {
    $cpuDelta = Get-UnsignedDelta $Before.cpu_time_100ns $After.cpu_time_100ns
    $corePercent = $null
    $hostPercent = $null
    if ($null -ne $cpuDelta -and $DurationSeconds -gt 0) {
        $corePercent = 100.0 * [double]$cpuDelta / 10000000.0 / $DurationSeconds
        $hostPercent = $corePercent / [Math]::Max(1, $LogicalProcessors)
    }
    $workingSets = @($Samples | ForEach-Object { [double]$_.working_set_bytes / 1MB })
    $privateBytes = @($Samples | ForEach-Object { [double]$_.private_bytes / 1MB })
    return [ordered]@{
        available = $true
        sample_count = $Samples.Count
        cpu_core_equivalent_percent = $corePercent
        cpu_host_capacity_percent = $hostPercent
        working_set_mib = Get-MetricSummary $workingSets
        private_bytes_mib = Get-MetricSummary $privateBytes
        maximum_threads = if ($Samples.Count -gt 0) {
            [uint64](($Samples.threads | Measure-Object -Maximum).Maximum)
        } else { $null }
        maximum_handles = if ($Samples.Count -gt 0) {
            [uint64](($Samples.handles | Measure-Object -Maximum).Maximum)
        } else { $null }
        io = [ordered]@{
            read_operations = Get-UnsignedDelta $Before.read_operations $After.read_operations
            write_operations = Get-UnsignedDelta $Before.write_operations $After.write_operations
            read_bytes = Get-UnsignedDelta $Before.read_bytes $After.read_bytes
            write_bytes = Get-UnsignedDelta $Before.write_bytes $After.write_bytes
        }
    }
}

function Get-StatusTelemetryDelta($BeforeStatus, $AfterStatus) {
    $before = Get-PropertyValue $BeforeStatus "telemetry"
    $after = Get-PropertyValue $AfterStatus "telemetry"
    if ($null -eq $before -or $null -eq $after) {
        return [ordered]@{ available = $false }
    }
    $beforeEvaluation = Get-PropertyValue $before "evaluation"
    $afterEvaluation = Get-PropertyValue $after "evaluation"
    $beforeMutations = Get-PropertyValue $before "mutations"
    $afterMutations = Get-PropertyValue $after "mutations"
    $beforeLogging = Get-PropertyValue $before "logging"
    $afterLogging = Get-PropertyValue $after "logging"
    return [ordered]@{
        available = $true
        evaluations_completed = Get-UnsignedDelta `
            (Get-PropertyValue $beforeEvaluation "completed_total" 0) `
            (Get-PropertyValue $afterEvaluation "completed_total" 0)
        evaluation_rolling_mean_us = Get-PropertyValue $afterEvaluation "rolling_mean_us" 0
        evaluation_rolling_p95_us = Get-PropertyValue $afterEvaluation "rolling_p95_us" 0
        evaluation_rolling_max_us = Get-PropertyValue $afterEvaluation "rolling_max_us" 0
        last_scanned_processes = Get-PropertyValue $afterEvaluation "last_scanned_processes" 0
        last_eligible_processes = Get-PropertyValue $afterEvaluation "last_eligible_processes" 0
        last_decisions = Get-PropertyValue $afterEvaluation "last_decisions" 0
        mutations = [ordered]@{
            placement_attempted = Get-UnsignedDelta `
                (Get-PropertyValue $beforeMutations "placement_attempted" 0) `
                (Get-PropertyValue $afterMutations "placement_attempted" 0)
            placement_succeeded = Get-UnsignedDelta `
                (Get-PropertyValue $beforeMutations "placement_succeeded" 0) `
                (Get-PropertyValue $afterMutations "placement_succeeded" 0)
            placement_failed = Get-UnsignedDelta `
                (Get-PropertyValue $beforeMutations "placement_failed" 0) `
                (Get-PropertyValue $afterMutations "placement_failed" 0)
            background_attempted = Get-UnsignedDelta `
                (Get-PropertyValue $beforeMutations "background_attempted" 0) `
                (Get-PropertyValue $afterMutations "background_attempted" 0)
            background_succeeded = Get-UnsignedDelta `
                (Get-PropertyValue $beforeMutations "background_succeeded" 0) `
                (Get-PropertyValue $afterMutations "background_succeeded" 0)
            background_failed = Get-UnsignedDelta `
                (Get-PropertyValue $beforeMutations "background_failed" 0) `
                (Get-PropertyValue $afterMutations "background_failed" 0)
        }
        logging = [ordered]@{
            records_written = Get-UnsignedDelta `
                (Get-PropertyValue $beforeLogging "records_written" 0) `
                (Get-PropertyValue $afterLogging "records_written" 0)
            bytes_written = Get-UnsignedDelta `
                (Get-PropertyValue $beforeLogging "bytes_written" 0) `
                (Get-PropertyValue $afterLogging "bytes_written" 0)
            write_errors = Get-UnsignedDelta `
                (Get-PropertyValue $beforeLogging "write_errors" 0) `
                (Get-PropertyValue $afterLogging "write_errors" 0)
            status_writes = Get-UnsignedDelta `
                (Get-PropertyValue $beforeLogging "status_writes" 0) `
                (Get-PropertyValue $afterLogging "status_writes" 0)
        }
    }
}

function Convert-DiagnosticReport($Report) {
    $findingCodes = @()
    foreach ($finding in @($Report.findings)) {
        $findingCodes += [string]$finding.code
    }
    return [ordered]@{
        schema_version = [int]$Report.schema_version
        duration_ms = [uint64]$Report.duration_ms
        sample_count = [int]$Report.sample_count
        system = [ordered]@{
            average_cpu_percent = [double]$Report.system.average_cpu_utilization_bps / 100.0
            maximum_domain_percent = [double]$Report.system.maximum_domain_utilization_bps / 100.0
            maximum_processor_queue_length = [uint64]$Report.system.maximum_processor_queue_length
            maximum_dpc_percent = [double]$Report.system.maximum_dpc_time_bps / 100.0
            maximum_interrupt_percent = [double]$Report.system.maximum_interrupt_time_bps / 100.0
            maximum_pages_input_per_second = [uint64]$Report.system.maximum_pages_input_per_second
            minimum_available_memory_mib = [double]$Report.system.minimum_available_memory_bytes / 1MB
            scheduler_latency = [ordered]@{
                samples = [uint64]$Report.system.scheduler_latency.window_samples
                p50_us = [uint64]$Report.system.scheduler_latency.p50_lateness_us
                p95_us = [uint64]$Report.system.scheduler_latency.p95_lateness_us
                p99_us = [uint64]$Report.system.scheduler_latency.p99_lateness_us
                maximum_us = [uint64]$Report.system.scheduler_latency.maximum_lateness_us
            }
        }
        taskbar = [ordered]@{
            available = [bool]$Report.shell.taskbar.available
            samples = [uint64]$Report.shell.taskbar.samples
            successful_samples = [uint64]$Report.shell.taskbar.successful_samples
            timeout_samples = [uint64]$Report.shell.taskbar.timeout_samples
            p50_us = [uint64]$Report.shell.taskbar.p50_response_us
            p95_us = [uint64]$Report.shell.taskbar.p95_response_us
            maximum_us = [uint64]$Report.shell.taskbar.maximum_response_us
        }
        shell = [ordered]@{
            explorer_processes = [uint64]$Report.shell.explorer_processes
            explorer_threads = [uint64]$Report.shell.explorer_threads
            explorer_windows = [uint64]$Report.shell.explorer_windows
            separate_process = Get-PropertyValue $Report.shell "launch_folders_in_separate_process"
        }
        virtualization = [ordered]@{
            wsl_processes = [uint64]$Report.virtualization.wsl_processes
            wsl_threads = [uint64]$Report.virtualization.wsl_threads
            vmware_vm_processes = [uint64]$Report.virtualization.vmware_vm_processes
            vmware_vm_threads = [uint64]$Report.virtualization.vmware_vm_threads
        }
        finding_codes = $findingCodes
    }
}

function Convert-ObserveJsonLines([string]$Path) {
    $domains = @{}
    $sampleCount = 0
    foreach ($line in @(Get-Content -LiteralPath $Path -ErrorAction Stop)) {
        if ([string]::IsNullOrWhiteSpace($line)) {
            continue
        }
        $sample = $line | ConvertFrom-Json
        $sampleCount++
        foreach ($load in @($sample.domain_loads)) {
            $key = "{0}:{1}" -f $load.domain.group, $load.domain.last_level_cache_index
            if (-not $domains.ContainsKey($key)) {
                $domains[$key] = [ordered]@{
                    utility = New-Object System.Collections.ArrayList
                    dpc = New-Object System.Collections.ArrayList
                    interrupt = New-Object System.Collections.ArrayList
                }
            }
            [void]$domains[$key].utility.Add([double]$load.utilization_bps / 100.0)
            [void]$domains[$key].dpc.Add([double]$load.dpc_time_bps / 100.0)
            [void]$domains[$key].interrupt.Add([double]$load.interrupt_time_bps / 100.0)
        }
    }
    $domainResults = @()
    foreach ($key in @($domains.Keys | Sort-Object)) {
        $parts = $key.Split(':')
        $domainResults += [ordered]@{
            group = [int]$parts[0]
            llc = [int]$parts[1]
            utilization_percent = Get-MetricSummary @($domains[$key].utility)
            dpc_percent = Get-MetricSummary @($domains[$key].dpc)
            interrupt_percent = Get-MetricSummary @($domains[$key].interrupt)
        }
    }
    return [ordered]@{
        samples = $sampleCount
        domains = $domainResults
    }
}

function Assert-Unassigned([int]$ProcessId) {
    $inspectionText = & $script:observerBinary inspect $ProcessId --json
    Assert-True ($LASTEXITCODE -eq 0) "ABBA_OBSERVER_INSPECT_FAILED"
    $inspection = $inspectionText | ConvertFrom-Json
    Assert-True (@($inspection.default_cpu_set_ids).Count -eq 0) `
        "ABBA_OBSERVER_WAS_ASSIGNED"
}

function Stop-OwnedProcess($Process) {
    if ($null -eq $Process) {
        return
    }
    try {
        $Process.Refresh()
        if (-not $Process.HasExited) {
            Stop-Process -Id $Process.Id -Force -ErrorAction SilentlyContinue
            [void]$Process.WaitForExit(5000)
        }
    } catch {
    }
}

$markerSource = @'
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Globalization;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

internal static class WinSchedHostAbbaMarker
{
    private const int WhMouseLl = 14;
    private const uint WmLButtonDown = 0x0201;
    private const uint PmRemove = 0x0001;
    private const uint EventSystemForeground = 0x0003;
    private const uint WineventOutOfContext = 0x0000;
    private const uint GaRoot = 2;
    private const uint WmNull = 0x0000;
    private const uint SmtoBlock = 0x0001;
    private const uint SmtoAbortIfHung = 0x0002;
    private const uint LlmhfInjectedMask = 0x00000003;
    private const int SmRemoteSession = 0x1000;
    private const int CandidateTimeoutMilliseconds = 30000;
    private const int MinimizeConfirmationMilliseconds = 3000;

    private sealed class CaptureSample
    {
        public double ClickToForegroundMs;
        public double ForegroundToResponsiveMs;
        public double ClickToResponsiveMs;
    }

    private sealed class ForegroundCandidate
    {
        public IntPtr Window;
        public uint EventTime;
        public int Generation;
    }

    private sealed class CaptureStateSnapshot
    {
        public int PhysicalLeftClicksObserved;
        public int TaskbarClicksObserved;
        public int CandidateTimeouts;
        public int CandidateReplacements;
        public int ConfirmedMinimizeClicks;
        public int PossibleMinimizeClicks;
        public int RejectedNonMinimizeClicks;
        public int IgnoredInjectedClicks;
        public int IgnoredUnarmedTaskbarClicks;
        public int PrimingActivations;
        public int RestoreCandidatesObserved;
        public int UnconfirmedRestoreResults;
    }

    private enum QueryUserNotificationState
    {
        NotPresent = 1,
        Busy = 2,
        RunningD3dFullScreen = 3,
        PresentationMode = 4,
        AcceptsNotifications = 5,
        QuietTime = 6,
        App = 7
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct Point
    {
        public int X;
        public int Y;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct Message
    {
        public IntPtr Window;
        public uint Id;
        public UIntPtr WParam;
        public IntPtr LParam;
        public uint Time;
        public Point Position;
        public uint Private;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct LowLevelMouseHookData
    {
        public Point Position;
        public uint MouseData;
        public uint Flags;
        public uint Time;
        public UIntPtr ExtraInfo;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct Rectangle
    {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    private delegate IntPtr LowLevelMouseProcedure(int code, IntPtr wParam, IntPtr lParam);

    private delegate void WinEventProcedure(
        IntPtr hook,
        uint eventType,
        IntPtr window,
        int objectId,
        int childId,
        uint eventThread,
        uint eventTime);

    private static readonly LowLevelMouseProcedure MouseProcedure = MouseHookCallback;
    private static readonly WinEventProcedure ForegroundProcedure = ForegroundEventCallback;
    private static readonly object Sync = new object();
    private static readonly AutoResetEvent ProbeSignal = new AutoResetEvent(false);
    private static readonly Queue<ForegroundCandidate> ForegroundCandidates =
        new Queue<ForegroundCandidate>();
    private static readonly Queue<CaptureSample> CompletedSamples =
        new Queue<CaptureSample>();

    private static volatile bool captureActive;
    private static volatile bool probeStop;
    private static bool candidateActive;
    private static int candidateGeneration;
    private static uint candidateClickTime;
    private static ulong candidateDeadline;
    private static IntPtr firefoxWindow;
    private static bool primed;
    private static bool minimizedConfirmed;
    private static bool minimizePending;
    private static int minimizeGeneration;
    private static IntPtr minimizeWindow;
    private static ulong minimizeDeadline;
    private static bool primingCompletedNotice;
    private static int candidateTimeouts;
    private static int candidateReplacements;
    private static int confirmedMinimizeClicks;
    private static int possibleMinimizeClicks;
    private static int rejectedNonMinimizeClicks;
    private static int ignoredInjectedClicks;
    private static int ignoredUnarmedTaskbarClicks;
    private static int primingActivations;
    private static int restoreCandidatesObserved;
    private static int unconfirmedRestoreResults;
    private static int physicalLeftClicksObserved;
    private static int taskbarClicksObserved;
    private static bool perMonitorDpiAware;

    [DllImport("user32.dll", SetLastError = true)]
    private static extern IntPtr SetWindowsHookEx(
        int hookId,
        LowLevelMouseProcedure procedure,
        IntPtr module,
        uint threadId);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool UnhookWindowsHookEx(IntPtr hook);

    [DllImport("user32.dll")]
    private static extern IntPtr CallNextHookEx(
        IntPtr hook,
        int code,
        IntPtr wParam,
        IntPtr lParam);

    [DllImport("user32.dll")]
    private static extern IntPtr SetWinEventHook(
        uint eventMinimum,
        uint eventMaximum,
        IntPtr eventHookModule,
        WinEventProcedure eventProcedure,
        uint processId,
        uint threadId,
        uint flags);

    [DllImport("user32.dll")]
    private static extern bool UnhookWinEvent(IntPtr hook);

    [DllImport("user32.dll")]
    private static extern bool PeekMessage(
        out Message message,
        IntPtr window,
        uint minimum,
        uint maximum,
        uint remove);

    [DllImport("user32.dll")]
    private static extern bool TranslateMessage(ref Message message);

    [DllImport("user32.dll")]
    private static extern IntPtr DispatchMessage(ref Message message);

    [DllImport("user32.dll")]
    private static extern int GetSystemMetrics(int index);

    [DllImport("user32.dll")]
    private static extern IntPtr GetForegroundWindow();

    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(IntPtr window, out uint processId);

    [DllImport("user32.dll")]
    private static extern bool IsIconic(IntPtr window);

    [DllImport("user32.dll")]
    private static extern IntPtr WindowFromPoint(Point point);

    [DllImport("user32.dll")]
    private static extern IntPtr GetAncestor(IntPtr window, uint flags);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int GetClassName(IntPtr window, StringBuilder className, int maximum);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern IntPtr FindWindow(string className, string windowName);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern IntPtr FindWindowEx(
        IntPtr parent,
        IntPtr childAfter,
        string className,
        string windowName);

    [DllImport("user32.dll")]
    private static extern bool GetWindowRect(IntPtr window, out Rectangle rectangle);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool SetProcessDpiAwarenessContext(IntPtr awarenessContext);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern IntPtr SendMessageTimeout(
        IntPtr window,
        uint message,
        UIntPtr wParam,
        IntPtr lParam,
        uint flags,
        uint timeout,
        out UIntPtr result);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)]
    private static extern IntPtr GetModuleHandle(string moduleName);

    [DllImport("kernel32.dll")]
    private static extern uint GetTickCount();

    [DllImport("kernel32.dll")]
    private static extern ulong GetTickCount64();

    [DllImport("shell32.dll")]
    private static extern int SHQueryUserNotificationState(out QueryUserNotificationState state);

    private static bool IsFullscreenOrPresentation(out bool queryAvailable)
    {
        QueryUserNotificationState state;
        int result = SHQueryUserNotificationState(out state);
        queryAvailable = result >= 0;
        if (!queryAvailable)
        {
            return true;
        }
        return state == QueryUserNotificationState.RunningD3dFullScreen ||
            state == QueryUserNotificationState.PresentationMode;
    }

    private static void WriteText(string path, string text)
    {
        File.WriteAllText(path, text, new UTF8Encoding(false));
    }

    private static string Boolean(bool value)
    {
        return value ? "true" : "false";
    }

    private static double ElapsedMilliseconds(uint start, uint end)
    {
        return unchecked(end - start);
    }

    private static bool IsFirefoxWindow(IntPtr window)
    {
        if (window == IntPtr.Zero)
        {
            return false;
        }
        uint processId;
        GetWindowThreadProcessId(window, out processId);
        if (processId == 0)
        {
            return false;
        }
        try
        {
            using (Process process = Process.GetProcessById(unchecked((int)processId)))
            {
                return String.Equals(process.ProcessName, "firefox", StringComparison.OrdinalIgnoreCase);
            }
        }
        catch
        {
            return false;
        }
    }

    private static bool IsTaskbarPoint(Point point)
    {
        IntPtr window = WindowFromPoint(point);
        if (window == IntPtr.Zero)
        {
            return false;
        }
        IntPtr root = GetAncestor(window, GaRoot);
        if (root == IntPtr.Zero)
        {
            root = window;
        }
        StringBuilder className = new StringBuilder(128);
        string value = GetClassName(root, className, className.Capacity) == 0
            ? String.Empty
            : className.ToString();
        if (String.Equals(value, "Shell_TrayWnd", StringComparison.Ordinal) ||
            String.Equals(value, "Shell_SecondaryTrayWnd", StringComparison.Ordinal))
        {
            return true;
        }
        IntPtr primary = FindWindow("Shell_TrayWnd", null);
        if (PointInsideWindow(primary, point))
        {
            return true;
        }
        IntPtr secondary = IntPtr.Zero;
        while (true)
        {
            secondary = FindWindowEx(
                IntPtr.Zero,
                secondary,
                "Shell_SecondaryTrayWnd",
                null);
            if (secondary == IntPtr.Zero)
            {
                break;
            }
            if (PointInsideWindow(secondary, point))
            {
                return true;
            }
        }
        return false;
    }

    private static bool PointInsideWindow(IntPtr window, Point point)
    {
        Rectangle rectangle;
        return window != IntPtr.Zero &&
            GetWindowRect(window, out rectangle) &&
            point.X >= rectangle.Left &&
            point.X < rectangle.Right &&
            point.Y >= rectangle.Top &&
            point.Y < rectangle.Bottom;
    }

    private static void CancelCandidateLocked()
    {
        candidateActive = false;
        candidateGeneration++;
        ForegroundCandidates.Clear();
    }

    private static IntPtr MouseHookCallback(int code, IntPtr wParam, IntPtr lParam)
    {
        if (captureActive && code >= 0 &&
            unchecked((uint)wParam.ToInt64()) == WmLButtonDown)
        {
            LowLevelMouseHookData data =
                (LowLevelMouseHookData)Marshal.PtrToStructure(lParam, typeof(LowLevelMouseHookData));
            if ((data.Flags & LlmhfInjectedMask) != 0)
            {
                lock (Sync)
                {
                    ignoredInjectedClicks++;
                }
            }
            else
            {
                bool taskbarPoint = IsTaskbarPoint(data.Position);
                IntPtr foreground = GetForegroundWindow();
                ulong now = GetTickCount64();
                lock (Sync)
                {
                    physicalLeftClicksObserved++;
                    if (captureActive && taskbarPoint)
                    {
                        taskbarClicksObserved++;
                    }
                    if (captureActive && taskbarPoint && primed && firefoxWindow != IntPtr.Zero &&
                        foreground == firefoxWindow && !minimizedConfirmed)
                    {
                        if (candidateActive)
                        {
                            candidateReplacements++;
                        }
                        CancelCandidateLocked();
                        minimizeGeneration++;
                        minimizePending = true;
                        minimizeWindow = firefoxWindow;
                        minimizeDeadline = now + MinimizeConfirmationMilliseconds;
                        possibleMinimizeClicks++;
                    }
                    else if (captureActive && taskbarPoint && (!primed || minimizedConfirmed))
                    {
                        if (candidateActive)
                        {
                            candidateReplacements++;
                        }
                        candidateGeneration++;
                        candidateActive = true;
                        candidateClickTime = data.Time;
                        candidateDeadline = now + CandidateTimeoutMilliseconds;
                        ForegroundCandidates.Clear();
                        restoreCandidatesObserved++;
                    }
                    else if (captureActive && taskbarPoint)
                    {
                        ignoredUnarmedTaskbarClicks++;
                    }
                }
            }
        }
        return CallNextHookEx(IntPtr.Zero, code, wParam, lParam);
    }

    private static void ForegroundEventCallback(
        IntPtr hook,
        uint eventType,
        IntPtr window,
        int objectId,
        int childId,
        uint eventThread,
        uint eventTime)
    {
        bool signal = false;
        if (captureActive && eventType == EventSystemForeground && window != IntPtr.Zero)
        {
            lock (Sync)
            {
                if (captureActive && candidateActive)
                {
                    ForegroundCandidate candidate = new ForegroundCandidate();
                    candidate.Window = window;
                    candidate.EventTime = eventTime;
                    candidate.Generation = candidateGeneration;
                    ForegroundCandidates.Enqueue(candidate);
                    signal = true;
                }
            }
        }
        if (signal)
        {
            ProbeSignal.Set();
        }
    }

    private static bool CompleteResponsiveCandidateLocked(
        int generation,
        IntPtr window,
        CaptureSample sample)
    {
        if (!candidateActive || generation != candidateGeneration)
        {
            return false;
        }
        firefoxWindow = window;
        CancelCandidateLocked();
        if (!primed)
        {
            primed = true;
            minimizedConfirmed = false;
            primingActivations++;
            primingCompletedNotice = true;
        }
        else if (minimizedConfirmed)
        {
            minimizedConfirmed = false;
            CompletedSamples.Enqueue(sample);
        }
        else
        {
            unconfirmedRestoreResults++;
        }
        return true;
    }

    private static void ProbeWorker()
    {
        while (!probeStop)
        {
            ProbeSignal.WaitOne(100);
            while (!probeStop)
            {
                ForegroundCandidate foreground = null;
                uint clickTime = 0;
                ulong deadline = 0;
                lock (Sync)
                {
                    if (ForegroundCandidates.Count == 0)
                    {
                        break;
                    }
                    foreground = ForegroundCandidates.Dequeue();
                    if (!candidateActive || foreground.Generation != candidateGeneration)
                    {
                        foreground = null;
                    }
                    else
                    {
                        clickTime = candidateClickTime;
                        deadline = candidateDeadline;
                    }
                }
                if (foreground == null)
                {
                    continue;
                }
                if (ElapsedMilliseconds(clickTime, foreground.EventTime) >
                    CandidateTimeoutMilliseconds)
                {
                    continue;
                }
                if (!IsFirefoxWindow(foreground.Window) || IsIconic(foreground.Window))
                {
                    continue;
                }

                while (!probeStop)
                {
                    lock (Sync)
                    {
                        if (!candidateActive || foreground.Generation != candidateGeneration)
                        {
                            break;
                        }
                    }
                    UIntPtr response;
                    IntPtr sent = SendMessageTimeout(
                        foreground.Window,
                        WmNull,
                        UIntPtr.Zero,
                        IntPtr.Zero,
                        SmtoBlock | SmtoAbortIfHung,
                        50,
                        out response);
                    if (sent != IntPtr.Zero)
                    {
                        uint responsiveTime = GetTickCount();
                        CaptureSample sample = new CaptureSample();
                        sample.ClickToForegroundMs =
                            ElapsedMilliseconds(clickTime, foreground.EventTime);
                        sample.ForegroundToResponsiveMs =
                            ElapsedMilliseconds(foreground.EventTime, responsiveTime);
                        sample.ClickToResponsiveMs =
                            ElapsedMilliseconds(clickTime, responsiveTime);
                        lock (Sync)
                        {
                            CompleteResponsiveCandidateLocked(
                                foreground.Generation,
                                foreground.Window,
                                sample);
                        }
                        break;
                    }
                    if (GetTickCount64() >= deadline)
                    {
                        break;
                    }
                    Thread.Sleep(2);
                }
            }
        }
    }

    private static int StateSelfTest(string resultPath)
    {
        ResetCaptureState();
        CaptureSample sample = new CaptureSample();
        sample.ClickToForegroundMs = 10.0;
        sample.ForegroundToResponsiveMs = 5.0;
        sample.ClickToResponsiveMs = 15.0;
        bool staleRejected;
        bool unconfirmedRejected;
        bool confirmedAccepted;
        bool wrapCorrect = ElapsedMilliseconds(0xFFFFFFF0u, 0x00000010u) == 32.0;
        lock (Sync)
        {
            primed = true;
            minimizedConfirmed = false;
            candidateGeneration = 10;
            candidateActive = true;
            staleRejected = !CompleteResponsiveCandidateLocked(
                9,
                new IntPtr(123),
                sample) && CompletedSamples.Count == 0;
            unconfirmedRejected = CompleteResponsiveCandidateLocked(
                10,
                new IntPtr(123),
                sample) &&
                CompletedSamples.Count == 0 &&
                unconfirmedRestoreResults == 1;

            candidateActive = true;
            int validGeneration = candidateGeneration;
            minimizedConfirmed = true;
            confirmedAccepted = CompleteResponsiveCandidateLocked(
                validGeneration,
                new IntPtr(123),
                sample) &&
                CompletedSamples.Count == 1 &&
                !minimizedConfirmed;
        }
        bool pass = staleRejected && unconfirmedRejected && confirmedAccepted && wrapCorrect;
        WriteText(
            resultPath,
            "{\"schema_version\":1,\"status\":\"" + (pass ? "pass" : "fail") +
            "\",\"stale_generation_rejected\":" + Boolean(staleRejected) +
            ",\"unconfirmed_restore_rejected\":" + Boolean(unconfirmedRejected) +
            ",\"confirmed_restore_accepted\":" + Boolean(confirmedAccepted) +
            ",\"uint_wrap_elapsed_ms\":" +
            ElapsedMilliseconds(0xFFFFFFF0u, 0x00000010u).ToString(
                "F0",
                CultureInfo.InvariantCulture) + "}");
        return pass ? 0 : 8;
    }

    private static CaptureStateSnapshot SnapshotState()
    {
        lock (Sync)
        {
            CaptureStateSnapshot snapshot = new CaptureStateSnapshot();
            snapshot.PhysicalLeftClicksObserved = physicalLeftClicksObserved;
            snapshot.TaskbarClicksObserved = taskbarClicksObserved;
            snapshot.CandidateTimeouts = candidateTimeouts;
            snapshot.CandidateReplacements = candidateReplacements;
            snapshot.ConfirmedMinimizeClicks = confirmedMinimizeClicks;
            snapshot.PossibleMinimizeClicks = possibleMinimizeClicks;
            snapshot.RejectedNonMinimizeClicks = rejectedNonMinimizeClicks;
            snapshot.IgnoredInjectedClicks = ignoredInjectedClicks;
            snapshot.IgnoredUnarmedTaskbarClicks = ignoredUnarmedTaskbarClicks;
            snapshot.PrimingActivations = primingActivations;
            snapshot.RestoreCandidatesObserved = restoreCandidatesObserved;
            snapshot.UnconfirmedRestoreResults = unconfirmedRestoreResults;
            return snapshot;
        }
    }

    private static int Guard(string resultPath)
    {
        bool available;
        bool fullscreen = IsFullscreenOrPresentation(out available);
        bool remote = GetSystemMetrics(SmRemoteSession) != 0;
        string status = available && !fullscreen && !remote ? "pass" : "rejected";
        WriteText(
            resultPath,
            "{\"schema_version\":1,\"status\":\"" + status +
            "\",\"notification_state_available\":" + Boolean(available) +
            ",\"fullscreen_or_presentation\":" + Boolean(fullscreen) +
            ",\"remote_session\":" + Boolean(remote) + "}");
        return status == "pass" ? 0 : 3;
    }

    private static void AppendSampleArray(
        StringBuilder builder,
        List<CaptureSample> samples,
        Func<CaptureSample, double> selector)
    {
        for (int index = 0; index < samples.Count; ++index)
        {
            if (index != 0)
            {
                builder.Append(',');
            }
            builder.Append(selector(samples[index]).ToString("F3", CultureInfo.InvariantCulture));
        }
    }

    private static void WriteCaptureResult(
        string path,
        string status,
        List<CaptureSample> samples,
        int fullscreenRejections,
        bool guardAvailable)
    {
        CaptureStateSnapshot state = SnapshotState();
        int rejected = state.CandidateTimeouts +
            state.CandidateReplacements +
            state.RejectedNonMinimizeClicks +
            state.UnconfirmedRestoreResults;
        StringBuilder builder = new StringBuilder();
        builder.Append("{\"schema_version\":3,\"capture_mode\":\"passive_taskbar_restore\",\"status\":\"");
        builder.Append(status);
        builder.Append("\",\"valid_attempts\":");
        builder.Append(samples.Count.ToString(CultureInfo.InvariantCulture));
        builder.Append(",\"physical_left_clicks_observed\":");
        builder.Append(state.PhysicalLeftClicksObserved.ToString(CultureInfo.InvariantCulture));
        builder.Append(",\"taskbar_clicks_observed\":");
        builder.Append(state.TaskbarClicksObserved.ToString(CultureInfo.InvariantCulture));
        builder.Append(",\"rejected_attempts\":");
        builder.Append(rejected.ToString(CultureInfo.InvariantCulture));
        builder.Append(",\"candidate_timeouts\":");
        builder.Append(state.CandidateTimeouts.ToString(CultureInfo.InvariantCulture));
        builder.Append(",\"candidate_replacements\":");
        builder.Append(state.CandidateReplacements.ToString(CultureInfo.InvariantCulture));
        builder.Append(",\"ignored_minimize_clicks\":");
        builder.Append(state.ConfirmedMinimizeClicks.ToString(CultureInfo.InvariantCulture));
        builder.Append(",\"possible_minimize_clicks\":");
        builder.Append(state.PossibleMinimizeClicks.ToString(CultureInfo.InvariantCulture));
        builder.Append(",\"rejected_non_minimize_clicks\":");
        builder.Append(state.RejectedNonMinimizeClicks.ToString(CultureInfo.InvariantCulture));
        builder.Append(",\"ignored_injected_clicks\":");
        builder.Append(state.IgnoredInjectedClicks.ToString(CultureInfo.InvariantCulture));
        builder.Append(",\"ignored_unarmed_taskbar_clicks\":");
        builder.Append(state.IgnoredUnarmedTaskbarClicks.ToString(CultureInfo.InvariantCulture));
        builder.Append(",\"priming_activations\":");
        builder.Append(state.PrimingActivations.ToString(CultureInfo.InvariantCulture));
        builder.Append(",\"restore_candidates_observed\":");
        builder.Append(state.RestoreCandidatesObserved.ToString(CultureInfo.InvariantCulture));
        builder.Append(",\"unconfirmed_restore_results\":");
        builder.Append(state.UnconfirmedRestoreResults.ToString(CultureInfo.InvariantCulture));
        builder.Append(",\"fullscreen_rejections\":");
        builder.Append(fullscreenRejections.ToString(CultureInfo.InvariantCulture));
        builder.Append(",\"notification_state_available\":");
        builder.Append(Boolean(guardAvailable));
        builder.Append(",\"input_generated\":false");
        builder.Append(",\"per_monitor_dpi_aware\":");
        builder.Append(Boolean(perMonitorDpiAware));
        builder.Append(",\"dedicated_probe_thread\":true");
        builder.Append(",\"minimize_state_required\":true");
        builder.Append(",\"click_timestamp_source\":\"MSLLHOOKSTRUCT.time\"");
        builder.Append(",\"foreground_timestamp_source\":\"EVENT_SYSTEM_FOREGROUND.time\"");
        builder.Append(",\"click_to_foreground_ms\":[");
        AppendSampleArray(builder, samples, delegate(CaptureSample sample) {
            return sample.ClickToForegroundMs;
        });
        builder.Append("],\"foreground_to_responsive_ms\":[");
        AppendSampleArray(builder, samples, delegate(CaptureSample sample) {
            return sample.ForegroundToResponsiveMs;
        });
        builder.Append("],\"click_to_responsive_ms\":[");
        AppendSampleArray(builder, samples, delegate(CaptureSample sample) {
            return sample.ClickToResponsiveMs;
        });
        builder.Append("]}");
        WriteText(path, builder.ToString());
    }

    private static void ResetCaptureState()
    {
        IntPtr initialForeground = GetForegroundWindow();
        bool initialFirefox = IsFirefoxWindow(initialForeground) && !IsIconic(initialForeground);
        lock (Sync)
        {
            candidateActive = false;
            candidateGeneration = 0;
            candidateClickTime = 0;
            candidateDeadline = 0;
            firefoxWindow = initialFirefox ? initialForeground : IntPtr.Zero;
            primed = initialFirefox;
            minimizedConfirmed = false;
            minimizePending = false;
            minimizeGeneration = 0;
            minimizeWindow = IntPtr.Zero;
            minimizeDeadline = 0;
            primingCompletedNotice = false;
            candidateTimeouts = 0;
            candidateReplacements = 0;
            confirmedMinimizeClicks = 0;
            possibleMinimizeClicks = 0;
            rejectedNonMinimizeClicks = 0;
            ignoredInjectedClicks = 0;
            ignoredUnarmedTaskbarClicks = 0;
            primingActivations = 0;
            restoreCandidatesObserved = 0;
            unconfirmedRestoreResults = 0;
            physicalLeftClicksObserved = 0;
            taskbarClicksObserved = 0;
            ForegroundCandidates.Clear();
            CompletedSamples.Clear();
        }
    }

    private static int Capture(
        string resultPath,
        string readyPath,
        string startPath,
        int durationSeconds,
        int requiredAttempts)
    {
        perMonitorDpiAware = SetProcessDpiAwarenessContext(new IntPtr(-4));
        if (!perMonitorDpiAware)
        {
            ResetCaptureState();
            WriteCaptureResult(
                resultPath,
                "dpi_awareness_failed",
                new List<CaptureSample>(),
                0,
                true);
            return 11;
        }
        IntPtr mouseHook = SetWindowsHookEx(
            WhMouseLl,
            MouseProcedure,
            GetModuleHandle(null),
            0);
        IntPtr foregroundHook = SetWinEventHook(
            EventSystemForeground,
            EventSystemForeground,
            IntPtr.Zero,
            ForegroundProcedure,
            0,
            0,
            WineventOutOfContext);
        if (mouseHook == IntPtr.Zero || foregroundHook == IntPtr.Zero)
        {
            if (mouseHook != IntPtr.Zero) UnhookWindowsHookEx(mouseHook);
            if (foregroundHook != IntPtr.Zero) UnhookWinEvent(foregroundHook);
            ResetCaptureState();
            WriteCaptureResult(
                resultPath,
                "passive_hook_registration_failed",
                new List<CaptureSample>(),
                0,
                true);
            return 2;
        }

        Thread probeThread = null;
        try
        {
            bool available;
            bool fullscreen = IsFullscreenOrPresentation(out available);
            if (!available || fullscreen || GetSystemMetrics(SmRemoteSession) != 0)
            {
                ResetCaptureState();
                WriteCaptureResult(
                    resultPath,
                    "fullscreen_presentation_or_remote_rejected",
                    new List<CaptureSample>(),
                    fullscreen ? 1 : 0,
                    available);
                return 3;
            }
            WriteText(readyPath, "ready");
            Console.WriteLine(
                "[marker] Passive mouse/foreground hooks registered. The response probe uses a separate worker thread.");
            Stopwatch startWait = Stopwatch.StartNew();
            while (!File.Exists(startPath))
            {
                if (startWait.Elapsed.TotalMinutes >= 5.0)
                {
                    ResetCaptureState();
                    WriteCaptureResult(
                        resultPath,
                        "start_timeout",
                        new List<CaptureSample>(),
                        0,
                        true);
                    return 4;
                }
                Message waitingMessage;
                while (PeekMessage(out waitingMessage, IntPtr.Zero, 0, 0, PmRemove))
                {
                    TranslateMessage(ref waitingMessage);
                    DispatchMessage(ref waitingMessage);
                }
                Thread.Sleep(25);
            }

            ResetCaptureState();
            probeStop = false;
            probeThread = new Thread(ProbeWorker);
            probeThread.IsBackground = true;
            probeThread.Name = "WinSched passive Firefox probe";
            probeThread.Start();
            captureActive = true;

            List<CaptureSample> samples = new List<CaptureSample>();
            int fullscreenRejections = 0;
            Stopwatch phase = Stopwatch.StartNew();
            long nextGuardCheck = 0;
            bool guardAvailable = true;
            bool guardViolation = false;
            int printedPossibleMinimize = 0;
            int printedConfirmedMinimize = 0;
            int printedRejectedNonMinimize = 0;
            int printedRestoreCandidates = 0;
            int printedTimeouts = 0;
            WriteCaptureResult(
                resultPath,
                "in_progress",
                samples,
                fullscreenRejections,
                guardAvailable);
            Console.WriteLine(
                "[marker] Capture started. Required valid attempts: " +
                requiredAttempts.ToString(CultureInfo.InvariantCulture));
            Console.WriteLine(
                "[marker] For EACH attempt: click Firefox once to minimize it, wait about one second, then click the same taskbar icon to restore it.");
            bool needsPriming;
            lock (Sync)
            {
                needsPriming = !primed;
            }
            if (needsPriming)
            {
                Console.WriteLine(
                    "[marker] Click Firefox once now to arm the phase. This first activation will not be measured.");
            }

            while (phase.Elapsed.TotalSeconds < durationSeconds)
            {
                Message message;
                while (PeekMessage(out message, IntPtr.Zero, 0, 0, PmRemove))
                {
                    TranslateMessage(ref message);
                    DispatchMessage(ref message);
                }

                long elapsedMilliseconds = phase.ElapsedMilliseconds;
                if (elapsedMilliseconds >= nextGuardCheck)
                {
                    bool currentAvailable;
                    bool currentFullscreen = IsFullscreenOrPresentation(out currentAvailable);
                    guardAvailable = guardAvailable && currentAvailable;
                    if (!currentAvailable || currentFullscreen)
                    {
                        fullscreenRejections++;
                        guardViolation = true;
                        Console.WriteLine("[marker] Fullscreen/presentation detected. Phase rejected.");
                    }
                    nextGuardCheck = elapsedMilliseconds + 250;
                }

                IntPtr windowToCheck = IntPtr.Zero;
                int minimizeToCheck = 0;
                ulong minimizeExpires = 0;
                lock (Sync)
                {
                    if (minimizePending)
                    {
                        windowToCheck = minimizeWindow;
                        minimizeToCheck = minimizeGeneration;
                        minimizeExpires = minimizeDeadline;
                    }
                }
                if (windowToCheck != IntPtr.Zero)
                {
                    bool iconic = IsIconic(windowToCheck);
                    bool expired = GetTickCount64() >= minimizeExpires;
                    if (iconic || expired)
                    {
                        lock (Sync)
                        {
                            if (minimizePending &&
                                minimizeGeneration == minimizeToCheck &&
                                minimizeWindow == windowToCheck)
                            {
                                minimizePending = false;
                                minimizeWindow = IntPtr.Zero;
                                if (iconic)
                                {
                                    minimizedConfirmed = true;
                                    confirmedMinimizeClicks++;
                                }
                                else
                                {
                                    minimizedConfirmed = false;
                                    rejectedNonMinimizeClicks++;
                                }
                            }
                        }
                    }
                }

                bool writeProgress = false;
                bool primingNotice = false;
                List<CaptureSample> newlyCompleted = new List<CaptureSample>();
                ulong now = GetTickCount64();
                lock (Sync)
                {
                    if (candidateActive && now >= candidateDeadline)
                    {
                        candidateTimeouts++;
                        CancelCandidateLocked();
                    }
                    if (primingCompletedNotice)
                    {
                        primingCompletedNotice = false;
                        primingNotice = true;
                        writeProgress = true;
                    }
                    while (CompletedSamples.Count != 0)
                    {
                        newlyCompleted.Add(CompletedSamples.Dequeue());
                    }
                }
                if (newlyCompleted.Count != 0)
                {
                    samples.AddRange(newlyCompleted);
                    writeProgress = true;
                }

                CaptureStateSnapshot state = SnapshotState();
                if (state.PossibleMinimizeClicks > printedPossibleMinimize)
                {
                    printedPossibleMinimize = state.PossibleMinimizeClicks;
                    Console.WriteLine(
                        "[marker] Taskbar click observed while Firefox was foreground. Verifying that Firefox is actually minimized.");
                }
                if (state.ConfirmedMinimizeClicks > printedConfirmedMinimize)
                {
                    printedConfirmedMinimize = state.ConfirmedMinimizeClicks;
                    writeProgress = true;
                    Console.WriteLine(
                        "[marker] Firefox minimize confirmed. Wait about one second, then click the same taskbar icon to restore it.");
                }
                if (state.RejectedNonMinimizeClicks > printedRejectedNonMinimize)
                {
                    printedRejectedNonMinimize = state.RejectedNonMinimizeClicks;
                    writeProgress = true;
                    Console.WriteLine(
                        "[marker] The click did not actually minimize Firefox and was rejected.");
                }
                if (state.RestoreCandidatesObserved > printedRestoreCandidates)
                {
                    printedRestoreCandidates = state.RestoreCandidatesObserved;
                    Console.WriteLine(
                        "[marker] Physical taskbar restore candidate observed. Waiting for Firefox foreground and response.");
                }
                if (state.CandidateTimeouts > printedTimeouts)
                {
                    printedTimeouts = state.CandidateTimeouts;
                    writeProgress = true;
                    Console.WriteLine("[marker] Restore candidate timed out and was rejected.");
                }
                if (primingNotice)
                {
                    Console.WriteLine(
                        "[marker] Firefox is armed. Now click it to minimize, wait about one second, then click it again to restore.");
                }
                foreach (CaptureSample sample in newlyCompleted)
                {
                    Console.WriteLine(
                        "[marker] Valid Firefox restore " +
                        (samples.IndexOf(sample) + 1).ToString(CultureInfo.InvariantCulture) +
                        "/" + requiredAttempts.ToString(CultureInfo.InvariantCulture) +
                        ": " + sample.ClickToResponsiveMs.ToString("F1", CultureInfo.InvariantCulture) + " ms");
                }
                if (writeProgress)
                {
                    WriteCaptureResult(
                        resultPath,
                        "in_progress",
                        samples,
                        fullscreenRejections,
                        guardAvailable);
                }
                if (guardViolation || samples.Count >= requiredAttempts)
                {
                    break;
                }
                Thread.Sleep(2);
            }

            captureActive = false;
            probeStop = true;
            ProbeSignal.Set();
            bool probeStopped = probeThread.Join(5000);
            string status;
            int exitCode;
            if (!probeStopped)
            {
                status = "probe_worker_stop_failed";
                exitCode = 7;
            }
            else if (guardViolation)
            {
                status = "fullscreen_or_presentation_rejected";
                exitCode = 5;
            }
            else if (samples.Count < requiredAttempts)
            {
                status = "insufficient_attempts";
                exitCode = 6;
            }
            else
            {
                status = "complete";
                exitCode = 0;
            }
            WriteCaptureResult(
                resultPath,
                status,
                samples,
                fullscreenRejections,
                guardAvailable);
            CaptureStateSnapshot finalState = SnapshotState();
            int rejected = finalState.CandidateTimeouts +
                finalState.CandidateReplacements +
                finalState.RejectedNonMinimizeClicks +
                finalState.UnconfirmedRestoreResults;
            Console.WriteLine(
                "[marker] Phase result: " + status + ", valid=" +
                samples.Count.ToString(CultureInfo.InvariantCulture) +
                ", rejected=" + rejected.ToString(CultureInfo.InvariantCulture) + ".");
            return exitCode;
        }
        finally
        {
            captureActive = false;
            probeStop = true;
            ProbeSignal.Set();
            UnhookWindowsHookEx(mouseHook);
            UnhookWinEvent(foregroundHook);
            if (probeThread != null && probeThread.IsAlive)
            {
                probeThread.Join(5000);
            }
        }
    }

    private static int Main(string[] args)
    {
        try
        {
            if (args.Length == 2 && String.Equals(args[0], "guard", StringComparison.Ordinal))
            {
                return Guard(args[1]);
            }
            if (args.Length == 2 &&
                String.Equals(args[0], "state-selftest", StringComparison.Ordinal))
            {
                return StateSelfTest(args[1]);
            }
            if (args.Length == 6 && String.Equals(args[0], "capture", StringComparison.Ordinal))
            {
                return Capture(
                    args[1],
                    args[2],
                    args[3],
                    Int32.Parse(args[4], CultureInfo.InvariantCulture),
                    Int32.Parse(args[5], CultureInfo.InvariantCulture));
            }
            return 9;
        }
        catch
        {
            return 10;
        }
    }
}
'@

function Invoke-Phase(
    [int]$Sequence,
    [string]$Label,
    [bool]$SchedulingEnabled,
    [int]$ServicePid,
    [int]$TrayPid,
    [long]$TrayStartTicks,
    [int]$LogicalProcessors
) {
    [void](Set-SchedulingState $SchedulingEnabled $ServicePid)
    $schedulingLabel = if ($SchedulingEnabled) { "ENABLED (Auto)" } else { "DISABLED" }
    Write-Host ""
    Write-Host ("================ PHASE {0}/4: {1} ================" -f $Sequence, $Label)
    Write-Host ("Scheduling: {0}" -f $schedulingLabel)
    Write-Host ("Settling for {0} seconds. Do not start the taskbar clicks yet." -f $SettleSeconds)
    Write-Host "Leave Firefox, WSL, and VMware in the normal workload state."
    if ($SettleSeconds -gt 0) {
        Start-Sleep -Seconds $SettleSeconds
    }
    $settledStatus = Read-Status
    Assert-True ($null -ne $settledStatus) "ABBA_STATUS_UNAVAILABLE"
    Assert-True ([int]$settledStatus.service_pid -eq $ServicePid) "ABBA_SERVICE_RESTARTED"
    Assert-True ([bool]$settledStatus.scheduling_enabled -eq $SchedulingEnabled) `
        "ABBA_SCHEDULING_STATE_DRIFT"

    Write-Host ""
    Write-Host ("READ THIS BEFORE PHASE {0}/4:" -f $Sequence)
    Write-Host ("You must complete {0} Firefox RESTORE attempts in this phase." -f $AttemptsPerPhase)
    Write-Host "No hotkeys are used. The helper only observes your real taskbar clicks."
    Write-Host "After capture starts:"
    Write-Host "  1. If asked, click Firefox once to arm the phase; this first activation is not measured."
    Write-Host "  2. Click the Firefox taskbar icon once to minimize Firefox."
    Write-Host "  3. Wait about one second."
    Write-Host "  4. Click the same icon once to restore Firefox. That restore is measured automatically."
    Write-Host "  5. Repeat steps 2-4 until the helper prints the required valid count."
    Write-Host "Do not click other taskbar icons during a measured pair."
    [void](Read-Host ("Press ENTER when you are ready to start phase {0}/4 capture" -f $Sequence))

    $prefix = "phase-{0}" -f $Sequence
    $diagnoseOutput = Join-Path $script:workDirectory "$prefix-diagnose.json"
    $diagnoseError = Join-Path $script:workDirectory "$prefix-diagnose.stderr"
    $observeOutput = Join-Path $script:workDirectory "$prefix-observe.jsonl"
    $observeError = Join-Path $script:workDirectory "$prefix-observe.stderr"
    $markerOutput = Join-Path $script:workDirectory "$prefix-marker.json"
    $markerReady = Join-Path $script:workDirectory "$prefix-marker.ready"
    $markerStart = Join-Path $script:workDirectory "$prefix-marker.start"
    $markerError = Join-Path $script:workDirectory "$prefix-marker.stderr"
    $diagnoseProcess = $null
    $observeProcess = $null
    $markerProcess = $null
    $serviceSamples = New-Object System.Collections.ArrayList
    $traySamples = New-Object System.Collections.ArrayList
    try {
        $markerProcess = Start-Process `
            -FilePath $script:markerBinary `
            -WorkingDirectory $script:workDirectory `
            -ArgumentList @(
                "capture",
                (Split-Path -Leaf $markerOutput),
                (Split-Path -Leaf $markerReady),
                (Split-Path -Leaf $markerStart),
                $MeasurementSeconds,
                $AttemptsPerPhase
            ) `
            -RedirectStandardError $markerError `
            -NoNewWindow `
            -PassThru
        Wait-Condition "ABBA_MARKER_READY_TIMEOUT" {
            (Test-Path -LiteralPath $markerReady -PathType Leaf) -or $markerProcess.HasExited
        } 15
        Assert-True (-not $markerProcess.HasExited) "ABBA_MARKER_START_FAILED"

        $diagnoseProcess = Start-Process `
            -FilePath $script:observerBinary `
            -ArgumentList @(
                "diagnose",
                "--duration-seconds", $MeasurementSeconds,
                "--interval-ms", 250,
                "--taskbar-timeout-ms", $TaskbarTimeoutMs,
                "--json"
            ) `
            -RedirectStandardOutput $diagnoseOutput `
            -RedirectStandardError $diagnoseError `
            -PassThru
        $observeProcess = Start-Process `
            -FilePath $script:observerBinary `
            -ArgumentList @(
                "observe",
                "--samples", $MeasurementSeconds,
                "--interval-ms", 1000,
                "--json"
            ) `
            -RedirectStandardOutput $observeOutput `
            -RedirectStandardError $observeError `
            -PassThru

        Wait-Condition "ABBA_OBSERVERS_EXITED_EARLY" {
            -not $diagnoseProcess.HasExited -and -not $observeProcess.HasExited
        } 5
        Assert-Unassigned $markerProcess.Id
        Assert-Unassigned $diagnoseProcess.Id
        Assert-Unassigned $observeProcess.Id

        $serviceBefore = Get-ProcessSnapshot $ServicePid
        $trayBefore = Get-ProcessSnapshot $TrayPid
        Assert-True ($trayBefore.start_time_ticks -eq $TrayStartTicks) "ABBA_TRAY_RESTARTED"
        Write-Utf8NoBom $markerStart "start"
        Write-Host ""
        Write-Host ("PHASE {0}/4 CAPTURE IS ACTIVE. Scheduling: {1}" -f $Sequence, $schedulingLabel)
        Write-Host ("Minimize Firefox, wait about one second, then restore it; repeat until {0}/{0}." -f $AttemptsPerPhase)
        Write-Host "The minimize click is ignored; only the restore click is timed. No hotkeys are needed."
        Write-Host "Do not close this PowerShell window. The next phase starts automatically."
        $captureStarted = [DateTime]::UtcNow
        $deadline = $captureStarted.AddSeconds($MeasurementSeconds + 30)
        $markerCompletionAnnounced = $false
        do {
            $currentStatus = Read-Status
            Assert-True ($null -ne $currentStatus) "ABBA_STATUS_UNAVAILABLE"
            Assert-True ([int]$currentStatus.service_pid -eq $ServicePid) "ABBA_SERVICE_RESTARTED"
            Assert-True ([bool]$currentStatus.scheduling_enabled -eq $SchedulingEnabled) `
                "ABBA_SCHEDULING_STATE_DRIFT"
            [void]$serviceSamples.Add((Get-ProcessSample $ServicePid $serviceBefore.start_time_ticks))
            [void]$traySamples.Add((Get-ProcessSample $TrayPid $TrayStartTicks))
            if ($markerProcess.HasExited -and
                (Test-Path -LiteralPath $markerOutput -PathType Leaf)) {
                $earlyMarker = Get-Content -LiteralPath $markerOutput -Raw | ConvertFrom-Json
                if ([string]$earlyMarker.status -match 'fullscreen|presentation|remote') {
                    Throw-Abba "ABBA_FULLSCREEN_OR_PRESENTATION_REJECTED"
                }
                if (-not $markerCompletionAnnounced -and
                    [string]$earlyMarker.status -eq "complete" -and
                    [int]$earlyMarker.valid_attempts -ge $AttemptsPerPhase) {
                    $markerCompletionAnnounced = $true
                    $remainingSeconds = [Math]::Max(
                        0,
                        [Math]::Ceiling((
                            $captureStarted.AddSeconds($MeasurementSeconds) - [DateTime]::UtcNow
                        ).TotalSeconds)
                    )
                    Write-Host (
                        "{0}/{0} accepted. No more clicks in this phase; background observation continues for about {1} second(s)." -f `
                            $AttemptsPerPhase,
                            $remainingSeconds
                    ) -ForegroundColor Green
                }
            }
            if ($diagnoseProcess.HasExited -and $observeProcess.HasExited -and $markerProcess.HasExited) {
                break
            }
            Start-Sleep -Seconds 1
        } while ([DateTime]::UtcNow -lt $deadline)

        Assert-True ($diagnoseProcess.HasExited) "ABBA_DIAGNOSE_TIMEOUT"
        Assert-True ($observeProcess.HasExited) "ABBA_OBSERVE_TIMEOUT"
        [void]$diagnoseProcess.WaitForExit(5000)
        [void]$observeProcess.WaitForExit(5000)
        [void]$markerProcess.WaitForExit(5000)
        $markerTimedOut = -not $markerProcess.HasExited
        Assert-True (Test-Path -LiteralPath $markerOutput -PathType Leaf) `
            "ABBA_MARKER_RESULT_MISSING"
        $marker = Get-Content -LiteralPath $markerOutput -Raw | ConvertFrom-Json
        $markerComplete = [string]$marker.status -eq "complete" -and
            [int]$marker.valid_attempts -ge $AttemptsPerPhase
        $passiveCheckpoint = [ordered]@{
            sequence = $Sequence
            label = $Label
            scheduling_enabled = $SchedulingEnabled
            measurement_complete = $false
            passive_measurement_complete = $markerComplete
            auxiliary_data_complete = $false
            marker = [ordered]@{
                schema_version = [int]$marker.schema_version
                capture_mode = [string]$marker.capture_mode
                status = [string]$marker.status
                valid_attempts = [int]$marker.valid_attempts
                rejected_attempts = [int]$marker.rejected_attempts
                candidate_timeouts = [int]$marker.candidate_timeouts
                candidate_replacements = [int]$marker.candidate_replacements
                ignored_minimize_clicks = [int]$marker.ignored_minimize_clicks
                possible_minimize_clicks = [int]$marker.possible_minimize_clicks
                rejected_non_minimize_clicks = [int]$marker.rejected_non_minimize_clicks
                ignored_injected_clicks = [int]$marker.ignored_injected_clicks
                ignored_unarmed_taskbar_clicks = [int]$marker.ignored_unarmed_taskbar_clicks
                priming_activations = [int]$marker.priming_activations
                restore_candidates_observed = [int]$marker.restore_candidates_observed
                unconfirmed_restore_results = [int]$marker.unconfirmed_restore_results
                fullscreen_rejections = [int]$marker.fullscreen_rejections
                input_generated = [bool]$marker.input_generated
                per_monitor_dpi_aware = [bool]$marker.per_monitor_dpi_aware
                dedicated_probe_thread = [bool]$marker.dedicated_probe_thread
                minimize_state_required = [bool]$marker.minimize_state_required
                click_timestamp_source = [string]$marker.click_timestamp_source
                foreground_timestamp_source = [string]$marker.foreground_timestamp_source
                click_to_foreground_ms = @(
                    $marker.click_to_foreground_ms | ForEach-Object { [double]$_ }
                )
                foreground_to_responsive_ms = @(
                    $marker.foreground_to_responsive_ms | ForEach-Object { [double]$_ }
                )
                click_to_responsive_ms = @(
                    $marker.click_to_responsive_ms | ForEach-Object { [double]$_ }
                )
                timing = Get-MetricSummary @($marker.click_to_responsive_ms)
            }
        }
        $checkpointPath = Join-Path $script:workDirectory "$prefix-passive-checkpoint.json"
        Write-Utf8NoBom $checkpointPath ([pscustomobject]$passiveCheckpoint | ConvertTo-Json -Depth 8)
        if ($markerTimedOut) {
            Throw-Abba "ABBA_MARKER_TIMEOUT"
        }
        $diagnoseExitCode = [int]$diagnoseProcess.ExitCode
        $observeExitCode = [int]$observeProcess.ExitCode
        Assert-True ([string]::IsNullOrWhiteSpace(
            (Get-Content -LiteralPath $markerError -Raw -ErrorAction SilentlyContinue)
        )) "ABBA_MARKER_STDERR"

        $serviceAfter = Get-ProcessSnapshot $ServicePid
        $trayAfter = Get-ProcessSnapshot $TrayPid
        Assert-True ($serviceAfter.start_time_ticks -eq $serviceBefore.start_time_ticks) `
            "ABBA_SERVICE_RESTARTED"
        Assert-True ($trayAfter.start_time_ticks -eq $TrayStartTicks) "ABBA_TRAY_RESTARTED"
        $endingStatus = Read-Status
        Assert-True ($null -ne $endingStatus) "ABBA_STATUS_UNAVAILABLE"
        Assert-True ([int]$endingStatus.service_pid -eq $ServicePid) "ABBA_SERVICE_RESTARTED"
        Assert-True ([bool]$endingStatus.scheduling_enabled -eq $SchedulingEnabled) `
            "ABBA_SCHEDULING_STATE_DRIFT"

        $diagnosticRaw = Get-Content -LiteralPath $diagnoseOutput -Raw | ConvertFrom-Json
        $diagnostic = Convert-DiagnosticReport $diagnosticRaw
        $llc = Convert-ObserveJsonLines $observeOutput
        $minimumDiagnosticSamples = [Math]::Max(1, $MeasurementSeconds * 3)
        Assert-True ([int]$diagnostic.schema_version -eq 1) "ABBA_DIAGNOSE_SCHEMA"
        Assert-True (
            [uint64]$diagnostic.duration_ms -ge [uint64]([Math]::Max(1, $MeasurementSeconds * 1000 - 1000))
        ) "ABBA_DIAGNOSE_DURATION_SHORT"
        Assert-True ([int]$diagnostic.sample_count -ge $minimumDiagnosticSamples) `
            "ABBA_DIAGNOSE_SAMPLES_SPARSE"
        Assert-True ([uint64]$diagnostic.taskbar.samples -eq [uint64]$diagnostic.sample_count) `
            "ABBA_TASKBAR_SAMPLE_CADENCE"
        Assert-True ([int]$llc.samples -eq $MeasurementSeconds) "ABBA_OBSERVE_SAMPLE_COUNT"
        Assert-True (@($llc.domains).Count -eq 8) "ABBA_OBSERVE_LLC_COUNT"
        if ($diagnoseExitCode -ne 0) {
            Write-Host ("Diagnostic exit code {0} was tolerated because its schema/sample checks passed." -f `
                $diagnoseExitCode) -ForegroundColor Yellow
        }
        if ($observeExitCode -ne 0) {
            Write-Host ("Observe exit code {0} was tolerated because its sample/domain checks passed." -f `
                $observeExitCode) -ForegroundColor Yellow
        }
        Write-Host (
            "Phase {0}/4 finished: {1} valid, {2} rejected attempt(s)." -f `
                $Sequence,
                [int]$marker.valid_attempts,
                [int]$marker.rejected_attempts
        )
        $duration = ([DateTime]::UtcNow - $captureStarted).TotalSeconds
        return [ordered]@{
            sequence = $Sequence
            label = $Label
            scheduling_enabled = $SchedulingEnabled
            measurement_complete = $markerComplete
            passive_measurement_complete = $markerComplete
            auxiliary_data_complete = $true
            marker = [ordered]@{
                schema_version = [int]$marker.schema_version
                capture_mode = [string]$marker.capture_mode
                status = [string]$marker.status
                valid_attempts = [int]$marker.valid_attempts
                rejected_attempts = [int]$marker.rejected_attempts
                candidate_timeouts = [int]$marker.candidate_timeouts
                candidate_replacements = [int]$marker.candidate_replacements
                ignored_minimize_clicks = [int]$marker.ignored_minimize_clicks
                possible_minimize_clicks = [int]$marker.possible_minimize_clicks
                rejected_non_minimize_clicks = [int]$marker.rejected_non_minimize_clicks
                ignored_injected_clicks = [int]$marker.ignored_injected_clicks
                ignored_unarmed_taskbar_clicks = [int]$marker.ignored_unarmed_taskbar_clicks
                priming_activations = [int]$marker.priming_activations
                restore_candidates_observed = [int]$marker.restore_candidates_observed
                unconfirmed_restore_results = [int]$marker.unconfirmed_restore_results
                fullscreen_rejections = [int]$marker.fullscreen_rejections
                input_generated = [bool]$marker.input_generated
                per_monitor_dpi_aware = [bool]$marker.per_monitor_dpi_aware
                dedicated_probe_thread = [bool]$marker.dedicated_probe_thread
                minimize_state_required = [bool]$marker.minimize_state_required
                click_timestamp_source = [string]$marker.click_timestamp_source
                foreground_timestamp_source = [string]$marker.foreground_timestamp_source
                click_to_foreground_ms = @(
                    $marker.click_to_foreground_ms | ForEach-Object { [double]$_ }
                )
                foreground_to_responsive_ms = @(
                    $marker.foreground_to_responsive_ms | ForEach-Object { [double]$_ }
                )
                click_to_responsive_ms = @(
                    $marker.click_to_responsive_ms | ForEach-Object { [double]$_ }
                )
                timing = Get-MetricSummary @($marker.click_to_responsive_ms)
            }
            diagnostic = $diagnostic
            llc = $llc
            overhead = [ordered]@{
                service = Get-ProcessMetrics `
                    $serviceBefore $serviceAfter @($serviceSamples) $duration $LogicalProcessors
                tray = Get-ProcessMetrics `
                    $trayBefore $trayAfter @($traySamples) $duration $LogicalProcessors
                controller = Get-StatusTelemetryDelta $settledStatus $endingStatus
            }
            integrity = [ordered]@{
                service_process_unchanged = $true
                tray_process_unchanged = $true
                observers_unassigned = $true
                scheduling_state_unchanged_during_measurement = $true
                diagnose_exit_code = $diagnoseExitCode
                observe_exit_code = $observeExitCode
                nonzero_observer_exit_tolerated_after_content_validation = `
                    ($diagnoseExitCode -ne 0 -or $observeExitCode -ne 0)
            }
        }
    } finally {
        Stop-OwnedProcess $diagnoseProcess
        Stop-OwnedProcess $observeProcess
        Stop-OwnedProcess $markerProcess
    }
}

function Get-Comparison([object[]]$Phases) {
    $enabledPhases = @($Phases | Where-Object { [bool]$_.scheduling_enabled })
    $disabledPhases = @($Phases | Where-Object { -not [bool]$_.scheduling_enabled })
    $enabledDurations = @($enabledPhases | ForEach-Object {
        @($_.marker.click_to_responsive_ms)
    })
    $disabledDurations = @($disabledPhases | ForEach-Object {
        @($_.marker.click_to_responsive_ms)
    })
    $enabledTiming = [ordered]@{
        click_to_foreground_ms = Get-MetricSummary @($enabledPhases | ForEach-Object {
            @($_.marker.click_to_foreground_ms)
        })
        foreground_to_responsive_ms = Get-MetricSummary @($enabledPhases | ForEach-Object {
            @($_.marker.foreground_to_responsive_ms)
        })
        click_to_responsive_ms = Get-MetricSummary $enabledDurations
    }
    $disabledTiming = [ordered]@{
        click_to_foreground_ms = Get-MetricSummary @($disabledPhases | ForEach-Object {
            @($_.marker.click_to_foreground_ms)
        })
        foreground_to_responsive_ms = Get-MetricSummary @($disabledPhases | ForEach-Object {
            @($_.marker.foreground_to_responsive_ms)
        })
        click_to_responsive_ms = Get-MetricSummary $disabledDurations
    }
    $improvement = $null
    $enabledP95 = $enabledTiming.click_to_responsive_ms.p95
    $disabledP95 = $disabledTiming.click_to_responsive_ms.p95
    if ($null -ne $disabledP95 -and [double]$disabledP95 -gt 0 -and
        $null -ne $enabledP95) {
        $improvement = 100.0 * ([double]$disabledP95 - [double]$enabledP95) /
            [double]$disabledP95
    }
    $enabledTaskbarP95 = Get-Percentile @($enabledPhases | ForEach-Object {
        [double]$_.diagnostic.taskbar.p95_us
    }) 50
    $disabledTaskbarP95 = Get-Percentile @($disabledPhases | ForEach-Object {
        [double]$_.diagnostic.taskbar.p95_us
    }) 50
    $enabledSchedulerP99 = Get-Percentile @($enabledPhases | ForEach-Object {
        [double]$_.diagnostic.system.scheduler_latency.p99_us
    }) 50
    $disabledSchedulerP99 = Get-Percentile @($disabledPhases | ForEach-Object {
        [double]$_.diagnostic.system.scheduler_latency.p99_us
    }) 50
    $taskbarRegression = $null
    if ($null -ne $disabledTaskbarP95 -and [double]$disabledTaskbarP95 -gt 0) {
        $taskbarRegression = 100.0 * ([double]$enabledTaskbarP95 - [double]$disabledTaskbarP95) /
            [double]$disabledTaskbarP95
    }
    $schedulerRegression = $null
    if ($null -ne $disabledSchedulerP99 -and [double]$disabledSchedulerP99 -gt 0) {
        $schedulerRegression = 100.0 * ([double]$enabledSchedulerP99 - [double]$disabledSchedulerP99) /
            [double]$disabledSchedulerP99
    }

    $pairResults = @()
    foreach ($pair in @(@(0, 1), @(3, 2))) {
        $first = $Phases[$pair[0]]
        $second = $Phases[$pair[1]]
        $enabled = if ([bool]$first.scheduling_enabled) { $first } else { $second }
        $disabled = if ([bool]$first.scheduling_enabled) { $second } else { $first }
        $enabledP95 = [double]$enabled.marker.timing.p95
        $disabledP95 = [double]$disabled.marker.timing.p95
        $direction = if ($enabledP95 -lt $disabledP95) {
            "enabled_faster"
        } elseif ($enabledP95 -gt $disabledP95) {
            "enabled_slower"
        } else {
            "tie"
        }
        $pairResults += [ordered]@{
            enabled_p95_ms = $enabledP95
            disabled_p95_ms = $disabledP95
            direction = $direction
            enabled_is_faster = $enabledP95 -lt $disabledP95
            enabled_is_slower = $enabledP95 -gt $disabledP95
        }
    }
    $complete = $enabledDurations.Count -ge (2 * $AttemptsPerPhase) -and
        $disabledDurations.Count -ge (2 * $AttemptsPerPhase) -and
        @($Phases | Where-Object { -not [bool]$_.measurement_complete }).Count -eq 0
    $passiveTimingPolicyEligible = $complete -and
        @($Phases | Where-Object {
            [int]$_.marker.schema_version -lt 3 -or
            [string]$_.marker.capture_mode -ne "passive_taskbar_restore" -or
            [bool]$_.marker.input_generated -or
            -not [bool]$_.marker.per_monitor_dpi_aware -or
            -not [bool]$_.marker.dedicated_probe_thread -or
            -not [bool]$_.marker.minimize_state_required
        }).Count -eq 0
    $enabledFasterConsistency = $passiveTimingPolicyEligible -and
        @($pairResults | Where-Object { -not [bool]$_.enabled_is_faster }).Count -eq 0
    $enabledSlowerConsistency = $passiveTimingPolicyEligible -and
        @($pairResults | Where-Object { -not [bool]$_.enabled_is_slower }).Count -eq 0
    $pairConsistency = $enabledFasterConsistency -or $enabledSlowerConsistency
    $objectiveNoRegression = $null -ne $taskbarRegression -and
        $null -ne $schedulerRegression -and
        [double]$taskbarRegression -le 10.0 -and
        [double]$schedulerRegression -le 10.0
    $verdict = "invalid"
    if ($passiveTimingPolicyEligible) {
        if ($null -ne $improvement -and [double]$improvement -ge 10.0 -and
            $enabledFasterConsistency -and $objectiveNoRegression) {
            $verdict = "helpful"
        } elseif (($null -ne $improvement -and [double]$improvement -le -10.0 -and
                $enabledSlowerConsistency) -or
            ($null -ne $taskbarRegression -and [double]$taskbarRegression -gt 10.0) -or
            ($null -ne $schedulerRegression -and [double]$schedulerRegression -gt 10.0)) {
            $verdict = "harmful"
        } else {
            $verdict = "no_clear_effect"
        }
    }
    return [ordered]@{
        verdict = $verdict
        passive_timing_policy_eligible = $passiveTimingPolicyEligible
        enabled = [ordered]@{
            passive_timing_ms = $enabledTiming
            median_phase_taskbar_p95_us = $enabledTaskbarP95
            median_phase_scheduler_p99_us = $enabledSchedulerP99
        }
        disabled = [ordered]@{
            passive_timing_ms = $disabledTiming
            median_phase_taskbar_p95_us = $disabledTaskbarP95
            median_phase_scheduler_p99_us = $disabledSchedulerP99
        }
        passive_click_to_responsive_p95_improvement_percent = $improvement
        taskbar_p95_regression_percent = $taskbarRegression
        scheduler_p99_regression_percent = $schedulerRegression
        pair_consistency = $pairConsistency
        pair_enabled_faster_consistency = $enabledFasterConsistency
        pair_enabled_slower_consistency = $enabledSlowerConsistency
        objective_no_regression_over_10_percent = $objectiveNoRegression
        pairs = $pairResults
    }
}

function Invoke-SelfTest {
    $current = @"
schema_version = 5
controller_mode = "auto"
all_user_processes = true
rules = []
[logging]
level = "trace"
max_file_size_mib = 10
retained_archives = 1
"@
    $legacy = @"
schema_version = 4
controller_mode = "auto"
rules = []
[logging]
enabled = true
max_file_size_mib = 10
retained_archives = 1
"@
    $currentResult = New-TestConfigurationText `
        $current "winsched-abba-observer-test.exe" "winsched-abba-marker-test.exe" "off"
    $legacyResult = New-TestConfigurationText `
        $legacy "winsched-abba-observer-test.exe" "winsched-abba-marker-test.exe" "off"
    Assert-True ($currentResult -match '(?m)^level = "off"$') "ABBA_SELF_TEST_CURRENT_LOGGING"
    Assert-True ($legacyResult -match '(?m)^enabled = false$') "ABBA_SELF_TEST_LEGACY_LOGGING"
    Assert-True ([regex]::Matches($currentResult, '(?m)^mode = "off"$').Count -eq 2) `
        "ABBA_SELF_TEST_RULES"
    Assert-True (-not [regex]::IsMatch($currentResult, '(?m)^\s*rules\s*=')) `
        "ABBA_SELF_TEST_EMPTY_INLINE_RULE_REMOVAL"
    $orphan = [regex]::Replace($current, '(?m)^\s*rules\s*=\s*\[\s*\]\s*$', '') + @"

[[rules]]
image = "winsched-abba-observer-deadbeef.exe"
mode = "off"
profile = "balanced"
"@
    $orphanRejected = $false
    try {
        [void](New-TestConfigurationText `
            $orphan "winsched-abba-observer-test.exe" "winsched-abba-marker-test.exe" "off")
    } catch {
        $orphanRejected = [string]$_.Exception.Message -eq "ABBA_ORPHAN_TEMP_RULES_DETECTED"
    }
    Assert-True $orphanRejected "ABBA_SELF_TEST_ORPHAN_TEMP_RULES"
    Assert-True ((Get-Percentile @(1, 2, 3, 4) 50) -eq 2) "ABBA_SELF_TEST_PERCENTILE"
    $syntheticPhases = @(
        [pscustomobject]@{
            scheduling_enabled = $true
            measurement_complete = $true
            marker = [pscustomobject]@{
                schema_version = 3
                capture_mode = "passive_taskbar_restore"
                input_generated = $false
                per_monitor_dpi_aware = $true
                dedicated_probe_thread = $true
                minimize_state_required = $true
                click_to_foreground_ms = @(1..10 | ForEach-Object { 20.0 })
                foreground_to_responsive_ms = @(1..10 | ForEach-Object { 10.0 })
                click_to_responsive_ms = @(1..10 | ForEach-Object { 30.0 })
                timing = [pscustomobject]@{ p95 = 30.0 }
            }
            diagnostic = [pscustomobject]@{
                taskbar = [pscustomobject]@{ p95_us = 100.0 }
                system = [pscustomobject]@{
                    scheduler_latency = [pscustomobject]@{ p99_us = 100.0 }
                }
            }
        },
        [pscustomobject]@{
            scheduling_enabled = $false
            measurement_complete = $true
            marker = [pscustomobject]@{
                schema_version = 3
                capture_mode = "passive_taskbar_restore"
                input_generated = $false
                per_monitor_dpi_aware = $true
                dedicated_probe_thread = $true
                minimize_state_required = $true
                click_to_foreground_ms = @(1..10 | ForEach-Object { 10.0 })
                foreground_to_responsive_ms = @(1..10 | ForEach-Object { 10.0 })
                click_to_responsive_ms = @(1..10 | ForEach-Object { 20.0 })
                timing = [pscustomobject]@{ p95 = 20.0 }
            }
            diagnostic = [pscustomobject]@{
                taskbar = [pscustomobject]@{ p95_us = 100.0 }
                system = [pscustomobject]@{
                    scheduler_latency = [pscustomobject]@{ p99_us = 100.0 }
                }
            }
        },
        [pscustomobject]@{
            scheduling_enabled = $false
            measurement_complete = $true
            marker = [pscustomobject]@{
                schema_version = 3
                capture_mode = "passive_taskbar_restore"
                input_generated = $false
                per_monitor_dpi_aware = $true
                dedicated_probe_thread = $true
                minimize_state_required = $true
                click_to_foreground_ms = @(1..10 | ForEach-Object { 30.0 })
                foreground_to_responsive_ms = @(1..10 | ForEach-Object { 10.0 })
                click_to_responsive_ms = @(1..10 | ForEach-Object { 40.0 })
                timing = [pscustomobject]@{ p95 = 40.0 }
            }
            diagnostic = [pscustomobject]@{
                taskbar = [pscustomobject]@{ p95_us = 100.0 }
                system = [pscustomobject]@{
                    scheduler_latency = [pscustomobject]@{ p99_us = 100.0 }
                }
            }
        },
        [pscustomobject]@{
            scheduling_enabled = $true
            measurement_complete = $true
            marker = [pscustomobject]@{
                schema_version = 3
                capture_mode = "passive_taskbar_restore"
                input_generated = $false
                per_monitor_dpi_aware = $true
                dedicated_probe_thread = $true
                minimize_state_required = $true
                click_to_foreground_ms = @(1..10 | ForEach-Object { 5.0 })
                foreground_to_responsive_ms = @(1..10 | ForEach-Object { 5.0 })
                click_to_responsive_ms = @(1..10 | ForEach-Object { 10.0 })
                timing = [pscustomobject]@{ p95 = 10.0 }
            }
            diagnostic = [pscustomobject]@{
                taskbar = [pscustomobject]@{ p95_us = 100.0 }
                system = [pscustomobject]@{
                    scheduler_latency = [pscustomobject]@{ p99_us = 100.0 }
                }
            }
        }
    )
    $inconsistent = Get-Comparison $syntheticPhases
    Assert-True ([string]$inconsistent.verdict -eq "no_clear_effect") `
        "ABBA_SELF_TEST_INCONSISTENT_PAIR_VERDICT"
    Assert-True ([bool]$inconsistent.passive_timing_policy_eligible) `
        "ABBA_SELF_TEST_PASSIVE_POLICY_ELIGIBILITY"
    Assert-True (-not [bool]$inconsistent.pair_consistency) `
        "ABBA_SELF_TEST_INCONSISTENT_PAIR_FLAG"
    $syntheticPhases[0].marker.capture_mode = "manual_hotkeys"
    $legacyManual = Get-Comparison $syntheticPhases
    Assert-True ([string]$legacyManual.verdict -eq "invalid") `
        "ABBA_SELF_TEST_MANUAL_POLICY_INVALID"
    Assert-True (-not [bool]$legacyManual.passive_timing_policy_eligible) `
        "ABBA_SELF_TEST_MANUAL_POLICY_ELIGIBILITY"
    $passiveMarkerCompilation = "skipped_non_windows"
    $passiveHookEarlyExit = "skipped_non_windows"
    $passiveStateMachine = "skipped_non_windows"
    if ($env:OS -eq "Windows_NT") {
        $markerTestPath = Join-Path $env:TEMP (
            "winsched-abba-marker-selftest-{0}.exe" -f [Guid]::NewGuid().ToString("N")
        )
        try {
            Add-Type `
                -TypeDefinition $markerSource `
                -Language CSharp `
                -OutputAssembly $markerTestPath `
                -OutputType ConsoleApplication
            Assert-True (Test-Path -LiteralPath $markerTestPath -PathType Leaf) `
                "ABBA_SELF_TEST_MARKER_COMPILATION"
            $passiveMarkerCompilation = "PASS"
            $captureDirectory = Join-Path $env:TEMP (
                "winsched-abba-marker-capture-selftest-{0}" -f [Guid]::NewGuid().ToString("N")
            )
            New-Item -ItemType Directory -Path $captureDirectory -Force | Out-Null
            try {
                $stateSelfTest = Start-Process `
                    -FilePath $markerTestPath `
                    -WorkingDirectory $captureDirectory `
                    -ArgumentList @("state-selftest", "state-selftest.json") `
                    -Wait `
                    -PassThru
                Assert-True ($stateSelfTest.ExitCode -eq 0) `
                    "ABBA_SELF_TEST_STATE_MACHINE_EXIT_CODE"
                $stateResult = Get-Content `
                    -LiteralPath (Join-Path $captureDirectory "state-selftest.json") `
                    -Raw |
                    ConvertFrom-Json
                Assert-True ([string]$stateResult.status -eq "pass") `
                    "ABBA_SELF_TEST_STATE_MACHINE_RESULT"
                Assert-True ([bool]$stateResult.stale_generation_rejected) `
                    "ABBA_SELF_TEST_STALE_GENERATION"
                Assert-True ([bool]$stateResult.unconfirmed_restore_rejected) `
                    "ABBA_SELF_TEST_UNCONFIRMED_RESTORE"
                Assert-True ([bool]$stateResult.confirmed_restore_accepted) `
                    "ABBA_SELF_TEST_CONFIRMED_RESTORE"
                Assert-True ([double]$stateResult.uint_wrap_elapsed_ms -eq 32.0) `
                    "ABBA_SELF_TEST_UINT_WRAP"
                $passiveStateMachine = "PASS"
                Write-Utf8NoBom (Join-Path $captureDirectory "start") "start"
                $capture = Start-Process `
                    -FilePath $markerTestPath `
                    -WorkingDirectory $captureDirectory `
                    -ArgumentList @("capture", "result.json", "ready", "start", 5, 0) `
                    -Wait `
                    -PassThru
                Assert-True ($capture.ExitCode -eq 0) "ABBA_SELF_TEST_MARKER_EARLY_EXIT_CODE"
                $captureResult = Get-Content `
                    -LiteralPath (Join-Path $captureDirectory "result.json") `
                    -Raw |
                    ConvertFrom-Json
                Assert-True ([string]$captureResult.status -eq "complete") `
                    "ABBA_SELF_TEST_MARKER_EARLY_EXIT_RESULT"
                Assert-True ([string]$captureResult.capture_mode -eq "passive_taskbar_restore") `
                    "ABBA_SELF_TEST_PASSIVE_CAPTURE_MODE"
                Assert-True (-not [bool]$captureResult.input_generated) `
                    "ABBA_SELF_TEST_GENERATED_INPUT"
                Assert-True ([bool]$captureResult.per_monitor_dpi_aware) `
                    "ABBA_SELF_TEST_PER_MONITOR_DPI_AWARE"
                Assert-True ([int]$captureResult.schema_version -eq 3) `
                    "ABBA_SELF_TEST_PASSIVE_SCHEMA"
                Assert-True ([bool]$captureResult.dedicated_probe_thread) `
                    "ABBA_SELF_TEST_DEDICATED_PROBE_THREAD"
                Assert-True ([bool]$captureResult.minimize_state_required) `
                    "ABBA_SELF_TEST_MINIMIZE_STATE_REQUIRED"
                Assert-True ([string]$captureResult.click_timestamp_source -eq `
                    "MSLLHOOKSTRUCT.time") "ABBA_SELF_TEST_CLICK_TIMESTAMP_SOURCE"
                Assert-True ([string]$captureResult.foreground_timestamp_source -eq `
                    "EVENT_SYSTEM_FOREGROUND.time") `
                    "ABBA_SELF_TEST_FOREGROUND_TIMESTAMP_SOURCE"
                $passiveHookEarlyExit = "PASS"
            } finally {
                Remove-Item -LiteralPath $captureDirectory -Recurse -Force -ErrorAction SilentlyContinue
            }
        } finally {
            Remove-Item -LiteralPath $markerTestPath -Force -ErrorAction SilentlyContinue
        }
    }
    [pscustomobject]@{
        result = "PASS"
        configuration_variants = 2
        temporary_rules = 2
        orphan_temp_rules = "PASS"
        percentile = "PASS"
        comparison_logic = "PASS"
        passive_marker_compilation = $passiveMarkerCompilation
        passive_state_machine = $passiveStateMachine
        passive_hook_early_exit = $passiveHookEarlyExit
        windows_state_changed = $false
    } | ConvertTo-Json -Depth 4
}

function Invoke-ObserverSelfTest {
    Assert-True ($env:OS -eq "Windows_NT") "ABBA_OBSERVER_SELF_TEST_WINDOWS_REQUIRED"
    $cli = Join-Path $InstallDirectory "winsched.exe"
    Assert-True (Test-Path -LiteralPath $cli -PathType Leaf) "ABBA_OBSERVER_SELF_TEST_CLI_MISSING"
    $directory = Join-Path $env:TEMP (
        "WinSchedAbbaObserverSelfTest-{0}" -f [Guid]::NewGuid().ToString("N")
    )
    New-Item -ItemType Directory -Path $directory -Force | Out-Null
    $diagnoseOutput = Join-Path $directory "diagnose.json"
    $diagnoseError = Join-Path $directory "diagnose.stderr"
    $observeOutput = Join-Path $directory "observe.jsonl"
    $observeError = Join-Path $directory "observe.stderr"
    $diagnose = $null
    $observe = $null
    try {
        $diagnose = Start-Process `
            -FilePath $cli `
            -ArgumentList @(
                "diagnose", "--duration-seconds", 5, "--interval-ms", 250,
                "--taskbar-timeout-ms", 50, "--json"
            ) `
            -RedirectStandardOutput $diagnoseOutput `
            -RedirectStandardError $diagnoseError `
            -PassThru
        $observe = Start-Process `
            -FilePath $cli `
            -ArgumentList @("observe", "--samples", 5, "--interval-ms", 1000, "--json") `
            -RedirectStandardOutput $observeOutput `
            -RedirectStandardError $observeError `
            -PassThru
        Assert-True ($diagnose.WaitForExit(15000)) "ABBA_OBSERVER_SELF_TEST_DIAGNOSE_TIMEOUT"
        Assert-True ($observe.WaitForExit(15000)) "ABBA_OBSERVER_SELF_TEST_OBSERVE_TIMEOUT"
        $diagnoseExitCode = [int]$diagnose.ExitCode
        $observeExitCode = [int]$observe.ExitCode
        $diagnosticRaw = Get-Content -LiteralPath $diagnoseOutput -Raw | ConvertFrom-Json
        $diagnostic = Convert-DiagnosticReport $diagnosticRaw
        $llc = Convert-ObserveJsonLines $observeOutput
        Assert-True ([int]$diagnostic.schema_version -eq 1) "ABBA_OBSERVER_SELF_TEST_SCHEMA"
        Assert-True ([int]$diagnostic.sample_count -ge 15) "ABBA_OBSERVER_SELF_TEST_DIAGNOSE_SAMPLES"
        Assert-True ([uint64]$diagnostic.taskbar.samples -eq [uint64]$diagnostic.sample_count) `
            "ABBA_OBSERVER_SELF_TEST_TASKBAR_SAMPLES"
        Assert-True ([int]$llc.samples -eq 5) "ABBA_OBSERVER_SELF_TEST_OBSERVE_SAMPLES"
        Assert-True (@($llc.domains).Count -eq 8) "ABBA_OBSERVER_SELF_TEST_LLC_COUNT"
        [pscustomobject]@{
            result = "PASS"
            diagnose_samples = [int]$diagnostic.sample_count
            taskbar_samples = [uint64]$diagnostic.taskbar.samples
            observe_samples = [int]$llc.samples
            llc_domains = @($llc.domains).Count
            diagnose_exit_code = $diagnoseExitCode
            observe_exit_code = $observeExitCode
            nonzero_exit_tolerated_after_content_validation = `
                ($diagnoseExitCode -ne 0 -or $observeExitCode -ne 0)
            windows_state_changed = $false
        } | ConvertTo-Json -Depth 4
    } finally {
        Stop-OwnedProcess $diagnose
        Stop-OwnedProcess $observe
        Remove-Item -LiteralPath $directory -Recurse -Force -ErrorAction SilentlyContinue
    }
}

function Invoke-PassivePilot {
    Assert-True ($env:OS -eq "Windows_NT") "ABBA_PASSIVE_PILOT_WINDOWS_REQUIRED"
    Assert-True ([Diagnostics.Process]::GetCurrentProcess().SessionId -gt 0) `
        "ABBA_PASSIVE_PILOT_INTERACTIVE_SESSION_REQUIRED"
    $directory = Join-Path $env:TEMP (
        "WinSchedPassivePilot-{0}" -f [Guid]::NewGuid().ToString("N")
    )
    $marker = Join-Path $directory "winsched-passive-pilot.exe"
    $resultPath = Join-Path $directory "result.json"
    $readyPath = Join-Path $directory "ready"
    $startPath = Join-Path $directory "start"
    $result = $null
    New-Item -ItemType Directory -Path $directory -Force | Out-Null
    try {
        Add-Type `
            -TypeDefinition $markerSource `
            -Language CSharp `
            -OutputAssembly $marker `
            -OutputType ConsoleApplication
        Write-Host ""
        Write-Host "PASSIVE FIREFOX TASKBAR PILOT" -ForegroundColor Cyan
        Write-Host "No hotkeys are used and the helper does not generate input."
        Write-Host "After CAPTURE IS ACTIVE appears:"
        Write-Host "  1. If asked, click Firefox once to arm the pilot."
        Write-Host "  2. Click Firefox once to minimize it."
        Write-Host "  3. Wait about one second, then click Firefox once to restore it."
        Write-Host "The pilot ends automatically after that one valid restore."
        [void](Read-Host "Press ENTER when you are ready")
        Write-Utf8NoBom $startPath "start"
        Write-Host "CAPTURE IS ACTIVE" -ForegroundColor Green
        $capture = Start-Process `
            -FilePath $marker `
            -WorkingDirectory $directory `
            -ArgumentList @("capture", "result.json", "ready", "start", 90, 1) `
            -NoNewWindow `
            -Wait `
            -PassThru
        Assert-True (Test-Path -LiteralPath $resultPath -PathType Leaf) `
            "ABBA_PASSIVE_PILOT_RESULT_MISSING"
        $result = Get-Content -LiteralPath $resultPath -Raw | ConvertFrom-Json
        Assert-True ($capture.ExitCode -eq 0 -and [string]$result.status -eq "complete") `
            "ABBA_PASSIVE_PILOT_INCOMPLETE"
        $pilotResult = [ordered]@{
            result = "PASS"
            schema_version = [int]$result.schema_version
            capture_mode = [string]$result.capture_mode
            valid_attempts = [int]$result.valid_attempts
            physical_left_clicks_observed = [int]$result.physical_left_clicks_observed
            taskbar_clicks_observed = [int]$result.taskbar_clicks_observed
            ignored_minimize_clicks = [int]$result.ignored_minimize_clicks
            priming_activations = [int]$result.priming_activations
            click_to_foreground_ms = [double]$result.click_to_foreground_ms[0]
            foreground_to_responsive_ms = [double]$result.foreground_to_responsive_ms[0]
            click_to_responsive_ms = [double]$result.click_to_responsive_ms[0]
            input_generated = [bool]$result.input_generated
            per_monitor_dpi_aware = [bool]$result.per_monitor_dpi_aware
            dedicated_probe_thread = [bool]$result.dedicated_probe_thread
            minimize_state_required = [bool]$result.minimize_state_required
            windows_state_changed = $false
        }
        $pilotDirectory = Split-Path -Parent $PilotResultPath
        if (-not [string]::IsNullOrWhiteSpace($pilotDirectory)) {
            New-Item -ItemType Directory -Path $pilotDirectory -Force | Out-Null
        }
        $pilotJson = [pscustomobject]$pilotResult | ConvertTo-Json -Depth 4
        Write-Utf8NoBom $PilotResultPath ($pilotJson + "`n")
        $pilotJson
    } catch {
        $errorCode = if ([string]$_.Exception.Message -match '^ABBA_[A-Z0-9_]+$') {
            [string]$_.Exception.Message
        } else {
            "ABBA_PASSIVE_PILOT_UNEXPECTED"
        }
        if ($null -eq $result -and (Test-Path -LiteralPath $resultPath -PathType Leaf)) {
            try {
                $result = Get-Content -LiteralPath $resultPath -Raw | ConvertFrom-Json
            } catch {
                $result = $null
            }
        }
        $pilotFailure = [ordered]@{
            result = "FAIL"
            error_code = $errorCode
            marker = if ($null -ne $result) {
                [ordered]@{
                    status = [string]$result.status
                    physical_left_clicks_observed = [int]$result.physical_left_clicks_observed
                    taskbar_clicks_observed = [int]$result.taskbar_clicks_observed
                    priming_activations = [int]$result.priming_activations
                    possible_minimize_clicks = [int]$result.possible_minimize_clicks
                    confirmed_minimize_clicks = [int]$result.ignored_minimize_clicks
                    restore_candidates_observed = [int]$result.restore_candidates_observed
                    valid_attempts = [int]$result.valid_attempts
                    candidate_timeouts = [int]$result.candidate_timeouts
                    per_monitor_dpi_aware = [bool]$result.per_monitor_dpi_aware
                }
            } else { $null }
            windows_state_changed = $false
        }
        $pilotDirectory = Split-Path -Parent $PilotResultPath
        if (-not [string]::IsNullOrWhiteSpace($pilotDirectory)) {
            New-Item -ItemType Directory -Path $pilotDirectory -Force | Out-Null
        }
        $failureJson = [pscustomobject]$pilotFailure | ConvertTo-Json -Depth 6
        Write-Utf8NoBom $PilotResultPath ($failureJson + "`n")
        $failureJson
    } finally {
        Remove-Item -LiteralPath $directory -Recurse -Force -ErrorAction SilentlyContinue
    }
}

if ($SelfTest) {
    Invoke-SelfTest
    return
}
if ($ObserverSelfTest) {
    Invoke-ObserverSelfTest
    return
}
if ($PassivePilot) {
    Invoke-PassivePilot
    return
}

$script:runMutex = [Threading.Mutex]::new($false, "Global\WinSched.HostAbba")
$script:runMutexOwned = $false
try {
    $script:runMutexOwned = $script:runMutex.WaitOne(0)
} catch [Threading.AbandonedMutexException] {
    $script:runMutexOwned = $true
}
if (-not $script:runMutexOwned) {
    $script:runMutex.Dispose()
    Throw-Abba "ABBA_ANOTHER_RUN_ACTIVE"
}

$script:serviceBinary = Join-Path $InstallDirectory "winsched-service.exe"
$installedCli = Join-Path $InstallDirectory "winsched.exe"
$script:configPath = Join-Path $DataDirectory "winsched.toml"
$script:statusPath = Join-Path $DataDirectory "status.json"
$runId = [Guid]::NewGuid().ToString("N").Substring(0, 12)
$script:workDirectory = Join-Path $env:TEMP ("WinSchedHostAbba-{0}" -f $runId)
$observerImage = "winsched-abba-observer-{0}.exe" -f $runId
$markerImage = "winsched-abba-marker-{0}.exe" -f $runId
$script:observerBinary = Join-Path $script:workDirectory $observerImage
$script:markerBinary = Join-Path $script:workDirectory $markerImage
$originalConfigBackupPath = Join-Path $script:workDirectory "winsched-original.toml"
$originalConfigBytes = $null
$originalConfigHash = $null
$testConfigHash = $null
$testConfigApplied = $false
$initialSchedulingEnabled = $null
$initialLoggingMode = $null
$initialServicePid = 0
$initialServiceRunning = $false
$trayPid = 0
$trayStartTicks = 0
$phases = @()
$mainErrorCode = $null
$mainErrorDetail = $null
$cleanupErrors = New-Object System.Collections.ArrayList
$topologySummary = $null
$toolVersion = $null
$guardSummary = $null
$restoreSummary = [ordered]@{
    config_bytes_restored = $false
    scheduling_state_restored = $false
    service_process_unchanged = $false
    logging_state_restored = $false
    temporary_files_removed = $false
    recovery_backup_created = $false
    recovery_backup_retained = $false
    external_config_preserved = $false
}

try {
    Assert-True ($env:OS -eq "Windows_NT") "ABBA_WINDOWS_REQUIRED"
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($identity)
    Assert-True ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) `
        "ABBA_ELEVATED_SHELL_REQUIRED"
    Assert-True ([Diagnostics.Process]::GetCurrentProcess().SessionId -gt 0) `
        "ABBA_INTERACTIVE_SESSION_REQUIRED"
    Assert-True (Test-Path -LiteralPath $installedCli -PathType Leaf) "ABBA_CLI_MISSING"
    Assert-True (Test-Path -LiteralPath $script:serviceBinary -PathType Leaf) `
        "ABBA_SERVICE_BINARY_MISSING"
    Assert-True (Test-Path -LiteralPath $script:configPath -PathType Leaf) `
        "ABBA_CONFIG_MISSING"
    Assert-True (Test-Path -LiteralPath $script:statusPath -PathType Leaf) `
        "ABBA_STATUS_UNAVAILABLE"
    $service = Get-Service -Name "WinSched" -ErrorAction SilentlyContinue
    Assert-True ($null -ne $service -and $service.Status -eq "Running") `
        "ABBA_SERVICE_MUST_BE_RUNNING"
    $initialServiceRunning = $true

    New-Item -ItemType Directory -Path $script:workDirectory -Force | Out-Null
    Copy-Item -LiteralPath $installedCli -Destination $script:observerBinary -Force
    Add-Type `
        -TypeDefinition $markerSource `
        -Language CSharp `
        -OutputAssembly $script:markerBinary `
        -OutputType ConsoleApplication

    $guardPath = Join-Path $script:workDirectory "guard.json"
    $guardProcess = Start-Process `
        -FilePath $script:markerBinary `
        -WorkingDirectory $script:workDirectory `
        -ArgumentList @("guard", (Split-Path -Leaf $guardPath)) `
        -PassThru `
        -Wait
    Assert-True (Test-Path -LiteralPath $guardPath -PathType Leaf) "ABBA_GUARD_RESULT_MISSING"
    $guard = Get-Content -LiteralPath $guardPath -Raw | ConvertFrom-Json
    $guardSummary = [ordered]@{
        notification_state_available = [bool]$guard.notification_state_available
        fullscreen_or_presentation = [bool]$guard.fullscreen_or_presentation
        remote_session = [bool]$guard.remote_session
    }
    Assert-True ($guardProcess.ExitCode -eq 0 -and [string]$guard.status -eq "pass") `
        "ABBA_FULLSCREEN_PRESENTATION_OR_REMOTE_REJECTED"

    $initialStatus = Read-Status
    Assert-True ($null -ne $initialStatus) "ABBA_STATUS_UNAVAILABLE"
    Assert-True ([string]$initialStatus.configured_mode -eq "auto") `
        "ABBA_AUTO_MODE_REQUIRED"
    Assert-True ($null -eq $initialStatus.last_error) "ABBA_INITIAL_SERVICE_ERROR"
    $initialServicePid = [int]$initialStatus.service_pid
    $initialSchedulingEnabled = [bool]$initialStatus.scheduling_enabled
    $initialLoggingMode = Get-LoggingMode $initialStatus
    Assert-True ($null -ne $initialLoggingMode) "ABBA_LOGGING_STATUS_UNAVAILABLE"
    $serviceCim = Get-CimInstance Win32_Service -Filter "Name='WinSched'"
    Assert-True ($null -ne $serviceCim -and [int]$serviceCim.ProcessId -eq $initialServicePid) `
        "ABBA_SERVICE_PID_MISMATCH"

    $currentSession = [Diagnostics.Process]::GetCurrentProcess().SessionId
    $trays = @(Get-Process -Name "winsched-tray" -ErrorAction SilentlyContinue |
        Where-Object { $_.SessionId -eq $currentSession })
    Assert-True ($trays.Count -eq 1) "ABBA_ONE_INTERACTIVE_TRAY_REQUIRED"
    $trayPid = [int]$trays[0].Id
    $trayStartTicks = $trays[0].StartTime.ToUniversalTime().Ticks

    $topology = & $script:observerBinary topology --json | ConvertFrom-Json
    Assert-True ($LASTEXITCODE -eq 0) "ABBA_TOPOLOGY_FAILED"
    $physicalCores = @($topology.cpu_sets | ForEach-Object {
        "{0}:{1}" -f $_.group, $_.core_index
    } | Sort-Object -Unique)
    Assert-True ($physicalCores.Count -eq 32) "ABBA_TARGET_PHYSICAL_CORE_COUNT"
    Assert-True (@($topology.cpu_sets).Count -eq 64) "ABBA_TARGET_LOGICAL_PROCESSOR_COUNT"
    Assert-True (@($topology.llc_domains).Count -eq 8) "ABBA_TARGET_LLC_COUNT"
    $topologySummary = [ordered]@{
        physical_cores = $physicalCores.Count
        logical_processors = @($topology.cpu_sets).Count
        llc_domains = @($topology.llc_domains).Count
    }
    $toolVersion = (& $script:observerBinary --version | Select-Object -First 1).Trim()

    $originalConfigBytes = [IO.File]::ReadAllBytes($script:configPath)
    $originalConfigHash = Get-Sha256 $script:configPath
    [IO.File]::WriteAllBytes($originalConfigBackupPath, $originalConfigBytes)
    Assert-True ((Get-Sha256 $originalConfigBackupPath) -eq $originalConfigHash) `
        "ABBA_ORIGINAL_CONFIG_BACKUP_FAILED"
    $restoreSummary.recovery_backup_created = $true
    $decoder = New-Object System.Text.UTF8Encoding($false, $true)
    $originalConfigText = $decoder.GetString($originalConfigBytes)
    $testConfigText = New-TestConfigurationText `
        $originalConfigText $observerImage $markerImage $LoggingLevelDuringTest
    $testConfigPath = Join-Path $script:workDirectory "winsched-abba.toml"
    Write-Utf8NoBom $testConfigPath $testConfigText
    & $script:observerBinary config-check $testConfigPath | Out-Null
    Assert-True ($LASTEXITCODE -eq 0) "ABBA_TEMP_CONFIG_INVALID"

    $beforeReload = Read-Status
    $beforeReloadSequence = [uint64]$beforeReload.config_reload_sequence
    Set-Utf8FileAtomically $script:configPath $testConfigText $runId
    $testConfigApplied = $true
    $testConfigHash = Get-Sha256 $script:configPath
    [void](Wait-ConfigReceipt `
        $beforeReloadSequence $LoggingLevelDuringTest $initialServicePid `
        "ABBA_TEMP_CONFIG_RELOAD_TIMEOUT")

    $opposite = -not $initialSchedulingEnabled
    $phaseStates = @($initialSchedulingEnabled, $opposite, $opposite, $initialSchedulingEnabled)
    $phaseLabels = @("A1", "B1", "B2", "A2")
    Write-Host ""
    Write-Host "WinSched physical-host A-B-B-A passive Firefox taskbar test"
    Write-Host "The harness only observes physical taskbar clicks and Firefox foreground/responsiveness events."
    Write-Host "It never clicks, changes focus, minimizes windows, or generates input."
    Write-Host ("There are four phases and {0} valid attempts per phase." -f $AttemptsPerPhase)
    for ($index = 0; $index -lt 4; $index++) {
        $phases += Invoke-Phase `
            ($index + 1) `
            $phaseLabels[$index] `
            $phaseStates[$index] `
            $initialServicePid `
            $trayPid `
            $trayStartTicks `
            $topologySummary.logical_processors
    }
} catch {
    $message = [string]$_.Exception.Message
    if ($message -match '^ABBA_[A-Z0-9_]+$') {
        $mainErrorCode = $message
    } else {
        $mainErrorCode = "ABBA_UNEXPECTED_FAILURE"
        $mainErrorDetail = Get-SafeUnexpectedError $_
        Write-Host "Unexpected harness error:" -ForegroundColor Red
        Write-Host ([string]$_.Exception.ToString())
    }
    if (Test-Path -LiteralPath $script:workDirectory -PathType Container) {
        foreach ($checkpointFile in @(Get-ChildItem `
            -LiteralPath $script:workDirectory `
            -Filter "phase-*-passive-checkpoint.json" `
            -File `
            -ErrorAction SilentlyContinue | Sort-Object Name)) {
            try {
                $checkpoint = Get-Content -LiteralPath $checkpointFile.FullName -Raw |
                    ConvertFrom-Json
                if (@($phases | Where-Object { $_.label -eq $checkpoint.label }).Count -eq 0) {
                    $phases += $checkpoint
                    Write-Host ("Recovered passive checkpoint for phase {0}: {1} valid attempt(s)." -f `
                        $checkpoint.label,
                        [int]$checkpoint.marker.valid_attempts)
                }
            } catch {
            }
        }
    }
} finally {
    if ($testConfigApplied -and $null -ne $originalConfigBytes) {
        try {
            $currentHash = Get-Sha256 $script:configPath
            if ($null -ne $testConfigHash -and $currentHash -ne $testConfigHash) {
                [void]$cleanupErrors.Add("ABBA_EXTERNAL_CONFIG_CHANGE_DETECTED")
                $conflictPath = Join-Path $script:workDirectory "external-config-conflict.toml"
                [IO.File]::WriteAllBytes(
                    $conflictPath,
                    [IO.File]::ReadAllBytes($script:configPath)
                )
                $restoreSummary.external_config_preserved = $true
            } else {
                $statusBeforeRestore = Read-Status
                $restoreSequence = if ($null -ne $statusBeforeRestore) {
                    [uint64]$statusBeforeRestore.config_reload_sequence
                } else { 0 }
                Set-FileAtomically $script:configPath $originalConfigBytes $runId
                $restoreSummary.config_bytes_restored =
                    (Get-Sha256 $script:configPath) -eq $originalConfigHash
                if ($initialServicePid -gt 0) {
                    [void](Wait-ConfigReceipt `
                        $restoreSequence $initialLoggingMode $initialServicePid `
                        "ABBA_ORIGINAL_CONFIG_RELOAD_TIMEOUT")
                    $restoreSummary.logging_state_restored = $true
                }
            }
        } catch {
            [void]$cleanupErrors.Add("ABBA_CONFIG_RESTORE_FAILED")
        }
    }
    if ($initialServiceRunning -and $initialServicePid -gt 0 -and
        $null -ne $initialSchedulingEnabled) {
        try {
            $service = Get-Service -Name "WinSched" -ErrorAction SilentlyContinue
            Assert-True ($null -ne $service -and $service.Status -eq "Running") `
                "ABBA_SERVICE_NOT_RUNNING_DURING_RESTORE"
            $status = Read-Status
            Assert-True ($null -ne $status -and [int]$status.service_pid -eq $initialServicePid) `
                "ABBA_SERVICE_RESTARTED"
            [void](Set-SchedulingState $initialSchedulingEnabled $initialServicePid)
            $restoreSummary.scheduling_state_restored = $true
            $restoreSummary.service_process_unchanged = $true
        } catch {
            [void]$cleanupErrors.Add("ABBA_SCHEDULING_RESTORE_FAILED")
        }
    }
}

$comparison = if ($phases.Count -eq 4) { Get-Comparison $phases } else {
    [ordered]@{ verdict = "invalid" }
}
$cleanupSucceeded = $cleanupErrors.Count -eq 0 -and
    $restoreSummary.config_bytes_restored -and
    $restoreSummary.scheduling_state_restored -and
    $restoreSummary.service_process_unchanged -and
    $restoreSummary.logging_state_restored
if ($cleanupSucceeded -and
    $mainErrorCode -ne "ABBA_UNEXPECTED_FAILURE" -and
    (Test-Path -LiteralPath $script:workDirectory -PathType Container)) {
    try {
        Remove-Item -LiteralPath $script:workDirectory -Recurse -Force
        $restoreSummary.temporary_files_removed = $true
    } catch {
        [void]$cleanupErrors.Add("ABBA_TEMP_CLEANUP_FAILED")
    }
}
$restoreSummary.recovery_backup_retained =
    Test-Path -LiteralPath $originalConfigBackupPath -PathType Leaf

$protocolComplete = $null -eq $mainErrorCode -and $phases.Count -eq 4 -and
    @($phases | Where-Object {
        -not [bool]$_.measurement_complete -or -not [bool]$_.auxiliary_data_complete
    }).Count -eq 0
$result = [ordered]@{
    schema_version = 1
    result = if ($protocolComplete -and $cleanupErrors.Count -eq 0) { "PASS" } else { "FAIL" }
    error_code = $mainErrorCode
    error_detail = $mainErrorDetail
    cleanup_error_codes = @($cleanupErrors)
    product = [ordered]@{
        cli_version = $toolVersion
        status_schema = if ($null -ne (Read-Status)) { [int](Read-Status).schema_version } else { $null }
    }
    environment = [ordered]@{
        topology = $topologySummary
        interactive_local_session = if ($null -ne $guardSummary) {
            -not [bool]$guardSummary.remote_session
        } else { $null }
    }
    protocol = [ordered]@{
        design = "ABBA"
        a_is_initial_scheduling_state = $true
        scenario = $Scenario
        settle_seconds = $SettleSeconds
        measurement_seconds = $MeasurementSeconds
        attempts_per_phase = $AttemptsPerPhase
        taskbar_timeout_ms = $TaskbarTimeoutMs
        logging_during_test = $LoggingLevelDuringTest
        measurement = "physical taskbar click to Firefox foreground and WM_NULL response"
        minimize_click_handling = "ignored when Firefox is foreground"
        restore_click_handling = "accepted after Firefox becomes foreground, restored, and responsive"
        passive_low_level_mouse_hook = $true
        passive_foreground_event_hook = $true
        per_monitor_dpi_aware_v2 = $true
        dedicated_response_probe_thread = $true
        click_timestamp_source = "MSLLHOOKSTRUCT.time"
        foreground_timestamp_source = "EVENT_SYSTEM_FOREGROUND.time"
        minimize_confirmed_with_is_iconic = $true
        human_reaction_time_in_endpoint = $false
        generated_input = $false
        focus_changes = $false
        automated_clicks = $false
        automated_minimize = $false
        notifications_or_sound = $false
        screenshots = $false
        wpr_or_etw_capture = $false
    }
    guard = $guardSummary
    phases = $phases
    comparison = $comparison
    restore = $restoreSummary
    limitations = @(
        "The marker requests Per-Monitor DPI Aware V2 before installing hooks so mouse points and taskbar rectangles share physical screen coordinates.",
        "The physical click timestamp comes from MSLLHOOKSTRUCT.time and foreground timing comes from EVENT_SYSTEM_FOREGROUND.time; injected clicks are rejected and no input is generated.",
        "The bounded WM_NULL probe runs on a dedicated worker so it cannot block the low-level mouse-hook message pump; it measures Firefox message responsiveness, not completed window painting or compositor presentation.",
        "A sample requires a preceding taskbar click that is independently confirmed to have made Firefox iconic; clicking a different taskbar item is rejected unless the operator later violates the protocol and returns to Firefox within the same candidate timeout.",
        "Natural WSL and VMware activity is observed but no in-guest throughput benchmark is performed, so this run cannot prove a five-percent virtualization performance bound.",
        "No ETW or WPR trace is recorded, so this run cannot identify a private Win32k, driver, storage, or application root cause.",
        "One ABBA session cannot eliminate expectation, thermal, cache, or time-order effects; repeat a mirrored BAAB session before a production policy decision.",
        "A helpful verdict requires consistent passive click-to-responsive p95 direction and no greater-than-ten-percent taskbar or scheduler p99 regression; no-clear-effect is not proof of equivalence."
    )
}

$resultDirectory = Split-Path -Parent $ResultPath
if (-not [string]::IsNullOrWhiteSpace($resultDirectory)) {
    New-Item -ItemType Directory -Path $resultDirectory -Force | Out-Null
}
Write-Utf8NoBom $ResultPath ([pscustomobject]$result | ConvertTo-Json -Depth 14)
[pscustomobject]$result | ConvertTo-Json -Depth 14
if ($script:runMutexOwned) {
    $script:runMutex.ReleaseMutex()
    $script:runMutexOwned = $false
}
$script:runMutex.Dispose()
if ([string]$result.result -ne "PASS") {
    if ($KeepWindowOnError) {
        Write-Host ""
        Write-Host "ABBA TEST STOPPED BEFORE A VALID RESULT." -ForegroundColor Red
        Write-Host ("Error code: {0}" -f [string]$result.error_code)
        if (-not [string]::IsNullOrWhiteSpace([string]$result.error_detail)) {
            Write-Host ("Error detail: {0}" -f [string]$result.error_detail)
        }
        Write-Host ("Cleanup errors: {0}" -f (@($result.cleanup_error_codes) -join ', '))
        Write-Host ("Config restored: {0}; Scheduling restored: {1}; Logging restored: {2}" -f `
            [bool]$restoreSummary.config_bytes_restored,
            [bool]$restoreSummary.scheduling_state_restored,
            [bool]$restoreSummary.logging_state_restored)
        [void](Read-Host "Press ENTER to close this window after reading the error")
    }
    exit 1
}
