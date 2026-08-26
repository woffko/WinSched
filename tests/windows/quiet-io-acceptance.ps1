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

$service = Join-Path $InstallDirectory "winsched-service.exe"
$tray = Join-Path $InstallDirectory "winsched-tray.exe"
$configPath = Join-Path $DataDirectory "winsched.toml"
$statusPath = Join-Path $DataDirectory "status.json"
$logPath = Join-Path $DataDirectory "winsched.log"
$taskName = "WinSchedQuietIoTray"
$originalConfig = $null
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

    $originalConfig = Get-Content -LiteralPath $configPath -Raw
    $disabledConfig = [regex]::Replace(
        $originalConfig,
        '(?m)(^\[logging\]\r?\n)enabled\s*=\s*(?:true|false)\s*$',
        '${1}enabled = false',
        1
    )
    Assert-True ($disabledConfig -ne $originalConfig) "logging fixture did not change config"
    [IO.File]::WriteAllText($configPath, $disabledConfig, [Text.UTF8Encoding]::new($false))
    Wait-Condition "service accepted disabled logging" {
        try {
            $status = Get-Content -LiteralPath $statusPath -Raw | ConvertFrom-Json
            return -not [bool]$status.applied_logging.enabled
        } catch { return $false }
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
    if ($null -ne $originalConfig -and (Test-Path -LiteralPath $configPath)) {
        [IO.File]::WriteAllText($configPath, $originalConfig, [Text.UTF8Encoding]::new($false))
    }
    $parent = Split-Path -Parent $ResultPath
    if ($parent) { New-Item -ItemType Directory -Path $parent -Force | Out-Null }
    [pscustomobject]$result | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $ResultPath -Encoding UTF8
}

[pscustomobject]$result | ConvertTo-Json -Depth 5
