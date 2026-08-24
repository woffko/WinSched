use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use winsched_config::{
    LoggingConfig, MAX_LOG_FILE_SIZE_MIB, MAX_RETAINED_LOG_ARCHIVES, MIN_LOG_FILE_SIZE_MIB,
};

pub(crate) enum EventSink {
    Console,
    Service(ServiceSink),
}

pub(crate) struct ServiceSink {
    path: PathBuf,
    applied: LoggingConfig,
    writer: Option<RotatingWriter>,
}

struct RotatingWriter {
    path: PathBuf,
    file: Option<File>,
    current_bytes: u64,
    max_bytes: u64,
    retained_archives: u8,
}

impl EventSink {
    pub(crate) const fn console() -> Self {
        Self::Console
    }

    pub(crate) fn service(path: PathBuf, config: LoggingConfig) -> io::Result<Self> {
        validate_config(config)?;
        let writer = if config.enabled {
            Some(RotatingWriter::open(&path, config)?)
        } else {
            None
        };
        Ok(Self::Service(ServiceSink {
            path,
            applied: config,
            writer,
        }))
    }

    pub(crate) fn reconfigure(&mut self, config: LoggingConfig) -> io::Result<()> {
        validate_config(config)?;
        let Self::Service(service) = self else {
            return Ok(());
        };
        if config == service.applied {
            return Ok(());
        }

        if !config.enabled {
            // Disabling is deliberately side-effect free for winsched.log*: close the active
            // handle, but do not create, append, rotate, truncate, prune, or delete any file.
            service.writer = None;
            service.applied = config;
            return Ok(());
        }

        if let Some(writer) = &mut service.writer {
            writer.reconfigure(config)?;
        } else {
            let writer = RotatingWriter::open(&service.path, config)?;
            service.writer = Some(writer);
        }
        service.applied = config;
        Ok(())
    }

    pub(crate) fn write_line(&mut self, line: &str) -> io::Result<()> {
        debug_assert!(!line.as_bytes().contains(&b'\n'));
        match self {
            Self::Console => {
                let stdout = io::stdout();
                let mut output = stdout.lock();
                writeln!(output, "{line}")?;
                output.flush()
            }
            Self::Service(service) => match &mut service.writer {
                Some(writer) => writer.write_line(line),
                None => Ok(()),
            },
        }
    }

    #[cfg(test)]
    const fn applied_logging(&self) -> Option<LoggingConfig> {
        match self {
            Self::Console => None,
            Self::Service(service) => Some(service.applied),
        }
    }
}

impl RotatingWriter {
    fn open(path: &Path, config: LoggingConfig) -> io::Result<Self> {
        Self::open_with_limits(path, config.max_file_size_bytes(), config.retained_archives)
    }

    fn open_with_limits(path: &Path, max_bytes: u64, retained_archives: u8) -> io::Result<Self> {
        prune_archives(path, retained_archives)?;
        let current_bytes = match fs::metadata(path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error),
        };
        if current_bytes > max_bytes {
            rotate_files(path, retained_archives)?;
        }
        let file = open_active(path)?;
        let current_bytes = file.metadata()?.len();
        Ok(Self {
            path: path.to_owned(),
            file: Some(file),
            current_bytes,
            max_bytes,
            retained_archives,
        })
    }

    fn reconfigure(&mut self, config: LoggingConfig) -> io::Result<()> {
        let prior_max_bytes = self.max_bytes;
        let prior_retained_archives = self.retained_archives;
        match self.reconfigure_limits(config.max_file_size_bytes(), config.retained_archives) {
            Ok(()) => Ok(()),
            Err(reconfigure_error) if self.file.is_none() => {
                match Self::open_with_limits(&self.path, prior_max_bytes, prior_retained_archives) {
                    Ok(restored) => {
                        *self = restored;
                        Err(reconfigure_error)
                    }
                    Err(recovery_error) => Err(io::Error::other(format!(
                        "log reconfiguration failed: {reconfigure_error}; restoring the prior writer failed: {recovery_error}"
                    ))),
                }
            }
            Err(error) => Err(error),
        }
    }

    fn reconfigure_limits(&mut self, max_bytes: u64, retained_archives: u8) -> io::Result<()> {
        self.current_bytes = self
            .file
            .as_ref()
            .ok_or_else(|| io::Error::other("active log handle is unavailable"))?
            .metadata()?
            .len();
        if retained_archives < self.retained_archives {
            prune_archives(&self.path, retained_archives)?;
        }
        if self.current_bytes > max_bytes {
            self.rotate(retained_archives)?;
        }
        self.max_bytes = max_bytes;
        self.retained_archives = retained_archives;
        Ok(())
    }

    fn write_line(&mut self, line: &str) -> io::Result<()> {
        let mut record = Vec::with_capacity(line.len().saturating_add(1));
        record.extend_from_slice(line.as_bytes());
        record.push(b'\n');
        let record_len = u64::try_from(record.len()).unwrap_or(u64::MAX);
        if self.current_bytes != 0 && self.current_bytes.saturating_add(record_len) > self.max_bytes
        {
            self.rotate(self.retained_archives)?;
        }
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| io::Error::other("active log handle is unavailable"))?;
        file.write_all(&record)?;
        file.flush()?;
        self.current_bytes = self.current_bytes.saturating_add(record_len);
        Ok(())
    }

    fn rotate(&mut self, retained_archives: u8) -> io::Result<()> {
        self.file = None;
        let rotation_result = rotate_files(&self.path, retained_archives);
        let reopen_result = open_active(&self.path);
        match reopen_result {
            Ok(file) => {
                self.current_bytes = file.metadata()?.len();
                self.file = Some(file);
            }
            Err(reopen_error) => {
                return Err(match rotation_result {
                    Ok(()) => reopen_error,
                    Err(rotation_error) => io::Error::other(format!(
                        "log rotation failed: {rotation_error}; reopening active log failed: {reopen_error}"
                    )),
                });
            }
        }
        rotation_result
    }
}

