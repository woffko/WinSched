[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory,
    [string]$InstallDirectory = "$env:ProgramFiles\WinSched",
    [string]$DataDirectory = "$env:ProgramData\WinSched"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms

function Expand-UnicodeEscapes([string]$Value) {
    return [System.Text.RegularExpressions.Regex]::Unescape($Value)
}

# Keep this Windows PowerShell 5.1 script ASCII; that host treats BOM-less UTF-8 as ANSI.
$script:ui = @{
    RuTitle = Expand-UnicodeEscapes "\u041d\u0430\u0441\u0442\u0440\u043e\u0439\u043a\u0438 WinSched"
    RuLanguage = Expand-UnicodeEscapes "\u0420\u0423"
    RuReload = Expand-UnicodeEscapes "\u041f\u0435\u0440\u0435\u0437\u0430\u0433\u0440\u0443\u0437\u0438\u0442\u044c \u0441 \u0434\u0438\u0441\u043a\u0430"
    RuRestore = Expand-UnicodeEscapes "\u0412\u043e\u0441\u0441\u0442\u0430\u043d\u043e\u0432\u0438\u0442\u044c \u0437\u043d\u0430\u0447\u0435\u043d\u0438\u044f \u043f\u043e \u0443\u043c\u043e\u043b\u0447\u0430\u043d\u0438\u044e..."
    RuApply = Expand-UnicodeEscapes "\u041f\u0440\u0438\u043c\u0435\u043d\u0438\u0442\u044c"
    RuClose = Expand-UnicodeEscapes "\u0417\u0430\u043a\u0440\u044b\u0442\u044c"
    RuGeneral = Expand-UnicodeEscapes "\u041e\u0441\u043d\u043e\u0432\u043d\u044b\u0435"
    RuController = Expand-UnicodeEscapes "\u0420\u0435\u0436\u0438\u043c \u043a\u043e\u043d\u0442\u0440\u043e\u043b\u043b\u0435\u0440\u0430"
    RuTrayAutostart = Expand-UnicodeEscapes "\u0410\u0432\u0442\u043e\u043c\u0430\u0442\u0438\u0447\u0435\u0441\u043a\u0438 \u0437\u0430\u043f\u0443\u0441\u043a\u0430\u0442\u044c WinSched \u0432 \u043e\u0431\u043b\u0430\u0441\u0442\u0438 \u0443\u0432\u0435\u0434\u043e\u043c\u043b\u0435\u043d\u0438\u0439 \u043f\u0440\u0438 \u0432\u0445\u043e\u0434\u0435 \u043f\u043e\u043b\u044c\u0437\u043e\u0432\u0430\u0442\u0435\u043b\u044f"
    RuAdaptive = Expand-UnicodeEscapes "\u0410\u0434\u0430\u043f\u0442\u0438\u0432\u043d\u044b\u0439 \u0440\u0435\u0436\u0438\u043c"
    RuAdaptiveHeading = Expand-UnicodeEscapes "\u041f\u043e\u043b\u0438\u0442\u0438\u043a\u0430 \u0430\u0434\u0430\u043f\u0442\u0438\u0432\u043d\u043e\u0433\u043e \u0440\u0430\u0437\u043c\u0435\u0449\u0435\u043d\u0438\u044f"
    RuResponsiveness = Expand-UnicodeEscapes "\u041e\u0442\u0437\u044b\u0432\u0447\u0438\u0432\u043e\u0441\u0442\u044c"
    RuResponsivenessHeading = Expand-UnicodeEscapes "\u0421\u0438\u0441\u0442\u0435\u043c\u043d\u044b\u0439 \u0440\u0435\u0437\u0435\u0440\u0432 \u043e\u0442\u0437\u044b\u0432\u0447\u0438\u0432\u043e\u0441\u0442\u0438"
    RuResponsivenessEnabled = Expand-UnicodeEscapes "\u0412\u043a\u043b\u044e\u0447\u0438\u0442\u044c \u0442\u043e\u043f\u043e\u043b\u043e\u0433\u0438\u0447\u0435\u0441\u043a\u0438\u0439 \u0441\u0438\u0441\u0442\u0435\u043c\u043d\u044b\u0439 \u0440\u0435\u0437\u0435\u0440\u0432"
    RuBackground = Expand-UnicodeEscapes "\u0424\u043e\u043d\u043e\u0432\u044b\u0435 \u0437\u0430\u0434\u0430\u0447\u0438"
    RuBackgroundHeading = Expand-UnicodeEscapes "\u042d\u0444\u0444\u0435\u043a\u0442\u0438\u0432\u043d\u043e\u0441\u0442\u044c \u0444\u043e\u043d\u043e\u0432\u044b\u0445 \u0437\u0430\u0434\u0430\u0447"
    RuBackgroundEnabled = Expand-UnicodeEscapes "\u0412\u043a\u043b\u044e\u0447\u0438\u0442\u044c \u044d\u0444\u0444\u0435\u043a\u0442\u0438\u0432\u043d\u043e\u0441\u0442\u044c \u0444\u043e\u043d\u043e\u0432\u044b\u0445 \u0437\u0430\u0434\u0430\u0447"
    RuEcoQos = Expand-UnicodeEscapes "\u041f\u0440\u0438\u043c\u0435\u043d\u044f\u0442\u044c EcoQoS"
    RuBackgroundMemory = Expand-UnicodeEscapes "\u041f\u043e\u043d\u0438\u0436\u0430\u0442\u044c \u043f\u0440\u0438\u043e\u0440\u0438\u0442\u0435\u0442 \u043f\u0430\u043c\u044f\u0442\u0438 \u0444\u043e\u043d\u043e\u0432\u044b\u0445 \u0437\u0430\u0434\u0430\u0447"
    RuMemoryGuard = Expand-UnicodeEscapes "\u0420\u0435\u0430\u0433\u0438\u0440\u043e\u0432\u0430\u0442\u044c \u043d\u0430 \u0443\u0432\u0435\u0434\u043e\u043c\u043b\u0435\u043d\u0438\u044f Windows \u043e \u043d\u0435\u0445\u0432\u0430\u0442\u043a\u0435 \u043f\u0430\u043c\u044f\u0442\u0438"
    RuProtectForeground = Expand-UnicodeEscapes "\u0417\u0430\u0449\u0438\u0449\u0430\u0442\u044c \u043f\u0440\u0438\u043b\u043e\u0436\u0435\u043d\u0438\u0435 foreground"
    RuProtectVisible = Expand-UnicodeEscapes "\u0417\u0430\u0449\u0438\u0449\u0430\u0442\u044c \u0432\u0438\u0434\u0438\u043c\u044b\u0435 \u0438 \u0441\u0432\u0451\u0440\u043d\u0443\u0442\u044b\u0435 \u043f\u0440\u0438\u043b\u043e\u0436\u0435\u043d\u0438\u044f"
    RuProtectAudio = Expand-UnicodeEscapes "\u0417\u0430\u0449\u0438\u0449\u0430\u0442\u044c \u043f\u0440\u0438\u043b\u043e\u0436\u0435\u043d\u0438\u044f \u0441 \u0430\u043a\u0442\u0438\u0432\u043d\u044b\u043c \u0430\u0443\u0434\u0438\u043e"
    RuRules = Expand-UnicodeEscapes "\u041f\u0440\u0430\u0432\u0438\u043b\u0430 \u043f\u0440\u043e\u0446\u0435\u0441\u0441\u043e\u0432"
    RuAddRule = Expand-UnicodeEscapes "\u0414\u043e\u0431\u0430\u0432\u0438\u0442\u044c \u043f\u0440\u0430\u0432\u0438\u043b\u043e \u043f\u0440\u043e\u0446\u0435\u0441\u0441\u0430"
    RuNoRules = Expand-UnicodeEscapes "\u042f\u0432\u043d\u044b\u0435 \u043f\u0440\u0430\u0432\u0438\u043b\u0430 \u043f\u0440\u043e\u0446\u0435\u0441\u0441\u043e\u0432 \u043d\u0435 \u043d\u0430\u0441\u0442\u0440\u043e\u0435\u043d\u044b."
    RuLogging = Expand-UnicodeEscapes "\u0416\u0443\u0440\u043d\u0430\u043b"
    RuLoggingHeading = Expand-UnicodeEscapes "\u0414\u0438\u0430\u0433\u043d\u043e\u0441\u0442\u0438\u0447\u0435\u0441\u043a\u0438\u0439 \u0436\u0443\u0440\u043d\u0430\u043b"
    RuLoggingLevel = Expand-UnicodeEscapes "\u0423\u0440\u043e\u0432\u0435\u043d\u044c \u0434\u0435\u0442\u0430\u043b\u0438\u0437\u0430\u0446\u0438\u0438 \u0436\u0443\u0440\u043d\u0430\u043b\u0430"
    RuLoggingOff = Expand-UnicodeEscapes "\u0412\u044b\u043a\u043b\u044e\u0447\u0435\u043d"
    RuLoggingNormal = Expand-UnicodeEscapes "\u041e\u0431\u044b\u0447\u043d\u044b\u0439 (\u0440\u0435\u043a\u043e\u043c\u0435\u043d\u0434\u0443\u0435\u0442\u0441\u044f)"
    RuLoggingTrace = Expand-UnicodeEscapes "\u0422\u0440\u0430\u0441\u0441\u0438\u0440\u043e\u0432\u043a\u0430"
    RuLogSize = Expand-UnicodeEscapes "\u041c\u0430\u043a\u0441\u0438\u043c\u0430\u043b\u044c\u043d\u044b\u0439 \u0440\u0430\u0437\u043c\u0435\u0440 \u0430\u043a\u0442\u0438\u0432\u043d\u043e\u0433\u043e \u0436\u0443\u0440\u043d\u0430\u043b\u0430 (\u041c\u0438\u0411)"
    RuLogArchives = Expand-UnicodeEscapes "\u0421\u043e\u0445\u0440\u0430\u043d\u044f\u0435\u043c\u044b\u0435 \u0446\u0438\u043a\u043b\u0438\u0447\u0435\u0441\u043a\u0438\u0435 \u0430\u0440\u0445\u0438\u0432\u044b"
    RuLoggingOffDescription = Expand-UnicodeEscapes "\u041e\u0431\u044b\u0447\u043d\u044b\u0439 \u0436\u0443\u0440\u043d\u0430\u043b \u0432\u044b\u043a\u043b\u044e\u0447\u0435\u043d. \u0421\u0443\u0449\u0435\u0441\u0442\u0432\u0443\u044e\u0449\u0438\u0435 \u0444\u0430\u0439\u043b\u044b \u0436\u0443\u0440\u043d\u0430\u043b\u0430 \u0438 \u0430\u0440\u0445\u0438\u0432\u044b \u0441\u043e\u0445\u0440\u0430\u043d\u044f\u044e\u0442\u0441\u044f."
}

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) {
        throw "ASSERTION FAILED: $Message"
    }
}

