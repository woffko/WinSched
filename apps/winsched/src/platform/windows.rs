#![allow(unsafe_code)]

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::Path;

use thiserror::Error;
use windows::Win32::Foundation::{CloseHandle, ERROR_NO_MORE_FILES, FILETIME, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Performance::{
    PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT_COUNTERVALUE, PDH_FMT_DOUBLE,
    PDH_HCOUNTER, PDH_HQUERY, PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData,
    PdhGetFormattedCounterValue, PdhOpenQueryW,
};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::SystemInformation::{
    CpuSetInformation, GetSystemCpuSetInformation, SYSTEM_CPU_SET_INFORMATION,
    SYSTEM_CPU_SET_INFORMATION_ALLOCATED, SYSTEM_CPU_SET_INFORMATION_ALLOCATED_TO_TARGET_PROCESS,
    SYSTEM_CPU_SET_INFORMATION_PARKED, SYSTEM_CPU_SET_INFORMATION_REALTIME,
};
use windows::Win32::System::Threading::{
    CREATE_SUSPENDED, CreateProcessW, GetPriorityClass, GetProcessDefaultCpuSets,
    GetProcessInformation, GetProcessTimes, OpenProcess, PROCESS_INFORMATION, PROCESS_NAME_WIN32,
    PROCESS_PROTECTION_LEVEL_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SET_LIMITED_INFORMATION, PROTECTION_LEVEL_NONE, ProcessProtectionLevelInfo,
    QueryFullProcessImageNameW, REALTIME_PRIORITY_CLASS, ResumeThread, STARTUPINFOW,
    SetProcessDefaultCpuSets, TerminateProcess,
};
use windows::core::{Error as WindowsError, HRESULT, PCWSTR, PWSTR};
use winsched_core::{
    CpuSet, CpuSetFlags, LlcDomainKey, Topology, TopologyError,
    adaptive::{DomainLoad, ExclusionReason, ProcessKey},
};

use super::{LaunchReport, MutationReport, ObservedProcess, ProcessSnapshot};

const RESUME_THREAD_FAILED: u32 = u32::MAX;

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
    #[error("{operation} failed with PDH status 0x{status:08X}")]
    PdhStatus {
        operation: &'static str,
        status: u32,
    },
    #[error("PDH counter '{path}' returned data status 0x{status:08X}")]
    PdhCounterStatus { path: String, status: u32 },
    #[error("PDH counter '{0}' returned a non-finite value")]
    PdhNonFinite(String),
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

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: The wrapper uniquely owns a valid Win32 handle.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
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
    path: String,
    handle: PDH_HCOUNTER,
}

/// Locale-independent PDH sampler for per-CPU processor utility.
#[derive(Debug)]
pub struct LoadSampler {
    query: OwnedPdhQuery,
    counters: Vec<ProcessorCounter>,
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
            let path = format!(
                r"\Processor Information({},{})\% Processor Utility",
                cpu.group, cpu.logical_processor_index
            );
            let wide_path = wide_null(OsStr::new(&path));
            let mut counter = PDH_HCOUNTER::default();
            // SAFETY: The query is live and wide_path is a valid null-terminated UTF-16 string.
            let status = unsafe {
                PdhAddEnglishCounterW(query.0, PCWSTR(wide_path.as_ptr()), 0, &raw mut counter)
            };
            check_pdh("PdhAddEnglishCounterW", status)?;
            counters.push(ProcessorCounter {
                domain: LlcDomainKey {
                    group: cpu.group,
                    last_level_cache_index: cpu.last_level_cache_index,
                },
                path,
                handle: counter,
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

        let mut domains = BTreeMap::<LlcDomainKey, (u64, u64)>::new();
        for counter in &self.counters {
            let mut value = PDH_FMT_COUNTERVALUE::default();
            // SAFETY: The counter belongs to the live query and value is writable.
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
            let utility = unsafe { value.Anonymous.doubleValue };
            if !utility.is_finite() {
                return Err(PlatformError::PdhNonFinite(counter.path.clone()));
            }
            let basis_points = utility_to_basis_points(utility);
            let aggregate = domains.entry(counter.domain).or_default();
            aggregate.0 += u64::from(basis_points);
            aggregate.1 += 1;
        }

        Ok(domains
            .into_iter()
            .map(|(domain, (sum, count))| DomainLoad {
                domain,
                utilization_bps: u16::try_from(sum / count)
                    .expect("an average of clamped basis points fits u16"),
            })
            .collect())
    }
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
    let initial_exclusion = if is_system_process(pid, &image_name) {
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

fn is_system_process(pid: u32, image_name: &str) -> bool {
    const SYSTEM_IMAGES: &[&str] = &[
        "audiodg.exe",
        "conhost.exe",
        "csrss.exe",
        "ctfmon.exe",
        "dwm.exe",
        "explorer.exe",
        "fontdrvhost.exe",
        "idle",
        "lsass.exe",
        "registry",
        "runtimebroker.exe",
        "searchhost.exe",
        "services.exe",
        "shellexperiencehost.exe",
        "sihost.exe",
        "smss.exe",
        "startmenuexperiencehost.exe",
        "svchost.exe",
        "system",
        "taskhostw.exe",
        "textinputhost.exe",
        "wininit.exe",
        "winlogon.exe",
        "winsched-service.exe",
        "winsched-tray.exe",
    ];
    pid <= 4
        || SYSTEM_IMAGES
            .iter()
            .any(|system| image_name.eq_ignore_ascii_case(system))
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
    let handle = unsafe { OpenProcess(access, false, pid)? };
    Ok(OwnedHandle::new(handle))
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
    fn fixed_exclusions_cover_windows_shell_and_service_hosts() {
        for image in [
            "svchost.exe",
            "Explorer.EXE",
            "RuntimeBroker.exe",
            "SearchHost.exe",
            "StartMenuExperienceHost.exe",
            "winsched-tray.exe",
        ] {
            assert!(is_system_process(1_000, image), "{image} must be excluded");
        }
        assert!(!is_system_process(1_000, "game.exe"));
    }
}
