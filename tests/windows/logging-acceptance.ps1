[CmdletBinding()]
param(
    [string]$InstallDirectory = "$env:ProgramData\WinSched",
    [string]$DataDirectory = "$env:ProgramData\WinSched"
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

function Wait-ServiceState([ValidateSet("Running", "Stopped")][string]$State) {
    Wait-Condition "WinSched service state $State" {
        $service = Get-Service -Name "WinSched" -ErrorAction SilentlyContinue
        $null -ne $service -and $service.Status.ToString() -eq $State
    } 30
}

function Stop-ServiceUnderTest {
    $service = Get-Service -Name "WinSched" -ErrorAction SilentlyContinue
    if ($null -ne $service -and $service.Status -ne "Stopped") {
        $serviceProcess = Get-CimInstance Win32_Service -Filter "Name='WinSched'"
        $serviceProcessId = [int]$serviceProcess.ProcessId
        & $script:serviceBinary stop | Out-Null
        if ($LASTEXITCODE -ne 0) {
            throw "winsched-service stop failed with exit code $LASTEXITCODE"
        }
        Wait-ServiceState "Stopped"
        if ($serviceProcessId -gt 0) {
            Wait-Condition "WinSched service process $serviceProcessId exited" {
                $null -eq (Get-Process -Id $serviceProcessId -ErrorAction SilentlyContinue)
            } 30
        }
    }
}

function Start-ServiceUnderTest {
    $service = Get-Service -Name "WinSched" -ErrorAction SilentlyContinue
    if ($null -eq $service) {
        throw "WinSched service is not installed"
    }
    if ($service.Status -ne "Running") {
        & $script:serviceBinary start | Out-Null
        if ($LASTEXITCODE -ne 0) {
            throw "winsched-service start failed with exit code $LASTEXITCODE"
        }
        Wait-ServiceState "Running"
    }
}

function Set-FileAtomically([string]$Path, [byte[]]$Bytes) {
    $directory = Split-Path -Parent $Path
    $temporaryPath = Join-Path $directory (
        ".{0}.logging-acceptance-{1}.tmp" -f `
            (Split-Path -Leaf $Path), `
            [Guid]::NewGuid().ToString("N")
    )
    $replacementBackup = "$temporaryPath.backup"
    try {
        [System.IO.File]::WriteAllBytes($temporaryPath, $Bytes)
        [System.IO.File]::Replace($temporaryPath, $Path, $replacementBackup, $true)
    } finally {
        foreach ($cleanupPath in @($temporaryPath, $replacementBackup)) {
            if (Test-Path -LiteralPath $cleanupPath) {
                Remove-Item -LiteralPath $cleanupPath -Force -ErrorAction SilentlyContinue
            }
        }
    }
}

function Set-Utf8FileAtomically([string]$Path, [string]$Text) {
    $encoding = New-Object System.Text.UTF8Encoding($false)
    Set-FileAtomically $Path $encoding.GetBytes($Text)
}

function New-LoggingConfigText(
    [string]$Base,
    [bool]$Enabled,
    [ValidateRange(1, 100)]
    [int]$MaxFileSizeMiB,
    [ValidateRange(0, 10)]
    [int]$RetainedArchives
) {
    $enabledText = $Enabled.ToString().ToLowerInvariant()
    $loggingBlock = @"
[logging]
enabled = $enabledText
max_file_size_mib = $MaxFileSizeMiB
retained_archives = $RetainedArchives
"@
    $result = [regex]::Replace(
        $Base,
        "(?m)^\s*schema_version\s*=\s*\d+\s*$",
        "schema_version = 2",
        1
    )
    $result = [regex]::Replace(
        $result,
        "(?m)^\s*controller_mode\s*=\s*`"[^`"]+`"\s*$",
        'controller_mode = "observe"',
        1
    )
    $result = [regex]::Replace(
        $result,
        "(?m)^\s*sample_interval_ms\s*=\s*\d+\s*$",
        "sample_interval_ms = 1000",
        1
    )
    $result = [regex]::Replace(
        $result,
        "(?m)^\s*all_user_processes\s*=\s*(true|false)\s*$",
        "all_user_processes = false",
        1
    )
    $loggingPattern = "(?ms)^\s*\[logging\]\s*.*?(?=^\s*\[|\z)"
    if ([regex]::IsMatch($result, $loggingPattern)) {
        return [regex]::Replace($result, $loggingPattern, "$loggingBlock`r`n", 1)
    }
    $policyPattern = "(?m)^\s*\[policy\]\s*$"
    if ([regex]::IsMatch($result, $policyPattern)) {
        return [regex]::Replace(
            $result,
            $policyPattern,
            "$loggingBlock`r`n`r`n[policy]",
            1
        )
    }
    return "$result`r`n`r`n$loggingBlock`r`n"
}

function Wait-AppliedLogging(
    [uint64]$AfterSequence,
    [bool]$Enabled,
    [int]$MaxFileSizeMiB,
    [int]$RetainedArchives,
    [int]$ExpectedPid,
    [string]$Description,
    [int]$TimeoutSeconds = 20
) {
    Wait-Condition $Description {
        $status = Read-Status
        $null -ne $status -and
            [int]$status.schema_version -eq 2 -and
            [uint64]$status.config_reload_sequence -gt $AfterSequence -and
            $status.config_reload_result -eq "reloaded" -and
            [bool]$status.applied_logging.enabled -eq $Enabled -and
            [int]$status.applied_logging.max_file_size_mib -eq $MaxFileSizeMiB -and
            [int]$status.applied_logging.retained_archives -eq $RetainedArchives -and
            ($ExpectedPid -eq 0 -or [int]$status.service_pid -eq $ExpectedPid)
    } $TimeoutSeconds
    return (Read-Status)
}

function Set-LoggingConfiguration(
    [bool]$Enabled,
    [int]$MaxFileSizeMiB,
    [int]$RetainedArchives,
    [int]$ExpectedPid
) {
    $status = Read-Status
    Assert-True ($null -ne $status) "status.json is unavailable before configuration reload"
    $baselineSequence = [uint64]$status.config_reload_sequence
    $text = New-LoggingConfigText `
        $script:baseConfigText `
        $Enabled `
        $MaxFileSizeMiB `
        $RetainedArchives
    Set-Utf8FileAtomically $script:configPath $text
    return Wait-AppliedLogging `
        $baselineSequence `
        $Enabled `
        $MaxFileSizeMiB `
        $RetainedArchives `
        $ExpectedPid `
        "logging configuration reload"
}

function Get-LogFiles {
    return @(
        Get-ChildItem -LiteralPath $DataDirectory -File -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -match '^winsched\.log(?:\.\d+)?$' } |
            Sort-Object Name
    )
}

function Remove-TestLogs {
    foreach ($file in @(Get-LogFiles)) {
        Remove-Item -LiteralPath $file.FullName -Force
    }
}

function Get-LogSnapshot {
    $snapshot = [ordered]@{}
    foreach ($file in @(Get-LogFiles)) {
        $snapshot[$file.Name] = [ordered]@{
            length = [int64]$file.Length
            sha256 = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        }
    }
    return $snapshot
}

function Assert-SnapshotsEqual(
    [System.Collections.IDictionary]$Expected,
    [System.Collections.IDictionary]$Actual,
    [string]$Description
) {
    $expectedKeys = @($Expected.Keys | Sort-Object)
    $actualKeys = @($Actual.Keys | Sort-Object)
    Assert-True (($expectedKeys -join "|") -eq ($actualKeys -join "|")) `
        "$Description changed the set of log files"
    foreach ($name in $expectedKeys) {
        Assert-True ($Expected[$name].length -eq $Actual[$name].length) `
            "$Description changed the length of $name"
        Assert-True ($Expected[$name].sha256 -eq $Actual[$name].sha256) `
            "$Description changed the bytes of $name"
    }
}

function Invoke-SchedulingChange {
    $before = Read-Status
    Assert-True ($null -ne $before) "status.json is unavailable before scheduling change"
    $targetEnabled = -not [bool]$before.scheduling_enabled
    $command = if ($targetEnabled) { "enable" } else { "disable" }
    & $script:serviceBinary $command | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "winsched-service $command failed with exit code $LASTEXITCODE"
    }
    Wait-Condition "scheduling state changed to $targetEnabled" {
        $current = Read-Status
        $null -ne $current -and [bool]$current.scheduling_enabled -eq $targetEnabled
    }
}

function Write-NearLimitSeed([int]$Cycle) {
    $maxBytes = 1MB
    $targetBytes = $maxBytes - 16
    $prefix = "{`"event`":`"acceptance_seed`",`"cycle`":$Cycle,`"payload`":`""
    $suffix = "`"}`n"
    $overhead = [System.Text.Encoding]::UTF8.GetByteCount($prefix + $suffix)
    $payloadLength = $targetBytes - $overhead
    Assert-True ($payloadLength -gt 0) "acceptance seed payload length is invalid"
    $text = $prefix + ("x" * $payloadLength) + $suffix
    $encoding = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText($script:activeLogPath, $text, $encoding)
    Assert-True ((Get-Item -LiteralPath $script:activeLogPath).Length -eq $targetBytes) `
        "acceptance seed did not reach the intended near-limit length"
}

function Read-SharedUtf8Lines([string]$Path) {
    $stream = [System.IO.File]::Open(
        $Path,
        [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::Read,
        ([System.IO.FileShare]::ReadWrite -bor [System.IO.FileShare]::Delete)
    )
    try {
        $reader = [System.IO.StreamReader]::new(
            $stream,
            [System.Text.Encoding]::UTF8,
            $true,
            4096,
            $true
        )
        try {
            $text = $reader.ReadToEnd()
        } finally {
            $reader.Dispose()
        }
    } finally {
        $stream.Dispose()
    }
    return @([regex]::Split($text, "\r?\n"))
}

function Get-SeedCycle([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        return $null
    }
    foreach ($line in @(Read-SharedUtf8Lines $Path)) {
        if ($line.Contains('"event":"acceptance_seed"')) {
            return [int](($line | ConvertFrom-Json).cycle)
        }
    }
    return $null
}

function Assert-ValidJsonLines([string]$Path) {
    Assert-True (Test-Path -LiteralPath $Path -PathType Leaf) "log file is missing: $Path"
    $lineCount = 0
    foreach ($line in @(Read-SharedUtf8Lines $Path)) {
        if ([string]::IsNullOrWhiteSpace($line)) {
            continue
        }
        [void]($line | ConvertFrom-Json)
        $lineCount++
    }
    Assert-True ($lineCount -gt 0) "log file contains no complete JSONL records: $Path"
}

function Wait-InitialStatus(
    [bool]$Enabled,
    [int]$MaxFileSizeMiB,
    [int]$RetainedArchives
) {
    Wait-Condition "initial status with expected logging policy" {
        $status = Read-Status
        $service = Get-CimInstance Win32_Service -Filter "Name='WinSched'" -ErrorAction SilentlyContinue
        $null -ne $status -and
            $null -ne $service -and
            $service.State -eq "Running" -and
            [int]$status.schema_version -eq 2 -and
            [int]$status.service_pid -eq [int]$service.ProcessId -and
            @("initial", "reloaded") -contains [string]$status.config_reload_result -and
            [bool]$status.applied_logging.enabled -eq $Enabled -and
            [int]$status.applied_logging.max_file_size_mib -eq $MaxFileSizeMiB -and
            [int]$status.applied_logging.retained_archives -eq $RetainedArchives
    } 30
    return (Read-Status)
}

$script:serviceBinary = Join-Path $InstallDirectory "winsched-service.exe"
$script:configPath = Join-Path $DataDirectory "winsched.toml"
$script:statusPath = Join-Path $DataDirectory "status.json"
$script:activeLogPath = Join-Path $DataDirectory "winsched.log"
$backupRoot = Join-Path $env:TEMP (
    "winsched-logging-acceptance-{0}" -f [Guid]::NewGuid().ToString("N")
)
$configBackupPath = Join-Path $backupRoot "winsched.toml"
$logBackupDirectory = Join-Path $backupRoot "logs"
$originalServiceRunning = $false
$serviceStateCaptured = $false
$backupCaptured = $false
$originalConfigBytes = $null
$script:baseConfigText = $null
$mainError = $null
$cleanupErrors = New-Object System.Collections.ArrayList
$result = $null

try {
    Assert-True (Test-Path -LiteralPath $script:serviceBinary -PathType Leaf) `
        "winsched-service.exe is missing"
    Assert-True (Test-Path -LiteralPath $script:configPath -PathType Leaf) `
        "winsched.toml is missing"
    $service = Get-Service -Name "WinSched" -ErrorAction SilentlyContinue
    Assert-True ($null -ne $service) "WinSched service is not installed"
    $originalServiceRunning = $service.Status -ne "Stopped"
    $serviceStateCaptured = $true

    New-Item -ItemType Directory -Path $logBackupDirectory -Force | Out-Null
    Stop-ServiceUnderTest
    $originalConfigBytes = [System.IO.File]::ReadAllBytes($script:configPath)
    [System.IO.File]::WriteAllBytes($configBackupPath, $originalConfigBytes)
    $script:baseConfigText = [System.Text.Encoding]::UTF8.GetString($originalConfigBytes)
    foreach ($file in @(Get-LogFiles)) {
        Copy-Item -LiteralPath $file.FullName -Destination $logBackupDirectory
    }
    $backupCaptured = $true

    Write-Host "logging stage: disabled startup creates no normal log"
    Remove-TestLogs
    $disabledText = New-LoggingConfigText $script:baseConfigText $false 1 2
    Set-Utf8FileAtomically $script:configPath $disabledText
    Start-ServiceUnderTest
    $disabledInitial = Wait-InitialStatus $false 1 2
    $disabledLogFiles = @(Get-LogFiles)
    $disabledLogDescription = @(
        $disabledLogFiles | ForEach-Object { "$($_.Name):$($_.Length)" }
    ) -join ", "
    Assert-True ($disabledLogFiles.Count -eq 0) `
        "disabled startup created log files: $disabledLogDescription"

    Write-Host "logging stage: hot enable and hot disable"
    $servicePid = [int]$disabledInitial.service_pid
    $enabledStatus = Set-LoggingConfiguration $true 1 2 $servicePid
    Assert-True ([int]$enabledStatus.service_pid -eq $servicePid) `
        "hot enable restarted the service"
    Wait-Condition "hot-enabled logger created its active log" {
        Test-Path -LiteralPath $script:activeLogPath -PathType Leaf
    }
    $disabledStatus = Set-LoggingConfiguration $false 1 2 $servicePid
    Assert-True ([int]$disabledStatus.service_pid -eq $servicePid) `
        "hot disable restarted the service"
    Start-Sleep -Milliseconds 750
    $disabledSnapshot = Get-LogSnapshot
    Assert-True ($disabledSnapshot.Count -gt 0) `
        "hot-disable test has no baseline log to protect"

    Invoke-SchedulingChange
    Invoke-SchedulingChange
    [void](Set-LoggingConfiguration $false 2 1 $servicePid)
    Start-Sleep -Milliseconds 750
    Assert-SnapshotsEqual `
        $disabledSnapshot `
        (Get-LogSnapshot) `
        "disabled controls and hot reload"

    Stop-ServiceUnderTest
    Start-ServiceUnderTest
    $disabledRestart = Wait-InitialStatus $false 2 1
    Assert-SnapshotsEqual `
        $disabledSnapshot `
        (Get-LogSnapshot) `
        "disabled service restart"

    Write-Host "logging stage: retained two-file circular archive ring"
    $restartPid = [int]$disabledRestart.service_pid
    [void](Set-LoggingConfiguration $true 1 2 $restartPid)
    for ($cycle = 1; $cycle -le 3; $cycle++) {
        Stop-ServiceUnderTest
        if ($cycle -eq 1) {
            Remove-TestLogs
        } elseif (Test-Path -LiteralPath $script:activeLogPath) {
            Remove-Item -LiteralPath $script:activeLogPath -Force
        }
        Write-NearLimitSeed $cycle
        Start-ServiceUnderTest
        [void](Wait-InitialStatus $true 1 2)
        Wait-Condition "cycle $cycle startup rotation" {
            Test-Path -LiteralPath "$($script:activeLogPath).1" -PathType Leaf
        }
        Assert-True ((Get-SeedCycle "$($script:activeLogPath).1") -eq $cycle) `
            "archive .1 is not the newest completed cycle $cycle"
        if ($cycle -ge 2) {
            Assert-True ((Get-SeedCycle "$($script:activeLogPath).2") -eq ($cycle - 1)) `
                "archive .2 is not the prior completed cycle"
        }
    }
    Assert-True (-not (Test-Path -LiteralPath "$($script:activeLogPath).3")) `
        "retained_archives=2 left an archive .3"
    Assert-ValidJsonLines $script:activeLogPath
    Assert-ValidJsonLines "$($script:activeLogPath).1"
    Assert-ValidJsonLines "$($script:activeLogPath).2"
    foreach ($file in @(Get-LogFiles)) {
        Assert-True ($file.Length -le 1MB) `
            "$($file.Name) exceeds max_file_size_mib=1"
    }

    Write-Host "logging stage: retained_archives zero truncates active"
    $currentPid = [int](Read-Status).service_pid
    [void](Set-LoggingConfiguration $true 1 0 $currentPid)
    Wait-Condition "archive pruning after retained_archives=0" {
        -not (Test-Path -LiteralPath "$($script:activeLogPath).1") -and
            -not (Test-Path -LiteralPath "$($script:activeLogPath).2")
    }
    Stop-ServiceUnderTest
    if (Test-Path -LiteralPath $script:activeLogPath) {
        Remove-Item -LiteralPath $script:activeLogPath -Force
    }
    Write-NearLimitSeed 99
    Start-ServiceUnderTest
    [void](Wait-InitialStatus $true 1 0)
    Assert-True ($null -eq (Get-SeedCycle $script:activeLogPath)) `
        "retained_archives=0 did not truncate the oversized active log"
    Assert-True (-not (Test-Path -LiteralPath "$($script:activeLogPath).1")) `
        "retained_archives=0 created archive .1"
    Assert-ValidJsonLines $script:activeLogPath
    Assert-True ((Get-Item -LiteralPath $script:activeLogPath).Length -le 1MB) `
        "truncated active log exceeds max_file_size_mib=1"

    $result = [ordered]@{
        result = "PASS"
        status_schema = 2
        disabled_absent_file = $true
        disabled_hot_reload_byte_stable = $true
        disabled_restart_byte_stable = $true
        hot_reload_same_pid = $true
        max_file_size_mib = 1
        retained_archives_tested = @(2, 0)
        circular_rotation = $true
        complete_jsonl = $true
    }
} catch {
    $mainError = $_.Exception.ToString()
} finally {
    if ($backupCaptured) {
        $safeToRestore = $true
        try {
            Stop-ServiceUnderTest
        } catch {
            [void]$cleanupErrors.Add("Could not stop service for restore: $($_.Exception.Message)")
            $safeToRestore = $false
        }
        if ($safeToRestore) {
            try {
                if ($null -ne $originalConfigBytes) {
                    Set-FileAtomically $script:configPath $originalConfigBytes
                }
            } catch {
                [void]$cleanupErrors.Add("Could not restore original configuration: $($_.Exception.Message)")
            }
            try {
                Remove-TestLogs
                if (Test-Path -LiteralPath $logBackupDirectory -PathType Container) {
                    foreach ($file in @(Get-ChildItem -LiteralPath $logBackupDirectory -File)) {
                        Copy-Item -LiteralPath $file.FullName -Destination $DataDirectory -Force
                    }
                }
            } catch {
                [void]$cleanupErrors.Add("Could not restore original log files: $($_.Exception.Message)")
            }
        }
    }
    if ($serviceStateCaptured -and $originalServiceRunning) {
        try {
            Start-ServiceUnderTest
        } catch {
            [void]$cleanupErrors.Add("Could not restore Running service state: $($_.Exception.Message)")
        }
    }
    if ($cleanupErrors.Count -eq 0 -and
        (Test-Path -LiteralPath $backupRoot -PathType Container)) {
        Remove-Item -LiteralPath $backupRoot -Recurse -Force -ErrorAction SilentlyContinue
    } elseif ($cleanupErrors.Count -gt 0) {
        [void]$cleanupErrors.Add("Recovery backup retained at $backupRoot")
    }
}

if ($cleanupErrors.Count -gt 0) {
    $cleanupText = @($cleanupErrors) -join "; "
    if ($null -ne $mainError) {
        throw "$mainError; cleanup failures: $cleanupText"
    }
    throw "logging acceptance cleanup failures: $cleanupText"
}
if ($null -ne $mainError) {
    throw $mainError
}
[pscustomobject]$result | ConvertTo-Json -Depth 6
