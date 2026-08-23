# WinSched 0.1.0

WinSched is a supported-API-only Windows 11 CPU placement controller written in
Rust. It observes processor-group and LLC topology, samples load with
locale-independent PDH counters, and applies Windows CPU Sets only when a
validated policy predicts a stable improvement.

## Highlights

- Automatic LocalSystem service with crash recovery and fail-closed cleanup.
- LLC-aware adaptive placement, sticky, performance, efficiency, strict, and
  observe-only modes.
- Per-session notification-area application with a large CPU icon, live status,
  Enable/Disable Scheduling, Start/Stop Service, Settings, logs, and advanced
  configuration actions.
- Administrative Settings GUI in English and Russian with General, Adaptive,
  and Process rules pages.
- Atomic validated configuration writes and durable service reload feedback.
- Windows 11 x64 GUI installer with upgrade-safe service provisioning,
  byte-identical config preservation, tray autostart, optional desktop shortcut,
  and explicit preserve/purge uninstall behavior.
- Scripted ZIP package for advanced deployment and diagnostics.

## Install

Download `WinSched-0.1.0-Setup-x64.exe` and its `.sha256` file. Copy Setup to a
local Windows drive, verify the checksum, and run it normally. Setup requests
UAC when administrative changes are required.

Do not elevate Setup directly from a `\\wsl.localhost\...` path. Copy it to
Downloads or another local NTFS directory first; an elevated token may not
retain the WSL network provider.

## Verification

- Final Setup SHA-256:
  `a63dbad5bfe9bdd36cf03d613ef509f92a26d21f6081bf16bc3352300d9e1367`
- Final ZIP SHA-256:
  `92f26c39108c7e7bd08b4605a9b278d354c1b7d9b0759593a8e8a3ba84d74b87`
- 57 native Rust tests, native and Windows-target Clippy, Windows release build,
  and RustSec audit passed.
- Fresh install, self-upgrade, transactional SCM rollback, Settings GUI,
  adaptive LLC movement, medium-integrity tray/autostart, preserve uninstall,
  and purge uninstall passed on Windows 11 x64 build 26340.

Full acceptance evidence is documented in
`tests/evidence/2026-08-23-gui-release-acceptance.md`.

## Known distribution limitation

This release is not Authenticode-signed. Windows SmartScreen may display a
warning until a production signing certificate is configured. No signing key or
secret is included in the repository.
