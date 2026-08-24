# WinSched 0.2.0 logging settings acceptance

Date: 2026-08-24

Test host: Windows 11 x64, `Microsoft Windows NT 10.0.26340.0`

Source branch: `feature/logging-settings`

## Result

The requested logging feature passed its release-candidate acceptance gates.
The Windows VM was restored after testing with the original schema-1
configuration hash, the service Running, scheduling enabled, and the tray
running in interactive session 1.

## Build and static verification

- `cargo fmt --all -- --check`: PASS
- `cargo test --workspace --all-targets`: PASS, 69 tests
- `cargo clippy --workspace --all-targets -- -D warnings`: PASS
- Windows MSVC cross-Clippy with warnings denied: PASS
- Windows MSVC release build: PASS
- RustSec audit: PASS, 383 locked dependencies scanned
- Windows PowerShell 5.1 parser for all acceptance scripts: PASS
- Windows-native `winsched-service` test executable: PASS, 24 tests

## Windows acceptance

| Gate | Result | Evidence |
| --- | --- | --- |
| GUI installer build | PASS | Inno Setup 7.1.0, frozen payload hashes verified |
| GUI upgrade | PASS | Existing configuration preserved byte-for-byte; all four installed EXE hashes matched the payload |
| Schema-1 compatibility | PASS | File remained schema 1 without `[logging]`; service status schema 2 applied `true / 10 MiB / 1` |
| Disabled startup | PASS | No `winsched.log*` file was created |
| Hot enable/disable | PASS | Same service PID; disabled files remained byte-stable |
| Circular rotation | PASS | 1 MiB limit, `.1/.2` newest-first ordering, no `.3`, complete JSONL records |
| Zero archives | PASS | Oversized active history was recycled without creating `.1` |
| Settings UI | PASS | EN/RU Logging page, disabled controls, retained values, Apply on/off, defaults, reload receipt, and exact cleanup |
| Tray schema-2 compatibility | PARTIAL | New tray displayed the complete live menu and status and successfully issued Disable/Stop/Start; the pre-existing global UI Automation harness stalled before writing its final result |

The tray harness limitation did not affect the logging or Settings acceptance
results. The VM screenshot confirmed the tray parsed the new status contract,
and the platform-independent tray model tests passed.

## Artifacts

- `dist/gui-installer/WinSched-0.2.0-Setup-x64.exe`
  - bytes: `6719727`
  - SHA-256: `7717f707979e196e33d161b3ec5cae408faeb537b5867a49fc034af931f542ef`
  - Authenticode: `NotSigned`
- `dist/WinSched-0.2.0-windows-x64.zip`
  - SHA-256: `287832319bbad961505be3efe28c7cf354b9e20afb81017c0fca1fbc3b8091ba`

## Restored VM state

- Service: `Running`
- Scheduling: `enabled`
- Phase: `running`
- Tray: running in session 1
- Configuration SHA-256:
  `e28fd73f1e4909f7cc2556398f8cd40b21dc87ef23f539b923f03ae9f8135875`
