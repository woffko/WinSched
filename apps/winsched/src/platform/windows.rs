#![allow(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::Path;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering as AtomicOrdering},
    mpsc,
};
use std::thread::JoinHandle;
use std::time::Duration;
#[cfg(test)]
use std::time::Instant;

use thiserror::Error;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_INVALID_PARAMETER, ERROR_IO_PENDING, ERROR_NO_MORE_FILES,
    ERROR_PIPE_CONNECTED, FILETIME, HANDLE, HLOCAL, HWND, LPARAM, LocalFree, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows::Win32::Media::Audio::{
    AudioSessionStateActive, DEVICE_STATE_ACTIVE, IAudioSessionControl2, IAudioSessionManager2,
    IMMDeviceEnumerator, MMDeviceEnumerator, eAll,
};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, MOVEFILE_REPLACE_EXISTING,
    MOVEFILE_WRITE_THROUGH, MoveFileExW, PIPE_ACCESS_INBOUND, ReadFile,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Memory::{
    CreateMemoryResourceNotification, HighMemoryResourceNotification,
    LowMemoryResourceNotification, QueryMemoryResourceNotification,
};
use windows::Win32::System::Performance::{
    PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT_COUNTERVALUE, PDH_FMT_DOUBLE,
    PDH_HCOUNTER, PDH_HQUERY, PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData,
    PdhGetFormattedCounterValue, PdhOpenQueryW,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId,
    GetNamedPipeClientSessionId, PIPE_READMODE_MESSAGE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_MESSAGE,
};
use windows::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::SystemInformation::{
    CpuSetInformation, GetSystemCpuSetInformation, GetSystemTimePreciseAsFileTime,
    GlobalMemoryStatusEx, MEMORYSTATUSEX, SYSTEM_CPU_SET_INFORMATION,
    SYSTEM_CPU_SET_INFORMATION_ALLOCATED, SYSTEM_CPU_SET_INFORMATION_ALLOCATED_TO_TARGET_PROCESS,
    SYSTEM_CPU_SET_INFORMATION_PARKED, SYSTEM_CPU_SET_INFORMATION_REALTIME,
};
use windows::Win32::System::Threading::{
    CREATE_SUSPENDED, CreateEventW, CreateProcessW, GetCurrentProcess, GetPriorityClass,
    GetProcessDefaultCpuSets, GetProcessInformation, GetProcessTimes, INFINITE, MEMORY_PRIORITY,
    MEMORY_PRIORITY_BELOW_NORMAL, MEMORY_PRIORITY_INFORMATION, MEMORY_PRIORITY_LOW,
    MEMORY_PRIORITY_MEDIUM, MEMORY_PRIORITY_NORMAL, MEMORY_PRIORITY_VERY_LOW, OpenProcess,
    PROCESS_INFORMATION, PROCESS_NAME_WIN32, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
    PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_STATE,
    PROCESS_PROTECTION_LEVEL_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SET_INFORMATION, PROCESS_SET_LIMITED_INFORMATION, PROTECTION_LEVEL_NONE,
    ProcessMemoryPriority, ProcessPowerThrottling, ProcessProtectionLevelInfo,
    QueryFullProcessImageNameW, REALTIME_PRIORITY_CLASS, ResetEvent, ResumeThread, STARTUPINFOW,
    SetEvent, SetProcessDefaultCpuSets, SetProcessInformation, TerminateProcess,
    WaitForMultipleObjects,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetWindowThreadProcessId, IsWindowVisible,
};
use windows::core::{BOOL, Error as WindowsError, HRESULT, Interface, PCWSTR, PWSTR};
use winsched_control::{
    INTERACTIVE_PIPE_NAME, INTERACTIVE_STATE_SCHEMA_VERSION, InteractiveActivityState,
};
use winsched_core::{
    CpuSet, CpuSetFlags, LlcDomainKey, Topology, TopologyError,
    adaptive::{DomainLoad, ExclusionReason, ProcessKey},
};

use super::{
    EfficiencyMutationReport, InteractiveActivity, InteractiveStateWake, LaunchReport,
    MutationReport, ObservedProcess, ProcessEcoQosState, ProcessEfficiencyOwnership,
    ProcessEfficiencySnapshot, ProcessEfficiencyState, ProcessMemoryPriority, ProcessResourceUsage,
    ProcessSnapshot, SystemPressureSample, safety,
};

const RESUME_THREAD_FAILED: u32 = u32::MAX;
const INTERACTIVE_PIPE_SDDL: &str = "D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;IU)";
const INTERACTIVE_PIPE_BUFFER_BYTES: u32 = 64 * 1024;
const MAX_INTERACTIVE_PUBLISHERS: usize = 64;
static ATOMIC_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

type InteractivePublisherKey = (u32, u32, u64);
type InteractiveStateMap = BTreeMap<InteractivePublisherKey, InteractiveActivityState>;
type SharedInteractiveStates = Arc<Mutex<InteractiveStateMap>>;

#[derive(Debug, Error)]
pub enum PlatformError {
    #[error("Windows API failed: {0}")]
    Windows(#[from] WindowsError),
    #[error("{operation} failed: {source}")]
    WindowsCall {
        operation: &'static str,
        source: WindowsError,
    },
    #[error(transparent)]
    Topology(#[from] TopologyError),
    #[error("invalid CPU Set buffer: {0}")]
    InvalidCpuSetBuffer(String),
    #[error(
        "CPU Set assignment verification failed for PID {pid}: requested {requested:?}, observed {observed:?}; rollback {rollback}"
    )]
    VerificationFailed {
        pid: u32,
        requested: Vec<u32>,
        observed: Vec<u32>,
        rollback: String,
    },
    #[error("the executable path does not exist or is not a file: {0}")]
    InvalidExecutable(String),
    #[error("ResumeThread failed for PID {0}")]
    ResumeFailed(u32),
    #[error("PID {pid} creation time changed: expected {expected}, observed {observed}")]
    ProcessIdentityChanged {
        pid: u32,
        expected: u64,
        observed: u64,
    },
    #[error("process PID {0} no longer exists")]
    ProcessUnavailable(u32),
    #[error("CPU Set assignment is denied for protected target PID {pid} ({image})")]
    ProtectedMutationTarget { pid: u32, image: String },
    #[error("CPU Set assignment is denied because PID {0} could not be identified")]
    UnidentifiedMutationTarget(u32),
    #[error("background efficiency is denied for PID {pid} ({image}): {reason}")]
    UnsafeEfficiencyTarget {
        pid: u32,
        image: String,
        reason: &'static str,
    },
    #[error("{operation} failed with PDH status 0x{status:08X}")]
    PdhStatus {
        operation: &'static str,
        status: u32,
    },
    #[error("PDH counter '{path}' returned data status 0x{status:08X}")]
    PdhCounterStatus { path: String, status: u32 },
    #[error("PDH counter '{0}' returned a non-finite value")]
    PdhNonFinite(String),
    #[error("Windows reported unsupported process memory priority {0}")]
    UnsupportedMemoryPriority(u32),
    #[error("process efficiency update failed for PID {pid}: {detail}; rollback {rollback}")]
    EfficiencyUpdateFailed {
        pid: u32,
        detail: String,
        rollback: String,
    },
    #[error(
        "process efficiency ownership changed for PID {pid}: expected {expected:?}, observed {observed:?}"
    )]
    EfficiencyOwnershipChanged {
        pid: u32,
        expected: ProcessEfficiencyState,
        observed: ProcessEfficiencyState,
    },
    #[error("interactive activity pipe failed: {0}")]
    InteractivePipe(String),
}

#[derive(Debug)]
struct OwnedHandle(HANDLE);

impl OwnedHandle {
    fn new(handle: HANDLE) -> Self {
        Self(handle)
    }

    const fn raw(&self) -> HANDLE {
        self.0
    }
}

#[derive(Debug)]
pub struct MemoryPressureMonitor {
    low_notification: OwnedHandle,
    high_notification: OwnedHandle,
    low_state: Mutex<bool>,
}

pub struct InteractiveStateServer {
    states: SharedInteractiveStates,
    stop_event: OwnedHandle,
    worker: Option<JoinHandle<()>>,
}

impl InteractiveStateServer {
    pub fn start(
        expected_tray_path: &Path,
        wake: Option<InteractiveStateWake>,
    ) -> Result<Self, PlatformError> {
        let expected_tray_path = std::fs::canonicalize(expected_tray_path)
            .unwrap_or_else(|_| expected_tray_path.to_path_buf());
        let states = Arc::new(Mutex::new(BTreeMap::new()));
        // SAFETY: The unnamed manual-reset event is owned by the server and remains alive until
        // the worker has stopped.
        let stop_event =
            OwnedHandle::new(unsafe { CreateEventW(None, true, false, PCWSTR::null())? });
        let stop_event_value = stop_event.raw().0 as usize;
        let worker_states = Arc::clone(&states);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name("winsched-interactive-pipe".to_owned())
            .spawn(move || {
                let worker_stop_event = HANDLE(stop_event_value as *mut std::ffi::c_void);
                interactive_pipe_worker(
                    &expected_tray_path,
                    &worker_states,
                    worker_stop_event,
                    &ready_tx,
                    wake.as_ref(),
                );
            })
            .map_err(|error| PlatformError::InteractivePipe(error.to_string()))?;
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Self {
                states,
                stop_event,
                worker: Some(worker),
            }),
            Ok(Err(error)) => {
                let _ = worker.join();
                Err(error)
            }
            Err(_) => {
                // SAFETY: The event handle is valid and shared with the worker for cancellation.
                let _ = unsafe { SetEvent(stop_event.raw()) };
                let _ = worker.join();
                Err(PlatformError::InteractivePipe(
                    "initialization timed out".to_owned(),
                ))
            }
        }
    }

    #[must_use]
    pub fn states(&self) -> Vec<InteractiveActivityState> {
        self.states
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .cloned()
            .collect()
    }
}

impl Drop for InteractiveStateServer {
    fn drop(&mut self) {
        // SAFETY: The event remains valid until after the worker has joined.
        let _ = unsafe { SetEvent(self.stop_event.raw()) };
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct OwnedSecurityDescriptor(PSECURITY_DESCRIPTOR);

impl Drop for OwnedSecurityDescriptor {
    fn drop(&mut self) {
        if !self.0.0.is_null() {
            // SAFETY: ConvertStringSecurityDescriptor allocated this block with LocalAlloc.
            let _ = unsafe { LocalFree(Some(HLOCAL(self.0.0))) };
        }
    }
}

fn pipe_security_descriptor() -> Result<OwnedSecurityDescriptor, PlatformError> {
    let descriptor = wide_null(OsStr::new(INTERACTIVE_PIPE_SDDL));
    let mut security = PSECURITY_DESCRIPTOR::default();
    // SAFETY: descriptor is NUL-terminated and security is a writable output value.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(descriptor.as_ptr()),
            SDDL_REVISION_1,
            &raw mut security,
            None,
        )?;
    }
    Ok(OwnedSecurityDescriptor(security))
}

fn create_interactive_pipe(
    security: &OwnedSecurityDescriptor,
) -> Result<OwnedHandle, PlatformError> {
    let name = wide_null(OsStr::new(INTERACTIVE_PIPE_NAME));
    let attributes = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>())
            .expect("security attributes size fits u32"),
        lpSecurityDescriptor: security.0.0,
        bInheritHandle: false.into(),
    };
    // SAFETY: Name and security descriptor remain valid for the complete call.
    let pipe = unsafe {
        CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
            PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            0,
            INTERACTIVE_PIPE_BUFFER_BYTES,
            1_000,
            Some(&raw const attributes),
        )
    };
    if pipe.is_invalid() {
        Err(WindowsError::from_thread().into())
    } else {
        Ok(OwnedHandle::new(pipe))
    }
}

