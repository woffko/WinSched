[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$SetupPath,
    [Parameter(Mandatory = $true)]
    [string]$TestDirectory,
    [Parameter(Mandatory = $true)]
    [string]$InteractiveUser,
    [Parameter(Mandatory = $true)]
    [string]$OutputRoot,
    [string]$InstallDirectory = "$env:ProgramFiles\WinSched",
    [string]$DataDirectory = "$env:ProgramData\WinSched"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw "ASSERTION FAILED: $Message" }
}

function Wait-Condition([string]$Description, [scriptblock]$Condition, [int]$TimeoutSeconds = 60) {
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        if (& $Condition) { return }
        Start-Sleep -Milliseconds 250
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Timed out waiting for: $Description"
}

function Set-FileAtomically([string]$Path, [byte[]]$Bytes) {
    $directory = Split-Path -Parent $Path
    $temporaryPath = Join-Path $directory (
        ".{0}.lifecycle-{1}.tmp" -f (Split-Path -Leaf $Path), [Guid]::NewGuid().ToString("N")
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
        foreach ($cleanupPath in @($temporaryPath, $replacementBackup)) {
            Remove-Item -LiteralPath $cleanupPath -Force -ErrorAction SilentlyContinue
        }
    }
}

function Invoke-Checked([string]$Name, [scriptblock]$Action) {
    Write-Host "Lifecycle stage: $Name"
    & $Action
    if ($LASTEXITCODE -ne 0) {
        throw "$Name failed with exit code $LASTEXITCODE"
    }
    $script:stages[$Name] = "PASS"
}

function Install-Silent {
    $process = Start-Process `
        -FilePath $SetupPath `
        -ArgumentList @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART') `
        -Wait `
        -PassThru
    Assert-True ($process.ExitCode -eq 0) "recovery Setup returned $($process.ExitCode)"
    Wait-Condition "WinSched service Running after recovery install" {
        $service = Get-Service WinSched -ErrorAction SilentlyContinue
        $null -ne $service -and $service.Status -eq "Running"
    } 90
}

function Invoke-GuiInstaller([string]$OutputDirectory, [string]$ExpectedMarker) {
    $arguments = @(
        '-NoLogo',
        '-NoProfile',
        '-NonInteractive',
        '-ExecutionPolicy',
        'Bypass',
        '-File',
        (Join-Path $TestDirectory "schedule-gui-installer-ui-acceptance.ps1"),
        '-AcceptanceScript',
        (Join-Path $TestDirectory "gui-installer-ui-acceptance.ps1"),
        '-SetupPath',
        $SetupPath,
        '-OutputDirectory',
        $OutputDirectory,
        '-InteractiveUser',
        $InteractiveUser
    )
    if (-not [string]::IsNullOrWhiteSpace($ExpectedMarker)) {
        $arguments += @('-ExpectedConfigMarker', $ExpectedMarker)
    }
    & powershell.exe @arguments
    if ($LASTEXITCODE -ne 0) {
        throw "GUI installer acceptance failed with exit code $LASTEXITCODE"
    }
}

function Invoke-GuiUninstaller([string]$OutputDirectory, [ValidateSet('Preserve', 'Purge')][string]$Choice) {
    & powershell.exe `
        -NoLogo `
        -NoProfile `
        -NonInteractive `
        -ExecutionPolicy Bypass `
        -File (Join-Path $TestDirectory "schedule-gui-uninstaller-ui-acceptance.ps1") `
        -AcceptanceScript (Join-Path $TestDirectory "gui-uninstaller-ui-acceptance.ps1") `
        -OutputDirectory $OutputDirectory `
        -InteractiveUser $InteractiveUser `
        -PurgeChoice $Choice `
        -InstallDirectory $InstallDirectory `
        -DataDirectory $DataDirectory
    if ($LASTEXITCODE -ne 0) {
        throw "GUI uninstaller $Choice acceptance failed with exit code $LASTEXITCODE"
    }
}

New-Item -ItemType Directory -Path $OutputRoot -Force | Out-Null
$resultPath = Join-Path $OutputRoot "installer-lifecycle-result.json"
$recoveryBackup = Join-Path $OutputRoot "original-config.recovery.bin"
$configPath = Join-Path $DataDirectory "winsched.toml"
$serviceBinary = Join-Path $InstallDirectory "winsched-service.exe"
$cliBinary = Join-Path $InstallDirectory "winsched.exe"
$script:stages = [ordered]@{}
$originalConfigBytes = $null
$originalConfigHash = $null
$originalScheduling = $null
$originalMode = $null
$originalLoggingLevel = $null
$mainError = $null
$mainStack = $null
$cleanupErrors = New-Object System.Collections.ArrayList
$setupHash = $null
$recoveryCompleted = $false

