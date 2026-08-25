#![allow(unsafe_code)]

use std::collections::BTreeSet;
use std::fs;
use std::mem::size_of;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use windows::Win32::Foundation::{ERROR_SUCCESS, HWND, LPARAM, WPARAM};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowW, GetClassNameW, GetWindowThreadProcessId, IsWindowVisible,
    SMTO_ABORTIFHUNG, SMTO_BLOCK, SendMessageTimeoutW, WM_NULL,
};
use windows::core::{BOOL, PCWSTR, w};
use winsched_control::{ControllerStatus, INSTALL_DIRECTORY_NAME, STATUS_FILE_NAME};
use winsched_core::latency::SchedulerLatencyProbe;

use super::{
    DIAGNOSTIC_SCHEMA_VERSION, DiagnosticError, DiagnosticOptions, DiagnosticReport,
    ServiceDiagnostic, ShellDiagnostic, SystemDiagnostic, TaskbarDiagnostic,
    VirtualizationDiagnostic, classify, parse_wsl_config,
};
use crate::platform::{self, ObservedProcess};

const LATENCY_PROBE_INTERVAL: Duration = Duration::from_millis(10);

enum TaskbarSample {
    Success(u64),
    Timeout,
    Unavailable,
}

#[derive(Default)]
struct WindowEnumeration {
    explorer_pids: BTreeSet<u32>,
    explorer_windows: u32,
}

pub(super) fn run(
    options: DiagnosticOptions,
    cancellation: Option<&AtomicBool>,
) -> Result<DiagnosticReport, DiagnosticError> {
    let topology = platform::system_topology()?;
    let (logical_processors, physical_cores) = topology_counts(&topology);
    let (sample_count, latency_window) = sampling_dimensions(options);
    let latency_probe = SchedulerLatencyProbe::start(true, LATENCY_PROBE_INTERVAL, latency_window)?;
    let mut load_sampler = platform::LoadSampler::new(&topology)?;
    let mut pressure_sampler = platform::SystemPressureSampler::new()?;
    load_sampler.prime()?;
    pressure_sampler.prime()?;

    let started = Instant::now();
    let mut cpu_total = 0u64;
    let mut cpu_samples = 0u64;
    let mut maximum_domain = 0u16;
    let mut maximum_queue = 0u32;
    let mut maximum_dpc = 0u16;
    let mut maximum_interrupt = 0u16;
    let mut maximum_pages_input = 0u64;
    let mut total_memory = 0u64;
    let mut minimum_available_memory = u64::MAX;
    let mut taskbar_samples = Vec::with_capacity(usize::try_from(sample_count).unwrap_or(0));

    for sample in 1..=sample_count {
        if cancellation.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            return Err(DiagnosticError::Cancelled);
        }
        let deadline = started + options.interval.saturating_mul(sample);
        let now = Instant::now();
        if now < deadline {
            thread::sleep(deadline - now);
        }
        if cancellation.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            return Err(DiagnosticError::Cancelled);
        }
        let loads = load_sampler.sample()?;
        if !loads.is_empty() {
            cpu_total = cpu_total.saturating_add(
                loads
                    .iter()
                    .map(|load| u64::from(load.utilization_bps))
                    .sum::<u64>()
                    / u64::try_from(loads.len()).unwrap_or(1),
            );
            cpu_samples = cpu_samples.saturating_add(1);
        }
        for load in loads {
            maximum_domain = maximum_domain.max(load.utilization_bps);
            maximum_dpc = maximum_dpc.max(load.dpc_time_bps);
            maximum_interrupt = maximum_interrupt.max(load.interrupt_time_bps);
        }
        let pressure = pressure_sampler.sample()?;
        maximum_queue = maximum_queue.max(pressure.processor_queue_length);
        maximum_pages_input = maximum_pages_input.max(pressure.pages_input_per_second);
        total_memory = pressure.total_physical_memory_bytes;
        minimum_available_memory =
            minimum_available_memory.min(pressure.available_physical_memory_bytes);
        taskbar_samples.push(probe_taskbar(options.taskbar_timeout));
    }

    let processes = platform::observe_processes(&topology)?;
    let shell = shell_diagnostic(&processes, &taskbar_samples);
    let virtualization = virtualization_diagnostic(&processes);
    let captured_at_unix_ms = unix_time_ms();
    let service = service_diagnostic(captured_at_unix_ms);
    let mut report = DiagnosticReport {
        schema_version: DIAGNOSTIC_SCHEMA_VERSION,
        captured_at_unix_ms,
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        sample_count,
        system: SystemDiagnostic {
            logical_processors,
            physical_cores,
            average_cpu_utilization_bps: u16::try_from(cpu_total / cpu_samples.max(1))
                .unwrap_or(10_000),
            maximum_domain_utilization_bps: maximum_domain,
            maximum_processor_queue_length: maximum_queue,
            maximum_dpc_time_bps: maximum_dpc,
            maximum_interrupt_time_bps: maximum_interrupt,
            maximum_pages_input_per_second: maximum_pages_input,
            total_physical_memory_bytes: total_memory,
            minimum_available_memory_bytes: if minimum_available_memory == u64::MAX {
                0
            } else {
                minimum_available_memory
            },
            scheduler_latency: latency_probe.status(),
        },
        shell,
        virtualization,
        service,
        findings: Vec::new(),
    };
    classify(&mut report);
    Ok(report)
}

