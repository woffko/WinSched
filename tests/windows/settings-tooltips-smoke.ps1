[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory,
    [string]$InstallDirectory = "$env:ProgramFiles\WinSched",
    [string]$DataDirectory = "$env:ProgramData\WinSched",
    [string]$ResultFileName = "settings-tooltips-result.json"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) {
        throw "ASSERTION FAILED: $Message"
    }
}

function Wait-Condition([string]$Description, [scriptblock]$Condition, [int]$TimeoutSeconds = 20) {
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        if (& $Condition) {
            return
        }
        Start-Sleep -Milliseconds 200
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Timed out waiting for: $Description"
}

function Get-AllElements($Root) {
    return $Root.FindAll(
        [System.Windows.Automation.TreeScope]::Descendants,
        [System.Windows.Automation.Condition]::TrueCondition
    )
}

function Find-Element($Root, [string]$Name, [bool]$Actionable = $false) {
    foreach ($element in @(Get-AllElements $Root)) {
        try {
            if ($element.Current.Name -ne $Name -or $element.Current.IsOffscreen) {
                continue
            }
            if (-not $Actionable) {
                return $element
            }
            foreach ($patternId in @(
                [System.Windows.Automation.InvokePattern]::Pattern,
                [System.Windows.Automation.SelectionItemPattern]::Pattern,
                [System.Windows.Automation.TogglePattern]::Pattern
            )) {
                $pattern = $null
                if ($element.TryGetCurrentPattern($patternId, [ref]$pattern)) {
                    return $element
                }
            }
        } catch {
            continue
        }
    }
    return $null
}

function Wait-Element($Root, [string]$Name, [bool]$Actionable = $false) {
    $script:elementCandidate = $null
    Wait-Condition "element '$Name'" {
        $candidate = Find-Element $Root $Name $Actionable
        if ($null -ne $candidate) {
            $script:elementCandidate = $candidate
            return $true
        }
        return $false
    }
    return $script:elementCandidate
}

function Invoke-Element($Element) {
    foreach ($patternId in @(
        [System.Windows.Automation.InvokePattern]::Pattern,
        [System.Windows.Automation.SelectionItemPattern]::Pattern,
        [System.Windows.Automation.TogglePattern]::Pattern
    )) {
        $pattern = $null
        if ($Element.TryGetCurrentPattern($patternId, [ref]$pattern)) {
            if ($patternId -eq [System.Windows.Automation.InvokePattern]::Pattern) {
                $pattern.Invoke()
            } elseif ($patternId -eq [System.Windows.Automation.SelectionItemPattern]::Pattern) {
                $pattern.Select()
            } else {
                $pattern.Toggle()
            }
            return
        }
    }
    throw "Element '$($Element.Current.Name)' is not actionable"
}

function Find-TopLevelWindow([int]$ProcessId) {
    foreach ($window in [System.Windows.Automation.AutomationElement]::RootElement.FindAll(
        [System.Windows.Automation.TreeScope]::Children,
        [System.Windows.Automation.Condition]::TrueCondition
    )) {
        try {
            if ($window.Current.ProcessId -eq $ProcessId -and
                $window.Current.ControlType -eq [System.Windows.Automation.ControlType]::Window -and
                $window.Current.Name -eq "WinSched Settings") {
                return $window
            }
        } catch {
            continue
        }
    }
    return $null
}

function Capture-Window($Window, [string]$Path) {
    $rect = $Window.Current.BoundingRectangle
    $bitmap = New-Object System.Drawing.Bitmap(
        [int][Math]::Ceiling($rect.Width),
        [int][Math]::Ceiling($rect.Height)
    )
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.CopyFromScreen(
            [int][Math]::Floor($rect.X),
            [int][Math]::Floor($rect.Y),
            0,
            0,
            $bitmap.Size
        )
        $bitmap.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png)
    } finally {
        $graphics.Dispose()
        $bitmap.Dispose()
    }
}

