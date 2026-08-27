[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$SetupPath,
    [Parameter(Mandatory = $true)]
    [string]$WorkDirectory,
    [Parameter(Mandatory = $true)]
    [string]$ExpectedSetupSha256,
    [Parameter(Mandatory = $true)]
    [string]$ExpectedWinSchedSha256,
    [Parameter(Mandatory = $true)]
    [string]$ExpectedServiceSha256,
    [Parameter(Mandatory = $true)]
    [string]$ExpectedTraySha256,
    [Parameter(Mandatory = $true)]
    [string]$ExpectedSettingsSha256,
    [string]$ExpectedPreviousVersion = "0.5.0",
    [string]$ExpectedVersion = "0.5.1",
    [ValidateSet("off", "normal", "trace")]
    [string]$ExpectedLoggingLevel = "off",
    [string]$InstallDirectory = "$env:ProgramFiles\WinSched",
    [string]$DataDirectory = "$env:ProgramData\WinSched"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw "ASSERTION FAILED: $Message" }
}

function Wait-Condition([string]$Description, [scriptblock]$Condition, [int]$TimeoutSeconds = 90) {
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        if (& $Condition) { return }
        Start-Sleep -Milliseconds 250
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Timed out waiting for: $Description"
}

function Get-ConsoleVersion([string]$Path) {
    return ((& $Path --version | Out-String).Trim() -replace '^\S+\s+', '')
}

