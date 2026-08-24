# WinSched 0.3.1

WinSched 0.3.1 adds discoverable product information and contextual Settings
help while preserving the validated 0.3.0 scheduling and rollback behavior.

## Tray About and repository access

- `About WinSched...` opens a native compatibility-safe dialog.
- The dialog shows the installed `0.3.1` version, project description, MIT
  license, and `https://github.com/woffko/WinSched`.
- `GitHub Repository` is a separate enabled action that opens the project page
  in the default browser.
- The tray continues to avoid a static `TaskDialogIndirect` import. This keeps
  the compatibility fix for the validated Windows environment.

## Contextual Settings help

Important Settings labels and controls now expose English/Russian hover help
across all pages:

- General: controller mode, sampling interval, process-activity threshold,
  implicit process scope, tray autostart, default rule mode, and workload
  profile.
- Adaptive: overload, improvement, stability, residency, cooldown, and
  mutation-rate safeguards.
- Responsiveness: physical-core reserve, latency guard, thresholds, stability,
  SMT policy, Memory-profile bounds, and resize cooldown.
- Process rules: executable matching, placement modes, workload profiles, and
  strict processor-group/LLC values.
- Logging: enable/disable semantics, active-file limit, and retained circular
  archives.

The help text explains units, scope, safety behavior, and tuning consequences;
it does not silently change any value.

## Validation summary

| Gate | Result | Evidence |
|---|---:|---|
| Rust workspace tests | 86 PASS | Unit and all-target tests |
| Native Clippy | PASS | `-D warnings` |
| Windows MSVC Clippy | PASS | `cargo xwin`, `-D warnings` |
| RustSec | PASS | 383 dependencies |
| Windows release build | PASS | Four x64 executables |
| Tray compatibility import | PASS | `TaskDialogIndirect` absent |
| PowerShell parser | PASS | 22 scripts |
| Setup upgrade | PASS | Config preserved byte-for-byte |
| Installed service | PASS | Running, Automatic, LocalSystem |
| About dialog | PASS | Version 0.3.1 and GitHub URL visible |
| GitHub tray action | PASS | Enabled and UIA-invokable |
| Settings tooltip smoke | PASS | Four rendered tooltips; config unchanged |
| Threadripper reserve/rollback | PASS | 4 reserved cores; exact cleanup |
| Threadripper performance | PASS | p99 -83.27%; throughput +15.10% |

Detailed environments, commands, example JSON, and interpretation boundaries
are documented in `docs/testing.md`. The focused 0.3.1 acceptance record is
`tests/evidence/2026-08-24-about-tooltips-acceptance.md`.

## Upgrade behavior

- Setup upgrades 0.3.0 in place.
- Existing `C:\ProgramData\WinSched\winsched.toml` bytes, comments, and Startup
  task selection are preserved.
- The service is transactionally reprovisioned from
  `C:\Program Files\WinSched` and returns to Running/Automatic/LocalSystem.
- About and tooltip additions do not change configuration schema 3 or CPU Set
  ownership semantics.

## Release artifacts

The release contains:

- `WinSched-0.3.1-Setup-x64.exe`
- `WinSched-0.3.1-Setup-x64.exe.sha256`
- `WinSched-0.3.1-windows-x64.zip`
- `WinSched-0.3.1-windows-x64.zip.sha256`

| Artifact | Bytes | SHA-256 |
|---|---:|---|
| `WinSched-0.3.1-Setup-x64.exe` | 6814559 | `113aae92a162ea9989979e0559e77cd9caa34c9a2812fd26fc07d9cc1cd93f62` |
| `WinSched-0.3.1-windows-x64.zip` | 5666163 | `e893dc4537b7448b1283663c4b9da00516e2a4b13f34e8eeaf80b8846d75c81c` |

Both checksum sidecars are included as separate release assets. GitHub server
digests are checked after upload against these local values.

## Known limitations

- The release is unsigned and may trigger Windows SmartScreen.
- Copy Setup to a local Windows drive before elevation; launching directly from
  a WSL UNC path can fail when the elevated token lacks that network provider.
- The performance result is a synthetic random-memory operation benchmark, not
  measured DRAM bandwidth.
