# WinSched 0.5.0 physical-host efficiency baseline

Date: 2026-08-26

Environment: Windows 11 x64, AMD Ryzen Threadripper 3970X, 32 physical cores,
64 logical processors, and eight LLC domains. This is a local operational
baseline, not an A/B claim of benefit.

## Installed state

- The four installed executables matched the published WinSched 0.5.0 release
  hashes.
- Controller mode was Auto with implicit all-user scope and the Balanced
  workload profile.
- Four physical cores and eight logical processors were excluded from managed
  application assignments across four LLC domains.
- Background Efficiency was inactive.

## Coherent retained-log snapshot

The active log and its one retained archive covered 1,000.206 seconds.

| Metric | Result |
|---|---:|
| JSONL bytes | 19,144,917 |
| Records | 73,931 |
| Policy decisions | 73,898 |
| Keep decisions | 73,882 (99.97835%) |
| Assign decisions | 16 (0.02165%) |
| Move decisions | 0 |
| Enforcement successes/failures | 16 / 0 |
| Distinct observed image names | 45 |
| Managed processes at the status snapshot | 82 |

The observed write rate was 19,141 bytes/s, or 65.72 MiB/hour and a projected
1.54 GiB/day of logical writes if sustained. The ring bounded disk occupancy,
but a 10 MiB file rotated approximately every 9.13 minutes and retained only
about 17 minutes of history. Every retained JSONL line parsed successfully.

## Responsiveness telemetry

Seventeen one-minute samples were retained:

| Metric | Result |
|---|---:|
| Scheduler wake p99 | 568-687 us; 595 us mean |
| Scheduler wake p95 | 535 us mean |
| Maximum single wake lateness | 4,657 us |
| Maximum DPC time | 2.50% |
| Maximum interrupt time | 1.75% |
| Maximum sampled LLC utilization | 59.81% |
| Pressure state | Normal in all samples |
| Memory-profile width | 28 physical cores throughout |

These measurements show a healthy sampled state. They do not prove that
WinSched caused the result because no paired disabled phase was run.

## Placement observations

- No managed assignment overlapped a reserved CPU Set.
- No Explorer, DWM, service-host, audio, WSL, or Hyper-V fixed-exclusion target
  appeared among managed decisions in the retained window.
- The most persistently loaded LLC averaged 34.81% utilization and had no
  managed assignment, consistent with load-aware initial placement.
- Four `vmware-vmx.exe` processes were each restricted to one LLC with six or
  eight logical processors.
- Thirteen Firefox processes were distributed across seven LLC domains.

The VMware placement can be beneficial for a small VM but can also cap a VM
that expects more than four physical cores. Guest configuration and throughput
were not measured in this baseline.

## Logging-off delta

An elevated, atomic configuration update disabled file logging without
restarting the service. The original configuration bytes were retained for the
planned ABBA restoration.

| 20-second service delta | Logging enabled | Logging disabled |
|---|---:|---:|
| One-core CPU share | 3.330% | 3.552% |
| Total 64-LP machine share | 0.05203% | 0.05550% |
| Write operations/s | 80.21 | 0.10 |
| Write bytes/s | 23,898.4 | 285.1 |

The file log remained byte-identical while disabled. The short CPU samples are
statistically indistinguishable and show that 0.5.0 still constructs and
serializes raw decisions before the disabled sink discards them. Version 0.5.1
therefore needs an early Off gate as well as Normal-level aggregation.

## Prior GUI-latency boundary

A separate WPR reproduction found the Firefox/taskbar delay while the machine
was approximately 91% idle and attributed the interval to an Explorer/Win32k
ReadyThread storm rather than global CPU, paging, disk, or WinSched tray
saturation. CPU placement benefit must therefore be established by a paired
ABBA test and must not be inferred from the healthy one-sided latency snapshot.
