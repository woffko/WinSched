# Background efficiency development validation — 2026-08-25

## Scope

This record covers the schema-4 Background Efficiency implementation before a
release candidate is published. It does not claim installer, multi-session, or
physical Threadripper acceptance.

The PASS results below belong to the bounded development snapshot on which the
commands were run. Subsequent hardening added event-driven controller wake-up,
per-property ownership masks, memory-monitor retention handling, and additional
rollback paths. A complete post-hardening rerun is PENDING; this record must not
be used as final release-candidate evidence.

## Result summary

| Gate | Environment | Result |
|---|---|---:|
| Formatting | WSL/Linux | PASS |
| Workspace unit/all-target tests | WSL/Linux | PASS, 102 tests |
| Native Clippy | WSL/Linux | PASS, `-D warnings` |
| Windows MSVC Clippy | WSL/cargo-xwin | PASS, `-D warnings` |
| RustSec audit | WSL/Linux | PASS, 383 dependencies |
| Windows platform/API suite | Physical Windows 11 host via WSL interop | PASS, 12 tests |
| Windows service suite | Physical Windows 11 host via WSL interop | PASS, 43 tests |
| Existing Windows config/control/core/CLI suites | Physical Windows 11 host via WSL interop | PASS, 62 tests |
| PowerShell parser | Windows PowerShell 5.1 | PASS, 23 files |
| Final post-hardening source/native matrix | WSL/Linux and Windows | PENDING |
| Event-driven pipe wake and one-second fallback safety cadence | Designated Windows VM | PENDING |
| Per-property override isolation and transient monitor failure | Windows-native and VM | PENDING |
| Parent/child policy characterization | Interactive `cmd.exe` -> later `ping.exe` | PASS: memory priority inherited, EcoQoS did not; parent rollback left the child's inherited value unchanged |
| Designated Windows VM runtime | Isolated Windows 11 x64 VM | PENDING in this development snapshot |
| GUI/installer rendered acceptance | Designated Windows VM | PENDING |
| Physical Threadripper performance A/B | Threadripper 3970X | NOT RUN for this feature |

The recorded 117 Windows-native console tests comprise 12 platform tests, 43
service tests, and 62 existing CLI/config/control/core tests. Native Settings
and tray library tests are included in the recorded 102-test workspace result.
Windows Settings and tray binaries compiled under the recorded MSVC Clippy gate
but still require the post-hardening compile rerun and rendered VM acceptance.

## Real Windows mutation tests

The tests create dedicated child processes and never target an existing user or
system process.

1. Read the child's original explicit EcoQoS tri-state and memory priority.
2. Apply EcoQoS plus Low memory priority with expected-state verification.
3. Verify both values through `GetProcessInformation`.
4. Restore the exact original values.
5. Apply again, simulate an external memory-priority override, restore, and
   verify that EcoQoS is restored while the external memory value is preserved.
6. Run the service reconciler twice to satisfy its clear-sample hysteresis,
   verify EcoQoS plus Below Normal memory priority, inject a visible-window
   cohort veto, and verify exact restoration plus an empty journal.

All operations passed. Test child processes were terminated and no `ping.exe`
helper remained afterward.

## Named-pipe and probe tests

The Windows platform suite also verified:

- the local-only named pipe accepts a client only when its real PID, session,
  creation time, and canonical executable path match;
- the service replaces the untrusted client timestamp with receipt time;
- the low/high memory-resource notification handles initialize and query;
- WASAPI enumerates active render/capture endpoints in a dedicated MTA context;
- message and publisher bounds compile under Windows MSVC linting.

These checks predate the event-driven wake-up change. The current design samples
interactive state every 250 ms, uses cancellable overlapped pipe operations with
event waits and no periodic pipe polling, wakes the controller after an
authenticated receipt, and keeps a one-second fallback safety cadence. Runtime
verification of that timing and shutdown behavior is PENDING.

## Documented policy boundaries

- The supported baseline is 64-bit Windows 11 22H2 (build 22621) or newer.
- The Background master switch and both process-level mutations are off in the
  packaged defaults. Native VM acceptance observed memory priority propagate
  to a later child while EcoQoS did not in the same cmd-to-ping path; restoring
  the parent did not restore the child. Child behavior remains an explicit
  workload acceptance gate.
- Restoring process memory priority does not retag pages already populated under
  another priority.
- EcoQoS and memory priority use separate ownership masks. An external override
  relinquishes only the changed property, leaving the other independently
  restorable.
- A transient memory-monitor query failure retains the last successful pressure
  value, including Low, while reporting the monitor unavailable. With no prior
  successful value, retained pressure starts as not low.
- Foreground/audio protection is responsive but not instantaneous: normally one
  250 ms tray sample plus event dispatch, with a one-second fallback evaluation
  and additional Windows scheduling or IPC latency possible.

## Commands

```bash
cargo fmt --all -- --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo audit
RC_PATH=/usr/lib/llvm-18/bin/llvm-rc cargo xwin clippy \
  --workspace --all-targets --target x86_64-pc-windows-msvc -- -D warnings
RC_PATH=/usr/lib/llvm-18/bin/llvm-rc cargo xwin test \
  --workspace --target x86_64-pc-windows-msvc --no-run
```

The resulting Windows console test executables were run through Windows' WSL
interop because `cargo-xwin` was configured with a Wine runner and Wine was not
installed. GUI test executables were not treated as runtime evidence.

## Remaining release gates

- complete post-hardening formatting, tests, lint, audit, Windows-native, and
  optimized build rerun;
- event-driven pipe connect/read shutdown, receipt wake-up, and one-second
  fallback cadence;
- authenticated-pipe rejection cases, including a mismatched client identity
  and a connected client that does not complete a message;
- per-property external override isolation;
- real low-memory/high-memory transitions plus transient monitor failure and
  retained-Low behavior;
- service plus unelevated tray named-pipe operation under LocalSystem;
- exact Background rule apply and minimized-window restore in a real
  interactive VM session;
- active render and capture audio veto;
- tray termination and 15-second stale-state rollback;
- two interactive sessions with independent authenticated sensor state;
- Settings EN/RU Background controls, hover help, and switch persistence;
- tray Background status row and the 75-second quiet-I/O gate while the tray is
  running;
- schema-3/0.4.0 upgrade preservation and legacy Background migration;
- disable, stop, invalid configuration, and service crash while background
  ownership or a pending transaction exists;
- normal/purge uninstall and upgrade while owned, including injected cleanup
  failure refusal;
- graphical installer compilation and local-drive launch.
