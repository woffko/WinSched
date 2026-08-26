# WinSched

WinSched is a Windows 11 CPU placement controller written in Rust. It groups
Windows CPU Sets by processor group and Last Level Cache (LLC), observes load
through locale-independent PDH counters, and moves opted-in processes only when
the policy predicts a stable and meaningful improvement.

WinSched does not replace, patch, or hook the Windows kernel scheduler. It uses
documented Windows CPU Set, PDH, process QoS, memory-priority, WASAPI, named
pipe, and Service Control Manager APIs.
User-Mode Scheduling is not used because it is not supported on Windows 11.

## Product components

- `winsched-service.exe` is the automatic LocalSystem controller. It starts
  with Windows, persists owned assignments, clears them on shutdown or disable,
  keeps an optional size-bounded circular JSONL log, reserves topology-aware
  system capacity, journals reversible background QoS changes before applying
  them, and recovers safely after an interrupted run.
- `winsched-tray.exe` is a per-session notification-area controller. It shows
  the current mode, service state, managed-process count, last activity, and
  last error. An MTA worker publishes authenticated foreground, visible-window,
  and active-audio veto signals to the service without periodic disk writes.
- `winsched-settings.exe` is the administrative graphical settings editor. It
  validates every field before atomically replacing the configuration and then
  reports whether the running service accepted the reload. Its Diagnostics page
  runs a bounded passive responsiveness probe in a background worker.
- `winsched.exe` is the inspection, passive-diagnostics, and manual-control CLI.

The tray menu contains:

- `Enable Scheduling` / `Disable Scheduling`
- `Start Service` / `Stop Service`
- current configured mode
- managed-process count
- background QoS active/protected count, memory-pressure state, and sensor state
- last controller activity and error
- `Settings...`, `Open Configuration (Advanced)`, `Open Logs`, `Refresh Status`,
  `About WinSched...`, `GitHub Repository`, and `Exit Tray`

The About dialog shows the installed version, project description, MIT license,
and GitHub URL. `GitHub Repository` opens
<https://github.com/woffko/WinSched> in the default browser.

Important Settings labels and controls provide contextual hover help in both
English and Russian, including units and the consequences of policy,
responsiveness, workload-profile, strict-placement, logging, and diagnostic
choices.

Interactive users receive only the service rights required to query, start,
stop, and send the two WinSched control codes. Administrators retain full
service control. The service itself remains the only writer of runtime and
ownership-journal state. The tray can send only authenticated veto telemetry;
it cannot select a process for mutation.

## Install

WinSched 0.5.0 requires 64-bit Windows 11 22H2 or newer (build 22621+). This
minimum keeps EcoQoS state queries on the supported Windows API baseline used
by the ownership journal.

The recommended package is `WinSched-<version>-Setup-x64.exe`. Copy it to a
local Windows drive and run it normally. The wizard requests UAC, installs the
four executables under `C:\Program Files\WinSched`, stores configuration and
runtime data under `C:\ProgramData\WinSched`, registers and starts the automatic
LocalSystem service, creates Start Menu shortcuts, and enables tray autostart by
default. A desktop shortcut is optional.

Do not launch the elevated installer directly from a WSL UNC path such as
`\\wsl.localhost\Ubuntu\...`: the elevated Windows token may not retain that
network provider and can report `ShellExecuteEx code 67`. Copy the installer to
Downloads, the desktop, or another local NTFS path first.

An in-place upgrade preserves the existing `winsched.toml` byte-for-byte,
including comments, and keeps the previous Startup/desktop task choices. The
service is stopped and transactionally reprovisioned before legacy ZIP binaries
are removed.

The packaged default configuration enables automatic placement for active,
eligible interactive-session processes. Session 0 services are never managed,
and implicit all-user scope requires at least 5% of one logical CPU during the
sample interval. Fixed safety exclusions also protect system, protected,
real-time, parked, and foreign-allocated resources, plus Windows shell and
service-host executables such as Explorer, Runtime Broker, and svchost. Exact
process rules bypass only the activity threshold, never the fixed safety
exclusions. New CPU Set assignments are also denied at the Windows mutation
boundary for shell/system targets and the WSL VM hosts `vmmem`, `vmmemWSL`,
`wslhost.exe`, and `wslservice.exe`; clearing an existing assignment remains
allowed. The extracted ZIP is a portable inspection and diagnostics payload;
it deliberately does not contain an elevated service installer. Use the
downloaded or locally hash-verified GUI Setup for installation, upgrade, and
removal so that payload integrity, fixed paths, ACL hardening, and SCM rollback
stay inside one transaction boundary.