fn interactive_pipe_worker(
    expected_tray_path: &Path,
    states: &SharedInteractiveStates,
    stop_event: HANDLE,
    ready: &mpsc::SyncSender<Result<(), PlatformError>>,
    wake: Option<&InteractiveStateWake>,
) {
    let security = match pipe_security_descriptor() {
        Ok(security) => security,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let pipe = match create_interactive_pipe(&security) {
        Ok(pipe) => pipe,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    // SAFETY: The unnamed manual-reset event is uniquely owned by this worker.
    let io_event = match unsafe { CreateEventW(None, true, false, PCWSTR::null()) } {
        Ok(event) => OwnedHandle::new(event),
        Err(error) => {
            let _ = ready.send(Err(error.into()));
            return;
        }
    };
    let _ = ready.send(Ok(()));

    while let PipeConnectResult::Connected =
        wait_for_pipe_client(pipe.raw(), io_event.raw(), stop_event)
    {
        let accepted = read_interactive_pipe_message(
            pipe.raw(),
            io_event.raw(),
            stop_event,
            expected_tray_path,
            states,
        );
        if accepted && let Some(wake) = wake {
            wake();
        }
        // SAFETY: The handle is a connected named-pipe server instance.
        let _ = unsafe { DisconnectNamedPipe(pipe.raw()) };
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PipeConnectResult {
    Connected,
    StopRequested,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OverlappedWaitResult {
    Completed(u32),
    StopRequested,
    TimedOut,
    Failed,
}

fn wait_for_pipe_client(pipe: HANDLE, io_event: HANDLE, stop_event: HANDLE) -> PipeConnectResult {
    // SAFETY: io_event is a valid manual-reset event used only by the worker thread.
    if unsafe { ResetEvent(io_event) }.is_err() {
        return PipeConnectResult::Failed;
    }
    let mut overlapped = OVERLAPPED {
        hEvent: io_event,
        ..Default::default()
    };
    // SAFETY: pipe was opened for overlapped I/O and overlapped remains alive until completion.
    match unsafe { ConnectNamedPipe(pipe, Some(&raw mut overlapped)) } {
        Ok(()) => PipeConnectResult::Connected,
        Err(error) if error.code() == HRESULT::from_win32(ERROR_PIPE_CONNECTED.0) => {
            PipeConnectResult::Connected
        }
        Err(error) if error.code() == HRESULT::from_win32(ERROR_IO_PENDING.0) => {
            match wait_for_pipe_io(pipe, &overlapped, stop_event, INFINITE) {
                OverlappedWaitResult::Completed(_) => PipeConnectResult::Connected,
                OverlappedWaitResult::StopRequested => PipeConnectResult::StopRequested,
                OverlappedWaitResult::TimedOut | OverlappedWaitResult::Failed => {
                    PipeConnectResult::Failed
                }
            }
        }
        Err(_) => PipeConnectResult::Failed,
    }
}

fn wait_for_pipe_io(
    pipe: HANDLE,
    overlapped: &OVERLAPPED,
    stop_event: HANDLE,
    timeout_ms: u32,
) -> OverlappedWaitResult {
    // SAFETY: Both handles remain valid during the wait and the array is borrowed for the call.
    let wait =
        unsafe { WaitForMultipleObjects(&[stop_event, overlapped.hEvent], false, timeout_ms) };
    if wait == WAIT_OBJECT_0 {
        cancel_and_drain_pipe_io(pipe, overlapped);
        return OverlappedWaitResult::StopRequested;
    }
    if wait.0 == WAIT_OBJECT_0.0 + 1 {
        let mut bytes_transferred = 0u32;
        // SAFETY: The event signaled completion and the OVERLAPPED storage is still valid.
        return if unsafe {
            GetOverlappedResult(pipe, overlapped, &raw mut bytes_transferred, false)
        }
        .is_ok()
        {
            OverlappedWaitResult::Completed(bytes_transferred)
        } else {
            OverlappedWaitResult::Failed
        };
    }
    if wait == WAIT_TIMEOUT {
        cancel_and_drain_pipe_io(pipe, overlapped);
        return OverlappedWaitResult::TimedOut;
    }
    cancel_and_drain_pipe_io(pipe, overlapped);
    OverlappedWaitResult::Failed
}

fn cancel_and_drain_pipe_io(pipe: HANDLE, overlapped: &OVERLAPPED) {
    // SAFETY: The OVERLAPPED belongs to an operation on pipe. Waiting for its terminal state
    // keeps both the stack storage and any associated buffer alive through cancellation.
    let _ = unsafe { CancelIoEx(pipe, Some(overlapped)) };
    let mut ignored = 0u32;
    // SAFETY: The operation has either completed or cancellation was requested; this drains it.
    let _ = unsafe { GetOverlappedResult(pipe, overlapped, &raw mut ignored, true) };
}

fn read_interactive_pipe_message(
    pipe: HANDLE,
    io_event: HANDLE,
    stop_event: HANDLE,
    expected_tray_path: &Path,
    states: &SharedInteractiveStates,
) -> bool {
    let mut client_pid = 0u32;
    let mut client_session_id = 0u32;
    // SAFETY: Both output pointers are writable and pipe is connected.
    if unsafe { GetNamedPipeClientProcessId(pipe, &raw mut client_pid) }.is_err()
        || unsafe { GetNamedPipeClientSessionId(pipe, &raw mut client_session_id) }.is_err()
        || client_pid == 0
        || client_session_id == 0
    {
        return false;
    }
    let Ok(client) = open_process(client_pid, false) else {
        return false;
    };
    let Ok(times) = query_process_times(client.raw()) else {
        return false;
    };
    let Some(client_path) = process_image_path(client.raw()) else {
        return false;
    };
    let client_path = std::fs::canonicalize(&client_path).unwrap_or_else(|_| client_path.into());
    if !client_path
        .to_string_lossy()
        .eq_ignore_ascii_case(&expected_tray_path.to_string_lossy())
    {
        return false;
    }

    let Some(buffer) = read_interactive_pipe_payload(pipe, io_event, stop_event) else {
        return false;
    };
    let Ok(mut state) = serde_json::from_slice::<InteractiveActivityState>(&buffer) else {
        return false;
    };
    if state.schema_version != INTERACTIVE_STATE_SCHEMA_VERSION
        || state.source_pid != client_pid
        || state.source_creation_time_100ns != times.creation_time_100ns
        || state.session_id != client_session_id
        || state.visible_pids.len() > 4_096
        || state.audible_pids.len() > 4_096
    {
        return false;
    }
    let received_at = unix_time_ms_local();
    state.updated_at_unix_ms = received_at;
    state.visible_pids.sort_unstable();
    state.visible_pids.dedup();
    state.audible_pids.sort_unstable();
    state.audible_pids.dedup();
    let mut states = states
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    insert_interactive_state(&mut states, state, received_at);
    true
}

fn insert_interactive_state(
    states: &mut InteractiveStateMap,
    state: InteractiveActivityState,
    received_at: u64,
) {
    states.retain(|_, prior| received_at.saturating_sub(prior.updated_at_unix_ms) <= 60_000);
    let key = (
        state.session_id,
        state.source_pid,
        state.source_creation_time_100ns,
    );
    if !states.contains_key(&key) && states.len() >= MAX_INTERACTIVE_PUBLISHERS {
        let oldest = states
            .iter()
            .min_by_key(|(_, state)| state.updated_at_unix_ms)
            .map(|(key, _)| *key);
        if let Some(oldest) = oldest {
            states.remove(&oldest);
        }
    }
    states.insert(key, state);
}

fn read_interactive_pipe_payload(
    pipe: HANDLE,
    io_event: HANDLE,
    stop_event: HANDLE,
) -> Option<Vec<u8>> {
    let mut buffer = vec![0u8; INTERACTIVE_PIPE_BUFFER_BYTES as usize];
    // SAFETY: io_event is a valid manual-reset event used only by the worker thread.
    if unsafe { ResetEvent(io_event) }.is_err() {
        return None;
    }
    let mut overlapped = OVERLAPPED {
        hEvent: io_event,
        ..Default::default()
    };
    // SAFETY: The buffer and OVERLAPPED remain alive until the operation reaches a terminal state.
    let bytes_read = match unsafe {
        ReadFile(
            pipe,
            Some(buffer.as_mut_slice()),
            None,
            Some(&raw mut overlapped),
        )
    } {
        Ok(()) => {
            let mut bytes_read = 0u32;
            // SAFETY: A successful overlapped ReadFile has completed before this query.
            if unsafe {
                GetOverlappedResult(pipe, &raw const overlapped, &raw mut bytes_read, false)
            }
            .is_err()
            {
                return None;
            }
            bytes_read
        }
        Err(error) if error.code() == HRESULT::from_win32(ERROR_IO_PENDING.0) => {
            match wait_for_pipe_io(pipe, &overlapped, stop_event, 1_000) {
                OverlappedWaitResult::Completed(bytes_read) => bytes_read,
                OverlappedWaitResult::StopRequested
                | OverlappedWaitResult::TimedOut
                | OverlappedWaitResult::Failed => return None,
            }
        }
        Err(error) if error.code() == HRESULT::from_win32(ERROR_BROKEN_PIPE.0) => return None,
        Err(_) => return None,
    };
    if bytes_read == 0 || u64::from(bytes_read) > u64::from(INTERACTIVE_PIPE_BUFFER_BYTES) {
        return None;
    }
    buffer.truncate(bytes_read as usize);
    Some(buffer)
}

fn unix_time_ms_local() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

impl MemoryPressureMonitor {
    pub fn new() -> Result<Self, PlatformError> {
        // SAFETY: The call creates a new kernel notification object owned by this wrapper.
        let low_notification = OwnedHandle::new(unsafe {
            CreateMemoryResourceNotification(LowMemoryResourceNotification)?
        });
        // SAFETY: The call creates a second kernel notification object owned by this wrapper.
        let high_notification = OwnedHandle::new(unsafe {
            CreateMemoryResourceNotification(HighMemoryResourceNotification)?
        });
        Ok(Self {
            low_notification,
            high_notification,
            low_state: Mutex::new(false),
        })
    }

    pub fn is_low(&self) -> Result<bool, PlatformError> {
        let mut low = BOOL::default();
        let mut high = BOOL::default();
        // SAFETY: Both handles are live memory-resource notifications and outputs are writable.
        unsafe {
            QueryMemoryResourceNotification(self.low_notification.raw(), &raw mut low)?;
            QueryMemoryResourceNotification(self.high_notification.raw(), &raw mut high)?;
        }
        let mut state = self
            .low_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if low.as_bool() {
            *state = true;
        } else if high.as_bool() {
            *state = false;
        }
        Ok(*state)
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: The wrapper uniquely owns a valid Win32 handle.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

pub fn atomic_replace_file(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temporary = None;
    for _ in 0..32 {
        let sequence = ATOMIC_TEMP_COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
        let candidate = parent.join(format!(
            ".winsched-atomic-{}-{sequence}.tmp",
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => {
                temporary = Some((candidate, file));
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    let Some((temporary_path, mut file)) = temporary else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not allocate a unique WinSched atomic-write file",
        ));
    };
    let write_result = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temporary_path);
        return Err(error);
    }

    let temporary_wide = wide_null(temporary_path.as_os_str());
    let destination_wide = wide_null(path.as_os_str());
    // SAFETY: Both paths are NUL-terminated, remain alive for the call, and the temporary file
    // is on the destination volume. REPLACE_EXISTING avoids any destination-missing window.
    let replace = unsafe {
        MoveFileExW(
            PCWSTR(temporary_wide.as_ptr()),
            PCWSTR(destination_wide.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if let Err(error) = replace {
        let _ = fs::remove_file(temporary_path);
        return Err(std::io::Error::other(error));
    }
    Ok(())
}

#[derive(Debug)]
struct OwnedPdhQuery(PDH_HQUERY);

impl Drop for OwnedPdhQuery {
    fn drop(&mut self) {
        if !self.0.0.is_null() {
            // SAFETY: The wrapper uniquely owns a query opened by PdhOpenQueryW.
            let _ = unsafe { PdhCloseQuery(self.0) };
        }
    }
}

#[derive(Debug)]
struct ProcessorCounter {
    domain: LlcDomainKey,
    utility: PdhCounter,
    dpc: Option<PdhCounter>,
    interrupt: Option<PdhCounter>,
}

#[derive(Debug)]
struct PdhCounter {
    path: String,
    handle: PDH_HCOUNTER,
}

/// Locale-independent PDH sampler for per-CPU processor utility.
#[derive(Debug)]
pub struct LoadSampler {
    query: OwnedPdhQuery,
    counters: Vec<ProcessorCounter>,
}

#[derive(Debug)]
pub struct SystemPressureSampler {
    query: OwnedPdhQuery,
    processor_queue: PdhCounter,
    pages_input: PdhCounter,
}

impl SystemPressureSampler {
    pub fn new() -> Result<Self, PlatformError> {
        let mut query = PDH_HQUERY::default();
        // SAFETY: The output pointer is valid and the null source selects live data.
        let status = unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &raw mut query) };
        check_pdh("PdhOpenQueryW(system pressure)", status)?;
        let query = OwnedPdhQuery(query);
        let processor_queue =
            add_pdh_counter(query.0, r"\System\Processor Queue Length".to_owned())?;
        let pages_input = add_pdh_counter(query.0, r"\Memory\Pages Input/sec".to_owned())?;
        Ok(Self {
            query,
            processor_queue,
            pages_input,
        })
    }

    pub fn prime(&mut self) -> Result<(), PlatformError> {
        // SAFETY: The query is valid for the lifetime of self.
        let status = unsafe { PdhCollectQueryData(self.query.0) };
        check_pdh("PdhCollectQueryData(system pressure initial)", status)
    }

    pub fn sample(&mut self) -> Result<SystemPressureSample, PlatformError> {
        // SAFETY: The query is valid for the lifetime of self.
        let status = unsafe { PdhCollectQueryData(self.query.0) };
        check_pdh("PdhCollectQueryData(system pressure sample)", status)?;
        let mut memory = MEMORYSTATUSEX {
            dwLength: u32::try_from(size_of::<MEMORYSTATUSEX>())
                .expect("MEMORYSTATUSEX size fits u32"),
            ..Default::default()
        };
        // SAFETY: memory has the documented length and is a valid writable output structure.
        unsafe { GlobalMemoryStatusEx(&raw mut memory)? };
        Ok(SystemPressureSample {
            processor_queue_length: rounded_u32(read_pdh_double(&self.processor_queue)?),
            pages_input_per_second: rounded_u64(read_pdh_double(&self.pages_input)?),
            total_physical_memory_bytes: memory.ullTotalPhys,
            available_physical_memory_bytes: memory.ullAvailPhys,
        })
    }
}

impl LoadSampler {
    pub fn new(topology: &Topology) -> Result<Self, PlatformError> {
        let mut query = PDH_HQUERY::default();
        // SAFETY: The output pointer is valid and the null source selects live data.
        let status = unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &raw mut query) };
        check_pdh("PdhOpenQueryW", status)?;
        let query = OwnedPdhQuery(query);

        let mut counters = Vec::with_capacity(topology.cpu_sets.len());
        for cpu in &topology.cpu_sets {
            let instance = format!("{},{}", cpu.group, cpu.logical_processor_index);
            let utility_path = format!(
                r"\Processor Information({},{})\% Processor Utility",
                cpu.group, cpu.logical_processor_index
            );
            counters.push(ProcessorCounter {
                domain: LlcDomainKey {
                    group: cpu.group,
                    last_level_cache_index: cpu.last_level_cache_index,
                },
                utility: add_pdh_counter(query.0, utility_path)?,
                dpc: add_pdh_counter(
                    query.0,
                    format!(r"\Processor Information({instance})\% DPC Time"),
                )
                .ok(),
                interrupt: add_pdh_counter(
                    query.0,
                    format!(r"\Processor Information({instance})\% Interrupt Time"),
                )
                .ok(),
            });
        }

        Ok(Self { query, counters })
    }

    pub fn prime(&mut self) -> Result<(), PlatformError> {
        // SAFETY: The query is valid for the lifetime of self.
        let status = unsafe { PdhCollectQueryData(self.query.0) };
        check_pdh("PdhCollectQueryData(initial)", status)
    }

    pub fn sample(&mut self) -> Result<Vec<DomainLoad>, PlatformError> {
        // SAFETY: The query is valid for the lifetime of self.
        let status = unsafe { PdhCollectQueryData(self.query.0) };
        check_pdh("PdhCollectQueryData(sample)", status)?;

        let mut domains = BTreeMap::<LlcDomainKey, (u64, u64, u64, u64)>::new();
        for counter in &self.counters {
            let basis_points = read_pdh_counter(&counter.utility)?;
            let dpc_time_bps = counter
                .dpc
                .as_ref()
                .and_then(|counter| read_pdh_counter(counter).ok())
                .unwrap_or(0);
            let interrupt_time_bps = counter
                .interrupt
                .as_ref()
                .and_then(|counter| read_pdh_counter(counter).ok())
                .unwrap_or(0);
            let aggregate = domains.entry(counter.domain).or_default();
            aggregate.0 += u64::from(basis_points);
            aggregate.1 += u64::from(dpc_time_bps);
            aggregate.2 += u64::from(interrupt_time_bps);
            aggregate.3 += 1;
        }

        Ok(domains
            .into_iter()
            .map(|(domain, (utility, dpc, interrupt, count))| DomainLoad {
                domain,
                utilization_bps: u16::try_from(utility / count)
                    .expect("an average of clamped basis points fits u16"),
                dpc_time_bps: u16::try_from(dpc / count)
                    .expect("an average of clamped basis points fits u16"),
                interrupt_time_bps: u16::try_from(interrupt / count)
                    .expect("an average of clamped basis points fits u16"),
            })
            .collect())
    }
}

fn add_pdh_counter(query: PDH_HQUERY, path: String) -> Result<PdhCounter, PlatformError> {
    let wide_path = wide_null(OsStr::new(&path));
    let mut handle = PDH_HCOUNTER::default();
    // SAFETY: The query is live and wide_path is a valid null-terminated UTF-16 string.
    let status =
        unsafe { PdhAddEnglishCounterW(query, PCWSTR(wide_path.as_ptr()), 0, &raw mut handle) };
    check_pdh("PdhAddEnglishCounterW", status)?;
    Ok(PdhCounter { path, handle })
}

fn read_pdh_counter(counter: &PdhCounter) -> Result<u16, PlatformError> {
    Ok(utility_to_basis_points(read_pdh_double(counter)?))
}

fn read_pdh_double(counter: &PdhCounter) -> Result<f64, PlatformError> {
    let mut value = PDH_FMT_COUNTERVALUE::default();
    // SAFETY: The counter belongs to a live query and value is writable.
    let status = unsafe {
        PdhGetFormattedCounterValue(counter.handle, PDH_FMT_DOUBLE, None, &raw mut value)
    };
    check_pdh("PdhGetFormattedCounterValue", status)?;
    if !matches!(value.CStatus, PDH_CSTATUS_VALID_DATA | PDH_CSTATUS_NEW_DATA) {
        return Err(PlatformError::PdhCounterStatus {
            path: counter.path.clone(),
            status: value.CStatus,
        });
    }
    // SAFETY: PDH_FMT_DOUBLE makes doubleValue the active union member.
    let measured = unsafe { value.Anonymous.doubleValue };
    if !measured.is_finite() {
        return Err(PlatformError::PdhNonFinite(counter.path.clone()));
    }
    Ok(measured)
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn rounded_u32(value: f64) -> u32 {
    value.clamp(0.0, f64::from(u32::MAX)).round() as u32
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
fn rounded_u64(value: f64) -> u64 {
    value.clamp(0.0, u64::MAX as f64).round() as u64
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn utility_to_basis_points(utility: f64) -> u16 {
    // The caller rejects non-finite values. Clamping proves the rounded value
    // is in the inclusive 0..=10_000 range before this conversion.
    (utility.clamp(0.0, 100.0) * 100.0).round() as u16
}

fn check_pdh(operation: &'static str, status: u32) -> Result<(), PlatformError> {
    if status == 0 {
        Ok(())
    } else {
        Err(PlatformError::PdhStatus { operation, status })
    }
}

pub fn system_topology() -> Result<Topology, PlatformError> {
    topology_for_process(None)
}

pub fn inspect_process(pid: u32) -> Result<ProcessSnapshot, PlatformError> {
    let process = open_process(pid, false)?;
    let topology = topology_for_process(Some(process.raw()))?;
    let default_cpu_set_ids = get_process_default_cpu_sets(process.raw())?;
    Ok(ProcessSnapshot {
        pid,
        default_cpu_set_ids,
        topology,
    })
}

pub fn inspect_process_efficiency(pid: u32) -> Result<ProcessEfficiencySnapshot, PlatformError> {
    let process = open_efficiency_process(pid, false)?;
    let key = ProcessKey {
        pid,
        creation_time_100ns: query_process_times(process.raw())?.creation_time_100ns,
    };
    Ok(ProcessEfficiencySnapshot {
        key,
        state: query_process_efficiency(process.raw())?,
    })
}

pub fn observe_processes(topology: &Topology) -> Result<Vec<ObservedProcess>, PlatformError> {
    // SAFETY: The flags request a read-only process snapshot and PID 0 is ignored for this mode.
    let snapshot = OwnedHandle::new(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)? });
    let mut entry = PROCESSENTRY32W {
        dwSize: u32::try_from(size_of::<PROCESSENTRY32W>()).expect("PROCESSENTRY32W size fits u32"),
        ..Default::default()
    };
    // SAFETY: entry has the required size and remains writable during enumeration.
    unsafe { Process32FirstW(snapshot.raw(), &raw mut entry)? };

    let mut processes = Vec::new();
    loop {
        if entry.th32ProcessID != 0 {
            processes.push(observe_process_entry(topology, &entry));
        }
        // SAFETY: snapshot and entry remain valid until enumeration ends.
        match unsafe { Process32NextW(snapshot.raw(), &raw mut entry) } {
            Ok(()) => {}
            Err(error) if error.code() == HRESULT::from_win32(ERROR_NO_MORE_FILES.0) => {
                break;
            }
            Err(error) => return Err(error.into()),
        }
    }
    processes.sort_by_key(|process| process.key);
    Ok(processes)
}

fn observe_process_entry(topology: &Topology, entry: &PROCESSENTRY32W) -> ObservedProcess {
    let pid = entry.th32ProcessID;
    let image_name = fixed_wide_string(&entry.szExeFile);
    let session_id = process_session_id(pid);
    let initial_exclusion = if safety::is_fixed_system_process(pid, &image_name) {
        Some(ExclusionReason::SystemProcess)
    } else if session_id == Some(0) {
        Some(ExclusionReason::SessionZero)
    } else if session_id.is_none() {
        Some(ExclusionReason::ProtectedProcess)
    } else {
        None
    };

    let Ok(process) = open_process(pid, false) else {
        return ObservedProcess {
            key: ProcessKey {
                pid,
                creation_time_100ns: 0,
            },
            parent_pid: entry.th32ParentProcessID,
            session_id,
            thread_count: entry.cntThreads,
            image_name,
            image_path: None,
            priority_class: None,
            cpu_time_100ns: 0,
            default_cpu_set_ids: Vec::new(),
            current_domain: None,
            exclusion: initial_exclusion.or(Some(ExclusionReason::ProtectedProcess)),
        };
    };

    let mut exclusion = initial_exclusion;
    let times = query_process_times(process.raw()).unwrap_or_else(|_| {
        exclusion.get_or_insert(ExclusionReason::ProtectedProcess);
        ProcessTimes::default()
    });
    let priority_class = process_priority_class(process.raw());
    if priority_class == Some(REALTIME_PRIORITY_CLASS.0) {
        exclusion.get_or_insert(ExclusionReason::RealtimeProcess);
    }
    if process_is_protected(process.raw()).unwrap_or(true) {
        exclusion.get_or_insert(ExclusionReason::ProtectedProcess);
    }
    let default_cpu_set_ids = get_process_default_cpu_sets(process.raw()).unwrap_or_else(|_| {
        exclusion.get_or_insert(ExclusionReason::ProtectedProcess);
        Vec::new()
    });

    ObservedProcess {
        key: ProcessKey {
            pid,
            creation_time_100ns: times.creation_time_100ns,
        },
        parent_pid: entry.th32ParentProcessID,
        session_id,
        thread_count: entry.cntThreads,
        image_name,
        image_path: process_image_path(process.raw()),
        priority_class,
        cpu_time_100ns: times.cpu_time_100ns,
        current_domain: topology.domain_for_cpu_set_ids(&default_cpu_set_ids),
        default_cpu_set_ids,
        exclusion,
    }
}

fn process_session_id(pid: u32) -> Option<u32> {
    let mut session_id = 0u32;
    // SAFETY: session_id is a valid writable output pointer for this PID query.
    unsafe { ProcessIdToSessionId(pid, &raw mut session_id).ok()? };
    Some(session_id)
}

#[derive(Debug, Clone, Copy, Default)]
struct ProcessTimes {
    creation_time_100ns: u64,
    cpu_time_100ns: u64,
}

fn query_process_times(process: HANDLE) -> Result<ProcessTimes, WindowsError> {
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: All output pointers are valid and the process handle has query access.
    unsafe {
        GetProcessTimes(
            process,
            &raw mut creation,
            &raw mut exit,
            &raw mut kernel,
            &raw mut user,
        )?;
    }
    Ok(ProcessTimes {
        creation_time_100ns: filetime_value(creation),
        cpu_time_100ns: filetime_value(kernel).saturating_add(filetime_value(user)),
    })
}

fn verify_process_identity(process: HANDLE, key: ProcessKey) -> Result<(), PlatformError> {
    let observed = query_process_times(process)?.creation_time_100ns;
    if observed == key.creation_time_100ns {
        Ok(())
    } else {
        Err(PlatformError::ProcessIdentityChanged {
            pid: key.pid,
            expected: key.creation_time_100ns,
            observed,
        })
    }
}

const fn filetime_value(value: FILETIME) -> u64 {
    (value.dwHighDateTime as u64) << 32 | value.dwLowDateTime as u64
}

fn process_priority_class(process: HANDLE) -> Option<u32> {
    // SAFETY: The process handle has query access.
    let priority = unsafe { GetPriorityClass(process) };
    (priority != 0).then_some(priority)
}

fn process_is_protected(process: HANDLE) -> Option<bool> {
    let mut information = PROCESS_PROTECTION_LEVEL_INFORMATION::default();
    // SAFETY: The typed output buffer and declared size match the requested information class.
    unsafe {
        GetProcessInformation(
            process,
            ProcessProtectionLevelInfo,
            (&raw mut information).cast(),
            u32::try_from(size_of::<PROCESS_PROTECTION_LEVEL_INFORMATION>())
                .expect("protection information size fits u32"),
        )
        .ok()?;
    }
    Some(information.ProtectionLevel != PROTECTION_LEVEL_NONE)
}

fn process_image_path(process: HANDLE) -> Option<String> {
    let mut buffer = vec![0u16; 32_768];
    let mut size = u32::try_from(buffer.len()).expect("image path buffer length fits u32");
    // SAFETY: The writable UTF-16 buffer is live and size describes its capacity.
    unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &raw mut size,
        )
        .ok()?;
    }
    buffer.truncate(usize::try_from(size).expect("u32 fits usize"));
    Some(OsString::from_wide(&buffer).to_string_lossy().into_owned())
}

fn fixed_wide_string(value: &[u16]) -> String {
    let length = value
        .iter()
        .position(|character| *character == 0)
        .unwrap_or(value.len());
    OsString::from_wide(&value[..length])
        .to_string_lossy()
        .into_owned()
}

pub fn apply_process(pid: u32, cpu_set_ids: &[u32]) -> Result<MutationReport, PlatformError> {
    let process = open_process(pid, true)?;
    replace_default_cpu_sets("apply", pid, process.raw(), cpu_set_ids)
}

pub fn clear_process(pid: u32) -> Result<MutationReport, PlatformError> {
    let process = open_process(pid, true)?;
    replace_default_cpu_sets("clear", pid, process.raw(), &[])
}

pub fn apply_process_key(
    key: ProcessKey,
    cpu_set_ids: &[u32],
) -> Result<MutationReport, PlatformError> {
    let process = open_process(key.pid, true)?;
    verify_process_identity(process.raw(), key)?;
    replace_default_cpu_sets("apply", key.pid, process.raw(), cpu_set_ids)
}

pub fn clear_process_key(key: ProcessKey) -> Result<MutationReport, PlatformError> {
    let process = open_process(key.pid, true)?;
    verify_process_identity(process.raw(), key)?;
    replace_default_cpu_sets("clear", key.pid, process.raw(), &[])
}

pub fn query_process_efficiency_key(
    key: ProcessKey,
) -> Result<ProcessEfficiencyState, PlatformError> {
    let process = open_efficiency_process(key.pid, false)?;
    verify_process_identity(process.raw(), key)?;
    query_process_efficiency(process.raw())
}

pub fn apply_process_efficiency_key(
    key: ProcessKey,
    expected: ProcessEfficiencyState,
    requested: ProcessEfficiencyState,
    ownership: ProcessEfficiencyOwnership,
) -> Result<EfficiencyMutationReport, PlatformError> {
    replace_process_efficiency(
        "apply_background_efficiency",
        key,
        expected,
        requested,
        ownership,
        true,
    )
}

pub fn restore_process_efficiency_key(
    key: ProcessKey,
    original: ProcessEfficiencyState,
    applied: ProcessEfficiencyState,
    ownership: ProcessEfficiencyOwnership,
    pending: Option<ProcessEfficiencyState>,
) -> Result<EfficiencyMutationReport, PlatformError> {
    let process = open_efficiency_process(key.pid, true)?;
    verify_process_identity(process.raw(), key)?;
    restore_process_efficiency(
        process.raw(),
        key.pid,
        original,
        applied,
        ownership,
        pending,
    )
}

fn restore_process_efficiency(
    process: HANDLE,
    pid: u32,
    original: ProcessEfficiencyState,
    applied: ProcessEfficiencyState,
    ownership: ProcessEfficiencyOwnership,
    pending: Option<ProcessEfficiencyState>,
) -> Result<EfficiencyMutationReport, PlatformError> {
    let previous = query_process_efficiency(process)?;
    let mut requested = previous;
    let mut observed = previous;
    let mut eco_qos_changed = false;
    let mut memory_priority_changed = false;
    let mut unrestored_ownership = ProcessEfficiencyOwnership::default();
    let mut property_errors = Vec::new();
    let eco_qos_owned = ownership.eco_qos
        && (previous.eco_qos == applied.eco_qos
            || pending.is_some_and(|pending| previous.eco_qos == pending.eco_qos));
    let external_eco_qos_preserved = ownership.eco_qos && !eco_qos_owned;

    if eco_qos_owned && previous.eco_qos != original.eco_qos {
        match set_process_eco_qos(process, original.eco_qos)
            .and_then(|()| query_process_eco_qos(process))
        {
            Ok(value) if value == original.eco_qos => {
                requested.eco_qos = original.eco_qos;
                observed.eco_qos = value;
                eco_qos_changed = true;
            }
            Ok(value) => {
                observed.eco_qos = value;
                unrestored_ownership.eco_qos = true;
                property_errors.push(format!(
                    "EcoQoS restore verification mismatch: expected {:?}, observed {value:?}",
                    original.eco_qos
                ));
            }
            Err(error) => {
                observed.eco_qos = query_process_eco_qos(process).unwrap_or(previous.eco_qos);
                unrestored_ownership.eco_qos = true;
                property_errors.push(format!("EcoQoS restore failed: {error}"));
            }
        }
    }

    let current_memory = match query_process_memory_priority(process) {
        Ok(value) => value,
        Err(error) => {
            if ownership.memory_priority {
                unrestored_ownership.memory_priority = true;
                property_errors.push(format!("memory-priority query failed: {error}"));
            }
            previous.memory_priority
        }
    };
    observed.memory_priority = current_memory;
    let current_memory_owned = ownership.memory_priority
        && !unrestored_ownership.memory_priority
        && (current_memory == applied.memory_priority
            || pending.is_some_and(|pending| current_memory == pending.memory_priority));
    let external_memory_priority_preserved =
        ownership.memory_priority && !unrestored_ownership.memory_priority && !current_memory_owned;
    if current_memory_owned && current_memory != original.memory_priority {
        match set_process_memory_priority(process, original.memory_priority)
            .and_then(|()| query_process_memory_priority(process))
        {
            Ok(value) if value == original.memory_priority => {
                requested.memory_priority = original.memory_priority;
                observed.memory_priority = value;
                memory_priority_changed = true;
            }
            Ok(value) => {
                observed.memory_priority = value;
                unrestored_ownership.memory_priority = true;
                property_errors.push(format!(
                    "memory-priority restore verification mismatch: expected {:?}, observed {value:?}",
                    original.memory_priority
                ));
            }
            Err(error) => {
                observed.memory_priority =
                    query_process_memory_priority(process).unwrap_or(current_memory);
                unrestored_ownership.memory_priority = true;
                property_errors.push(format!("memory-priority restore failed: {error}"));
            }
        }
    }
    Ok(EfficiencyMutationReport {
        operation: "restore_background_efficiency".to_owned(),
        pid,
        committed: unrestored_ownership.is_empty(),
        previous,
        requested,
        observed,
        eco_qos_changed,
        memory_priority_changed,
        external_eco_qos_preserved,
        external_memory_priority_preserved,
        unrestored_ownership,
        property_errors,
    })
}

fn replace_process_efficiency(
    operation: &str,
    key: ProcessKey,
    expected: ProcessEfficiencyState,
    requested: ProcessEfficiencyState,
    ownership: ProcessEfficiencyOwnership,
    enforce_target_safety: bool,
) -> Result<EfficiencyMutationReport, PlatformError> {
    let process = open_efficiency_process(key.pid, true)?;
    verify_process_identity(process.raw(), key)?;
    if enforce_target_safety {
        ensure_efficiency_target_is_safe(key.pid, process.raw())?;
    }
    let previous = query_process_efficiency(process.raw())?;
    if !ownership.matches(expected, previous) {
        return Err(PlatformError::EfficiencyOwnershipChanged {
            pid: key.pid,
            expected,
            observed: previous,
        });
    }
    let effective_requested = ProcessEfficiencyState {
        eco_qos: if ownership.eco_qos {
            requested.eco_qos
        } else {
            previous.eco_qos
        },
        memory_priority: if ownership.memory_priority {
            requested.memory_priority
        } else {
            previous.memory_priority
        },
    };
    let update = set_process_efficiency(process.raw(), effective_requested, ownership).and_then(
        |()| {
            let observed = query_process_efficiency(process.raw())?;
            if ownership.matches(effective_requested, observed) {
                Ok(observed)
            } else {
                Err(PlatformError::EfficiencyUpdateFailed {
                    pid: key.pid,
                    detail: format!(
                        "verification mismatch: requested {effective_requested:?} for {ownership:?}, observed {observed:?}"
                    ),
                    rollback: "not attempted".to_owned(),
                })
            }
        },
    );
    let observed = match update {
        Ok(observed) => observed,
        Err(error) => {
            let rollback = match restore_process_efficiency(
                process.raw(),
                key.pid,
                previous,
                effective_requested,
                ownership,
                None,
            ) {
                Ok(report) if report.unrestored_ownership.is_empty() => "succeeded".to_owned(),
                Ok(report) => format!("incomplete: {}", report.property_errors.join("; ")),
                Err(rollback_error) => format!("failed: {rollback_error}"),
            };
            return Err(PlatformError::EfficiencyUpdateFailed {
                pid: key.pid,
                detail: error.to_string(),
                rollback,
            });
        }
    };

    let eco_qos_changed = ownership.eco_qos && previous.eco_qos != effective_requested.eco_qos;
    let memory_priority_changed = ownership.memory_priority
        && previous.memory_priority != effective_requested.memory_priority;
    Ok(EfficiencyMutationReport {
        operation: operation.to_owned(),
        pid: key.pid,
        committed: true,
        previous,
        requested: effective_requested,
        observed,
        eco_qos_changed,
        memory_priority_changed,
        external_eco_qos_preserved: !ownership.eco_qos,
        external_memory_priority_preserved: !ownership.memory_priority,
        unrestored_ownership: ProcessEfficiencyOwnership::default(),
        property_errors: Vec::new(),
    })
}

fn query_process_efficiency(process: HANDLE) -> Result<ProcessEfficiencyState, PlatformError> {
    Ok(ProcessEfficiencyState {
        eco_qos: query_process_eco_qos(process)?,
        memory_priority: query_process_memory_priority(process)?,
    })
}

fn query_process_eco_qos(process: HANDLE) -> Result<ProcessEcoQosState, PlatformError> {
    let mut power = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ..Default::default()
    };
    // SAFETY: The output type and byte length match ProcessPowerThrottling.
    unsafe {
        GetProcessInformation(
            process,
            ProcessPowerThrottling,
            (&raw mut power).cast(),
            u32::try_from(size_of::<PROCESS_POWER_THROTTLING_STATE>())
                .expect("power throttling state size fits u32"),
        )?;
    }
    Ok(
        if power.ControlMask & PROCESS_POWER_THROTTLING_EXECUTION_SPEED == 0 {
            ProcessEcoQosState::Unset
        } else if power.StateMask & PROCESS_POWER_THROTTLING_EXECUTION_SPEED == 0 {
            ProcessEcoQosState::Disabled
        } else {
            ProcessEcoQosState::Enabled
        },
    )
}

fn query_process_memory_priority(process: HANDLE) -> Result<ProcessMemoryPriority, PlatformError> {
    let mut memory = MEMORY_PRIORITY_INFORMATION::default();
    // SAFETY: The output type and byte length match ProcessMemoryPriority.
    unsafe {
        GetProcessInformation(
            process,
            ProcessMemoryPriority,
            (&raw mut memory).cast(),
            u32::try_from(size_of::<MEMORY_PRIORITY_INFORMATION>())
                .expect("memory priority information size fits u32"),
        )?;
    }
    process_memory_priority(memory.MemoryPriority)
}

fn set_process_efficiency(
    process: HANDLE,
    requested: ProcessEfficiencyState,
    ownership: ProcessEfficiencyOwnership,
) -> Result<(), PlatformError> {
    if ownership.eco_qos {
        set_process_eco_qos(process, requested.eco_qos)?;
    }
    if ownership.memory_priority {
        set_process_memory_priority(process, requested.memory_priority)?;
    }
    Ok(())
}

fn set_process_eco_qos(
    process: HANDLE,
    requested: ProcessEcoQosState,
) -> Result<(), PlatformError> {
    let mut power = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ..Default::default()
    };
    // Preserve power-throttling flags owned by the target or another controller.
    // SAFETY: The output type and byte length match ProcessPowerThrottling.
    unsafe {
        GetProcessInformation(
            process,
            ProcessPowerThrottling,
            (&raw mut power).cast(),
            u32::try_from(size_of::<PROCESS_POWER_THROTTLING_STATE>())
                .expect("power throttling state size fits u32"),
        )?;
    }
    match requested {
        ProcessEcoQosState::Unset => {
            power.ControlMask &= !PROCESS_POWER_THROTTLING_EXECUTION_SPEED;
            power.StateMask &= !PROCESS_POWER_THROTTLING_EXECUTION_SPEED;
        }
        ProcessEcoQosState::Disabled => {
            power.ControlMask |= PROCESS_POWER_THROTTLING_EXECUTION_SPEED;
            power.StateMask &= !PROCESS_POWER_THROTTLING_EXECUTION_SPEED;
        }
        ProcessEcoQosState::Enabled => {
            power.ControlMask |= PROCESS_POWER_THROTTLING_EXECUTION_SPEED;
            power.StateMask |= PROCESS_POWER_THROTTLING_EXECUTION_SPEED;
        }
    }
    // SAFETY: The input type and byte length match ProcessPowerThrottling.
    unsafe {
        SetProcessInformation(
            process,
            ProcessPowerThrottling,
            (&raw const power).cast(),
            u32::try_from(size_of::<PROCESS_POWER_THROTTLING_STATE>())
                .expect("power throttling state size fits u32"),
        )?;
    }

    Ok(())
}

fn set_process_memory_priority(
    process: HANDLE,
    requested: ProcessMemoryPriority,
) -> Result<(), PlatformError> {
    let memory = MEMORY_PRIORITY_INFORMATION {
        MemoryPriority: windows_memory_priority(requested),
    };
    // SAFETY: The input type and byte length match ProcessMemoryPriority.
    unsafe {
        SetProcessInformation(
            process,
            ProcessMemoryPriority,
            (&raw const memory).cast(),
            u32::try_from(size_of::<MEMORY_PRIORITY_INFORMATION>())
                .expect("memory priority information size fits u32"),
        )?;
    }
    Ok(())
}

fn process_memory_priority(
    priority: MEMORY_PRIORITY,
) -> Result<ProcessMemoryPriority, PlatformError> {
    match priority {
        MEMORY_PRIORITY_VERY_LOW => Ok(ProcessMemoryPriority::VeryLow),
        MEMORY_PRIORITY_LOW => Ok(ProcessMemoryPriority::Low),
        MEMORY_PRIORITY_MEDIUM => Ok(ProcessMemoryPriority::Medium),
        MEMORY_PRIORITY_BELOW_NORMAL => Ok(ProcessMemoryPriority::BelowNormal),
        MEMORY_PRIORITY_NORMAL => Ok(ProcessMemoryPriority::Normal),
        value => Err(PlatformError::UnsupportedMemoryPriority(value.0)),
    }
}

const fn windows_memory_priority(priority: ProcessMemoryPriority) -> MEMORY_PRIORITY {
    match priority {
        ProcessMemoryPriority::VeryLow => MEMORY_PRIORITY_VERY_LOW,
        ProcessMemoryPriority::Low => MEMORY_PRIORITY_LOW,
        ProcessMemoryPriority::Medium => MEMORY_PRIORITY_MEDIUM,
        ProcessMemoryPriority::BelowNormal => MEMORY_PRIORITY_BELOW_NORMAL,
        ProcessMemoryPriority::Normal => MEMORY_PRIORITY_NORMAL,
    }
}

fn open_efficiency_process(pid: u32, mutate: bool) -> Result<OwnedHandle, PlatformError> {
    let mut access = PROCESS_QUERY_LIMITED_INFORMATION;
    if mutate {
        access |= PROCESS_SET_INFORMATION;
    }
    // SAFETY: OpenProcess validates the PID and requested access; inheritance is disabled.
    match unsafe { OpenProcess(access, false, pid) } {
        Ok(handle) => Ok(OwnedHandle::new(handle)),
        Err(error) if error.code() == HRESULT::from_win32(ERROR_INVALID_PARAMETER.0) => {
            Err(PlatformError::ProcessUnavailable(pid))
        }
        Err(error) => Err(error.into()),
    }
}

impl PlatformError {
    pub(super) const fn process_no_longer_matches(&self) -> bool {
        matches!(
            self,
            Self::ProcessUnavailable(_) | Self::ProcessIdentityChanged { .. }
        )
    }

    pub(super) const fn efficiency_ownership_changed(&self) -> bool {
        matches!(self, Self::EfficiencyOwnershipChanged { .. })
    }
}

pub fn capture_interactive_activity() -> Result<InteractiveActivity, PlatformError> {
    let session_id = current_session_id()?;
    let (window_probe_available, foreground_pid, visible_pids) = window_activity();
    let (audio_probe_available, audible_pids) = match audible_processes() {
        Ok(pids) => (true, pids),
        Err(_) => (false, Vec::new()),
    };
    Ok(InteractiveActivity {
        session_id,
        foreground_pid,
        visible_pids,
        audible_pids,
        window_probe_available,
        audio_probe_available,
    })
}

pub fn current_session_id() -> Result<u32, PlatformError> {
    process_session_id(std::process::id()).ok_or_else(|| PlatformError::WindowsCall {
        operation: "ProcessIdToSessionId(interactive probe)",
        source: WindowsError::from_thread(),
    })
}

pub fn current_process_key() -> Result<ProcessKey, PlatformError> {
    let pid = std::process::id();
    let process = open_process(pid, false)?;
    Ok(ProcessKey {
        pid,
        creation_time_100ns: query_process_times(process.raw())?.creation_time_100ns,
    })
}

pub fn current_process_resource_usage() -> Result<ProcessResourceUsage, PlatformError> {
    // SAFETY: The pseudo handle is valid for the lifetime of the calling process and must not be
    // closed. Both queries only read accounting information for that process.
    let process = unsafe { GetCurrentProcess() };
    let times = query_process_times(process)?;
    let mut memory = PROCESS_MEMORY_COUNTERS {
        cb: u32::try_from(size_of::<PROCESS_MEMORY_COUNTERS>())
            .expect("PROCESS_MEMORY_COUNTERS size fits in u32"),
        ..PROCESS_MEMORY_COUNTERS::default()
    };
    // SAFETY: memory is a fully initialized writable structure of the advertised size and the
    // current-process pseudo handle has query access.
    unsafe { K32GetProcessMemoryInfo(process, &raw mut memory, memory.cb) }.ok()?;
    // SAFETY: This API returns the current UTC FILETIME value and has no pointer arguments.
    let now_100ns = filetime_value(unsafe { GetSystemTimePreciseAsFileTime() });

    Ok(ProcessResourceUsage {
        uptime_ms: now_100ns.saturating_sub(times.creation_time_100ns) / 10_000,
        cpu_time_100ns: times.cpu_time_100ns,
        working_set_bytes: u64::try_from(memory.WorkingSetSize).unwrap_or(u64::MAX),
    })
}

fn window_activity() -> (bool, Option<u32>, Vec<u32>) {
    let foreground = unsafe { GetForegroundWindow() };
    let mut foreground_pid = 0u32;
    let foreground_pid = (!foreground.0.is_null()
        && unsafe { GetWindowThreadProcessId(foreground, Some(&raw mut foreground_pid)) } != 0
        && foreground_pid != 0)
        .then_some(foreground_pid);

    let mut visible = BTreeSet::<u32>::new();
    // SAFETY: The callback receives the live map pointer only for this synchronous call.
    let result = unsafe {
        EnumWindows(
            Some(collect_visible_window_pid),
            LPARAM((&raw mut visible).cast::<core::ffi::c_void>() as isize),
        )
    };
    match result {
        Ok(()) if foreground_pid.is_some() => (true, foreground_pid, visible.into_iter().collect()),
        Err(_) => (false, foreground_pid, Vec::new()),
        Ok(()) => (false, None, visible.into_iter().collect()),
    }
}

unsafe extern "system" fn collect_visible_window_pid(hwnd: HWND, context: LPARAM) -> BOOL {
    // SAFETY: window_activity passes a valid BTreeMap pointer for the synchronous enumeration.
    let visible = unsafe { &mut *(context.0 as *mut BTreeSet<u32>) };
    // Minimized windows remain protected because restoring them from the taskbar is interactive.
    if unsafe { IsWindowVisible(hwnd).as_bool() } {
        let mut pid = 0u32;
        if unsafe { GetWindowThreadProcessId(hwnd, Some(&raw mut pid)) } != 0 && pid != 0 {
            visible.insert(pid);
        }
    }
    true.into()
}

struct ComApartment;

impl ComApartment {
    fn initialize() -> Result<Self, WindowsError> {
        // SAFETY: This probe owns the COM initialization balance on its worker thread.
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
        Ok(Self)
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        // SAFETY: initialize succeeded on this thread and each success requires one uninitialize.
        unsafe { CoUninitialize() };
    }
}

fn audible_processes() -> Result<Vec<u32>, WindowsError> {
    let _apartment = ComApartment::initialize()?;
    // SAFETY: MMDeviceEnumerator is an in-process COM class and no aggregation is requested.
    let devices: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
    // SAFETY: The enumerator remains alive while the returned collection is used.
    let endpoints = unsafe { devices.EnumAudioEndpoints(eAll, DEVICE_STATE_ACTIVE)? };
    let endpoint_count = unsafe { endpoints.GetCount()? };
    let mut audible = BTreeSet::<u32>::new();
    for endpoint_index in 0..endpoint_count {
        let endpoint = unsafe { endpoints.Item(endpoint_index)? };
        let manager: IAudioSessionManager2 = unsafe { endpoint.Activate(CLSCTX_ALL, None)? };
        let sessions = unsafe { manager.GetSessionEnumerator()? };
        let session_count = unsafe { sessions.GetCount()? };
        for session_index in 0..session_count {
            let control = unsafe { sessions.GetSession(session_index)? };
            if unsafe { control.GetState()? } != AudioSessionStateActive {
                continue;
            }
            let control: IAudioSessionControl2 = control.cast()?;
            let pid = unsafe { control.GetProcessId()? };
            if pid != 0 {
                audible.insert(pid);
            }
        }
    }
    Ok(audible.into_iter().collect())
}

pub fn run_assigned(
    program: &Path,
    args: &[OsString],
    cpu_set_ids: &[u32],
) -> Result<LaunchReport, PlatformError> {
    if !program.is_file() {
        return Err(PlatformError::InvalidExecutable(
            program.display().to_string(),
        ));
    }

    let application = wide_null(program.as_os_str());
    let mut command_line = build_command_line(program.as_os_str(), args);
    let mut startup = STARTUPINFOW {
        cb: u32::try_from(size_of::<STARTUPINFOW>()).expect("STARTUPINFOW size fits u32"),
        ..Default::default()
    };
    // SAFETY: PROCESS_INFORMATION is a plain Win32 output structure.
    let mut process_info: PROCESS_INFORMATION = unsafe { zeroed() };

    // SAFETY: All pointers reference live, correctly terminated buffers for the duration of the call.
    unsafe {
        CreateProcessW(
            PCWSTR(application.as_ptr()),
            Some(PWSTR(command_line.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_SUSPENDED,
            None,
            PCWSTR::null(),
            &raw mut startup,
            &raw mut process_info,
        )?;
    }

    let process = OwnedHandle::new(process_info.hProcess);
    let thread = OwnedHandle::new(process_info.hThread);
    let pid = process_info.dwProcessId;

    if let Err(error) = replace_default_cpu_sets("run", pid, process.raw(), cpu_set_ids) {
        // SAFETY: The process was created suspended by this function and is still owned here.
        let _ = unsafe { TerminateProcess(process.raw(), 1) };
        return Err(error);
    }

    // SAFETY: hThread is the valid suspended primary thread returned by CreateProcessW.
    if unsafe { ResumeThread(thread.raw()) } == RESUME_THREAD_FAILED {
        // SAFETY: The process is still owned here and cannot be safely left suspended.
        let _ = unsafe { TerminateProcess(process.raw(), 1) };
        return Err(PlatformError::ResumeFailed(pid));
    }

    Ok(LaunchReport {
        pid,
        cpu_set_ids: sorted(cpu_set_ids),
    })
}

fn open_process(pid: u32, mutate: bool) -> Result<OwnedHandle, PlatformError> {
    let mut access = PROCESS_QUERY_LIMITED_INFORMATION;
    if mutate {
        access |= PROCESS_SET_LIMITED_INFORMATION;
    }
    // SAFETY: OpenProcess validates the PID and requested access; inheritance is disabled.
    match unsafe { OpenProcess(access, false, pid) } {
        Ok(handle) => Ok(OwnedHandle::new(handle)),
        Err(error) if error.code() == HRESULT::from_win32(ERROR_INVALID_PARAMETER.0) => {
            Err(PlatformError::ProcessUnavailable(pid))
        }
        Err(error) => Err(error.into()),
    }
}

fn topology_for_process(process: Option<HANDLE>) -> Result<Topology, PlatformError> {
    let mut required_bytes = 0u32;
    // SAFETY: The first call intentionally supplies no buffer and asks Windows for its size.
    unsafe {
        let _ = GetSystemCpuSetInformation(None, 0, &raw mut required_bytes, process, None);
    }
    if required_bytes == 0 {
        return Err(WindowsError::from_thread().into());
    }

    let words = usize::try_from(required_bytes)
        .expect("u32 fits usize")
        .div_ceil(size_of::<u64>());
    let mut buffer = vec![0u64; words];
    let buffer_bytes =
        u32::try_from(buffer.len() * size_of::<u64>()).expect("CPU Set buffer length fits u32");
    let mut returned_bytes = required_bytes;

    // SAFETY: The u64 buffer is sufficiently aligned and at least required_bytes long.
    let success = unsafe {
        GetSystemCpuSetInformation(
            Some(buffer.as_mut_ptr().cast::<SYSTEM_CPU_SET_INFORMATION>()),
            buffer_bytes,
            &raw mut returned_bytes,
            process,
            None,
        )
    };
    if !success.as_bool() {
        return Err(WindowsError::from_thread().into());
    }

    parse_cpu_set_buffer(&buffer, returned_bytes)
}

fn parse_cpu_set_buffer(buffer: &[u64], returned_bytes: u32) -> Result<Topology, PlatformError> {
    let total = usize::try_from(returned_bytes).expect("u32 fits usize");
    if total > std::mem::size_of_val(buffer) {
        return Err(PlatformError::InvalidCpuSetBuffer(format!(
            "Windows returned {total} bytes into a {} byte buffer",
            std::mem::size_of_val(buffer)
        )));
    }

    let mut offset = 0usize;
    let mut cpu_sets = Vec::new();
    while offset < total {
        if total - offset < size_of::<SYSTEM_CPU_SET_INFORMATION>() {
            return Err(PlatformError::InvalidCpuSetBuffer(format!(
                "truncated record header at byte {offset}"
            )));
        }

        if !offset.is_multiple_of(size_of::<u64>()) {
            return Err(PlatformError::InvalidCpuSetBuffer(format!(
                "record at byte {offset} is not naturally aligned"
            )));
        }
        let word_index = offset / size_of::<u64>();
        // SAFETY: The backing storage is u64-aligned, the word index is in bounds,
        // and the complete record header was validated above.
        let information = unsafe {
            &*buffer
                .as_ptr()
                .add(word_index)
                .cast::<SYSTEM_CPU_SET_INFORMATION>()
        };
        let record_size = usize::try_from(information.Size).expect("u32 fits usize");
        if record_size < size_of::<SYSTEM_CPU_SET_INFORMATION>() || offset + record_size > total {
            return Err(PlatformError::InvalidCpuSetBuffer(format!(
                "invalid record size {record_size} at byte {offset}"
            )));
        }

        if information.Type == CpuSetInformation {
            // SAFETY: CpuSetInformation guarantees that the CpuSet union member is active.
            let raw = unsafe { information.Anonymous.CpuSet };
            // SAFETY: Both nested unions are part of the active CpuSet record.
            let flags = unsafe { raw.Anonymous1.AllFlags };
            // SAFETY: SchedulingClass is the documented byte view of the second union.
            let scheduling_class = unsafe { raw.Anonymous2.SchedulingClass };
            cpu_sets.push(CpuSet {
                id: raw.Id,
                group: raw.Group,
                logical_processor_index: raw.LogicalProcessorIndex,
                core_index: raw.CoreIndex,
                last_level_cache_index: raw.LastLevelCacheIndex,
                numa_node_index: raw.NumaNodeIndex,
                efficiency_class: raw.EfficiencyClass,
                scheduling_class,
                flags: CpuSetFlags {
                    parked: u32::from(flags) & SYSTEM_CPU_SET_INFORMATION_PARKED != 0,
                    allocated: u32::from(flags) & SYSTEM_CPU_SET_INFORMATION_ALLOCATED != 0,
                    allocated_to_target_process: u32::from(flags)
                        & SYSTEM_CPU_SET_INFORMATION_ALLOCATED_TO_TARGET_PROCESS
                        != 0,
                    realtime: u32::from(flags) & SYSTEM_CPU_SET_INFORMATION_REALTIME != 0,
                },
                allocation_tag: raw.AllocationTag,
            });
        }

        offset += record_size;
    }

    Topology::new(cpu_sets).map_err(Into::into)
}

fn get_process_default_cpu_sets(process: HANDLE) -> Result<Vec<u32>, PlatformError> {
    let mut required_count = 0u32;
    // SAFETY: The first call requests the required element count without a data buffer.
    let success = unsafe { GetProcessDefaultCpuSets(process, None, &raw mut required_count) };
    if required_count == 0 {
        return if success.as_bool() {
            Ok(Vec::new())
        } else {
            Err(PlatformError::WindowsCall {
                operation: "GetProcessDefaultCpuSets(size query)",
                source: WindowsError::from_thread(),
            })
        };
    }

    let mut ids = vec![0u32; usize::try_from(required_count).expect("u32 fits usize")];
    // SAFETY: The buffer has exactly the size returned by the preceding query.
    let success = unsafe {
        GetProcessDefaultCpuSets(process, Some(ids.as_mut_slice()), &raw mut required_count)
    };
    if !success.as_bool() {
        return Err(PlatformError::WindowsCall {
            operation: "GetProcessDefaultCpuSets(data query)",
            source: WindowsError::from_thread(),
        });
    }
    ids.truncate(usize::try_from(required_count).expect("u32 fits usize"));
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

fn replace_default_cpu_sets(
    operation: &str,
    pid: u32,
    process: HANDLE,
    requested: &[u32],
) -> Result<MutationReport, PlatformError> {
    if !requested.is_empty() {
        ensure_assignment_target_is_safe(pid, process)?;
    }
    let previous = get_process_default_cpu_sets(process)?;
    let requested = sorted(requested);
    set_process_default_cpu_sets(process, &requested)?;
    let observed = get_process_default_cpu_sets(process)?;

    if observed != requested {
        let rollback = match set_process_default_cpu_sets(process, &previous) {
            Ok(()) => match get_process_default_cpu_sets(process) {
                Ok(restored) if restored == previous => "succeeded".to_owned(),
                Ok(restored) => format!("verification mismatch: observed {restored:?}"),
                Err(error) => format!("verification failed: {error}"),
            },
            Err(error) => format!("failed: {error}"),
        };
        return Err(PlatformError::VerificationFailed {
            pid,
            requested,
            observed,
            rollback,
        });
    }

    Ok(MutationReport {
        operation: operation.to_owned(),
        pid,
        committed: true,
        previous_cpu_set_ids: previous,
        requested_cpu_set_ids: requested,
        observed_cpu_set_ids: observed,
    })
}

fn ensure_assignment_target_is_safe(pid: u32, process: HANDLE) -> Result<(), PlatformError> {
    let image = process_image_path(process)
        .and_then(|path| {
            Path::new(&path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .ok_or(PlatformError::UnidentifiedMutationTarget(pid))?;
    if safety::is_fixed_system_process(pid, &image) {
        Err(PlatformError::ProtectedMutationTarget { pid, image })
    } else {
        Ok(())
    }
}

fn ensure_efficiency_target_is_safe(pid: u32, process: HANDLE) -> Result<(), PlatformError> {
    let image = process_image_path(process)
        .and_then(|path| {
            Path::new(&path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .ok_or(PlatformError::UnidentifiedMutationTarget(pid))?;
    let reason = if safety::is_fixed_system_process(pid, &image) {
        Some("fixed system or virtualization host exclusion")
    } else if process_session_id(pid).is_none_or(|session| session == 0) {
        Some("session 0 or unqueryable session")
    } else if process_priority_class(process) == Some(REALTIME_PRIORITY_CLASS.0) {
        Some("realtime priority process")
    } else if process_is_protected(process).unwrap_or(true) {
        Some("protected or unqueryable process")
    } else {
        None
    };
    reason.map_or(Ok(()), |reason| {
        Err(PlatformError::UnsafeEfficiencyTarget { pid, image, reason })
    })
}

fn set_process_default_cpu_sets(process: HANDLE, cpu_set_ids: &[u32]) -> Result<(), PlatformError> {
    let ids = (!cpu_set_ids.is_empty()).then_some(cpu_set_ids);
    // SAFETY: The process handle has PROCESS_SET_LIMITED_INFORMATION access and the slice is live.
    let success = unsafe { SetProcessDefaultCpuSets(process, ids) };
    if success.as_bool() {
        Ok(())
    } else {
        Err(WindowsError::from_thread().into())
    }
}

fn sorted(values: &[u32]) -> Vec<u32> {
    let mut result = values.to_vec();
    result.sort_unstable();
    result.dedup();
    result
}

fn wide_null(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}

fn build_command_line(program: &OsStr, args: &[OsString]) -> Vec<u16> {
    let mut command = quote_windows_argument(program);
    for argument in args {
        command.push(' ');
        command.push_str(&quote_windows_argument(argument));
    }
    command.encode_utf16().chain(Some(0)).collect()
}

fn quote_windows_argument(value: &OsStr) -> String {
    let value = value.to_string_lossy();
    if !value.is_empty()
        && !value
            .chars()
            .any(|character| character.is_whitespace() || character == '"')
    {
        return value.into_owned();
    }

    let mut result = String::from('"');
    let mut backslashes = 0usize;
    for character in value.chars() {
        match character {
            '\\' => backslashes += 1,
            '"' => {
                result.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                result.push('"');
                backslashes = 0;
            }
            _ => {
                result.extend(std::iter::repeat_n('\\', backslashes));
                backslashes = 0;
                result.push(character);
            }
        }
    }
    result.extend(std::iter::repeat_n('\\', backslashes * 2));
    result.push('"');
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    static PIPE_TEST_LOCK: Mutex<()> = Mutex::new(());

    struct ChildGuard(std::process::Child);

    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn background_child() -> ChildGuard {
        ChildGuard(
            std::process::Command::new("cmd.exe")
                .args(["/d", "/c", "ping.exe -n 30 127.0.0.1 >nul"])
                .spawn()
                .expect("background child must start"),
        )
    }

    fn delayed_child_parent() -> ChildGuard {
        ChildGuard(
            std::process::Command::new("cmd.exe")
                .args([
                    "/d",
                    "/q",
                    "/c",
                    "ping.exe -n 3 127.0.0.1 >nul & ping.exe -n 30 127.0.0.1 >nul",
                ])
                .spawn()
                .expect("delayed child parent must start"),
        )
    }

    fn different_memory_priority(priority: ProcessMemoryPriority) -> ProcessMemoryPriority {
        if priority == ProcessMemoryPriority::Low {
            ProcessMemoryPriority::BelowNormal
        } else {
            ProcessMemoryPriority::Low
        }
    }

    fn third_memory_priority(
        first: ProcessMemoryPriority,
        second: ProcessMemoryPriority,
    ) -> ProcessMemoryPriority {
        [
            ProcessMemoryPriority::VeryLow,
            ProcessMemoryPriority::Low,
            ProcessMemoryPriority::Medium,
            ProcessMemoryPriority::BelowNormal,
            ProcessMemoryPriority::Normal,
        ]
        .into_iter()
        .find(|candidate| *candidate != first && *candidate != second)
        .expect("five memory priorities always contain a third value")
    }

    #[test]
    fn quotes_windows_arguments_without_a_shell() {
        assert_eq!(quote_windows_argument(OsStr::new("plain")), "plain");
        assert_eq!(
            quote_windows_argument(OsStr::new("two words")),
            "\"two words\""
        );
        assert_eq!(quote_windows_argument(OsStr::new("")), "\"\"");
        assert_eq!(
            quote_windows_argument(OsStr::new(r#"say "hello""#)),
            r#""say \"hello\"""#
        );
        assert_eq!(
            quote_windows_argument(OsStr::new(r"C:\path with space\")),
            r#""C:\path with space\\""#
        );
    }

    #[test]
    fn atomic_replace_overwrites_an_existing_windows_file_without_temp_residue() {
        let directory = std::env::temp_dir().join(format!(
            "winsched-atomic-replace-{}-{}",
            std::process::id(),
            ATOMIC_TEMP_COUNTER.fetch_add(1, AtomicOrdering::Relaxed)
        ));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("status.json");

        atomic_replace_file(&path, b"first").unwrap();
        atomic_replace_file(&path, b"second").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn current_process_resource_usage_is_live_and_monotonic() {
        let first = current_process_resource_usage().unwrap();
        std::thread::yield_now();
        let second = current_process_resource_usage().unwrap();

        assert!(first.working_set_bytes > 0);
        assert!(second.working_set_bytes > 0);
        assert!(second.uptime_ms >= first.uptime_ms);
        assert!(second.cpu_time_100ns >= first.cpu_time_100ns);
    }

    #[test]
    fn efficiency_state_round_trips_on_an_owned_child_process() {
        let child = background_child();
        let pid = child.0.id();
        let process = open_efficiency_process(pid, false).unwrap();
        let key = ProcessKey {
            pid,
            creation_time_100ns: query_process_times(process.raw())
                .unwrap()
                .creation_time_100ns,
        };
        let original = query_process_efficiency_key(key).unwrap();
        let requested = ProcessEfficiencyState {
            eco_qos: if original.eco_qos == ProcessEcoQosState::Enabled {
                ProcessEcoQosState::Disabled
            } else {
                ProcessEcoQosState::Enabled
            },
            memory_priority: different_memory_priority(original.memory_priority),
        };
        let ownership = ProcessEfficiencyOwnership::between(original, requested);

        let applied = apply_process_efficiency_key(key, original, requested, ownership).unwrap();
        assert_eq!(applied.observed, requested);
        let restored =
            restore_process_efficiency_key(key, original, requested, ownership, None).unwrap();
        assert_eq!(restored.observed, original);
        assert!(!restored.external_eco_qos_preserved);
        assert!(!restored.external_memory_priority_preserved);
    }

    #[test]
    fn ownership_safe_restore_preserves_an_external_memory_override() {
        let child = background_child();
        let pid = child.0.id();
        let process = open_efficiency_process(pid, false).unwrap();
        let key = ProcessKey {
            pid,
            creation_time_100ns: query_process_times(process.raw())
                .unwrap()
                .creation_time_100ns,
        };
        let original = query_process_efficiency_key(key).unwrap();
        let requested = ProcessEfficiencyState {
            eco_qos: ProcessEcoQosState::Enabled,
            memory_priority: different_memory_priority(original.memory_priority),
        };
        let ownership = ProcessEfficiencyOwnership::between(original, requested);
        apply_process_efficiency_key(key, original, requested, ownership).unwrap();
        let process = open_efficiency_process(pid, true).unwrap();
        let external_memory =
            third_memory_priority(original.memory_priority, requested.memory_priority);
        set_process_memory_priority(process.raw(), external_memory).unwrap();

        let restored =
            restore_process_efficiency_key(key, original, requested, ownership, None).unwrap();
        assert_eq!(restored.observed.eco_qos, original.eco_qos);
        assert_eq!(restored.observed.memory_priority, external_memory);
        assert!(restored.external_memory_priority_preserved);
    }

    #[test]
    fn eco_only_ownership_ignores_and_preserves_unowned_memory_changes() {
        let child = background_child();
        let pid = child.0.id();
        let process = open_efficiency_process(pid, false).unwrap();
        let key = ProcessKey {
            pid,
            creation_time_100ns: query_process_times(process.raw())
                .unwrap()
                .creation_time_100ns,
        };
        let original = query_process_efficiency_key(key).unwrap();
        let first_eco = if original.eco_qos == ProcessEcoQosState::Enabled {
            ProcessEcoQosState::Disabled
        } else {
            ProcessEcoQosState::Enabled
        };
        let first = ProcessEfficiencyState {
            eco_qos: first_eco,
            memory_priority: original.memory_priority,
        };
        let eco_only = ProcessEfficiencyOwnership {
            eco_qos: true,
            memory_priority: false,
        };
        apply_process_efficiency_key(key, original, first, eco_only).unwrap();

        let external_memory = if original.memory_priority == ProcessMemoryPriority::Low {
            ProcessMemoryPriority::Normal
        } else {
            ProcessMemoryPriority::Low
        };
        let process = open_efficiency_process(pid, true).unwrap();
        set_process_memory_priority(process.raw(), external_memory).unwrap();

        let reapplied = apply_process_efficiency_key(key, first, first, eco_only).unwrap();
        assert_eq!(reapplied.observed.memory_priority, external_memory);
        let restored =
            restore_process_efficiency_key(key, original, first, eco_only, None).unwrap();
        assert_eq!(restored.observed.eco_qos, original.eco_qos);
        assert_eq!(restored.observed.memory_priority, external_memory);
        assert!(!restored.external_memory_priority_preserved);
    }

    #[test]
    fn memory_only_ownership_ignores_and_preserves_unowned_eco_changes() {
        let child = background_child();
        let pid = child.0.id();
        let process = open_efficiency_process(pid, false).unwrap();
        let key = ProcessKey {
            pid,
            creation_time_100ns: query_process_times(process.raw())
                .unwrap()
                .creation_time_100ns,
        };
        let original = query_process_efficiency_key(key).unwrap();
        let requested_memory = if original.memory_priority == ProcessMemoryPriority::Low {
            ProcessMemoryPriority::BelowNormal
        } else {
            ProcessMemoryPriority::Low
        };
        let requested = ProcessEfficiencyState {
            eco_qos: original.eco_qos,
            memory_priority: requested_memory,
        };
        let memory_only = ProcessEfficiencyOwnership {
            eco_qos: false,
            memory_priority: true,
        };
        apply_process_efficiency_key(key, original, requested, memory_only).unwrap();

        let external_eco = if original.eco_qos == ProcessEcoQosState::Enabled {
            ProcessEcoQosState::Disabled
        } else {
            ProcessEcoQosState::Enabled
        };
        let process = open_efficiency_process(pid, true).unwrap();
        set_process_eco_qos(process.raw(), external_eco).unwrap();

        let reapplied =
            apply_process_efficiency_key(key, requested, requested, memory_only).unwrap();
        assert_eq!(reapplied.observed.eco_qos, external_eco);
        let restored =
            restore_process_efficiency_key(key, original, requested, memory_only, None).unwrap();
        assert_eq!(restored.observed.eco_qos, external_eco);
        assert_eq!(restored.observed.memory_priority, original.memory_priority);
        assert!(!restored.external_eco_qos_preserved);
    }

    #[test]
    fn memory_priority_but_not_eco_qos_propagates_to_a_later_cmd_child() {
        let parent = delayed_child_parent();
        let parent_pid = parent.0.id();
        let topology = system_topology().unwrap();
        let baseline_deadline = Instant::now() + Duration::from_secs(2);
        let baseline_child = loop {
            let candidate = observe_processes(&topology)
                .unwrap()
                .into_iter()
                .find(|process| {
                    process.parent_pid == parent_pid
                        && process.image_name.eq_ignore_ascii_case("ping.exe")
                });
            if let Some(candidate) = candidate {
                break candidate;
            }
            assert!(
                Instant::now() < baseline_deadline,
                "baseline child did not start"
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        let baseline = query_process_efficiency_key(baseline_child.key).unwrap();
        let before = BTreeSet::from([baseline_child.key]);
        let parent_process = open_efficiency_process(parent_pid, false).unwrap();
        let parent_key = ProcessKey {
            pid: parent_pid,
            creation_time_100ns: query_process_times(parent_process.raw())
                .unwrap()
                .creation_time_100ns,
        };
        let original = query_process_efficiency_key(parent_key).unwrap();
        let requested = ProcessEfficiencyState {
            eco_qos: if original.eco_qos == ProcessEcoQosState::Enabled {
                ProcessEcoQosState::Disabled
            } else {
                ProcessEcoQosState::Enabled
            },
            memory_priority: different_memory_priority(original.memory_priority),
        };
        let ownership = ProcessEfficiencyOwnership::between(original, requested);
        apply_process_efficiency_key(parent_key, original, requested, ownership).unwrap();

        let deadline = Instant::now() + Duration::from_secs(6);
        let inherited_child = loop {
            let candidate = observe_processes(&topology)
                .unwrap()
                .into_iter()
                .find(|process| {
                    process.parent_pid == parent_pid
                        && process.image_name.eq_ignore_ascii_case("ping.exe")
                        && !before.contains(&process.key)
                });
            if let Some(candidate) = candidate {
                break candidate;
            }
            assert!(Instant::now() < deadline, "delayed child did not start");
            std::thread::sleep(Duration::from_millis(100));
        };
        let inherited = query_process_efficiency_key(inherited_child.key).unwrap();
        eprintln!(
            "child efficiency after parent apply: parent_original={original:?}, parent_requested={requested:?}, child={inherited:?}"
        );
        assert_eq!(inherited.eco_qos, baseline.eco_qos);
        assert_eq!(inherited.memory_priority, requested.memory_priority);

        restore_process_efficiency_key(parent_key, original, requested, ownership, None).unwrap();
        let after_parent_restore = query_process_efficiency_key(inherited_child.key).unwrap();
        eprintln!("child efficiency after parent restore: child={after_parent_restore:?}");
        assert_eq!(after_parent_restore.eco_qos, baseline.eco_qos);
        assert_eq!(
            after_parent_restore.memory_priority, requested.memory_priority,
            "restoring the parent does not retroactively restore a live child"
        );
    }

    #[test]
    fn updating_a_publisher_at_capacity_does_not_evict_another_publisher() {
        let mut states = InteractiveStateMap::new();
        for index in 0..MAX_INTERACTIVE_PUBLISHERS {
            let source_pid = u32::try_from(index + 1).unwrap();
            let state = InteractiveActivityState {
                schema_version: INTERACTIVE_STATE_SCHEMA_VERSION,
                session_id: 1,
                source_pid,
                source_creation_time_100ns: u64::from(source_pid) * 10,
                window_probe_available: true,
                audio_probe_available: true,
                foreground_pid: None,
                visible_pids: Vec::new(),
                audible_pids: Vec::new(),
                updated_at_unix_ms: 100,
            };
            insert_interactive_state(&mut states, state, 100);
        }
        let updated = InteractiveActivityState {
            updated_at_unix_ms: 200,
            ..states.get(&(1, 1, 10)).unwrap().clone()
        };
        insert_interactive_state(&mut states, updated, 200);

        assert_eq!(states.len(), MAX_INTERACTIVE_PUBLISHERS);
        assert!(states.contains_key(&(1, 2, 20)));
        assert_eq!(states.get(&(1, 1, 10)).unwrap().updated_at_unix_ms, 200);
    }

    #[test]
    fn authenticated_pipe_accepts_current_process_and_stamps_receive_time() {
        let _pipe_test = PIPE_TEST_LOCK.lock().unwrap();
        let executable = std::env::current_exe().unwrap();
        let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let wake_counter = Arc::clone(&wakes);
        let wake = Arc::new(move || {
            wake_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }) as InteractiveStateWake;
        let server = InteractiveStateServer::start(&executable, Some(wake)).unwrap();
        let source = current_process_key().unwrap();
        let state = InteractiveActivityState {
            schema_version: INTERACTIVE_STATE_SCHEMA_VERSION,
            session_id: current_session_id().unwrap(),
            source_pid: source.pid,
            source_creation_time_100ns: source.creation_time_100ns,
            window_probe_available: true,
            audio_probe_available: true,
            foreground_pid: Some(source.pid),
            visible_pids: vec![source.pid],
            audible_pids: Vec::new(),
            updated_at_unix_ms: 1,
        };
        let encoded = serde_json::to_vec(&state).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .open(INTERACTIVE_PIPE_NAME)
            {
                Ok(mut pipe) => {
                    use std::io::Write as _;
                    pipe.write_all(&encoded).unwrap();
                    break;
                }
                Err(_) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => panic!("interactive pipe client could not connect: {error}"),
            }
        }
        loop {
            if let Some(received) = server.states().into_iter().next() {
                assert_eq!(received.source_pid, source.pid);
                assert_eq!(received.session_id, state.session_id);
                assert_ne!(received.updated_at_unix_ms, 1);
                assert_eq!(wakes.load(std::sync::atomic::Ordering::Relaxed), 1);
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "interactive pipe state was not received"
            );
            std::thread::sleep(Duration::from_millis(20));
        }

        let stalled_deadline = std::time::Instant::now() + Duration::from_secs(3);
        let stalled_client = loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .open(INTERACTIVE_PIPE_NAME)
            {
                Ok(pipe) => break pipe,
                Err(_) if std::time::Instant::now() < stalled_deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => panic!("stalled interactive pipe client could not connect: {error}"),
            }
        };
        let drop_started = std::time::Instant::now();
        drop(server);
        assert!(
            drop_started.elapsed() < Duration::from_secs(2),
            "interactive pipe server did not cancel a stalled client read promptly"
        );
        drop(stalled_client);
    }

    #[test]
    fn authenticated_pipe_rejects_a_client_with_the_wrong_image_path() {
        let _pipe_test = PIPE_TEST_LOCK.lock().unwrap();
        let executable = std::env::current_exe().unwrap();
        let wrong_path = executable.with_file_name("not-the-winsched-client.exe");
        let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let wake_counter = Arc::clone(&wakes);
        let wake = Arc::new(move || {
            wake_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }) as InteractiveStateWake;
        let server = InteractiveStateServer::start(&wrong_path, Some(wake)).unwrap();
        let source = current_process_key().unwrap();
        let state = InteractiveActivityState {
            schema_version: INTERACTIVE_STATE_SCHEMA_VERSION,
            session_id: current_session_id().unwrap(),
            source_pid: source.pid,
            source_creation_time_100ns: source.creation_time_100ns,
            window_probe_available: true,
            audio_probe_available: true,
            foreground_pid: None,
            visible_pids: Vec::new(),
            audible_pids: Vec::new(),
            updated_at_unix_ms: 1,
        };
        let encoded = serde_json::to_vec(&state).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .open(INTERACTIVE_PIPE_NAME)
            {
                Ok(mut pipe) => {
                    use std::io::Write as _;
                    let _ = pipe.write_all(&encoded);
                    break;
                }
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => panic!("negative-auth pipe client could not connect: {error}"),
            }
        }
        std::thread::sleep(Duration::from_millis(100));
        assert!(server.states().is_empty());
        assert_eq!(wakes.load(std::sync::atomic::Ordering::Relaxed), 0);
    }

    #[test]
    fn memory_pressure_and_wasapi_probes_initialize_in_user_mode() {
        let monitor = MemoryPressureMonitor::new().unwrap();
        let _ = monitor.is_low().unwrap();
        let pids = audible_processes().unwrap();
        assert!(pids.windows(2).all(|pair| pair[0] < pair[1]));
    }
}
