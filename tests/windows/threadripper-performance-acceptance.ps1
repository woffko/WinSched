[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$WinSched,
    [Parameter(Mandatory = $true)]
    [string]$Service,
    [Parameter(Mandatory = $true)]
    [string]$WorkDirectory,
    [int]$WorkerCount = 48,
    [int]$WorkingSetMiB = 1024,
    [int]$WarmupSeconds = 5,
    [int]$MeasurementSeconds = 20,
    [int]$CooldownSeconds = 3
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) {
        throw "ASSERTION FAILED: $Message"
    }
}

function Wait-Condition(
    [string]$Description,
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
    throw "Timed out waiting for: $Description"
}

function Wait-ServiceState([string]$State, [int]$TimeoutSeconds = 30) {
    Wait-Condition "WinSched service state $State" {
        $current = Get-Service -Name "WinSched" -ErrorAction SilentlyContinue
        $null -ne $current -and $current.Status.ToString() -eq $State
    } $TimeoutSeconds
}

function Write-Utf8NoBom([string]$Path, [string]$Value) {
    [IO.File]::WriteAllText($Path, $Value, [Text.UTF8Encoding]::new($false))
}

function Get-Median([double[]]$Values) {
    Assert-True ($Values.Count -gt 0) "cannot calculate a median of an empty set"
    $ordered = @($Values | Sort-Object)
    $middle = [int][Math]::Floor($ordered.Count / 2)
    if (($ordered.Count % 2) -eq 1) {
        return [double]$ordered[$middle]
    }
    return ([double]$ordered[$middle - 1] + [double]$ordered[$middle]) / 2.0
}

function Get-RangePercent([double[]]$Values) {
    $median = Get-Median $Values
    if ($median -le 0.0) {
        return [double]::PositiveInfinity
    }
    $minimum = ($Values | Measure-Object -Minimum).Minimum
    $maximum = ($Values | Measure-Object -Maximum).Maximum
    return 100.0 * ([double]$maximum - [double]$minimum) / $median
}

function Get-Inspection([int]$ProcessId) {
    $result = & $WinSched inspect $ProcessId --json | ConvertFrom-Json
    Assert-True ($LASTEXITCODE -eq 0) "winsched inspect failed for PID $ProcessId"
    return $result
}

