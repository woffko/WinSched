[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$PackageDirectory,
    [Parameter(Mandatory = $true)]
    [string]$InteractiveUser,
    [string]$InstallDirectory = (Join-Path ([Environment]::GetFolderPath("ProgramFiles")) "WinSched"),
    [string]$DataDirectory = (Join-Path ([Environment]::GetFolderPath("CommonApplicationData")) "WinSched")
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
$acceptanceStartedUnixMs = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
$expectedInstallDirectory = [IO.Path]::GetFullPath(
    (Join-Path ([Environment]::GetFolderPath("ProgramFiles")) "WinSched")
).TrimEnd('\')
$expectedDataDirectory = [IO.Path]::GetFullPath(
    (Join-Path ([Environment]::GetFolderPath("CommonApplicationData")) "WinSched")
).TrimEnd('\')
$InstallDirectory = [IO.Path]::GetFullPath($InstallDirectory).TrimEnd('\')
$DataDirectory = [IO.Path]::GetFullPath($DataDirectory).TrimEnd('\')
if (-not [string]::Equals(
    $InstallDirectory,
    $expectedInstallDirectory,
    [StringComparison]::OrdinalIgnoreCase
)) {
    throw "Full acceptance requires the isolated VM default directory: $expectedInstallDirectory"
}
if (-not [string]::Equals(
    $DataDirectory,
    $expectedDataDirectory,
    [StringComparison]::OrdinalIgnoreCase
)) {
    throw "Full acceptance requires the isolated VM data directory: $expectedDataDirectory"
}

function Assert-True($Condition, [string]$Message) {
    if ($Condition -is [Array]) {
        throw "ASSERTION TYPE ERROR: condition is an array with $($Condition.Count) values: $Message"
    }
    if (-not [bool]$Condition) {
        throw "ASSERTION FAILED: $Message"
    }
}

function Assert-NoReservedCpuSets(
    [object[]]$CpuSetIds,
    [object[]]$ReservedCpuSetIds,
    [string]$Description
) {
    $overlap = @($CpuSetIds | Where-Object { $ReservedCpuSetIds -contains $_ })
    Assert-True ($overlap.Count -eq 0) `
        "$Description contains reserved CPU Sets: $($overlap -join ', ')"
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
        Start-Sleep -Milliseconds 250
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Timed out waiting for: $Description"
}

function Read-Status {
    $path = Join-Path $DataDirectory "status.json"
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        return $null
    }
    try {
        return Get-Content -LiteralPath $path -Raw | ConvertFrom-Json
    } catch {
        return $null
    }
}

function Read-Managed {
    $path = Join-Path $DataDirectory "managed-state.json"
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        return $null
    }
    try {
        return Get-Content -LiteralPath $path -Raw | ConvertFrom-Json
    } catch {
        return $null
    }
}

function Read-ServiceLogEvents {
    $events = New-Object Collections.Generic.List[object]
    $files = @(
        Get-ChildItem -LiteralPath $DataDirectory -Filter "winsched.log*" -File |
            Where-Object Name -Match '^winsched\.log(?:\.\d+)?$' |
            Sort-Object Name -Descending
    )
    foreach ($file in $files) {
        try {
            $stream = [IO.File]::Open(
                $file.FullName,
                [IO.FileMode]::Open,
                [IO.FileAccess]::Read,
                ([IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete)
            )
        } catch [IO.FileNotFoundException] {
            continue
        }
        try {
            $reader = [IO.StreamReader]::new($stream, [Text.Encoding]::UTF8, $true, 4096, $true)
            try {
                $text = $reader.ReadToEnd()
            } finally {
                $reader.Dispose()
            }
        } finally {
            $stream.Dispose()
        }
        $lines = @([regex]::Split($text, "\r?\n"))
        if (-not ($text.EndsWith("`n") -or $text.EndsWith("`r")) -and $lines.Count -gt 0) {
            $lines = @($lines | Select-Object -First ($lines.Count - 1))
        }
        foreach ($line in $lines) {
            if (-not [string]::IsNullOrWhiteSpace($line)) {
                $events.Add(($line | ConvertFrom-Json))
            }
        }
    }
    return $events.ToArray()
}

function Read-BackgroundManaged {
    $path = Join-Path $DataDirectory "background-state.json"
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        return $null
    }
    try {
        return Get-Content -LiteralPath $path -Raw | ConvertFrom-Json
    } catch {
        return $null
    }
}

function Test-BackgroundStateEmpty {
    $state = Read-BackgroundManaged
    return $null -eq $state -or @($state.processes).Count -eq 0
}

function Get-Inspection([int]$ProcessId) {
    $output = & (Join-Path $InstallDirectory "winsched.exe") inspect $ProcessId --json
    if ($LASTEXITCODE -ne 0) {
        throw "winsched inspect failed for PID $ProcessId"
    }
    return $output | ConvertFrom-Json
}

function Wait-ServiceState([string]$State, [int]$TimeoutSeconds = 30) {
    Wait-Condition "WinSched service state $State" {
        $service = Get-Service -Name "WinSched" -ErrorAction SilentlyContinue
        return $service -and $service.Status.ToString() -eq $State
    } $TimeoutSeconds
}

function Assert-PowerShellSyntax([string]$Path) {
    $tokens = $null
    $errors = $null
    [void][System.Management.Automation.Language.Parser]::ParseFile(
        $Path,
        [ref]$tokens,
        [ref]$errors
    )
    Assert-True ($errors.Count -eq 0) "PowerShell parser errors in $Path`: $errors"
}

function Write-Utf8NoBom([string]$Path, [string]$Value) {
    $encoding = New-Object System.Text.UTF8Encoding($false)
    $directory = Split-Path -Parent $Path
    $temporaryPath = Join-Path $directory (
        ".{0}.full-acceptance-{1}.tmp" -f `
            (Split-Path -Leaf $Path), `
            [Guid]::NewGuid().ToString("N")
    )
    $replacementBackup = "$temporaryPath.backup"
    try {
        [IO.File]::WriteAllText($temporaryPath, $Value, $encoding)
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

function Get-ReloadBaseline {
    $status = Read-Status
    Assert-True ($null -ne $status) "status is unavailable before config reload"
    return [pscustomobject]@{
        service_pid = [int]$status.service_pid
        sequence = [uint64]$status.config_reload_sequence
    }
}

function Wait-ConfigReload($Baseline, [string]$Description, [int]$TimeoutSeconds = 45) {
    Wait-Condition $Description {
        $status = Read-Status
        if ($null -eq $status -or $status.config_reload_result -ne "reloaded") {
            return $false
        }
        if ([int]$status.service_pid -ne [int]$Baseline.service_pid) {
            return [uint64]$status.config_reload_sequence -gt 0
        }
        return [uint64]$status.config_reload_sequence -gt [uint64]$Baseline.sequence
    } $TimeoutSeconds
}

function Start-InteractiveBurner(
    [string]$UserId,
    [string]$ScriptPath,
    [string]$PidPath,
    [string]$TaskName
) {
    $source = @'
param([string]$PidFile)
[IO.File]::WriteAllText(
    $PidFile,
    [Diagnostics.Process]::GetCurrentProcess().Id.ToString(),
    [Text.Encoding]::ASCII
)
while ($true) {
    [Math]::Sqrt(12345) | Out-Null
}
'@
    Write-Utf8NoBom $ScriptPath $source
    Remove-Item -LiteralPath $PidPath -Force -ErrorAction SilentlyContinue
    $arguments = "-NoProfile -NonInteractive -ExecutionPolicy Bypass -File `"$ScriptPath`" -PidFile `"$PidPath`""
    $action = New-ScheduledTaskAction -Execute "powershell.exe" -Argument $arguments
    $principal = New-ScheduledTaskPrincipal -UserId $UserId -LogonType Interactive -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet `
        -AllowStartIfOnBatteries `
        -DontStopIfGoingOnBatteries `
        -ExecutionTimeLimit ([TimeSpan]::FromMinutes(30))
    Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false -ErrorAction SilentlyContinue
    Register-ScheduledTask `
        -TaskName $TaskName `
        -Action $action `
        -Principal $principal `
        -Settings $settings | Out-Null
    Start-ScheduledTask -TaskName $TaskName | Out-Null
    Wait-Condition "interactive CPU burner PID" {
        Test-Path -LiteralPath $PidPath -PathType Leaf
    }
    $processId = [int](Get-Content -LiteralPath $PidPath -Raw)
    return Get-Process -Id $processId
}

function Start-InteractiveExecutable(
    [string]$UserId,
    [string]$Executable,
    [string]$Arguments,
    [string]$TaskName
) {
    $action = if ([string]::IsNullOrEmpty($Arguments)) {
        New-ScheduledTaskAction -Execute $Executable
    } else {
        New-ScheduledTaskAction -Execute $Executable -Argument $Arguments
    }
    $principal = New-ScheduledTaskPrincipal -UserId $UserId -LogonType Interactive -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet `
        -AllowStartIfOnBatteries `
        -DontStopIfGoingOnBatteries `
        -ExecutionTimeLimit ([TimeSpan]::FromMinutes(30))
    Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false -ErrorAction SilentlyContinue
    Register-ScheduledTask `
        -TaskName $TaskName `
        -Action $action `
        -Principal $principal `
        -Settings $settings | Out-Null
    Start-ScheduledTask -TaskName $TaskName | Out-Null
}

$serviceBinary = Join-Path $InstallDirectory "winsched-service.exe"
$cliBinary = Join-Path $InstallDirectory "winsched.exe"
$installedConfig = Join-Path $DataDirectory "winsched.toml"
$packageConfig = Join-Path $PackageDirectory "winsched.toml"
$burner = $null
$burnerTask = "WinSchedCpuBurnerAcceptance"
$trayTask = "WinSchedTraySensorAcceptance"
$windowTask = "WinSchedVisibleWindowAcceptance"
$burnerScript = Join-Path $env:PUBLIC "WinSchedCpuBurnerAcceptance.ps1"
$burnerPidFile = Join-Path $env:PUBLIC "WinSchedCpuBurnerAcceptance.pid"
$loadScript = Join-Path $env:PUBLIC "WinSchedLlcLoadAcceptance.ps1"
$loadProcessId = $null
$visibleProcessId = $null
$windowScript = Join-Path $env:PUBLIC "WinSchedVisibleWindowAcceptance.ps1"
$windowPidFile = Join-Path $env:PUBLIC "WinSchedVisibleWindowAcceptance.pid"
$productionConfig = $null
$backgroundMutationTelemetry = $null

Get-Content -LiteralPath (Join-Path $PackageDirectory "SHA256SUMS") | ForEach-Object {
    if ($_ -notmatch '^(?<hash>[0-9a-f]{64})\s+(?<file>.+)$') {
        throw "Invalid SHA256SUMS line: $_"
    }
    $path = Join-Path $PackageDirectory $Matches.file
    Assert-True (Test-Path -LiteralPath $path -PathType Leaf) "Hash target missing: $path"
    $actual = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
    Assert-True ($actual -eq $Matches.hash) "SHA-256 mismatch for $($Matches.file)"
}

try {
    foreach ($binaryName in @(
        "winsched.exe",
        "winsched-service.exe",
        "winsched-tray.exe",
        "winsched-settings.exe"
    )) {
        $binaryPath = Join-Path $InstallDirectory $binaryName
        Assert-True (Test-Path -LiteralPath $binaryPath -PathType Leaf) `
            "installed candidate is missing $binaryName"
        $version = if ($binaryName -in @("winsched.exe", "winsched-service.exe")) {
            $versionOutput = (& $binaryPath --version | Out-String).Trim()
            $match = [regex]::Match($versionOutput, '^\S+\s+(?<version>\d+\.\d+\.\d+)$')
            Assert-True $match.Success "unexpected --version output from ${binaryName}: $versionOutput"
            $match.Groups["version"].Value
        } else {
            [Diagnostics.FileVersionInfo]::GetVersionInfo($binaryPath).ProductVersion.Trim()
        }
        Assert-True ($version -eq "0.5.1") `
            "full acceptance requires installed 0.5.1, found $version in $binaryName"
    }
    Wait-ServiceState "Running"

    $service = Get-CimInstance Win32_Service -Filter "Name='WinSched'"
    Assert-True ($service.StartMode -eq "Auto") "service start mode is not Automatic"
    Assert-True ($service.StartName -eq "LocalSystem") "service account is not LocalSystem"
    Assert-True (Test-Path -LiteralPath $serviceBinary -PathType Leaf) "service binary missing"
    Assert-True (Test-Path -LiteralPath (Join-Path $InstallDirectory "winsched-tray.exe")) "tray binary missing"
    Assert-True (Test-Path -LiteralPath (Join-Path $InstallDirectory "winsched-settings.exe")) "settings binary missing"
    Assert-True (Test-Path -LiteralPath $cliBinary) "CLI binary missing"

    $trayBinary = Join-Path $InstallDirectory "winsched-tray.exe"
    Start-InteractiveExecutable $InteractiveUser $trayBinary "" $trayTask
    Wait-Condition "interactive tray sensor process" {
        @((Get-Process -Name "winsched-tray" -ErrorAction SilentlyContinue)).Count -gt 0
    }

    $startup = Join-Path ([Environment]::GetFolderPath("CommonStartup")) "WinSched Tray.lnk"
    Assert-True (Test-Path -LiteralPath $startup -PathType Leaf) "tray Startup shortcut missing"
    $startMenu = Join-Path $env:ProgramData "Microsoft\Windows\Start Menu\Programs\WinSched"
    Assert-True (Test-Path -LiteralPath (Join-Path $startMenu "WinSched.lnk")) "tray Start Menu shortcut missing"
    Assert-True (Test-Path -LiteralPath (Join-Path $startMenu "WinSched Settings.lnk")) "settings Start Menu shortcut missing"
    Assert-True (Test-Path -LiteralPath (Join-Path $startMenu "Uninstall WinSched.lnk")) "uninstall shortcut missing"

    $sddl = (& sc.exe sdshow WinSched | Out-String)
    Assert-True ($LASTEXITCODE -eq 0) "sc.exe sdshow failed"
    Assert-True ($sddl.Contains("(A;;CCLCSWRPWPLOCRRC;;;IU)")) "interactive service control ACE missing"
    $failure = (& sc.exe qfailure WinSched | Out-String)
    Assert-True ($failure -match "RESTART") "service restart failure actions missing"

    Wait-Condition "initial service heartbeat" {
        $status = Read-Status
        return $status -and $status.phase -eq "running" -and $status.configured_mode -eq "auto"
    }
    $status = Read-Status
    Assert-True ([int]$status.schema_version -eq 5) "service did not publish status schema 5"
    Assert-True ([bool]$status.scheduling_enabled) "automatic package did not start with scheduling enabled"
    Assert-True ($status.llc_domains -gt 0) "status reports no LLC domains"
    Assert-True ([bool]$status.applied_responsiveness.enabled) `
        "packaged configuration did not enable responsiveness reserve"
    Assert-True (@($status.system_reserve.reserved_physical_cores).Count -gt 0) `
        "service published no reserved physical cores"
    Assert-True (@($status.system_reserve.reserved_cpu_set_ids).Count -gt 0) `
        "service published no reserved CPU Sets"
    $reservedCpuSetIds = @($status.system_reserve.reserved_cpu_set_ids)
    $topology = & $cliBinary topology --json | ConvertFrom-Json
    foreach ($reservedCore in @($status.system_reserve.reserved_physical_cores)) {
        $siblings = @($topology.cpu_sets | Where-Object {
            [int]$_.group -eq [int]$reservedCore.group -and
                [int]$_.core_index -eq [int]$reservedCore.core_index
        })
        Assert-True ($siblings.Count -gt 0) "reserved physical core has no CPU Sets"
        Assert-True (@($siblings | Where-Object {
            $reservedCpuSetIds -notcontains $_.id
        }).Count -eq 0) "reserve split an SMT sibling pair"
    }

    Write-Host "acceptance stage: Session 0 exclusions"
    $observed = & $cliBinary processes --include-excluded --json | ConvertFrom-Json
    Assert-True ($LASTEXITCODE -eq 0) "process observation command failed"
    $unexcludedSessionZero = @($observed | Where-Object {
        $_.session_id -eq 0 -and $null -eq $_.exclusion
    })
    Assert-True ($unexcludedSessionZero.Count -eq 0) "Session 0 process escaped fixed exclusion"
    $sshd = @($observed | Where-Object {
        $_.image_name -eq "sshd.exe" -and $_.session_id -eq 0
    })
    Assert-True ($sshd.Count -gt 0) "acceptance SSH server process was not observed"
    $excludedSshd = @($sshd | Where-Object { $_.exclusion -eq "SessionZero" })
    $allSshdExcluded = $excludedSshd.Count -eq $sshd.Count
    Assert-True $allSshdExcluded "sshd.exe was not excluded as SessionZero"
    foreach ($infrastructureImage in @("svchost.exe", "explorer.exe")) {
        $infrastructure = @($observed | Where-Object { $_.image_name -eq $infrastructureImage })
        Assert-True ($infrastructure.Count -gt 0) "$infrastructureImage was not observed"
        $excludedInfrastructure = @($infrastructure | Where-Object {
            $_.exclusion -eq "SystemProcess"
        })
        Assert-True ($excludedInfrastructure.Count -eq $infrastructure.Count) `
            "$infrastructureImage escaped the fixed infrastructure exclusion"
    }

    Write-Host "acceptance stage: interactive CPU burner"
    $burner = Start-InteractiveBurner $InteractiveUser $burnerScript $burnerPidFile $burnerTask
    $burnerId = $burner.Id
    Wait-Condition "CPU burner managed by WinSched" {
        $managed = Read-Managed
        return $managed -and @($managed.processes | Where-Object { $_.key.pid -eq $burnerId }).Count -eq 1
    } 45
    $inspection = Get-Inspection $burnerId
    Assert-True (@($inspection.default_cpu_set_ids).Count -gt 0) "managed process has no CPU Set assignment"
    $observedAfterBurner = & $cliBinary processes --include-excluded --json | ConvertFrom-Json
    $burnerObservation = @($observedAfterBurner | Where-Object { $_.key.pid -eq $burnerId })
    Assert-True ($burnerObservation.Count -eq 1) "managed CPU burner observation is not unique"
    Assert-True ($burnerObservation[0].session_id -gt 0) "managed CPU burner is not in an interactive session"
    Assert-NoReservedCpuSets `
        @($inspection.default_cpu_set_ids) `
        $reservedCpuSetIds `
        "balanced burner assignment"

    Write-Host "acceptance stage: background efficiency and visible-window veto"
    $productionConfig = Get-Content -LiteralPath $packageConfig -Raw
    $baselineEfficiency = (Get-Inspection $burnerId).efficiency.state
    Assert-True ($null -ne $baselineEfficiency) "baseline process efficiency state unavailable"
    $backgroundConfig = ($productionConfig -replace `
        'all_user_processes\s*=\s*true', `
        'all_user_processes = false').TrimEnd() + @"


[[rules]]
image = "powershell.exe"
mode = "auto"
profile = "background"
"@
    $backgroundConfig = $backgroundConfig -replace `
        '(?m)(^\[background_efficiency\]\r?\n)enabled\s*=\s*false\s*$', `
        '${1}enabled = true'
    $backgroundConfig = $backgroundConfig -replace `
        '(?m)^\s*eco_qos_enabled\s*=\s*false\s*$', `
        'eco_qos_enabled = true'
    Assert-True (
        $backgroundConfig -match '(?m)(^\[background_efficiency\]\r?\n)enabled\s*=\s*true\s*$' -and
        $backgroundConfig -match '(?m)^\s*eco_qos_enabled\s*=\s*true\s*$'
    ) "background fixture did not explicitly opt into the feature and EcoQoS"
    $backgroundReloadBaseline = Get-ReloadBaseline
    Write-Utf8NoBom $installedConfig $backgroundConfig
    & $cliBinary config-check $installedConfig | Out-Null
    Assert-True ($LASTEXITCODE -eq 0) "background-efficiency fixture failed config validation"
    Wait-ConfigReload $backgroundReloadBaseline "background configuration reload receipt"
    Wait-Condition "interactive probe accepted by service" {
        $current = Read-Status
        return $current -and
            [int]$current.background_efficiency.required_probe_sessions -ge 1 -and
            [int]$current.background_efficiency.interactive_probe_sessions -ge 1
    } 45
    Wait-Condition "background efficiency applied to headless burner" {
        $state = Read-BackgroundManaged
        $inspection = Get-Inspection $burnerId
        return $state -and
            @($state.processes | Where-Object { $_.key.pid -eq $burnerId }).Count -eq 1 -and
            @($inspection.default_cpu_set_ids).Count -eq 0 -and
            $inspection.efficiency.state.eco_qos -eq "enabled" -and
            $inspection.efficiency.state.memory_priority -eq $baselineEfficiency.memory_priority
    } 45

    $windowSource = @'
param([string]$PidFile)
Add-Type -AssemblyName System.Windows.Forms
[IO.File]::WriteAllText(
    $PidFile,
    [Diagnostics.Process]::GetCurrentProcess().Id.ToString(),
    [Text.Encoding]::ASCII
)
$form = New-Object Windows.Forms.Form
$form.Text = "WinSched visible-window acceptance"
$form.ShowInTaskbar = $true
$form.WindowState = [Windows.Forms.FormWindowState]::Minimized
[void]$form.ShowDialog()
'@
    Write-Utf8NoBom $windowScript $windowSource
    Remove-Item -LiteralPath $windowPidFile -Force -ErrorAction SilentlyContinue
    $windowArguments = "-NoProfile -ExecutionPolicy Bypass -File `"$windowScript`" -PidFile `"$windowPidFile`""
    Start-InteractiveExecutable `
        $InteractiveUser `
        "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe" `
        $windowArguments `
        $windowTask
    Wait-Condition "minimized visible-window helper PID" {
        Test-Path -LiteralPath $windowPidFile -PathType Leaf
    }
    $visibleProcessId = [int](Get-Content -LiteralPath $windowPidFile -Raw)
    Wait-Condition "visible-window cohort restored" {
        $inspection = Get-Inspection $burnerId
        return $inspection.efficiency.state.eco_qos -eq $baselineEfficiency.eco_qos -and
            $inspection.efficiency.state.memory_priority -eq $baselineEfficiency.memory_priority -and
            [int](Read-Status).background_efficiency.protected_processes -ge 1
    } 45

    Stop-Process -Id $visibleProcessId -Force -ErrorAction SilentlyContinue
    $visibleProcessId = $null
    Unregister-ScheduledTask -TaskName $windowTask -Confirm:$false -ErrorAction SilentlyContinue
    Wait-Condition "background efficiency reapplied after visible window closed" {
        $inspection = Get-Inspection $burnerId
        return $inspection.efficiency.state.eco_qos -eq "enabled" -and
            $inspection.efficiency.state.memory_priority -eq $baselineEfficiency.memory_priority
    } 45

    Write-Host "acceptance stage: stale tray sensor restores owned background state"
    $interactiveTrays = @(
        Get-Process -Name "winsched-tray" -ErrorAction SilentlyContinue |
            Where-Object { $_.SessionId -gt 0 }
    )
    Assert-True ($interactiveTrays.Count -gt 0) `
        "no interactive tray process exists for the stale-sensor test"
    $interactiveTrays | Stop-Process -Force -ErrorAction Stop
    Wait-Condition "interactive tray processes stopped" {
        @(
            Get-Process -Name "winsched-tray" -ErrorAction SilentlyContinue |
                Where-Object { $_.SessionId -gt 0 }
        ).Count -eq 0
    } 15
    Wait-Condition "stale tray signal triggered background restore" {
        try {
            $inspection = Get-Inspection $burnerId
            $current = Read-Status
            return $current -and
                [int]$current.background_efficiency.interactive_probe_sessions -eq 0 -and
                $inspection.efficiency.state.eco_qos -eq $baselineEfficiency.eco_qos -and
                $inspection.efficiency.state.memory_priority -eq $baselineEfficiency.memory_priority -and
                (Test-BackgroundStateEmpty)
        } catch {
            return $false
        }
    } 45
    Start-InteractiveExecutable $InteractiveUser $trayBinary "" $trayTask
    Wait-Condition "background efficiency reapplied after tray sensor recovery" {
        try {
            $inspection = Get-Inspection $burnerId
            $current = Read-Status
            return $current -and
                [int]$current.background_efficiency.interactive_probe_sessions -ge 1 -and
                $inspection.efficiency.state.eco_qos -eq "enabled" -and
                $inspection.efficiency.state.memory_priority -eq $baselineEfficiency.memory_priority
        } catch {
            return $false
        }
    } 45

    Write-Host "acceptance stage: disable restores active background ownership"
    & $serviceBinary disable | Out-Null
    Assert-True ($LASTEXITCODE -eq 0) "disable command failed during background acceptance"
    Wait-Condition "disable restored active background state" {
        try {
            $inspection = Get-Inspection $burnerId
            $current = Read-Status
            return $current -and -not [bool]$current.scheduling_enabled -and
                $inspection.efficiency.state.eco_qos -eq $baselineEfficiency.eco_qos -and
                $inspection.efficiency.state.memory_priority -eq $baselineEfficiency.memory_priority -and
                (Test-BackgroundStateEmpty)
        } catch {
            return $false
        }
    } 30
    & $serviceBinary enable | Out-Null
    Assert-True ($LASTEXITCODE -eq 0) "enable command failed during background acceptance"
    Wait-Condition "re-enable reapplied background efficiency" {
        try {
            $inspection = Get-Inspection $burnerId
            $current = Read-Status
            return $current -and [bool]$current.scheduling_enabled -and
                $inspection.efficiency.state.eco_qos -eq "enabled" -and
                $inspection.efficiency.state.memory_priority -eq $baselineEfficiency.memory_priority
        } catch {
            return $false
        }
    } 45
    Wait-Condition "background mutation self-observability" {
        $current = Read-Status
        if ($null -eq $current -or $null -eq $current.telemetry) { return $false }
        $mutations = $current.telemetry.mutations
        return [uint64]$mutations.background_attempted -gt 0 -and
            [uint64]$mutations.background_succeeded -gt 0 -and
            [uint64]$mutations.background_attempted -eq
                ([uint64]$mutations.background_succeeded + [uint64]$mutations.background_failed)
    } 30
    $backgroundMutationTelemetry = (Read-Status).telemetry.mutations
    Assert-True ([uint64]$backgroundMutationTelemetry.background_failed -eq 0) `
        "background mutation telemetry reports failed operations"

    Write-Host "acceptance stage: graceful stop restores active background ownership"
    & $serviceBinary stop | Out-Null
    Assert-True ($LASTEXITCODE -eq 0) "stop command failed during background acceptance"
    Wait-ServiceState "Stopped"
    $stoppedEfficiency = (Get-Inspection $burnerId).efficiency.state
    Assert-True ($stoppedEfficiency.eco_qos -eq $baselineEfficiency.eco_qos) `
        "graceful stop left EcoQoS owned"
    Assert-True (
        $stoppedEfficiency.memory_priority -eq $baselineEfficiency.memory_priority
    ) "graceful stop left memory priority owned"
    Assert-True (Test-BackgroundStateEmpty) `
        "graceful stop left background ownership journal entries"
    & $serviceBinary start | Out-Null
    Assert-True ($LASTEXITCODE -eq 0) "start command failed during background acceptance"
    Wait-ServiceState "Running"
    Wait-Condition "service restart reapplied background efficiency" {
        try {
            $inspection = Get-Inspection $burnerId
            return $inspection.efficiency.state.eco_qos -eq "enabled" -and
                $inspection.efficiency.state.memory_priority -eq $baselineEfficiency.memory_priority
        } catch {
            return $false
        }
    } 45

    Write-Host "acceptance stage: forced service crash recovers active background ownership"
    $backgroundCrashPid = [int](
        Get-CimInstance Win32_Service -Filter "Name='WinSched'"
    ).ProcessId
    Stop-Process -Id $backgroundCrashPid -Force
    Wait-Condition "SCM restart after background-owned crash" {
        $serviceAfterCrash = Get-CimInstance Win32_Service -Filter "Name='WinSched'"
        $serviceAfterCrash.State -eq "Running" -and
            [int]$serviceAfterCrash.ProcessId -ne 0 -and
            [int]$serviceAfterCrash.ProcessId -ne $backgroundCrashPid
    } 90
    $backgroundRecoveryPid = [int](
        Get-CimInstance Win32_Service -Filter "Name='WinSched'"
    ).ProcessId
    Wait-Condition "background journal recovered after forced service crash" {
        try {
            $inspection = Get-Inspection $burnerId
            $state = Read-BackgroundManaged
            $current = Read-Status
            return $current -and [int]$current.service_pid -eq $backgroundRecoveryPid -and
                $state -and
                @($state.processes | Where-Object { $_.key.pid -eq $burnerId }).Count -eq 1 -and
                $inspection.efficiency.state.eco_qos -eq "enabled" -and
                $inspection.efficiency.state.memory_priority -eq $baselineEfficiency.memory_priority
        } catch {
            return $false
        }
    } 45

    Write-Host "acceptance stage: invalid config restores active background ownership"
    Write-Utf8NoBom $installedConfig "schema_version = 5`nunknown_field = true"
    Wait-Condition "invalid config rejected with background cleanup" {
        try {
            $inspection = Get-Inspection $burnerId
            $current = Read-Status
            return $current -and $current.last_error -and
                $inspection.efficiency.state.eco_qos -eq $baselineEfficiency.eco_qos -and
                $inspection.efficiency.state.memory_priority -eq $baselineEfficiency.memory_priority -and
                (Test-BackgroundStateEmpty)
        } catch {
            return $false
        }
    } 45
    $backgroundRecoveryBaseline = Get-ReloadBaseline
    Write-Utf8NoBom $installedConfig $backgroundConfig
    Wait-ConfigReload $backgroundRecoveryBaseline "valid background recovery reload receipt"
    Wait-Condition "valid background configuration recovered after rejection" {
        try {
            $inspection = Get-Inspection $burnerId
            $current = Read-Status
            return $current -and -not $current.last_error -and
                $inspection.efficiency.state.eco_qos -eq "enabled" -and
                $inspection.efficiency.state.memory_priority -eq $baselineEfficiency.memory_priority
        } catch {
            return $false
        }
    } 45

    $productionRestoreBaseline = Get-ReloadBaseline
    Write-Utf8NoBom $installedConfig $productionConfig
    Wait-ConfigReload $productionRestoreBaseline "production reload after Background rule removal"
    Wait-Condition "background efficiency restored after rule removal" {
        try {
            $inspection = Get-Inspection $burnerId
            return $inspection.efficiency.state.eco_qos -eq $baselineEfficiency.eco_qos -and
                $inspection.efficiency.state.memory_priority -eq $baselineEfficiency.memory_priority -and
                (Test-BackgroundStateEmpty)
        } catch {
            return $false
        }
    } 45

    Write-Host "acceptance stage: memory and compute workload profiles"
    $memoryConfig = ($productionConfig -replace `
        'all_user_processes\s*=\s*true', `
        'all_user_processes = false').TrimEnd() + @"


[[rules]]
image = "powershell.exe"
mode = "sticky"
profile = "memory"
"@
    $memoryReloadBaseline = Get-ReloadBaseline
    Write-Utf8NoBom $installedConfig $memoryConfig
    Wait-ConfigReload $memoryReloadBaseline "memory profile config reload receipt"
    Wait-Condition "memory profile applied to burner" {
        $current = Get-Inspection $burnerId
        $ids = @($current.default_cpu_set_ids)
        if ($ids.Count -eq 0) {
            return $false
        }
        $selected = @($topology.cpu_sets | Where-Object { $ids -contains $_.id })
        $cores = @($selected | ForEach-Object {
            "{0}:{1}" -f $_.group, $_.core_index
        } | Sort-Object -Unique)
        return $ids.Count -eq $cores.Count
    } 45
    $memoryInspection = Get-Inspection $burnerId
    $memoryCpuSetIds = @($memoryInspection.default_cpu_set_ids)
    Assert-NoReservedCpuSets $memoryCpuSetIds $reservedCpuSetIds "memory-profile assignment"
    $memorySelected = @($topology.cpu_sets | Where-Object {
        $memoryCpuSetIds -contains $_.id
    })
    $memoryPhysicalCores = @($memorySelected | ForEach-Object {
        "{0}:{1}" -f $_.group, $_.core_index
    } | Sort-Object -Unique)
    Assert-True ($memoryCpuSetIds.Count -eq $memoryPhysicalCores.Count) `
        "memory profile used more than one SMT sibling per physical core"
    Assert-True ($memoryPhysicalCores.Count -le [int](Read-Status).memory_profile_physical_cores) `
        "memory profile exceeded its adaptive physical-core width"

    $computeConfig = $memoryConfig -replace 'profile\s*=\s*"memory"', 'profile = "compute"'
    $availableComputeIds = @($topology.cpu_sets | Where-Object {
        -not $_.flags.parked -and
            -not $_.flags.realtime -and
            (-not $_.flags.allocated -or $_.flags.allocated_to_target_process) -and
            $reservedCpuSetIds -notcontains $_.id
    } | Select-Object -ExpandProperty id | Sort-Object)
    $computeReloadBaseline = Get-ReloadBaseline
    Write-Utf8NoBom $installedConfig $computeConfig
    Wait-ConfigReload $computeReloadBaseline "compute profile config reload receipt"
    Wait-Condition "compute profile applied to burner" {
        $ids = @((Get-Inspection $burnerId).default_cpu_set_ids | Sort-Object)
        return ($ids -join ',') -eq ($availableComputeIds -join ',')
    } 45
    $computeCpuSetIds = @((Get-Inspection $burnerId).default_cpu_set_ids)
    Assert-NoReservedCpuSets $computeCpuSetIds $reservedCpuSetIds "compute-profile assignment"
    Assert-True ($computeCpuSetIds.Count -ge $memoryCpuSetIds.Count) `
        "compute profile exposed fewer CPU Sets than memory profile"

    $balancedRestoreStatus = Read-Status
    $balancedRestorePid = [int]$balancedRestoreStatus.service_pid
    $balancedRestoreSequence = [uint64]$balancedRestoreStatus.config_reload_sequence
    Write-Utf8NoBom $installedConfig $productionConfig
    Wait-Condition "balanced configuration reloaded after profile acceptance" {
        $current = Read-Status
        return $current -and
            (([int]$current.service_pid -ne $balancedRestorePid -and
                [uint64]$current.config_reload_sequence -gt 0) -or
             ([int]$current.service_pid -eq $balancedRestorePid -and
                [uint64]$current.config_reload_sequence -gt $balancedRestoreSequence)) -and
            $current.config_reload_result -eq "reloaded"
    } 45
    Wait-Condition "balanced burner assignment restored" {
        @((Get-Inspection $burnerId).default_cpu_set_ids).Count -gt 0
    } 45
    Assert-NoReservedCpuSets `
        @((Get-Inspection $burnerId).default_cpu_set_ids) `
        $reservedCpuSetIds `
        "restored balanced assignment"

    Write-Host "acceptance stage: adaptive LLC move"
    $moveConfig = $productionConfig `
        -replace '(?m)^overload_threshold_bps\s*=\s*\d+', 'overload_threshold_bps = 5000' `
        -replace '(?m)^minimum_improvement_bps\s*=\s*\d+', 'minimum_improvement_bps = 0' `
        -replace '(?m)^stability_samples\s*=\s*\d+', 'stability_samples = 2' `
        -replace '(?m)^minimum_residency_ms\s*=\s*\d+', 'minimum_residency_ms = 1000' `
        -replace '(?m)^cooldown_ms\s*=\s*\d+', 'cooldown_ms = 1000'
    $moveConfig = $moveConfig -replace `
        '(?m)(^\s*\[responsiveness\]\s*\r?\n\s*)enabled\s*=\s*true', `
        '${1}enabled = false'
    Assert-True (
        $moveConfig -match '(?m)^\s*\[responsiveness\]\s*\r?\n\s*enabled\s*=\s*false\s*$'
    ) "adaptive-move fixture did not disable responsiveness reserve"
    $moveReloadBaseline = Get-ReloadBaseline
    Write-Utf8NoBom $installedConfig $moveConfig
    & $cliBinary config-check $installedConfig | Out-Null
    Assert-True ($LASTEXITCODE -eq 0) "adaptive-move fixture failed config validation"
    Wait-ConfigReload $moveReloadBaseline "adaptive-move config reload receipt"
    Wait-Condition "managed burner ready for adaptive move" {
        $entries = @((Read-Managed).processes | Where-Object { $_.key.pid -eq $burnerId })
        return $entries.Count -eq 1
    } 45
    $sourceEntry = @((Read-Managed).processes | Where-Object { $_.key.pid -eq $burnerId })
    Assert-True ($sourceEntry.Count -eq 1) "managed burner ownership entry is not unique"
    $sourceEntry = $sourceEntry[0]
    Assert-True ($null -ne $sourceEntry) "managed burner ownership entry disappeared"
    $sourceGroup = [int]$sourceEntry.placement.anchor_domain.group
    $sourceLlc = [int]$sourceEntry.placement.anchor_domain.last_level_cache_index
    $sourceSelector = "{0}:{1}" -f $sourceGroup, $sourceLlc
    Write-Utf8NoBom $loadScript "while (`$true) { [Math]::Sqrt(67890) | Out-Null }"
    $loadPowerShell = Join-Path $env:SystemRoot "System32\WindowsPowerShell\v1.0\powershell.exe"
    $loadProcess = Start-Process $loadPowerShell -ArgumentList @(
        "-NoProfile",
        "-NonInteractive",
        "-File",
        $loadScript
    ) -PassThru
    $loadProcessId = $loadProcess.Id
    & $cliBinary apply $loadProcessId --llc $sourceSelector --commit --json | Out-Null
    Assert-True ($LASTEXITCODE -eq 0) "failed to assign the fixed-LLC load process"
    Wait-Condition "adaptive burner moved away from overloaded LLC" {
        $entry = @((Read-Managed).processes | Where-Object { $_.key.pid -eq $burnerId })
        if ($entry.Count -ne 1) {
            return $false
        }
        $entry = $entry[0]
        return (
            [int]$entry.placement.anchor_domain.group -ne $sourceGroup -or
            [int]$entry.placement.anchor_domain.last_level_cache_index -ne $sourceLlc
        )
    } 60
    $movedEntry = @((Read-Managed).processes | Where-Object { $_.key.pid -eq $burnerId })[0]
    Assert-True (
        $movedEntry.placement.anchor_domain.last_level_cache_index -ne $sourceLlc -or
        $movedEntry.placement.anchor_domain.group -ne $sourceGroup
    ) `
        "adaptive controller did not change the burner LLC"
    Stop-Process -Id $loadProcessId -Force
    $loadProcessId = $null
    Write-Utf8NoBom $installedConfig $productionConfig
    Start-Sleep -Seconds 2

    & $serviceBinary disable | Out-Null
    Assert-True ($LASTEXITCODE -eq 0) "disable command failed"
    Wait-Condition "scheduling disabled and assignments cleared" {
        $current = Read-Status
        return $current -and -not $current.scheduling_enabled -and $current.managed_processes -eq 0
    }
    Assert-True (@((Get-Inspection $burnerId).default_cpu_set_ids).Count -eq 0) "disable did not clear CPU Sets"

    & $serviceBinary stop | Out-Null
    Assert-True ($LASTEXITCODE -eq 0) "stop command failed"
    Wait-ServiceState "Stopped"
    & $serviceBinary start | Out-Null
    Assert-True ($LASTEXITCODE -eq 0) "start command failed"
    Wait-ServiceState "Running"
    Wait-Condition "disabled preference restored after restart" {
        $current = Read-Status
        return $current -and $current.phase -eq "disabled" -and -not $current.scheduling_enabled
    }

    & $serviceBinary enable | Out-Null
    Assert-True ($LASTEXITCODE -eq 0) "enable command failed"
    Wait-Condition "scheduling re-enabled and burner assigned" {
        $current = Read-Status
        $managed = Read-Managed
        return $current -and $current.scheduling_enabled -and
            $managed -and @($managed.processes | Where-Object { $_.key.pid -eq $burnerId }).Count -eq 1
    } 45

    $validConfig = Get-Content -LiteralPath $packageConfig -Raw
    Write-Utf8NoBom $installedConfig "schema_version = 1`nunknown_field = true"
    Wait-Condition "invalid hot reload rejected fail-closed" {
        $current = Read-Status
        return $current -and $current.last_error -and $current.managed_processes -eq 0
    }
    Assert-True (@((Get-Inspection $burnerId).default_cpu_set_ids).Count -eq 0) "invalid config did not clear CPU Sets"
    Write-Utf8NoBom $installedConfig $validConfig
    Wait-Condition "valid configuration restored" {
        $current = Read-Status
        return $current -and $current.configured_mode -eq "auto" -and -not $current.last_error
    }
    Wait-Condition "burner reassigned after valid reload" {
        $managed = Read-Managed
        return $managed -and @($managed.processes | Where-Object { $_.key.pid -eq $burnerId }).Count -eq 1
    } 45

    $oldPid = (Get-CimInstance Win32_Service -Filter "Name='WinSched'").ProcessId
    Stop-Process -Id $oldPid -Force
    Wait-Condition "SCM automatic restart after forced termination" {
        $currentService = Get-CimInstance Win32_Service -Filter "Name='WinSched'"
        return $currentService.State -eq "Running" -and
            $currentService.ProcessId -ne 0 -and $currentService.ProcessId -ne $oldPid
    } 90
    $newPid = (Get-CimInstance Win32_Service -Filter "Name='WinSched'").ProcessId
    Wait-Condition "new service heartbeat after recovery" {
        $current = Read-Status
        return $current -and $current.service_pid -eq $newPid -and $current.phase -eq "running"
    }
    Assert-True (@((Get-Inspection $burnerId).default_cpu_set_ids).Count -gt 0) "recovery lost owned CPU Sets"

    & $serviceBinary stop | Out-Null
    Assert-True ($LASTEXITCODE -eq 0) "final graceful stop command failed"
    Wait-ServiceState "Stopped"
    Assert-True (@((Get-Inspection $burnerId).default_cpu_set_ids).Count -eq 0) "graceful stop did not clear CPU Sets"
    $finalRestartStartedUnixMs = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    & $serviceBinary start | Out-Null
    Assert-True ($LASTEXITCODE -eq 0) "final service start command failed"
    Wait-ServiceState "Running"

    Wait-Condition "schema-5 self-observability after final restart" {
        $current = Read-Status
        return $current -and $current.telemetry -and
            [uint64]$current.telemetry.evaluation.completed_total -gt 0 -and
            [uint64]$current.telemetry.mutations.placement_attempted -gt 0 -and
            [uint64]$current.telemetry.mutations.placement_succeeded -gt 0 -and
            $null -ne $current.telemetry.service_process
    } 45
    $finalStatus = Read-Status
    Assert-True ($null -ne $finalStatus.telemetry) "status self-observability is missing"
    Assert-True ([uint64]$finalStatus.telemetry.evaluation.completed_total -gt 0) `
        "evaluation telemetry did not advance"
    Assert-True ([int]$finalStatus.telemetry.evaluation.window_samples -gt 0) `
        "evaluation telemetry window is empty"
    Assert-True ([uint64]$finalStatus.telemetry.evaluation.rolling_max_us -gt 0) `
        "evaluation duration telemetry is empty"
    Assert-True ([uint64]$finalStatus.telemetry.logging.records_written -gt 0) `
        "logging telemetry did not count records"
    Assert-True ([uint64]$finalStatus.telemetry.logging.status_writes -gt 0) `
        "status-write telemetry did not advance"
    Assert-True ([uint64]$finalStatus.telemetry.logging.write_errors -eq 0) `
        "logging telemetry reports write errors"
    $placementMutations = $finalStatus.telemetry.mutations
    Assert-True ([uint64]$placementMutations.placement_attempted -gt 0) `
        "placement mutation telemetry did not advance"
    Assert-True (
        [uint64]$placementMutations.placement_attempted -eq
        ([uint64]$placementMutations.placement_succeeded + [uint64]$placementMutations.placement_failed)
    ) "placement mutation outcome counters are inconsistent"
    Assert-True ([uint64]$placementMutations.placement_succeeded -gt 0) `
        "placement mutation telemetry has no successful operation"
    Assert-True ($null -ne $finalStatus.telemetry.service_process) `
        "service process telemetry is unavailable"
    Assert-True ([uint64]$finalStatus.telemetry.service_process.uptime_ms -gt 0) `
        "service uptime telemetry is empty"
    Assert-True ([uint64]$finalStatus.telemetry.service_process.cpu_time_100ns -gt 0) `
        "service CPU telemetry is empty"
    Assert-True ([uint64]$finalStatus.telemetry.service_process.working_set_bytes -gt 0) `
        "service working-set telemetry is empty"

    Wait-Condition "one complete 60-second Normal decision window" {
        $complete = @(Read-ServiceLogEvents | Where-Object {
            $_.event -eq "decision_summary" -and
                $_.flush_reason -eq "periodic" -and
                [uint64]$_.timestamp_ms -ge [uint64]$finalRestartStartedUnixMs -and
                [uint64]$_.window_duration_ms -ge 60000
        })
        $complete.Count -gt 0
    } 75

    $logEvents = @(Read-ServiceLogEvents | Where-Object {
        [uint64]$_.timestamp_ms -ge [uint64]$acceptanceStartedUnixMs
    })
    $decisionSummaries = @($logEvents | Where-Object event -eq "decision_summary")
    Assert-True ($decisionSummaries.Count -gt 0) `
        "normal logging emitted no periodic decision summary"
    $periodicSummaries = @($decisionSummaries | Where-Object flush_reason -eq "periodic")
    Assert-True ($periodicSummaries.Count -gt 0) `
        "normal logging emitted no interval-complete decision summary"
    foreach ($summary in $periodicSummaries) {
        Assert-True ([uint64]$summary.window_duration_ms -ge 60000) `
            "normal logging emitted a periodic summary before 60 seconds"
        Assert-True ([uint64]$summary.decisions -gt 0) `
            "normal logging emitted an empty periodic summary"
    }
    $acceptanceElapsedMs = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() - $acceptanceStartedUnixMs
    $maximumPeriodicSummaries = [int][Math]::Ceiling($acceptanceElapsedMs / 60000.0) + 4
    Assert-True ($periodicSummaries.Count -le $maximumPeriodicSummaries) `
        "normal logging emitted too many periodic summaries: $($periodicSummaries.Count)"
    foreach ($summary in $decisionSummaries) {
        $summaryFields = @($summary.PSObject.Properties.Name)
        Assert-True ($summaryFields -notcontains "process" -and $summaryFields -notcontains "image") `
            "decision summary leaked a process identity"
    }
    $rawDecisions = @($logEvents | Where-Object event -eq "decision")
    Assert-True ($rawDecisions.Count -gt 0) `
        "normal logging lost mutation-shaped decisions"
    foreach ($rawDecision in $rawDecisions) {
        $actionJson = $rawDecision.action | ConvertTo-Json -Compress
        Assert-True ($actionJson -match '^\{"(Assign|Move|Clear)"') `
            "normal logging emitted a raw no-op decision: $actionJson"
    }

    [pscustomobject]@{
        result = "PASS"
        windows_version = [Environment]::OSVersion.VersionString
        package = Split-Path -Leaf $PackageDirectory
        service_pid_after_recovery = $newPid
        llc_domains = (Read-Status).llc_domains
        reserved_physical_cores = @((Read-Status).system_reserve.reserved_physical_cores).Count
        reserved_cpu_sets = @((Read-Status).system_reserve.reserved_cpu_set_ids).Count
        memory_profile_cpu_sets = $memoryCpuSetIds.Count
        compute_profile_cpu_sets = $computeCpuSetIds.Count
        workload_profiles = "PASS"
        background_efficiency = "PASS"
        background_tray_stale_restore = "PASS"
        background_disable_restore = "PASS"
        background_stop_restore = "PASS"
        background_crash_recovery = "PASS"
        background_invalid_config_restore = "PASS"
        normal_decision_coalescing = "PASS"
        controller_self_observability = "PASS"
        background_mutations_observed = [ordered]@{
            attempted = [uint64]$backgroundMutationTelemetry.background_attempted
            succeeded = [uint64]$backgroundMutationTelemetry.background_succeeded
            failed = [uint64]$backgroundMutationTelemetry.background_failed
        }
        install_directory = $InstallDirectory
        data_directory = $DataDirectory
    } | ConvertTo-Json -Depth 4
} finally {
    if ($burner -and -not $burner.HasExited) {
        Stop-Process -Id $burner.Id -Force -ErrorAction SilentlyContinue
    }
    if ($loadProcessId) {
        Stop-Process -Id $loadProcessId -Force -ErrorAction SilentlyContinue
    }
    if ($visibleProcessId) {
        Stop-Process -Id $visibleProcessId -Force -ErrorAction SilentlyContinue
    }
    if ($productionConfig -and (Test-Path -LiteralPath $installedConfig)) {
        Write-Utf8NoBom $installedConfig $productionConfig
    }
    Unregister-ScheduledTask -TaskName $burnerTask -Confirm:$false -ErrorAction SilentlyContinue
    Unregister-ScheduledTask -TaskName $trayTask -Confirm:$false -ErrorAction SilentlyContinue
    Unregister-ScheduledTask -TaskName $windowTask -Confirm:$false -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $burnerScript -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $burnerPidFile -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $loadScript -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $windowScript -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $windowPidFile -Force -ErrorAction SilentlyContinue
}
