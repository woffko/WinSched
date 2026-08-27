[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$CandidateBinary,
    [Parameter(Mandatory = $true)]
    [string]$DisabledIdleScript,
    [string]$InstallDirectory = "$env:ProgramFiles\WinSched",
    [string]$DataDirectory = "$env:ProgramData\WinSched",
    [ValidateRange(10, 300)]
    [int]$DurationSeconds = 30,
    [string]$WorkDirectory = "$env:ProgramData\WinSchedCandidateAcceptance",
    [string]$ResultPath = "$env:PUBLIC\WinSchedCandidateAcceptance\result.json"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Assert-True([bool]$Condition, [string]$Code) {
    if (-not $Condition) { throw $Code }
}

function Wait-Condition([string]$Code, [scriptblock]$Condition, [int]$TimeoutSeconds = 45) {
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

function Write-Result($Value) {
    $parent = Split-Path -Parent $ResultPath
    if (-not [string]::IsNullOrWhiteSpace($parent)) {
        New-Item -ItemType Directory -Path $parent -Force | Out-Null
    }
    [IO.File]::WriteAllText(
        $ResultPath,
        ([pscustomobject]$Value | ConvertTo-Json -Depth 10) + "`n",
        [Text.UTF8Encoding]::new($false)
    )
}

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
Assert-True ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) `
    "CANDIDATE_ACCEPTANCE_ELEVATION_REQUIRED"

$installedBinary = Join-Path $InstallDirectory "winsched-service.exe"
$backupBinary = Join-Path $WorkDirectory "winsched-service.original.exe"
$innerResult = Join-Path $WorkDirectory "disabled-idle-result.json"
$configPath = Join-Path $DataDirectory "winsched.toml"
$originalHash = $null
$candidateHash = $null
$configHash = $null
$initialServiceRunning = $false
$initialScheduling = $null
$candidateResult = $null
$mainError = $null
$restoreError = $null
$restore = [ordered]@{
    binary_restored = $false
    config_unchanged = $false
    service_state_restored = $false
    scheduling_state_restored = $false
}

try {
    Assert-True (Test-Path -LiteralPath $CandidateBinary -PathType Leaf) `
        "CANDIDATE_ACCEPTANCE_BINARY_MISSING"
    Assert-True (Test-Path -LiteralPath $DisabledIdleScript -PathType Leaf) `
        "CANDIDATE_ACCEPTANCE_SCRIPT_MISSING"
    Assert-True (Test-Path -LiteralPath $installedBinary -PathType Leaf) `
        "CANDIDATE_ACCEPTANCE_INSTALLED_BINARY_MISSING"
    Assert-True (Test-Path -LiteralPath $configPath -PathType Leaf) `
        "CANDIDATE_ACCEPTANCE_CONFIG_MISSING"
    $service = Get-Service -Name "WinSched" -ErrorAction SilentlyContinue
    Assert-True ($null -ne $service -and $service.Status -eq "Running") `
        "CANDIDATE_ACCEPTANCE_SERVICE_NOT_RUNNING"
    $initialServiceRunning = $true
    $initialStatus = Read-Status
    Assert-True ($null -ne $initialStatus -and $null -eq $initialStatus.last_error) `
        "CANDIDATE_ACCEPTANCE_INITIAL_STATUS"
    $initialScheduling = [bool]$initialStatus.scheduling_enabled
    $originalHash = Get-Sha256 $installedBinary
    $candidateHash = Get-Sha256 $CandidateBinary
    $configHash = Get-Sha256 $configPath
    New-Item -ItemType Directory -Path $WorkDirectory -Force | Out-Null
    Copy-Item -LiteralPath $installedBinary -Destination $backupBinary -Force
    Assert-True ((Get-Sha256 $backupBinary) -eq $originalHash) `
        "CANDIDATE_ACCEPTANCE_BACKUP_HASH"

    Stop-Service -Name "WinSched" -Force
    Wait-Condition "CANDIDATE_ACCEPTANCE_STOP_TIMEOUT" {
        (Get-Service -Name "WinSched").Status -eq "Stopped"
    }
    Copy-Item -LiteralPath $CandidateBinary -Destination $installedBinary -Force
    Assert-True ((Get-Sha256 $installedBinary) -eq $candidateHash) `
        "CANDIDATE_ACCEPTANCE_COPY_HASH"
    Start-Service -Name "WinSched"
    Wait-Condition "CANDIDATE_ACCEPTANCE_START_TIMEOUT" {
        $currentService = Get-Service -Name "WinSched"
        $currentStatus = Read-Status
        $currentService.Status -eq "Running" -and
            $null -ne $currentStatus -and
            $null -eq $currentStatus.last_error
    }

    Remove-Item -LiteralPath $innerResult -Force -ErrorAction SilentlyContinue
    $powershell = "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe"
    $arguments = @(
        '-NoLogo', '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass',
        '-File', ('"{0}"' -f $DisabledIdleScript),
        '-InstallDirectory', ('"{0}"' -f $InstallDirectory),
        '-DataDirectory', ('"{0}"' -f $DataDirectory),
        '-DurationSeconds', $DurationSeconds,
        '-ResultPath', ('"{0}"' -f $innerResult)
    ) -join ' '
    $inner = Start-Process -FilePath $powershell -ArgumentList $arguments -Wait -PassThru
    Assert-True (Test-Path -LiteralPath $innerResult -PathType Leaf) `
        "CANDIDATE_ACCEPTANCE_INNER_RESULT_MISSING"
    $candidateResult = Get-Content -LiteralPath $innerResult -Raw | ConvertFrom-Json
    Assert-True ($inner.ExitCode -eq 0 -and [string]$candidateResult.result -eq "PASS") `
        "CANDIDATE_ACCEPTANCE_DISABLED_IDLE_FAILED"
} catch {
    $mainError = [string]$_.Exception.Message
} finally {
    if ($null -ne $originalHash -and (Test-Path -LiteralPath $backupBinary -PathType Leaf)) {
        try {
            $service = Get-Service -Name "WinSched" -ErrorAction SilentlyContinue
            if ($null -ne $service -and $service.Status -ne "Stopped") {
                Stop-Service -Name "WinSched" -Force
                Wait-Condition "CANDIDATE_ACCEPTANCE_RESTORE_STOP_TIMEOUT" {
                    (Get-Service -Name "WinSched").Status -eq "Stopped"
                }
            }
            Copy-Item -LiteralPath $backupBinary -Destination $installedBinary -Force
            $restore.binary_restored = (Get-Sha256 $installedBinary) -eq $originalHash
            if ($initialServiceRunning) {
                Start-Service -Name "WinSched"
                Wait-Condition "CANDIDATE_ACCEPTANCE_RESTORE_START_TIMEOUT" {
                    (Get-Service -Name "WinSched").Status -eq "Running" -and $null -ne (Read-Status)
                }
                $restore.service_state_restored = $true
                $restoredStatus = Read-Status
                if ($null -ne $initialScheduling -and
                    [bool]$restoredStatus.scheduling_enabled -ne $initialScheduling) {
                    $command = if ($initialScheduling) { "enable" } else { "disable" }
                    & $installedBinary $command | Out-Null
                    Assert-True ($LASTEXITCODE -eq 0) `
                        "CANDIDATE_ACCEPTANCE_RESTORE_SCHEDULING_COMMAND"
                    Wait-Condition "CANDIDATE_ACCEPTANCE_RESTORE_SCHEDULING_TIMEOUT" {
                        [bool](Read-Status).scheduling_enabled -eq $initialScheduling
                    }
                }
                $restore.scheduling_state_restored =
                    [bool](Read-Status).scheduling_enabled -eq $initialScheduling
            }
            $restore.config_unchanged = (Get-Sha256 $configPath) -eq $configHash
        } catch {
            $restoreError = [string]$_.Exception.Message
        }
    }
}

$accepted = $null -eq $mainError -and $null -eq $restoreError -and
    $restore.binary_restored -and $restore.config_unchanged -and
    $restore.service_state_restored -and $restore.scheduling_state_restored
$result = [ordered]@{
    result = if ($accepted) { "PASS" } else { "FAIL" }
    error = $mainError
    restore_error = $restoreError
    original_binary_sha256 = $originalHash
    candidate_binary_sha256 = $candidateHash
    candidate = $candidateResult
    restore = $restore
}
Write-Result $result
[pscustomobject]$result | ConvertTo-Json -Depth 10
if (-not $accepted) { exit 1 }
