[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$OldSetupPath,
    [Parameter(Mandatory = $true)]
    [string]$CurrentSetupPath,
    [Parameter(Mandatory = $true)]
    [string]$CurrentPayloadDirectory,
    [Parameter(Mandatory = $true)]
    [string]$WorkDirectory,
    [string]$InstallDirectory = "$env:ProgramFiles\WinSched",
    [string]$DataDirectory = "$env:ProgramData\WinSched"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Assert-True([bool]$Condition, [string]$Code) {
    if (-not $Condition) { throw $Code }
}

function Wait-Condition(
    [string]$Code,
    [scriptblock]$Condition,
    [int]$TimeoutSeconds = 90
) {
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        if (& $Condition) { return }
        Start-Sleep -Milliseconds 250
    } while ([DateTime]::UtcNow -lt $deadline)
    throw $Code
}

function Get-Sha256([string]$Path) {
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Read-Status {
    $path = Join-Path $DataDirectory "status.json"
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { return $null }
    try { return Get-Content -LiteralPath $path -Raw | ConvertFrom-Json } catch { return $null }
}

function Get-ConsoleVersion([string]$Name) {
    $path = Join-Path $InstallDirectory $Name
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { return $null }
    $value = (& $path --version | Out-String).Trim()
    if ($LASTEXITCODE -ne 0) { return $null }
    return $value
}

function Get-GuiVersion([string]$Name) {
    $path = Join-Path $InstallDirectory $Name
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { return $null }
    return (Get-Item -LiteralPath $path).VersionInfo.ProductVersion.Trim()
}

function Set-FileAtomically([string]$Path, [byte[]]$Bytes) {
    $directory = Split-Path -Parent $Path
    New-Item -ItemType Directory -Path $directory -Force | Out-Null
    $temporaryPath = Join-Path $directory (
        ".{0}.v051-upgrade-{1}.tmp" -f `
            (Split-Path -Leaf $Path), [Guid]::NewGuid().ToString("N")
    )
    $replacementBackup = "$temporaryPath.backup"
    try {
        [IO.File]::WriteAllBytes($temporaryPath, $Bytes)
        if (Test-Path -LiteralPath $Path -PathType Leaf) {
            [IO.File]::Replace($temporaryPath, $Path, $replacementBackup, $true)
        } else {
            [IO.File]::Move($temporaryPath, $Path)
        }
    } finally {
        Remove-Item -LiteralPath $temporaryPath -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $replacementBackup -Force -ErrorAction SilentlyContinue
    }
}

function Invoke-Setup([string]$Path) {
    $process = Start-Process `
        -FilePath $Path `
        -ArgumentList @("/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART") `
        -Wait `
        -PassThru
    Assert-True ($process.ExitCode -eq 0) "V051_UPGRADE_SETUP_FAILED"
    Wait-Condition "V051_UPGRADE_SERVICE_START_TIMEOUT" {
        $service = Get-Service WinSched -ErrorAction SilentlyContinue
        $null -ne $service -and $service.Status -eq "Running"
    }
}

function Invoke-PurgeUninstall {
    $uninstallers = @(
        Get-ChildItem -LiteralPath $InstallDirectory -Filter "unins*.exe" -File |
            Where-Object Name -Match '^unins\d+\.exe$' |
            Sort-Object LastWriteTimeUtc -Descending
    )
    Assert-True ($uninstallers.Count -gt 0) "V051_UPGRADE_UNINSTALLER_MISSING"
    $process = Start-Process `
        -FilePath $uninstallers[0].FullName `
        -ArgumentList @(
            "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/PURGEDATA"
        ) `
        -Wait `
        -PassThru
    Assert-True ($process.ExitCode -eq 0) "V051_UPGRADE_UNINSTALL_FAILED"
    Wait-Condition "V051_UPGRADE_SERVICE_REMOVE_TIMEOUT" {
        $null -eq (Get-Service WinSched -ErrorAction SilentlyContinue)
    }
    Wait-Condition "V051_UPGRADE_FILES_REMOVE_TIMEOUT" {
        -not (Test-Path -LiteralPath $InstallDirectory)
    }
    Assert-True (-not (Test-Path -LiteralPath $DataDirectory)) `
        "V051_UPGRADE_PURGE_RETAINED_DATA"
}

function Get-PayloadHashes {
    $result = @{}
    Get-Content -LiteralPath (Join-Path $CurrentPayloadDirectory "SHA256SUMS") |
        ForEach-Object {
            Assert-True (
                $_ -match '^(?<hash>[0-9a-f]{64})\s{2}(?<name>[A-Za-z0-9._-]+)$'
            ) "V051_UPGRADE_PAYLOAD_MANIFEST_INVALID"
            $result[$Matches.name] = $Matches.hash
        }
    return $result
}

function Test-PayloadHashes {
    $payloadHashes = Get-PayloadHashes
    foreach ($name in @(
        "winsched.exe",
        "winsched-service.exe",
        "winsched-monitor.exe",
        "winsched-tray.exe",
        "winsched-settings.exe",
        "README.md"
    )) {
        $path = Join-Path $InstallDirectory $name
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { return $false }
        if ((Get-Sha256 $path) -ne $payloadHashes[$name]) { return $false }
    }
    return $true
}

function Write-Result($Value) {
    $path = Join-Path $WorkDirectory "current-upgrade-from-v051-result.json"
    [IO.File]::WriteAllText(
        $path,
        ([pscustomobject]$Value | ConvertTo-Json -Depth 10) + "`n",
        [Text.UTF8Encoding]::new($false)
    )
}

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
Assert-True ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) `
    "V051_UPGRADE_ELEVATION_REQUIRED"

$configPath = Join-Path $DataDirectory "winsched.toml"
$serviceBinary = Join-Path $InstallDirectory "winsched-service.exe"
$originalConfigBytes = $null
$originalConfigHash = $null
$originalScheduling = $null
$originalMode = $null
$originalLogging = $null
$mainError = $null
$restoreError = $null
$upgradeResult = $null
$restore = [ordered]@{
    current_setup_installed = $false
    config_bytes_restored = $false
    scheduling_restored = $false
    service_running = $false
    payload_hashes_match = $false
}

New-Item -ItemType Directory -Path $WorkDirectory -Force | Out-Null
try {
    foreach ($path in @(
        $OldSetupPath,
        $CurrentSetupPath,
        (Join-Path $CurrentPayloadDirectory "SHA256SUMS")
    )) {
        Assert-True (Test-Path -LiteralPath $path -PathType Leaf) `
            "V051_UPGRADE_INPUT_MISSING"
    }
    Assert-True ((Get-ConsoleVersion "winsched.exe") -eq "winsched 0.6.0") `
        "V051_UPGRADE_CURRENT_PREREQUISITE"
    $initialStatus = Read-Status
    Assert-True ($null -ne $initialStatus -and $null -eq $initialStatus.last_error) `
        "V051_UPGRADE_CURRENT_STATUS"
    $originalScheduling = [bool]$initialStatus.scheduling_enabled
    $originalMode = [string]$initialStatus.configured_mode
    $originalLogging = [string]$initialStatus.applied_logging.level
    $originalConfigBytes = [IO.File]::ReadAllBytes($configPath)
    $originalConfigHash = Get-Sha256 $configPath
    [IO.File]::WriteAllBytes(
        (Join-Path $WorkDirectory "original-current-config.bin"),
        $originalConfigBytes
    )

    Invoke-PurgeUninstall
    Invoke-Setup $OldSetupPath
    Assert-True ((Get-ConsoleVersion "winsched.exe") -eq "winsched 0.5.1") `
        "V051_UPGRADE_OLD_CLI_VERSION"
    Assert-True ((Get-ConsoleVersion "winsched-service.exe") -eq "winsched-service 0.5.1") `
        "V051_UPGRADE_OLD_SERVICE_VERSION"
    Assert-True ((Get-GuiVersion "winsched-tray.exe") -eq "0.5.1") `
        "V051_UPGRADE_OLD_TRAY_VERSION"
    Assert-True ((Get-GuiVersion "winsched-settings.exe") -eq "0.5.1") `
        "V051_UPGRADE_OLD_SETTINGS_VERSION"
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $InstallDirectory "winsched-monitor.exe"))) `
        "V051_UPGRADE_OLD_MONITOR_PRESENT"

    $oldStatus = Read-Status
    Assert-True ($null -ne $oldStatus -and [int]$oldStatus.schema_version -eq 5) `
        "V051_UPGRADE_OLD_STATUS_SCHEMA"
    Assert-True ($null -eq $oldStatus.last_error) "V051_UPGRADE_OLD_STATUS_ERROR"
    $oldScheduling = [bool]$oldStatus.scheduling_enabled
    $oldMode = [string]$oldStatus.configured_mode
    $oldLogging = [string]$oldStatus.applied_logging.level

    $marker = "# v051-upgrade-preserve-$([Guid]::NewGuid().ToString('N'))"
    $oldText = Get-Content -LiteralPath $configPath -Raw
    $markedBytes = [Text.UTF8Encoding]::new($false).GetBytes(
        $oldText.TrimEnd([char[]]"`r`n") + "`r`n$marker`r`n"
    )
    $oldSequence = [uint64]$oldStatus.config_reload_sequence
    Set-FileAtomically $configPath $markedBytes
    $configHashBefore = Get-Sha256 $configPath
    Wait-Condition "V051_UPGRADE_OLD_MARKER_RELOAD_TIMEOUT" {
        $status = Read-Status
        $null -ne $status -and
            [uint64]$status.config_reload_sequence -gt $oldSequence -and
            [string]$status.config_reload_result -eq "reloaded" -and
            $null -eq $status.last_error
    }

    $currentSetupHash = Get-Sha256 $CurrentSetupPath
    Invoke-Setup $CurrentSetupPath
    Assert-True ((Get-ConsoleVersion "winsched.exe") -eq "winsched 0.6.0") `
        "V051_UPGRADE_CURRENT_CLI_VERSION"
    Assert-True ((Get-ConsoleVersion "winsched-service.exe") -eq "winsched-service 0.6.0") `
        "V051_UPGRADE_CURRENT_SERVICE_VERSION"
    Assert-True ((Get-GuiVersion "winsched-monitor.exe") -eq "0.6.0") `
        "V051_UPGRADE_CURRENT_MONITOR_VERSION"
    Assert-True ((Get-Sha256 $configPath) -eq $configHashBefore) `
        "V051_UPGRADE_CONFIG_CHANGED"
    Assert-True ((Get-Content -LiteralPath $configPath -Raw).Contains($marker)) `
        "V051_UPGRADE_MARKER_REMOVED"

    Wait-Condition "V051_UPGRADE_CURRENT_STATUS_TIMEOUT" {
        $status = Read-Status
        $null -ne $status -and
            [int]$status.schema_version -eq 5 -and
            [bool]$status.scheduling_enabled -eq $oldScheduling -and
            [string]$status.configured_mode -eq $oldMode -and
            [string]$status.applied_logging.level -eq $oldLogging -and
            $null -eq $status.last_error
    }
    Assert-True (Test-PayloadHashes) "V051_UPGRADE_PAYLOAD_HASH_MISMATCH"
    foreach ($shortcut in @(
        "$env:ProgramData\Microsoft\Windows\Start Menu\Programs\Startup\WinSched Tray.lnk",
        "$env:ProgramData\Microsoft\Windows\Start Menu\Programs\WinSched\WinSched Settings.lnk",
        "$env:ProgramData\Microsoft\Windows\Start Menu\Programs\WinSched\WinSched Process Monitor.lnk"
    )) {
        Assert-True (Test-Path -LiteralPath $shortcut -PathType Leaf) `
            "V051_UPGRADE_SHORTCUT_MISSING"
    }

    $upgradeResult = [ordered]@{
        result = "PASS"
        previous_setup_sha256 = Get-Sha256 $OldSetupPath
        current_setup_sha256 = $currentSetupHash
        previous_version = "0.5.1"
        installed_version = "0.6.0"
        config_sha256_before = $configHashBefore
        config_sha256_after = Get-Sha256 $configPath
        config_byte_identical = $true
        scheduling_preserved = $true
        mode_preserved = $true
        logging_level_preserved = $true
        five_payload_hashes_match = $true
        monitor_installed = $true
        shortcuts_preserved_or_created = $true
        service_running = $true
        status_schema = 5
    }
} catch {
    $mainError = [string]$_.Exception.Message
} finally {
    if ($null -ne $originalConfigBytes) {
        try {
            if ((Get-ConsoleVersion "winsched.exe") -ne "winsched 0.6.0") {
                Invoke-Setup $CurrentSetupPath
            }
            $restore.current_setup_installed = `
                (Get-ConsoleVersion "winsched.exe") -eq "winsched 0.6.0"
            $beforeRestore = Read-Status
            $beforePid = if ($null -ne $beforeRestore) { [int]$beforeRestore.service_pid } else { 0 }
            $beforeSequence = if ($null -ne $beforeRestore) {
                [uint64]$beforeRestore.config_reload_sequence
            } else { 0 }
            Set-FileAtomically $configPath $originalConfigBytes
            $restore.config_bytes_restored = (Get-Sha256 $configPath) -eq $originalConfigHash
            Wait-Condition "V051_UPGRADE_RESTORE_CONFIG_TIMEOUT" {
                $status = Read-Status
                if ($null -eq $status -or $null -ne $status.last_error) { return $false }
                $fresh = if ([int]$status.service_pid -eq $beforePid) {
                    [uint64]$status.config_reload_sequence -gt $beforeSequence
                } else {
                    [uint64]$status.config_reload_sequence -gt 0
                }
                $fresh -and
                    [string]$status.configured_mode -eq $originalMode -and
                    [string]$status.applied_logging.level -eq $originalLogging
            }
            $current = Read-Status
            if ([bool]$current.scheduling_enabled -ne $originalScheduling) {
                $command = if ($originalScheduling) { "enable" } else { "disable" }
                & $serviceBinary $command | Out-Null
                Assert-True ($LASTEXITCODE -eq 0) `
                    "V051_UPGRADE_RESTORE_SCHEDULING_COMMAND"
            }
            Wait-Condition "V051_UPGRADE_RESTORE_SCHEDULING_TIMEOUT" {
                [bool](Read-Status).scheduling_enabled -eq $originalScheduling
            }
            $restore.scheduling_restored = $true
            $restore.service_running = (Get-Service WinSched).Status -eq "Running"
            $restore.payload_hashes_match = Test-PayloadHashes
        } catch {
            $restoreError = [string]$_.Exception.Message
        }
    }
    if ($null -eq $upgradeResult) {
        $upgradeResult = [ordered]@{
            result = "FAIL"
            error = $mainError
        }
    }
    $upgradeResult["original_state_restored"] = `
        $restore.current_setup_installed -and
        $restore.config_bytes_restored -and
        $restore.scheduling_restored -and
        $restore.service_running -and
        $restore.payload_hashes_match
    $upgradeResult["restore"] = $restore
    $upgradeResult["restore_error"] = $restoreError
    if (-not [bool]$upgradeResult.original_state_restored) {
        $upgradeResult.result = "FAIL"
    }
    Write-Result $upgradeResult
}

[pscustomobject]$upgradeResult | ConvertTo-Json -Depth 10
if ([string]$upgradeResult.result -ne "PASS") { exit 1 }
