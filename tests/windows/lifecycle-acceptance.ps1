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

try {
    Write-Host "lifecycle stage: upgrade preserves implicit configuration"
    $customConfig = (Get-Content -LiteralPath $installedConfig -Raw) -replace 'minimum_process_utilization_bps\s*=\s*\d+', 'minimum_process_utilization_bps = 777'
    Write-Utf8NoBom $installedConfig $customConfig
    & (Join-Path $PackageDirectory "install.ps1") -NoTrayLaunch
    Assert-True ($LASTEXITCODE -eq 0) "implicit-config upgrade failed"
    $preservedConfig = Get-Content -LiteralPath $installedConfig -Raw
    Assert-True ($preservedConfig -match 'minimum_process_utilization_bps\s*=\s*777') "upgrade overwrote the existing configuration"

    Write-Host "lifecycle stage: normal uninstall preserves data"
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
    Assert-True (Test-Path -LiteralPath (Join-Path $InstallDirectory "winsched.log") -PathType Leaf) "normal uninstall removed logs"
    Assert-True (-not (Test-Path -LiteralPath $startupShortcut)) "normal uninstall left the Startup shortcut"
    Assert-True (-not (Test-Path -LiteralPath $programs)) "normal uninstall left the Start Menu directory"

    Write-Host "lifecycle stage: purge uninstall removes data"
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
        upgrade_preserved_threshold_bps = 777
        normal_uninstall_preserved_data = $true
        purge_removed_data = $true
        default_tray_integrity_rid = $integrityRid
        startup_tray_integrity_rid = $startupIntegrityRid
        final_tray_pid = $startupTray.Id
    } | ConvertTo-Json -Depth 4
} finally {
    Unregister-ScheduledTask -TaskName $installTask -Confirm:$false -ErrorAction SilentlyContinue
    Unregister-ScheduledTask -TaskName $startupTask -Confirm:$false -ErrorAction SilentlyContinue
}
