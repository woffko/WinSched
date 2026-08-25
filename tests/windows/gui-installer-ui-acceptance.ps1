[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$SetupPath,
    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory,
    [string]$ExpectedConfigMarker = ""
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -AssemblyName System.Drawing

function Assert-True($Condition, [string]$Message) {
    if (-not [bool]$Condition) {
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

function Get-SetupRoot($Process) {
    try {
        $Process.Refresh()
        if ($Process.MainWindowHandle -ne 0) {
            return [System.Windows.Automation.AutomationElement]::FromHandle(
                $Process.MainWindowHandle
            )
        }
    } catch {
        return $null
    }

    $processCondition = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::ProcessIdProperty,
        [int]$Process.Id
    )
    return [System.Windows.Automation.AutomationElement]::RootElement.FindFirst(
        [System.Windows.Automation.TreeScope]::Children,
        $processCondition
    )
}

function Find-SetupWindow([datetime]$LaunchedAfter) {
    $currentSessionId = (Get-Process -Id $PID).SessionId
    $windows = [System.Windows.Automation.AutomationElement]::RootElement.FindAll(
        [System.Windows.Automation.TreeScope]::Children,
        [System.Windows.Automation.Condition]::TrueCondition
    )
    foreach ($window in $windows) {
        try {
            if ($window.Current.ControlType -ne [System.Windows.Automation.ControlType]::Window -or
                $window.Current.Name -ne "Setup - WinSched") {
                continue
            }
            $owner = Get-Process -Id $window.Current.ProcessId -ErrorAction SilentlyContinue
            if ($owner -and
                $owner.SessionId -eq $currentSessionId -and
                $owner.StartTime -ge $LaunchedAfter.AddSeconds(-2)) {
                return $window
            }
        } catch {
            continue
        }
    }
    return $null
}

function Find-Element(
    $Root,
    [System.Windows.Automation.ControlType]$ControlType,
    [string]$NamePattern
) {
    $elements = $Root.FindAll(
        [System.Windows.Automation.TreeScope]::Descendants,
        [System.Windows.Automation.Condition]::TrueCondition
    )
    foreach ($element in $elements) {
        try {
            if ($element.Current.ControlType -eq $ControlType -and
                $element.Current.Name -like $NamePattern) {
                return $element
            }
        } catch {
            continue
        }
    }
    return $null
}

function Find-ElementByName($Root, [string]$NamePattern) {
    $elements = $Root.FindAll(
        [System.Windows.Automation.TreeScope]::Descendants,
        [System.Windows.Automation.Condition]::TrueCondition
    )
    foreach ($element in $elements) {
        try {
            if ($element.Current.Name -like $NamePattern) {
                return $element
            }
        } catch {
            continue
        }
    }
    return $null
}

function Invoke-Element($Element) {
    $pattern = $null
    if (-not $Element.TryGetCurrentPattern(
        [System.Windows.Automation.InvokePattern]::Pattern,
        [ref]$pattern
    )) {
        throw "Element '$($Element.Current.Name)' does not support InvokePattern"
    }
    $pattern.Invoke()
}

function Select-Radio($Element) {
    $pattern = $null
    if (-not $Element.TryGetCurrentPattern(
        [System.Windows.Automation.SelectionItemPattern]::Pattern,
        [ref]$pattern
    )) {
        throw "Radio '$($Element.Current.Name)' does not support SelectionItemPattern"
    }
    $pattern.Select()
}

function Get-ToggleState($Element) {
    $pattern = $null
    if (-not $Element.TryGetCurrentPattern(
        [System.Windows.Automation.TogglePattern]::Pattern,
        [ref]$pattern
    )) {
        throw "Checkbox '$($Element.Current.Name)' does not support TogglePattern"
    }
    return $pattern.Current.ToggleState.ToString()
}

function Capture-Window($Root, [string]$Path) {
    $rect = $Root.Current.BoundingRectangle
    $x = [int][Math]::Floor($rect.X)
    $y = [int][Math]::Floor($rect.Y)
    $width = [int][Math]::Ceiling($rect.Width)
    $height = [int][Math]::Ceiling($rect.Height)
    Assert-True ($width -gt 100 -and $height -gt 100) "Setup window bounds are invalid"
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

function Click-Button($Root, [string]$NamePattern) {
    $button = Find-Element `
        $Root `
        ([System.Windows.Automation.ControlType]::Button) `
        $NamePattern
    Assert-True ($null -ne $button) "Button '$NamePattern' was not found"
    Assert-True $button.Current.IsEnabled "Button '$($button.Current.Name)' is disabled"
    Invoke-Element $button
}

New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$resultPath = Join-Path $OutputDirectory "gui-installer-result.json"
$launcher = $null
$setup = $null
$setupRoot = $null

try {
    Assert-True (Test-Path -LiteralPath $SetupPath -PathType Leaf) "Setup executable is missing"
    $launchedAfter = Get-Date
    $launcher = Start-Process `
        -FilePath $SetupPath `
        -ArgumentList @("/LANG=english") `
        -PassThru
    Wait-Condition "Setup wizard window" {
        $candidate = Find-SetupWindow $launchedAfter
        if ($null -ne $candidate) {
            $script:setupRoot = $candidate
            return $true
        }
        return $false
    }
    $root = $setupRoot
    $setup = Get-Process -Id $root.Current.ProcessId -ErrorAction Stop
    Capture-Window $root (Join-Path $OutputDirectory "installer-welcome.png")
    Click-Button $root "*Next*"

    Wait-Condition "license acceptance radio" {
        $root = Get-SetupRoot $setup
        $null -ne (Find-Element $root ([System.Windows.Automation.ControlType]::RadioButton) "I accept*")
    }
    $root = Get-SetupRoot $setup
    $accept = Find-Element `
        $root `
        ([System.Windows.Automation.ControlType]::RadioButton) `
        "I accept*"
    Select-Radio $accept
    Capture-Window $root (Join-Path $OutputDirectory "installer-license.png")
    Click-Button $root "*Next*"

    $startup = $null
    for ($page = 0; $page -lt 4 -and $null -eq $startup; $page++) {
        Start-Sleep -Milliseconds 300
        $root = Get-SetupRoot $setup
        $startup = Find-ElementByName $root "Start the WinSched tray*"
        if ($null -eq $startup) {
            Click-Button $root "*Next*"
        }
    }
    Assert-True ($null -ne $startup) "Select Additional Tasks page was not found"
    $root = Get-SetupRoot $setup
    $desktop = Find-ElementByName $root "Create a desktop shortcut*"
    Assert-True ($null -ne $desktop) "Desktop shortcut task was not found"
    Capture-Window $root (Join-Path $OutputDirectory "installer-tasks.png")
    Click-Button $root "*Next*"

    Wait-Condition "Install button" {
        $root = Get-SetupRoot $setup
        $null -ne (Find-Element $root ([System.Windows.Automation.ControlType]::Button) "*Install*")
    }
    $root = Get-SetupRoot $setup
    Capture-Window $root (Join-Path $OutputDirectory "installer-ready.png")
    Click-Button $root "*Install*"

    Wait-Condition "Finish button" {
        $root = Get-SetupRoot $setup
        $finish = Find-Element $root ([System.Windows.Automation.ControlType]::Button) "*Finish*"
        $null -ne $finish -and $finish.Current.IsEnabled
    } 90
    $root = Get-SetupRoot $setup
    $launch = Find-ElementByName $root "Launch WinSched*"
    Assert-True ($null -ne $launch) "Finish-page Launch WinSched checkbox was not found"
    Capture-Window $root (Join-Path $OutputDirectory "installer-finish.png")
    Click-Button $root "*Finish*"
    Assert-True ($setup.WaitForExit(15000)) "Setup process did not exit after Finish"

    Wait-Condition "WinSched service running" {
        $service = Get-Service -Name "WinSched" -ErrorAction SilentlyContinue
        $service -and $service.Status -eq "Running"
    }
    $service = Get-CimInstance Win32_Service -Filter "Name='WinSched'"
    $config = Get-Content "C:\ProgramData\WinSched\winsched.toml" -Raw
    $settingsPath = "C:\Program Files\WinSched\winsched-settings.exe"
    $settingsShortcut = "C:\ProgramData\Microsoft\Windows\Start Menu\Programs\WinSched\WinSched Settings.lnk"
    Assert-True ($service.PathName -match "Program Files\\WinSched\\winsched-service.exe") `
        "Service does not run from Program Files"
    if ($ExpectedConfigMarker) {
        Assert-True ($config.Contains($ExpectedConfigMarker)) `
            "GUI install overwrote the preserved configuration marker"
    }
    Assert-True (Test-Path -LiteralPath $settingsPath -PathType Leaf) `
        "Settings application is missing"
    Assert-True (Test-Path -LiteralPath $settingsShortcut -PathType Leaf) `
        "Settings Start Menu shortcut is missing"
    Assert-True (Test-Path "C:\ProgramData\Microsoft\Windows\Start Menu\Programs\Startup\WinSched Tray.lnk") `
        "Startup shortcut is missing"
    Assert-True (-not (Test-Path "C:\Users\Public\Desktop\WinSched.lnk")) `
        "Desktop shortcut was created even though its task is off by default"

    [pscustomobject]@{
        result = "PASS"
        setup_sha256 = (Get-FileHash -LiteralPath $SetupPath -Algorithm SHA256).Hash.ToLowerInvariant()
        setup_exit_code = $setup.ExitCode
        service_state = $service.State
        service_path = $service.PathName
        config_marker_preserved = [bool]$ExpectedConfigMarker
        startup_shortcut = $true
        desktop_shortcut = $false
        settings_shortcut = $true
        launch_option_present = $true
        installed_sha256 = [ordered]@{
            winsched = (Get-FileHash "C:\Program Files\WinSched\winsched.exe" -Algorithm SHA256).Hash.ToLowerInvariant()
            service = (Get-FileHash "C:\Program Files\WinSched\winsched-service.exe" -Algorithm SHA256).Hash.ToLowerInvariant()
            tray = (Get-FileHash "C:\Program Files\WinSched\winsched-tray.exe" -Algorithm SHA256).Hash.ToLowerInvariant()
            settings = (Get-FileHash $settingsPath -Algorithm SHA256).Hash.ToLowerInvariant()
            readme = (Get-FileHash "C:\Program Files\WinSched\README.md" -Algorithm SHA256).Hash.ToLowerInvariant()
        }
        screenshots = @(
            "installer-welcome.png",
            "installer-license.png",
            "installer-tasks.png",
            "installer-ready.png",
            "installer-finish.png"
        )
    } | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $resultPath -Encoding UTF8
} catch {
    if ($setup) {
        try {
            $root = Get-SetupRoot $setup
            if ($root) {
                Capture-Window $root (Join-Path $OutputDirectory "installer-error.png")
            }
        } catch {
        }
    }
    [pscustomobject]@{
        result = "FAIL"
        error = $_.Exception.ToString()
        script_stack = $_.ScriptStackTrace
    } | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $resultPath -Encoding UTF8
    if ($setup -and $setup.Id -ne $PID) {
        Stop-Process -Id $setup.Id -Force -ErrorAction SilentlyContinue
    }
    if ($launcher -and $launcher.Id -ne $PID) {
        Stop-Process -Id $launcher.Id -Force -ErrorAction SilentlyContinue
    }
    exit 1
}
