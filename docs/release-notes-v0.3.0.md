# WinSched 0.3.0

WinSched 0.3 adds a topology-aware system responsiveness reserve and workload
profiles designed around high-core-count AMD Threadripper systems.

## Service logging and GUI controls

- Service JSONL logging can be enabled or disabled without restarting the
  service.
- Settings exposes the active-file size limit and retained archive count.
- Rotation keeps complete records, bounds the active file, prunes old archives,
  and supports zero retained archives.
- Configuration reload is transactional: a failed logging reconfiguration
  retains or restores the previous usable writer.
- The English/Russian Settings GUI includes General, Adaptive, Responsiveness,
  Process rules, and Logging pages with atomic Apply and service receipts.

## Responsiveness reserve

- Reserve capacity is calculated from physical cores, never logical threads.
- Whole SMT sibling pairs are spread deterministically over LLC domains.
- Protected Windows processes remain unrestricted and may use every CPU.
- Reserved CPU Sets are removed only from WinSched-managed application plans.
- The default 10 percent plan on the validated Threadripper 3970X reserves four
  physical cores and eight threads across four of eight LLC domains.
- Existing managed assignments refresh immediately when the reserve changes.

## Workload profiles

- `interactive`: stable single-LLC placement; automatic placement becomes
  sticky.
- `memory`: stable multi-LLC placement with one SMT sibling per physical core
  by default and adaptive physical-core width.
- `compute`: both SMT siblings across every non-reserved assignable core.
- `background` and `balanced`: existing LLC-aware policy over the non-reserved
  topology.

Managed-state schema 2 now records exact multi-LLC CPU Set partitions while
preserving schema-1 recovery.

## Latency guard and telemetry

- A normal-priority 10 ms probe publishes bounded p50, p95, p99, and maximum
  scheduler wake lateness.
- Optional per-LLC DPC and interrupt-time PDH counters are nonfatal when
  unavailable.
- Sustained latency or interrupt pressure shrinks the memory profile by ten
  percent within configured bounds.
- Sustained recovery restores one physical core after cooldown.
- `winsched responsiveness-plan CONFIG` previews the complete reserve, memory,
  and compute partitions without changing a process.

The integrated controller does not claim direct per-process DRAM-bandwidth
measurement. AMD uProf remains an optional external calibration tool.

On the validated 3970X, the six-phase 48-worker synthetic memory-contention
gate reduced median reserve-local scheduler wake p99 from 5858.3 to 980.3
microseconds (83.27 percent) while increasing useful random-memory operation
throughput by 15.10 percent. The workload used about 74 percent of total logical
CPU capacity in the unmanaged baseline, so the result does not depend on full
processor saturation. This metric is not a DRAM-bandwidth claim.

## Configuration and compatibility

- Controller configuration schema is now 3.
- Schema-1 and schema-2 files remain accepted and keep the responsiveness
  controller disabled until explicitly enabled.
- New installations enable the validated 10 percent reserve by default.
- Settings adds an English/Russian Responsiveness page and workload-profile
  selectors.
- Tray status shows reserved cores, p99 latency, DPC load, memory-profile width,
  and pressure state.
