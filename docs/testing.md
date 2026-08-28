# Testing and release validation

This document describes current WinSched 0.6.0 validation and retained earlier
release evidence. It separates source-level checks, Windows VM acceptance,
physical Threadripper acceptance, and artifact verification so that a passing
narrow test is not presented as proof of a broader behavior.

## 0.6.0 Process Monitor development validation

Version 0.6.0 adds an on-demand, non-elevated process window while preserving
the 0.5.1 scheduling policy. Process polling is a focused-window-only activity;
normal tray and service operation do not start the monitor or write monitoring
receipts.

| Area | Environment | Current result |
|---|---|---:|
| Native workspace tests and Clippy | WSL/Linux | PASS |
| Windows-target Clippy | WSL/xwin | PASS |
| PowerShell parser sweep | Windows PowerShell 5.1 | PASS, 37 scripts |
| Windows-native execution | Windows VM | PASS, 180 tests in 12 executables |
| Detailed Windows process snapshot | Windows VM | PASS, 72/72 RAM and QoS rows |
| Active/minimized/resumed polling | Windows VM | PASS, 2 → 2 → 3 snapshots |
| Monitor single-instance activation | Windows VM | PASS |
| Tray left click / right-click menu | Windows VM | PASS |
| Exact-rule handoff to one Settings instance | Windows VM | PASS |
| Configuration without Apply | Windows VM | PASS, byte-identical |
| Frozen ZIP and GUI Setup build | WSL + Windows VM | PASS, ZIP `b9392cf1...6802`; Setup `5d4c25bc...ef2a` |
| Installed 0.5.1 to 0.6.0 upgrade | Windows VM | PASS on Setup `5d4c25bc...ef2a`, byte-identical config and five payload hashes |
| Repeat-install ACL allowlist regression | Windows VM | PASS, Monitor accepted; injected provision failure exited 9 and rolled back |
| GUI Setup and lifecycle | Windows VM | PASS, nine stages and final state restoration |
| Physical Process Monitor smoke | Threadripper host | PASS on exact Setup, byte-identical config and operator-confirmed UI |

The first `67dea2da...bd7f` Setup candidate failed safely before uninstall or
purge because the application ACL allowlist omitted the newly installed
`winsched-monitor.exe`. Inno Setup returned its standard
Preparing-to-Install failure code 7, and the lifecycle harness restored the
original configuration, Scheduling state, and Running service. The corrected
frozen payload adds Monitor to both allowlists and adds a build-time check that
evaluates the actual packaged `secure-data.ps1` against every payload file.

The focused repeat-install test on the accepted `5d4c25bc...ef2a` Setup then
returned the intended custom exit code 9 for the injected invalid configuration,
published the ERROR receipt, and restored the prior configuration, service
binary, and SCM fields. The complete lifecycle subsequently passed provision
rollback, failure receipt, silent preserve, silent purge, clean GUI install,
GUI preserve uninstall, preserved-data reinstall, GUI purge uninstall, and
final silent install. The original configuration SHA-256
`20d98408bef9cf98a4efa5455d786f997e8e47053b5fca2d209a984f8a9fb813`,
Scheduling state, and Running service were restored with no cleanup errors.

The accepted Setup was also tested independently as an in-place upgrade over
the accepted 0.5.1 Setup `4c92f9ef...60d3`. The test preserved a marked schema-5
configuration byte-for-byte, Scheduling, configured mode, and logging level;
installed Monitor and its shortcut; matched all five installed executable
hashes to the frozen payload; and restored the original 0.6.0 VM state after
the comparison.

Detailed evidence and example receipts are in
[`tests/evidence/2026-08-28-v0.6.0-installer-lifecycle.md`](../tests/evidence/2026-08-28-v0.6.0-installer-lifecycle.md)
and `tests/evidence/runtime/v0.6.0/`.

