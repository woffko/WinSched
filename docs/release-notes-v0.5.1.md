# WinSched 0.5.1 development notes

WinSched 0.5.1 is under validation and is not yet a published release.

## Implemented

- Configuration schema 5 with Off, Normal, and Trace logging levels.
- Schema-1 through schema-4 logging migration without rewriting preserved
  configuration during Setup.
- One-minute aggregation of unchanged placement decisions in Normal mode.
- Early Off gating that avoids raw decision JSON construction.
- Immediate mutation, enforcement, cleanup, reload, transition, and failure
  events.
- Status schema 5 with anonymous evaluation, mutation, logging, status-write,
  service CPU, uptime, and working-set telemetry.
- Settings controls and bilingual controller-efficiency status.
- A passive Firefox taskbar ABBA marker that observes physical clicks,
  foreground events, and bounded message-pump response without generating input
  or changing focus. Windows event timestamps, a dedicated response-probe
  worker, and an `IsIconic` gate keep the mouse hook non-blocking and require a
  real minimize-before-restore pair.
- A Disabled-mode wait fix that prevents an overdue policy timestamp from
  turning the inactive cleanup/status loop into a zero-wait spin.

## Current validation

| Gate | Result |
|---|---:|
| Config schema/migration tests | PASS, 24 tests |
| Native workspace tests | PASS, 123 tests |
| Native Clippy | PASS, `-D warnings` |
| Windows-target Clippy | PASS, `-D warnings` |
| Windows PowerShell 5.1 parser | PASS, 31 scripts |
| Physical logging-off baseline | PASS |
| Windows-native tests | PASS, 175 tests in 10 executables |
| Windows VM Disabled idle | PASS, 0% of one core, 0.10 writes/s, 3 heartbeats in 30 seconds |
| Windows VM Normal/Trace/Off and rotation | PASS |
| Windows VM 0.5.0 upgrade | PASS on current Setup hash, byte-identical schema-4 fixture |
| Windows VM 75-second quiet-I/O | PASS, 7 status writes; logs byte-stable |
| Windows VM interactive UI | PASS, tray, Settings, tooltips, and Diagnostics |
| Installer preserve/purge lifecycle | PASS, nine stages with final state restoration |
| Physical host 0.5.1 upgrade | PASS, exact TOML and installed hashes |
| Physical corrected-candidate reinstall | PASS, current Setup/service hash and byte-identical TOML |
| Physical corrected Disabled idle | PASS, 0.208% one core, 0.10 writes/s, 0 evaluations |
| Physical Off/Normal logging efficiency | PASS |
| Earlier physical manual-marker ABBA | INVALID for policy: human timing, inconsistent pairs, Disabled busy-loop confounder |
| Physical passive marker pilot | PASS, confirmed minimize/restore and 187 ms click-to-response |
| Physical passive taskbar ABBA | PASS, 40/40 samples; valid `no_clear_effect` |
| Responsive scope decision | NO CHANGE; ABBA did not establish a 10% benefit |
