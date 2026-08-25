# WinSched 0.4.0 diagnostics and quiet-I/O acceptance

Date: 2026-08-25

Source branch: `feature/diagnostics-quiet-io`

## Scope

This acceptance covers passive CLI/GUI diagnostics, privacy boundaries,
taskbar probing, status heartbeat cadence, responsiveness-log coalescing,
immediate reload receipts, fixed shell/WSL CPU Set exclusions, regression UI,
frozen artifacts, and Setup upgrade behavior.

## Environments

### Physical target

- Windows 11 Pro for Workstations Experimental/Insider 26H2 build 26300.9032
- AMD Ryzen Threadripper 3970X, 32 physical cores / 64 logical processors
- 256 GiB RAM
- WSL and VMware active
- installed WinSched 0.3.1 service used only as read-only live status context

### Windows VM

- Windows 11 x64
- 2 vCPU / 4 GiB RAM
- interactive Session 1 plus elevated SSH administration
- frozen WinSched 0.3.1 installation upgraded to 0.4.0
- Inno Setup 7.1.0 x64, downloaded from the immutable upstream release

## Source and artifact gates

| Gate | Result | Detail |
|---|---:|---|
| Formatting | PASS | `cargo fmt --all -- --check` |
| Native tests | PASS | 100 tests, 0 failures |
| Native Clippy | PASS | workspace/all targets, `-D warnings` |
| Windows Clippy | PASS | xwin MSVC workspace/all targets, `-D warnings` |
| RustSec | PASS | 383 dependencies, no vulnerability finding |
| Windows release build | PASS | four optimized x64 executables |
| Tray compatibility import | PASS | no `TaskDialogIndirect` import |
| ZIP integrity | PASS | archive test and internal `SHA256SUMS` |
| PowerShell parser | PASS | 23 files, 0 Windows PowerShell 5.1 errors |
| Inno compiler provenance | PASS | GitHub attestation, SHA-256, valid Pyrsys B.V. Authenticode |

## Physical diagnostic acceptance

The final frozen `winsched.exe` hash was
`f8f5ca6d7992014631a7341b8418176ae234244a6c507584ec06979eff6ab3bd`.
It ran for 10 seconds without installation or input generation.

| Field | Result |
|---|---:|
| Exit code / stderr | 0 / empty |
| Samples | 40 |
| Average CPU | 7.61% |
| Maximum processor queue | 1 |
| Scheduler wake p99 | normal |
| Taskbar available | true |
| Taskbar p95 / timeouts | 189 us / 0 |
| Finding codes | `healthy` |
| WSL advice | inactive; no automatic change |
| User path/title leakage | none |

An earlier candidate treated queue length 2 as saturation on 64 logical
processors. Physical acceptance caught the false positive. The classifier now
uses `max(2, ceil(logical_processors / 4))`; a dedicated regression test covers
the 64-LP/queue-2 case.

## VM diagnostic and GUI acceptance

- Session 0 CLI correctly reported `taskbar.available=false` and still emitted
  scheduler/DPC findings from supported data.
- Session 1 Diagnostics GUI returned 40 samples with taskbar available and no
  timeout.
- Saved schema-1 JSON parsed successfully, contained no user paths or window
  titles, reported no automatic WSL change, and was removed by the harness.
- CLI `--output` and stdout carried the same capture/schema; the explicit test
  file was privacy-safe and removed immediately after verification.
- The configuration SHA-256 did not change.
- The pre-existing Settings suite passed EN/RU pages, tooltips, single-instance
  behavior, logging toggles, Apply receipts, defaults, and exact restoration of
  the original TOML hash.

## Service write cadence

A 75-second live-service observation produced eight distinct `status.json`
writes. After the initial partial interval, observed intervals were:

```text
10045, 10073, 10052, 10067, 10074, 10059 ms
```

Two new `responsiveness_sample` events were observed because the 75-second
window crossed two periodic boundaries; both carried `reason=periodic`.
Service state remained Running and configuration hash remained unchanged.

A byte-identical TOML rewrite produced an authoritative reload receipt in
128 ms. Sequence advanced from 1 to 2, result was `reloaded`, and the
configuration SHA-256 remained unchanged.

## Installer and upgrade acceptance

- Final Setup SHA-256:
  `b364eed34d0ea3db54d9aa1e192a53acfcff6b1d4f5d59e8ea15a5fdab89a14d`
- Frozen ZIP SHA-256:
  `ad893a1c512ea6e3c9752eb166c41a250b44f2c8e1b66371ac28f581f5cd754b`
- All four frozen 0.3.1 executables were restored before the final upgrade.
- Setup returned 0, preserved the marker-bearing configuration byte-for-byte,
  installed four files matching the final frozen payload, retained Startup and
  Settings shortcuts, and returned the service to Running under LocalSystem.
- The original pre-test TOML bytes were restored afterwards and every new test
  marker was absent.
- The final Setup wizard passed Welcome, License, Tasks, Ready, Install, and
  Finish UI Automation. Startup remained enabled; the optional desktop shortcut
  remained disabled.
- Tray acceptance passed live reserve/latency rows, About version 0.4.0, the
  repository URL, and the invokable GitHub action.

## Safety conclusions

- Diagnostics generated no pointer, keyboard, focus, configuration, WSL, or
  CPU Set mutation.
- Explorer/DWM and fixed shell/service hosts remain excluded from automatic
  control.
- New CPU Set assignments to fixed WSL VM hosts are denied by the platform
  mutation boundary; cleanup remains possible.
- `SeparateProcess=0` is not recommended. A physical-host user A/B test made
  Explorer tab opening slower, so the diagnostic treats Explorer fan-out only
  as context and never changes this Windows setting.

## Boundaries

- The 2-vCPU VM validates behavior and lifecycle, not Threadripper performance.
- The physical passive diagnostic validates supported signals at the sampled
  time; it does not identify a private Win32k implementation detail.
- Existing 0.3.x Threadripper topology, rollback, and synthetic contention
  performance evidence remains the scheduling-performance baseline.
- The release remains unsigned.