function Wait-Condition(
    [string]$Description,
    [scriptblock]$Condition,
    [int]$TimeoutSeconds = 30
) {
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        if (& $Condition) {
            return
        }
        Start-Sleep -Milliseconds 200
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Timed out waiting for: $Description"
}

function Write-Result([System.Collections.IDictionary]$Value) {
    [pscustomobject]$Value |
        ConvertTo-Json -Depth 8 |
        Set-Content -LiteralPath $script:resultPath -Encoding UTF8
}

function Read-ServiceStatus {
    if (-not (Test-Path -LiteralPath $script:statusPath -PathType Leaf)) {
        return $null
    }
    try {
        return Get-Content -LiteralPath $script:statusPath -Raw | ConvertFrom-Json
    } catch {
        return $null
    }
}

function Get-StatusBaseline {
    $status = Read-ServiceStatus
    if ($null -eq $status) {
        return [pscustomobject]@{
            has_status = $false
            service_pid = 0
            sequence = [uint64]0
        }
    }
    return [pscustomobject]@{
        has_status = $true
        service_pid = [int]$status.service_pid
        sequence = [uint64]$status.config_reload_sequence
    }
}

function Test-NewReloadReceipt($Status, $Baseline) {
    if ($null -eq $Status) {
        return $false
    }
    if (-not [bool]$Baseline.has_status) {
        return [uint64]$Status.config_reload_sequence -gt 0
    }
    if ([int]$Status.service_pid -ne [int]$Baseline.service_pid) {
        return [uint64]$Status.config_reload_sequence -gt 0
    }
    return [uint64]$Status.config_reload_sequence -gt [uint64]$Baseline.sequence
}

function Set-FileAtomically([string]$Path, [byte[]]$Bytes) {
    $directory = Split-Path -Parent $Path
    $temporaryPath = Join-Path $directory (
        ".{0}.settings-ui-{1}.tmp" -f (Split-Path -Leaf $Path), [Guid]::NewGuid().ToString("N")
    )
    $replacementBackup = "$temporaryPath.backup"
    try {
        [System.IO.File]::WriteAllBytes($temporaryPath, $Bytes)
        [System.IO.File]::Replace($temporaryPath, $Path, $replacementBackup, $true)
    } finally {
        foreach ($cleanupPath in @($temporaryPath, $replacementBackup)) {
            if (Test-Path -LiteralPath $cleanupPath) {
                Remove-Item -LiteralPath $cleanupPath -Force -ErrorAction SilentlyContinue
            }
        }
    }
}

function Set-Utf8FileAtomically([string]$Path, [string]$Text) {
    $encoding = New-Object System.Text.UTF8Encoding($false)
    Set-FileAtomically $Path $encoding.GetBytes($Text)
}

function Wait-ServiceReload(
    $AfterBaseline,
    [string]$ExpectedMode,
    [string]$ExpectedLoggingLevel,
    [int]$ExpectedLogSizeMiB,
    [int]$ExpectedLogArchives,
    [string]$Description,
    [int]$TimeoutSeconds = 20
) {
    Wait-Condition $Description {
        $status = Read-ServiceStatus
        $null -ne $status -and
            [int]$status.schema_version -eq 5 -and
            (Test-NewReloadReceipt $status $AfterBaseline) -and
            $status.config_reload_result -eq "reloaded" -and
            $status.configured_mode -eq $ExpectedMode -and
            [string]$status.applied_logging.level -eq $ExpectedLoggingLevel -and
            [int]$status.applied_logging.max_file_size_mib -eq $ExpectedLogSizeMiB -and
            [int]$status.applied_logging.retained_archives -eq $ExpectedLogArchives -and
            [bool]$status.applied_responsiveness.enabled -and
            @($status.system_reserve.reserved_physical_cores).Count -gt 0
    } $TimeoutSeconds
}

function Wait-RestoreReload(
    $AfterBaseline,
    [string]$ExpectedMode,
    [string]$ExpectedLoggingLevel,
    [int]$ExpectedLogSizeMiB,
    [int]$ExpectedLogArchives,
    [int]$TimeoutSeconds = 25
) {
    Wait-Condition "service reload after restoring original configuration" {
        $status = Read-ServiceStatus
        $null -ne $status -and
            [int]$status.schema_version -eq 5 -and
            (Test-NewReloadReceipt $status $AfterBaseline) -and
            $status.config_reload_result -eq "reloaded" -and
            $status.configured_mode -eq $ExpectedMode -and
            [string]$status.applied_logging.level -eq $ExpectedLoggingLevel -and
            [int]$status.applied_logging.max_file_size_mib -eq $ExpectedLogSizeMiB -and
            [int]$status.applied_logging.retained_archives -eq $ExpectedLogArchives
    } $TimeoutSeconds
}

function Get-CurrentSessionSettingsProcesses {
    return @(
        Get-Process -Name "winsched-settings" -ErrorAction SilentlyContinue |
            Where-Object {
                try {
                    $_.SessionId -eq $script:currentSessionId -and
                        [string]::Equals(
                            $_.Path,
                            $script:settingsPath,
                            [StringComparison]::OrdinalIgnoreCase
                        )
                } catch {
                    $false
                }
            }
    )
}

function Get-SettingsWindow([datetime]$LaunchedAfter) {
    $matches = @()
    $minimumStartTime = if ($LaunchedAfter -le [DateTime]::MinValue.AddSeconds(5)) {
        [DateTime]::MinValue
    } else {
        $LaunchedAfter.AddSeconds(-2)
    }
    $windows = [System.Windows.Automation.AutomationElement]::RootElement.FindAll(
        [System.Windows.Automation.TreeScope]::Children,
        [System.Windows.Automation.Condition]::TrueCondition
    )
    foreach ($window in $windows) {
        try {
            if ($window.Current.ControlType -ne [System.Windows.Automation.ControlType]::Window -or
                $window.Current.Name -notin @("WinSched Settings", $script:ui.RuTitle)) {
                continue
            }
            $owner = Get-Process -Id $window.Current.ProcessId -ErrorAction SilentlyContinue
            if ($null -eq $owner -or
                $owner.SessionId -ne $script:currentSessionId -or
                $owner.StartTime -lt $minimumStartTime -or
                -not [string]::Equals(
                    $owner.Path,
                    $script:settingsPath,
                    [StringComparison]::OrdinalIgnoreCase
                )) {
                continue
            }
            $matches += $window
        } catch {
            continue
        }
    }
    if ($matches.Count -eq 1) {
        return $matches[0]
    }
    if ($matches.Count -gt 1) {
        throw "More than one fresh WinSched Settings top-level window was found"
    }
    return $null
}