fn topology_counts(topology: &winsched_core::Topology) -> (u32, u32) {
    let logical = u32::try_from(topology.cpu_sets.len()).unwrap_or(u32::MAX);
    let physical = u32::try_from(
        topology
            .cpu_sets
            .iter()
            .map(|cpu| (cpu.group, cpu.core_index))
            .collect::<BTreeSet<_>>()
            .len(),
    )
    .unwrap_or(u32::MAX);
    (logical, physical)
}

fn sampling_dimensions(options: DiagnosticOptions) -> (u32, usize) {
    let requested_samples = options
        .duration
        .as_millis()
        .div_ceil(options.interval.as_millis());
    let sample_count = u32::try_from(requested_samples.clamp(1, 1_200)).unwrap_or(1_200);
    let latency_window = usize::try_from(
        options
            .duration
            .as_millis()
            .div_ceil(LATENCY_PROBE_INTERVAL.as_millis())
            .saturating_add(100),
    )
    .unwrap_or(12_100);
    (sample_count, latency_window)
}

fn shell_diagnostic(
    processes: &[ObservedProcess],
    taskbar_samples: &[TaskbarSample],
) -> ShellDiagnostic {
    let explorer = processes
        .iter()
        .filter(|process| process.image_name.eq_ignore_ascii_case("explorer.exe"))
        .collect::<Vec<_>>();
    let explorer_pids = explorer
        .iter()
        .map(|process| process.key.pid)
        .collect::<BTreeSet<_>>();
    ShellDiagnostic {
        taskbar: summarize_taskbar(taskbar_samples),
        explorer_processes: u32::try_from(explorer.len()).unwrap_or(u32::MAX),
        explorer_threads: explorer.iter().fold(0u32, |threads, process| {
            threads.saturating_add(process.thread_count)
        }),
        explorer_windows: count_explorer_windows(explorer_pids),
        launch_folders_in_separate_process: read_separate_process(),
    }
}

fn virtualization_diagnostic(processes: &[ObservedProcess]) -> VirtualizationDiagnostic {
    let wsl = processes
        .iter()
        .filter(|process| {
            matches_image(
                &process.image_name,
                &["vmmem", "vmmemwsl", "wslhost.exe", "wslservice.exe"],
            )
        })
        .collect::<Vec<_>>();
    let vmware = processes
        .iter()
        .filter(|process| process.image_name.eq_ignore_ascii_case("vmware-vmx.exe"))
        .collect::<Vec<_>>();
    let wsl_config_contents = std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .map(|profile| profile.join(".wslconfig"))
        .and_then(|path| fs::read_to_string(path).ok());
    VirtualizationDiagnostic {
        wsl_processes: u32::try_from(wsl.len()).unwrap_or(u32::MAX),
        wsl_threads: wsl.iter().fold(0u32, |threads, process| {
            threads.saturating_add(process.thread_count)
        }),
        vmware_vm_processes: u32::try_from(vmware.len()).unwrap_or(u32::MAX),
        vmware_vm_threads: vmware.iter().fold(0u32, |threads, process| {
            threads.saturating_add(process.thread_count)
        }),
        wsl_config: parse_wsl_config(wsl_config_contents.as_deref()),
        wsl_advice: super::WslAdviceDiagnostic::default(),
    }
}

fn service_diagnostic(now_ms: u64) -> ServiceDiagnostic {
    let program_data = std::env::var_os("PROGRAMDATA")
        .map_or_else(|| PathBuf::from(r"C:\ProgramData"), PathBuf::from);
    let status = fs::read(
        program_data
            .join(INSTALL_DIRECTORY_NAME)
            .join(STATUS_FILE_NAME),
    )
    .ok()
    .and_then(|contents| serde_json::from_slice::<ControllerStatus>(&contents).ok());
    status.map_or_else(ServiceDiagnostic::default, |status| ServiceDiagnostic {
        available: true,
        schema_version: Some(status.schema_version),
        phase: Some(status.phase),
        scheduling_enabled: Some(status.scheduling_enabled),
        responsiveness_pressure: Some(status.responsiveness_pressure),
        scheduler_latency_p99_us: Some(status.scheduler_latency.p99_lateness_us),
        status_age_ms: Some(now_ms.saturating_sub(status.updated_at_unix_ms)),
    })
}