The same exact Setup passed the final physical-host gate. The 0.5.1 TOML hash
was unchanged after upgrade, all five installed executable hashes matched the
frozen payload, SCM remained Running/Automatic/LocalSystem, and the operator
confirmed the left-click, single-instance, right-click menu, required-column,
rule-draft-without-Apply, and minimize/restore interactions. No automated input
was used. See
[`tests/evidence/2026-08-28-v0.6.0-physical-smoke.md`](../tests/evidence/2026-08-28-v0.6.0-physical-smoke.md).

## 0.5.1 development validation

Version 0.5.1 is not yet released. Its first phase addresses the measured
0.5.0 decision-log write amplification and adds anonymous self-observability;
placement scope changes remain gated on a physical-host ABBA result.

| Area | Environment | Current result |
|---|---|---:|
| Schema-5 Off/Normal/Trace migration | Native Rust tests | PASS |
| Normal decision aggregation | Native Rust tests | PASS |
| Status-schema-5 telemetry contract | Native Rust tests | PASS |
| Current-process resource probe | Windows-target compile | PASS |
| Workspace unit tests | WSL/Linux | PASS, 123 tests |
| Native and Windows-target Clippy | WSL/xwin | PASS, `-D warnings` |
| PowerShell acceptance syntax | Windows PowerShell 5.1 | PASS, 31 scripts |
| Physical logging-off baseline | Threadripper host | PASS |
| Windows-native execution | Windows VM | PASS, 175 tests in 10 executables |
| Disabled idle after zero-wait fix | Windows VM | PASS, 0% one core, 0.10 writes/s, 3 heartbeats / 30 seconds |
| Normal/Trace/Off hot reload and rotation | Windows VM | PASS |
| 0.5.0 to 0.5.1 Setup upgrade | Windows VM | PASS on current Setup hash, byte-identical schema-4 fixture |
| Current GUI Setup build | Inno Setup 7.1.0 on Windows VM | PASS, SHA-256 `4c92f9ef2d4c10662bc86c042a0f9df09cc9553c3b94b738be6383325c760d3d` |
| 75-second quiet-I/O | Windows VM | PASS, 7 status writes; service/tray logs byte-stable |
| Tray, About, Settings, tooltip, and Diagnostics UI | Windows VM | PASS |
| Installer preserve/purge lifecycle | Windows VM | PASS on current Setup hash, nine stages with final state restoration |
| Physical 0.5.0 to 0.5.1 upgrade | Threadripper host | PASS, exact TOML and payload hashes |
| Physical corrected 0.5.1 reinstall | Threadripper host | PASS, current Setup/service hash and byte-identical TOML |
| Physical corrected Disabled idle | Threadripper host | PASS, 0.208% one core, 0.10 writes/s, 3 heartbeats / 30 seconds |
| Physical Logging Off efficiency | Threadripper host | PASS, 0 log records/bytes and byte-stable files |
| Physical Normal aggregation | Threadripper host | PASS, 9 records / 4.6 KiB in 75 seconds |
| Earlier manual-marker Auto/Disabled ABBA | Threadripper host | INVALID for policy |
| Passive Firefox marker pilot | Threadripper host | PASS, 3/3 taskbar clicks, confirmed minimize, 187 ms restore |
| Passive Firefox taskbar Auto/Disabled ABBA | Threadripper host | PASS, 40/40; `no_clear_effect` |

See [Efficiency and observability design](efficiency-observability-v0.5.1.md)
and `tests/evidence/2026-08-26-host-efficiency-baseline.md`.
Privacy-minimized structured VM receipts are under
`tests/evidence/runtime/v0.5.1/`.
The consolidated VM report is
`tests/evidence/2026-08-27-v0.5.1-vm-acceptance.md`.

## Schema-4 final validation

