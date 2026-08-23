# WinSched 0.1.0 GUI release acceptance

Date: 2026-08-23
Target: Windows 11 x64 (`Microsoft Windows NT 10.0.26340.0`)
Test machine topology: two LLC domains
Release status: PASS

## Exact artifacts

- GUI installer: `WinSched-0.1.0-Setup-x64.exe`
  - bytes: `6706787`
  - SHA-256: `a63dbad5bfe9bdd36cf03d613ef509f92a26d21f6081bf16bc3352300d9e1367`
  - Authenticode: unsigned development release
  - compiler: Inno Setup 7.1.0 x64, non-commercial edition
- Scripted ZIP: `WinSched-0.1.0-windows-x64.zip`
  - bytes: `5423254`
  - SHA-256: `92f26c39108c7e7bd08b4605a9b278d354c1b7d9b0759593a8e8a3ba84d74b87`
- Frozen Windows executables:
  - `winsched.exe`: `d6939b899eeab02198cbd3a6aaccbf1981c485fd9fdf5714e30f18782512bc65`
  - `winsched-service.exe`: `6993e8627c5290f3a85ed2620b413b862118b567b1a4a5860524e1b5ef749adf`
  - `winsched-tray.exe`: `7536299e76c8594d6736ff6d686e1f3cfd5d396cb3491b7c7014c3408eac6e2a`
  - `winsched-settings.exe`: `3609b2e1fe5e83d4b2cd6882313573e99dc621dae2cd05463e23e8a3712e90ad`

The portable installer checksum, ZIP checksum, and every entry in the frozen
payload `SHA256SUMS` were verified locally. The four files installed by the GUI
wizard were byte-identical to the frozen payload.

## Build and static gates

- Rust toolchain: stable 1.95, edition 2024.
- `cargo fmt --all -- --check`: PASS.
- Native workspace tests: 57 PASS.
- Native workspace Clippy with `-D warnings`: PASS.
- Windows MSVC-target workspace Clippy with `-D warnings`: PASS.
- Windows x64 release build: PASS.
- RustSec audit: PASS for the 383-package lockfile. The initially resolved
  vulnerable `quick-xml 0.38.4` chain was removed by updating `zbus_xml` and
  related lockfile packages.
- Windows PowerShell 5.1 parser: 17 installer and acceptance scripts PASS.
- Inno Setup compiler verification and compile: PASS.
- LSP MCP was enrolled but misclassified Rust files as plaintext in the final
  session. No LSP semantic claim is made for that session; native and Windows
  target builds/Clippy are the authoritative type gates.

## Exact GUI installer acceptance

The final Setup SHA above was installed from a clean machine state through the
interactive English wizard. The acceptance verified:

- Welcome, license, tasks, ready, install, and finish pages.
- Branded CPU images on the large and small wizard surfaces.
- Startup selected by default and desktop shortcut unselected by default.
- LocalSystem, Automatic, Running service from `C:\Program Files\WinSched`.
- Configuration and runtime data under `C:\ProgramData\WinSched`.
- Settings and Startup shortcuts present; desktop shortcut absent.
- Installed hashes equal to the frozen payload.

Window-only evidence is under `docs/screenshots/installer-*.png`. The structured
result is `tests/evidence/runtime/gui-installer-result.json`.

## Settings GUI acceptance

The installed settings binary passed native unit tests and a live interactive
AccessKit/UI Automation acceptance:

- English and Russian interfaces.
- General, Adaptive, and Process rules pages.
- Readable full-width Adaptive rows at the minimum supported window size.
- Controller mode, all policy values, explicit rules, and Strict-domain fields.
- Machine-wide tray autostart control.
- Single-instance enforcement.
- Two-step Restore defaults confirmation.
- Atomic Apply, durable `config_reloaded` JSONL confirmation, Reload, Cancel,
  unsaved-close confirmation, and Close.
- Exact restoration of the original configuration after the test.

The successful Apply banner and EN/RU pages are stored under
`docs/screenshots/settings-*.png`. The structured result is
`tests/evidence/runtime/settings-ui-result.json`.

## Tray and service runtime acceptance

The final frozen payload passed the combined service, adaptive, lifecycle, and
tray suite:

- Automatic LocalSystem service registration, restricted interactive control
  ACL, failure actions, crash restart, and status recovery.
- Session 0 and infrastructure process exclusions.
- Real interactive CPU burner assignment and a load-driven adaptive LLC move.
- Disable cleanup, invalid-config fail-close, persisted runtime state, and
  restart recovery.
- Default and Startup tray launches at medium integrity RID `0x2000`.
- One tray instance, enlarged CPU icon, informative status fields, and working
  Enable/Disable, Start/Stop, Settings, advanced config, logs, refresh, and exit
  actions.

The combined console evidence is
`tests/evidence/runtime/frozen-payload-final-acceptance.log`; the safe structured
tray result is `tests/evidence/runtime/tray-ui-result.json`.

## Upgrade and rollback acceptance

- Exact final Setup self-upgrade: PASS.
- Existing TOML, comments, and marker preserved byte-for-byte.
- Startup choice and Settings shortcut preserved.
- Installed files remained equal to the frozen payload.
- Fault injection after the existing SCM entry had been changed returned
  nonzero and restored all captured state:
  - ImagePath
  - start mode
  - account
  - display name
  - running state
  - description
  - failure actions and non-crash flag
  - SDDL
- The restored service returned to Running and neither service nor config bytes
  changed.

Structured evidence:

- `tests/evidence/runtime/gui-upgrade-result.json`
- `tests/evidence/runtime/provision-rollback-result.json`

## Uninstall acceptance

The interactive uninstaller displayed the explicit data-removal prompt with
**No** focused by default. The prompt screenshot is
`docs/screenshots/uninstaller-data-prompt.png`.

The styled VCL message box did not accept synthetic cross-process button input
from the scheduled automation session. Product semantics were therefore
verified independently with the exact same final uninstaller:

- Without `/PURGEDATA`: exit 0; service, Program Files, registry entry, Startup,
  and Start Menu removed; ProgramData and config bytes preserved exactly.
- With `/PURGEDATA`: exit 0; all of the above plus ProgramData removed.
- Known legacy ZIP Start Menu shortcuts are removed by exact filename; no
  wildcard or recursive Program Files deletion is used.

Structured evidence:

- `tests/evidence/runtime/silent-uninstall-preserve-result.json`
- `tests/evidence/runtime/silent-uninstall-purge-result.json`

## Distribution notes

- The release is intentionally unsigned. Windows SmartScreen may warn until a
  production Authenticode certificate is supplied.
- Copy Setup to a local Windows drive before launch. Elevating directly from a
  WSL UNC path can fail with `ShellExecuteEx code 67` because the elevated token
  does not retain the WSL network provider.
- No production signing or public CI signing secret was used or stored.
