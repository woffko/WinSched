[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$SetupPath,
    [Parameter(Mandatory = $true)]
    [string]$PayloadDirectory,
    [string]$OutputDirectory = "$env:PUBLIC\WinSchedFinalAcceptance\output"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) {
        throw "ASSERTION FAILED: $Message"
    }
}

function Wait-ServiceRunning([int]$TimeoutSeconds = 30) {
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        $service = Get-Service -Name WinSched -ErrorAction SilentlyContinue
        if ($null -ne $service -and $service.Status -eq "Running") {
            return
        }
        Start-Sleep -Milliseconds 250
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "WinSched service did not return to Running after upgrade"
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

function Set-FileAtomically([string]$Path, [byte[]]$Bytes) {
    $directory = Split-Path -Parent $Path
    $temporaryPath = Join-Path $directory (
        ".{0}.gui-upgrade-{1}.tmp" -f (Split-Path -Leaf $Path), [Guid]::NewGuid().ToString("N")
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

function Set-Utf8FileAtomically([string]$Path, [string]$Text) {
    Set-FileAtomically $Path ([Text.UTF8Encoding]::new($false).GetBytes($Text))
}

function Set-LegacyLoggingEnabled([string]$Text, [bool]$Enabled) {
    $encoded = if ($Enabled) { "true" } else { "false" }
    $sectionPattern = '(?ms)^\s*\[logging\]\s*(?<body>.*?)(?=^\s*\[|\z)'
    $section = [regex]::Match($Text, $sectionPattern)
    if (-not $section.Success) {
        return $Text.TrimEnd([char[]]"`r`n") + "`r`n`r`n[logging]`r`nenabled = $encoded`r`n"
    }
    $body = $section.Groups['body'].Value
    Assert-True (-not ([regex]::IsMatch($body, '(?m)^\s*level\s*='))) `
        "schema-4 fixture unexpectedly contains logging.level"
    if ([regex]::IsMatch($body, '(?m)^\s*enabled\s*=\s*(true|false)\s*$')) {
        $updatedBody = [regex]::Replace(
            $body,
            '(?m)^\s*enabled\s*=\s*(true|false)\s*$',
            "enabled = $encoded",
            1
        )
    } else {
        $updatedBody = "enabled = $encoded`r`n$body"
    }
    return $Text.Substring(0, $section.Index) +
        "[logging]`r`n$updatedBody" +
        $Text.Substring($section.Index + $section.Length)
}

function Get-ConsoleVersion([string]$Path, [string]$ProgramName) {
    Assert-True (Test-Path -LiteralPath $Path -PathType Leaf) `
        "installed binary is missing before upgrade: $ProgramName"
    $output = (& $Path --version | Out-String).Trim()
    Assert-True ($LASTEXITCODE -eq 0) `
        "$ProgramName --version returned exit code $LASTEXITCODE"
    $match = [regex]::Match($output, '^\S+\s+(?<version>\d+\.\d+\.\d+)$')
    Assert-True $match.Success "unexpected $ProgramName --version output: $output"
    return $match.Groups['version'].Value
}

function Get-GuiVersion([string]$Path, [string]$ProgramName) {
    Assert-True (Test-Path -LiteralPath $Path -PathType Leaf) `
        "installed binary is missing before upgrade: $ProgramName"
    $version = (Get-Item -LiteralPath $Path).VersionInfo.ProductVersion
    Assert-True (-not [string]::IsNullOrWhiteSpace($version)) `
        "$ProgramName has no ProductVersion resource"
    return $version.Trim()
}

function Get-PayloadHashes([string]$Directory) {
    $hashes = [ordered]@{}
    foreach ($name in @(
        "winsched.exe",
        "winsched-service.exe",
        "winsched-tray.exe",
        "winsched-settings.exe",
        "README.md"
    )) {
        $path = Join-Path $Directory $name
        Assert-True (Test-Path -LiteralPath $path -PathType Leaf) "payload binary is missing: $name"
        $hashes[$name] = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
    }
    return $hashes
}

New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$resultPath = Join-Path $OutputDirectory "gui-upgrade-result.json"
$logPath = Join-Path $OutputDirectory "gui-upgrade.log"
$configPath = "$env:ProgramData\WinSched\winsched.toml"
$installDirectory = "$env:ProgramFiles\WinSched"
$startupShortcut = "$env:ProgramData\Microsoft\Windows\Start Menu\Programs\Startup\WinSched Tray.lnk"
$settingsShortcut = "$env:ProgramData\Microsoft\Windows\Start Menu\Programs\WinSched\WinSched Settings.lnk"
$marker = "# gui-upgrade-preserve-marker-$([Guid]::NewGuid().ToString('N'))"
$backgroundImage = "winsched-background-$([Guid]::NewGuid().ToString('N').Substring(0, 8)).exe"
$backgroundStatePath = Join-Path "$env:ProgramData\WinSched" 'background-state.json'
$result = $null
$exitCode = 1
$recoveryBackupPath = Join-Path $OutputDirectory "gui-upgrade-original-config.bin"
$originalConfigBytes = $null
$originalConfigHash = $null
$fixtureInstalled = $false
$upgradeCompleted = $false
$originalMode = $null
$originalLegacyLoggingEnabled = $null
$cleanupErrors = New-Object System.Collections.ArrayList

try {
    Assert-True (Test-Path -LiteralPath $SetupPath -PathType Leaf) "Setup is missing"
    Assert-True (Test-Path -LiteralPath $configPath -PathType Leaf) "installed config is missing"
    Assert-True (Test-Path -LiteralPath $startupShortcut -PathType Leaf) "Startup task is not enabled before upgrade"
    Wait-ServiceRunning

    $preUpgradeVersions = [ordered]@{
        'winsched.exe' = Get-ConsoleVersion `
            (Join-Path $installDirectory 'winsched.exe') `
            'winsched.exe'
        'winsched-service.exe' = Get-ConsoleVersion `
            (Join-Path $installDirectory 'winsched-service.exe') `
            'winsched-service.exe'
        'winsched-tray.exe' = Get-GuiVersion `
            (Join-Path $installDirectory 'winsched-tray.exe') `
            'winsched-tray.exe'
        'winsched-settings.exe' = Get-GuiVersion `
            (Join-Path $installDirectory 'winsched-settings.exe') `
            'winsched-settings.exe'
    }
    foreach ($entry in $preUpgradeVersions.GetEnumerator()) {
        Assert-True ($entry.Value -eq '0.5.0') `
            "upgrade prerequisite $($entry.Key) is $($entry.Value), expected exactly 0.5.0"
    }

    $preUpgradeStatus = Get-Content `
        -LiteralPath (Join-Path "$env:ProgramData\WinSched" 'status.json') `
        -Raw |
        ConvertFrom-Json
    Assert-True ([int]$preUpgradeStatus.schema_version -eq 4) `
        "0.5.0 service status is not schema 4 before upgrade"
    $originalMode = [string]$preUpgradeStatus.configured_mode
    $originalLegacyLoggingEnabled = [bool]$preUpgradeStatus.applied_logging.enabled
    if (Test-Path -LiteralPath $backgroundStatePath -PathType Leaf) {
        $preUpgradeBackgroundState = Get-Content `
            -LiteralPath $backgroundStatePath `
            -Raw |
            ConvertFrom-Json
        Assert-True (@($preUpgradeBackgroundState.processes).Count -eq 0) `
            "upgrade prerequisite contains stale background ownership"
    }

    $originalConfigBytes = [IO.File]::ReadAllBytes($configPath)
    $originalConfigHash = (Get-FileHash -LiteralPath $configPath -Algorithm SHA256).Hash.ToLowerInvariant()
    [IO.File]::WriteAllBytes($recoveryBackupPath, $originalConfigBytes)
    Assert-True (
        (Get-FileHash -LiteralPath $recoveryBackupPath -Algorithm SHA256).Hash.ToLowerInvariant() -eq
        $originalConfigHash
    ) "could not verify the original configuration recovery copy"
    $beforeText = [Text.UTF8Encoding]::new($false, $true).GetString($originalConfigBytes)
    $schemaMatch = [regex]::Match(
        $beforeText,
        '(?m)^\s*schema_version\s*=\s*(?<schema>\d+)\s*$'
    )
    Assert-True $schemaMatch.Success "installed config has no schema_version before upgrade"
    Assert-True ([int]$schemaMatch.Groups['schema'].Value -eq 4) `
        "upgrade prerequisite config schema is $($schemaMatch.Groups['schema'].Value), expected exactly 4"
    $withMarker = $beforeText.TrimEnd([char[]]"`r`n") + @"

$marker

[[rules]]
image = "$backgroundImage"
mode = "sticky"
profile = "background"
"@
    $withMarker = Set-LegacyLoggingEnabled $withMarker $false
    $fixtureInstalled = $true
    Set-Utf8FileAtomically $configPath $withMarker
    $oldCli = Join-Path $installDirectory 'winsched.exe'
    $beforeUpgrade = (& $oldCli config-check $configPath --json | Out-String) |
        ConvertFrom-Json
    Assert-True ($LASTEXITCODE -eq 0) "0.5.0 rejected the schema-4 Background fixture"
    Assert-True ([int]$beforeUpgrade.schema_version -eq 4) `
        "0.5.0 did not normalize the fixture as schema 4"
    $oldBackgroundRule = @($beforeUpgrade.rules | Where-Object {
        $_.image -eq $backgroundImage
    })
    Assert-True ($oldBackgroundRule.Count -eq 1) `
        "0.5.0 did not preserve exactly one Background rule"
    Assert-True ($oldBackgroundRule[0].profile -eq 'background') `
        "0.5.0 did not preserve schema-4 Background semantics"
    Assert-True (-not [bool]$beforeUpgrade.logging.enabled) `
        "upgrade fixture did not explicitly set schema-4 logging.enabled=false"
    $configHashBefore = (Get-FileHash -LiteralPath $configPath -Algorithm SHA256).Hash.ToLowerInvariant()
    $setupHash = (Get-FileHash -LiteralPath $SetupPath -Algorithm SHA256).Hash.ToLowerInvariant()
    $payloadHashes = Get-PayloadHashes $PayloadDirectory

    $process = Start-Process `
        -FilePath $SetupPath `
        -ArgumentList @(
            "/VERYSILENT",
            "/SUPPRESSMSGBOXES",
            "/NORESTART",
            "/LOG=$logPath"
        ) `
        -Wait `
        -PassThru
    Assert-True ($process.ExitCode -eq 0) "Setup upgrade returned $($process.ExitCode)"
    $upgradeCompleted = $true
    Wait-ServiceRunning

    $configHashAfter = (Get-FileHash -LiteralPath $configPath -Algorithm SHA256).Hash.ToLowerInvariant()
    Assert-True ($configHashAfter -eq $configHashBefore) "upgrade changed config bytes"
    Assert-True ((Get-Content -LiteralPath $configPath -Raw).Contains($marker)) `
        "upgrade removed the config marker"

    $newCli = Join-Path $installDirectory 'winsched.exe'
    $normalizedAfterUpgrade = (& $newCli config-check $configPath --json | Out-String) |
        ConvertFrom-Json
    Assert-True ($LASTEXITCODE -eq 0) "0.5.1 rejected the preserved schema-4 configuration"
    Assert-True ([int]$normalizedAfterUpgrade.schema_version -eq 5) `
        "0.5.1 did not normalize the schema-4 configuration to schema 5 in memory"
    Assert-True (-not [bool]$normalizedAfterUpgrade.background_efficiency.enabled) `
        "schema-4 upgrade unexpectedly enabled background efficiency"
    Assert-True ([string]$normalizedAfterUpgrade.logging.level -eq 'off') `
        "schema-4 logging.enabled=false did not migrate to off"
    $migratedBackgroundRule = @($normalizedAfterUpgrade.rules | Where-Object {
        $_.image -eq $backgroundImage
    })
    Assert-True ($migratedBackgroundRule.Count -eq 1) `
        "0.5.1 did not preserve exactly one schema-4 Background rule"
    Assert-True ($migratedBackgroundRule[0].profile -eq 'background') `
        "schema-4 Background rule was incorrectly normalized to another profile"

    Wait-Condition "schema-5 service status after upgrade" {
        try {
            $status = Get-Content `
                -LiteralPath (Join-Path "$env:ProgramData\WinSched" 'status.json') `
                -Raw |
                ConvertFrom-Json
            [int]$status.schema_version -eq 5 -and
                [string]$status.applied_logging.level -eq "off" -and
                -not [bool]$status.applied_background_efficiency.enabled -and
                [int]$status.background_efficiency.managed_processes -eq 0
        } catch {
            $false
        }
    } 45
    if (Test-Path -LiteralPath $backgroundStatePath -PathType Leaf) {
        $backgroundState = Get-Content -LiteralPath $backgroundStatePath -Raw |
            ConvertFrom-Json
        Assert-True (@($backgroundState.processes).Count -eq 0) `
            "schema-4 Background upgrade created QoS ownership records"
    }

    $offStatus = Get-Content `
        -LiteralPath (Join-Path "$env:ProgramData\WinSched" 'status.json') `
        -Raw |
        ConvertFrom-Json
    $trueReloadBaseline = [uint64]$offStatus.config_reload_sequence
    $legacyTrueText = Set-LegacyLoggingEnabled `
        (Get-Content -LiteralPath $configPath -Raw) `
        $true
    Set-Utf8FileAtomically $configPath $legacyTrueText
    Wait-Condition "schema-4 logging.enabled=true hot migration" {
        try {
            $status = Get-Content `
                -LiteralPath (Join-Path "$env:ProgramData\WinSched" 'status.json') `
                -Raw |
                ConvertFrom-Json
            [uint64]$status.config_reload_sequence -gt $trueReloadBaseline -and
                [string]$status.config_reload_result -eq "reloaded" -and
                [string]$status.applied_logging.level -eq "normal"
        } catch {
            $false
        }
    } 30
    $normalizedLegacyTrue = (& $newCli config-check $configPath --json | Out-String) |
        ConvertFrom-Json
    Assert-True ($LASTEXITCODE -eq 0) "0.5.1 rejected schema-4 logging.enabled=true"
    Assert-True ([string]$normalizedLegacyTrue.logging.level -eq "normal") `
        "schema-4 logging.enabled=true did not migrate to normal"

    $installedHashes = [ordered]@{}
    foreach ($name in $payloadHashes.Keys) {
        $installed = Join-Path $installDirectory $name
        Assert-True (Test-Path -LiteralPath $installed -PathType Leaf) "installed binary is missing: $name"
        $installedHashes[$name] = (Get-FileHash -LiteralPath $installed -Algorithm SHA256).Hash.ToLowerInvariant()
        Assert-True ($installedHashes[$name] -eq $payloadHashes[$name]) `
            "installed $name does not match frozen payload"
    }

    $service = Get-CimInstance Win32_Service -Filter "Name='WinSched'"
    Assert-True ($service.State -eq "Running") "service is not Running after upgrade"
    Assert-True ($service.StartMode -eq "Auto") "service is not Automatic after upgrade"
    Assert-True ($service.StartName -eq "LocalSystem") "service account changed during upgrade"
    Assert-True ($service.PathName -match "Program Files\\WinSched\\winsched-service.exe") `
        "service ImagePath left Program Files"
    Assert-True (Test-Path -LiteralPath $startupShortcut -PathType Leaf) `
        "upgrade lost the persisted Startup task"
    Assert-True (Test-Path -LiteralPath $settingsShortcut -PathType Leaf) `
        "upgrade lost the Settings shortcut"

    $result = [ordered]@{
        result = "PASS"
        setup_sha256 = $setupHash
        setup_exit_code = $process.ExitCode
        config_sha256_before = $configHashBefore
        config_sha256_after = $configHashAfter
        config_byte_identical = $true
        marker = $marker
        pre_upgrade_versions = $preUpgradeVersions
        pre_upgrade_config_schema = 4
        background_image = $backgroundImage
        profile_before_upgrade = $oldBackgroundRule[0].profile
        profile_after_upgrade = $migratedBackgroundRule[0].profile
        logging_level_after_upgrade = $normalizedAfterUpgrade.logging.level
        logging_false_through_setup = "off"
        logging_true_hot_reload = $normalizedLegacyTrue.logging.level
        background_qos_enabled = $false
        service_state = $service.State
        service_path = $service.PathName
        installed_sha256 = $installedHashes
    }
    $exitCode = 0
} catch {
    $result = [ordered]@{
        result = "FAIL"
        error = $_.Exception.ToString()
        script_stack = $_.ScriptStackTrace
    }
    $exitCode = 1
} finally {
    if ($fixtureInstalled -and $null -ne $originalConfigBytes) {
        try {
            $statusBeforeRestore = Get-Content `
                -LiteralPath (Join-Path "$env:ProgramData\WinSched" 'status.json') `
                -Raw |
                ConvertFrom-Json
            $restoreSequence = [uint64]$statusBeforeRestore.config_reload_sequence
            Set-FileAtomically $configPath $originalConfigBytes
            Assert-True (
                (Get-FileHash -LiteralPath $configPath -Algorithm SHA256).Hash.ToLowerInvariant() -eq
                $originalConfigHash
            ) "original configuration bytes were not restored exactly"
            if ($upgradeCompleted) {
                $expectedLevel = if ($originalLegacyLoggingEnabled) { "normal" } else { "off" }
                Wait-Condition "service reload of restored original configuration" {
                    try {
                        $status = Get-Content `
                            -LiteralPath (Join-Path "$env:ProgramData\WinSched" 'status.json') `
                            -Raw |
                            ConvertFrom-Json
                        [int]$status.schema_version -eq 5 -and
                            [uint64]$status.config_reload_sequence -gt $restoreSequence -and
                            [string]$status.config_reload_result -eq "reloaded" -and
                            [string]$status.configured_mode -eq $originalMode -and
                            [string]$status.applied_logging.level -eq $expectedLevel
                    } catch {
                        $false
                    }
                } 30
            }
            $result["original_config_restored"] = $true
            Remove-Item -LiteralPath $recoveryBackupPath -Force -ErrorAction SilentlyContinue
        } catch {
            [void]$cleanupErrors.Add($_.Exception.Message)
            $result["original_config_restored"] = $false
            $result["recovery_backup_retained"] = `
                Test-Path -LiteralPath $recoveryBackupPath -PathType Leaf
        }
    }
    $result["cleanup_completed"] = $cleanupErrors.Count -eq 0
    $result["cleanup_errors"] = @($cleanupErrors)
    if ($cleanupErrors.Count -gt 0) {
        $result["result"] = "FAIL"
        $result["error"] = "Upgrade cleanup failed: $(@($cleanupErrors) -join '; ')"
        $exitCode = 1
    }
    [pscustomobject]$result |
        ConvertTo-Json -Depth 7 |
        Set-Content -LiteralPath $resultPath -Encoding UTF8
}

[pscustomobject]$result | ConvertTo-Json -Depth 7
exit $exitCode