| Area | Environment | Current result |
|---|---|---:|
| Schema 1-3 migration and exact-rule scope | Native Rust tests | PASS |
| Workspace unit tests | WSL/Linux | PASS, 104 tests |
| Windows-target Clippy | WSL/cargo-xwin | PASS, `-D warnings` |
| Windows-native matrix | Designated VM | PASS, 152 tests |
| EcoQoS + memory-priority apply/readback/restore | Owned child processes | PASS |
| Per-property external override preservation | Native Windows and runtime VM | PASS |
| Authenticated local named pipe | Windows-native positive and negative tests | PASS |
| Event-driven wake, stale restore, and fallback safety cadence | Designated VM | PASS |
| Low/high memory APIs, transient retention, and WASAPI enumeration | Windows-native tests | PASS |
| Service policy apply then minimized/visible cohort restore | Windows-native child-process test | PASS |
| PowerShell syntax | Windows PowerShell 5.1 parser | PASS |
| Parent-to-later-child process-policy characterization | Interactive `cmd.exe` -> `ping.exe` | PASS: memory priority propagated, EcoQoS did not; parent rollback did not change the live child |
| Windows VM service/tray/GUI/installer/lifecycle acceptance | Designated VM | PASS |
| 75-second service plus tray quiet-I/O gate | Designated VM | PASS: 7 status writes; logs byte-stable |
| Physical Threadripper performance comparison | Physical host | NOT REPEATED for this feature |

The final Windows-native console suites passed 152 substantive tests across the
platform, CLI, configuration, control, core, service, Settings, and tray layers.
Zero-test GUI entry binaries were excluded from that count. The same frozen
payload then passed installed service, crash-recovery, interactive UI, upgrade,
uninstall, ACL, rollback, logging, and quiet-I/O acceptance.

The intended timing contract is a 250 ms interactive tray sample, an
event-driven controller wake after each authenticated pipe receipt, and a
one-second fallback safety cadence while an exact Background rule or owned
record exists. It is not an instantaneous or hard real-time guarantee. The
named-pipe worker uses cancellable overlapped event waits rather than periodic
polling.

Ownership and rollback are masked independently for EcoQoS and memory priority:
an external override relinquishes only the changed property. The Background
master switch and both mutations remain off in the packaged defaults. The VM
observed memory-priority inheritance, no EcoQoS inheritance in the tested
`cmd.exe` to `ping.exe` path, and no retroactive child restore when the parent
was restored. Process-level memory-priority restoration also does not retag
pages already populated under a different priority. These are explicit design
constraints, not broader rollback claims.

See [Background efficiency architecture](background-efficiency-design.md) for
the trust boundary, write-ahead journal, and per-property rollback contract.
The final evidence is
`tests/evidence/2026-08-26-v0.5.0-final-acceptance.md`.

## Test environments

### Windows installer and lifecycle VM

- Supported product baseline: 64-bit Windows 11 22H2, build 22621 or newer
- Windows 11 x64 (`Microsoft Windows NT 10.0.26340.0`)
- two logical processors and two visible LLC domains
- interactive user session for tray and Settings UI Automation
- elevated session for installer, service, lifecycle, and rollback tests
- Inno Setup 7.1.0 for the graphical installer

This VM validates compatibility, installation, service control, UI behavior,
configuration preservation, and rollback. Its small virtual topology is not
used to claim Threadripper performance.

### Physical Threadripper target

- AMD Ryzen Threadripper 3970X
- 32 physical cores and 64 logical processors
- one Windows processor group and one NUMA node
- eight LLC/CCX domains
- four physical cores and eight CPU Sets per LLC domain

This host validates the physical-core reserve, SMT sibling grouping,
multi-LLC workload partitions, exact CPU Set rollback, and the representative
memory-contention performance gate.

## Consolidated result matrix

