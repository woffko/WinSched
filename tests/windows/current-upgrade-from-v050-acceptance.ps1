[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$OldSetupPath,
    [Parameter(Mandatory = $true)]
    [string]$CurrentSetupPath,
    [Parameter(Mandatory = $true)]
    [string]$CurrentPayloadDirectory,
    [Parameter(Mandatory = $true)]
    [string]$TestDirectory,
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

function Wait-Condition([string]$Code, [scriptblock]$Condition, [int]$TimeoutSeconds = 90) {
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

function Set-FileAtomically([string]$Path, [byte[]]$Bytes) {
    $directory = Split-Path -Parent $Path
    New-Item -ItemType Directory -Path $directory -Force | Out-Null
    $temporaryPath = Join-Path $directory (
        ".{0}.v050-upgrade-{1}.tmp" -f `
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
    Assert-True ($process.ExitCode -eq 0) "V050_UPGRADE_SETUP_FAILED"
    Wait-Condition "V050_UPGRADE_SERVICE_START_TIMEOUT" {
        $service = Get-Service WinSched -ErrorAction SilentlyContinue
        $null -ne $service -and $service.Status -eq "Running"
    }
}

function Invoke-PurgeUninstall {
    $uninstaller = @(
        Get-ChildItem -LiteralPath $InstallDirectory -Filter "unins*.exe" -File |
            Where-Object Name -Match '^unins\d+\.exe$' |
            Sort-Object LastWriteTimeUtc -Descending
    )[0].FullName
    Assert-True (Test-Path -LiteralPath $uninstaller -PathType Leaf) `
        "V050_UPGRADE_UNINSTALLER_MISSING"
    $process = Start-Process `
        -FilePath $uninstaller `
        -ArgumentList @(
            "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/PURGEDATA"
        ) `
        -Wait `
        -PassThru
    Assert-True ($process.ExitCode -eq 0) "V050_UPGRADE_UNINSTALL_FAILED"
    Wait-Condition "V050_UPGRADE_SERVICE_REMOVE_TIMEOUT" {
        $null -eq (Get-Service WinSched -ErrorAction SilentlyContinue)
    }
    Wait-Condition "V050_UPGRADE_FILES_REMOVE_TIMEOUT" {
        -not (Test-Path -LiteralPath $InstallDirectory)
    }
    Assert-True (-not (Test-Path -LiteralPath $DataDirectory)) `
        "V050_UPGRADE_PURGE_RETAINED_DATA"
}

function Get-ConsoleVersion {
    $path = Join-Path $InstallDirectory "winsched.exe"
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { return $null }
    $value = (& $path --version | Out-String).Trim()
    if ($LASTEXITCODE -ne 0) { return $null }
    return $value
}

function Get-PayloadHashes {
    $result = @{}
    Get-Content -LiteralPath (Join-Path $CurrentPayloadDirectory "SHA256SUMS") |
        ForEach-Object {
            Assert-True (
                $_ -match '^(?<hash>[0-9a-f]{64})\s{2}(?<name>[A-Za-z0-9._-]+)$'
            ) "V050_UPGRADE_PAYLOAD_MANIFEST_INVALID"
            $result[$Matches.name] = $Matches.hash
        }
    return $result
}

function Write-Result($Value) {
    $path = Join-Path $WorkDirectory "current-upgrade-from-v050-result.json"
    [IO.File]::WriteAllText(
        $path,
        ([pscustomobject]$Value | ConvertTo-Json -Depth 10) + "`n",
        [Text.UTF8Encoding]::new($false)
    )
}

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
Assert-True ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) `
    "V050_UPGRADE_ELEVATION_REQUIRED"

$configPath = Join-Path $DataDirectory "winsched.toml"
$serviceBinary = Join-Path $InstallDirectory "winsched-service.exe"
$upgradeOutput = Join-Path $WorkDirectory "inner-upgrade"
$originalConfigBytes = $null
$originalConfigHash = $null
$originalScheduling = $null
$originalMode = $null
$originalLogging = $null
$upgradeReceipt = $null
$mainError = $null
$restoreError = $null
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
        (Join-Path $CurrentPayloadDirectory "SHA256SUMS"),
        (Join-Path $TestDirectory "gui-upgrade-acceptance.ps1")
    )) {
        Assert-True (Test-Path -LiteralPath $path -PathType Leaf) `
            "V050_UPGRADE_INPUT_MISSING"
    }
    Assert-True ((Get-ConsoleVersion) -eq "winsched 0.6.0") `
        "V050_UPGRADE_CURRENT_PREREQUISITE"
    $initialStatus = Read-Status
    Assert-True ($null -ne $initialStatus -and $null -eq $initialStatus.last_error) `
        "V050_UPGRADE_CURRENT_STATUS"
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
    Assert-True ((Get-ConsoleVersion) -eq "winsched 0.5.0") `
        "V050_UPGRADE_OLD_INSTALL_PREREQUISITE"

    New-Item -ItemType Directory -Path $upgradeOutput -Force | Out-Null
    $powershell = "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe"
    $arguments = @(
        '-NoLogo', '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass',
        '-File', ('"{0}"' -f (Join-Path $TestDirectory "gui-upgrade-acceptance.ps1")),
        '-SetupPath', ('"{0}"' -f $CurrentSetupPath),
        '-PayloadDirectory', ('"{0}"' -f $CurrentPayloadDirectory),
        '-OutputDirectory', ('"{0}"' -f $upgradeOutput)
    ) -join ' '
    $upgrade = Start-Process `
        -FilePath $powershell `
        -ArgumentList $arguments `
        -Wait `
        -PassThru
    $upgradeResultPath = Join-Path $upgradeOutput "gui-upgrade-result.json"
    Assert-True (Test-Path -LiteralPath $upgradeResultPath -PathType Leaf) `
        "V050_UPGRADE_RECEIPT_MISSING"
    $upgradeReceipt = Get-Content -LiteralPath $upgradeResultPath -Raw | ConvertFrom-Json
    Assert-True ($upgrade.ExitCode -eq 0 -and [string]$upgradeReceipt.result -eq "PASS") `
        "V050_UPGRADE_INNER_FAILED"
} catch {
    $mainError = [string]$_.Exception.Message
} finally {
    if ($null -ne $originalConfigBytes) {
        try {
            if ((Get-ConsoleVersion) -ne "winsched 0.6.0") {
                Invoke-Setup $CurrentSetupPath
            }
            $restore.current_setup_installed = (Get-ConsoleVersion) -eq "winsched 0.6.0"
            $beforeRestore = Read-Status
            $beforePid = if ($null -ne $beforeRestore) { [int]$beforeRestore.service_pid } else { 0 }
            $beforeSequence = if ($null -ne $beforeRestore) {
                [uint64]$beforeRestore.config_reload_sequence
            } else { 0 }
            Set-FileAtomically $configPath $originalConfigBytes
            $restore.config_bytes_restored = (Get-Sha256 $configPath) -eq $originalConfigHash
            Wait-Condition "V050_UPGRADE_RESTORE_CONFIG_TIMEOUT" {
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
                Assert-True ($LASTEXITCODE -eq 0) "V050_UPGRADE_RESTORE_SCHEDULING_COMMAND"
            }
            Wait-Condition "V050_UPGRADE_RESTORE_SCHEDULING_TIMEOUT" {
                [bool](Read-Status).scheduling_enabled -eq $originalScheduling
            }
            $restore.scheduling_restored = $true
            $restore.service_running = (Get-Service WinSched).Status -eq "Running"
            $payloadHashes = Get-PayloadHashes
            $payloadMatch = $true
            foreach ($name in @(
                "winsched.exe",
                "winsched-service.exe",
                "winsched-monitor.exe",
                "winsched-tray.exe",
                "winsched-settings.exe"
            )) {
                $payloadMatch = $payloadMatch -and
                    (Get-Sha256 (Join-Path $InstallDirectory $name)) -eq $payloadHashes[$name]
            }
            $restore.payload_hashes_match = $payloadMatch
        } catch {
            $restoreError = [string]$_.Exception.Message
        }
    }
}

$passed = $null -eq $mainError -and $null -eq $restoreError -and
    $restore.current_setup_installed -and $restore.config_bytes_restored -and
    $restore.scheduling_restored -and $restore.service_running -and
    $restore.payload_hashes_match
$result = [ordered]@{
    result = if ($passed) { "PASS" } else { "FAIL" }
    current_setup_sha256 = if (Test-Path -LiteralPath $CurrentSetupPath) {
        Get-Sha256 $CurrentSetupPath
    } else { $null }
    old_setup_sha256 = if (Test-Path -LiteralPath $OldSetupPath) {
        Get-Sha256 $OldSetupPath
    } else { $null }
    upgrade = $upgradeReceipt
    restore = $restore
    error = $mainError
    restore_error = $restoreError
}
Write-Result $result
[pscustomobject]$result | ConvertTo-Json -Depth 10
if (-not $passed) { exit 1 }
