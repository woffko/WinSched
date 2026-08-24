# WinSched 0.2.0

WinSched 0.2.0 adds configurable, bounded service logging and a dedicated
Logging page to the graphical Settings application.

## Highlights

- Detailed `winsched.log` output can be enabled or disabled without restarting
  the service.
- The active log limit is configurable from 1 to 100 MiB.
- Circular retention is configurable from 0 to 10 archives. Archive `.1` is
  the newest; zero archives reuses only the active file.
- Rotation preserves complete JSONL records. An individual oversized record is
  never split.
- Disabled logging is side-effect free for `winsched.log*`: existing files are
  preserved and the service does not create, append, rotate, truncate, prune,
  or delete them.
- Settings reports Apply success through a durable `status.json` receipt, so
  confirmation no longer depends on the diagnostic log being enabled.
- Reload receipts survive a missing prior status and a service restart, include
  a dedicated rejection reason, and match the complete saved configuration.
- A failed transient reload remains retryable from Settings.
- Existing schema-1 configuration files remain valid. WinSched applies logging
  defaults in memory, preserves the original file during upgrade, and writes
  schema 2 on the next Settings save.

## Defaults

```toml
[logging]
enabled = true
max_file_size_mib = 10
retained_archives = 1
```

Critical startup and logging failures remain independent of the optional
diagnostic stream and can be written to `winsched-emergency.log`.

## Compatibility

- Windows 11 x64, build 22000 or newer
- In-place upgrade from WinSched 0.1.0
- Existing configuration, comments, tray autostart choice, and desktop shortcut
  choice are preserved by the graphical installer

The development binaries and installer are unsigned and can trigger Windows
SmartScreen.