| Area | Test and scope | Environment | Result | Key evidence |
|---|---|---|---|---|
| Formatting | `cargo fmt --all -- --check` | WSL/Linux | PASS | No formatting diff |
| Rust tests | Workspace unit and all-target tests | WSL/Linux | PASS | 104 tests, 0 failures |
| Native lint | Workspace Clippy with `-D warnings` | WSL/Linux | PASS | No warnings |
| Windows lint | MSVC-target workspace Clippy with `-D warnings` | WSL/xwin | PASS | No warnings |
| Dependency audit | `cargo audit` | WSL/Linux | PASS | 383 dependencies, no vulnerability finding |
| Windows build | Optimized x64 MSVC workspace build | WSL/xwin | PASS | Four release executables produced |
| Compatibility import | Tray PE import scan | WSL/Linux | PASS | `TaskDialogIndirect` absent |
| PowerShell syntax | Installer and Windows acceptance scripts | Windows PowerShell 5.1 | PASS | Full final parser sweep, 0 errors |
| Frozen payload | `SHA256SUMS` verification | WSL/Linux and Windows VM | PASS | Every staged file matched |
| Inno Setup | Frozen payload verification and Setup compile | Windows VM | PASS | Inno Setup 7.1.0 |
| Frozen Setup artifact | Eleven-stage exact-SHA lifecycle/runtime suite | Windows VM | PASS | Setup `5971a182...d553`; ZIP `bc9703f3...7a17` |
| Windows-native tests | Ten substantive test executables | Windows VM | PASS | 152 tests, 0 failures |
| In-place upgrade | Final Setup 0.5.0 over all four frozen 0.4.0 EXEs | Windows VM | PASS | Schema-3 configuration byte-identical; installed EXEs matched payload |
| Service state | SCM registration after upgrade | Windows VM | PASS | Running, Automatic, LocalSystem, Program Files path |
| Controller runtime | Session 0 exclusion, profiles, Background veto/restore, adaptive move, disable, invalid config, two crash recoveries | Windows VM | PASS | Exact CPU Set and per-property QoS ownership cleanup |
| Pipe authentication | Installed tray identity plus mismatched-image negative test | Windows VM | PASS | Invalid client published no state or wake |
| Installer rollback | Fault after SCM provisioning | Windows VM | PASS | SCM fields, state, SDDL, failure actions, config and binary hashes restored |
| Provision receipt failure | Invalid configuration through exact final Setup | Windows VM | PASS | `ERROR` receipt, Setup exit 9, prior service and config restored |
| Lifecycle | GUI and silent preserve/purge uninstall, final clean install | Windows VM | PASS | Data semantics, marker cleanup, shortcuts and Startup integrity verified |
| Circular logging | Enable/disable, size limit, retention, rotation, recovery | Windows VM | PASS | Transactional reconfiguration and complete records |
| Passive CLI diagnostic | Session 0 and interactive Session 1; privacy-safe schema 1 | Physical host and Windows VM | PASS | Bounded read-only collection |
| Diagnostics GUI | Background worker, cancellation contract, localized results, save/cleanup | Windows VM | PASS | 40 samples; taskbar available; config unchanged |
| Status/log cadence | 10-second heartbeat, change writes, 60-second responsiveness summary | Windows VM | PASS | 7 writes/75 s; disabled service/tray logs byte-stable |
| Retained 0.4 diagnostic baseline | Byte-identical TOML rewrite bypasses heartbeat delay | Windows VM | PASS | Receipt in 128 ms; current schema-4 Settings reload separately passed |
| Settings GUI | Six EN/RU configuration pages, Diagnostics, atomic Apply, service receipt, defaults, single instance | Windows VM | PASS | 27 controls/actions and 15 screenshots through AccessKit/UI Automation |
| Settings tooltips | Real pointer hover on important controls | Windows VM | PASS | Five rendered tooltips; config unchanged |
| Tray About | About dialog version and GitHub URL | Windows VM | PASS | Version 0.5.0 and repository URL visible |
| GitHub action | Repository tray item | Windows VM | PASS | Enabled and exposes InvokePattern |
| Tray status/control | Service/scheduling actions, reserve, latency, mode, Background status | Windows VM | PASS | Live schema-4 rows and absolute System32 Notepad actions |
| Physical topology | Reserve, Memory, and Compute plans | Threadripper 3970X | PASS | 4 reserved cores; 28 Memory and 56 Compute CPU Sets |
| Physical rollback | Apply and clear exact partitions | Threadripper 3970X | PASS | No CPU Set remained after controller exit |
| Performance gate | Six-phase 48-worker, 1-GiB memory contention A/B | Threadripper 3970X | PASS | p99 improved 83.27%; throughput increased 15.10% |

The physical Threadripper rows are the retained scheduling baseline from the
earlier release and were not repeated for Background Efficiency. Version 0.5.0
received a separate final source/native and installed-VM matrix rather than
inheriting acceptance from that baseline.

