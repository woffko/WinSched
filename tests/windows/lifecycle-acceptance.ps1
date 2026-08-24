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

Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class WinSchedToken {
    const uint PROCESS_QUERY_LIMITED_INFORMATION = 0x1000;
    const uint TOKEN_QUERY = 0x0008;
    const int TokenIntegrityLevel = 25;
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern IntPtr OpenProcess(uint access, bool inherit, uint processId);
    [DllImport("advapi32.dll", SetLastError = true)]
    static extern bool OpenProcessToken(IntPtr process, uint access, out IntPtr token);
    [DllImport("advapi32.dll", SetLastError = true)]
    static extern bool GetTokenInformation(IntPtr token, int informationClass, IntPtr information, int informationLength, out int returnLength);
    [DllImport("advapi32.dll")]
    static extern IntPtr GetSidSubAuthorityCount(IntPtr sid);
    [DllImport("advapi32.dll")]
    static extern IntPtr GetSidSubAuthority(IntPtr sid, uint index);
    [DllImport("kernel32.dll")]
    static extern bool CloseHandle(IntPtr handle);
    public static int IntegrityRid(uint processId) {
        IntPtr process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, processId);
        if (process == IntPtr.Zero) throw new System.ComponentModel.Win32Exception();
        IntPtr token;
        try {
            if (!OpenProcessToken(process, TOKEN_QUERY, out token)) throw new System.ComponentModel.Win32Exception();
        } finally {
            CloseHandle(process);
        }
        try {
            int length;
            GetTokenInformation(token, TokenIntegrityLevel, IntPtr.Zero, 0, out length);
            IntPtr buffer = Marshal.AllocHGlobal(length);
            try {
                if (!GetTokenInformation(token, TokenIntegrityLevel, buffer, length, out length)) throw new System.ComponentModel.Win32Exception();
                IntPtr sid = Marshal.ReadIntPtr(buffer);
                byte count = Marshal.ReadByte(GetSidSubAuthorityCount(sid));
                return Marshal.ReadInt32(GetSidSubAuthority(sid, (uint)(count - 1)));
            } finally {
                Marshal.FreeHGlobal(buffer);
            }
        } finally {
            CloseHandle(token);
        }
    }
}
"@

function Assert-True($Condition, [string]$Message) {
    if (-not [bool]$Condition) {
        throw "ASSERTION FAILED: $Message"
    }
}

function Wait-Condition([string]$Description, [scriptblock]$Condition, [int]$TimeoutSeconds = 30) {
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        if (& $Condition) {
            return
        }
        Start-Sleep -Milliseconds 250
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Timed out waiting for: $Description"
}

function Wait-ServiceAbsent {
    Wait-Condition "WinSched service absent" {
        $null -eq (Get-Service -Name "WinSched" -ErrorAction SilentlyContinue)
    }
}

function Write-Utf8NoBom([string]$Path, [string]$Value) {
    $encoding = New-Object System.Text.UTF8Encoding($false)
    [IO.File]::WriteAllText($Path, $Value, $encoding)
}

function Stop-Tray {
    $deadline = [DateTime]::UtcNow.AddSeconds(15)
    do {
        $processes = @(Get-Process -Name "winsched-tray" -ErrorAction SilentlyContinue)
        foreach ($process in $processes) {
            Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
        }
        if ($processes.Count -ne 0) {
            Start-Sleep -Milliseconds 250
        }
    } while ($processes.Count -ne 0 -and [DateTime]::UtcNow -lt $deadline)
    Assert-True (-not (Get-Process -Name "winsched-tray" -ErrorAction SilentlyContinue)) "tray process survived forced test cleanup"
}

function Start-InteractiveTask(
    [string]$TaskName,
    [string]$UserId,
    [string]$Execute,
    [string]$Arguments,
    [ValidateSet("Limited", "Highest")]
    [string]$RunLevel
) {
    $action = New-ScheduledTaskAction -Execute $Execute -Argument $Arguments
    $principal = New-ScheduledTaskPrincipal -UserId $UserId -LogonType Interactive -RunLevel $RunLevel
    $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit ([TimeSpan]::FromMinutes(5))
    Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false -ErrorAction SilentlyContinue
    Register-ScheduledTask -TaskName $TaskName -Action $action -Principal $principal -Settings $settings | Out-Null
    Start-ScheduledTask -TaskName $TaskName
}