function Assert-ExactIds([object[]]$Actual, [object[]]$Expected, [string]$Description) {
    $actualText = @($Actual | ForEach-Object { [int64]$_ } | Sort-Object) -join ","
    $expectedText = @($Expected | ForEach-Object { [int64]$_ } | Sort-Object) -join ","
    Assert-True ($actualText -eq $expectedText) `
        "$Description CPU Sets differ: actual=$actualText expected=$expectedText"
}

function Write-BenchmarkConfig(
    [string]$Path,
    [ValidateSet("auto", "observe")][string]$ControllerMode
) {
    $document = @"
schema_version = 3
controller_mode = "$ControllerMode"
sample_interval_ms = 1000
minimum_process_utilization_bps = 0
all_user_processes = false
default_rule_mode = "auto"
default_workload_profile = "balanced"

[logging]
enabled = false
max_file_size_mib = 10
retained_archives = 1

[responsiveness]
enabled = true
system_reserve_percent = 10
minimum_reserved_cores = 2
maximum_reserved_cores = 8
latency_guard_enabled = false
latency_target_p99_us = 2000
latency_recovery_p99_us = 1000
adjustment_stability_samples = 5

[responsiveness.memory]
use_smt = false
minimum_physical_cores = 28
maximum_physical_cores = 28
resize_cooldown_ms = 300000

[policy]
overload_threshold_bps = 8500
minimum_improvement_bps = 2000
stability_samples = 3
minimum_residency_ms = 10000
cooldown_ms = 30000
max_mutations_per_evaluation = 1

[[rules]]
image = "winsched-memory-benchmark.exe"
mode = "sticky"
profile = "memory"
"@
    Write-Utf8NoBom $Path $document
}

$benchmarkSource = @'
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Globalization;
using System.IO;
using System.Runtime.InteropServices;
using System.Threading;

internal static class WinSchedBenchmark
{
    private const uint CreateWaitableTimerHighResolution = 0x00000002;
    private const uint TimerAllAccess = 0x001F0003;
    private const uint Infinite = 0xFFFFFFFF;
    private static int measuring;
    private static int stopping;

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern IntPtr CreateWaitableTimerEx(
        IntPtr attributes,
        string timerName,
        uint flags,
        uint desiredAccess);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern IntPtr CreateWaitableTimer(
        IntPtr attributes,
        bool manualReset,
        string timerName);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool SetWaitableTimer(
        IntPtr timer,
        ref long dueTime,
        int period,
        IntPtr completionRoutine,
        IntPtr argument,
        bool resume);

    [DllImport("kernel32.dll")]
    private static extern uint WaitForSingleObject(IntPtr handle, uint milliseconds);

    [DllImport("kernel32.dll")]
    private static extern bool CancelWaitableTimer(IntPtr timer);

    [DllImport("kernel32.dll")]
    private static extern bool CloseHandle(IntPtr handle);

    [DllImport("kernel32.dll")]
    private static extern IntPtr GetCurrentProcess();

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool SetProcessDefaultCpuSets(
        IntPtr process,
        uint[] cpuSetIds,
        uint cpuSetIdCount);

    [DllImport("winmm.dll")]
    private static extern uint timeBeginPeriod(uint period);

    [DllImport("winmm.dll")]
    private static extern uint timeEndPeriod(uint period);

    private static int Main(string[] args)
    {
        try
        {
            if (args.Length == 0)
            {
                throw new ArgumentException("mode is required");
            }
            if (String.Equals(args[0], "workload", StringComparison.OrdinalIgnoreCase))
            {
                return RunWorkload(args);
            }
            if (String.Equals(args[0], "probe", StringComparison.OrdinalIgnoreCase))
            {
                return RunProbe(args);
            }
            throw new ArgumentException("unknown mode: " + args[0]);
        }
        catch (Exception error)
        {
            Console.Error.WriteLine(error.ToString());
            return 1;
        }
    }

    private static int RunWorkload(string[] args)
    {
        if (args.Length != 8)
        {
            throw new ArgumentException(
                "workload requires ready, start, result, workers, MiB, warmup, measurement");
        }
        string readyPath = args[1];
        string startPath = args[2];
        string resultPath = args[3];
        int workerCount = Int32.Parse(args[4], CultureInfo.InvariantCulture);
        int totalMiB = Int32.Parse(args[5], CultureInfo.InvariantCulture);
        int warmupSeconds = Int32.Parse(args[6], CultureInfo.InvariantCulture);
        int measurementSeconds = Int32.Parse(args[7], CultureInfo.InvariantCulture);
        if (workerCount < 1 || totalMiB < workerCount || warmupSeconds < 1 ||
            measurementSeconds < 1)
        {
            throw new ArgumentOutOfRangeException("invalid workload dimensions");
        }

        measuring = 0;
        stopping = 0;
        int longsPerWorker = checked((totalMiB / workerCount) * 1024 * 1024 / 8);
        long[][] buffers = new long[workerCount][];
        long[] measuredOperations = new long[workerCount];
        long[] checksums = new long[workerCount];
        ManualResetEvent startWorkers = new ManualResetEvent(false);
        Thread[] workers = new Thread[workerCount];

        for (int worker = 0; worker < workerCount; ++worker)
        {
            buffers[worker] = new long[longsPerWorker];
            for (int offset = 0; offset < longsPerWorker; offset += 512)
            {
                buffers[worker][offset] = worker + offset;
            }
            int workerIndex = worker;
            workers[worker] = new Thread(delegate()
            {
                ulong state = 0x9E3779B97F4A7C15UL ^
                    ((ulong)(workerIndex + 1) * 0xBF58476D1CE4E5B9UL);
                long localMeasured = 0;
                long checksum = 0;
                long[] buffer = buffers[workerIndex];
                startWorkers.WaitOne();
                while (Volatile.Read(ref stopping) == 0)
                {
                    for (int operation = 0; operation < 4096; ++operation)
                    {
                        state ^= state >> 12;
                        state ^= state << 25;
                        state ^= state >> 27;
                        int index = (int)((state * 2685821657736338717UL) %
                            (ulong)buffer.Length);
                        long value = buffer[index] + 1;
                        buffer[index] = value;
                        checksum ^= value;
                    }
                    if (Volatile.Read(ref measuring) != 0)
                    {
                        localMeasured += 4096;
                    }
                }
                measuredOperations[workerIndex] = localMeasured;
                checksums[workerIndex] = checksum;
            });
            workers[worker].IsBackground = true;
            workers[worker].Priority = ThreadPriority.Normal;
            workers[worker].Start();
        }

        File.WriteAllText(readyPath, "ready", new System.Text.UTF8Encoding(false));
        WaitForStartFile(startPath);
        startWorkers.Set();
        Thread.Sleep(checked(warmupSeconds * 1000));

        Process current = Process.GetCurrentProcess();
        TimeSpan cpuBefore = current.TotalProcessorTime;
        Stopwatch wall = Stopwatch.StartNew();
        Volatile.Write(ref measuring, 1);
        Thread.Sleep(checked(measurementSeconds * 1000));
        Volatile.Write(ref measuring, 0);
        Volatile.Write(ref stopping, 1);
        for (int worker = 0; worker < workers.Length; ++worker)
        {
            workers[worker].Join();
        }
        wall.Stop();
        TimeSpan cpuAfter = current.TotalProcessorTime;

        long operations = 0;
        long checksumTotal = 0;
        for (int worker = 0; worker < workerCount; ++worker)
        {
            operations += measuredOperations[worker];
            checksumTotal ^= checksums[worker];
        }
        double seconds = wall.Elapsed.TotalSeconds;
        double throughputMops = operations / seconds / 1000000.0;
        double cpuPercent = (cpuAfter - cpuBefore).TotalSeconds / seconds /
            Environment.ProcessorCount * 100.0;
        string json = String.Format(
            CultureInfo.InvariantCulture,
            "{{\"mode\":\"workload\",\"workers\":{0},\"working_set_mib\":{1}," +
            "\"measurement_seconds\":{2:F6},\"operations\":{3}," +
            "\"throughput_mops\":{4:F6},\"process_cpu_percent\":{5:F6}," +
            "\"checksum\":{6}}}",
            workerCount,
            totalMiB,
            seconds,
            operations,
            throughputMops,
            cpuPercent,
            checksumTotal);
        File.WriteAllText(resultPath, json, new System.Text.UTF8Encoding(false));
        return 0;
    }

    private static int RunProbe(string[] args)
    {
        if (args.Length != 8)
        {
            throw new ArgumentException(
                "probe requires ready, start, result, warmup, measurement, period-ms, CPU Sets");
        }
        string readyPath = args[1];
        string startPath = args[2];
        string resultPath = args[3];
        int warmupSeconds = Int32.Parse(args[4], CultureInfo.InvariantCulture);
        int measurementSeconds = Int32.Parse(args[5], CultureInfo.InvariantCulture);
        int periodMilliseconds = Int32.Parse(args[6], CultureInfo.InvariantCulture);
        string[] cpuSetTokens = args[7].Split(new char[] { ',' },
            StringSplitOptions.RemoveEmptyEntries);
        uint[] cpuSetIds = new uint[cpuSetTokens.Length];
        for (int index = 0; index < cpuSetTokens.Length; ++index)
        {
            cpuSetIds[index] = UInt32.Parse(
                cpuSetTokens[index],
                CultureInfo.InvariantCulture);
        }
        if (warmupSeconds < 1 || measurementSeconds < 1 || periodMilliseconds < 1)
        {
            throw new ArgumentOutOfRangeException("invalid probe dimensions");
        }
        if (cpuSetIds.Length == 0)
        {
            throw new ArgumentException("probe CPU Set list is empty");
        }
        if (!SetProcessDefaultCpuSets(
            GetCurrentProcess(),
            cpuSetIds,
            (uint)cpuSetIds.Length))
        {
            throw new InvalidOperationException(
                "SetProcessDefaultCpuSets failed: " + Marshal.GetLastWin32Error());
        }

        File.WriteAllText(readyPath, "ready", new System.Text.UTF8Encoding(false));
        WaitForStartFile(startPath);
        Thread.Sleep(checked(warmupSeconds * 1000));
        timeBeginPeriod(1);
        IntPtr timer = CreateWaitableTimerEx(
            IntPtr.Zero,
            null,
            CreateWaitableTimerHighResolution,
            TimerAllAccess);
        bool highResolution = timer != IntPtr.Zero;
        if (timer == IntPtr.Zero)
        {
            timer = CreateWaitableTimer(IntPtr.Zero, false, null);
        }
        if (timer == IntPtr.Zero)
        {
            timeEndPeriod(1);
            throw new InvalidOperationException(
                "CreateWaitableTimer failed: " + Marshal.GetLastWin32Error());
        }

        List<double> latenessMicroseconds = new List<double>(
            measurementSeconds * 1000 / periodMilliseconds + 16);
        Stopwatch phase = Stopwatch.StartNew();
        try
        {
            while (phase.Elapsed.TotalSeconds < measurementSeconds)
            {
                long dueTime = -checked((long)periodMilliseconds * 10000L);
                Stopwatch sample = Stopwatch.StartNew();
                if (!SetWaitableTimer(
                    timer,
                    ref dueTime,
                    0,
                    IntPtr.Zero,
                    IntPtr.Zero,
                    false))
                {
                    throw new InvalidOperationException(
                        "SetWaitableTimer failed: " + Marshal.GetLastWin32Error());
                }
                uint wait = WaitForSingleObject(timer, Infinite);
                sample.Stop();
                if (wait != 0)
                {
                    throw new InvalidOperationException(
                        "WaitForSingleObject failed: " + wait);
                }
                double elapsedMicroseconds = sample.ElapsedTicks * 1000000.0 /
                    Stopwatch.Frequency;
                latenessMicroseconds.Add(Math.Max(
                    0.0,
                    elapsedMicroseconds - periodMilliseconds * 1000.0));
            }
        }
        finally
        {
            CancelWaitableTimer(timer);
            CloseHandle(timer);
            timeEndPeriod(1);
        }

        latenessMicroseconds.Sort();
        if (latenessMicroseconds.Count == 0)
        {
            throw new InvalidOperationException("probe produced no samples");
        }
        double p50 = Percentile(latenessMicroseconds, 0.50);
        double p95 = Percentile(latenessMicroseconds, 0.95);
        double p99 = Percentile(latenessMicroseconds, 0.99);
        double maximum = latenessMicroseconds[latenessMicroseconds.Count - 1];
        string json = String.Format(
            CultureInfo.InvariantCulture,
            "{{\"mode\":\"probe\",\"samples\":{0},\"period_ms\":{1}," +
            "\"cpu_set_count\":{2},\"high_resolution_timer\":{3}," +
            "\"p50_us\":{4:F6},\"p95_us\":{5:F6},\"p99_us\":{6:F6}," +
            "\"max_us\":{7:F6}",
            latenessMicroseconds.Count,
            periodMilliseconds,
            cpuSetIds.Length,
            highResolution ? "true" : "false",
            p50,
            p95,
            p99,
            maximum) + "}";
        File.WriteAllText(resultPath, json, new System.Text.UTF8Encoding(false));
        return 0;
    }

    private static double Percentile(List<double> ordered, double percentile)
    {
        int index = (int)Math.Ceiling(ordered.Count * percentile) - 1;
        if (index < 0)
        {
            index = 0;
        }
        if (index >= ordered.Count)
        {
            index = ordered.Count - 1;
        }
        return ordered[index];
    }

    private static void WaitForStartFile(string path)
    {
        Stopwatch timeout = Stopwatch.StartNew();
        while (!File.Exists(path))
        {
            if (timeout.Elapsed.TotalSeconds > 60.0)
            {
                throw new TimeoutException("start signal was not created");
            }
            Thread.Sleep(10);
        }
    }
}
'@

New-Item -ItemType Directory -Path $WorkDirectory -Force | Out-Null
$helper = Join-Path $WorkDirectory "winsched-benchmark-helper.exe"
$workload = Join-Path $WorkDirectory "winsched-memory-benchmark.exe"
$probe = Join-Path $WorkDirectory "winsched-latency-probe.exe"
$baselineConfig = Join-Path $WorkDirectory "benchmark-observe.toml"
$managedConfig = Join-Path $WorkDirectory "benchmark-auto.toml"
$summaryPath = Join-Path $WorkDirectory "performance-result.json"
$serviceProcess = $null
$workloadProcess = $null
$probeProcess = $null
$installedServiceWasRunning = $false
$installedServiceStateCaptured = $false

function Stop-PhaseProcesses {
    if ($serviceProcess -and -not $serviceProcess.HasExited) {
        Stop-Process -Id $serviceProcess.Id -Force -ErrorAction SilentlyContinue
    }
    if ($workloadProcess -and -not $workloadProcess.HasExited) {
        try {
            & $WinSched clear $workloadProcess.Id --commit --json | Out-Null
        } catch {
        }
        Stop-Process -Id $workloadProcess.Id -Force -ErrorAction SilentlyContinue
    }
    if ($probeProcess -and -not $probeProcess.HasExited) {
        Stop-Process -Id $probeProcess.Id -Force -ErrorAction SilentlyContinue
    }
}

function Invoke-Phase([string]$Mode, [int]$Sequence, [object[]]$ExpectedCpuSetIds) {
    $prefix = Join-Path $WorkDirectory ("{0}-{1}" -f $Sequence, $Mode)
    $startFile = "$prefix-start.signal"
    $workloadReady = "$prefix-workload.ready"
    $probeReady = "$prefix-probe.ready"
    $workloadResult = "$prefix-workload.json"
    $probeResult = "$prefix-probe.json"
    $workloadOutput = "$prefix-workload.stdout.log"
    $workloadError = "$prefix-workload.stderr.log"
    $probeOutput = "$prefix-probe.stdout.log"
    $probeError = "$prefix-probe.stderr.log"
    $serviceOutput = "$prefix-service.stdout.log"
    $serviceError = "$prefix-service.stderr.log"
    Remove-Item -LiteralPath @(
        $startFile,
        $workloadReady,
        $probeReady,
        $workloadResult,
        $probeResult,
        $workloadOutput,
        $workloadError,
        $probeOutput,
        $probeError,
        $serviceOutput,
        $serviceError
    ) -Force -ErrorAction SilentlyContinue

    try {
        $script:workloadProcess = Start-Process `
            -FilePath $workload `
            -ArgumentList @(
                "workload",
                $workloadReady,
                $startFile,
                $workloadResult,
                $WorkerCount,
                $WorkingSetMiB,
                $WarmupSeconds,
                $MeasurementSeconds
            ) `
            -RedirectStandardOutput $workloadOutput `
            -RedirectStandardError $workloadError `
            -PassThru
        $script:probeProcess = Start-Process `
            -FilePath $probe `
            -ArgumentList @(
                "probe",
                $probeReady,
                $startFile,
                $probeResult,
                $WarmupSeconds,
                $MeasurementSeconds,
                2,
                (@($script:reservedCpuSetIds) -join ",")
            ) `
            -RedirectStandardOutput $probeOutput `
            -RedirectStandardError $probeError `
            -PassThru
        Wait-Condition "phase $Sequence workload and probe ready" {
            (Test-Path -LiteralPath $workloadReady -PathType Leaf) -and
                (Test-Path -LiteralPath $probeReady -PathType Leaf)
        } 30

        $iterations = $WarmupSeconds + $MeasurementSeconds + 6
        $phaseConfig = if ($Mode -eq "managed") {
            $managedConfig
        } else {
            $baselineConfig
        }
        $script:serviceProcess = Start-Process `
            -FilePath $Service `
            -ArgumentList @(
                "console",
                "--config",
                $phaseConfig,
                "--iterations",
                $iterations
            ) `
            -RedirectStandardOutput $serviceOutput `
            -RedirectStandardError $serviceError `
            -PassThru
        if ($Mode -eq "managed") {
            Wait-Condition "phase $Sequence memory partition applied" {
                $inspection = Get-Inspection $workloadProcess.Id
                $actual = @($inspection.default_cpu_set_ids | Sort-Object)
                ($actual -join ",") -eq (@($ExpectedCpuSetIds | Sort-Object) -join ",")
            } 30
            Assert-ExactIds `
                @((Get-Inspection $workloadProcess.Id).default_cpu_set_ids) `
                $ExpectedCpuSetIds `
                "managed workload"
        } else {
            Start-Sleep -Seconds 2
            Assert-True (
                @((Get-Inspection $workloadProcess.Id).default_cpu_set_ids).Count -eq 0
            ) "baseline workload unexpectedly has CPU Sets"
        }
        Assert-ExactIds `
            @((Get-Inspection $probeProcess.Id).default_cpu_set_ids) `
            @($script:reservedCpuSetIds) `
            "reserve-local latency probe"

        Start-Sleep -Seconds 2
        Write-Utf8NoBom $startFile "start"
        Wait-Condition "phase $Sequence workload and probe results" {
            (Test-Path -LiteralPath $workloadResult -PathType Leaf) -and
                (Test-Path -LiteralPath $probeResult -PathType Leaf)
        } ($WarmupSeconds + $MeasurementSeconds + 30)
        Start-Sleep -Milliseconds 250
        Assert-True ([string]::IsNullOrWhiteSpace(
            (Get-Content -LiteralPath $workloadError -Raw -ErrorAction SilentlyContinue)
        )) "phase $Sequence workload wrote to stderr"
        Assert-True ([string]::IsNullOrWhiteSpace(
            (Get-Content -LiteralPath $probeError -Raw -ErrorAction SilentlyContinue)
        )) "phase $Sequence probe wrote to stderr"
        Assert-True (Test-Path -LiteralPath $workloadResult -PathType Leaf) `
            "phase $Sequence workload result is missing"
        Assert-True (Test-Path -LiteralPath $probeResult -PathType Leaf) `
            "phase $Sequence probe result is missing"

        $workloadData = Get-Content -LiteralPath $workloadResult -Raw | ConvertFrom-Json
        $probeData = Get-Content -LiteralPath $probeResult -Raw | ConvertFrom-Json
        $minimumSamples = $MeasurementSeconds * 100
        Assert-True ([int]$probeData.samples -ge $minimumSamples) `
            "phase $Sequence probe sample count is too low"
        Assert-True ([double]$workloadData.throughput_mops -gt 0.0) `
            "phase $Sequence throughput is not positive"
        Assert-True ([double]$probeData.p99_us -gt 0.0) `
            "phase $Sequence p99 is not positive"
        Assert-True ([bool]$probeData.high_resolution_timer) `
            "phase $Sequence did not obtain a high-resolution waitable timer"

        if ($serviceProcess) {
            Wait-Condition "phase $Sequence controller graceful stop" {
                if (-not (Test-Path -LiteralPath $serviceOutput -PathType Leaf)) {
                    return $false
                }
                $log = Get-Content -LiteralPath $serviceOutput -Raw
                return $log.Contains('"event":"controller_stopped"')
            } 45
            Assert-True ([string]::IsNullOrWhiteSpace(
                (Get-Content -LiteralPath $serviceError -Raw -ErrorAction SilentlyContinue)
            )) "phase $Sequence controller wrote to stderr"
            $controllerLog = Get-Content -LiteralPath $serviceOutput -Raw
            Assert-True ($controllerLog.Contains(
                '"event":"controller_stopped","success":true'
            )) "phase $Sequence controller did not report a successful stop"
        }

        $result = [ordered]@{
            sequence = $Sequence
            mode = $Mode
            throughput_mops = [double]$workloadData.throughput_mops
            process_cpu_percent = [double]$workloadData.process_cpu_percent
            latency_p50_us = [double]$probeData.p50_us
            latency_p95_us = [double]$probeData.p95_us
            latency_p99_us = [double]$probeData.p99_us
            latency_max_us = [double]$probeData.max_us
            latency_samples = [int]$probeData.samples
            high_resolution_timer = [bool]$probeData.high_resolution_timer
        }
        Write-Host ([pscustomobject]$result | ConvertTo-Json -Compress)
        return [pscustomobject]$result
    } finally {
        Stop-PhaseProcesses
        $script:serviceProcess = $null
        $script:workloadProcess = $null
        $script:probeProcess = $null
    }
}