The detailed acceptance records are:

- `tests/evidence/2026-08-24-threadripper-responsiveness-acceptance.md`
- `tests/evidence/runtime/threadripper-performance-result.json`
- `tests/evidence/2026-08-24-logging-settings-acceptance.md`
- `tests/evidence/2026-08-24-about-tooltips-acceptance.md`
- `tests/evidence/2026-08-25-diagnostics-quiet-io-acceptance.md`
- `tests/evidence/2026-08-26-v0.5.0-final-acceptance.md`

## Representative performance result

The performance harness uses a separate normal-priority high-resolution timer
probe and a 48-worker random private-buffer read-modify-write workload. Both
Observe and Auto phases run the controller, and the probe uses the same eight
reserve CPU Sets in both modes. The only intended A/B difference is whether the
workload is unrestricted or assigned the exact 28-core Memory partition.

| Metric | Observe baseline | Auto + Memory profile | Change |
|---|---:|---:|---:|
| Median scheduler wake p99 | 5858.3 us | 980.3 us | 83.27% lower |
| Median useful throughput | 543.814493 Mops/s | 625.956536 Mops/s | 15.10% higher |
| Process CPU share | about 74.2% | about 43.4% | no full-CPU saturation dependency |
| Throughput run range | 1.10% | 5.15% | both below the 10% stability limit |

`Mops/s` is the harness's synthetic random-memory operation rate. It is not a
hardware-counter DRAM-bandwidth measurement. AMD uProf remains an optional
external calibration tool and is not a runtime dependency.

## Reproducible test examples

### Complete source and release pipeline

From WSL or Linux with Rust 1.95, `cargo-xwin`, LLVM `llvm-rc`, and `cargo-audit`:

```bash
./scripts/build-release.sh
```

The script runs formatting, tests, native and Windows Clippy, RustSec, the
Windows release build, and the tray PE import guard before creating the ZIP.

The individual commands are:

```bash
cargo fmt --all -- --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo audit
RC_PATH=/usr/lib/llvm-18/bin/llvm-rc cargo xwin clippy \
  --workspace --all-targets --target x86_64-pc-windows-msvc -- -D warnings
RC_PATH=/usr/lib/llvm-18/bin/llvm-rc cargo xwin build \
  --workspace --release --target x86_64-pc-windows-msvc
```

Run the cross-built native test executables from an elevated VM shell through
the interactive-session controller. It stops an existing WinSched service to
avoid named-pipe collisions, runs every selected executable serially as the
active elevated user, and restores the original service state in `finally`.
Limited-user tray and Settings behavior remains a separate UI acceptance gate:

```powershell
.\tests\windows\native-test-runner.ps1 `
  -TestDirectory C:\Users\Public\WinSchedNativeTests `
  -OutputDirectory C:\Users\Public\WinSchedNativeResults
```

This distinction is required: SSH service sessions are Session 0, and the
native safety tests intentionally reject process-policy targets outside an
interactive session.

### Read-only topology and plan preview

These commands do not change process placement:

```powershell
.\winsched.exe topology --json
.\winsched.exe responsiveness-plan C:\ProgramData\WinSched\winsched.toml --json
.\winsched.exe observe --samples 5 --interval-ms 1000 --json
```

On the validated 3970X, the default preview reports four reserved physical
cores, eight reserved CPU Sets, 28 Memory CPU Sets, and 56 Compute CPU Sets.

### Elevated Windows controller acceptance

Run from an elevated Windows PowerShell in an interactive test session:

```powershell
.\tests\windows\full-acceptance.ps1 `
  -PackageDirectory .\dist\WinSched-0.5.0-windows-x64 `
  -InteractiveUser $env:USERNAME
```

The script creates a real interactive CPU burner, verifies profile partitions
and reserve exclusions, injects an invalid configuration, forces one service
crash, confirms SCM recovery, and verifies CPU Set cleanup.

### Physical Threadripper performance acceptance

Run only on the intended 32-core/64-thread target from an elevated shell:

