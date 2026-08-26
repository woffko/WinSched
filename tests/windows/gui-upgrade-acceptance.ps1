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
$legacyImage = "winsched-legacy-$([Guid]::NewGuid().ToString('N').Substring(0, 8)).exe"
$backgroundStatePath = Join-Path "$env:ProgramData\WinSched" 'background-state.json'
$result = $null
$exitCode = 1

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
        Assert-True ($entry.Value -eq '0.4.0') `
            "upgrade prerequisite $($entry.Key) is $($entry.Value), expected exactly 0.4.0"
    }

    $preUpgradeStatus = Get-Content `
        -LiteralPath (Join-Path "$env:ProgramData\WinSched" 'status.json') `
        -Raw |
        ConvertFrom-Json
    Assert-True ([int]$preUpgradeStatus.schema_version -eq 3) `
        "0.4.0 service status is not schema 3 before upgrade"
    if (Test-Path -LiteralPath $backgroundStatePath -PathType Leaf) {
        $preUpgradeBackgroundState = Get-Content `
            -LiteralPath $backgroundStatePath `
            -Raw |
            ConvertFrom-Json
        Assert-True (@($preUpgradeBackgroundState.processes).Count -eq 0) `
            "upgrade prerequisite contains stale background ownership"
    }

    $beforeText = Get-Content -LiteralPath $configPath -Raw
    $schemaMatch = [regex]::Match(
        $beforeText,
        '(?m)^\s*schema_version\s*=\s*(?<schema>\d+)\s*$'
    )
    Assert-True $schemaMatch.Success "installed config has no schema_version before upgrade"
    Assert-True ([int]$schemaMatch.Groups['schema'].Value -eq 3) `
        "upgrade prerequisite config schema is $($schemaMatch.Groups['schema'].Value), expected exactly 3"
    $withMarker = $beforeText.TrimEnd([char[]]"`r`n") + @"

$marker

[[rules]]
image = "$legacyImage"
mode = "sticky"
profile = "background"
"@
    [System.IO.File]::WriteAllText(
        $configPath,
        $withMarker,
        [System.Text.UTF8Encoding]::new($false)
    )
    $oldCli = Join-Path $installDirectory 'winsched.exe'
    $legacyBeforeUpgrade = (& $oldCli config-check $configPath --json | Out-String) |
        ConvertFrom-Json
    Assert-True ($LASTEXITCODE -eq 0) "0.4.0 rejected the schema-3 legacy background fixture"
    Assert-True ([int]$legacyBeforeUpgrade.schema_version -eq 3) `
        "0.4.0 did not normalize the fixture as schema 3"
    $oldLegacyRule = @($legacyBeforeUpgrade.rules | Where-Object {
        $_.image -eq $legacyImage
    })
    Assert-True ($oldLegacyRule.Count -eq 1) `
        "0.4.0 did not preserve exactly one legacy background rule"
    Assert-True ($oldLegacyRule[0].profile -eq 'background') `
        "0.4.0 did not interpret the legacy rule as the old background placement profile"
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
    Wait-ServiceRunning

    $configHashAfter = (Get-FileHash -LiteralPath $configPath -Algorithm SHA256).Hash.ToLowerInvariant()
    Assert-True ($configHashAfter -eq $configHashBefore) "upgrade changed config bytes"
    Assert-True ((Get-Content -LiteralPath $configPath -Raw).Contains($marker)) `
        "upgrade removed the config marker"

    $newCli = Join-Path $installDirectory 'winsched.exe'
    $normalizedAfterUpgrade = (& $newCli config-check $configPath --json | Out-String) |
        ConvertFrom-Json
    Assert-True ($LASTEXITCODE -eq 0) "0.5.0 rejected the preserved schema-3 configuration"
    Assert-True ([int]$normalizedAfterUpgrade.schema_version -eq 4) `
        "0.5.0 did not normalize the schema-3 configuration to schema 4 in memory"
    Assert-True (-not [bool]$normalizedAfterUpgrade.background_efficiency.enabled) `
        "legacy schema unexpectedly enabled background efficiency"
    $migratedLegacyRule = @($normalizedAfterUpgrade.rules | Where-Object {
        $_.image -eq $legacyImage
    })
    Assert-True ($migratedLegacyRule.Count -eq 1) `
        "0.5.0 did not preserve exactly one migrated legacy rule"
    Assert-True ($migratedLegacyRule[0].profile -eq 'balanced') `
        "legacy Background rule did not migrate to Balanced placement semantics"

    Wait-Condition "schema-4 service status after upgrade" {
        try {
            $status = Get-Content `
                -LiteralPath (Join-Path "$env:ProgramData\WinSched" 'status.json') `
                -Raw |
                ConvertFrom-Json
            [int]$status.schema_version -eq 4 -and
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
            "legacy Background migration created QoS ownership records"
    }

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
        pre_upgrade_config_schema = 3
        legacy_background_image = $legacyImage
        legacy_profile_before_upgrade = $oldLegacyRule[0].profile
        migrated_profile_after_upgrade = $migratedLegacyRule[0].profile
        legacy_background_qos_enabled = $false
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
    [pscustomobject]$result |
        ConvertTo-Json -Depth 7 |
        Set-Content -LiteralPath $resultPath -Encoding UTF8
}

[pscustomobject]$result | ConvertTo-Json -Depth 7
exit $exitCode
