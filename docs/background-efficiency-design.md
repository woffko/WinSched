# Background efficiency architecture

This document defines the schema-4 background-efficiency policy introduced in
WinSched 0.5.0. The feature is a reversible user-mode policy; it is
not a replacement scheduler and does not install a kernel driver.

The supported baseline is 64-bit Windows 11 22H2 or newer (build 22621+). The
minimum aligns the ownership journal with the Windows process-information APIs
used to query and verify EcoQoS state.

## Scope

Background efficiency is independent from CPU Set placement. A process is in
scope only when all of the following are true:

- the controller is in `auto` mode and scheduling is enabled;
- `[background_efficiency].enabled` is true;
- an exact executable-name rule has `profile = "background"`;
- the process is in a nonzero interactive session and passes the fixed safety
  exclusions;
- fresh foreground, visible-window, and audio veto data is available for that
  session;
- the process and its matching image cohort are not protected by a veto.

`all_user_processes` and `default_workload_profile` never opt a process into
this mutation surface. Schemas 1 through 3 migrate with background efficiency
disabled, and their old Background placement profiles migrate to Balanced so
they do not silently change meaning. In schema 4, an exact Background profile
always resolves CPU Set placement to Off regardless of the global feature
switch. Its rule `mode = "off"` remains a kill switch for Background Efficiency.
This makes the QoS policy independent and ensures a foreground/visible/audio
veto does not leave the process constrained to one WinSched CPU partition.

## Applied policy

For an eligible process, WinSched can independently request:

- EcoQoS through `SetProcessInformation(ProcessPowerThrottling)` while changing
  only `PROCESS_POWER_THROTTLING_EXECUTION_SPEED`;
- `MEMORY_PRIORITY_BELOW_NORMAL` during normal operation;
- `MEMORY_PRIORITY_LOW` while Windows reports a low-memory condition.

WinSched does not set Idle or Realtime process priority, does not force
HighQoS, and does not change the timer-resolution throttling flag. If either
EcoQoS or memory-priority handling is disabled, that property retains its
original value.

The Background master switch and both mutation properties are disabled in the
packaged defaults. On the designated VM, a later `ping.exe` inherited its
tagged `cmd.exe` parent's memory priority; EcoQoS did not propagate in that
specific test. Restoring the parent did not restore the live child. Users must
therefore enable either process-level property only for a tested leaf workload;
indirect child state is outside the rollback contract.

Memory-priority rollback restores and verifies the directly managed process value. It does
not retag pages that were already populated while another priority was active,
so exact process-state rollback is not a claim of page-by-page reversal.

The memory-pressure state uses both `LowMemoryResourceNotification` and
`HighMemoryResourceNotification`. A low notification enters pressure, a high
notification leaves it, and the documented middle band retains the preceding
state. This avoids a private threshold and rapid oscillation. Status reports
monitor availability separately from the retained pressure value. A transient
query failure retains the last successful value, including Low, while marking
the monitor unavailable. If no successful reading exists, the initial retained
state is not low.

## Interactive veto channel

The LocalSystem service runs in Session 0. It cannot reliably enumerate the
foreground desktop or per-user WASAPI sessions. The unelevated tray therefore
collects the signals in each interactive session:

- `GetForegroundWindow` and `GetWindowThreadProcessId`;
- visible top-level windows from `EnumWindows` and `IsWindowVisible`;
- active render and capture sessions from WASAPI
  `IAudioSessionManager2`/`IAudioSessionControl2`.

Minimized windows remain protected because restoring a minimized application
from the taskbar is an interactive operation. Protection expands to observed
descendants and to the same-image cohort selected by the exact rule, which
covers common multi-process applications.

The tray samples the interactive state every 250 ms and publishes compact
schema-1 JSON through the local-only `WinSchedInteractive-v1` named pipe only
after a change or at a five-second heartbeat. The server uses cancellable
overlapped connect/read operations and event waits; it has no sleep-based or
periodic named-pipe polling loop. The service:

