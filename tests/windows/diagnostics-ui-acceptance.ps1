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
        Start-Sleep -Milliseconds 200
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Timed out waiting for: $Description"
}

function Get-AllElements($Root) {
    $elements = New-Object System.Collections.ArrayList
    [void]$elements.Add($Root)
    foreach ($element in $Root.FindAll(
        [System.Windows.Automation.TreeScope]::Descendants,
        [System.Windows.Automation.Condition]::TrueCondition
    )) {
        [void]$elements.Add($element)
    }
    $elements
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
            $legacy = $null
            if ($element.TryGetCurrentPattern(
                [System.Windows.Automation.LegacyIAccessiblePattern]::Pattern,
                [ref]$legacy
            ) -and -not [string]::IsNullOrWhiteSpace($legacy.Current.DefaultAction)) {
                return $element
            }
        } catch {
            continue
        }
    }
    return $null
}

function Wait-Element($Root, [string]$Name, [bool]$Actionable = $false, [int]$TimeoutSeconds = 30) {
    $script:elementCandidate = $null
    Wait-Condition "element '$Name'" {
        $candidate = Find-Element $Root $Name $Actionable
        if ($null -ne $candidate) {
            $script:elementCandidate = $candidate
            return $true
        }
        return $false
    } $TimeoutSeconds
    $script:elementCandidate
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
    $legacy = $null
    if ($Element.TryGetCurrentPattern(
        [System.Windows.Automation.LegacyIAccessiblePattern]::Pattern,
        [ref]$legacy
    ) -and -not [string]::IsNullOrWhiteSpace($legacy.Current.DefaultAction)) {
        $legacy.DoDefaultAction()
        return
    }
    throw "Element '$($Element.Current.Name)' is not actionable"
}

function Find-SettingsWindow([int]$ProcessId) {
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

New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$resultPath = Join-Path $OutputDirectory "settings-ui-result.json"
$settingsPath = Join-Path $InstallDirectory "winsched-settings.exe"
$configPath = Join-Path $DataDirectory "winsched.toml"
$process = $null
$savedReport = $null

try {
    Assert-True (Test-Path -LiteralPath $settingsPath -PathType Leaf) `
        "Settings executable is missing"
    Assert-True (@(Get-Process winsched-settings -ErrorAction SilentlyContinue).Count -eq 0) `
        "A Settings process is already running"
    $configHash = (Get-FileHash -LiteralPath $configPath -Algorithm SHA256).Hash
    $downloads = Join-Path $env:USERPROFILE "Downloads"
    $beforeReports = @(
        Get-ChildItem -LiteralPath $downloads -Filter "WinSched-diagnostic-*.json" `
            -ErrorAction SilentlyContinue |
            Select-Object -ExpandProperty FullName
    )

    $process = Start-Process -FilePath $settingsPath -WorkingDirectory $InstallDirectory -PassThru
    Wait-Condition "WinSched Settings window" {
        $candidate = Find-SettingsWindow $process.Id
        if ($null -ne $candidate) {
            $script:settingsWindowCandidate = $candidate
            return $true
        }
        return $false
    }
    $window = $script:settingsWindowCandidate

    Invoke-Element (Wait-Element $window "EN" $true)
    Invoke-Element (Wait-Element $window "Diagnostics" $true)
    [void](Wait-Element $window "Passive diagnostics" $false)
    Invoke-Element (Wait-Element $window "Run passive 10-second diagnostic" $true)
    [void](Wait-Element $window "Copy JSON" $true 35)
    [void](Wait-Element $window "Measurements" $false)
    [void](Wait-Element $window "Findings" $false)
    Invoke-Element (Wait-Element $window "Save JSON to Downloads" $true)

    Wait-Condition "saved diagnostic JSON" {
        $after = @(
            Get-ChildItem -LiteralPath $downloads -Filter "WinSched-diagnostic-*.json" `
                -ErrorAction SilentlyContinue |
                Select-Object -ExpandProperty FullName
        )
        $new = @($after | Where-Object { $beforeReports -notcontains $_ })
        if ($new.Count -eq 1) {
            $script:savedReportCandidate = $new[0]
            return $true
        }
        return $false
    }
    $savedReport = $script:savedReportCandidate
    $raw = Get-Content -LiteralPath $savedReport -Raw
    $report = $raw | ConvertFrom-Json
    Assert-True ([int]$report.schema_version -eq 1) "Unexpected diagnostic schema"
    Assert-True ([int]$report.sample_count -ge 30) "Too few diagnostic samples"
    Assert-True ([bool]$report.shell.taskbar.available) `
        "Interactive taskbar probe was unavailable"
    Assert-True ([int]$report.shell.taskbar.samples -eq [int]$report.sample_count) `
        "Taskbar and system sample counts differ"
    Assert-True (@($report.findings).Count -gt 0) "Diagnostic produced no finding"
    Assert-True (-not $raw.Contains("C:\Users\")) "Report leaked a user path"
    Assert-True (-not $raw.Contains("window_title")) "Report leaked a window title field"
    Assert-True (-not [bool]$report.virtualization.wsl_advice.automatic_changes_performed) `
        "WSL advisor reported an automatic change"
    Assert-True ((Get-FileHash -LiteralPath $configPath -Algorithm SHA256).Hash -eq $configHash) `
        "Diagnostic changed the WinSched configuration"

    Invoke-Element (Wait-Element $window "Close" $true)
    Wait-Condition "Settings process exit" {
        $null -eq (Get-Process -Id $process.Id -ErrorAction SilentlyContinue)
    }
    Remove-Item -LiteralPath $savedReport -Force
    Assert-True (-not (Test-Path -LiteralPath $savedReport)) `
        "Saved diagnostic report was not cleaned up"
    $savedReport = $null

    [pscustomobject]@{
        result = "PASS"
        session_id = [Diagnostics.Process]::GetCurrentProcess().SessionId
        diagnostic_schema = [int]$report.schema_version
        sample_count = [int]$report.sample_count
        taskbar_available = [bool]$report.shell.taskbar.available
        taskbar_timeouts = [int]$report.shell.taskbar.timeout_samples
        finding_codes = @($report.findings | ForEach-Object { $_.code })
        privacy_safe = $true
        configuration_changed = $false
        cleanup_completed = $true
    } | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $resultPath -Encoding UTF8
}
catch {
    if ($null -ne $savedReport -and (Test-Path -LiteralPath $savedReport)) {
        Remove-Item -LiteralPath $savedReport -Force -ErrorAction SilentlyContinue
        $savedReport = $null
    }
    if ($process -and $null -ne (Get-Process -Id $process.Id -ErrorAction SilentlyContinue)) {
        Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
        $process.WaitForExit(5000) | Out-Null
    }
    $cleanupCompleted = `
        $null -eq $savedReport -and `
        ($null -eq $process -or $null -eq (Get-Process -Id $process.Id -ErrorAction SilentlyContinue))
    [pscustomobject]@{
        result = "FAIL"
        error = $_.Exception.ToString()
        script_stack = $_.ScriptStackTrace
        cleanup_completed = $cleanupCompleted
    } | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $resultPath -Encoding UTF8
    exit 1
}
finally {
    if ($null -ne $savedReport -and (Test-Path -LiteralPath $savedReport)) {
        Remove-Item -LiteralPath $savedReport -Force -ErrorAction SilentlyContinue
    }
    if ($process -and $null -ne (Get-Process -Id $process.Id -ErrorAction SilentlyContinue)) {
        Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
    }
}
