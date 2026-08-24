# Testing and release validation

This document describes the validation used for WinSched 0.3.1. It separates
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
| Rust tests | Workspace unit and all-target tests | WSL/Linux | PASS | 86 tests, 0 failures |
| Native lint | Workspace Clippy with `-D warnings` | WSL/Linux | PASS | No warnings |
| Windows lint | MSVC-target workspace Clippy with `-D warnings` | WSL/xwin | PASS | No warnings |
| Dependency audit | `cargo audit` | WSL/Linux | PASS | 383 dependencies, no vulnerability finding |
| Windows build | Optimized x64 MSVC workspace build | WSL/xwin | PASS | Four release executables produced |
| Compatibility import | Tray PE import scan | WSL/Linux | PASS | `TaskDialogIndirect` absent |
| PowerShell syntax | Installer and Windows acceptance scripts | Windows PowerShell 5.1 | PASS | 22 scripts, 0 parser errors |
| Frozen payload | `SHA256SUMS` verification | WSL/Linux and Windows VM | PASS | Every staged file matched |
| Inno Setup | Frozen payload verification and Setup compile | Windows VM | PASS | Inno Setup 7.1.0 |
| In-place upgrade | Setup 0.3.1 over 0.3.0 | Windows VM | PASS | Configuration byte-identical; installed EXEs matched payload |
| Service state | SCM registration after upgrade | Windows VM | PASS | Running, Automatic, LocalSystem, Program Files path |
| Controller runtime | Profiles, reserve exclusion, adaptive move, disable/enable, invalid-config fail-close, SCM recovery | Windows VM | PASS | Exact CPU Set ownership and cleanup |
| Lifecycle | Legacy schema, normal uninstall, purge uninstall, Startup integrity | Windows VM | PASS | Preserve and purge paths verified |
| Circular logging | Enable/disable, size limit, retention, rotation, recovery | Windows VM | PASS | Transactional reconfiguration and complete records |
| Settings GUI | EN/RU pages, atomic Apply, service receipt, defaults, single instance | Windows VM | PASS | AccessKit/UI Automation |
| Settings tooltips | Real pointer hover on important controls | Windows VM | PASS | Four rendered tooltips on three pages; config unchanged |
| Tray About | About dialog version and GitHub URL | Windows VM | PASS | Version 0.3.1 and repository URL visible |
| GitHub action | Repository tray item | Windows VM | PASS | Enabled and exposes InvokePattern |
| Tray status | Reserve, p99/DPC, memory width, mode | Windows VM | PASS | Live schema-3 status rows visible |
| Physical topology | Reserve, Memory, and Compute plans | Threadripper 3970X | PASS | 4 reserved cores; 28 Memory and 56 Compute CPU Sets |
| Physical rollback | Apply and clear exact partitions | Threadripper 3970X | PASS | No CPU Set remained after controller exit |
| Performance gate | Six-phase 48-worker, 1-GiB memory contention A/B | Threadripper 3970X | PASS | p99 improved 83.27%; throughput increased 15.10% |

The controller runtime, lifecycle, circular logging, and physical Threadripper
rows are the accepted 0.3.0 baseline. Version 0.3.1 changes only the tray,
Settings presentation, tests, and documentation; it does not change service,
policy, topology, configuration-schema, or CPU Set ownership logic. The full
0.3.1 workspace was nevertheless rebuilt and linted, and the exact 0.3.1
executables were verified through Setup upgrade and focused UI acceptance.

The detailed acceptance records are:

- `tests/evidence/2026-08-24-threadripper-responsiveness-acceptance.md`
- `tests/evidence/runtime/threadripper-performance-result.json`
- `tests/evidence/2026-08-24-logging-settings-acceptance.md`
- `tests/evidence/2026-08-24-about-tooltips-acceptance.md`

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
  -PackageDirectory .\dist\WinSched-0.3.1-windows-x64 `
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
  -ExpectedVersion 0.3.1
```

Example accepted result:

```json
{
  "result": "PASS",
  "about_menu_text": "About WinSched...",
  "github_menu_text": "GitHub Repository",
  "github_action_invokable": true,
  "about_version": "0.3.1",
  "about_github_url": "https://github.com/woffko/WinSched"
}
```

### Artifact checksum verification

On Windows:

```powershell
Get-FileHash .\WinSched-0.3.1-Setup-x64.exe -Algorithm SHA256
Get-FileHash .\WinSched-0.3.1-windows-x64.zip -Algorithm SHA256
```

On Linux or WSL:

```bash
(cd dist/WinSched-0.3.1-windows-x64 && sha256sum -c SHA256SUMS)
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
- Artifacts are intentionally unsigned. SmartScreen behavior and production
  Authenticode signing remain external release concerns.
