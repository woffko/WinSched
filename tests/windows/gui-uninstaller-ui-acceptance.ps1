[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory,
    [ValidateSet("Preserve", "Purge")]
    [string]$PurgeChoice = "Preserve",
    [string]$InstallDirectory = "$env:ProgramFiles\WinSched",
    [string]$DataDirectory = "$env:ProgramData\WinSched"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class WinSchedUninstallWindow {
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    public static extern IntPtr SendMessage(IntPtr window, uint message, IntPtr wParam, IntPtr lParam);
    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr window);
    public const uint BM_CLICK = 0x00F5;
    public const uint WM_KEYDOWN = 0x0100;
    public const uint WM_KEYUP = 0x0101;
    public const int VK_RETURN = 0x0D;
}
"@

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

function Add-TrackedProcessId([int]$ProcessId) {
    if ($ProcessId -gt 0 -and $ProcessId -ne $PID) {
        [void]$script:trackedProcessIds.Add($ProcessId)
    }
}

function Get-FreshUninstallWindows([datetime]$LaunchedAfter) {
    $matches = @()
    $windows = [System.Windows.Automation.AutomationElement]::RootElement.FindAll(
        [System.Windows.Automation.TreeScope]::Children,
        [System.Windows.Automation.Condition]::TrueCondition
    )
    foreach ($window in $windows) {
        try {
            if ($window.Current.ControlType -ne [System.Windows.Automation.ControlType]::Window -or
                $window.Current.Name -notlike "*WinSched*Uninstall*") {
                continue
            }

            $owner = Get-Process -Id $window.Current.ProcessId -ErrorAction SilentlyContinue
            if ($null -eq $owner -or
                $owner.SessionId -ne $script:currentSessionId -or
                $owner.StartTime -lt $LaunchedAfter.AddSeconds(-5)) {
                continue
            }

            Add-TrackedProcessId $owner.Id
            $matches += $window
        } catch {
            continue
        }
    }
    return $matches
}

function Test-WindowContains($Window, [string]$NamePattern) {
    $elements = $Window.FindAll(
        [System.Windows.Automation.TreeScope]::Descendants,
        [System.Windows.Automation.Condition]::TrueCondition
    )
    foreach ($element in $elements) {
        try {
            if ($element.Current.Name -like $NamePattern) {
                return $true
            }
        } catch {
            continue
        }
    }
    return $false
}

function Find-UninstallDialog(
    [datetime]$LaunchedAfter,
    [string]$ContentPattern
) {
    foreach ($window in @(Get-FreshUninstallWindows $LaunchedAfter)) {
        try {
            if (Test-WindowContains $window $ContentPattern) {
                return $window
            }
        } catch {
            continue
        }
    }
    return $null
}