function Get-AllAccessibleElements($Root) {
    $elements = New-Object System.Collections.ArrayList
    [void]$elements.Add($Root)
    foreach ($element in $Root.FindAll(
        [System.Windows.Automation.TreeScope]::Descendants,
        [System.Windows.Automation.Condition]::TrueCondition
    )) {
        [void]$elements.Add($element)
    }
    return $elements
}

function Get-NamedAccessibleElements($Root, [string[]]$Names) {
    $found = @()
    foreach ($element in @(Get-AllAccessibleElements $Root)) {
        try {
            if ($Names -contains $element.Current.Name) {
                $found += $element
            }
        } catch {
            continue
        }
    }
    return $found
}

function Test-ElementActionable($Element) {
    foreach ($patternId in @(
        [System.Windows.Automation.InvokePattern]::Pattern,
        [System.Windows.Automation.SelectionItemPattern]::Pattern,
        [System.Windows.Automation.TogglePattern]::Pattern
    )) {
        $pattern = $null
        try {
            if ($Element.TryGetCurrentPattern($patternId, [ref]$pattern)) {
                return $true
            }
        } catch {
        }
    }
    $legacy = $null
    try {
        if ($Element.TryGetCurrentPattern(
            [System.Windows.Automation.LegacyIAccessiblePattern]::Pattern,
            [ref]$legacy
        ) -and -not [string]::IsNullOrWhiteSpace($legacy.Current.DefaultAction)) {
            return $true
        }
    } catch {
    }
    return $false
}

function Find-AccessibleElement(
    $Root,
    [string[]]$Names,
    [bool]$Actionable = $false
) {
    foreach ($element in @(Get-NamedAccessibleElements $Root $Names)) {
        try {
            if (-not $element.Current.IsOffscreen -and
                (-not $Actionable -or (Test-ElementActionable $element))) {
                return $element
            }
        } catch {
            continue
        }
    }
    return $null
}

function Wait-AccessibleElement(
    $Root,
    [string[]]$Names,
    [bool]$Actionable = $false,
    [int]$TimeoutSeconds = 15
) {
    $script:foundAccessibleElement = $null
    Wait-Condition ("accessible element '{0}'" -f ($Names -join "' or '")) {
        $candidate = Find-AccessibleElement $Root $Names $Actionable
        if ($null -ne $candidate) {
            $script:foundAccessibleElement = $candidate
            return $true
        }
        return $false
    } $TimeoutSeconds
    return $script:foundAccessibleElement
}

function Invoke-AccessibleElement($Element) {
    Assert-True ($null -ne $Element) "Accessible action element is missing"
    Assert-True $Element.Current.IsEnabled "Accessible element '$($Element.Current.Name)' is disabled"

    $pattern = $null
    if ($Element.TryGetCurrentPattern(
        [System.Windows.Automation.InvokePattern]::Pattern,
        [ref]$pattern
    )) {
        $pattern.Invoke()
        return
    }

    $pattern = $null
    if ($Element.TryGetCurrentPattern(
        [System.Windows.Automation.SelectionItemPattern]::Pattern,
        [ref]$pattern
    )) {
        $pattern.Select()
        return
    }

    $pattern = $null
    if ($Element.TryGetCurrentPattern(
        [System.Windows.Automation.TogglePattern]::Pattern,
        [ref]$pattern
    )) {
        $pattern.Toggle()
        return
    }

    $pattern = $null
    if ($Element.TryGetCurrentPattern(
        [System.Windows.Automation.LegacyIAccessiblePattern]::Pattern,
        [ref]$pattern
    ) -and -not [string]::IsNullOrWhiteSpace($pattern.Current.DefaultAction)) {
        $pattern.DoDefaultAction()
        return
    }

    throw "Accessible element '$($Element.Current.Name)' exposes no supported action pattern"
}

function Get-ToggleState($Element) {
    $pattern = $null
    if ($Element.TryGetCurrentPattern(
        [System.Windows.Automation.TogglePattern]::Pattern,
        [ref]$pattern
    )) {
        return $pattern.Current.ToggleState.ToString()
    }
    throw "Accessible element '$($Element.Current.Name)' exposes no TogglePattern"
}

function Set-NamedToggleState(
    $Root,
    [string[]]$Names,
    [ValidateSet("On", "Off")]
    [string]$ExpectedState
) {
    $element = Wait-AccessibleElement $Root $Names $true
    if ((Get-ToggleState $element) -ne $ExpectedState) {
        Invoke-AccessibleElement $element
    }
    Wait-Condition ("toggle '{0}' state $ExpectedState" -f ($Names -join "' or '")) {
        $current = Find-AccessibleElement $Root $Names $true
        $null -ne $current -and (Get-ToggleState $current) -eq $ExpectedState
    }
}

function Test-SelectionState($Element) {
    $pattern = $null
    if ($Element.TryGetCurrentPattern(
        [System.Windows.Automation.SelectionItemPattern]::Pattern,
        [ref]$pattern
    )) {
        return [bool]$pattern.Current.IsSelected
    }
    if ($Element.TryGetCurrentPattern(
        [System.Windows.Automation.TogglePattern]::Pattern,
        [ref]$pattern
    )) {
        return $pattern.Current.ToggleState.ToString() -eq "On"
    }
    throw "Accessible element '$($Element.Current.Name)' exposes no selection state"
}

function Set-NamedSelection($Root, [string[]]$Names) {
    $element = Wait-AccessibleElement $Root $Names $true
    if (-not (Test-SelectionState $element)) {
        Invoke-AccessibleElement $element
    }
    Wait-Condition ("selection '{0}'" -f ($Names -join "' or '")) {
        $current = Find-AccessibleElement $Root $Names $true
        $null -ne $current -and (Test-SelectionState $current)
    }
}

function Find-NamedNumericControl($Root, [string[]]$Names) {
    foreach ($element in @(Get-NamedAccessibleElements $Root $Names)) {
        $pattern = $null
        try {
            if ($element.TryGetCurrentPattern(
                [System.Windows.Automation.RangeValuePattern]::Pattern,
                [ref]$pattern
            )) {
                return $element
            }
            $pattern = $null
            if ($element.TryGetCurrentPattern(
                [System.Windows.Automation.ValuePattern]::Pattern,
                [ref]$pattern
            )) {
                return $element
            }
        } catch {
            continue
        }
    }
    return $null
}

function Get-NumericControlValue($Element) {
    $pattern = $null
    if ($Element.TryGetCurrentPattern(
        [System.Windows.Automation.RangeValuePattern]::Pattern,
        [ref]$pattern
    )) {
        return [double]$pattern.Current.Value
    }
    $pattern = $null
    if ($Element.TryGetCurrentPattern(
        [System.Windows.Automation.ValuePattern]::Pattern,
        [ref]$pattern
    )) {
        $match = [regex]::Match($pattern.Current.Value, '-?\d+(?:[\.,]\d+)?')
        if ($match.Success) {
            return [double]::Parse(
                $match.Value.Replace(',', '.'),
                [Globalization.CultureInfo]::InvariantCulture
            )
        }
    }
    throw "Accessible element '$($Element.Current.Name)' exposes no numeric value"
}

function Assert-NumericControl(
    $Root,
    [string[]]$Names,
    [bool]$ExpectedEnabled,
    [double]$ExpectedValue
) {
    $script:numericControlCandidate = $null
    Wait-Condition ("numeric control '{0}'" -f ($Names -join "' or '")) {
        $candidate = Find-NamedNumericControl $Root $Names
        if ($null -ne $candidate) {
            $script:numericControlCandidate = $candidate
            return $true
        }
        return $false
    }
    $control = $script:numericControlCandidate
    Assert-True ([bool]$control.Current.IsEnabled -eq $ExpectedEnabled) `
        "Numeric control '$($control.Current.Name)' enabled state is not $ExpectedEnabled"
    $actual = Get-NumericControlValue $control
    Assert-True ([Math]::Abs($actual - $ExpectedValue) -lt 0.001) `
        "Numeric control '$($control.Current.Name)' value is $actual, expected $ExpectedValue"
}

function Invoke-NamedAccessibleElement($Root, [string[]]$Names) {
    $element = Wait-AccessibleElement $Root $Names $true
    Invoke-AccessibleElement $element
}

function Test-AccessibleName($Root, [string[]]$Names) {
    return $null -ne (Find-AccessibleElement $Root $Names $false)
}