function Set-FileAtomically([string]$Path, [byte[]]$Bytes) {
    $directory = Split-Path -Parent $Path
    $temporaryPath = Join-Path $directory (
        ".{0}.host-upgrade-{1}.tmp" -f (Split-Path -Leaf $Path), [Guid]::NewGuid().ToString("N")
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

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
Assert-True ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) `
    "physical-host upgrade requires an elevated shell"

New-Item -ItemType Directory -Path $WorkDirectory -Force | Out-Null
$resultPath = Join-Path $WorkDirectory "host-upgrade-result.json"
$backupPath = Join-Path $WorkDirectory "pre-upgrade-config.bin"
$setupLog = Join-Path $WorkDirectory "host-upgrade-setup.log"
$configPath = Join-Path $DataDirectory "winsched.toml"
$cliPath = Join-Path $InstallDirectory "winsched.exe"
$servicePath = Join-Path $InstallDirectory "winsched-service.exe"
$trayPath = Join-Path $InstallDirectory "winsched-tray.exe"
$settingsPath = Join-Path $InstallDirectory "winsched-settings.exe"
$originalBytes = $null
$originalHash = $null
$result = $null
$exitCode = 1

try {
    Assert-True (Test-Path -LiteralPath $SetupPath -PathType Leaf) "Setup is missing"
    Assert-True (Test-Path -LiteralPath $configPath -PathType Leaf) "installed config is missing"
    Assert-True (Test-Path -LiteralPath $cliPath -PathType Leaf) "installed CLI is missing"
    Assert-True ((Get-ConsoleVersion $cliPath) -eq $ExpectedPreviousVersion) `
        "unexpected pre-upgrade version"
    Assert-True (
        (Get-FileHash -LiteralPath $SetupPath -Algorithm SHA256).Hash.ToLowerInvariant() -eq
        $ExpectedSetupSha256
    ) "Setup SHA-256 mismatch"

    $beforeStatus = Get-Content -LiteralPath (Join-Path $DataDirectory "status.json") -Raw |
        ConvertFrom-Json
    Assert-True ((Get-Service WinSched).Status -eq "Running") "service is not Running"
    Assert-True ($null -eq $beforeStatus.last_error) "pre-upgrade service reports an error"
    $originalBytes = [IO.File]::ReadAllBytes($configPath)
    [IO.File]::WriteAllBytes($backupPath, $originalBytes)
    $originalHash = (Get-FileHash -LiteralPath $backupPath -Algorithm SHA256).Hash.ToLowerInvariant()
    Assert-True (
        (Get-FileHash -LiteralPath $configPath -Algorithm SHA256).Hash.ToLowerInvariant() -eq
        $originalHash
    ) "pre-upgrade config backup mismatch"

    $process = Start-Process `
        -FilePath $SetupPath `
        -ArgumentList @(
            '/VERYSILENT',
            '/SUPPRESSMSGBOXES',
            '/NORESTART',
            "/LOG=$setupLog"
        ) `
        -Wait `
        -PassThru
    Assert-True ($process.ExitCode -eq 0) "Setup returned $($process.ExitCode)"
    Wait-Condition "WinSched 0.5.1 service status" {
        try {
            $status = Get-Content -LiteralPath (Join-Path $DataDirectory "status.json") -Raw |
                ConvertFrom-Json
            (Get-Service WinSched).Status -eq "Running" -and
                [int]$status.schema_version -eq 5 -and
                [string]$status.applied_logging.level -eq $ExpectedLoggingLevel -and
                [bool]$status.scheduling_enabled -eq [bool]$beforeStatus.scheduling_enabled -and
                $null -eq $status.last_error
        } catch { $false }
    } 120

    Assert-True ((Get-ConsoleVersion $cliPath) -eq $ExpectedVersion) `
        "installed CLI version mismatch"
    Assert-True (
        (Get-FileHash -LiteralPath $configPath -Algorithm SHA256).Hash.ToLowerInvariant() -eq
        $originalHash
    ) "upgrade changed config bytes"
    foreach ($entry in @(
        [pscustomobject]@{ Path = $cliPath; Hash = $ExpectedWinSchedSha256 },
        [pscustomobject]@{ Path = $servicePath; Hash = $ExpectedServiceSha256 },
        [pscustomobject]@{ Path = $trayPath; Hash = $ExpectedTraySha256 },
        [pscustomobject]@{ Path = $settingsPath; Hash = $ExpectedSettingsSha256 }
    )) {
        Assert-True (
            (Get-FileHash -LiteralPath $entry.Path -Algorithm SHA256).Hash.ToLowerInvariant() -eq
            $entry.Hash
        ) "installed hash mismatch: $($entry.Path)"
    }

    $afterStatus = Get-Content -LiteralPath (Join-Path $DataDirectory "status.json") -Raw |
        ConvertFrom-Json
    $result = [ordered]@{
        result = "PASS"
        setup_sha256 = $ExpectedSetupSha256
        previous_version = $ExpectedPreviousVersion
        installed_version = $ExpectedVersion
        config_sha256_before = $originalHash
        config_sha256_after = (Get-FileHash $configPath -Algorithm SHA256).Hash.ToLowerInvariant()
        config_byte_identical = $true
        status_schema = [int]$afterStatus.schema_version
        logging_level = [string]$afterStatus.applied_logging.level
        scheduling_preserved = [bool]$afterStatus.scheduling_enabled -eq [bool]$beforeStatus.scheduling_enabled
        service_running = (Get-Service WinSched).Status -eq "Running"
        last_error = $afterStatus.last_error
        recovery_backup_retained = $true
    }
    $exitCode = 0
} catch {
    if ($null -ne $originalBytes -and (Test-Path -LiteralPath $configPath -PathType Leaf)) {
        try { Set-FileAtomically $configPath $originalBytes } catch {}
    }
    $service = Get-Service WinSched -ErrorAction SilentlyContinue
    if ($null -ne $service -and $service.Status -ne "Running") {
        Start-Service WinSched -ErrorAction SilentlyContinue
    }
    $result = [ordered]@{
        result = "FAIL"
        error = $_.Exception.ToString()
        script_stack = $_.ScriptStackTrace
        recovery_backup_retained = Test-Path -LiteralPath $backupPath -PathType Leaf
    }
    $exitCode = 1
} finally {
    [IO.File]::WriteAllText(
        $resultPath,
        ([pscustomobject]$result | ConvertTo-Json -Depth 6) + "`n",
        [Text.UTF8Encoding]::new($false)
    )
}

[pscustomobject]$result | ConvertTo-Json -Depth 6
exit $exitCode