try {
    Assert-True (Test-Path -LiteralPath $WinSched -PathType Leaf) "winsched is missing"
    Assert-True (Test-Path -LiteralPath $Service -PathType Leaf) "service is missing"
    Assert-True ($WorkerCount -ge 1) "WorkerCount must be positive"
    Assert-True ($WorkingSetMiB -ge $WorkerCount) `
        "WorkingSetMiB must provide at least one MiB per worker"
    Assert-True ($WarmupSeconds -ge 1) "WarmupSeconds must be positive"
    Assert-True ($MeasurementSeconds -ge 5) `
        "MeasurementSeconds must be at least five seconds"

    $installedService = Get-Service -Name "WinSched" -ErrorAction SilentlyContinue
    if ($null -ne $installedService) {
        $installedServiceWasRunning = $installedService.Status -ne "Stopped"
        $installedServiceStateCaptured = $true
        if ($installedServiceWasRunning) {
            & $Service stop | Out-Null
            Assert-True ($LASTEXITCODE -eq 0) "could not stop installed WinSched service"
            Wait-ServiceState "Stopped"
        }
    }

    Remove-Item -LiteralPath $helper,$workload,$probe -Force -ErrorAction SilentlyContinue
    Add-Type `
        -TypeDefinition $benchmarkSource `
        -Language CSharp `
        -OutputAssembly $helper `
        -OutputType ConsoleApplication
    Copy-Item -LiteralPath $helper -Destination $workload -Force
    Copy-Item -LiteralPath $helper -Destination $probe -Force
    Write-BenchmarkConfig $baselineConfig "observe"
    Write-BenchmarkConfig $managedConfig "auto"
    & $WinSched config-check $baselineConfig | Out-Null
    Assert-True ($LASTEXITCODE -eq 0) "observe benchmark configuration is invalid"
    & $WinSched config-check $managedConfig | Out-Null
    Assert-True ($LASTEXITCODE -eq 0) "auto benchmark configuration is invalid"

    $topology = & $WinSched topology --json | ConvertFrom-Json
    $plan = & $WinSched responsiveness-plan $managedConfig --json | ConvertFrom-Json
    $physicalCores = @($topology.cpu_sets | ForEach-Object {
        "{0}:{1}" -f $_.group, $_.core_index
    } | Sort-Object -Unique)
    Assert-True ($physicalCores.Count -eq 32) `
        "target does not expose 32 physical cores"
    Assert-True (@($topology.cpu_sets).Count -eq 64) `
        "target does not expose 64 logical processors"
    Assert-True (@($topology.llc_domains).Count -eq 8) `
        "target does not expose eight LLC domains"
    Assert-True (@($plan.system_reserve.reserved_physical_cores).Count -eq 4) `
        "plan does not reserve four physical cores"
    Assert-True (@($plan.memory_profile.cpu_set_ids).Count -eq 28) `
        "memory profile does not expose 28 CPU Sets"
    $expectedCpuSetIds = @($plan.memory_profile.cpu_set_ids)
    $script:reservedCpuSetIds = @($plan.system_reserve.reserved_cpu_set_ids)

    $phaseOrder = @("baseline", "managed", "managed", "baseline", "baseline", "managed")
    $results = @()
    for ($index = 0; $index -lt $phaseOrder.Count; ++$index) {
        $results += Invoke-Phase $phaseOrder[$index] ($index + 1) $expectedCpuSetIds
        if ($index + 1 -lt $phaseOrder.Count) {
            Start-Sleep -Seconds $CooldownSeconds
        }
    }

    $baseline = @($results | Where-Object { $_.mode -eq "baseline" })
    $managed = @($results | Where-Object { $_.mode -eq "managed" })
    Assert-True ($baseline.Count -eq 3) "expected three baseline phases"
    Assert-True ($managed.Count -eq 3) "expected three managed phases"
    $baselineThroughput = Get-Median @($baseline.throughput_mops)
    $managedThroughput = Get-Median @($managed.throughput_mops)
    $baselineP99 = Get-Median @($baseline.latency_p99_us)
    $managedP99 = Get-Median @($managed.latency_p99_us)
    $latencyImprovementPercent = 100.0 * ($baselineP99 - $managedP99) / $baselineP99
    $throughputDeltaPercent = 100.0 * `
        ($managedThroughput - $baselineThroughput) / $baselineThroughput
    $baselineThroughputRange = Get-RangePercent @($baseline.throughput_mops)
    $managedThroughputRange = Get-RangePercent @($managed.throughput_mops)
    $latencyGate = $latencyImprovementPercent -ge 20.0
    $throughputGate = $throughputDeltaPercent -ge -5.0
    $noiseGate = $baselineThroughputRange -le 10.0 -and `
        $managedThroughputRange -le 10.0

    $summary = [ordered]@{
        result = if ($latencyGate -and $throughputGate -and $noiseGate) {
            "PASS"
        } else {
            "FAIL"
        }
        target = [ordered]@{
            physical_cores = $physicalCores.Count
            logical_processors = @($topology.cpu_sets).Count
            llc_domains = @($topology.llc_domains).Count
            reserved_physical_cores = @(
                $plan.system_reserve.reserved_physical_cores
            ).Count
            memory_profile_cpu_sets = $expectedCpuSetIds.Count
        }
        workload = [ordered]@{
            workers = $WorkerCount
            working_set_mib = $WorkingSetMiB
            warmup_seconds = $WarmupSeconds
            measurement_seconds = $MeasurementSeconds
            operation = "random private-buffer 64-bit read-modify-write"
            throughput_unit = "million operations per second"
        }
        medians = [ordered]@{
            baseline_throughput_mops = $baselineThroughput
            managed_throughput_mops = $managedThroughput
            throughput_delta_percent = $throughputDeltaPercent
            baseline_latency_p99_us = $baselineP99
            managed_latency_p99_us = $managedP99
            latency_improvement_percent = $latencyImprovementPercent
        }
        stability = [ordered]@{
            baseline_throughput_range_percent = $baselineThroughputRange
            managed_throughput_range_percent = $managedThroughputRange
            maximum_allowed_range_percent = 10.0
        }
        gates = [ordered]@{
            latency_improvement_at_least_20_percent = $latencyGate
            throughput_loss_at_most_5_percent = $throughputGate
            throughput_run_range_at_most_10_percent = $noiseGate
        }
        phases = $results
        metric_scope = "synthetic random-memory operations; not measured DRAM bandwidth"
    }
    [pscustomobject]$summary |
        ConvertTo-Json -Depth 8 |
        Set-Content -LiteralPath $summaryPath -Encoding UTF8
    [pscustomobject]$summary | ConvertTo-Json -Depth 8
    Assert-True ($noiseGate) `
        "throughput variance exceeded 10 percent; rerun under stable thermals"
    Assert-True ($latencyGate) `
        "managed median p99 did not improve by at least 20 percent"
    Assert-True ($throughputGate) `
        "managed median throughput loss exceeded 5 percent"
} finally {
    Stop-PhaseProcesses
    if ($installedServiceStateCaptured -and $installedServiceWasRunning) {
        try {
            & $Service start | Out-Null
            if ($LASTEXITCODE -eq 0) {
                Wait-ServiceState "Running"
            }
        } catch {
        }
    }
}
