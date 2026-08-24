[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$WinSched,
    [Parameter(Mandatory = $true)]
    [string]$Service,
    [Parameter(Mandatory = $true)]
    [string]$WorkDirectory
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) {
        throw "ASSERTION FAILED: $Message"
    }
}

function Wait-Condition([string]$Description, [scriptblock]$Condition, [int]$TimeoutSeconds = 30) {
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

function Write-ProfileConfig([string]$Path, [ValidateSet("memory", "compute")][string]$Profile) {
    $document = @"
schema_version = 3
controller_mode = "auto"
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
latency_guard_enabled = true
latency_target_p99_us = 2000
latency_recovery_p99_us = 1000
adjustment_stability_samples = 5

[responsiveness.memory]
use_smt = false
minimum_physical_cores = 8
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
image = "winsched-host-probe.exe"
mode = "sticky"
profile = "$Profile"
"@
    Write-Utf8NoBom $Path $document
}

function Get-Inspection([int]$ProcessId) {
    $result = & $WinSched inspect $ProcessId --json | ConvertFrom-Json
    Assert-True ($LASTEXITCODE -eq 0) "winsched inspect failed for PID $ProcessId"
    return $result
}

function Assert-ExactIds([object[]]$Actual, [object[]]$Expected, [string]$Description) {
    $actualText = @($Actual | Sort-Object) -join ","
    $expectedText = @($Expected | Sort-Object) -join ","
    Assert-True ($actualText -eq $expectedText) `
        "$Description CPU Sets differ: actual=$actualText expected=$expectedText"
}

New-Item -ItemType Directory -Path $WorkDirectory -Force | Out-Null
$probe = Join-Path $WorkDirectory "winsched-host-probe.exe"
$probeOutput = Join-Path $WorkDirectory "probe-output.log"
$probeError = Join-Path $WorkDirectory "probe-error.log"
$memoryConfig = Join-Path $WorkDirectory "memory.toml"
$computeConfig = Join-Path $WorkDirectory "compute.toml"
$memoryServiceOutput = Join-Path $WorkDirectory "memory-service.log"
$memoryServiceError = Join-Path $WorkDirectory "memory-service-error.log"
$computeServiceOutput = Join-Path $WorkDirectory "compute-service.log"
$computeServiceError = Join-Path $WorkDirectory "compute-service-error.log"
$probeProcess = $null
$serviceProcess = $null
$installedServiceWasRunning = $false
$installedServiceStateCaptured = $false

try {
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
    Copy-Item -LiteralPath $WinSched -Destination $probe -Force
    Write-ProfileConfig $memoryConfig "memory"
    Write-ProfileConfig $computeConfig "compute"
    & $WinSched config-check $memoryConfig | Out-Null
    Assert-True ($LASTEXITCODE -eq 0) "memory profile configuration is invalid"
    & $WinSched config-check $computeConfig | Out-Null
    Assert-True ($LASTEXITCODE -eq 0) "compute profile configuration is invalid"

    $topology = & $WinSched topology --json | ConvertFrom-Json
    Assert-True (@($topology.cpu_sets).Count -eq 64) "target does not expose 64 logical processors"
    Assert-True (@($topology.llc_domains).Count -eq 8) "target does not expose eight LLC domains"
    $physicalCores = @($topology.cpu_sets | ForEach-Object {
        "{0}:{1}" -f $_.group, $_.core_index
    } | Sort-Object -Unique)
    Assert-True ($physicalCores.Count -eq 32) "target does not expose 32 physical cores"

    $probeProcess = Start-Process `
        -FilePath $probe `
        -ArgumentList @("observe", "--samples", "40", "--interval-ms", "1000", "--json") `
        -RedirectStandardOutput $probeOutput `
        -RedirectStandardError $probeError `
        -PassThru
    Wait-Condition "host probe process running" {
        -not $probeProcess.HasExited
    }
    Assert-True (@((Get-Inspection $probeProcess.Id).default_cpu_set_ids).Count -eq 0) `
        "host probe inherited or retained an external CPU Set assignment"

    $memoryPlan = & $WinSched responsiveness-plan $memoryConfig --json | ConvertFrom-Json
    Assert-True (@($memoryPlan.system_reserve.reserved_physical_cores).Count -eq 4) `
        "memory plan did not reserve four physical cores"
    Assert-True (@($memoryPlan.system_reserve.reserved_cpu_set_ids).Count -eq 8) `
        "memory plan did not reserve eight SMT CPU Sets"
    Assert-True (@($memoryPlan.memory_profile.physical_cores).Count -eq 28) `
        "memory plan did not expose 28 physical cores"
    Assert-True (@($memoryPlan.memory_profile.cpu_set_ids).Count -eq 28) `
        "memory plan did not keep one SMT sibling per physical core"

    $serviceProcess = Start-Process `
        -FilePath $Service `
        -ArgumentList @("console", "--config", $memoryConfig, "--iterations", "5") `
        -RedirectStandardOutput $memoryServiceOutput `
        -RedirectStandardError $memoryServiceError `
        -PassThru
    Wait-Condition "memory partition applied to host probe" {
        $ids = @((Get-Inspection $probeProcess.Id).default_cpu_set_ids)
        ($ids -join ",") -eq (@($memoryPlan.memory_profile.cpu_set_ids | Sort-Object) -join ",")
    }
    Assert-ExactIds `
        @((Get-Inspection $probeProcess.Id).default_cpu_set_ids) `
        @($memoryPlan.memory_profile.cpu_set_ids) `
        "memory profile"
    Assert-True ($serviceProcess.WaitForExit(15000)) "memory service console did not exit"
    $memoryControllerLog = Get-Content -LiteralPath $memoryServiceOutput -Raw
    Assert-True ($memoryControllerLog.Contains('"event":"controller_stopped","success":true')) `
        "memory controller did not report a successful stop"
    Assert-True ([string]::IsNullOrWhiteSpace(
        (Get-Content -LiteralPath $memoryServiceError -Raw -ErrorAction SilentlyContinue)
    )) "memory controller wrote an error to stderr"
    Wait-Condition "memory profile rollback" {
        @((Get-Inspection $probeProcess.Id).default_cpu_set_ids).Count -eq 0
    }
    $serviceProcess = $null

    $computePlan = & $WinSched responsiveness-plan $computeConfig --json | ConvertFrom-Json
    Assert-True (@($computePlan.compute_profile.physical_cores).Count -eq 28) `
        "compute plan did not expose 28 physical cores"
    Assert-True (@($computePlan.compute_profile.cpu_set_ids).Count -eq 56) `
        "compute plan did not keep both SMT siblings"
    $serviceProcess = Start-Process `
        -FilePath $Service `
        -ArgumentList @("console", "--config", $computeConfig, "--iterations", "5") `
        -RedirectStandardOutput $computeServiceOutput `
        -RedirectStandardError $computeServiceError `
        -PassThru
    Wait-Condition "compute partition applied to host probe" {
        $ids = @((Get-Inspection $probeProcess.Id).default_cpu_set_ids)
        ($ids -join ",") -eq (@($computePlan.compute_profile.cpu_set_ids | Sort-Object) -join ",")
    }
    Assert-ExactIds `
        @((Get-Inspection $probeProcess.Id).default_cpu_set_ids) `
        @($computePlan.compute_profile.cpu_set_ids) `
        "compute profile"
    Assert-True ($serviceProcess.WaitForExit(15000)) "compute service console did not exit"
    $computeControllerLog = Get-Content -LiteralPath $computeServiceOutput -Raw
    Assert-True ($computeControllerLog.Contains('"event":"controller_stopped","success":true')) `
        "compute controller did not report a successful stop"
    Assert-True ([string]::IsNullOrWhiteSpace(
        (Get-Content -LiteralPath $computeServiceError -Raw -ErrorAction SilentlyContinue)
    )) "compute controller wrote an error to stderr"
    Wait-Condition "compute profile rollback" {
        @((Get-Inspection $probeProcess.Id).default_cpu_set_ids).Count -eq 0
    }
    $serviceProcess = $null

    [pscustomobject]@{
        result = "PASS"
        physical_cores = $physicalCores.Count
        logical_processors = @($topology.cpu_sets).Count
        llc_domains = @($topology.llc_domains).Count
        reserved_physical_cores = @($memoryPlan.system_reserve.reserved_physical_cores).Count
        reserved_cpu_sets = @($memoryPlan.system_reserve.reserved_cpu_set_ids).Count
        memory_profile_cpu_sets = @($memoryPlan.memory_profile.cpu_set_ids).Count
        compute_profile_cpu_sets = @($computePlan.compute_profile.cpu_set_ids).Count
        rollback = "PASS"
    } | ConvertTo-Json -Depth 4
} finally {
    if ($serviceProcess -and -not $serviceProcess.HasExited) {
        Stop-Process -Id $serviceProcess.Id -Force -ErrorAction SilentlyContinue
    }
    if ($probeProcess -and -not $probeProcess.HasExited) {
        try {
            & $WinSched clear $probeProcess.Id --commit --json | Out-Null
        } catch {
        }
        Stop-Process -Id $probeProcess.Id -Force -ErrorAction SilentlyContinue
    }
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
