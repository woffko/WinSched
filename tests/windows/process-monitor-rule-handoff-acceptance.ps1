[CmdletBinding()]
param(
    [string]$InstallDirectory = (Join-Path ([Environment]::GetFolderPath("ProgramFiles")) "WinSched"),
    [string]$DataDirectory = (Join-Path ([Environment]::GetFolderPath("CommonApplicationData")) "WinSched"),
    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes

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

function Get-Window($Process) {
    $Process.Refresh()
    if ($Process.MainWindowHandle -eq [IntPtr]::Zero) { return $null }
    return [System.Windows.Automation.AutomationElement]::FromHandle($Process.MainWindowHandle)
}

function Test-EditValue($Window, [string]$Expected) {
    if ($null -eq $Window) { return $false }
    $elements = $Window.FindAll(
        [System.Windows.Automation.TreeScope]::Descendants,
        [System.Windows.Automation.Condition]::TrueCondition
    )
    foreach ($element in $elements) {
        try {
            if ($element.Current.ControlType -ne [System.Windows.Automation.ControlType]::Edit) {
                continue
            }
            $pattern = $null
            if ($element.TryGetCurrentPattern(
                [System.Windows.Automation.ValuePattern]::Pattern,
                [ref]$pattern
            ) -and $pattern.Current.Value -eq $Expected) {
                return $true
            }
        } catch {
            continue
        }
    }
    return $false
}

New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$resultPath = Join-Path $OutputDirectory "process-monitor-rule-handoff-result.json"
$settingsPath = Join-Path $InstallDirectory "winsched-settings.exe"
$configPath = Join-Path $DataDirectory "winsched.toml"
$firstImage = "winsched-monitor-draft-one.exe"
$secondImage = "winsched-monitor-draft-two.exe"
$settings = $null

try {
    Assert-True (Test-Path -LiteralPath $settingsPath -PathType Leaf) `
        "installed Settings is missing"
    Assert-True (Test-Path -LiteralPath $configPath -PathType Leaf) `
        "installed configuration is missing"
    Get-Process -Name "winsched-settings" -ErrorAction SilentlyContinue |
        Stop-Process -Force -ErrorAction SilentlyContinue
    $beforeHash = (Get-FileHash -LiteralPath $configPath -Algorithm SHA256).Hash.ToLowerInvariant()

    $settings = Start-Process `
        -FilePath $settingsPath `
        -WorkingDirectory $InstallDirectory `
        -ArgumentList @("--rule-image", $firstImage) `
        -PassThru
    Wait-Condition "first exact-rule draft" {
        Test-EditValue (Get-Window $settings) $firstImage
    } 30
    Assert-True (
        (Get-FileHash -LiteralPath $configPath -Algorithm SHA256).Hash.ToLowerInvariant() -eq
            $beforeHash
    ) "first draft changed configuration before Apply"

    $second = Start-Process `
        -FilePath $settingsPath `
        -WorkingDirectory $InstallDirectory `
        -ArgumentList @("--rule-image", $secondImage) `
        -PassThru
    Assert-True ($second.WaitForExit(15000)) `
        "second Settings handoff process did not exit"
    Assert-True (-not $settings.HasExited) `
        "existing Settings process exited during handoff"
    Wait-Condition "second exact-rule draft delivered to existing Settings" {
        Test-EditValue (Get-Window $settings) $secondImage
    } 30
    Assert-True (
        (Get-FileHash -LiteralPath $configPath -Algorithm SHA256).Hash.ToLowerInvariant() -eq
            $beforeHash
    ) "second draft changed configuration before Apply"

    [pscustomobject]@{
        result = "PASS"
        first_draft_prefilled = $true
        second_draft_forwarded_to_existing_instance = $true
        single_settings_instance = $true
        config_unchanged_without_apply = $true
        config_sha256 = $beforeHash
    } | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $resultPath -Encoding UTF8
} catch {
    [pscustomobject]@{
        result = "FAIL"
        error = $_.Exception.ToString()
        script_stack = $_.ScriptStackTrace
    } | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $resultPath -Encoding UTF8
    exit 1
} finally {
    if ($null -ne $settings) {
        Stop-Process -Id $settings.Id -Force -ErrorAction SilentlyContinue
    }
}
