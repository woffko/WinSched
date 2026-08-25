# Testing and release validation

This document describes the validation used for WinSched 0.4.0. It separates
source-level checks, Windows VM acceptance, physical Threadripper acceptance,
and artifact verification so that a passing narrow test is not presented as
proof of a broader behavior.

## Test environments

### Windows installer and lifecycle VM

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
| Rust tests | Workspace unit and all-target tests | WSL/Linux | PASS | 100 tests, 0 failures |
| Native lint | Workspace Clippy with `-D warnings` | WSL/Linux | PASS | No warnings |
| Windows lint | MSVC-target workspace Clippy with `-D warnings` | WSL/xwin | PASS | No warnings |
| Dependency audit | `cargo audit` | WSL/Linux | PASS | 383 dependencies, no vulnerability finding |
| Windows build | Optimized x64 MSVC workspace build | WSL/xwin | PASS | Four release executables produced |
| Compatibility import | Tray PE import scan | WSL/Linux | PASS | `TaskDialogIndirect` absent |
| PowerShell syntax | Installer and Windows acceptance scripts | Windows PowerShell 5.1 | PASS | 23 scripts, 0 parser errors |
| Frozen payload | `SHA256SUMS` verification | WSL/Linux and Windows VM | PASS | Every staged file matched |
| Inno Setup | Frozen payload verification and Setup compile | Windows VM | PASS | Inno Setup 7.1.0 |
| In-place upgrade | Final Setup 0.4.0 over all four frozen 0.3.1 EXEs | Windows VM | PASS | Configuration byte-identical; installed EXEs matched payload |
| Service state | SCM registration after upgrade | Windows VM | PASS | Running, Automatic, LocalSystem, Program Files path |
| Controller runtime | Profiles, reserve exclusion, adaptive move, disable/enable, invalid-config fail-close, SCM recovery | Windows VM | PASS | Exact CPU Set ownership and cleanup |
| Lifecycle | Legacy schema, normal uninstall, purge uninstall, Startup integrity | Windows VM | PASS | Preserve and purge paths verified |
| Circular logging | Enable/disable, size limit, retention, rotation, recovery | Windows VM | PASS | Transactional reconfiguration and complete records |
| Passive CLI diagnostic | Session 0 and interactive Session 1; privacy-safe schema 1 | Physical host and Windows VM | PASS | Bounded read-only collection |
| Diagnostics GUI | Background worker, cancellation contract, localized results, save/cleanup | Windows VM | PASS | 40 samples; taskbar available; config unchanged |
| Status/log cadence | 10-second heartbeat and 60-second responsiveness summary | Windows VM | PASS | 8 writes/75 s; periodic log reasons |
| Reload receipt | Byte-identical TOML rewrite bypasses heartbeat delay | Windows VM | PASS | Receipt in 128 ms; hash unchanged |
| Settings GUI | EN/RU pages, Diagnostics, atomic Apply, service receipt, defaults, single instance | Windows VM | PASS | AccessKit/UI Automation |
| Settings tooltips | Real pointer hover on important controls | Windows VM | PASS | Four rendered tooltips on three pages; config unchanged |
| Tray About | About dialog version and GitHub URL | Windows VM | PASS | Version 0.4.0 and repository URL visible |
| GitHub action | Repository tray item | Windows VM | PASS | Enabled and exposes InvokePattern |
| Tray status | Reserve, p99/DPC, memory width, mode | Windows VM | PASS | Live schema-3 status rows visible |
| Physical topology | Reserve, Memory, and Compute plans | Threadripper 3970X | PASS | 4 reserved cores; 28 Memory and 56 Compute CPU Sets |
| Physical rollback | Apply and clear exact partitions | Threadripper 3970X | PASS | No CPU Set remained after controller exit |
| Performance gate | Six-phase 48-worker, 1-GiB memory contention A/B | Threadripper 3970X | PASS | p99 improved 83.27%; throughput increased 15.10% |

The controller runtime, lifecycle, circular logging, and physical Threadripper
performance rows remain the accepted 0.3.x scheduling baseline. Version 0.4.0
adds read-only diagnostics, lower-level WSL/system mutation protection, a
10-second status heartbeat, and coalesced responsiveness samples. Those changes
received separate physical-host, VM, GUI, cadence, upgrade, and frozen-artifact
acceptance rather than inheriting broad claims from the earlier baseline.

The detailed acceptance records are:

- `tests/evidence/2026-08-24-threadripper-responsiveness-acceptance.md`
- `tests/evidence/runtime/threadripper-performance-result.json`
- `tests/evidence/2026-08-24-logging-settings-acceptance.md`
- `tests/evidence/2026-08-24-about-tooltips-acceptance.md`
- `tests/evidence/2026-08-25-diagnostics-quiet-io-acceptance.md`

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
  -PackageDirectory .\dist\WinSched-0.4.0-windows-x64 `
  -InteractiveUser $env:USERNAME `
  -InstallDirectory C:\ProgramData\WinSchedAcceptance
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
  -ExpectedVersion 0.4.0
```

Example accepted result:

```json
{
  "result": "PASS",
  "about_menu_text": "About WinSched...",
  "github_menu_text": "GitHub Repository",
  "github_action_invokable": true,
  "about_version": "0.4.0",
  "about_github_url": "https://github.com/woffko/WinSched"
}
```

### Artifact checksum verification

On Windows:

```powershell
Get-FileHash .\WinSched-0.4.0-Setup-x64.exe -Algorithm SHA256
Get-FileHash .\WinSched-0.4.0-windows-x64.zip -Algorithm SHA256
```

On Linux or WSL:

```bash
(cd dist/WinSched-0.4.0-windows-x64 && sha256sum -c SHA256SUMS)
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
- Artifacts are intentionally unsigned. SmartScreen behavior and production
  Authenticode signing remain external release concerns.
