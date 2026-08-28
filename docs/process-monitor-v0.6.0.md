# WinSched 0.6.0 Process Monitor design

WinSched Process Monitor is a separate, non-elevated executable opened by a
normal left click on the notification-area icon. The existing tray menu remains
available on right click. Keeping the monitor outside the tray isolates GUI and
snapshot failures from the interactive protection publisher and avoids a UAC
prompt for read-only inspection.

## Sampling contract

The monitor schedules a one-second process snapshot only while its viewport is
focused, visible, not minimized, and not occluded. Losing focus stops new work;
one already-running bounded snapshot may finish. Returning to the window starts
an immediate refresh. CPU/LLC topology is queried once and reused. The default
snapshot is limited to the current interactive session; querying every session
is an explicit UI option.

Each row can expose image name, PID and creation identity, session, one-core CPU
use over the latest interval, working set, thread count, priority class,
explicit CPU Set IDs, current LLC, EcoQoS, memory priority, effective rule or
scope, exclusion reason, and CPU Set assignment ownership. Unavailable process
fields remain visible as unavailable rather than dropping the whole row.

The normal executable writes no monitoring data. A hidden acceptance-only
argument can write a snapshot-start counter to a caller-selected file so tests
can prove that minimized polling stops; the tray never passes this argument.

## Single instance and activation

The first monitor instance owns a per-user file lock and a per-session named
auto-reset event. A later invocation signals that event and exits. The existing
window restores, becomes visible, requests focus, and refreshes immediately.
The activation channel carries no process or configuration data.

## Exact-rule handoff

The row context menu offers `Create exact rule...` or `Edit exact rule...`.
Safety-excluded processes cannot be newly opted into control. The monitor starts
elevated Settings with only `--rule-image <image.exe>`; paths and additional
arguments are rejected.

Settings creates an unsaved `auto`/`balanced` draft or scrolls to the existing
case-insensitive exact rule. Apply remains mandatory. If Settings is already
open, the second elevated invocation writes a bounded, fresh per-user activation
request, signals a per-session event, and exits. The existing editor consumes
the request and requests focus. The request cannot directly save configuration
or invoke a service mutation.

## Validation boundary

VM acceptance covers read-only Windows process details, visible controls and
columns, active sampling, a stable minimized interval, resume after activation,
monitor single-instance behavior, tray left/right click semantics, Settings
single-instance forwarding, prefilled drafts, and byte-identical configuration
when Apply is not selected.
