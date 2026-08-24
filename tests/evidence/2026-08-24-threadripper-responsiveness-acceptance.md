# WinSched 0.3.0 Threadripper responsiveness acceptance

Date: 2026-08-24
Physical target: AMD Ryzen Threadripper 3970X, Windows 11 x64
Installer VM: Windows 11 x64 (`Microsoft Windows NT 10.0.26340.0`)
Release status: PASS

## Exact artifacts

- GUI installer: `WinSched-0.3.0-Setup-x64.exe`
  - bytes: `6809334`
  - SHA-256: `5de36e57a35aefa37ddd25e3d6f61a7f31812ab2b2ebf428eb13ec78a0489a0a`
  - Authenticode: unsigned development release
  - compiler: Inno Setup 7.1.0 x64, non-commercial edition
- Scripted ZIP: `WinSched-0.3.0-windows-x64.zip`
  - bytes: `5653491`
  - SHA-256: `145e153da1981cad6751cca2006c5566bdfc9fa8acb45a048e030621ae9c17bf`
- Frozen Windows executables:
  - `winsched.exe`: `a16929d8968a8eebb9e91d1ea15d01e9d49e4a00e0110aa8bb5655dd5e7bad1d`
  - `winsched-service.exe`: `1a9b95b537d536e59983875ce32f49f1066ce65ab52f643ab911bc5073fc264b`
  - `winsched-tray.exe`: `f6c18a38742ea5287b9cf8c487f7d5dde6bb60ae95403b2551669581cf564508`
  - `winsched-settings.exe`: `941455fdcc62d87ab2bd01185ec9ca173fde3f0460470b0d578394475f98bdd9`

The final GUI upgrade acceptance verified that every installed executable was
byte-identical to this frozen payload and that the existing configuration was
preserved byte-for-byte.

## Physical Threadripper topology and rollback

The final Windows binaries observed the physical host directly:

- 32 physical cores and 64 logical processors
- one processor group and one NUMA node
- eight LLC/CCX domains
- four physical cores and eight CPU Sets per LLC domain
- four reserved physical cores and eight reserved SMT CPU Sets at 10 percent
- memory profile: 28 physical cores and 28 CPU Sets, one sibling per core
- compute profile: 28 physical cores and 56 CPU Sets, both SMT siblings

The isolated host acceptance applied the exact memory and compute partitions to
a uniquely named process, compared every CPU Set ID with the previewed plan,
then verified complete rollback. The pre-existing installed service was stopped
only for the isolated test and restored to Running/Automatic afterward.

## Representative memory-contention performance gate

`tests/windows/threadripper-performance-acceptance.ps1` compiled a dependency-
free C# helper and ran six phases in `A-B-B-A-A-B` order. Each phase used:

- 48 normal-priority worker threads, below full 64-logical-CPU saturation
- 1 GiB of private per-worker buffers in aggregate
- random 64-bit read-modify-write operations, reported as operations per second
- five seconds of warm-up and 20 seconds of measurement
- a separate normal-priority high-resolution waitable-timer probe
- the same eight reserve CPU Sets for the probe in both modes
- `Observe` for baseline and `Auto` for managed, keeping controller overhead
  symmetric while changing only the workload CPU Set assignment

The harness verified that baseline workload CPU Sets were empty, the managed
workload had the exact 28-ID memory partition, the probe had the exact eight-ID
reserve partition in both modes, every controller stopped successfully, and
all assignments were released before the installed service was restored.

Median results:

- p99 scheduler wake lateness: `5858.3 us` baseline, `980.3 us` managed
- p99 improvement: `83.2665%` (required: at least `20%`)
- useful throughput: `543.814493 Mops/s` baseline, `625.956536 Mops/s` managed
- throughput delta: `+15.1048%` (required loss: no more than `5%`)
- throughput range: `1.0977%` baseline and `5.1526%` managed (limit: `10%`)
- baseline workload CPU use: `74.14%` of total logical CPU capacity
- managed workload CPU use: `43.30%` to `43.39%`

The throughput metric is synthetic random-memory operations, not measured DRAM
bandwidth. AMD uProf was not installed and is not a runtime dependency. The
complete structured result is
`tests/evidence/runtime/threadripper-performance-result.json`.

## Windows VM release acceptance

The final frozen payload passed the Windows VM gates:

- all Windows-native service tests: 32 PASS
- exact GUI Setup upgrade, installed-file hashes, and byte-identical config
- Automatic LocalSystem service registration and SCM crash recovery
- workload profiles, reserve exclusion, adaptive LLC move, disable/enable,
  invalid-config fail-close, graceful stop, and CPU Set rollback
- schema-1 lifecycle compatibility, preserve and purge uninstall paths
- logging enable/disable, size/retention configuration, and circular rotation
- English/Russian Settings Responsiveness UI and per-rule workload profiles
- focused tray UI Automation with reserve, latency/DPC, memory width, and mode

The final tray text on the two-vCPU VM was:

- `System reserve: 1 core / 1 thread`
- `Latency: p99 2116 us / DPC 1.52% / memory 1 core (elevated)`
- `Mode: Auto`

The VM was left with the final service Running/Automatic from
`C:\Program Files\WinSched`, LocalSystem account, and the final tray running in
the interactive user session.

## Scope and remaining production gate

- The release is intentionally unsigned. Windows SmartScreen may warn until a
  production Authenticode certificate is supplied.
- Copy Setup to a local Windows drive before elevation; direct launch from a
  WSL UNC path can fail because the elevated token may not retain the network
  provider.
- No production signing credential or public release was used during this
  acceptance.
