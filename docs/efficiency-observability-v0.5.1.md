# Efficiency and observability design for WinSched 0.5.1

WinSched 0.5.1 reduces steady-state diagnostic I/O and publishes anonymous
self-observability before changing the broad process-placement scope. The
physical-host baseline is recorded in
`tests/evidence/2026-08-26-host-efficiency-baseline.md`.

## Logging levels

Configuration schema 5 replaces `logging.enabled` with one unambiguous level:

- `off`: no routine file sink and no decision aggregation or serialization;
- `normal`: important events and mutation-shaped decisions are immediate, while
  Keep/Ignore decisions become one `decision_summary` per 60 seconds;
- `trace`: Normal events plus every raw per-process decision.

Schemas 1 through 4 migrate in memory. A legacy false/true or omitted
`logging.enabled` becomes Off/Normal. A schemaless document containing the
removed `logging.enabled` field is treated as schema 4, matching pre-0.5.1
serde behavior. Schema 4 retains Background profile semantics; only schemas 1
through 3 normalize their legacy Background placement profile to Balanced.

Normal summaries contain only bounded aggregate counts: window duration,
decision count, unique process cardinality, enforcement requests, action
counts, and fixed reason counts. Exact cardinality storage is capped at 4,096
process keys and exposes a saturation flag. Summaries contain no PID, image
name, path, title, SID, session row, or command line. A heartbeat flushes an
elapsed non-empty window even when no later decision arrives. Reconfiguration,
Scheduling enable/disable, fail-closed policy changes, and shutdown also flush
a partial non-empty window.

## Status schema 5

`status.json` keeps its ten-second/change-driven publication gate and adds one
optional `telemetry` object:

- rolling last-60 evaluation duration mean, p95, maximum, and population counts;
- cumulative placement and Background mutation outcomes;
- successful logical log records/bytes, log write errors, and status writes;
- cumulative service CPU time, uptime, and instantaneous working set.

Resource probe failure is nonfatal and represented by an unavailable optional
service-process sample. Uptime is taken from the controller's monotonic clock,
so wall-clock changes cannot skew the displayed lifetime CPU average. All values
are anonymous scalars. Telemetry changes alone never force an extra status write,
and a heartbeat-only wake never performs an extra full process scan.

## Physical-host ABBA gate

The first decision gate uses A-B-B-A, where A is the initial Scheduling state.
The service remains running and only the reversible enable/disable control is
used. Each phase has a stabilization period and a bounded measurement period.

GUI timing is passive. A low-level mouse hook timestamps a non-injected physical
left click in the taskbar, `EVENT_SYSTEM_FOREGROUND` timestamps Firefox becoming
foreground and restored, and a bounded `WM_NULL` probe timestamps its message
pump response. The click and foreground endpoints use the timestamps supplied
by those Windows events rather than the time at which the harness processes
them. The response probe runs on a dedicated worker and therefore never blocks
the low-level mouse-hook message pump. The marker requests Per-Monitor DPI Aware
V2 before installing either hook so physical mouse points and taskbar window
rectangles use the same coordinate space.

When Firefox is already foreground, the first taskbar click is a possible
minimize. It is ignored only after `IsIconic` independently confirms that the
Firefox window actually became minimized. The following restore click is
measured automatically. Human keypress/reaction time is not in the endpoint.

The harness never calls SendInput, UI Automation, taskbar activation,
SetForegroundWindow, Show Desktop, audio feedback, notifications, or screenshot
APIs. It records no coordinates, titles, URLs, PIDs, or process names. Process
identity is used only in memory to accept Firefox foreground events.
Fullscreen/presentation mode rejects a phase.

A temporary observer executable and marker helper receive exact Off rules so
the measurement tools cannot become their own placement targets. A verified,
recoverable copy of the original bytes is written before mutation. If the live
file still has the exact harness hash, the original configuration bytes and
Scheduling state are restored in `finally`. If another actor changes the file,
the run is invalidated, both versions are retained, and the external edit is not
overwritten. Diagnostic duration, taskbar cadence, and LLC sample counts must
also match the bounded phase before it can pass.

Broad scheduling is considered helpful only when both A/B pairs agree, passive
click-to-responsive p95 improves by at least 10%, scheduler/taskbar latency does
not regress by more than 10%, and workload conditions remain comparable.
Otherwise the result is harmful, no-clear-effect, or invalid. VMware/WSL
throughput remains a separate gate.

The first completed physical run used manual F11/F12 endpoints and is invalid
for a policy decision. Its two pair directions disagreed, and the Disabled
phases exposed a separate service defect: an inactive controller reused an
overdue policy deadline and spun with a zero wait. Those phases consumed
35.62-40.57% of one core and performed 27,768-31,443 writes, so they are also
confounded.

The corrected candidate gives an inactive controller its configured wait,
capped by the ten-second status heartbeat, and skips empty cleanup journal
writes. A 30-second Windows VM acceptance measured 0% of one core, three writes,
three status heartbeats, and zero policy evaluations while Disabled. The wrapper
then restored the exact original service binary, configuration, service state,
and Scheduling state. The corrected physical host measured 0.208% of one core,
three writes, three status heartbeats, and zero policy evaluations over the same
30-second Disabled gate.

The final physical passive ABBA completed 40/40 accepted restores. Enabled p95
was 250 ms versus 234 ms Disabled, a 6.84% disadvantage below the predeclared
10% decision threshold. Both pair directions had Enabled slower, while the
independent taskbar and scheduler metrics did not regress. The policy verdict is
therefore `no_clear_effect`, not harmful and not helpful.

An operator-closed pre-measurement run left two temporary Off rules. The later
measurement phases did not overlap with that aborted process, but the original
configuration required a separate exact-hash recovery afterward. The original
bytes, Scheduling, Logging Off, service PID, and Running state were verified.
The harness now refuses orphan `winsched-abba-*` rules and uses a single-instance
mutex so such state cannot be silently accepted as a new baseline.

## Deferred scope changes

Virtualization protection, foreground CPU-Set escape, process-cohort placement,
and Conservative/Responsive/Aggressive presets remain deferred because ABBA did
not establish the required benefit. The earlier Win32k/taskbar trace is not
evidence that CPU Sets can fix an activation-specific message-pump stall. A
mirrored BAAB or a targeted intervention may be tested later, but this release
does not broaden placement scope.