```powershell
.\tests\windows\threadripper-performance-acceptance.ps1 `
  -WinSched C:\WinSchedTest\winsched.exe `
  -Service C:\WinSchedTest\winsched-service.exe `
  -WorkDirectory C:\WinSchedTest\performance
```

The harness refuses a topology other than 32 physical cores, 64 logical
processors, and eight LLC domains. It restores the previously installed
service state in `finally`, including failure paths.

Example summary fields from the accepted run:

```json
{
  "result": "PASS",
  "medians": {
    "baseline_latency_p99_us": 5858.3,
    "managed_latency_p99_us": 980.3,
    "latency_improvement_percent": 83.26647662291109,
    "throughput_delta_percent": 15.104791074407805
  }
}
```

### Passive responsiveness diagnostic

The CLI command is bounded and does not generate input or mutate policy:

```powershell
.\winsched.exe diagnose --duration-seconds 10 --interval-ms 250 --json
```

The physical 32-core/64-LP acceptance produced 40 samples, 7.61% average CPU,
queue length 1, taskbar p95 189 us, no timeout, finding `healthy`, and no WSL
advice. The JSON contained no window title or user path. The dedicated
interactive GUI harness is:

```powershell
.\tests\windows\diagnostics-ui-acceptance.ps1 `
  -OutputDirectory C:\Users\Public\WinSchedDiagnosticEvidence
```

It launches Settings through an interactive scheduled task in unattended VM
acceptance, runs the background diagnostic, saves and parses JSON, verifies
privacy/configuration invariants, removes the saved report, and closes Settings.

### Focused Settings tooltip acceptance

Run in the same interactive session as the Settings window:

```powershell
.\tests\windows\settings-tooltips-smoke.ps1 `
  -OutputDirectory C:\Users\Public\WinSchedTooltipEvidence
```

The script moves the real mouse pointer, waits for tooltip content through UI
Automation, captures screenshots, closes Settings, and reports
`configuration_changed = false`.

### Focused tray About acceptance

With the tray already running in the interactive session:

```powershell
.\tests\windows\tray-responsiveness-smoke.ps1 `
  -OutputDirectory C:\Users\Public\WinSchedTrayEvidence `
  -ExpectedVersion 0.5.0
```

Example accepted result:

```json
{
  "result": "PASS",
  "about_menu_text": "About WinSched...",
  "github_menu_text": "GitHub Repository",
  "github_action_invokable": true,
  "about_version": "0.5.0",
  "about_github_url": "https://github.com/woffko/WinSched"
}
```

### Artifact checksum verification

On Windows:

```powershell
Get-FileHash .\WinSched-0.5.0-Setup-x64.exe -Algorithm SHA256
Get-FileHash .\WinSched-0.5.0-windows-x64.zip -Algorithm SHA256
```

On Linux or WSL:

```bash
(cd dist/WinSched-0.5.0-windows-x64 && sha256sum -c SHA256SUMS)
```

## Interpretation and boundaries

- A WSL cross-build does not prove Windows installation or runtime behavior;
  Setup and the frozen payload are also tested on Windows.
- The two-vCPU VM proves lifecycle and UI behavior, not Threadripper topology
  or performance.
- The physical performance gate proves the tested synthetic contention case,
  not every application and not measured DRAM bandwidth.
- About and tooltip UI Automation proves visible content and action semantics;
  the GitHub action is checked as invokable without opening a browser during
  unattended acceptance.
- The taskbar probe proves bounded `WM_NULL` response behavior in the caller's
  interactive session. It does not identify a private internal Win32k lock or a
  specific third-party fault.
- `.wslconfig` advice is pressure-gated and read-only. It is not workload
  attribution and never edits WSL state.
- A real low/high-memory notification transition, a live multi-user-session
  quorum, and a real render/capture audio veto were not executed on the
  designated VM. Native tests cover the APIs, state machines, and fail-closed
  paths, but do not turn those environment-specific live gates into PASS rows.
- Artifacts are intentionally unsigned. SmartScreen behavior and production
  Authenticode signing remain external release concerns.
