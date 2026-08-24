# Threadripper responsiveness architecture

This document defines the WinSched 0.3 topology-aware responsiveness policy.
It is based on a live Windows CPU Set snapshot from the target AMD Ryzen
Threadripper 3970X host, not the flattened topology exposed inside WSL.

## Observed target topology

- 32 physical cores and 64 logical processors with SMT enabled
- one Windows processor group
- one Windows NUMA node
- eight last-level-cache domains
- four physical cores and eight CPU Sets per LLC domain
- heterogeneous Windows scheduling-class rankings despite one efficiency class

The controller must never calculate reserve capacity from logical processors.
A 10 percent reserve therefore rounds upward from 3.2 to four physical cores,
and reserves both SMT siblings for each selected core.

## System reserve contract

- Protected Windows processes remain unassigned and may use every processor.
- Reserved CPU Sets are excluded only from WinSched-managed application plans.
- The reserve is deterministic and spread evenly over LLC domains.
- The highest scheduling-class physical core in each selected region wins.
- At least one physical core remains outside the reserve.
- Parked, realtime, and foreign-allocated CPU Sets are never made assignable.
- A configuration change refreshes owned assignments immediately without
  waiting for an overload threshold.

The reserve is soft: applications outside WinSched scope can still use those
processors. WinSched does not claim undocumented Windows exclusive-core
allocation facilities.

## Workload profiles

- `system`: represented by the fixed safety exclusions; no CPU Set mutation.
- `interactive`: stable single-LLC placement. `auto` placement is interpreted
  as sticky for this profile.
- `memory`: a stable multi-LLC partition with one logical processor per
  physical core by default. Its physical-core width is adaptive.
- `compute`: every assignable SMT sibling across the non-reserved topology.
- `background` and `balanced`: existing LLC placement behavior over the
  non-reserved topology.

Exact `strict` placement always overrides workload-profile partition planning.
Foreign CPU Set assignments remain untouched unless the exact rule is strict.

## Adaptive memory width

A normal-priority 10 millisecond probe records a bounded 60 second scheduler
wake-latency window. The service publishes p50, p95, p99, maximum lateness, and
per-LLC DPC and interrupt-time maxima.

After a complete sample window, sustained p99, DPC, or interrupt pressure
shrinks the memory profile by ten percent, bounded by the configured minimum.
Sustained recovery restores one physical core after the configured cooldown.
The default 3970X bounds are 8 through 28 cores with a five minute cooldown.

This is concurrency shaping, not memory migration. WinSched does not claim to
measure per-process DRAM bandwidth without an optional hardware-counter
provider. AMD uProf remains an acceptance and calibration tool rather than a
runtime dependency.

## Ownership and rollback

Managed-state schema 2 stores the exact observed CPU Set partition plus one
anchor LLC. Schema-1 journals migrate in memory and learn their exact partition
from the next matching process observation. Disable, stop, invalid
configuration, exclusion, or persistence failure clears only assignments owned
by WinSched.

## Acceptance gates

1. Deterministic 3970X fixture: four reserved physical cores, eight CPU Sets,
   spread over four of eight LLC domains.
2. No reserved ID appears in an interactive, memory, compute, background, or
   balanced application partition.
3. Memory profile uses one SMT sibling per selected physical core by default.
4. Compute profile uses both siblings and can span multiple LLC domains.
5. A reserve or profile change refreshes a managed process without oscillation.
6. Schema 1 and 2 configuration migration and schema-1 managed-state migration
   preserve rollback.
7. Windows-native service tests, Settings UI automation, installer upgrade, and
   live Threadripper observe/apply/rollback all pass.
8. Under the representative memory workload, p99 scheduling lateness improves
   by at least 20 percent while useful workload throughput loses no more than
   5 percent. Otherwise the candidate is retuned or rejected.

The accepted six-phase 48-worker/1-GiB run measured an 83.27 percent p99
improvement and a 15.10 percent throughput increase. The independent probe was
assigned the same reserved CPU Sets in both Observe and Auto phases; the
workload alone changed from unrestricted to the exact memory-profile partition.