fn validate_config(config: LoggingConfig) -> io::Result<()> {
    if !(MIN_LOG_FILE_SIZE_MIB..=MAX_LOG_FILE_SIZE_MIB).contains(&config.max_file_size_mib) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "logging.max_file_size_mib is outside the supported range",
        ));
    }
    if config.retained_archives > MAX_RETAINED_LOG_ARCHIVES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "logging.retained_archives is outside the supported range",
        ));
    }
    Ok(())
}

fn open_active(path: &Path) -> io::Result<File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    OpenOptions::new().create(true).append(true).open(path)
}

fn rotate_files(path: &Path, retained_archives: u8) -> io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if retained_archives == 0 {
        return fs::remove_file(path);
    }

    remove_if_exists(&archive_path(path, retained_archives))?;
    for index in (1..retained_archives).rev() {
        let source = archive_path(path, index);
        if source.exists() {
            fs::rename(source, archive_path(path, index + 1))?;
        }
    }
    fs::rename(path, archive_path(path, 1))
}

fn prune_archives(path: &Path, retained_archives: u8) -> io::Result<()> {
    for index in retained_archives.saturating_add(1)..=MAX_RETAINED_LOG_ARCHIVES {
        remove_if_exists(&archive_path(path, index))?;
    }
    Ok(())
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn archive_path(path: &Path, index: u8) -> PathBuf {
    let mut value: OsString = path.as_os_str().to_owned();
    value.push(format!(".{index}"));
    PathBuf::from(value)
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(name: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock must follow the Unix epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "winsched-event-logger-{name}-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn contents(path: &Path) -> String {
        fs::read_to_string(path).unwrap()
    }

    #[test]
    fn rotates_complete_records_with_newest_archive_at_one() {
        let directory = TestDirectory::new("circular");
        let path = directory.0.join("winsched.log");
        let mut writer = RotatingWriter::open_with_limits(&path, 12, 2).unwrap();

        for line in ["aaaa", "bbbb", "cccc", "dddd", "eeee"] {
            writer.write_line(line).unwrap();
        }

        assert_eq!(contents(&path), "eeee\n");
        assert_eq!(contents(&archive_path(&path, 1)), "cccc\ndddd\n");
        assert_eq!(contents(&archive_path(&path, 2)), "aaaa\nbbbb\n");
        assert!(!archive_path(&path, 3).exists());
    }

    #[test]
    fn zero_archives_truncates_the_previous_active_log() {
        let directory = TestDirectory::new("zero-retention");
        let path = directory.0.join("winsched.log");
        let mut writer = RotatingWriter::open_with_limits(&path, 6, 0).unwrap();

        writer.write_line("one").unwrap();
        writer.write_line("two").unwrap();

        assert_eq!(contents(&path), "two\n");
        assert!(!archive_path(&path, 1).exists());
    }

    #[test]
    fn oversized_single_record_remains_whole() {
        let directory = TestDirectory::new("oversized");
        let path = directory.0.join("winsched.log");
        let mut writer = RotatingWriter::open_with_limits(&path, 5, 1).unwrap();
        let oversized = "a-record-larger-than-the-cap";

        writer.write_line(oversized).unwrap();
        assert_eq!(contents(&path), format!("{oversized}\n"));
        assert!(!archive_path(&path, 1).exists());

        writer.write_line("x").unwrap();
        assert_eq!(contents(&path), "x\n");
        assert_eq!(contents(&archive_path(&path, 1)), format!("{oversized}\n"));
    }

    #[test]
    fn lowering_limit_rotates_and_lowering_retention_prunes() {
        let directory = TestDirectory::new("reconfigure");
        let path = directory.0.join("winsched.log");
        let mut writer = RotatingWriter::open_with_limits(&path, 100, 3).unwrap();
        writer.write_line("active-record").unwrap();
        fs::write(archive_path(&path, 1), "old-one\n").unwrap();
        fs::write(archive_path(&path, 2), "old-two\n").unwrap();
        fs::write(archive_path(&path, 3), "old-three\n").unwrap();

        writer.reconfigure_limits(5, 1).unwrap();

        assert_eq!(contents(&path), "");
        assert_eq!(contents(&archive_path(&path, 1)), "active-record\n");
        assert!(!archive_path(&path, 2).exists());
        assert!(!archive_path(&path, 3).exists());
    }

    #[test]
    fn disabled_service_sink_never_touches_log_files() {
        let directory = TestDirectory::new("disabled");
        let path = directory.0.join("winsched.log");
        let archive = archive_path(&path, 1);
        fs::write(&path, "active-before\n").unwrap();
        fs::write(&archive, "archive-before\n").unwrap();
        let disabled = LoggingConfig {
            enabled: false,
            max_file_size_mib: 1,
            retained_archives: 0,
        };

        let mut sink = EventSink::service(path.clone(), disabled).unwrap();
        sink.write_line("ignored").unwrap();
        sink.reconfigure(LoggingConfig {
            max_file_size_mib: 2,
            retained_archives: 5,
            ..disabled
        })
        .unwrap();

        assert_eq!(contents(&path), "active-before\n");
        assert_eq!(contents(&archive), "archive-before\n");

        let absent_path = directory.0.join("absent.log");
        let mut absent_sink = EventSink::service(absent_path.clone(), disabled).unwrap();
        absent_sink.write_line("ignored").unwrap();
        assert!(!absent_path.exists());
        assert!(!archive_path(&absent_path, 1).exists());
    }

    #[test]
    fn failed_enable_keeps_the_disabled_policy_transactionally() {
        let directory = TestDirectory::new("failed-enable");
        let parent_file = directory.0.join("not-a-directory");
        fs::write(&parent_file, "blocking file").unwrap();
        let path = parent_file.join("winsched.log");
        let disabled = LoggingConfig {
            enabled: false,
            max_file_size_mib: 1,
            retained_archives: 0,
        };
        let mut sink = EventSink::service(path, disabled).unwrap();

        assert!(
            sink.reconfigure(LoggingConfig {
                enabled: true,
                ..disabled
            })
            .is_err()
        );
        assert_eq!(sink.applied_logging(), Some(disabled));
    }

    #[test]
    fn failed_reconfigure_recovers_the_prior_writer_when_the_path_is_available() {
        let directory = TestDirectory::new("reconfigure-recovery");
        let path = directory.0.join("winsched.log");
        let mut writer = RotatingWriter::open_with_limits(&path, 100, 2).unwrap();
        writer.write_line("before").unwrap();

        writer.file = None;
        assert!(
            writer
                .reconfigure(LoggingConfig {
                    enabled: true,
                    max_file_size_mib: 1,
                    retained_archives: 1,
                })
                .is_err()
        );
        writer.write_line("after").unwrap();
        assert_eq!(contents(&path), "before\nafter\n");
        assert_eq!(writer.max_bytes, 100);
        assert_eq!(writer.retained_archives, 2);
    }

    #[test]
    fn console_sink_is_not_disabled_by_file_logging_policy() {
        let mut sink = EventSink::console();
        sink.reconfigure(LoggingConfig {
            enabled: false,
            max_file_size_mib: 1,
            retained_archives: 0,
        })
        .unwrap();

        assert_eq!(sink.applied_logging(), None);
        assert!(matches!(sink, EventSink::Console));
    }
}