fn summarize_taskbar(samples: &[TaskbarSample]) -> TaskbarDiagnostic {
    let mut responses = samples
        .iter()
        .filter_map(|sample| match sample {
            TaskbarSample::Success(duration) => Some(*duration),
            TaskbarSample::Timeout | TaskbarSample::Unavailable => None,
        })
        .collect::<Vec<_>>();
    responses.sort_unstable();
    let timeout_samples = samples
        .iter()
        .filter(|sample| matches!(sample, TaskbarSample::Timeout))
        .count();
    let available = samples
        .iter()
        .any(|sample| !matches!(sample, TaskbarSample::Unavailable));
    TaskbarDiagnostic {
        available,
        samples: u32::try_from(samples.len()).unwrap_or(u32::MAX),
        successful_samples: u32::try_from(responses.len()).unwrap_or(u32::MAX),
        timeout_samples: u32::try_from(timeout_samples).unwrap_or(u32::MAX),
        p50_response_us: percentile(&responses, 50),
        p95_response_us: percentile(&responses, 95),
        maximum_response_us: responses.last().copied().unwrap_or(0),
    }
}

fn percentile(sorted: &[u64], percentile: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = sorted.len().saturating_mul(percentile).div_ceil(100);
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn probe_taskbar(timeout: Duration) -> TaskbarSample {
    // SAFETY: The class and null title are immutable, valid strings for this lookup.
    let Ok(taskbar) = (unsafe { FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()) }) else {
        return TaskbarSample::Unavailable;
    };
    let started = Instant::now();
    let timeout_ms = u32::try_from(timeout.as_millis()).unwrap_or(250);
    // SAFETY: WM_NULL carries no pointers or side effects. The bounded timeout prevents a hung
    // shell thread from blocking this dedicated diagnostic worker indefinitely.
    let result = unsafe {
        SendMessageTimeoutW(
            taskbar,
            WM_NULL,
            WPARAM(0),
            LPARAM(0),
            SMTO_ABORTIFHUNG | SMTO_BLOCK,
            timeout_ms,
            None,
        )
    };
    if result.0 == 0 {
        TaskbarSample::Timeout
    } else {
        TaskbarSample::Success(u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX))
    }
}

fn count_explorer_windows(explorer_pids: BTreeSet<u32>) -> u32 {
    let mut state = WindowEnumeration {
        explorer_pids,
        explorer_windows: 0,
    };
    // SAFETY: The callback is synchronous and lparam points to state for the complete call.
    let _ = unsafe {
        EnumWindows(
            Some(enum_window),
            LPARAM((&raw mut state).cast::<()>() as isize),
        )
    };
    state.explorer_windows
}

unsafe extern "system" fn enum_window(window: HWND, parameter: LPARAM) -> BOOL {
    // SAFETY: count_explorer_windows passes a live WindowEnumeration pointer synchronously.
    let state = unsafe { &mut *(parameter.0 as *mut WindowEnumeration) };
    // SAFETY: window is provided by EnumWindows and is valid during the callback.
    if !unsafe { IsWindowVisible(window) }.as_bool() {
        return BOOL(1);
    }
    let mut pid = 0u32;
    // SAFETY: pid is a valid writable output and window is supplied by EnumWindows.
    unsafe { GetWindowThreadProcessId(window, Some(&raw mut pid)) };
    if !state.explorer_pids.contains(&pid) {
        return BOOL(1);
    }
    let mut class = [0u16; 128];
    // SAFETY: class is a valid writable UTF-16 buffer and window remains valid.
    let length = unsafe { GetClassNameW(window, &mut class) };
    if length > 0 {
        let class = String::from_utf16_lossy(&class[..usize::try_from(length).unwrap_or(0)]);
        if matches!(class.as_str(), "CabinetWClass" | "ExploreWClass") {
            state.explorer_windows = state.explorer_windows.saturating_add(1);
        }
    }
    BOOL(1)
}

fn read_separate_process() -> Option<bool> {
    let mut value = 0u32;
    let mut bytes = u32::try_from(size_of::<u32>()).expect("u32 size fits u32");
    // SAFETY: value and bytes are valid writable outputs and the predefined HKCU handle remains
    // owned by Windows. Only one REG_DWORD value is read.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced"),
            w!("SeparateProcess"),
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut value).cast()),
            Some(&raw mut bytes),
        )
    };
    (status == ERROR_SUCCESS).then_some(value != 0)
}

fn matches_image(image: &str, candidates: &[&str]) -> bool {
    candidates
        .iter()
        .any(|candidate| image.eq_ignore_ascii_case(candidate))
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}