function Assert-Tooltip(
    $Window,
    [string]$TargetName,
    [string]$ExpectedText,
    [string]$ScreenshotPath
) {
    $target = Wait-Element $Window $TargetName $false
    $rect = $target.Current.BoundingRectangle
    Assert-True ($rect.Width -gt 0 -and $rect.Height -gt 0) `
        "Tooltip target '$TargetName' has invalid bounds"
    [System.Windows.Forms.Cursor]::Position = New-Object System.Drawing.Point(
        [int][Math]::Round($rect.X + $rect.Width / 2),
        [int][Math]::Round($rect.Y + $rect.Height / 2)
    )
    Wait-Condition "tooltip for '$TargetName'" {
        $null -ne (Find-Element `
            ([System.Windows.Automation.AutomationElement]::RootElement) `
            $ExpectedText `
            $false)
    } 8
    Capture-Window $Window $ScreenshotPath
}

New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$resultPath = Join-Path $OutputDirectory $ResultFileName
$settingsPath = Join-Path $InstallDirectory "winsched-settings.exe"
$process = $null
$window = $null

try {
    Assert-True (Test-Path -LiteralPath $settingsPath -PathType Leaf) `
        "Settings executable is missing"
    Assert-True (@(Get-Process winsched-settings -ErrorAction SilentlyContinue).Count -eq 0) `
        "A Settings process is already running"
    $process = Start-Process `
        -FilePath $settingsPath `
        -WorkingDirectory $InstallDirectory `
        -PassThru
    Wait-Condition "WinSched Settings window" {
        $candidate = Find-TopLevelWindow $process.Id
        if ($null -ne $candidate) {
            $script:settingsWindowCandidate = $candidate
            return $true
        }
        return $false
    }
    $window = $script:settingsWindowCandidate

    Invoke-Element (Wait-Element $window "General" $true)
    Assert-Tooltip `
        $window `
        "Sample interval (milliseconds)" `
        "How often the service samples process and CPU activity. Lower values react faster but add more telemetry work." `
        (Join-Path $OutputDirectory "tooltip-general.png")
    Assert-Tooltip `
        $window `
        "Default workload profile" `
        "Interactive stays on one LLC, Memory spreads one thread per physical core by default, Compute uses both SMT siblings, and Background can opt an exact rule into reversible EcoQoS/memory handling. Balanced retains standard LLC-aware adaptive behavior." `
        (Join-Path $OutputDirectory "tooltip-workload-profile.png")

    Invoke-Element (Wait-Element $window "Responsiveness" $true)
    Assert-Tooltip `
        $window `
        "Enable topology-aware system reserve" `
        "Excludes complete physical-core CPU Sets from managed application plans while leaving Windows and protected system processes unrestricted." `
        (Join-Path $OutputDirectory "tooltip-responsiveness.png")

    Invoke-Element (Wait-Element $window "Background" $true)
    Assert-Tooltip `
        $window `
        "Enable background efficiency" `
        "Enables journaled process-level EcoQoS and memory-priority handling for explicitly marked background processes. Both mutations are off by default: native acceptance confirmed that a parent's memory priority propagates to children created later, and parent rollback does not restore those live children. Enable a property only for a known leaf workload." `
        (Join-Path $OutputDirectory "tooltip-background.png")

    Invoke-Element (Wait-Element $window "Logging" $true)
    Assert-Tooltip `
        $window `
        "Enable detailed service logging" `
        "Writes detailed service events as JSONL. Disable it to stop routine disk writes; existing logs remain until removed manually or by uninstall purge." `
        (Join-Path $OutputDirectory "tooltip-logging.png")

    Invoke-Element (Wait-Element $window "Close" $true)
    Wait-Condition "Settings process exit" {
        $null -eq (Get-Process -Id $process.Id -ErrorAction SilentlyContinue)
    }

    [pscustomobject]@{
        result = "PASS"
        session_id = [Diagnostics.Process]::GetCurrentProcess().SessionId
        tooltips = @(
            "Sample interval (milliseconds)",
            "Default workload profile",
            "Enable topology-aware system reserve",
            "Enable background efficiency",
            "Enable detailed service logging"
        )
        pages = @("General", "Responsiveness", "Background", "Logging")
        configuration_changed = $false
    } | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $resultPath -Encoding UTF8
} catch {
    [pscustomobject]@{
        result = "FAIL"
        error = $_.Exception.ToString()
        script_stack = $_.ScriptStackTrace
    } | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $resultPath -Encoding UTF8
    exit 1
} finally {
    if ($process -and
        $null -ne (Get-Process -Id $process.Id -ErrorAction SilentlyContinue)) {
        Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
    }
}