function Find-DialogButton($Dialog, [string]$Name) {
    $buttons = $Dialog.FindAll(
        [System.Windows.Automation.TreeScope]::Descendants,
        (New-Object System.Windows.Automation.PropertyCondition(
            [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
            [System.Windows.Automation.ControlType]::Button
        ))
    )
    foreach ($button in $buttons) {
        try {
            $normalizedName = $button.Current.Name.Replace("&", "").Trim()
            if ($normalizedName -eq $Name) {
                return $button
            }
        } catch {
            continue
        }
    }
    return $null
}

function Test-SameAutomationElement($Left, $Right) {
    if ($null -eq $Left -or $null -eq $Right) {
        return $false
    }
    try {
        $leftId = [string]::Join(".", $Left.GetRuntimeId())
        $rightId = [string]::Join(".", $Right.GetRuntimeId())
        return $leftId -eq $rightId
    } catch {
        return $false
    }
}

function Test-ElementHasFocus($Element) {
    try {
        if ($Element.Current.HasKeyboardFocus) {
            return $true
        }
        return Test-SameAutomationElement `
            $Element `
            ([System.Windows.Automation.AutomationElement]::FocusedElement)
    } catch {
        return $false
    }
}

function Invoke-AutomationElement($Element) {
    Assert-True ($null -ne $Element) "Automation element is missing"
    Assert-True $Element.Current.IsEnabled "Automation element '$($Element.Current.Name)' is disabled"
    $pattern = $null
    if (-not $Element.TryGetCurrentPattern(
        [System.Windows.Automation.InvokePattern]::Pattern,
        [ref]$pattern
    )) {
        throw "Element '$($Element.Current.Name)' does not support InvokePattern"
    }
    $pattern.Invoke()
}

function Invoke-NativeDialogButton($Dialog, $Button, [string]$FallbackKeys) {
    try {
        $Button.SetFocus()
    } catch {
    }
    $dialogHandle = [IntPtr]$Dialog.Current.NativeWindowHandle
    if ($dialogHandle -ne [IntPtr]::Zero) {
        [void][WinSchedUninstallWindow]::SetForegroundWindow($dialogHandle)
        Start-Sleep -Milliseconds 250
    }

    $buttonHandle = [IntPtr]$Button.Current.NativeWindowHandle
    if ($buttonHandle -ne [IntPtr]::Zero) {
        [void][WinSchedUninstallWindow]::SendMessage(
            $buttonHandle,
            [WinSchedUninstallWindow]::BM_CLICK,
            [IntPtr]::Zero,
            [IntPtr]::Zero
        )
        Start-Sleep -Milliseconds 250
    }

    if ($dialogHandle -ne [IntPtr]::Zero) {
        [void][WinSchedUninstallWindow]::SendMessage(
            $dialogHandle,
            [WinSchedUninstallWindow]::WM_KEYDOWN,
            [IntPtr][WinSchedUninstallWindow]::VK_RETURN,
            [IntPtr]::Zero
        )
        [void][WinSchedUninstallWindow]::SendMessage(
            $dialogHandle,
            [WinSchedUninstallWindow]::WM_KEYUP,
            [IntPtr][WinSchedUninstallWindow]::VK_RETURN,
            [IntPtr]::Zero
        )
        return
    }
    [System.Windows.Forms.SendKeys]::SendWait($FallbackKeys)
}

function Capture-Window($Window, [string]$Path) {
    try {
        $Window.SetFocus()
        Start-Sleep -Milliseconds 200
    } catch {
    }

    $rect = $Window.Current.BoundingRectangle
    $x = [int][Math]::Floor($rect.X)
    $y = [int][Math]::Floor($rect.Y)
    $width = [int][Math]::Ceiling($rect.Width)
    $height = [int][Math]::Ceiling($rect.Height)
    Assert-True ($width -gt 100 -and $height -gt 80) "Uninstaller window bounds are invalid"

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

function Stop-TrackedUninstallProcesses {
    foreach ($processId in @($script:trackedProcessIds)) {
        if ($processId -eq $PID) {
            continue
        }
        try {
            $process = Get-Process -Id $processId -ErrorAction Stop
            if ($process.Id -eq $PID -or
                $process.SessionId -ne $script:currentSessionId -or
                $process.StartTime -lt $script:launchedAfter.AddSeconds(-5) -or
                (
                    $process.ProcessName -notlike "unins*" -and
                    $process.ProcessName -notlike "_unins*"
                )) {
                continue
            }
            Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
        } catch {
        }
    }
}

function Get-FreshUninstallerProcesses {
    return @(
        Get-Process -Name "unins*", "_unins*" -ErrorAction SilentlyContinue |
            Where-Object {
                try {
                    $_.Id -ne $PID -and
                        $_.SessionId -eq $script:currentSessionId -and
                        $_.StartTime -ge $script:launchedAfter.AddSeconds(-5)
                } catch {
                    $false
                }
            }
    )
}

function Resolve-UninstallerPath([string]$Directory) {
    $candidates = @(
        Get-ChildItem -LiteralPath $Directory -Filter "unins*.exe" -File -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -match '^unins\d+\.exe$' } |
            Sort-Object LastWriteTimeUtc -Descending
    )
    Assert-True ($candidates.Count -gt 0) "Inno Setup uninstaller is missing from '$Directory'"
    return $candidates[0].FullName
}

New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$scenario = $PurgeChoice.ToLowerInvariant()
$resultPath = Join-Path $OutputDirectory "gui-uninstaller-$scenario-result.json"
$confirmationScreenshot = Join-Path $OutputDirectory "uninstaller-$scenario-confirmation.png"
$purgeScreenshot = Join-Path $OutputDirectory "uninstaller-$scenario-purge-prompt.png"
$completionScreenshot = Join-Path $OutputDirectory "uninstaller-$scenario-complete.png"
$errorScreenshot = Join-Path $OutputDirectory "uninstaller-$scenario-error.png"
Remove-Item -LiteralPath $resultPath -Force -ErrorAction SilentlyContinue

$script:currentSessionId = (Get-Process -Id $PID).SessionId
$script:trackedProcessIds = New-Object 'System.Collections.Generic.HashSet[int]'
$script:launchedAfter = [DateTime]::MaxValue
$launcher = $null
$confirmationDialog = $null
$purgeDialog = $null
$completionDialog = $null
$result = $null
$exitCode = 0
$resultWritten = $false
$configHashBefore = $null
$configHashAfter = $null
$markerPath = Join-Path $DataDirectory "gui-uninstaller-acceptance.marker"
$configPath = Join-Path $DataDirectory "winsched.toml"
$startupShortcut = Join-Path $env:ProgramData "Microsoft\Windows\Start Menu\Programs\Startup\WinSched Tray.lnk"
$groupDirectory = Join-Path $env:ProgramData "Microsoft\Windows\Start Menu\Programs\WinSched"
$groupShortcut = Join-Path $groupDirectory "WinSched.lnk"
$groupUninstallShortcut = Join-Path $groupDirectory "Uninstall WinSched.lnk"
$desktopShortcut = Join-Path $env:Public "Desktop\WinSched.lnk"
$desktopShortcutExisted = $false

try {
    Assert-True (Test-Path -LiteralPath $InstallDirectory -PathType Container) `
        "WinSched install directory is missing"
    $uninstallerPath = Resolve-UninstallerPath $InstallDirectory
    Assert-True (Test-Path -LiteralPath $configPath -PathType Leaf) `
        "WinSched configuration is missing before uninstall"
    Assert-True ($null -ne (Get-Service -Name "WinSched" -ErrorAction SilentlyContinue)) `
        "WinSched service is missing before uninstall"
    Assert-True (Test-Path -LiteralPath $startupShortcut -PathType Leaf) `
        "Default Startup shortcut is missing before uninstall"
    Assert-True (Test-Path -LiteralPath $groupShortcut -PathType Leaf) `
        "Start Menu WinSched shortcut is missing before uninstall"
    Assert-True (Test-Path -LiteralPath $groupUninstallShortcut -PathType Leaf) `
        "Start Menu uninstall shortcut is missing before uninstall"

    $desktopShortcutExisted = Test-Path -LiteralPath $desktopShortcut -PathType Leaf
    $markerValue = "WinSched GUI uninstall acceptance $PurgeChoice $([Guid]::NewGuid())"
    Set-Content -LiteralPath $markerPath -Value $markerValue -Encoding UTF8
    $configHashBefore = (Get-FileHash -LiteralPath $configPath -Algorithm SHA256).Hash

    $script:launchedAfter = Get-Date
    $launcher = Start-Process `
        -FilePath $uninstallerPath `
        -ArgumentList @("/LANG=english") `
        -PassThru
    Add-TrackedProcessId $launcher.Id

    Wait-Condition -Description "standard WinSched uninstall confirmation" -TimeoutSeconds 30 -Condition {
        $candidate = Find-UninstallDialog `
            $script:launchedAfter `
            "*completely remove WinSched*"
        if ($null -ne $candidate) {
            $script:confirmationDialog = $candidate
            return $true
        }
        return $false
    }
    $confirmationDialog = $script:confirmationDialog
    Capture-Window $confirmationDialog $confirmationScreenshot
    $initialYes = Find-DialogButton $confirmationDialog "Yes"
    Assert-True ($null -ne $initialYes) "Initial uninstall Yes button was not found"
    Invoke-AutomationElement $initialYes

    Wait-Condition -Description "WinSched data purge prompt" -TimeoutSeconds 60 -Condition {
        $candidate = Find-UninstallDialog `
            $script:launchedAfter `
            "*Also remove the WinSched configuration*"
        if ($null -ne $candidate) {
            $script:purgeDialog = $candidate
            return $true
        }
        return $false
    }
    $purgeDialog = $script:purgeDialog
    $purgeYes = Find-DialogButton $purgeDialog "Yes"
    $purgeNo = Find-DialogButton $purgeDialog "No"
    Assert-True ($null -ne $purgeYes) "Purge prompt Yes button was not found"
    Assert-True ($null -ne $purgeNo) "Purge prompt No button was not found"

    Wait-Condition -Description "purge prompt default focus on No" -TimeoutSeconds 5 -Condition {
        Test-ElementHasFocus $purgeNo
    }
    Capture-Window $purgeDialog $purgeScreenshot

    if ($PurgeChoice -eq "Preserve") {
        Invoke-NativeDialogButton $purgeDialog $purgeNo "{ENTER}"
    } else {
        Invoke-NativeDialogButton $purgeDialog $purgeYes "%Y"
    }

    Wait-Condition -Description "WinSched uninstall completion dialog" -TimeoutSeconds 90 -Condition {
        $candidate = Find-UninstallDialog `
            $script:launchedAfter `
            "*successfully removed*"
        if ($null -ne $candidate) {
            $script:completionDialog = $candidate
            return $true
        }
        return $false
    }
    $completionDialog = $script:completionDialog
    Capture-Window $completionDialog $completionScreenshot
    $completionOk = Find-DialogButton $completionDialog "OK"
    Assert-True ($null -ne $completionOk) "Uninstall completion OK button was not found"
    Invoke-NativeDialogButton $completionDialog $completionOk "{ENTER}"

    Wait-Condition -Description "all WinSched uninstaller windows closed" -TimeoutSeconds 90 -Condition {
        @(Get-FreshUninstallWindows $script:launchedAfter).Count -eq 0
    }
    Wait-Condition -Description "all WinSched uninstaller processes exited" -TimeoutSeconds 90 -Condition {
        @(Get-FreshUninstallerProcesses).Count -eq 0
    }
    Wait-Condition -Description "WinSched service removed" -TimeoutSeconds 30 -Condition {
        $null -eq (Get-Service -Name "WinSched" -ErrorAction SilentlyContinue)
    }
    Wait-Condition -Description "WinSched Program Files directory removed" -TimeoutSeconds 30 -Condition {
        -not (Test-Path -LiteralPath $InstallDirectory)
    }
    Wait-Condition -Description "WinSched shortcuts removed" -TimeoutSeconds 30 -Condition {
        -not (Test-Path -LiteralPath $startupShortcut) -and
        -not (Test-Path -LiteralPath $groupShortcut) -and
        -not (Test-Path -LiteralPath $groupUninstallShortcut) -and
        (-not $desktopShortcutExisted -or -not (Test-Path -LiteralPath $desktopShortcut))
    }

    Assert-True ($null -eq (Get-CimInstance Win32_Service -Filter "Name='WinSched'" -ErrorAction SilentlyContinue)) `
        "WinSched remains registered in the Service Control Manager"

    if ($PurgeChoice -eq "Preserve") {
        Assert-True (Test-Path -LiteralPath $DataDirectory -PathType Container) `
            "WinSched data directory was removed after choosing No"
        Assert-True (Test-Path -LiteralPath $configPath -PathType Leaf) `
            "WinSched configuration was removed after choosing No"
        Assert-True (Test-Path -LiteralPath $markerPath -PathType Leaf) `
            "Acceptance marker was removed after choosing No"
        Assert-True ((Get-Content -LiteralPath $markerPath -Raw).Trim() -eq $markerValue) `
            "Acceptance marker changed after choosing No"
        $configHashAfter = (Get-FileHash -LiteralPath $configPath -Algorithm SHA256).Hash
        Assert-True ($configHashAfter -eq $configHashBefore) `
            "WinSched configuration bytes changed during preserve uninstall"
        Remove-Item -LiteralPath $markerPath -Force
        Assert-True (-not (Test-Path -LiteralPath $markerPath)) `
            "Acceptance marker cleanup failed after preserve verification"
    } else {
        Assert-True (-not (Test-Path -LiteralPath $DataDirectory)) `
            "WinSched data directory remains after choosing Yes"
    }

    $result = [pscustomobject][ordered]@{
        result = "PASS"
        scenario = $PurgeChoice
        purge_prompt_default = "No"
        purge_choice = $(if ($PurgeChoice -eq "Preserve") { "No" } else { "Yes" })
        launcher_process_id = $launcher.Id
        observed_uninstaller_process_ids = @($trackedProcessIds)
        service_removed = $true
        install_directory_removed = $true
        startup_shortcut_removed = $true
        start_menu_shortcuts_removed = $true
        desktop_shortcut_was_present = $desktopShortcutExisted
        desktop_shortcut_removed_if_present = $true
        data_directory_preserved = ($PurgeChoice -eq "Preserve")
        data_directory_purged = ($PurgeChoice -eq "Purge")
        acceptance_marker_cleaned = ($PurgeChoice -eq "Preserve")
        config_sha256_before = $configHashBefore
        config_sha256_after = $configHashAfter
        screenshots = @(
            [IO.Path]::GetFileName($confirmationScreenshot),
            [IO.Path]::GetFileName($purgeScreenshot),
            [IO.Path]::GetFileName($completionScreenshot)
        )
    }
} catch {
    $exitCode = 1
    foreach ($window in @($completionDialog, $purgeDialog, $confirmationDialog)) {
        if ($null -eq $window) {
            continue
        }
        try {
            Capture-Window $window $errorScreenshot
            break
        } catch {
        }
    }
    if (-not (Test-Path -LiteralPath $errorScreenshot -PathType Leaf) -and
        $script:launchedAfter -ne [DateTime]::MaxValue) {
        foreach ($window in @(Get-FreshUninstallWindows $script:launchedAfter)) {
            try {
                Capture-Window $window $errorScreenshot
                break
            } catch {
            }
        }
    }
    $result = [pscustomobject][ordered]@{
        result = "FAIL"
        scenario = $PurgeChoice
        error = $_.Exception.ToString()
        script_stack = $_.ScriptStackTrace
        launcher_process_id = $(if ($null -ne $launcher) { $launcher.Id } else { $null })
        observed_uninstaller_process_ids = @($trackedProcessIds)
        error_screenshot = $(
            if (Test-Path -LiteralPath $errorScreenshot -PathType Leaf) {
                [IO.Path]::GetFileName($errorScreenshot)
            } else {
                $null
            }
        )
    }
} finally {
    if ($null -eq $result) {
        $exitCode = 1
        $result = [pscustomobject][ordered]@{
            result = "FAIL"
            scenario = $PurgeChoice
            error = "Acceptance test ended without producing a result object."
        }
    }

    try {
        $result | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $resultPath -Encoding UTF8
        $resultWritten = $true
    } catch {
        [Console]::Error.WriteLine("Could not write acceptance result '$resultPath': $($_.Exception.Message)")
        $exitCode = 1
    }

    if ($resultWritten -and $exitCode -ne 0) {
        Stop-TrackedUninstallProcesses
    }
}

exit $exitCode
