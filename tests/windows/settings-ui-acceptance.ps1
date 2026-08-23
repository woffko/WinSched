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
    RuRules = Expand-UnicodeEscapes "\u041f\u0440\u0430\u0432\u0438\u043b\u0430 \u043f\u0440\u043e\u0446\u0435\u0441\u0441\u043e\u0432"
    RuAddRule = Expand-UnicodeEscapes "\u0414\u043e\u0431\u0430\u0432\u0438\u0442\u044c \u043f\u0440\u0430\u0432\u0438\u043b\u043e \u043f\u0440\u043e\u0446\u0435\u0441\u0441\u0430"
    RuNoRules = Expand-UnicodeEscapes "\u042f\u0432\u043d\u044b\u0435 \u043f\u0440\u0430\u0432\u0438\u043b\u0430 \u043f\u0440\u043e\u0446\u0435\u0441\u0441\u043e\u0432 \u043d\u0435 \u043d\u0430\u0441\u0442\u0440\u043e\u0435\u043d\u044b."
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

function Get-StatusTimestamp {
    $status = Read-ServiceStatus
    if ($null -eq $status) {
        return [uint64]0
    }
    return [uint64]$status.updated_at_unix_ms
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
    [uint64]$AfterTimestamp,
    [int64]$AfterLogLength,
    [string]$ExpectedMode,
    [string]$Description,
    [int]$TimeoutSeconds = 20
) {
    Wait-Condition $Description {
        $status = Read-ServiceStatus
        $logTail = Read-FileTail $script:logPath $AfterLogLength
        $null -ne $status -and
            [uint64]$status.updated_at_unix_ms -gt $AfterTimestamp -and
            $status.configured_mode -eq $ExpectedMode -and
            $status.last_activity -ne "Configuration rejected; fail-closed" -and
            $logTail.Contains('"event":"config_reloaded"')
    } $TimeoutSeconds
}

function Get-FileLength([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        return [int64]0
    }
    return [int64](Get-Item -LiteralPath $Path).Length
}

function Read-FileTail([string]$Path, [int64]$Offset) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        return ""
    }
    $stream = [System.IO.File]::Open(
        $Path,
        [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::Read,
        ([System.IO.FileShare]::ReadWrite -bor [System.IO.FileShare]::Delete)
    )
    try {
        if ($stream.Length -lt $Offset) {
            return ""
        }
        [void]$stream.Seek($Offset, [System.IO.SeekOrigin]::Begin)
        $remaining = [int]($stream.Length - $Offset)
        $bytes = New-Object byte[] $remaining
        $read = $stream.Read($bytes, 0, $remaining)
        return [System.Text.Encoding]::UTF8.GetString($bytes, 0, $read)
    } finally {
        $stream.Dispose()
    }
}