- owns the pipe and rejects remote clients;
- grants pipe access only to System, Administrators, and interactive users;
- obtains the real client PID and session ID from the pipe handle;
- checks the client's creation time and canonical image path against the
  installed `winsched-tray.exe`;
- ignores the client timestamp and records receipt time itself;
- bounds one message to 64 KiB, each PID list to 4096 entries, and retained
  publishers to 64;
- treats the data only as a veto, never as a source of mutation targets.

The state expires after 15 seconds. A missing, incomplete, stale, malformed, or
unauthenticated state prevents a new policy and restores only processes already
owned by WinSched. Each authenticated pipe receipt wakes the controller through
an event-driven control signal. While an exact Background rule or owned record
exists, a one-second fallback safety cadence bounds re-evaluation independently
of the slower CPU-placement interval without forcing full process enumeration
at the tray's 4 Hz sampling rate. Veto response is therefore responsive, not
instantaneous: normally one 250 ms tray sample plus event dispatch, with
additional Windows scheduling and IPC latency possible. The policy requires two
consecutive clear samples before first application, while a veto restores on
the next event-driven or fallback evaluation without that two-sample delay.

## Ownership journal and rollback

`background-state.json` is separate from the CPU Set journal. Each record is
keyed by PID plus process creation time and contains:

- the exact original EcoQoS and memory-priority values;
- the last state verified as applied by WinSched;
- an optional pending state written before a mutation;
- independent EcoQoS and memory-priority ownership masks for applied and
  pending mutations;
- a per-property external-override block when another controller changes a
  value.

The transaction sequence is:

1. Open the process with query access and verify its creation time.
2. Read the original state and capability-probe both APIs.
3. Persist a pending journal record atomically.
4. Reopen with `PROCESS_SET_INFORMATION`, verify the expected state, mutate,
   and read back the result.
5. Mark the pending state as applied and persist again.

After a crash, the service distinguishes a mutation that did not start, one
that completed before the final journal write, and a partial or external
change. Disable, service stop, rule removal, an interactive veto, invalid
configuration, and uninstall all use the same rollback path.

Rollback is property-specific. EcoQoS is restored only when its current value
still equals a WinSched-applied or pending value; memory priority follows the
same rule independently. A value changed by another controller is preserved and
only that property's ownership is relinquished. The block survives transient
foreground, visibility, and audio vetoes for the same process identity. An
explicit scheduling disable, graceful service stop/restart, rule removal, or
process exit clears this advisory block together with the rest of controller
state. The other property can remain owned, be restored, and be reconciled
independently. PID reuse can therefore never restore state into an unrelated
process.

The service refuses normal uninstall while either ownership journal still has
pending entries. Installer fallback paths do not bypass this check when the
installed service executable is available.

## Safety boundaries

The background mutation boundary independently rejects:

- Session 0;
- protected or unqueryable processes;
- Realtime-priority processes;
- Explorer, DWM, audio and shell infrastructure;
- WinSched processes;
- Hyper-V and WSL hosts including `vmmem`, `vmmemWSL`, `vmcompute.exe`,
  `vmwp.exe`, `wslhost.exe`, and `wslservice.exe`.

VMware workloads are not implicit targets. A user must create and test an
exact Background rule before a VMware executable can enter this policy.

## Validation boundary

The final 0.5.0 source and Windows-native matrices passed schema migration,
exact-rule scope, positive and negative named-pipe authentication, stale and
incomplete veto behavior, event-driven wake and shutdown, the one-second
fallback cadence, independent ownership masks, transient memory-monitor
retention, WAL recovery, real EcoQoS and memory-priority readback, external
override preservation, and real apply/visible-restore on owned child processes.
The designated VM also passed service crash recovery, installer upgrade and
rollback, tray/Settings UI, uninstall, logging, and quiet-I/O acceptance.

Three environment-specific live gates remain unexecuted: a forced real Windows
low-memory transition, multi-interactive-session quorum on a machine with more
than one active user session, and a real render/capture audio veto on an
acceptance-grade endpoint. Their APIs, state machines, and fail-closed paths are
covered by native tests. Physical Threadripper performance acceptance remains
separate and was not repeated for this feature.
