[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory,
    [string]$InstallDirectory = "$env:ProgramFiles\WinSched",
    [string]$DataDirectory = "$env:ProgramData\WinSched",
    [string]$ExpectedVersion = "0.3.1"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -AssemblyName System.Drawing

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) {
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

function Get-AllElements {
    return [System.Windows.Automation.AutomationElement]::RootElement.FindAll(
        [System.Windows.Automation.TreeScope]::Descendants,
        [System.Windows.Automation.Condition]::TrueCondition
    )
}

function Find-Element([string]$Pattern, $ControlType) {
    foreach ($element in @(Get-AllElements)) {
        try {
            if ($element.Current.Name -like $Pattern -and
                $element.Current.ControlType -eq $ControlType) {
                return $element
            }
        } catch {
            continue
        }
    }
    return $null
}

function Find-ElementUnder($Root, [string]$Pattern, $ControlType) {
    foreach ($element in @($Root.FindAll(
        [System.Windows.Automation.TreeScope]::Descendants,
        [System.Windows.Automation.Condition]::TrueCondition
    ))) {
        try {
            if ($element.Current.Name -like $Pattern -and
                $element.Current.ControlType -eq $ControlType) {
                return $element
            }
        } catch {
            continue
        }
    }
    return $null
}

function Get-ElementText($Root) {
    $names = @()
    foreach ($element in @($Root.FindAll(
        [System.Windows.Automation.TreeScope]::Descendants,
        [System.Windows.Automation.Condition]::TrueCondition
    ))) {
        try {
            if (-not [string]::IsNullOrWhiteSpace($element.Current.Name)) {
                $names += $element.Current.Name
            }
        } catch {
            continue
        }
    }
    return $names -join "`n"
}

function Invoke-Element($Element) {
    $pattern = $null
    Assert-True ($Element.TryGetCurrentPattern(
        [System.Windows.Automation.InvokePattern]::Pattern,
        [ref]$pattern
    )) "tray icon exposes no InvokePattern"
    $pattern.Invoke()
}

function Find-TrayIcon {
    $icon = Find-Element "WinSched:*" ([System.Windows.Automation.ControlType]::Button)
    if ($null -ne $icon) {
        return $icon
    }
    $chevron = Find-Element "Show hidden icons*" ([System.Windows.Automation.ControlType]::Button)
    if ($null -ne $chevron) {
        Invoke-Element $chevron
        Start-Sleep -Milliseconds 500
        return Find-Element "WinSched:*" ([System.Windows.Automation.ControlType]::Button)
    }
    return $null
}

function Capture-Screen([string]$Path) {
    Add-Type -AssemblyName System.Windows.Forms
    $bounds = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $bitmap = New-Object System.Drawing.Bitmap($bounds.Width, $bounds.Height)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.CopyFromScreen($bounds.Location, [System.Drawing.Point]::Empty, $bounds.Size)
        $bitmap.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png)
    } finally {
        $graphics.Dispose()
        $bitmap.Dispose()
    }
}

New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$resultPath = Join-Path $OutputDirectory "tray-ui-result.json"
$screenshotPath = Join-Path $OutputDirectory "tray-responsiveness.png"
$aboutScreenshotPath = Join-Path $OutputDirectory "tray-about.png"
$statusPath = Join-Path $DataDirectory "status.json"

try {
    Assert-True (Test-Path -LiteralPath (Join-Path $InstallDirectory "winsched-tray.exe")) `
        "tray executable is missing"
    $status = Get-Content -LiteralPath $statusPath -Raw | ConvertFrom-Json
    Assert-True ([int]$status.schema_version -eq 3) "service status is not schema 3"

    $script:trayIcon = $null
    Wait-Condition "WinSched tray icon" {
        $candidate = Find-TrayIcon
        if ($null -ne $candidate) {
            $script:trayIcon = $candidate
            return $true
        }
        return $false
    }
    Invoke-Element $script:trayIcon
    Start-Sleep -Milliseconds 500

    $reserve = Find-Element "System reserve:*" ([System.Windows.Automation.ControlType]::MenuItem)
    $latency = Find-Element "Latency:*" ([System.Windows.Automation.ControlType]::MenuItem)
    $mode = Find-Element "Mode: Auto" ([System.Windows.Automation.ControlType]::MenuItem)
    $about = Find-Element "About WinSched*" ([System.Windows.Automation.ControlType]::MenuItem)
    $github = Find-Element "GitHub Repository" ([System.Windows.Automation.ControlType]::MenuItem)
    Assert-True ($null -ne $reserve) "System reserve tray row is missing"
    Assert-True ($null -ne $latency) "Latency tray row is missing"
    Assert-True ($null -ne $mode) "Mode tray row is missing"
    Assert-True ($null -ne $about) "About tray action is missing"
    Assert-True ($null -ne $github) "GitHub tray action is missing"
    Assert-True ($github.Current.IsEnabled) "GitHub tray action is disabled"
    $githubInvoke = $null
    Assert-True ($github.TryGetCurrentPattern(
        [System.Windows.Automation.InvokePattern]::Pattern,
        [ref]$githubInvoke
    )) "GitHub tray action is not invokable"
    $aboutMenuText = $about.Current.Name
    $githubMenuText = $github.Current.Name
    Capture-Screen $screenshotPath

    Invoke-Element $about
    $script:aboutWindow = $null
    Wait-Condition "About WinSched dialog" {
        $candidate = Find-Element "About WinSched" ([System.Windows.Automation.ControlType]::Window)
        if ($null -ne $candidate) {
            $script:aboutWindow = $candidate
            return $true
        }
        return $false
    }
    $aboutText = Get-ElementText $script:aboutWindow
    Assert-True ($aboutText.Contains("Version: $ExpectedVersion")) `
        "About dialog does not show version $ExpectedVersion"
    Assert-True ($aboutText.Contains("https://github.com/woffko/WinSched")) `
        "About dialog does not show the GitHub URL"
    Capture-Screen $aboutScreenshotPath
    $ok = Find-ElementUnder `
        $script:aboutWindow `
        "OK" `
        ([System.Windows.Automation.ControlType]::Button)
    Assert-True ($null -ne $ok) "About dialog has no OK button"
    Invoke-Element $ok

    [pscustomobject]@{
        result = "PASS"
        session_id = [Diagnostics.Process]::GetCurrentProcess().SessionId
        reserve_text = $reserve.Current.Name
        latency_text = $latency.Current.Name
        mode_text = $mode.Current.Name
        about_menu_text = $aboutMenuText
        github_menu_text = $githubMenuText
        github_action_invokable = $true
        about_version = $ExpectedVersion
        about_github_url = "https://github.com/woffko/WinSched"
        screenshot = $screenshotPath
        about_screenshot = $aboutScreenshotPath
    } | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $resultPath -Encoding UTF8
} catch {
    [pscustomobject]@{
        result = "FAIL"
        session_id = [Diagnostics.Process]::GetCurrentProcess().SessionId
        error = $_.Exception.ToString()
        script_stack = $_.ScriptStackTrace
    } | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $resultPath -Encoding UTF8
    exit 1
}
