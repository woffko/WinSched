# WinSched 0.5.0 release notes

WinSched 0.5.0 adds an opt-in, journaled Background Efficiency policy for
Windows 11. The final source, native-Windows, installer, service, tray, Settings,
lifecycle, and quiet-I/O acceptance suite passed on the designated Windows VM.

This release requires 64-bit Windows 11 22H2 or newer (build 22621+).
The ZIP is a portable inspection/diagnostics payload; elevated lifecycle
operations are intentionally confined to the GUI Setup transaction.
The legacy portable PowerShell/CMD install and uninstall entry points were
removed from the source and package; GUI Setup is the only supported lifecycle.

## Background Efficiency

- Exact rules with `profile = "background"` can opt into EcoQoS and lower
  Windows memory priority.
- The Background master switch and both mutations remain off in the packaged
  defaults. VM acceptance observed memory priority, but not EcoQoS, propagate
  from a tagged `cmd.exe` to a later `ping.exe`; parent rollback did not restore
  that live child. Launcher and parent workloads are outside the rollback
  contract and require explicit child-tree testing before opt-in.
- Broad `all_user_processes` scope and the default profile never enable this
  policy.
- Exact schema-4 Background profiles use no WinSched CPU Set placement even
  while the global feature switch is off, so interactive veto restoration is
  not undermined by a remaining LLC constraint. Rule `mode = "off"` remains a
  kill switch.
- Normal background memory priority is Below Normal. A Windows low-memory
  notification changes owned processes to Low until the matching high-memory
  recovery notification arrives.
- Restoring the process-level memory-priority value does not retag pages that
  were already populated under another priority.
- Idle/Realtime priority, forced HighQoS, timer-resolution throttling, kernel
  drivers, direct SMU/MSR/PCI access, and CPU Sets for WSL VM hosts are not
  used.

## Interactive safety sensor

- The unelevated tray observes foreground, visible and minimized top-level
  windows, plus active render and capture audio sessions.
- The LocalSystem service receives change-driven state over an authenticated,
  local-only named pipe. No per-second telemetry file is written.
- The tray samples every 250 ms. Cancellable overlapped pipe I/O uses event
  waits with no periodic polling, and each authenticated receipt wakes the
  controller. A one-second fallback safety cadence remains active while an
  exact Background rule or owned record exists.
- The service validates the tray's real PID, session ID, creation time, and
  canonical installed image path and timestamps receipt itself.
- Tray state can only veto or restore an owned policy; it can never select a
  mutation target.
- Missing, incomplete, stale, malformed, or unauthenticated state fails closed.
  A new application requires two clear samples, while a veto restores on the
  next event-driven or fallback safety evaluation. This is responsive but not a
  hard real-time or instantaneous guarantee.
- Protection expands to observed descendants and the matching executable
  cohort, covering common multi-process applications.
- Memory-pressure status reports monitor availability separately. A transient
  query failure retains the last successful pressure state, including Low;
  before the first successful reading, the retained state starts as not low.

## Transactional ownership and rollback

- New `background-state.json` write-ahead records contain exact original,
  pending, and applied values plus per-property ownership masks, keyed by PID
  plus creation time.
- The journal is persisted before mutation and recovered after interrupted or
  partially completed updates.
- EcoQoS and memory priority are restored independently only when their current
  value still matches a WinSched-owned value.
- An external QoS or memory-priority change is preserved and relinquishes only
  that property's ownership. It does not prevent independent rollback or
  reconciliation of the other property.
- Disable, stop, rule removal, invalid configuration, sensor loss, service
  recovery, upgrade, and uninstall share the same rollback path.
- Normal uninstall refuses to remove the service while any managed CPU Set or
  background state still requires cleanup.

## Settings, tray, CLI, and schemas

- Settings has a bilingual Background page with hover help for every policy and
  safety switch, plus live eligible/managed/protected, sensor, and memory state.
- The tray menu adds a Background QoS status row.
- `winsched inspect PID` preserves its existing JSON fields and adds explicit
  EcoQoS and memory-priority information when supported.
- Configuration schema is now 4 and status schema is now 4.
- Schema 1 through 3 files remain accepted. Their legacy Background profiles
  normalize to Balanced, and Background Efficiency remains disabled during
  migration. Opt-in requires explicitly selecting Background again and enabling
  the master switch plus each desired mutation property.

## Final validation

| Gate | Result |
|---|---:|
| Native workspace tests | PASS, 104 tests |
| Native Clippy | PASS, `-D warnings` |
| Windows MSVC Clippy | PASS, `-D warnings` |
| RustSec audit | PASS, 383 dependencies |
| Windows-native console tests | PASS, 152 tests |
| Real EcoQoS/memory apply, independent ownership, and exact restore | PASS on owned child processes |
| Authenticated named pipe and negative image authentication | PASS |
| Event-driven wake, stale-sensor restore, and one-second safety fallback | PASS |
| Service apply, visible-window veto, disable, stop, and crash recovery | PASS |
| PowerShell 5.1 parser | PASS |
| Optimized Windows x64 MSVC build | PASS, four executables |
| Parent/child policy characterization | PASS: memory priority inherited; EcoQoS did not in the tested cmd-to-ping path |
| 0.4.0 to 0.5.0 upgrade and configuration preservation | PASS |
| Interactive Setup, tray, Settings, tooltips, About, and Diagnostics | PASS |
| GUI and silent preserve/purge uninstall | PASS |
| Installer ACL hardening and injected SCM rollback | PASS |
| Setup `ERROR` receipt, nonzero exit and prior-service restore | PASS, exit code 9 |
| Frozen manifest missing/duplicate/traversal rejection | PASS |
| Circular logging and 75-second quiet-I/O gate | PASS |
| Physical Threadripper performance A/B | NOT REPEATED for this feature; the earlier scheduling baseline remains accepted |

See
[`tests/evidence/2026-08-26-v0.5.0-final-acceptance.md`](../tests/evidence/2026-08-26-v0.5.0-final-acceptance.md)
and
[`docs/background-efficiency-design.md`](background-efficiency-design.md)
for the detailed validation and safety boundaries.

The VM was not deliberately driven into a real Windows low-memory transition,
had only one interactive session, and had no acceptance-grade render/capture
audio endpoint. Those three live environment gates are not claimed as PASS.
Memory-notification handling, multi-session intersection/quorum, WASAPI
enumeration, and fail-closed behavior are covered by the Windows-native matrix.

The release artifacts are intentionally not Authenticode-signed. Verify the
published SHA-256 files before installation.

## Release artifacts

| Artifact | Bytes | SHA-256 |
|---|---:|---|
| `WinSched-0.5.0-Setup-x64.exe` | 6,981,726 | `5971a182659b764e8c7d20c2fb99b8451b6f67b034d97dd6b50c456d4d00d553` |
| `WinSched-0.5.0-windows-x64.zip` | 6,038,833 | `bc9703f3fc229031c6d65f7b8945d5ed713213ebccd5fbfe7932b1e6910e7a17` |