function Assert-HoverTooltip(
    $Window,
    [string[]]$TargetNames,
    [string]$ExpectedText
) {
    $target = Wait-AccessibleElement $Window $TargetNames $false
    $rect = $target.Current.BoundingRectangle
    Assert-True ($rect.Width -gt 0 -and $rect.Height -gt 0) `
        "Tooltip target '$($TargetNames -join "' or '")' has invalid bounds"
    [System.Windows.Forms.Cursor]::Position = New-Object System.Drawing.Point(
        [int][Math]::Round($rect.X + $rect.Width / 2),
        [int][Math]::Round($rect.Y + $rect.Height / 2)
    )
    Wait-Condition "tooltip '$ExpectedText'" {
        Test-AccessibleName `
            ([System.Windows.Automation.AutomationElement]::RootElement) `
            @($ExpectedText)
    } 8
}

function Capture-Window($Window, [string]$Path) {
    try {
        $Window.SetFocus()
        Start-Sleep -Milliseconds 250
    } catch {
    }

    $rect = $Window.Current.BoundingRectangle
    $x = [int][Math]::Floor($rect.X)
    $y = [int][Math]::Floor($rect.Y)
    $width = [int][Math]::Ceiling($rect.Width)
    $height = [int][Math]::Ceiling($rect.Height)
    Assert-True ($width -gt 400 -and $height -gt 300) "Settings window bounds are invalid"

    $bitmap = New-Object System.Drawing.Bitmap($width, $height)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.CopyFromScreen($x, $y, 0, 0, $bitmap.Size)
        $bitmap.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png)
    } finally {
        $graphics.Dispose()
        $bitmap.Dispose()
    }
}

function Get-ConfigScalar([string]$Text, [string]$Name) {
    $match = [regex]::Match(
        $Text,
        "(?m)^\s*" + [regex]::Escape($Name) + "\s*=\s*([^#\r\n]+?)\s*$"
    )
    if (-not $match.Success) {
        throw "Configuration field '$Name' was not found"
    }
    return $match.Groups[1].Value.Trim().Trim('"')
}

function Get-ConfigLogging([string]$Text) {
    $section = [regex]::Match(
        $Text,
        "(?ms)^\s*\[logging\]\s*(?<body>.*?)(?=^\s*\[|\z)"
    )
    if (-not $section.Success) {
        return [pscustomobject]@{
            level = "normal"
            max_file_size_mib = 10
            retained_archives = 1
        }
    }
    $body = $section.Groups["body"].Value
    $level = [regex]::Match($body, '(?m)^\s*level\s*=\s*"(?<value>[^\"]+)"\s*$')
    if ($level.Success) {
        $loggingLevel = $level.Groups["value"].Value
    } else {
        $enabled = [regex]::Match($body, '(?m)^\s*enabled\s*=\s*(?<value>true|false)\s*$')
        $loggingLevel = if ($enabled.Success -and $enabled.Groups["value"].Value -eq "false") {
            "off"
        } else {
            "normal"
        }
    }
    return [pscustomobject]@{
        level = $loggingLevel
        max_file_size_mib = [int](Get-ConfigScalar $Text "max_file_size_mib")
        retained_archives = [int](Get-ConfigScalar $Text "retained_archives")
    }
}

function Get-ConfigSectionScalar([string]$Text, [string]$Section, [string]$Name) {
    $sectionMatch = [regex]::Match(
        $Text,
        "(?ms)^\s*\[" + [regex]::Escape($Section) + "\]\s*(.*?)(?=^\s*\[|\z)"
    )
    if (-not $sectionMatch.Success) {
        throw "Configuration section '$Section' was not found"
    }
    return Get-ConfigScalar $sectionMatch.Groups[1].Value $Name
}

function Assert-ProductDefaults([string]$Text) {
    $expected = [ordered]@{
        schema_version = "5"
        controller_mode = "auto"
        sample_interval_ms = "1000"
        minimum_process_utilization_bps = "500"
        all_user_processes = "true"
        default_rule_mode = "auto"
        default_workload_profile = "balanced"
        overload_threshold_bps = "8500"
        minimum_improvement_bps = "2000"
        stability_samples = "3"
        minimum_residency_ms = "10000"
        cooldown_ms = "30000"
        max_mutations_per_evaluation = "1"
    }
    $loggingExpected = [ordered]@{
        level = "normal"
        max_file_size_mib = "10"
        retained_archives = "1"
    }
    foreach ($field in $loggingExpected.Keys) {
        $actual = Get-ConfigSectionScalar $Text "logging" $field
        Assert-True ($actual -eq $loggingExpected[$field]) (
            "Product logging default '$field' is '$actual', expected '$($loggingExpected[$field])'"
        )
    }
    $responsivenessExpected = [ordered]@{
        enabled = "true"
        system_reserve_percent = "10"
        minimum_reserved_cores = "2"
        maximum_reserved_cores = "8"
        latency_guard_enabled = "true"
        latency_target_p99_us = "2000"
        latency_recovery_p99_us = "1000"
        adjustment_stability_samples = "5"
    }
    foreach ($field in $responsivenessExpected.Keys) {
        $actual = Get-ConfigSectionScalar $Text "responsiveness" $field
        Assert-True ($actual -eq $responsivenessExpected[$field]) (
            "Product responsiveness default '$field' is '$actual', expected '$($responsivenessExpected[$field])'"
        )
    }
    $backgroundExpected = [ordered]@{
        enabled = "false"
        eco_qos_enabled = "false"
        memory_priority_enabled = "false"
        memory_pressure_guard_enabled = "true"
        protect_foreground = "true"
        protect_visible = "true"
        protect_audio = "true"
    }
    foreach ($field in $backgroundExpected.Keys) {
        $actual = Get-ConfigSectionScalar $Text "background_efficiency" $field
        Assert-True ($actual -eq $backgroundExpected[$field]) (
            "Product background-efficiency default '$field' is '$actual', expected '$($backgroundExpected[$field])'"
        )
    }
    $memoryExpected = [ordered]@{
        use_smt = "false"
        minimum_physical_cores = "8"
        maximum_physical_cores = "28"
        resize_cooldown_ms = "300000"
    }
    foreach ($field in $memoryExpected.Keys) {
        $actual = Get-ConfigSectionScalar $Text "responsiveness.memory" $field
        Assert-True ($actual -eq $memoryExpected[$field]) (
            "Product memory-profile default '$field' is '$actual', expected '$($memoryExpected[$field])'"
        )
    }
    foreach ($field in $expected.Keys) {
        $actual = Get-ConfigScalar $Text $field
        Assert-True ($actual -eq $expected[$field]) (
            "Product default '$field' is '$actual', expected '$($expected[$field])'"
        )
    }
    Assert-True (-not [regex]::IsMatch($Text, "(?m)^\s*\[\[rules\]\]\s*$")) `
        "Restore defaults did not remove process rules"
    Assert-True (-not $Text.Contains($script:marker)) `
        "Restore defaults left the external acceptance marker in the saved configuration"
}

function Dismiss-SingleInstanceNotice([datetime]$LaunchedAfter) {
    $deadline = [DateTime]::UtcNow.AddSeconds(20)
    do {
        $windows = [System.Windows.Automation.AutomationElement]::RootElement.FindAll(
            [System.Windows.Automation.TreeScope]::Children,
            [System.Windows.Automation.Condition]::TrueCondition
        )
        foreach ($window in $windows) {
            try {
                $owner = Get-Process -Id $window.Current.ProcessId -ErrorAction SilentlyContinue
                if ($null -eq $owner -or
                    $owner.SessionId -ne $script:currentSessionId -or
                    $owner.StartTime -lt $LaunchedAfter.AddSeconds(-2)) {
                    continue
                }
                $message = Find-AccessibleElement $window @(
                    "*another WinSched Settings window is already open*"
                ) $false
                if ($null -eq $message) {
                    foreach ($element in @(Get-AllAccessibleElements $window)) {
                        if ($element.Current.Name -like "*another WinSched Settings window is already open*") {
                            $message = $element
                            break
                        }
                    }
                }
                if ($null -eq $message) {
                    continue
                }
                $ok = Find-AccessibleElement $window @("OK") $true
                if ($null -ne $ok) {
                    Invoke-AccessibleElement $ok
                }
                return $true
            } catch {
                continue
            }
        }
        Start-Sleep -Milliseconds 200
    } while ([DateTime]::UtcNow -lt $deadline)
    return $false
}

function Stop-TrackedSettingsProcess([int]$ProcessId) {
    if ($ProcessId -le 0 -or $ProcessId -eq $PID) {
        return
    }
    try {
        $process = Get-Process -Id $ProcessId -ErrorAction Stop
        if ($process.Id -eq $PID -or
            $process.SessionId -ne $script:currentSessionId -or
            -not [string]::Equals(
                $process.Path,
                $script:settingsPath,
                [StringComparison]::OrdinalIgnoreCase
            )) {
            return
        }
        Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
    } catch {
    }
}

New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$script:resultPath = Join-Path $OutputDirectory "settings-ui-result.json"
$script:settingsPath = Join-Path $InstallDirectory "winsched-settings.exe"
$script:configPath = Join-Path $DataDirectory "winsched.toml"
$script:statusPath = Join-Path $DataDirectory "status.json"
$script:currentSessionId = (Get-Process -Id $PID).SessionId
$script:marker = "settings-ui-acceptance-{0}" -f [Guid]::NewGuid().ToString("N")
$backupPath = Join-Path $env:TEMP ("winsched-settings-ui-{0}.bak" -f [Guid]::NewGuid().ToString("N"))
$screenshots = New-Object System.Collections.ArrayList
$originalBytes = $null
$originalHash = $null
$originalMode = $null
$originalLoggingLevel = $null
$originalLogSizeMiB = 0
$originalLogArchives = 0
$workingConfigInstalled = $false
$configurationRestored = $true
$primaryProcessId = 0
$launcherProcessId = 0
$secondProcessId = 0
$secondLaunchedAfter = [DateTime]::MaxValue
$primaryWindow = $null
$mainResult = "FAIL"
$mainError = $null
$mainStack = $null
$cleanupErrors = New-Object System.Collections.ArrayList
$result = [ordered]@{
    result = "FAIL"
    cleanup_completed = $false
    session_id = $script:currentSessionId
    phase = "initializing"
}
Remove-Item -LiteralPath $script:resultPath -Force -ErrorAction SilentlyContinue

try {
    Assert-True (Test-Path -LiteralPath $script:settingsPath -PathType Leaf) `
        "Installed settings executable is missing"
    Assert-True (Test-Path -LiteralPath $script:configPath -PathType Leaf) `
        "WinSched configuration is missing"
    Assert-True (Test-Path -LiteralPath $script:statusPath -PathType Leaf) `
        "WinSched status file is missing"
    $service = Get-Service -Name "WinSched" -ErrorAction SilentlyContinue
    Assert-True ($null -ne $service -and $service.Status -eq "Running") `
        "WinSched service must be running"
    Assert-True (@(Get-CurrentSessionSettingsProcesses).Count -eq 0) `
        "A WinSched Settings process is already running in the interactive session"

    $originalBytes = [System.IO.File]::ReadAllBytes($script:configPath)
    [System.IO.File]::WriteAllBytes($backupPath, $originalBytes)
    $originalHash = (Get-FileHash -LiteralPath $backupPath -Algorithm SHA256).Hash.ToLowerInvariant()
    $originalStatus = Read-ServiceStatus
    Assert-True ($null -ne $originalStatus -and [int]$originalStatus.schema_version -eq 5) `
        "WinSched status schema 5 is required before Settings acceptance"
    $originalMode = [string]$originalStatus.configured_mode
    $originalLoggingLevel = [string]$originalStatus.applied_logging.level
    $originalLogSizeMiB = [int]$originalStatus.applied_logging.max_file_size_mib
    $originalLogArchives = [int]$originalStatus.applied_logging.retained_archives

    $workingConfig = @"
# External backup marker: $($script:marker)
schema_version = 5
controller_mode = "observe"
sample_interval_ms = 2500
minimum_process_utilization_bps = 500
all_user_processes = false
default_rule_mode = "auto"
default_workload_profile = "balanced"

[logging]
level = "off"
max_file_size_mib = 2
retained_archives = 2

[responsiveness]
enabled = true
system_reserve_percent = 10
minimum_reserved_cores = 2
maximum_reserved_cores = 8
latency_guard_enabled = true
latency_target_p99_us = 2000
latency_recovery_p99_us = 1000
adjustment_stability_samples = 5

[responsiveness.memory]
use_smt = false
minimum_physical_cores = 8
maximum_physical_cores = 28
resize_cooldown_ms = 300000

[policy]
overload_threshold_bps = 8500
minimum_improvement_bps = 2000
stability_samples = 3
minimum_residency_ms = 10000
cooldown_ms = 30000
max_mutations_per_evaluation = 1
"@
    $workingBaseline = Get-StatusBaseline
    Set-Utf8FileAtomically $script:configPath $workingConfig
    $workingConfigInstalled = $true
    $configurationRestored = $false
    Wait-ServiceReload `
        $workingBaseline `
        "observe" `
        "off" `
        2 `
        2 `
        "service reload of non-default acceptance configuration"
    $workingOnDisk = Get-Content -LiteralPath $script:configPath -Raw
    Assert-True ($workingOnDisk.Contains($script:marker)) `
        "Acceptance configuration does not contain its external backup marker"
    Assert-True ((Get-ConfigScalar $workingOnDisk "sample_interval_ms") -eq "2500") `
        "Acceptance configuration does not differ from ControllerConfig defaults"

    $launchedAfter = Get-Date
    $launcher = Start-Process `
        -FilePath $script:settingsPath `
        -WorkingDirectory $InstallDirectory `
        -PassThru
    $launcherProcessId = $launcher.Id
    Wait-Condition "fresh WinSched Settings top-level window" {
        $candidate = Get-SettingsWindow $launchedAfter
        if ($null -ne $candidate) {
            $script:primaryWindowCandidate = $candidate
            return $true
        }
        return $false
    } 30
    $primaryWindow = $script:primaryWindowCandidate
    $primaryProcessId = [int]$primaryWindow.Current.ProcessId
    Assert-True ($primaryProcessId -ne $PID) "Settings process unexpectedly equals the test PID"
    Wait-Condition "exactly one settings process" {
        $processes = @(Get-CurrentSessionSettingsProcesses)
        $processes.Count -eq 1 -and $processes[0].Id -eq $primaryProcessId
    }

    [void](Wait-AccessibleElement $primaryWindow @("EN") $true 20)
    [void](Wait-AccessibleElement $primaryWindow @($script:ui.RuLanguage) $true 20)
    [void](Wait-AccessibleElement $primaryWindow @(
        "Reload from disk", $script:ui.RuReload
    ) $true 20)
    [void](Wait-AccessibleElement $primaryWindow @(
        "Restore defaults...", $script:ui.RuRestore
    ) $true 20)
    [void](Wait-AccessibleElement $primaryWindow @("Apply", $script:ui.RuApply) $true 20)
    [void](Wait-AccessibleElement $primaryWindow @("Close", $script:ui.RuClose) $true 20)

    $secondLaunchedAfter = Get-Date
    $second = Start-Process `
        -FilePath $script:settingsPath `
        -WorkingDirectory $InstallDirectory `
        -PassThru
    $secondProcessId = $second.Id
    Assert-True ($secondProcessId -ne $PID) "Second settings process unexpectedly equals the test PID"
    $singleInstanceNoticeDismissed = Dismiss-SingleInstanceNotice $secondLaunchedAfter
    Assert-True $singleInstanceNoticeDismissed "Single-instance notice did not appear"
    Assert-True $second.WaitForExit(20000) "Second settings launch did not exit through the instance guard"
    Wait-Condition "single settings instance after a second launch" {
        $processes = @(Get-CurrentSessionSettingsProcesses)
        $processes.Count -eq 1 -and $processes[0].Id -eq $primaryProcessId
    }

    Invoke-NamedAccessibleElement $primaryWindow @("EN")
    [void](Wait-AccessibleElement $primaryWindow @("General") $true)
    Invoke-NamedAccessibleElement $primaryWindow @("General")
    [void](Wait-AccessibleElement $primaryWindow @("Controller behavior") $false)
    Assert-HoverTooltip `
        $primaryWindow `
        @("Sample interval (milliseconds)") `
        "How often the service samples process and CPU activity. Lower values react faster but add more telemetry work."
    Assert-HoverTooltip `
        $primaryWindow `
        @("Default workload profile") `
        "Interactive stays on one LLC, Memory spreads one thread per physical core by default, Compute uses both SMT siblings, and Background can opt an exact rule into reversible EcoQoS/memory handling. Balanced retains standard LLC-aware adaptive behavior."
    [void](Wait-AccessibleElement $primaryWindow @(
        "Start the WinSched tray automatically when a user signs in"
    ) $true)
    $path = Join-Path $OutputDirectory "settings-general-en.png"
    Capture-Window $primaryWindow $path
    [void]$screenshots.Add((Split-Path -Leaf $path))

    Invoke-NamedAccessibleElement $primaryWindow @("Adaptive")
    [void](Wait-AccessibleElement $primaryWindow @("Adaptive placement policy") $false)
    $path = Join-Path $OutputDirectory "settings-adaptive-en.png"
    Capture-Window $primaryWindow $path
    [void]$screenshots.Add((Split-Path -Leaf $path))

    Invoke-NamedAccessibleElement $primaryWindow @("Responsiveness")
    [void](Wait-AccessibleElement $primaryWindow @("System responsiveness reserve") $false)
    $responsivenessToggle = Wait-AccessibleElement `
        $primaryWindow `
        @("Enable topology-aware system reserve") `
        $true
    Assert-True ((Get-ToggleState $responsivenessToggle) -eq "On") `
        "Working configuration did not enable the system reserve"
    Assert-HoverTooltip `
        $primaryWindow `
        @("Enable topology-aware system reserve") `
        "Excludes complete physical-core CPU Sets from managed application plans while leaving Windows and protected system processes unrestricted."
    Assert-NumericControl $primaryWindow @("System reserve percent") $true 10
    Assert-NumericControl $primaryWindow @("Minimum reserved cores") $true 2
    Assert-NumericControl $primaryWindow @("Maximum reserved cores") $true 8
    Assert-NumericControl $primaryWindow @("Latency target p99 (microseconds)") $true 2000
    Assert-NumericControl $primaryWindow @("Latency recovery p99 (microseconds)") $true 1000
    Assert-NumericControl $primaryWindow @("Adjustment stability samples") $true 5
    $smtToggle = Wait-AccessibleElement `
        $primaryWindow `
        @("Allow both SMT threads per physical core") `
        $true
    Assert-True ((Get-ToggleState $smtToggle) -eq "Off") `
        "Memory profile unexpectedly enabled both SMT siblings"
    Assert-NumericControl $primaryWindow @("Minimum memory-profile cores") $true 8
    Assert-NumericControl $primaryWindow @("Maximum memory-profile cores") $true 28
    Assert-NumericControl `
        $primaryWindow `
        @("Memory resize cooldown (milliseconds)") `
        $true `
        300000
    $path = Join-Path $OutputDirectory "settings-responsiveness-en.png"
    Capture-Window $primaryWindow $path
    [void]$screenshots.Add((Split-Path -Leaf $path))

    Invoke-NamedAccessibleElement $primaryWindow @("Background")
    [void](Wait-AccessibleElement $primaryWindow @("Background efficiency") $false)
    $backgroundToggle = Wait-AccessibleElement `
        $primaryWindow `
        @("Enable background efficiency") `
        $true
    Assert-True ((Get-ToggleState $backgroundToggle) -eq "Off") `
        "Schema-5 working configuration unexpectedly enabled background efficiency"
    Assert-HoverTooltip `
        $primaryWindow `
        @("Enable background efficiency") `
        "Enables journaled process-level EcoQoS and memory-priority handling for explicitly marked background processes. Both mutations are off by default: native acceptance confirmed that a parent's memory priority propagates to children created later, and parent rollback does not restore those live children. Enable a property only for a known leaf workload."
    Set-NamedToggleState $primaryWindow @("Enable background efficiency") "On"
    Set-NamedToggleState $primaryWindow @("Apply EcoQoS") "Off"
    Set-NamedToggleState $primaryWindow @("Lower background memory priority") "On"
    Set-NamedToggleState `
        $primaryWindow `
        @("React to Windows low-memory notifications") `
        "On"
    Set-NamedToggleState $primaryWindow @("Protect the foreground application") "On"
    Set-NamedToggleState `
        $primaryWindow `
        @("Protect visible and minimized applications") `
        "On"
    Set-NamedToggleState `
        $primaryWindow `
        @("Protect applications with active audio") `
        "On"
    $path = Join-Path $OutputDirectory "settings-background-en.png"
    Capture-Window $primaryWindow $path
    [void]$screenshots.Add((Split-Path -Leaf $path))

    Invoke-NamedAccessibleElement $primaryWindow @("Process rules")
    [void](Wait-AccessibleElement $primaryWindow @("Add process rule") $true)
    [void](Wait-AccessibleElement $primaryWindow @(
        "No explicit process rules are configured."
    ) $false)
    $path = Join-Path $OutputDirectory "settings-process-rules-en.png"
    Capture-Window $primaryWindow $path
    [void]$screenshots.Add((Split-Path -Leaf $path))

    Invoke-NamedAccessibleElement $primaryWindow @("Logging")
    [void](Wait-AccessibleElement $primaryWindow @("Diagnostic logging") $false)
    [void](Wait-AccessibleElement $primaryWindow @("Log detail level") $false)
    $loggingOff = Wait-AccessibleElement $primaryWindow @("Off") $true
    Assert-True (Test-SelectionState $loggingOff) `
        "Working configuration did not load logging level Off"
    Assert-HoverTooltip `
        $primaryWindow `
        @("Log detail level") `
        "Off performs no routine log writes. Normal records changes, failures, and one aggregated decision summary per minute. Trace additionally writes every per-process policy decision and can generate substantial disk I/O."
    Assert-NumericControl `
        $primaryWindow `
        @("Maximum active log size (MiB)") `
        $false `
        2
    Assert-NumericControl `
        $primaryWindow `
        @("Retained circular archives") `
        $false `
        2
    [void](Wait-AccessibleElement $primaryWindow @(
        "Routine logging is off. Existing log and archive files are preserved."
    ) $false)
    $path = Join-Path $OutputDirectory "settings-logging-disabled-en.png"
    Capture-Window $primaryWindow $path
    [void]$screenshots.Add((Split-Path -Leaf $path))

    Invoke-NamedAccessibleElement $primaryWindow @($script:ui.RuLanguage)
    [void](Wait-AccessibleElement $primaryWindow @($script:ui.RuResponsiveness) $true)
    Invoke-NamedAccessibleElement $primaryWindow @($script:ui.RuResponsiveness)
    [void](Wait-AccessibleElement $primaryWindow @($script:ui.RuResponsivenessHeading) $false)
    $responsivenessToggle = Wait-AccessibleElement `
        $primaryWindow `
        @($script:ui.RuResponsivenessEnabled) `
        $true
    Assert-True ((Get-ToggleState $responsivenessToggle) -eq "On") `
        "Russian Responsiveness tab did not preserve the enabled reserve"
    $path = Join-Path $OutputDirectory "settings-responsiveness-ru.png"
    Capture-Window $primaryWindow $path
    [void]$screenshots.Add((Split-Path -Leaf $path))

    [void](Wait-AccessibleElement $primaryWindow @($script:ui.RuBackground) $true)
    Invoke-NamedAccessibleElement $primaryWindow @($script:ui.RuBackground)
    [void](Wait-AccessibleElement $primaryWindow @($script:ui.RuBackgroundHeading) $false)
    foreach ($toggle in @(
        [pscustomobject]@{ Name = $script:ui.RuBackgroundEnabled; State = "On" },
        [pscustomobject]@{ Name = $script:ui.RuEcoQos; State = "Off" },
        [pscustomobject]@{ Name = $script:ui.RuBackgroundMemory; State = "On" },
        [pscustomobject]@{ Name = $script:ui.RuMemoryGuard; State = "On" },
        [pscustomobject]@{ Name = $script:ui.RuProtectForeground; State = "On" },
        [pscustomobject]@{ Name = $script:ui.RuProtectVisible; State = "On" },
        [pscustomobject]@{ Name = $script:ui.RuProtectAudio; State = "On" }
    )) {
        $control = Wait-AccessibleElement $primaryWindow @($toggle.Name) $true
        Assert-True ((Get-ToggleState $control) -eq $toggle.State) `
            "Russian Background control '$($toggle.Name)' did not preserve state $($toggle.State)"
    }
    $path = Join-Path $OutputDirectory "settings-background-ru.png"
    Capture-Window $primaryWindow $path
    [void]$screenshots.Add((Split-Path -Leaf $path))

    [void](Wait-AccessibleElement $primaryWindow @($script:ui.RuLogging) $true)
    Invoke-NamedAccessibleElement $primaryWindow @($script:ui.RuLogging)
    [void](Wait-AccessibleElement $primaryWindow @($script:ui.RuLoggingHeading) $false)
    [void](Wait-AccessibleElement $primaryWindow @($script:ui.RuLoggingLevel) $false)
    $loggingOff = Wait-AccessibleElement $primaryWindow @($script:ui.RuLoggingOff) $true
    Assert-True (Test-SelectionState $loggingOff) `
        "Russian Logging tab did not preserve logging level Off"
    Assert-NumericControl $primaryWindow @($script:ui.RuLogSize) $false 2
    Assert-NumericControl $primaryWindow @($script:ui.RuLogArchives) $false 2
    [void](Wait-AccessibleElement $primaryWindow @($script:ui.RuLoggingOffDescription) $false)
    $path = Join-Path $OutputDirectory "settings-logging-disabled-ru.png"
    Capture-Window $primaryWindow $path
    [void]$screenshots.Add((Split-Path -Leaf $path))

    [void](Wait-AccessibleElement $primaryWindow @($script:ui.RuGeneral) $true)
    Invoke-NamedAccessibleElement $primaryWindow @($script:ui.RuGeneral)
    [void](Wait-AccessibleElement $primaryWindow @($script:ui.RuController) $false)
    [void](Wait-AccessibleElement $primaryWindow @($script:ui.RuTrayAutostart) $true)
    $path = Join-Path $OutputDirectory "settings-general-ru.png"
    Capture-Window $primaryWindow $path
    [void]$screenshots.Add((Split-Path -Leaf $path))

    Invoke-NamedAccessibleElement $primaryWindow @($script:ui.RuAdaptive)
    [void](Wait-AccessibleElement $primaryWindow @($script:ui.RuAdaptiveHeading) $false)
    $path = Join-Path $OutputDirectory "settings-adaptive-ru.png"
    Capture-Window $primaryWindow $path
    [void]$screenshots.Add((Split-Path -Leaf $path))

    Invoke-NamedAccessibleElement $primaryWindow @($script:ui.RuRules)
    [void](Wait-AccessibleElement $primaryWindow @($script:ui.RuAddRule) $true)
    [void](Wait-AccessibleElement $primaryWindow @($script:ui.RuNoRules) $false)
    $path = Join-Path $OutputDirectory "settings-process-rules-ru.png"
    Capture-Window $primaryWindow $path
    [void]$screenshots.Add((Split-Path -Leaf $path))

    Invoke-NamedAccessibleElement $primaryWindow @("EN")
    [void](Wait-AccessibleElement $primaryWindow @("Logging") $true)
    Invoke-NamedAccessibleElement $primaryWindow @("Logging")
    [void](Wait-AccessibleElement $primaryWindow @("Diagnostic logging") $false)
    Set-NamedSelection $primaryWindow @("Normal (recommended)")
    Assert-NumericControl $primaryWindow @("Maximum active log size (MiB)") $true 2
    Assert-NumericControl $primaryWindow @("Retained circular archives") $true 2
    $path = Join-Path $OutputDirectory "settings-logging-enabled-en.png"
    Capture-Window $primaryWindow $path
    [void]$screenshots.Add((Split-Path -Leaf $path))

    $loggingOnBaseline = Get-StatusBaseline
    $apply = Wait-AccessibleElement $primaryWindow @("Apply") $true
    Assert-True $apply.Current.IsEnabled "Apply is disabled after selecting normal logging"
    Invoke-AccessibleElement $apply
    Wait-Condition "enabled logging persisted by GUI Apply" {
        try {
            $text = Get-Content -LiteralPath $script:configPath -Raw
            (Get-ConfigScalar $text "level") -eq "normal" -and
                (Get-ConfigScalar $text "max_file_size_mib") -eq "2" -and
                (Get-ConfigScalar $text "retained_archives") -eq "2"
        } catch {
            $false
        }
    }
    Wait-ServiceReload `
        $loggingOnBaseline `
        "observe" `
        "normal" `
        2 `
        2 `
        "service receipt after selecting normal logging in the GUI"
    Wait-Condition "background switches persisted by GUI Apply" {
        try {
            $text = Get-Content -LiteralPath $script:configPath -Raw
            (Get-ConfigSectionScalar $text "background_efficiency" "enabled") -eq "true" -and
                (Get-ConfigSectionScalar $text "background_efficiency" "eco_qos_enabled") -eq "false" -and
                (Get-ConfigSectionScalar $text "background_efficiency" "memory_priority_enabled") -eq "true" -and
                (Get-ConfigSectionScalar $text "background_efficiency" "memory_pressure_guard_enabled") -eq "true" -and
                (Get-ConfigSectionScalar $text "background_efficiency" "protect_foreground") -eq "true" -and
                (Get-ConfigSectionScalar $text "background_efficiency" "protect_visible") -eq "true" -and
                (Get-ConfigSectionScalar $text "background_efficiency" "protect_audio") -eq "true"
        } catch {
            $false
        }
    }
    $backgroundStatus = Read-ServiceStatus
    Assert-True ([bool]$backgroundStatus.applied_background_efficiency.enabled) `
        "service did not apply the GUI background master switch"
    Assert-True (-not [bool]$backgroundStatus.applied_background_efficiency.eco_qos_enabled) `
        "service unexpectedly enabled EcoQoS after GUI Apply"

    Set-NamedSelection $primaryWindow @("Trace")
    $traceBaseline = Get-StatusBaseline
    $apply = Wait-AccessibleElement $primaryWindow @("Apply") $true
    Assert-True $apply.Current.IsEnabled "Apply is disabled after selecting trace logging"
    Invoke-AccessibleElement $apply
    Wait-Condition "trace logging persisted by GUI Apply" {
        try {
            (Get-ConfigScalar (Get-Content -LiteralPath $script:configPath -Raw) "level") -eq "trace"
        } catch {
            $false
        }
    }
    Wait-ServiceReload `
        $traceBaseline `
        "observe" `
        "trace" `
        2 `
        2 `
        "service receipt after selecting trace logging in the GUI"
    $path = Join-Path $OutputDirectory "settings-logging-trace-en.png"
    Capture-Window $primaryWindow $path
    [void]$screenshots.Add((Split-Path -Leaf $path))

    Set-NamedSelection $primaryWindow @("Off")
    Assert-NumericControl $primaryWindow @("Maximum active log size (MiB)") $false 2
    Assert-NumericControl $primaryWindow @("Retained circular archives") $false 2
    $loggingOffBaseline = Get-StatusBaseline
    $apply = Wait-AccessibleElement $primaryWindow @("Apply") $true
    Assert-True $apply.Current.IsEnabled "Apply is disabled after selecting logging Off"
    Invoke-AccessibleElement $apply
    Wait-Condition "disabled logging persisted by GUI Apply" {
        try {
            $text = Get-Content -LiteralPath $script:configPath -Raw
            (Get-ConfigScalar $text "level") -eq "off" -and
                (Get-ConfigScalar $text "max_file_size_mib") -eq "2" -and
                (Get-ConfigScalar $text "retained_archives") -eq "2"
        } catch {
            $false
        }
    }
    Wait-ServiceReload `
        $loggingOffBaseline `
        "observe" `
        "off" `
        2 `
        2 `
        "service receipt after selecting logging Off in the GUI"
    Invoke-NamedAccessibleElement $primaryWindow @("Reload from disk")
    [void](Wait-AccessibleElement $primaryWindow @(
        "Configuration reloaded from disk."
    ) $false)
    $loggingOff = Wait-AccessibleElement $primaryWindow @("Off") $true
    Assert-True (Test-SelectionState $loggingOff) `
        "Reload from disk did not preserve logging level Off"
    Assert-NumericControl $primaryWindow @("Maximum active log size (MiB)") $false 2
    Assert-NumericControl $primaryWindow @("Retained circular archives") $false 2

    Invoke-NamedAccessibleElement $primaryWindow @("Background")
    foreach ($toggle in @(
        [pscustomobject]@{ Name = "Enable background efficiency"; State = "On" },
        [pscustomobject]@{ Name = "Apply EcoQoS"; State = "Off" },
        [pscustomobject]@{ Name = "Lower background memory priority"; State = "On" },
        [pscustomobject]@{ Name = "React to Windows low-memory notifications"; State = "On" },
        [pscustomobject]@{ Name = "Protect the foreground application"; State = "On" },
        [pscustomobject]@{ Name = "Protect visible and minimized applications"; State = "On" },
        [pscustomobject]@{ Name = "Protect applications with active audio"; State = "On" }
    )) {
        $control = Wait-AccessibleElement $primaryWindow @($toggle.Name) $true
        Assert-True ((Get-ToggleState $control) -eq $toggle.State) `
            "Reload from disk did not preserve Background control '$($toggle.Name)'"
    }

    [void](Wait-AccessibleElement $primaryWindow @("General") $true)
    Invoke-NamedAccessibleElement $primaryWindow @("General")
    Invoke-NamedAccessibleElement $primaryWindow @("Restore defaults...")
    [void](Wait-AccessibleElement $primaryWindow @(
        "Restore every setting and remove all process rules?"
    ) $false)
    [void](Wait-AccessibleElement $primaryWindow @("Confirm restore defaults") $true)
    [void](Wait-AccessibleElement $primaryWindow @("Keep current settings") $true)
    $path = Join-Path $OutputDirectory "settings-restore-confirmation-en.png"
    Capture-Window $primaryWindow $path
    [void]$screenshots.Add((Split-Path -Leaf $path))

    Invoke-NamedAccessibleElement $primaryWindow @("Confirm restore defaults")
    [void](Wait-AccessibleElement $primaryWindow @("Unsaved changes") $false)
    [void](Wait-AccessibleElement $primaryWindow @(
        "Defaults loaded into the editor. Choose Apply to save them."
    ) $false)
    $apply = Wait-AccessibleElement $primaryWindow @("Apply") $true
    Assert-True $apply.Current.IsEnabled "Apply is not enabled after restoring defaults in the editor"

    $applyBaseline = Get-StatusBaseline
    Invoke-AccessibleElement $apply
    Wait-Condition "product defaults persisted by Apply" {
        try {
            $text = Get-Content -LiteralPath $script:configPath -Raw
            Assert-ProductDefaults $text
            return $true
        } catch {
            return $false
        }
    } 20
    Wait-ServiceReload `
        $applyBaseline `
        "auto" `
        "normal" `
        10 `
        1 `
        "service confirmation of GUI Apply" `
        20
    [void](Wait-AccessibleElement $primaryWindow @(
        "Configuration applied and reloaded by the WinSched service."
    ) $false 20)
    $defaultsText = Get-Content -LiteralPath $script:configPath -Raw
    Assert-ProductDefaults $defaultsText
    $path = Join-Path $OutputDirectory "settings-defaults-applied-en.png"
    Capture-Window $primaryWindow $path
    [void]$screenshots.Add((Split-Path -Leaf $path))

    Invoke-NamedAccessibleElement $primaryWindow @("Reload from disk")
    [void](Wait-AccessibleElement $primaryWindow @(
        "Configuration reloaded from disk."
    ) $false)

    $close = Wait-AccessibleElement $primaryWindow @("Close") $true
    Invoke-AccessibleElement $close
    Wait-Condition "settings process exit through Close" {
        $null -eq (Get-Process -Id $primaryProcessId -ErrorAction SilentlyContinue)
    } 15
    Assert-True (@(Get-CurrentSessionSettingsProcesses).Count -eq 0) `
        "Settings process survived the Close control"

    $mainResult = "PASS"
    $result = [ordered]@{
        result = "PASS"
        cleanup_completed = $false
        session_id = $script:currentSessionId
        settings_pid = $primaryProcessId
        second_launch_pid = $secondProcessId
        second_launch_exit_code = $second.ExitCode
        single_instance_notice_dismissed = $singleInstanceNoticeDismissed
        single_instance_verified = $true
        languages = @("EN", "RU")
        pages = @("General", "Adaptive", "Responsiveness", "Background", "Process rules", "Logging")
        controls = @(
            "Tray autostart",
            "Enable topology-aware system reserve",
            "System reserve percent",
            "Minimum reserved cores",
            "Maximum reserved cores",
            "Latency target p99 (microseconds)",
            "Latency recovery p99 (microseconds)",
            "Adjustment stability samples",
            "Allow both SMT threads per physical core",
            "Minimum memory-profile cores",
            "Maximum memory-profile cores",
            "Memory resize cooldown (milliseconds)",
            "Enable background efficiency",
            "Apply EcoQoS",
            "Lower background memory priority",
            "React to Windows low-memory notifications",
            "Protect the foreground application",
            "Protect visible and minimized applications",
            "Protect applications with active audio",
            "Log detail level",
            "Off",
            "Normal (recommended)",
            "Trace",
            "Maximum active log size (MiB)",
            "Retained circular archives",
            "Restore defaults...",
            "Confirm restore defaults",
            "Apply",
            "Reload from disk",
            "Close"
        )
        controller_defaults_applied = $true
        responsiveness_defaults_applied = $true
        responsiveness_en_ru_ui = $true
        background_en_ru_ui = $true
        background_switch_persistence = $true
        logging_levels_persistence = $true
        logging_defaults_applied = $true
        tooltip_pages_verified = @("General", "Responsiveness", "Background", "Logging")
        tooltips_verified = 5
        service_reload_observed = $true
        original_config_sha256 = $originalHash
        screenshots = @($screenshots)
        accesskit_note = "UI Automation names and action patterns were resolved through egui/eframe AccessKit."
        phase = "main_acceptance_complete"
    }
} catch {
    $mainResult = "FAIL"
    $mainError = $_.Exception.ToString()
    $mainStack = $_.ScriptStackTrace
    if ($null -ne $primaryWindow) {
        try {
            $path = Join-Path $OutputDirectory "settings-ui-error.png"
            Capture-Window $primaryWindow $path
            [void]$screenshots.Add((Split-Path -Leaf $path))
        } catch {
        }
    }
    $result = [ordered]@{
        result = "FAIL"
        cleanup_completed = $false
        session_id = $script:currentSessionId
        settings_pid = $primaryProcessId
        second_launch_pid = $secondProcessId
        error = $mainError
        script_stack = $mainStack
        screenshots = @($screenshots)
        accesskit_note = "If failure is an element-name or pattern mismatch, inspect the VM UIA tree emitted by the current egui/eframe AccessKit build."
        phase = "main_acceptance_failed"
    }
} finally {
    # Publish a result before cleanup so failures never disappear behind cleanup work.
    Write-Result $result

    if ($primaryProcessId -gt 0 -and
        $null -ne (Get-Process -Id $primaryProcessId -ErrorAction SilentlyContinue)) {
        try {
            $window = Get-SettingsWindow ([DateTime]::MinValue)
            if ($null -ne $window) {
                $close = Find-AccessibleElement $window @("Close", $script:ui.RuClose) $true
                if ($null -ne $close -and $close.Current.IsEnabled) {
                    Invoke-AccessibleElement $close
                    Wait-Condition "settings cleanup close" {
                        $null -eq (Get-Process -Id $primaryProcessId -ErrorAction SilentlyContinue)
                    } 5
                }
            }
        } catch {
            [void]$cleanupErrors.Add("Graceful settings cleanup failed: $($_.Exception.Message)")
        }
        if ($null -ne (Get-Process -Id $primaryProcessId -ErrorAction SilentlyContinue)) {
            Stop-TrackedSettingsProcess $primaryProcessId
        }
    }

    if ($workingConfigInstalled -and $null -ne $originalBytes) {
        try {
            $restoreBaseline = Get-StatusBaseline
            Set-FileAtomically $script:configPath $originalBytes
            Wait-RestoreReload `
                $restoreBaseline `
                $originalMode `
                $originalLoggingLevel `
                $originalLogSizeMiB `
                $originalLogArchives `
                25
            $restoredHash = (Get-FileHash -LiteralPath $script:configPath -Algorithm SHA256).Hash.ToLowerInvariant()
            if ($restoredHash -ne $originalHash) {
                throw "Original configuration bytes were not restored exactly"
            }
            $result["original_config_restored"] = $true
            $result["restored_config_sha256"] = $restoredHash
            $configurationRestored = $true
        } catch {
            [void]$cleanupErrors.Add("Original configuration restore failed: $($_.Exception.Message)")
            $result["original_config_restored"] = $false
        }
    }

    if ($secondProcessId -gt 0 -and
        $secondLaunchedAfter -ne [DateTime]::MaxValue -and
        $null -ne (Get-Process -Id $secondProcessId -ErrorAction SilentlyContinue)) {
        [void](Dismiss-SingleInstanceNotice $secondLaunchedAfter)
    }

    foreach ($trackedProcessId in @($primaryProcessId, $launcherProcessId, $secondProcessId) | Select-Object -Unique) {
        if ($trackedProcessId -gt 0 -and
            $null -ne (Get-Process -Id $trackedProcessId -ErrorAction SilentlyContinue)) {
            Stop-TrackedSettingsProcess $trackedProcessId
        }
    }

    if ($configurationRestored) {
        Remove-Item -LiteralPath $backupPath -Force -ErrorAction SilentlyContinue
    } elseif (Test-Path -LiteralPath $backupPath -PathType Leaf) {
        $result["recovery_backup_path"] = $backupPath
    }
    $result["cleanup_completed"] = ($cleanupErrors.Count -eq 0 -and $configurationRestored)
    $result["cleanup_errors"] = @($cleanupErrors)
    $result["phase"] = "complete"
    if ($cleanupErrors.Count -gt 0) {
        $result["result"] = "FAIL"
        $result["error"] = @($cleanupErrors) -join "; "
    } else {
        $result["result"] = $mainResult
        if ($mainResult -eq "FAIL") {
            $result["error"] = $mainError
            $result["script_stack"] = $mainStack
        }
    }
    Write-Result $result
}

if ($result.result -ne "PASS") {
    exit 1
}
