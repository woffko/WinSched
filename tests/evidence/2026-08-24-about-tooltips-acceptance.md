# WinSched 0.3.1 About and Settings tooltip acceptance

Date: 2026-08-24
Target: Windows 11 x64 (`Microsoft Windows NT 10.0.26340.0`)
Release status: PASS

## Exact artifacts

- GUI installer: `WinSched-0.3.1-Setup-x64.exe`
  - bytes: `6814559`
  - SHA-256: `113aae92a162ea9989979e0559e77cd9caa34c9a2812fd26fc07d9cc1cd93f62`
  - Authenticode: unsigned development release
- Scripted ZIP: `WinSched-0.3.1-windows-x64.zip`
  - bytes: `5666163`
  - SHA-256: `e893dc4537b7448b1283663c4b9da00516e2a4b13f34e8eeaf80b8846d75c81c`
- Updated executables:
  - `winsched-tray.exe`: `54d94851eb3cd739e99bd7b2834504a85adbd0bc39c7fc90737769d9380f10e5`
  - `winsched-settings.exe`: `62441a383b04765aec10bb2640ed081092294b18ca2bf4996e7bda1b74157306`

## About and repository UI

The focused tray UI Automation acceptance ran in interactive session 1 and
verified:

- `About WinSched...` is present and opens a native About dialog.
- The dialog displays `Version: 0.3.1`.
- The dialog displays `https://github.com/woffko/WinSched`.
- `GitHub Repository` is present, enabled, and exposes an InvokePattern.
- The About dialog closes normally through its OK button.
- The tray remains non-elevated and continues to report live service state.

The build's PE import guard confirmed that `winsched-tray.exe` does not import
`TaskDialogIndirect`.

## Settings tooltips

Important Settings labels and controls now expose contextual English/Russian
hover help across General, Adaptive, Responsiveness, Process rules, and
Logging. The text explains units, scope, safety behavior, profile semantics,
strict placement, and tuning consequences.

A focused AccessKit/UI Automation smoke moved the real pointer over controls
and waited for rendered tooltip content on three pages. It verified:

- `Sample interval (milliseconds)`
- `Default workload profile`
- `Enable topology-aware system reserve`
- `Enable detailed service logging`

The smoke exited through Close and confirmed that no configuration value was
changed.

## Build and upgrade gates

- Native workspace tests: 86 PASS.
- Native workspace Clippy with `-D warnings`: PASS.
- Windows MSVC-target workspace Clippy with `-D warnings`: PASS.
- Windows x64 release build: PASS.
- RustSec audit: PASS for 383 dependencies.
- Windows PowerShell 5.1 parsing for the updated acceptance scripts: PASS.
- Inno Setup 7.1.0 compile: PASS.
- Exact Setup upgrade from 0.3.0: PASS.
- Existing configuration bytes remained identical during the upgrade.
- Installed service returned to Running/Automatic/LocalSystem from
  `C:\Program Files\WinSched`.
- Installed executable hashes matched the frozen 0.3.1 payload.

The VM was left with WinSched 0.3.1 installed, the service running, and the
tray running in the interactive session.
