# WinSched

WinSched is a Windows 11 CPU placement controller written in Rust. It groups
Windows CPU Sets by processor group and Last Level Cache (LLC), observes load
through locale-independent PDH counters, and moves opted-in processes only when
the policy predicts a stable and meaningful improvement.

WinSched does not replace, patch, or hook the Windows kernel scheduler. It uses
documented Windows CPU Set, PDH, process, and Service Control Manager APIs.
User-Mode Scheduling is not used because it is not supported on Windows 11.

## Product components

- `winsched-service.exe` is the automatic LocalSystem controller. It starts
  with Windows, persists owned assignments, clears them on shutdown or disable,
  keeps an optional size-bounded circular JSONL log, reserves topology-aware
  system capacity, and recovers safely after an interrupted run.
- `winsched-tray.exe` is a per-session notification-area controller. It shows
  the current mode, service state, managed-process count, last activity, and
  last error.
- `winsched-settings.exe` is the administrative graphical settings editor. It
  validates every field before atomically replacing the configuration and then
  reports whether the running service accepted the reload.
- `winsched.exe` is the inspection and manual-control CLI.

The tray menu contains:

- `Enable Scheduling` / `Disable Scheduling`
- `Start Service` / `Stop Service`
- current configured mode
- managed-process count
- last controller activity and error
- `Settings...`, `Open Configuration (Advanced)`, `Open Logs`, `Refresh Status`,
  and `Exit Tray`

Interactive users receive only the service rights required to query, start,
stop, and send the two WinSched control codes. Administrators retain full
service control. The service itself remains the only writer of runtime and
managed-assignment state.

## Install

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
exclusions. The extracted ZIP remains available for advanced or scripted
deployment. Run `Install WinSched.cmd` from that directory. To install a custom
configuration with the ZIP package, run the elevated PowerShell installer:

```powershell
.\install.ps1 -Configuration C:\Path\To\winsched.toml
```

An upgrade without an explicit `-Configuration` preserves the installed
`winsched.toml`. Supplying the option deliberately replaces it after validation.

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
- `status.json` — atomic tray heartbeat/status snapshot
- `runtime-state.json` — persisted tray enable/disable choice
- `managed-state.json` — PID-and-creation-time ownership journal

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
in `winsched-emergency.log`. Existing schema-1 configurations remain accepted
and immediately use the default logging policy (`true`, 10 MiB, one archive)
in memory. Their first save through Settings writes the current schema without
losing existing values.

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
- `background` and `balanced` retain LLC-aware behavior outside the reserve.

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

An invalid hot-reloaded configuration immediately clears owned CPU Set
assignments and switches the controller to a safe observe-only empty scope.
External CPU Set assignments are not overridden unless a rule explicitly uses
`strict` mode. If Windows rejects a cleanup mutation, WinSched retains that
process in `managed-state.json`, reports the failure, and retries instead of
forgetting ownership.

See `config/winsched.example.toml` for a narrow observe-only example and
`config/winsched.default.toml` for the packaged automatic configuration.

Open `Settings...` from the tray or `WinSched Settings` from the Start Menu to
edit General, Adaptive policy, Responsiveness, Process rules, and Logging
pages. The editor
supports English and Russian, validates all policy/rule/logging invariants,
uses a two-step confirmation before restoring defaults, and never writes a
partially updated TOML file. UAC is required because the configuration controls
a LocalSystem service. `Open Configuration (Advanced)` remains available for
inspection and manual expert editing.

## CLI

Useful read-only commands:

```text
winsched topology
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
multi-size CPU icon, stages the four executables and scripted installer,
produces portable SHA-256 files, and writes a versioned ZIP under `dist/`.

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

The generated tray source and all Windows icon sizes are reproducible with:

```text
cargo run -p xtask
```
