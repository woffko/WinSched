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

function Get-PayloadHashes([string]$Directory) {
    $hashes = [ordered]@{}
    foreach ($name in @(
        "winsched.exe",
        "winsched-service.exe",
        "winsched-tray.exe",
        "winsched-settings.exe"
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
$result = $null
$exitCode = 1

try {
    Assert-True (Test-Path -LiteralPath $SetupPath -PathType Leaf) "Setup is missing"
    Assert-True (Test-Path -LiteralPath $configPath -PathType Leaf) "installed config is missing"
    Assert-True (Test-Path -LiteralPath $startupShortcut -PathType Leaf) "Startup task is not enabled before upgrade"
    Wait-ServiceRunning

    $beforeText = Get-Content -LiteralPath $configPath -Raw
    $withMarker = $beforeText.TrimEnd([char[]]"`r`n") + "`r`n" + $marker + "`r`n"
    [System.IO.File]::WriteAllText(
        $configPath,
        $withMarker,
        [System.Text.UTF8Encoding]::new($false)
    )
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
