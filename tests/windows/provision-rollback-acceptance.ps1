[CmdletBinding()]
param(
    [string]$InstallDirectory = "$env:ProgramFiles\WinSched",
    [string]$DataDirectory = "$env:ProgramData\WinSched",
    [string]$ResultPath = "$env:PUBLIC\WinSchedFinalAcceptance\output\provision-rollback-result.json"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) {
        throw "ASSERTION FAILED: $Message"
    }
}

function Get-ServiceSnapshot {
    $service = Get-CimInstance Win32_Service -Filter "Name='WinSched'"
    Assert-True ($null -ne $service) "WinSched service is not registered"
    return [ordered]@{
        path_name = [string]$service.PathName
        start_mode = [string]$service.StartMode
        start_name = [string]$service.StartName
        display_name = [string]$service.DisplayName
        state = [string]$service.State
        description = (& "$env:SystemRoot\System32\sc.exe" qdescription WinSched | Out-String).Trim()
        failure_actions = (& "$env:SystemRoot\System32\sc.exe" qfailure WinSched | Out-String).Trim()
        failure_flag = (& "$env:SystemRoot\System32\sc.exe" qfailureflag WinSched | Out-String).Trim()
        sddl = (& "$env:SystemRoot\System32\sc.exe" sdshow WinSched | Out-String).Trim()
    }
}

function Wait-ServiceState([string]$Expected, [int]$TimeoutSeconds = 30) {
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        $service = Get-Service -Name WinSched -ErrorAction SilentlyContinue
        if ($null -ne $service -and $service.Status.ToString() -eq $Expected) {
            return
        }
        Start-Sleep -Milliseconds 250
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "WinSched did not reach service state '$Expected'"
}

function Assert-SnapshotEqual($Before, $After) {
    foreach ($field in @(
        "path_name",
        "start_mode",
        "start_name",
        "display_name",
        "state",
        "description",
        "failure_actions",
        "failure_flag",
        "sddl"
    )) {
        Assert-True ($Before[$field] -ceq $After[$field]) `
            "service field '$field' was not restored after provisioning failure"
    }
}

$serviceExe = Join-Path $InstallDirectory "winsched-service.exe"
$configPath = Join-Path $DataDirectory "winsched.toml"
$fakeSc = Join-Path $InstallDirectory "sc.exe"
$stdoutPath = Join-Path $env:TEMP "winsched-provision-rollback-stdout.txt"
$stderrPath = Join-Path $env:TEMP "winsched-provision-rollback-stderr.txt"
$result = $null
$exitCode = 1

try {
    Assert-True (Test-Path -LiteralPath $serviceExe -PathType Leaf) "service executable is missing"
    Assert-True (Test-Path -LiteralPath $configPath -PathType Leaf) "configuration is missing"
    Wait-ServiceState "Running"

    $before = Get-ServiceSnapshot
    $serviceHashBefore = (Get-FileHash -LiteralPath $serviceExe -Algorithm SHA256).Hash.ToLowerInvariant()
    $configHashBefore = (Get-FileHash -LiteralPath $configPath -Algorithm SHA256).Hash.ToLowerInvariant()

    Assert-True (-not (Test-Path -LiteralPath $fakeSc)) `
        "fault-injection target already exists in the install directory"
    Copy-Item -LiteralPath "$env:SystemRoot\System32\where.exe" -Destination $fakeSc -Force
    try {
        Remove-Item -LiteralPath $stdoutPath, $stderrPath -Force -ErrorAction SilentlyContinue
        $process = Start-Process `
            -FilePath $serviceExe `
            -ArgumentList @("provision", "--config", $configPath, "--allow-auto", "--start") `
            -RedirectStandardOutput $stdoutPath `
            -RedirectStandardError $stderrPath `
            -Wait `
            -PassThru
        $provisionExitCode = $process.ExitCode
        $commandOutput = @(
            Get-Content -LiteralPath $stdoutPath -Raw -ErrorAction SilentlyContinue
            Get-Content -LiteralPath $stderrPath -Raw -ErrorAction SilentlyContinue
        ) -join "`n"
        $commandOutput = $commandOutput.Trim()
    } finally {
        Remove-Item -LiteralPath $fakeSc -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $stdoutPath, $stderrPath -Force -ErrorAction SilentlyContinue
    }

    Assert-True ($provisionExitCode -ne 0) `
        "fault-injected provisioning unexpectedly succeeded"
    Assert-True ($commandOutput -match "sc\.exe|cannot find|not found|Command") `
        "provisioning failure did not report the injected sc.exe resolution fault"

    Wait-ServiceState "Running"
    $after = Get-ServiceSnapshot
    Assert-SnapshotEqual $before $after
    Assert-True (
        (Get-FileHash -LiteralPath $serviceExe -Algorithm SHA256).Hash.ToLowerInvariant() -eq
        $serviceHashBefore
    ) "service executable changed during failed provisioning"
    Assert-True (
        (Get-FileHash -LiteralPath $configPath -Algorithm SHA256).Hash.ToLowerInvariant() -eq
        $configHashBefore
    ) "configuration changed during failed provisioning"

    $result = [ordered]@{
        result = "PASS"
        injected_exit_code = $provisionExitCode
        service_state = $after.state
        service_path = $after.path_name
        service_sha256 = $serviceHashBefore
        config_sha256 = $configHashBefore
        restored_fields = @($before.Keys)
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
    $parent = Split-Path -Parent $ResultPath
    if ($parent) {
        New-Item -ItemType Directory -Path $parent -Force | Out-Null
    }
    [pscustomobject]$result |
        ConvertTo-Json -Depth 6 |
        Set-Content -LiteralPath $ResultPath -Encoding UTF8
}

[pscustomobject]$result | ConvertTo-Json -Depth 6
exit $exitCode