function Wait-RestoreReload(
    [uint64]$AfterTimestamp,
    [int64]$AfterLogLength,
    [string]$ExpectedMode,
    [int]$TimeoutSeconds = 25
) {
    Wait-Condition "service reload after restoring original configuration" {
        $status = Read-ServiceStatus
        $logTail = Read-FileTail $script:logPath $AfterLogLength
        $null -ne $status -and
            [uint64]$status.updated_at_unix_ms -gt $AfterTimestamp -and
            $status.configured_mode -eq $ExpectedMode -and
            $status.last_activity -ne "Configuration rejected; fail-closed" -and
            $logTail.Contains('"event":"config_reloaded"')
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

function Invoke-NamedAccessibleElement($Root, [string[]]$Names) {
    $element = Wait-AccessibleElement $Root $Names $true
    Invoke-AccessibleElement $element
}

function Test-AccessibleName($Root, [string[]]$Names) {
    return $null -ne (Find-AccessibleElement $Root $Names $false)
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

function Assert-ProductDefaults([string]$Text) {
    $expected = [ordered]@{
        schema_version = "1"
        controller_mode = "auto"
        sample_interval_ms = "1000"
        minimum_process_utilization_bps = "500"
        all_user_processes = "true"
        default_rule_mode = "auto"
        overload_threshold_bps = "8500"
        minimum_improvement_bps = "2000"
        stability_samples = "3"
        minimum_residency_ms = "10000"
        cooldown_ms = "30000"
        max_mutations_per_evaluation = "1"
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
$script:logPath = Join-Path $DataDirectory "winsched.log"
$script:currentSessionId = (Get-Process -Id $PID).SessionId
$script:marker = "settings-ui-acceptance-{0}" -f [Guid]::NewGuid().ToString("N")
$backupPath = Join-Path $env:TEMP ("winsched-settings-ui-{0}.bak" -f [Guid]::NewGuid().ToString("N"))
$screenshots = New-Object System.Collections.ArrayList
$originalBytes = $null
$originalHash = $null
$workingConfigInstalled = $false
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
    Assert-True (Test-Path -LiteralPath $script:logPath -PathType Leaf) `
        "WinSched event log is missing"
    $service = Get-Service -Name "WinSched" -ErrorAction SilentlyContinue
    Assert-True ($null -ne $service -and $service.Status -eq "Running") `
        "WinSched service must be running"
    Assert-True (@(Get-CurrentSessionSettingsProcesses).Count -eq 0) `
        "A WinSched Settings process is already running in the interactive session"

    $originalBytes = [System.IO.File]::ReadAllBytes($script:configPath)
    [System.IO.File]::WriteAllBytes($backupPath, $originalBytes)
    $originalHash = (Get-FileHash -LiteralPath $backupPath -Algorithm SHA256).Hash.ToLowerInvariant()

    $workingConfig = @"
# External backup marker: $($script:marker)
schema_version = 1
controller_mode = "observe"
sample_interval_ms = 2500
minimum_process_utilization_bps = 500
all_user_processes = false
default_rule_mode = "auto"

[policy]
overload_threshold_bps = 8500
minimum_improvement_bps = 2000
stability_samples = 3
minimum_residency_ms = 10000
cooldown_ms = 30000
max_mutations_per_evaluation = 1
"@
    $workingBaseline = Get-StatusTimestamp
    $workingLogBaseline = Get-FileLength $script:logPath
    Set-Utf8FileAtomically $script:configPath $workingConfig
    $workingConfigInstalled = $true
    Wait-ServiceReload `
        $workingBaseline `
        $workingLogBaseline `
        "observe" `
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

    Invoke-NamedAccessibleElement $primaryWindow @("Process rules")
    [void](Wait-AccessibleElement $primaryWindow @("Add process rule") $true)
    [void](Wait-AccessibleElement $primaryWindow @(
        "No explicit process rules are configured."
    ) $false)
    $path = Join-Path $OutputDirectory "settings-process-rules-en.png"
    Capture-Window $primaryWindow $path
    [void]$screenshots.Add((Split-Path -Leaf $path))

    Invoke-NamedAccessibleElement $primaryWindow @($script:ui.RuLanguage)
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

    $applyBaseline = Get-StatusTimestamp
    $applyLogBaseline = Get-FileLength $script:logPath
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
    Wait-ServiceReload $applyBaseline $applyLogBaseline "auto" "service confirmation of GUI Apply" 20
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
        pages = @("General", "Adaptive", "Process rules")
        controls = @(
            "Tray autostart",
            "Restore defaults...",
            "Confirm restore defaults",
            "Apply",
            "Reload from disk",
            "Close"
        )
        controller_defaults_applied = $true
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
            $restoreBaseline = Get-StatusTimestamp
            $restoreLogBaseline = Get-FileLength $script:logPath
            $originalText = [System.Text.Encoding]::UTF8.GetString($originalBytes)
            $originalMode = Get-ConfigScalar $originalText "controller_mode"
            Set-FileAtomically $script:configPath $originalBytes
            Wait-RestoreReload $restoreBaseline $restoreLogBaseline $originalMode 25
            $restoredHash = (Get-FileHash -LiteralPath $script:configPath -Algorithm SHA256).Hash.ToLowerInvariant()
            if ($restoredHash -ne $originalHash) {
                throw "Original configuration bytes were not restored exactly"
            }
            $result["original_config_restored"] = $true
            $result["restored_config_sha256"] = $restoredHash
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

    Remove-Item -LiteralPath $backupPath -Force -ErrorAction SilentlyContinue
    $result["cleanup_completed"] = $true
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