$installedConfig = Join-Path $InstallDirectory "winsched.toml"
$startupShortcut = Join-Path ([Environment]::GetFolderPath("CommonStartup")) "WinSched Tray.lnk"
$programs = Join-Path $env:ProgramData "Microsoft\Windows\Start Menu\Programs\WinSched"
$publicRoot = Join-Path $env:PUBLIC "WinSchedLifecycleAcceptance"
$publicPackage = Join-Path $publicRoot "package"
$wrapper = Join-Path $publicRoot "interactive-install.ps1"
$wrapperResult = Join-Path $publicRoot "interactive-install-result.json"
$installTask = "WinSchedInteractiveInstallAcceptance"
$startupTask = "WinSchedStartupShortcutAcceptance"
$logRetentionProbe = Join-Path $InstallDirectory "winsched.log.10"
$logRetentionProbeCreated = $false

try {
    Write-Host "lifecycle stage: upgrade preserves and normalizes schema-1 configuration"
    $customConfig = Get-Content -LiteralPath $installedConfig -Raw
    $customConfig = [regex]::Replace(
        $customConfig,
        '(?m)^\s*schema_version\s*=\s*\d+\s*$',
        'schema_version = 1'
    )
    $customConfig = [regex]::Replace(
        $customConfig,
        '(?ms)^\s*\[logging\]\s*.*?(?=^\s*\[|\z)',
        ''
    )
    foreach ($section in @('responsiveness.memory', 'responsiveness')) {
        $customConfig = [regex]::Replace(
            $customConfig,
            '(?ms)^\s*\[' + [regex]::Escape($section) + '\]\s*.*?(?=^\s*\[|\z)',
            ''
        )
    }
    $customConfig = $customConfig -replace `
        'minimum_process_utilization_bps\s*=\s*\d+', `
        'minimum_process_utilization_bps = 777'
    Write-Utf8NoBom $installedConfig $customConfig
    $legacyConfigHash = (Get-FileHash -LiteralPath $installedConfig -Algorithm SHA256).Hash.ToLowerInvariant()
    & (Join-Path $PackageDirectory "install.ps1") -NoTrayLaunch
    Assert-True ($LASTEXITCODE -eq 0) "implicit-config upgrade failed"
    $preservedConfig = Get-Content -LiteralPath $installedConfig -Raw
    $preservedConfigHash = (Get-FileHash -LiteralPath $installedConfig -Algorithm SHA256).Hash.ToLowerInvariant()
    Assert-True ($preservedConfigHash -eq $legacyConfigHash) `
        "upgrade did not preserve the schema-1 configuration byte-for-byte"
    Assert-True ($preservedConfig -match '(?m)^\s*schema_version\s*=\s*1\s*$') `
        "upgrade rewrote the legacy schema version"
    Assert-True ($preservedConfig -notmatch '(?m)^\s*\[logging\]\s*$') `
        "upgrade inserted a logging table into the preserved schema-1 file"
    Assert-True ($preservedConfig -notmatch '(?m)^\s*\[responsiveness(?:\.memory)?\]\s*$') `
        "upgrade inserted a responsiveness table into the preserved schema-1 file"
    Assert-True ($preservedConfig -match 'minimum_process_utilization_bps\s*=\s*777') "upgrade overwrote the existing configuration"
    Wait-Condition "schema-1 logging defaults applied by the upgraded service" {
        try {
            $status = Get-Content -LiteralPath (Join-Path $InstallDirectory "status.json") -Raw |
                ConvertFrom-Json
            [int]$status.schema_version -eq 3 -and
                [bool]$status.applied_logging.enabled -and
                [int]$status.applied_logging.max_file_size_mib -eq 10 -and
                [int]$status.applied_logging.retained_archives -eq 1 -and
                -not [bool]$status.applied_responsiveness.enabled -and
                @($status.system_reserve.reserved_cpu_set_ids).Count -eq 0
        } catch {
            $false
        }
    }

    Write-Host "lifecycle stage: normal uninstall preserves data"
    if (-not (Test-Path -LiteralPath $logRetentionProbe -PathType Leaf)) {
        Write-Utf8NoBom $logRetentionProbe "WinSched lifecycle archive preservation probe`n"
        $logRetentionProbeCreated = $true
    }
    $logFilesBeforeUninstall = [ordered]@{}
    foreach ($logFile in @(
        Get-ChildItem -LiteralPath $InstallDirectory -File -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -match '^winsched\.log(?:\.\d+)?$' }
    )) {
        $logFilesBeforeUninstall[$logFile.Name] = [ordered]@{
            path = $logFile.FullName
            sha256 = if ($logFile.Name -eq "winsched.log") {
                $null
            } else {
                (Get-FileHash -LiteralPath $logFile.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
            }
        }
    }
    Assert-True ($logFilesBeforeUninstall.Count -ge 2) `
        "normal-uninstall fixture did not include an active log and an archive"
    & (Join-Path $PackageDirectory "uninstall.ps1")
    Wait-ServiceAbsent
    foreach ($binary in @(
        "winsched.exe",
        "winsched-service.exe",
        "winsched-tray.exe",
        "winsched-settings.exe"
    )) {
        Assert-True (-not (Test-Path -LiteralPath (Join-Path $InstallDirectory $binary))) "normal uninstall left $binary"
    }
    Assert-True (Test-Path -LiteralPath $installedConfig -PathType Leaf) "normal uninstall removed configuration"
    foreach ($entry in $logFilesBeforeUninstall.GetEnumerator()) {
        Assert-True (Test-Path -LiteralPath $entry.Value.path -PathType Leaf) `
            "normal uninstall removed $($entry.Key)"
        if ($entry.Key -ne "winsched.log") {
            $archiveHash = (Get-FileHash -LiteralPath $entry.Value.path -Algorithm SHA256).Hash.ToLowerInvariant()
            Assert-True ($archiveHash -eq $entry.Value.sha256) `
                "normal uninstall modified archive $($entry.Key)"
        }
    }
    Assert-True (-not (Test-Path -LiteralPath $startupShortcut)) "normal uninstall left the Startup shortcut"
    Assert-True (-not (Test-Path -LiteralPath $programs)) "normal uninstall left the Start Menu directory"

    Write-Host "lifecycle stage: purge uninstall removes data"
    if ($logRetentionProbeCreated -and (Test-Path -LiteralPath $logRetentionProbe)) {
        Remove-Item -LiteralPath $logRetentionProbe -Force
        $logRetentionProbeCreated = $false
    }
    & (Join-Path $PackageDirectory "install.ps1") -Configuration (Join-Path $PackageDirectory "winsched.toml") -NoTrayLaunch
    Assert-True ($LASTEXITCODE -eq 0) "reinstall before purge failed"
    & (Join-Path $PackageDirectory "uninstall.ps1") -PurgeData
    Wait-ServiceAbsent
    Assert-True (-not (Test-Path -LiteralPath $InstallDirectory)) "purge uninstall left the installation directory"

    Write-Host "lifecycle stage: default interactive install launches limited tray"
    if (Test-Path -LiteralPath $publicRoot) {
        Remove-Item -LiteralPath $publicRoot -Recurse -Force
    }
    New-Item -ItemType Directory -Path $publicPackage -Force | Out-Null
    Copy-Item -Path (Join-Path $PackageDirectory "*") -Destination $publicPackage -Recurse -Force
    $wrapperSource = @'
param([string]$PackageDirectory, [string]$ResultPath)
$ErrorActionPreference = "Stop"
try {
    & (Join-Path $PackageDirectory "install.ps1") -Configuration (Join-Path $PackageDirectory "winsched.toml")
    [pscustomobject]@{
        result = "PASS"
        identity = [Security.Principal.WindowsIdentity]::GetCurrent().Name
    } | ConvertTo-Json | Set-Content -LiteralPath $ResultPath -Encoding UTF8
} catch {
    [pscustomobject]@{
        result = "FAIL"
        error = $_.Exception.ToString()
    } | ConvertTo-Json | Set-Content -LiteralPath $ResultPath -Encoding UTF8
    exit 1
}
'@
    Write-Utf8NoBom $wrapper $wrapperSource
    Remove-Item -LiteralPath $wrapperResult -Force -ErrorAction SilentlyContinue
    $quote = [char]34
    $wrapperArguments = "-NoProfile -NonInteractive -ExecutionPolicy Bypass -File $quote$wrapper$quote -PackageDirectory $quote$publicPackage$quote -ResultPath $quote$wrapperResult$quote"
    Start-InteractiveTask $installTask $InteractiveUser "powershell.exe" $wrapperArguments "Highest"
    Wait-Condition "interactive default installer result" {
        Test-Path -LiteralPath $wrapperResult -PathType Leaf
    } 90
    $installResult = Get-Content -LiteralPath $wrapperResult -Raw | ConvertFrom-Json
    if ($installResult.result -ne "PASS") {
        $installError = $installResult.PSObject.Properties["error"].Value
        throw "ASSERTION FAILED: interactive default install failed: $installError"
    }
    Wait-Condition "default installer tray process" {
        @(Get-Process -Name "winsched-tray" -ErrorAction SilentlyContinue | Where-Object { $_.SessionId -eq 1 }).Count -eq 1
    }
    $tray = @(Get-Process -Name "winsched-tray" -ErrorAction SilentlyContinue | Where-Object { $_.SessionId -eq 1 })[0]
    $integrityRid = [WinSchedToken]::IntegrityRid([uint32]$tray.Id)
    Assert-True ($integrityRid -ge 0x2000 -and $integrityRid -lt 0x3000) "default installer tray is not medium integrity (RID 0x$($integrityRid.ToString('X')))"

    Write-Host "lifecycle stage: Startup shortcut launches tray"
    Stop-Tray
    $escapedShortcut = $startupShortcut.Replace("'", "''")
    $shortcutArguments = "-NoProfile -NonInteractive -Command $quote" + "Start-Process -FilePath '$escapedShortcut'" + "$quote"
    Start-InteractiveTask $startupTask $InteractiveUser "powershell.exe" $shortcutArguments "Limited"
    Wait-Condition "tray launched from Startup shortcut" {
        @(Get-Process -Name "winsched-tray" -ErrorAction SilentlyContinue | Where-Object { $_.SessionId -eq 1 }).Count -eq 1
    }
    $startupTray = @(Get-Process -Name "winsched-tray" -ErrorAction SilentlyContinue | Where-Object { $_.SessionId -eq 1 })[0]
    $startupIntegrityRid = [WinSchedToken]::IntegrityRid([uint32]$startupTray.Id)
    Assert-True ($startupIntegrityRid -ge 0x2000 -and $startupIntegrityRid -lt 0x3000) "Startup shortcut tray is not medium integrity"
    Assert-True ((Get-Service -Name "WinSched").Status -eq "Running") "service is not running after lifecycle acceptance"

    [pscustomobject]@{
        result = "PASS"
        schema1_upgrade_preserved_bytes = $true
        schema1_logging_defaults_applied = $true
        schema1_responsiveness_default_disabled = $true
        upgrade_preserved_threshold_bps = 777
        normal_uninstall_preserved_data = $true
        purge_removed_data = $true
        default_tray_integrity_rid = $integrityRid
        startup_tray_integrity_rid = $startupIntegrityRid
        final_tray_pid = $startupTray.Id
    } | ConvertTo-Json -Depth 4
} finally {
    if ($logRetentionProbeCreated -and (Test-Path -LiteralPath $logRetentionProbe)) {
        Remove-Item -LiteralPath $logRetentionProbe -Force -ErrorAction SilentlyContinue
    }
    Unregister-ScheduledTask -TaskName $installTask -Confirm:$false -ErrorAction SilentlyContinue
    Unregister-ScheduledTask -TaskName $startupTask -Confirm:$false -ErrorAction SilentlyContinue
}
