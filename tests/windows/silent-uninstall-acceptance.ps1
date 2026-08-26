[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$SetupPath,
    [ValidateSet("Preserve", "Purge")]
    [string]$Scenario,
    [string]$OutputDirectory = "$env:PUBLIC\WinSchedFinalAcceptance\output"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) {
        throw "ASSERTION FAILED: $Message"
    }
}

function Wait-Condition([string]$Description, [scriptblock]$Condition, [int]$TimeoutSeconds = 45) {
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        if (& $Condition) {
            return
        }
        Start-Sleep -Milliseconds 250
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Timed out waiting for: $Description"
}

New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$scenarioName = $Scenario.ToLowerInvariant()
$resultPath = Join-Path $OutputDirectory "silent-uninstall-$scenarioName-result.json"
$installDirectory = "$env:ProgramFiles\WinSched"
$dataDirectory = "$env:ProgramData\WinSched"
$configPath = Join-Path $dataDirectory "winsched.toml"
$markerPath = Join-Path $dataDirectory "silent-uninstall-acceptance.marker"
$startupShortcut = "$env:ProgramData\Microsoft\Windows\Start Menu\Programs\Startup\WinSched Tray.lnk"
$startMenu = "$env:ProgramData\Microsoft\Windows\Start Menu\Programs\WinSched"
$result = $null
$exitCode = 1

try {
    Assert-True (Test-Path -LiteralPath $SetupPath -PathType Leaf) "Setup is missing"
    $setup = Start-Process `
        -FilePath $SetupPath `
        -ArgumentList @("/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART") `
        -Wait `
        -PassThru
    Assert-True ($setup.ExitCode -eq 0) "Setup returned $($setup.ExitCode)"
    Wait-Condition "WinSched service Running before uninstall" {
        (Get-Service WinSched -ErrorAction SilentlyContinue).Status -eq "Running"
    }

    Assert-True (Test-Path -LiteralPath $configPath -PathType Leaf) "config is missing"
    $marker = "silent-uninstall-$Scenario-$([Guid]::NewGuid().ToString('N'))"
    Set-Content -LiteralPath $markerPath -Value $marker -Encoding UTF8
    $configHashBefore = (Get-FileHash -LiteralPath $configPath -Algorithm SHA256).Hash.ToLowerInvariant()
    $setupHash = (Get-FileHash -LiteralPath $SetupPath -Algorithm SHA256).Hash.ToLowerInvariant()

    $uninstaller = @(
        Get-ChildItem -LiteralPath $installDirectory -Filter "unins*.exe" -File |
            Where-Object Name -match '^unins\d+\.exe$' |
            Sort-Object LastWriteTimeUtc -Descending
    )[0].FullName
    Assert-True (Test-Path -LiteralPath $uninstaller -PathType Leaf) "uninstaller is missing"
    $arguments = @("/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART")
    if ($Scenario -eq "Purge") {
        $arguments += "/PURGEDATA"
    }
    $uninstall = Start-Process `
        -FilePath $uninstaller `
        -ArgumentList $arguments `
        -Wait `
        -PassThru
    Assert-True ($uninstall.ExitCode -eq 0) "uninstaller returned $($uninstall.ExitCode)"

    Wait-Condition "WinSched service absent" {
        $null -eq (Get-Service WinSched -ErrorAction SilentlyContinue)
    }
    Wait-Condition "WinSched Program Files absent" {
        -not (Test-Path -LiteralPath $installDirectory)
    }
    Assert-True (-not (Test-Path -LiteralPath $startupShortcut)) "Startup shortcut remains"
    Assert-True (-not (Test-Path -LiteralPath $startMenu)) "Start Menu group remains"

    if ($Scenario -eq "Preserve") {
        Assert-True (Test-Path -LiteralPath $dataDirectory -PathType Container) `
            "preserve uninstall removed ProgramData"
        Assert-True (Test-Path -LiteralPath $configPath -PathType Leaf) `
            "preserve uninstall removed config"
        Assert-True (Test-Path -LiteralPath $markerPath -PathType Leaf) `
            "preserve uninstall removed marker"
        $configHashAfter = (Get-FileHash -LiteralPath $configPath -Algorithm SHA256).Hash.ToLowerInvariant()
        Assert-True ($configHashAfter -eq $configHashBefore) `
            "preserve uninstall changed config bytes"
        Remove-Item -LiteralPath $markerPath -Force
        Assert-True (-not (Test-Path -LiteralPath $markerPath)) `
            "silent preserve acceptance marker cleanup failed"
    } else {
        Assert-True (-not (Test-Path -LiteralPath $dataDirectory)) `
            "purge uninstall left ProgramData"
        $configHashAfter = $null
    }

    $result = [ordered]@{
        result = "PASS"
        scenario = $Scenario
        setup_sha256 = $setupHash
        setup_exit_code = $setup.ExitCode
        uninstall_exit_code = $uninstall.ExitCode
        service_removed = $true
        program_files_removed = $true
        shortcuts_removed = $true
        data_preserved = ($Scenario -eq "Preserve")
        data_purged = ($Scenario -eq "Purge")
        config_sha256_before = $configHashBefore
        config_sha256_after = $configHashAfter
        acceptance_marker_cleaned = ($Scenario -eq "Preserve")
    }
    $exitCode = 0
} catch {
    $result = [ordered]@{
        result = "FAIL"
        scenario = $Scenario
        error = $_.Exception.ToString()
        script_stack = $_.ScriptStackTrace
    }
    $exitCode = 1
} finally {
    [pscustomobject]$result |
        ConvertTo-Json -Depth 6 |
        Set-Content -LiteralPath $resultPath -Encoding UTF8
}

[pscustomobject]$result | ConvertTo-Json -Depth 6
exit $exitCode
