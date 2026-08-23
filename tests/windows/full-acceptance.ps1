[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$PackageDirectory,
    [Parameter(Mandatory = $true)]
    [string]$InteractiveUser,
    [string]$InstallDirectory = "$env:ProgramData\WinSched"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Assert-True($Condition, [string]$Message) {
    if ($Condition -is [Array]) {
        throw "ASSERTION TYPE ERROR: condition is an array with $($Condition.Count) values: $Message"
    }
    if (-not [bool]$Condition) {
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
        Start-Sleep -Milliseconds 250
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Timed out waiting for: $Description"
}

function Read-Status {
    $path = Join-Path $InstallDirectory "status.json"
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
    $path = Join-Path $InstallDirectory "managed-state.json"
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        return $null
    }
    try {
        return Get-Content -LiteralPath $path -Raw | ConvertFrom-Json
    } catch {
        return $null
    }
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
    [IO.File]::WriteAllText($Path, $Value, $encoding)
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
        -ExecutionTimeLimit ([TimeSpan]::FromMinutes(5))
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

$serviceBinary = Join-Path $InstallDirectory "winsched-service.exe"
$cliBinary = Join-Path $InstallDirectory "winsched.exe"
$installedConfig = Join-Path $InstallDirectory "winsched.toml"
$packageConfig = Join-Path $PackageDirectory "winsched.toml"
$burner = $null
$burnerTask = "WinSchedCpuBurnerAcceptance"
$burnerScript = Join-Path $env:PUBLIC "WinSchedCpuBurnerAcceptance.ps1"
$burnerPidFile = Join-Path $env:PUBLIC "WinSchedCpuBurnerAcceptance.pid"
$loadScript = Join-Path $env:PUBLIC "WinSchedLlcLoadAcceptance.ps1"
$loadProcessId = $null
$productionConfig = $null

Assert-PowerShellSyntax (Join-Path $PackageDirectory "install.ps1")
Assert-PowerShellSyntax (Join-Path $PackageDirectory "uninstall.ps1")

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
    & (Join-Path $PackageDirectory "install.ps1") `
        -InstallDirectory $InstallDirectory `
        -Configuration $packageConfig `
        -NoTrayLaunch
    Assert-True ($LASTEXITCODE -eq 0) "installer returned exit code $LASTEXITCODE"
    Wait-ServiceState "Running"

    $service = Get-CimInstance Win32_Service -Filter "Name='WinSched'"
    Assert-True ($service.StartMode -eq "Auto") "service start mode is not Automatic"
    Assert-True ($service.StartName -eq "LocalSystem") "service account is not LocalSystem"
    Assert-True (Test-Path -LiteralPath $serviceBinary -PathType Leaf) "service binary missing"
    Assert-True (Test-Path -LiteralPath (Join-Path $InstallDirectory "winsched-tray.exe")) "tray binary missing"
    Assert-True (Test-Path -LiteralPath (Join-Path $InstallDirectory "winsched-settings.exe")) "settings binary missing"
    Assert-True (Test-Path -LiteralPath $cliBinary) "CLI binary missing"

    $startup = Join-Path ([Environment]::GetFolderPath("CommonStartup")) "WinSched Tray.lnk"
    Assert-True (Test-Path -LiteralPath $startup -PathType Leaf) "tray Startup shortcut missing"
    $startMenu = Join-Path $env:ProgramData "Microsoft\Windows\Start Menu\Programs\WinSched"
    Assert-True (Test-Path -LiteralPath (Join-Path $startMenu "WinSched Tray.lnk")) "tray Start Menu shortcut missing"
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
    Assert-True ([bool]$status.scheduling_enabled) "automatic package did not start with scheduling enabled"
    Assert-True ($status.llc_domains -gt 0) "status reports no LLC domains"

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

    Write-Host "acceptance stage: adaptive LLC move"
    $productionConfig = Get-Content -LiteralPath $packageConfig -Raw
    $moveConfig = $productionConfig `
        -replace 'overload_threshold_bps\s*=\s*\d+', 'overload_threshold_bps = 5000' `
        -replace 'minimum_improvement_bps\s*=\s*\d+', 'minimum_improvement_bps = 0' `
        -replace 'stability_samples\s*=\s*\d+', 'stability_samples = 2' `
        -replace 'minimum_residency_ms\s*=\s*\d+', 'minimum_residency_ms = 1000' `
        -replace 'cooldown_ms\s*=\s*\d+', 'cooldown_ms = 1000'
    Write-Utf8NoBom $installedConfig $moveConfig
    Start-Sleep -Seconds 2
    $sourceEntry = @((Read-Managed).processes | Where-Object { $_.key.pid -eq $burnerId })
    Assert-True ($sourceEntry.Count -eq 1) "managed burner ownership entry is not unique"
    $sourceEntry = $sourceEntry[0]
    Assert-True ($null -ne $sourceEntry) "managed burner ownership entry disappeared"
    $sourceGroup = [int]$sourceEntry.domain.group
    $sourceLlc = [int]$sourceEntry.domain.last_level_cache_index
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
            [int]$entry.domain.group -ne $sourceGroup -or
            [int]$entry.domain.last_level_cache_index -ne $sourceLlc
        )
    } 60
    $movedEntry = @((Read-Managed).processes | Where-Object { $_.key.pid -eq $burnerId })[0]
    Assert-True ($movedEntry.domain.last_level_cache_index -ne $sourceLlc -or $movedEntry.domain.group -ne $sourceGroup) `
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
    } 30
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
    & $serviceBinary start | Out-Null
    Assert-True ($LASTEXITCODE -eq 0) "final service start command failed"
    Wait-ServiceState "Running"

    [pscustomobject]@{
        result = "PASS"
        windows_version = [Environment]::OSVersion.VersionString
        package = Split-Path -Leaf $PackageDirectory
        service_pid_after_recovery = $newPid
        llc_domains = (Read-Status).llc_domains
        install_directory = $InstallDirectory
    } | ConvertTo-Json -Depth 4
} finally {
    if ($burner -and -not $burner.HasExited) {
        Stop-Process -Id $burner.Id -Force -ErrorAction SilentlyContinue
    }
    if ($loadProcessId) {
        Stop-Process -Id $loadProcessId -Force -ErrorAction SilentlyContinue
    }
    if ($productionConfig -and (Test-Path -LiteralPath $installedConfig)) {
        Write-Utf8NoBom $installedConfig $productionConfig
    }
    Unregister-ScheduledTask -TaskName $burnerTask -Confirm:$false -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $burnerScript -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $burnerPidFile -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $loadScript -Force -ErrorAction SilentlyContinue
}