Use **Settings > Apps > Installed apps > WinSched > Uninstall** or the Start
Menu uninstall shortcut. Uninstall removes the service, Program Files, and
shortcuts, then asks whether configuration, logs, and saved state should also be
removed. The safe default is **No**. For unattended removal, omit `/PURGEDATA`
to preserve ProgramData or include it to purge all WinSched data:

```text
unins000.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART
unins000.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /PURGEDATA
```

Unsigned development builds can trigger Windows SmartScreen. Production code
signing requires an external Authenticode certificate and is not part of the
source build.

## Configuration and state

Application binaries are stored under `C:\Program Files\WinSched`. Persistent
data is stored under `C:\ProgramData\WinSched`:

- `winsched.toml` — validated, fail-closed configuration
- `winsched.log` and optional `winsched.log.1` ... `.10` archives — bounded
  circular JSONL controller logs
- `winsched-emergency.log` — independent critical startup and logging failures
- `status.json` — atomic tray heartbeat/status snapshot, published immediately
  for control/reload/error transitions and otherwise at a 10-second heartbeat
- `runtime-state.json` — persisted tray enable/disable choice
- `managed-state.json` — PID-and-creation-time ownership journal
- `background-state.json` — write-ahead ownership journal for exact original,
  pending, and applied EcoQoS/memory-priority states

`controller_mode` has three values:

- `off` scopes nothing.
- `observe` evaluates and logs decisions without changing CPU Sets.
- `auto` allows enforcement; the tray can then enable or disable scheduling.

`minimum_process_utilization_bps` controls the activity threshold for processes
included only by `all_user_processes`; `500` means 5% of one logical CPU.
Explicit image rules remain deterministic even while an application is idle.

The `[logging]` section controls the service diagnostic log. `enabled = false`
stops all writes, creation, rotation, and deletion of `winsched.log*` while
preserving files already on disk. `max_file_size_mib` limits the active file to
1–100 MiB, and `retained_archives` keeps 0–10 circular archives. Archive `.1`
is always the newest; `0` reuses only the active file. A single diagnostic
record is never split even if that record alone is larger than the configured
limit. Critical startup or logging failures can still be recorded separately
in `winsched-emergency.log`. Existing schema-1 through schema-3 configurations
remain accepted. Their first save through Settings writes schema 4 without
losing existing values. Background efficiency remains disabled after migration
because older Background profiles did not opt into QoS mutation. Steady
`responsiveness_sample` telemetry is coalesced
to one periodic summary per 60 seconds. Initial state, stable pressure
transitions, memory-profile width changes, errors, and controller decisions
remain immediate.

The `[responsiveness]` section reserves whole physical cores for Windows by
removing their CPU Sets only from WinSched-managed application assignments.
Protected system processes remain unrestricted and may use every processor.
The default is 10 percent, rounded upward and bounded to 2–8 physical cores;
all SMT siblings of a reserved core stay together, and the reserve is spread
over LLC domains. On the validated 32-core Threadripper 3970X topology this is
four physical cores and eight logical processors across four of eight LLCs.

Process rules also have an independent `profile`:

- `interactive` uses stable single-LLC placement; automatic placement becomes
  sticky.
- `memory` uses a stable multi-LLC partition with one SMT sibling per physical
  core by default and an adaptive physical-core width.
- `compute` uses both SMT siblings across all non-reserved assignable cores.
- `background` opts that exact schema-4 rule into the reversible
  background-efficiency policy. CPU Set placement is always disabled for this
  profile, even while the global feature switch is off, so a protected
  foreground/visible/audio process is not left LLC-constrained. Its exact-rule
  `mode = "off"` remains a kill switch. Use another profile when CPU placement
  is desired.
- `balanced` retains standard LLC-aware behavior outside the reserve.

The `[background_efficiency]` section controls an additional exact-rule-only
policy. It never follows `all_user_processes` or the default workload profile.
Eligible headless background processes can receive EcoQoS and Below Normal
memory priority; a Windows low-memory notification temporarily changes only
the owned memory priority to Low. Idle process priority, forced HighQoS, timer
throttling, kernel drivers, SMU/MSR access, and CPU Sets for `vmmemWSL` are not
used.

The packaged configuration leaves the Background master switch, EcoQoS, and
memory-priority mutation off by default. Native acceptance on the designated VM
showed that a later `ping.exe` child inherited its tagged `cmd.exe` parent's
memory priority, while EcoQoS did not propagate in that specific case. Parent
rollback did not restore the live child's inherited memory value. Enable either
process-level property only for a tested leaf workload; indirect child state is
outside WinSched's journal. Restoring the queried process memory value also does
not retag pages already populated under a different priority.