try {
    Assert-True (Test-Path -LiteralPath $SetupPath -PathType Leaf) "Setup is missing"
    Assert-True (Test-Path -LiteralPath $configPath -PathType Leaf) "installed config is missing"
    Assert-True (Test-Path -LiteralPath $serviceBinary -PathType Leaf) "installed service is missing"
    $setupHash = (Get-FileHash -LiteralPath $SetupPath -Algorithm SHA256).Hash.ToLowerInvariant()
    $originalConfigBytes = [IO.File]::ReadAllBytes($configPath)
    [IO.File]::WriteAllBytes($recoveryBackup, $originalConfigBytes)
    $originalConfigHash = (Get-FileHash -LiteralPath $recoveryBackup -Algorithm SHA256).Hash.ToLowerInvariant()
    Assert-True (
        (Get-FileHash -LiteralPath $configPath -Algorithm SHA256).Hash.ToLowerInvariant() -eq
        $originalConfigHash
    ) "recovery config copy does not match the live file"
    $status = Get-Content -LiteralPath (Join-Path $DataDirectory "status.json") -Raw |
        ConvertFrom-Json
    $originalScheduling = [bool]$status.scheduling_enabled
    $originalMode = [string]$status.configured_mode
    $originalLoggingLevel = [string]$status.applied_logging.level

    Invoke-Checked "provision rollback" {
        & powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass `
            -File (Join-Path $TestDirectory "provision-rollback-acceptance.ps1") `
            -InstallDirectory $InstallDirectory `
            -DataDirectory $DataDirectory `
            -ResultPath (Join-Path $OutputRoot "provision-rollback-result.json")
    }
    Invoke-Checked "Setup error receipt" {
        & powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass `
            -File (Join-Path $TestDirectory "setup-error-receipt-acceptance.ps1") `
            -SetupPath $SetupPath `
            -InstallDirectory $InstallDirectory `
            -DataDirectory $DataDirectory `
            -ResultPath (Join-Path $OutputRoot "setup-error-receipt-result.json")
    }
    Invoke-Checked "silent preserve uninstall" {
        & powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass `
            -File (Join-Path $TestDirectory "silent-uninstall-acceptance.ps1") `
            -SetupPath $SetupPath `
            -Scenario Preserve `
            -OutputDirectory (Join-Path $OutputRoot "silent-preserve")
    }
    Invoke-Checked "silent purge uninstall" {
        & powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass `
            -File (Join-Path $TestDirectory "silent-uninstall-acceptance.ps1") `
            -SetupPath $SetupPath `
            -Scenario Purge `
            -OutputDirectory (Join-Path $OutputRoot "silent-purge")
    }

    Write-Host "Lifecycle stage: clean GUI install"
    Invoke-GuiInstaller (Join-Path $OutputRoot "gui-clean-install") ""
    $script:stages["clean GUI install"] = "PASS"

    $marker = "# lifecycle-preserve-$([Guid]::NewGuid().ToString('N'))"
    $currentText = Get-Content -LiteralPath $configPath -Raw
    $markedText = $currentText.TrimEnd([char[]]"`r`n") + "`r`n$marker`r`n"
    Set-FileAtomically $configPath ([Text.UTF8Encoding]::new($false).GetBytes($markedText))
    $markedHash = (Get-FileHash -LiteralPath $configPath -Algorithm SHA256).Hash.ToLowerInvariant()

    Write-Host "Lifecycle stage: GUI preserve uninstall"
    Invoke-GuiUninstaller (Join-Path $OutputRoot "gui-preserve-uninstall") Preserve
    Assert-True (Test-Path -LiteralPath $configPath -PathType Leaf) `
        "GUI preserve uninstall removed config"
    Assert-True (
        (Get-FileHash -LiteralPath $configPath -Algorithm SHA256).Hash.ToLowerInvariant() -eq
        $markedHash
    ) "GUI preserve uninstall changed marked config bytes"
    $script:stages["GUI preserve uninstall"] = "PASS"

    Write-Host "Lifecycle stage: GUI reinstall with preserved data"
    Invoke-GuiInstaller (Join-Path $OutputRoot "gui-preserved-reinstall") $marker
    $script:stages["GUI preserved reinstall"] = "PASS"

    Write-Host "Lifecycle stage: GUI purge uninstall"
    Invoke-GuiUninstaller (Join-Path $OutputRoot "gui-purge-uninstall") Purge
    Assert-True (-not (Test-Path -LiteralPath $DataDirectory)) `
        "GUI purge uninstall retained ProgramData"
    $script:stages["GUI purge uninstall"] = "PASS"

    Write-Host "Lifecycle stage: final silent install"
    Install-Silent
    Assert-True ((& $cliBinary --version | Out-String).Trim() -eq "winsched 0.6.0") `
        "final installed CLI version is not 0.6.0"
    $script:stages["final silent install"] = "PASS"
} catch {
    $mainError = $_.Exception.ToString()
    $mainStack = $_.ScriptStackTrace
} finally {
    if ($null -ne $originalConfigBytes) {
        try {
            if (-not (Test-Path -LiteralPath $serviceBinary -PathType Leaf)) {
                Install-Silent
            } else {
                $service = Get-Service WinSched -ErrorAction SilentlyContinue
                if ($null -eq $service) { Install-Silent }
            }
            $statusBeforeRestore = Get-Content `
                -LiteralPath (Join-Path $DataDirectory "status.json") `
                -Raw |
                ConvertFrom-Json
            $restorePid = [int]$statusBeforeRestore.service_pid
            $restoreSequence = [uint64]$statusBeforeRestore.config_reload_sequence
            Set-FileAtomically $configPath $originalConfigBytes
            Assert-True (
                (Get-FileHash -LiteralPath $configPath -Algorithm SHA256).Hash.ToLowerInvariant() -eq
                $originalConfigHash
            ) "original config bytes were not restored"
            $service = Get-Service WinSched -ErrorAction SilentlyContinue
            if ($null -eq $service -or $service.Status -ne "Running") {
                Start-Service WinSched
            }
            Wait-Condition "restored WinSched status" {
                try {
                    $status = Get-Content -LiteralPath (Join-Path $DataDirectory "status.json") -Raw |
                        ConvertFrom-Json
                    $freshReceipt = if ([int]$status.service_pid -eq $restorePid) {
                        [uint64]$status.config_reload_sequence -gt $restoreSequence
                    } else {
                        [uint64]$status.config_reload_sequence -gt 0
                    }
                    [int]$status.schema_version -eq 5 -and
                        $freshReceipt -and
                        [string]$status.config_reload_result -eq "reloaded" -and
                        [string]$status.configured_mode -eq $originalMode -and
                        [string]$status.applied_logging.level -eq $originalLoggingLevel -and
                        $null -eq $status.last_error
                } catch { $false }
            } 90
            if ($originalScheduling) {
                & $serviceBinary enable | Out-Null
            } else {
                & $serviceBinary disable | Out-Null
            }
            Assert-True ($LASTEXITCODE -eq 0) "could not restore Scheduling state"
            Wait-Condition "restored Scheduling state" {
                try {
                    $status = Get-Content -LiteralPath (Join-Path $DataDirectory "status.json") -Raw |
                        ConvertFrom-Json
                    [bool]$status.scheduling_enabled -eq $originalScheduling
                } catch { $false }
            } 30
            $recoveryCompleted = $true
            Remove-Item -LiteralPath $recoveryBackup -Force -ErrorAction SilentlyContinue
        } catch {
            [void]$cleanupErrors.Add($_.Exception.ToString())
        }
    }
}

$passed = $null -eq $mainError -and $cleanupErrors.Count -eq 0
$finalService = Get-Service WinSched -ErrorAction SilentlyContinue
$result = [ordered]@{
    result = if ($passed) { "PASS" } else { "FAIL" }
    setup_sha256 = $setupHash
    stages = $script:stages
    original_config_sha256 = $originalConfigHash
    original_config_restored = $recoveryCompleted
    original_scheduling_restored = $recoveryCompleted
    service_running = $null -ne $finalService -and $finalService.Status -eq "Running"
    cleanup_errors = @($cleanupErrors)
    error = $mainError
    script_stack = $mainStack
    recovery_backup_retained = Test-Path -LiteralPath $recoveryBackup -PathType Leaf
}
[IO.File]::WriteAllText(
    $resultPath,
    ([pscustomobject]$result | ConvertTo-Json -Depth 8) + "`n",
    [Text.UTF8Encoding]::new($false)
)
[pscustomobject]$result | ConvertTo-Json -Depth 8
if (-not $passed) { exit 1 }
