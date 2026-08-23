# WinSched 0.1.0 release acceptance (superseded)

This earlier ZIP-focused snapshot is superseded by
`2026-08-23-gui-release-acceptance.md`, which is bound to the final GUI Setup,
Settings application, and exact current hashes.

Date: 2026-08-23

This report records acceptance of the local Windows x64 release candidate. It
contains no test-machine address, account, or credential material.

## Frozen artifact

- Archive: `dist/WinSched-0.1.0-windows-x64.zip`
- Archive SHA-256: `a7e7b424122b61bb730c0ad7073f5e793446b318f3ac3a7ac4d55167670a0883`
- `winsched.exe`: `c3723836146c30a612f17f505f1a58b1d194e6a26b4bc0cc2ff262e6f45608f2`
- `winsched-service.exe`: `47234c2d10c30aba929de3d41aa14b59b3ff6d4293d0daf5b59aa4b439d325fb`
- `winsched-tray.exe`: `f3a6b1f52f6cab218326e1b286260409fd40360f69626bab83a6c121bcef2967`
- The installed executable hashes matched the staged executable hashes.
- The portable outer checksum and all entries in packaged `SHA256SUMS` passed.

## Build and static gates

The frozen package was produced by `scripts/build-release.sh`, which passed:

- `cargo fmt --all -- --check`
- 42 native Rust tests
- native Clippy with `-D warnings`
- RustSec `cargo audit` with no vulnerability finding
- Windows MSVC cross-target Clippy with `-D warnings`
- Windows x64 release build
- PE import rejection for `TaskDialogIndirect`
- multi-size ICO generation and validation (16, 20, 24, 32, 48, 64, 128, and 256 px)

A fresh project-scoped `mcpls 0.3.9` process resolved Rust document symbols in
the service and tray modules. Rust-analyzer reported only the expected inactive
`cfg(windows)` hints on the Linux host; Windows Clippy/build remained the
authoritative type-check for those modules.

## Windows-native tests

The final Windows test executables ran on Windows 11 and passed 19/19:

- Win32 platform and fixed infrastructure exclusions: 2/2
- CLI parsing: 3/3
- service state, cleanup fault injection, activity scope, ownership, and control: 9/9
- tray presentation model: 4/4
- cross-process instance-lock primitive: 1/1

## Frozen ZIP end-to-end acceptance

The combined `tests/windows/final-acceptance.ps1` run completed with exit code 0
on Windows 11 build 26340. Its three constituent matrices all returned `PASS`:

1. Service and adaptive placement
   - Automatic LocalSystem service and restricted interactive SCM ACL
   - Session 0, SSH service, Windows shell, and service-host exclusions
   - activity-gated automatic scope
   - real CPU Set assignment and controlled PDH-to-policy-to-Win32 LLC move
   - Enable/Disable persistence, graceful cleanup, invalid-config fail-close
   - forced-process termination, SCM restart actions, and ownership recovery
2. Installer lifecycle
   - implicit upgrade preserved a custom threshold of 777 basis points
   - normal uninstall removed service/binaries/shortcuts and preserved data
   - purge uninstall removed the data directory
   - default interactive install launched the tray at medium integrity (RID 8192)
   - the all-users Startup shortcut launched the tray at medium integrity
3. Tray UI automation
   - one tray instance and embedded CPU icon
   - Enable/Disable Scheduling and Start/Stop Service
   - Mode, managed-process count, last activity, and last error
   - Open Configuration, Open Logs, Refresh Status, and Exit Tray
   - tray restart after Exit and no obsolete loader-error window

The final run left the service Running and scheduling enabled, with one tray
process in the interactive console session and no acceptance scheduled tasks.
The final status heartbeat had no error.

## External release limitation

The binaries are unsigned. Windows SmartScreen may warn until an external
Authenticode certificate is obtained. Code signing was not available in this
workspace and does not affect the verified runtime behavior above.