Foreground, visible, minimized, and active render/capture-audio applications
are protected. The unelevated tray sends change-driven data plus a five-second
heartbeat over a local-only named pipe. It samples interactive state every
250 ms. The pipe worker uses cancellable overlapped I/O and event waits rather
than periodic polling. The service verifies the tray image, PID, creation time,
and session, timestamps receipt itself, and lets telemetry only veto or undo an
existing policy. An authenticated receipt wakes the controller through an
event-driven control signal;
while any Background rule or owned process exists, a one-second fallback safety
cadence also bounds re-evaluation independently of the configured CPU-placement
interval. Protection is therefore responsive but not instantaneous: normally
one tray sample plus event dispatch, subject to Windows scheduling and IPC
latency. Missing or stale data causes owned policy to be restored on an
event-driven or fallback safety evaluation. Two clear samples are required
before a new application.

The memory-pressure status distinguishes an unavailable monitor from a
successful non-low reading. A transient query failure retains the last known
pressure state, including Low, while reporting the monitor unavailable; when no
successful reading exists, the state starts as not low.

`background-state.json` is written before mutation. The journal carries
separate EcoQoS and memory-priority ownership masks. WinSched restores a
property only if it still equals the value WinSched applied. An external change
relinquishes and blocks only that property's ownership across transient
foreground/visible/audio vetoes; an explicit disable, graceful stop/restart,
rule removal, or process exit clears the advisory block. The other property
remains independently restorable and reconcilable. Service disable, stop, rule
removal, invalid configuration, crash recovery, and uninstall all use the same
rollback journal. See
[Background efficiency architecture](docs/background-efficiency-design.md).

A 10 ms normal-priority latency probe publishes bounded p50/p95/p99 wake
lateness. Optional DPC and interrupt PDH counters are aggregated per LLC. When
the configured p99 or interrupt pressure stays elevated, the memory profile
shrinks by ten percent; sustained recovery restores one core after cooldown.
This changes concurrency, not memory placement, and does not claim to measure
per-process DRAM bandwidth without an external hardware-counter calibration.
In the validated 48-worker/1-GiB Threadripper contention gate, the 28-core
memory profile reduced reserve-local scheduler wake p99 from 5858.3 to 980.3
microseconds while increasing synthetic random-memory operation throughput by
15.10 percent. The unmanaged workload used about 74 percent of total logical
CPU capacity rather than fully saturating the processor.

An invalid hot-reloaded configuration triggers fail-closed cleanup of owned CPU
Set assignments and background policy, then switches the controller to a safe
observe-only empty scope.
External CPU Set assignments are not overridden unless a rule explicitly uses
`strict` mode. If Windows rejects a cleanup mutation, WinSched retains that
process in `managed-state.json`, reports the failure, and retries instead of
forgetting ownership.

See `config/winsched.example.toml` for a narrow observe-only example and
`config/winsched.default.toml` for the packaged automatic configuration.

Open `Settings...` from the tray or `WinSched Settings` from the Start Menu to
edit General, Adaptive policy, Responsiveness, Background, Process rules, and Logging
pages, or run the read-only Diagnostics page. The editor
supports English and Russian, validates all policy/rule/logging invariants,
uses a two-step confirmation before restoring defaults, and never writes a
partially updated TOML file. UAC is required because the configuration controls
a LocalSystem service. `Open Configuration (Advanced)` remains available for
inspection and manual expert editing.

## Passive diagnostics

`winsched diagnose` measures the current user session without generating input,
changing focus, editing configuration, restarting WSL, or changing CPU Sets.
It samples CPU/LLC utility, processor queue length, DPC/interrupt pressure,
pages input, available memory, normal-priority wake latency, Explorer process
and window counts, active WSL/VMware processes, and the running WinSched status.
The taskbar probe sends only bounded `WM_NULL` messages to `Shell_TrayWnd` from
the interactive caller, with `SMTO_ABORTIFHUNG` and a 50 ms timeout. It never
runs from the LocalSystem service in Session 0.

Stable finding codes distinguish CPU saturation, scheduler latency,
DPC/interrupt pressure, memory pressure, Explorer fan-out, and taskbar latency
while CPU capacity remains available. The latter explicitly explains that more
reserved cores are unlikely to fix a shell-path stall. Explorer fan-out is
reported only as context; WinSched never changes the Windows
`SeparateProcess` setting.

The JSON schema excludes window titles, user names, executable paths, and raw
`.wslconfig` contents. Only recognized WSL resource values are reported. WSL
advice appears only when WSL is active together with measured host CPU or memory
pressure. It is a read-only suggestion: WinSched never edits `.wslconfig`,
runs `wsl --shutdown`, or assigns CPU Sets to `vmmemWSL`. Microsoft documents
the global WSL 2 settings and restart semantics at
<https://learn.microsoft.com/windows/wsl/wsl-config>.

