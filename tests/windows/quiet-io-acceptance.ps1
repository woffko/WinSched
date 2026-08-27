[CmdletBinding()]
param(
    [string]$InstallDirectory = "$env:ProgramFiles\WinSched",
    [string]$DataDirectory = "$env:ProgramData\WinSched",
    [string]$InteractiveUser,
    [int]$DurationSeconds = 75,
    [string]$ResultPath = "$env:PUBLIC\WinSchedFinalAcceptance\output\quiet-io-result.json"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw "ASSERTION FAILED: $Message" }
}

function Wait-Condition([string]$Description, [scriptblock]$Condition, [int]$TimeoutSeconds = 30) {
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        if (& $Condition) { return }
        Start-Sleep -Milliseconds 250
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Timed out waiting for: $Description"
}

function Read-Status {
    if (-not (Test-Path -LiteralPath $statusPath -PathType Leaf)) { return $null }
    try { return Get-Content -LiteralPath $statusPath -Raw | ConvertFrom-Json } catch { return $null }
}

function Set-FileAtomically([string]$Path, [byte[]]$Bytes) {
    $directory = Split-Path -Parent $Path
    $temporaryPath = Join-Path $directory (
        ".{0}.quiet-io-{1}.tmp" -f (Split-Path -Leaf $Path), [Guid]::NewGuid().ToString("N")
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

function New-DisabledLoggingConfigText([string]$Text) {
    $schema = [regex]::Match(
        $Text,
        '(?m)^\s*schema_version\s*=\s*(?<value>\d+)\s*$'
    )
    Assert-True $schema.Success "config schema_version is missing"
    $schemaVersion = [int]$schema.Groups['value'].Value
    $sectionPattern = '(?ms)^\s*\[logging\]\s*(?<body>.*?)(?=^\s*\[|\z)'
    $section = [regex]::Match($Text, $sectionPattern)
    Assert-True $section.Success "logging section is missing"
    $body = $section.Groups['body'].Value
    if ($schemaVersion -ge 5) {
        Assert-True (-not [regex]::IsMatch($body, '(?m)^\s*enabled\s*=')) `
            "schema-5 logging section contains removed enabled field"
        if ([regex]::IsMatch($body, '(?m)^\s*level\s*=\s*"(?:off|normal|trace)"\s*$')) {
            $updatedBody = [regex]::Replace(
                $body,
                '(?m)^\s*level\s*=\s*"(?:off|normal|trace)"\s*$',
                'level = "off"',
                1
            )
        } else {
            $updatedBody = "level = `"off`"`r`n$body"
        }
    } else {
        Assert-True (-not [regex]::IsMatch($body, '(?m)^\s*level\s*=')) `
            "legacy logging section contains schema-5 level field"
        if ([regex]::IsMatch($body, '(?m)^\s*enabled\s*=\s*(true|false)\s*$')) {
            $updatedBody = [regex]::Replace(
                $body,
                '(?m)^\s*enabled\s*=\s*(true|false)\s*$',
                'enabled = false',
                1
            )
        } else {
            $updatedBody = "enabled = false`r`n$body"
        }
    }
    return $Text.Substring(0, $section.Index) +
        "[logging]`r`n$updatedBody" +
        $Text.Substring($section.Index + $section.Length)
}

$service = Join-Path $InstallDirectory "winsched-service.exe"
$tray = Join-Path $InstallDirectory "winsched-tray.exe"
$configPath = Join-Path $DataDirectory "winsched.toml"
$statusPath = Join-Path $DataDirectory "status.json"
$logPath = Join-Path $DataDirectory "winsched.log"
$taskName = "WinSchedQuietIoTray"
$originalConfigBytes = $null
$originalConfigHash = $null
$originalStatus = $null
$configChanged = $false
$trayStarted = $false
$result = $null

try {
    Assert-True (Test-Path -LiteralPath $service -PathType Leaf) "service binary is missing"
    Assert-True (Test-Path -LiteralPath $tray -PathType Leaf) "tray binary is missing"
    Assert-True (Test-Path -LiteralPath $configPath -PathType Leaf) "config is missing"
    if ([string]::IsNullOrWhiteSpace($InteractiveUser)) {
        $InteractiveUser = (Get-CimInstance Win32_ComputerSystem).UserName
    }
    Assert-True (-not [string]::IsNullOrWhiteSpace($InteractiveUser)) "no interactive user"

    $originalConfigBytes = [IO.File]::ReadAllBytes($configPath)
    $originalConfigHash = (Get-FileHash -LiteralPath $configPath -Algorithm SHA256).Hash
    $decoder = [Text.UTF8Encoding]::new($false, $true)
    $originalConfig = $decoder.GetString($originalConfigBytes)
    $originalStatus = Read-Status
    Assert-True ($null -ne $originalStatus -and [int]$originalStatus.schema_version -eq 5) `
        "schema-5 status is required before quiet-I/O acceptance"
    $reloadSequence = [uint64]$originalStatus.config_reload_sequence
    $disabledConfig = New-DisabledLoggingConfigText $originalConfig
    Assert-True ($disabledConfig -ne $originalConfig) "logging fixture did not change config"
    Set-FileAtomically $configPath ([Text.UTF8Encoding]::new($false).GetBytes($disabledConfig))
    $configChanged = $true
    Wait-Condition "service accepted disabled logging" {
        $status = Read-Status
        return $null -ne $status -and
            [uint64]$status.config_reload_sequence -gt $reloadSequence -and
            [string]$status.config_reload_result -eq "reloaded" -and
            [string]$status.applied_logging.level -eq "off"
    }

    Get-Process -Name "winsched-tray" -ErrorAction SilentlyContinue |
        Where-Object SessionId -gt 0 |
        Stop-Process -Force -ErrorAction SilentlyContinue
    $action = New-ScheduledTaskAction -Execute $tray -WorkingDirectory $InstallDirectory
    $principal = New-ScheduledTaskPrincipal `
        -UserId $InteractiveUser `
        -LogonType Interactive `
        -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet `
        -AllowStartIfOnBatteries `
        -DontStopIfGoingOnBatteries `
        -ExecutionTimeLimit ([TimeSpan]::FromMinutes(3))
    Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue
    Register-ScheduledTask -TaskName $taskName -Action $action -Principal $principal -Settings $settings | Out-Null
    Start-ScheduledTask -TaskName $taskName
    Wait-Condition "limited tray process" {
        @(Get-Process -Name "winsched-tray" -ErrorAction SilentlyContinue |
            Where-Object SessionId -gt 0).Count -gt 0
    }
    $trayStarted = $true

    $logHashBefore = if (Test-Path -LiteralPath $logPath -PathType Leaf) {
        (Get-FileHash -LiteralPath $logPath -Algorithm SHA256).Hash
    } else { $null }
    $interactiveSid = ([Security.Principal.NTAccount]::new($InteractiveUser)).Translate(
        [Security.Principal.SecurityIdentifier]
    ).Value
    $interactiveProfile = Get-CimInstance Win32_UserProfile |
        Where-Object SID -eq $interactiveSid |
        Select-Object -First 1
    Assert-True ($null -ne $interactiveProfile) "interactive user profile is missing"
    $trayLog = Join-Path (Join-Path $interactiveProfile.LocalPath "AppData\Local\WinSched") "tray.log"
    $trayLogLengthBefore = if (Test-Path -LiteralPath $trayLog -PathType Leaf) {
        (Get-Item -LiteralPath $trayLog).Length
    } else { 0 }
    $lastStatusWrite = (Get-Item -LiteralPath $statusPath).LastWriteTimeUtc
    $statusWrites = 0
    $deadline = [DateTime]::UtcNow.AddSeconds($DurationSeconds)
    while ([DateTime]::UtcNow -lt $deadline) {
        Start-Sleep -Milliseconds 250
        $currentWrite = (Get-Item -LiteralPath $statusPath).LastWriteTimeUtc
        if ($currentWrite -ne $lastStatusWrite) {
            $statusWrites++
            $lastStatusWrite = $currentWrite
        }
    }
    $logHashAfter = if (Test-Path -LiteralPath $logPath -PathType Leaf) {
        (Get-FileHash -LiteralPath $logPath -Algorithm SHA256).Hash
    } else { $null }
    $trayLogLengthAfter = if (Test-Path -LiteralPath $trayLog -PathType Leaf) {
        (Get-Item -LiteralPath $trayLog).Length
    } else { 0 }

    Assert-True ($statusWrites -ge 5 -and $statusWrites -le 10) `
        "unexpected status write count over $DurationSeconds seconds: $statusWrites"
    Assert-True ($logHashAfter -eq $logHashBefore) "disabled service log changed"
    Assert-True ($trayLogLengthAfter -eq $trayLogLengthBefore) "healthy tray wrote an error log"
    $result = [ordered]@{
        result = "PASS"
        duration_seconds = $DurationSeconds
        status_writes = $statusWrites
        service_log_byte_stable = $true
        tray_log_byte_stable = $true
        original_config_restored = $false
        tray_session = @(Get-Process -Name "winsched-tray" -ErrorAction SilentlyContinue |
            Where-Object SessionId -gt 0 | Select-Object -First 1 -ExpandProperty SessionId)
    }
} catch {
    $result = [ordered]@{
        result = "FAIL"
        error = $_.Exception.ToString()
        script_stack = $_.ScriptStackTrace
    }
    throw
} finally {
    if ($trayStarted) {
        Get-Process -Name "winsched-tray" -ErrorAction SilentlyContinue |
            Where-Object SessionId -gt 0 |
            Stop-Process -Force -ErrorAction SilentlyContinue
    }
    Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue
    if ($configChanged -and $null -ne $originalConfigBytes -and
        (Test-Path -LiteralPath $configPath)) {
        try {
            $restoreBaseline = Read-Status
            $restoreSequence = if ($null -ne $restoreBaseline) {
                [uint64]$restoreBaseline.config_reload_sequence
            } else { 0 }
            Set-FileAtomically $configPath $originalConfigBytes
            Wait-Condition "service accepted byte-exact restored configuration" {
                $status = Read-Status
                $null -ne $status -and
                    [uint64]$status.config_reload_sequence -gt $restoreSequence -and
                    [string]$status.config_reload_result -eq "reloaded" -and
                    [string]$status.configured_mode -eq [string]$originalStatus.configured_mode -and
                    [string]$status.applied_logging.level -eq [string]$originalStatus.applied_logging.level -and
                    [int]$status.applied_logging.max_file_size_mib -eq [int]$originalStatus.applied_logging.max_file_size_mib -and
                    [int]$status.applied_logging.retained_archives -eq [int]$originalStatus.applied_logging.retained_archives
            } 30
            Assert-True (
                (Get-FileHash -LiteralPath $configPath -Algorithm SHA256).Hash -eq $originalConfigHash
            ) "original configuration bytes were not restored exactly"
            $result["original_config_restored"] = $true
        } catch {
            if ($null -eq $result) { $result = [ordered]@{} }
            $result["result"] = "FAIL"
            $result["cleanup_error"] = $_.Exception.ToString()
            $result["original_config_restored"] = $false
        }
    }
    $parent = Split-Path -Parent $ResultPath
    if ($parent) { New-Item -ItemType Directory -Path $parent -Force | Out-Null }
    [pscustomobject]$result | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $ResultPath -Encoding UTF8
}

[pscustomobject]$result | ConvertTo-Json -Depth 5
if ([string]$result.result -ne "PASS") { exit 1 }
