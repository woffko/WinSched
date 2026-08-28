# WinSched 0.6.0 release notes

WinSched 0.6.0 adds an on-demand Process Monitor while preserving the accepted
0.5.1 scheduling policy and configuration contract.

## Implemented

- Separate non-elevated `winsched-monitor.exe` process window.
- Left tray click opens or focuses Process Monitor; right click keeps the full
  tray menu.
- Focused-window-only one-second process sampling with no normal file writes.
- CPU, working set, threads, priority, CPU Sets, LLC, EcoQoS, memory priority,
  rule/scope, exclusion, and assignment columns.
- Current-session default with an explicit all-sessions/system toggle.
- Service, Scheduling, mode, managed count, reserve, scheduler p99, Background,
  last activity, and error header.
- Filter, Refresh, and Settings controls.
- Right-click exact-rule draft/edit handoff to elevated Settings without an
  automatic configuration write.
- Single-instance activation for Monitor and Settings.
- Fixed self-exclusion for every WinSched executable.

## Validation

| Gate | Result |
|---|---:|
| Native workspace tests | PASS |
| Native Clippy | PASS, `-D warnings` |
| Windows-target Clippy | PASS, `-D warnings` |
| PowerShell 5.1 parser sweep | PASS, 37 scripts |
| Windows-native tests | PASS, 180 tests in 12 executables |
| Windows snapshot API smoke | PASS, RAM/QoS available for all 72 sampled VM processes |
| Monitor active/minimized/resume | PASS, counter 2 → 2 → 3 |
| Monitor single-instance focus | PASS |
| Tray left click and right-click menu | PASS |
| Settings exact-rule draft handoff | PASS, two drafts, one instance |
| Configuration without Apply | PASS, byte-identical |
| Frozen five-EXE ZIP | PASS, SHA-256 `b9392cf1df77f738a91fbcb504279135d4fc7301d1621729d6306e652ed26802` |
| GUI Setup build | PASS, Inno Setup 7.1.0, SHA-256 `5d4c25bc3bdf11100bf129aa2ad52b7553194ad5239a0974cce2b0d0646fef2a` |
| Installed 0.5.1 → 0.6.0 upgrade | PASS on exact final Setup, byte-identical config and five payload hashes |
| Repeat-install security allowlist | PASS, Monitor accepted and injected provision failure returned exit 9 with full rollback |
| Installer/package lifecycle | PASS, nine stages with original config, Scheduling, and Running service restored |
| Physical-host smoke | PASS on exact Setup, byte-identical config, five payload hashes, operator-confirmed Monitor UI |

The first Setup candidate exposed a repeat-install regression before the
destructive lifecycle began: the application ACL allowlist did not include the
new `winsched-monitor.exe`, so Inno Setup stopped in `PrepareToInstall` with exit
code 7. The allowlist now covers Monitor for both application and portable data
payloads. The GUI installer builder also evaluates the allowlist embedded in
the frozen payload against every packaged file before invoking Inno Setup.

The accepted Setup passed the focused failure-receipt rollback test and the
complete provision, install, preserve, purge, GUI, reinstall, and recovery
lifecycle. See the
[0.6.0 installer lifecycle evidence](https://github.com/woffko/WinSched/blob/v0.6.0/tests/evidence/2026-08-28-v0.6.0-installer-lifecycle.md)
and the privacy-minimized JSON receipts under
`tests/evidence/runtime/v0.6.0/`.

The exact accepted Setup was then installed over 0.5.1 on the physical
Threadripper host. Read-only verification found the prior TOML byte-identical,
all five installed executable hashes exact, the service Running/Automatic as
LocalSystem, Scheduling enabled in Auto mode, and no status error. The operator
confirmed tray left-click activation, single-instance focus, the preserved
right-click menu, required columns, exact-rule draft handoff without Apply, and
polling resumption after minimize/restore. See
[physical Process Monitor smoke](https://github.com/woffko/WinSched/blob/v0.6.0/tests/evidence/2026-08-28-v0.6.0-physical-smoke.md).