Examples:

```text
winsched diagnose
winsched diagnose --duration-seconds 10 --json
winsched diagnose --json --output C:\Temp\winsched-diagnostic.json
```

The Settings Diagnostics page runs the same engine on a background thread,
supports cancellation, displays localized findings, copies JSON, and writes a
report to Downloads only after an explicit button press.

## CLI

Useful read-only commands:

```text
winsched topology
winsched diagnose --json
winsched responsiveness-plan C:\Path\To\winsched.toml
winsched observe --samples 5
winsched processes
winsched inspect PID
winsched config-check C:\Path\To\winsched.toml
```

Manual mutations remain previews unless `--commit` is supplied:

```text
winsched apply PID --llc auto
winsched apply PID --llc GROUP:LLC --performance-only --commit
winsched clear PID --commit
winsched run --llc GROUP:LLC --commit C:\Path\To\app.exe -- arg1 arg2
```

`run --commit` creates the child suspended, verifies its CPU Set assignment,
and only then resumes it. It terminates the still-suspended child if assignment
or verification fails.

`inspect PID` also reports the explicit EcoQoS tri-state and Windows memory
priority when those queries are supported. There is intentionally no manual
background-QoS commit command because mutations must go through the ownership
journal and rollback controller.

Service control is also available from an interactive terminal:

```text
winsched-service status
winsched-service enable
winsched-service disable
winsched-service start
winsched-service stop
```

## Build and verify

The workspace requires Rust 1.95 or newer. Native policy tests and linting run
on Linux, WSL, or Windows:

```text
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo audit
```

For an MSVC cross-build from WSL, point `RC_PATH` at LLVM's resource compiler:

```text
RC_PATH=/usr/lib/llvm-18/bin/llvm-rc cargo xwin clippy --workspace --all-targets \
  --target x86_64-pc-windows-msvc -- -D warnings
RC_PATH=/usr/lib/llvm-18/bin/llvm-rc cargo xwin build --workspace --release \
  --target x86_64-pc-windows-msvc
```

`scripts/build-release.sh` runs formatting, native tests, native and Windows
Clippy, RustSec audit, release build, and the tray PE import gate. It embeds the
multi-size CPU icon, stages the four portable executables and verified GUI
installer helper, produces SHA-256 files, and writes a versioned ZIP under
`dist/`.

Build the graphical installer on Windows with the official Inno Setup 7
compiler after the frozen payload has been produced:

```powershell
.\scripts\build-gui-installer.ps1 `
  -ISCC "C:\Path\To\Inno Setup 7\ISCC.exe"
```

The script verifies every frozen payload hash before compiling and writes
`WinSched-<version>-Setup-x64.exe` plus a portable `.sha256` file under
`dist\gui-installer`. The final Setup must be accepted on a local Windows 11 x64
machine; a WSL cross-build alone is not release evidence.

## Testing and evidence

WinSched uses separate source, Windows VM, and physical Threadripper gates. The
final 0.5.0 source, native-Windows, service, tray, Settings, installer, upgrade,
uninstall, crash-recovery, logging, and quiet-I/O gates passed on the designated
Windows 11 VM. The previously accepted physical Threadripper topology and
contention measurements remain a scheduling baseline; they were not repeated
for the opt-in Background Efficiency feature.

The 0.5.0 summary is:

| Gate | Result |
|---|---:|
| Rust workspace tests | 104 PASS |
| Native and Windows-target Clippy | PASS |
| RustSec audit | PASS, 383 dependencies |
| Windows-native matrix | 152 PASS |
| Service/runtime and crash recovery | PASS |
| Tray, Settings, tooltips, and Diagnostics UI | PASS |
| Setup 0.4.0 to 0.5.0 upgrade and clean install | PASS |
| GUI and silent preserve/purge uninstall | PASS |
| Quiet I/O | PASS, 7 status writes in 75 seconds; logs byte-stable |
| Earlier Threadripper topology/apply/rollback | PASS, retained baseline |
| Earlier Threadripper p99 / throughput gate | 83.27% lower p99; 15.10% higher throughput |

A forced real low-memory transition, a live multi-interactive-session quorum,
and a real render/capture audio veto were not executed on this VM. Their API,
state-machine, authentication, and fail-closed paths are covered by native
tests; these environment-specific live gates remain explicit limitations.

See [Testing and release validation](docs/testing.md) for the complete matrix,
environment boundaries, exact results, commands, and example JSON output. The
version-specific acceptance records are under `tests/evidence/`.

The generated tray source and all Windows icon sizes are reproducible with:

```text
cargo run -p xtask
```
