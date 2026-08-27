[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$SetupPath,
    [string]$InstallDirectory = "$env:ProgramFiles\WinSched",
    [string]$DataDirectory = "$env:ProgramData\WinSched",
    [int]$ExpectedExitCode = 9,
    [string]$ResultPath = "$env:PUBLIC\WinSchedFinalAcceptance\output\setup-error-receipt-result.json"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw "ASSERTION FAILED: $Message" }
}

function Wait-Condition(
    [string]$Description,
    [scriptblock]$Condition,
    [int]$TimeoutSeconds = 90
) {
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        if (& $Condition) { return }
        Start-Sleep -Milliseconds 250
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Timed out waiting for: $Description"
}

function Quote-ProcessArgument([string]$Value) {
    if ($Value.Contains('"')) { throw "Unsupported quote in process argument" }
    return '"' + $Value + '"'
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

function Assert-ServiceSnapshotEqual($Before, $After) {
    foreach ($field in $Before.Keys) {
        Assert-True ($Before[$field] -ceq $After[$field]) `
            "service field '$field' was not restored after Setup failure"
    }
}

$configPath = Join-Path $DataDirectory "winsched.toml"
$receiptPath = Join-Path $DataDirectory "provision-result.txt"
$servicePath = Join-Path $InstallDirectory "winsched-service.exe"
$resultDirectory = Split-Path -Parent $ResultPath
$backupPath = Join-Path $resultDirectory "setup-error-receipt-config.backup"
$watcherPath = Join-Path $resultDirectory "setup-error-receipt-watcher.ps1"
$setupLog = Join-Path $resultDirectory "setup-error-receipt.log"
$systemPowerShell = "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe"
$originalConfig = $null
$watcher = $null
$result = $null
$exitCode = 1

try {
    New-Item -ItemType Directory -Path $resultDirectory -Force | Out-Null
    Assert-True (Test-Path -LiteralPath $SetupPath -PathType Leaf) "Setup is missing"
    Assert-True (Test-Path -LiteralPath $configPath -PathType Leaf) "configuration is missing"
    Assert-True (Test-Path -LiteralPath $servicePath -PathType Leaf) "service executable is missing"
    Wait-Condition "WinSched service Running before failure injection" {
        (Get-Service WinSched -ErrorAction SilentlyContinue).Status -eq "Running"
    }

    $setupHash = (Get-FileHash -LiteralPath $SetupPath -Algorithm SHA256).Hash.ToLowerInvariant()
    $serviceHash = (Get-FileHash -LiteralPath $servicePath -Algorithm SHA256).Hash.ToLowerInvariant()
    $before = Get-ServiceSnapshot
    $originalConfig = [IO.File]::ReadAllBytes($configPath)
    [IO.File]::WriteAllBytes($backupPath, $originalConfig)
    $configHash = (Get-FileHash -LiteralPath $backupPath -Algorithm SHA256).Hash.ToLowerInvariant()
    Remove-Item -LiteralPath $receiptPath -Force -ErrorAction SilentlyContinue

    $watcherSource = @'
$ErrorActionPreference = "Stop"
$deadline = [DateTime]::UtcNow.AddSeconds(90)
do {
    if (Test-Path -LiteralPath $args[0] -PathType Leaf) {
        try {
            $text = Get-Content -LiteralPath $args[0] -Raw
            if ($text -match '(?m)^ERROR') {
                [IO.File]::WriteAllBytes($args[1], [IO.File]::ReadAllBytes($args[2]))
                exit 0
            }
        } catch {
        }
    }
    Start-Sleep -Milliseconds 10
} while ([DateTime]::UtcNow -lt $deadline)
exit 1
'@
    [IO.File]::WriteAllText($watcherPath, $watcherSource, [Text.UTF8Encoding]::new($false))
    [IO.File]::WriteAllText(
        $configPath,
        "schema_version = 5`nunknown_field = true`n",
        [Text.UTF8Encoding]::new($false)
    )

    $watcherArguments = @(
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy", "Bypass",
        "-File", (Quote-ProcessArgument $watcherPath),
        (Quote-ProcessArgument $receiptPath),
        (Quote-ProcessArgument $configPath),
        (Quote-ProcessArgument $backupPath)
    ) -join " "
    $watcher = Start-Process `
        -FilePath $systemPowerShell `
        -ArgumentList $watcherArguments `
        -WindowStyle Hidden `
        -PassThru

    $setup = Start-Process `
        -FilePath $SetupPath `
        -ArgumentList @(
            "/VERYSILENT",
            "/SUPPRESSMSGBOXES",
            "/NORESTART",
            "/LOG=$setupLog"
        ) `
        -Wait `
        -PassThru
    Assert-True ($setup.ExitCode -eq $ExpectedExitCode) `
        "Setup returned $($setup.ExitCode), expected $ExpectedExitCode"
    Assert-True ($watcher.WaitForExit(95000)) "receipt watcher did not exit"
    Assert-True ($watcher.ExitCode -eq 0) "Setup did not publish an ERROR receipt"

    Wait-Condition "WinSched service restored after Setup failure" {
        (Get-Service WinSched -ErrorAction SilentlyContinue).Status -eq "Running"
    } 120
    Wait-Condition "valid controller status after Setup failure" {
        try {
            $status = Get-Content -LiteralPath (Join-Path $DataDirectory "status.json") -Raw |
                ConvertFrom-Json
            [int]$status.schema_version -eq 5 -and $null -eq $status.last_error
        } catch { $false }
    } 120

    $receipt = (Get-Content -LiteralPath $receiptPath -Raw).Trim()
    Assert-True ($receipt -match '(?m)^ERROR') "failure receipt does not contain ERROR"
    Assert-True (
        (Get-FileHash -LiteralPath $configPath -Algorithm SHA256).Hash.ToLowerInvariant() -eq
        $configHash
    ) "configuration bytes were not restored"
    Assert-True (
        (Get-FileHash -LiteralPath $servicePath -Algorithm SHA256).Hash.ToLowerInvariant() -eq
        $serviceHash
    ) "installed service executable changed"
    $after = Get-ServiceSnapshot
    Assert-ServiceSnapshotEqual $before $after

    $result = [ordered]@{
        result = "PASS"
        setup_sha256 = $setupHash
        setup_exit_code = $setup.ExitCode
        receipt = "ERROR"
        configuration_restored = $true
        service_restored = $true
        service_sha256 = $serviceHash
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
    if ($null -ne $originalConfig -and (Test-Path -LiteralPath $configPath)) {
        [IO.File]::WriteAllBytes($configPath, $originalConfig)
    }
    if ($null -ne $watcher -and -not $watcher.HasExited) {
        Stop-Process -Id $watcher.Id -Force -ErrorAction SilentlyContinue
    }
    Remove-Item -LiteralPath $receiptPath, $backupPath, $watcherPath -Force -ErrorAction SilentlyContinue
    $service = Get-Service WinSched -ErrorAction SilentlyContinue
    if ($null -ne $service -and $service.Status -ne "Running") {
        Start-Service WinSched -ErrorAction SilentlyContinue
    }
    [pscustomobject]$result |
        ConvertTo-Json -Depth 6 |
        Set-Content -LiteralPath $ResultPath -Encoding UTF8
}

[pscustomobject]$result | ConvertTo-Json -Depth 6
exit $exitCode
