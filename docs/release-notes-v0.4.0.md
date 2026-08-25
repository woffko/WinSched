# WinSched 0.4.0

WinSched 0.4.0 adds bounded passive Windows responsiveness diagnostics and
reduces routine service filesystem activity. It is based on a physical-host
investigation where taskbar and foreground-window message processing stalled
despite substantial spare CPU capacity.

## Passive diagnostics

- `winsched diagnose` runs a bounded read-only collection in the current user
  session. `--json` emits stable schema 1 JSON and `--output` optionally writes
  the same report to an explicit path.
- The collector measures CPU/LLC utility, processor queue length,
  DPC/interrupt pressure, pages input, available physical memory,
  normal-priority wake latency, Explorer process/window fan-out, active WSL and
  VMware processes, and live WinSched service context.
- The taskbar probe sends only `WM_NULL` to `Shell_TrayWnd` with
  `SMTO_ABORTIFHUNG` and a 50 ms timeout. It never generates input, changes
  focus, or runs from the LocalSystem service in Session 0.
- Privacy-safe JSON excludes window titles, user names, executable paths, and
  raw `.wslconfig` contents.
- Stable finding codes distinguish CPU saturation, scheduler latency,
  DPC/interrupt pressure, memory pressure, Explorer fan-out, and shell latency
  with spare CPU. Explorer fan-out is context only; WinSched never changes the
  Windows `SeparateProcess` setting.

## Diagnostics GUI

- Settings has a new English/Russian Diagnostics page.
- Collection runs on a background thread, supports cooperative cancellation,
  and leaves the Settings event loop responsive.
- Results include localized measurements and recommendations. JSON can be
  copied or saved to Downloads only after an explicit user action.
- The GUI and CLI share the same collector and classifier.

## Quieter service I/O

- `status.json` remains an atomic status and reload-receipt contract.
- Startup, stop, Enable/Disable, configuration reload, error transitions, and
  responsiveness-width changes publish immediately.
- Unchanged steady state uses a 10-second heartbeat instead of rewriting the
  file every one-second policy sample.
- Steady `responsiveness_sample` logging is coalesced to a 60-second periodic
  summary. Initial state, stable pressure transitions, and width changes remain
  immediate.

## System and WSL safety

- Explorer, DWM, shell hosts, service hosts, and WSL VM hosts remain outside
  automatic placement policy.
- The lower Windows mutation boundary now denies new CPU Set assignments for
  fixed shell/system targets and `vmmem`, `vmmemWSL`, `wslhost.exe`, and
  `wslservice.exe`. Clearing an existing assignment remains available for
  recovery.
- `.wslconfig` analysis recognizes only memory, processor, swap, crash-dump,
  and automatic-memory-reclaim values. Advice is pressure-gated and read-only;
  WinSched never edits the file or runs `wsl --shutdown`.

## Validation summary

| Gate | Result | Evidence |
|---|---:|---|
| Rust workspace tests | 100 PASS | Native unit and all-target tests |
| Native and Windows Clippy | PASS | `-D warnings` |
| RustSec audit | PASS | 383 dependencies, no vulnerability finding |
| Windows release build | PASS | Four optimized x64 MSVC executables |
| Physical-host CLI diagnostic | PASS | 40 samples, taskbar available, privacy-safe JSON |
| Diagnostics GUI | PASS | Session 1, 40 samples, save/copy controls, cleanup |
| Status heartbeat | PASS | 8 writes in 75 seconds; steady intervals 10.045–10.074 seconds |
| Responsiveness log cadence | PASS | Periodic reason at the 60-second cadence |
| Immediate reload receipt | PASS | 128 ms; configuration hash unchanged |
| Existing Settings regression | PASS | EN/RU, Apply, tooltips, defaults, single instance |
| Setup upgrade | PASS | 0.3.1 to 0.4.0; config byte-identical; four hashes matched |
| GUI installer wizard | PASS | Welcome, License, Tasks, Ready, Install, Finish |
| Tray regression | PASS | Version 0.4.0, live rows, About, GitHub action |
| PowerShell parser | PASS | 23 scripts, 0 errors on Windows PowerShell 5.1 |

The focused acceptance record is
`tests/evidence/2026-08-25-diagnostics-quiet-io-acceptance.md`.

## Upgrade behavior

- Setup upgrades 0.3.1 in place.
- Existing `C:\ProgramData\WinSched\winsched.toml` bytes and comments are
  preserved.
- Configuration schema 3 and status schema 3 remain compatible.
- The service returns to Running/Automatic/LocalSystem from Program Files.
- Startup task selection and Settings shortcuts are retained.

## Release artifacts

| Artifact | Bytes | SHA-256 |
|---|---:|---|
| `WinSched-0.4.0-Setup-x64.exe` | 6,887,165 | `b364eed34d0ea3db54d9aa1e192a53acfcff6b1d4f5d59e8ea15a5fdab89a14d` |
| `WinSched-0.4.0-windows-x64.zip` | 5,905,218 | `ad893a1c512ea6e3c9752eb166c41a250b44f2c8e1b66371ac28f581f5cd754b` |

Both checksum sidecars are published separately.

## Known limitations

- Diagnostics reports supported pressure signals; it does not claim to name a
  private internal Win32k lock or a specific third-party fault.
- A taskbar probe can only run in an interactive user session. Session 0
  correctly reports the taskbar as unavailable.
- WSL advice is not workload attribution and never applies changes.
- Artifacts are unsigned and may trigger Windows SmartScreen.
