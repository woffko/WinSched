use std::ffi::OsString;
use std::fs;
use std::num::{NonZeroU16, NonZeroU64};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use thiserror::Error;
use winsched::platform;
use winsched_core::{
    AssignmentPlan, DomainSelector, LlcDomainKey, Topology,
    adaptive::{EnforcementMode, PlacementMode, PolicyAction, PolicyConfig, PolicyEngine},
    plan_assignment,
};

#[derive(Debug, Parser)]
#[command(
    name = "winsched",
    version,
    about = "Inspect and control Windows CPU Set placement"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print the Windows CPU Set and LLC topology.
    Topology {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Sample per-LLC processor utility without changing any process.
    Observe {
        /// Number of samples to collect after the required initial PDH sample.
        #[arg(long, default_value = "5")]
        samples: NonZeroU16,
        /// Milliseconds between samples. Values below 1000 are diagnostic only.
        #[arg(long, default_value = "1000")]
        interval_ms: NonZeroU64,
        /// Emit one JSON object per sample.
        #[arg(long)]
        json: bool,
    },
    /// Enumerate processes and their current default CPU Set placement.
    Processes {
        /// Include protected, realtime, and core system processes.
        #[arg(long)]
        include_excluded: bool,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Evaluate the adaptive policy without applying any decision.
    PolicyObserve {
        /// Number of complete topology/load/process evaluations.
        #[arg(long, default_value = "5")]
        samples: NonZeroU16,
        /// Milliseconds between evaluations.
        #[arg(long, default_value = "1000")]
        interval_ms: NonZeroU64,
        /// Include Ignore and Keep decisions in human-readable output.
        #[arg(long)]
        include_unchanged: bool,
        /// Restrict evaluation to one or more PIDs.
        #[arg(long = "pid")]
        pids: Vec<u32>,
        /// Restrict evaluation to case-insensitive executable names.
        #[arg(long = "image")]
        images: Vec<String>,
        /// Explicitly evaluate every non-excluded process.
        #[arg(long)]
        all_user_processes: bool,
        /// Emit one complete JSON object per evaluation.
        #[arg(long)]
        json: bool,
    },
    /// Parse and validate a controller TOML file without starting the service.
    ConfigCheck {
        path: PathBuf,
        /// Emit the normalized configuration as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Inspect CPU Set placement for one process.
    Inspect {
        pid: u32,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Preview or apply an LLC assignment to one process.
    Apply {
        pid: u32,
        /// `auto` or a group-relative `GROUP:LLC` pair, for example `0:1`.
        #[arg(long, default_value = "auto")]
        llc: String,
        /// Keep only the fastest processor efficiency class in the LLC.
        #[arg(long)]
        performance_only: bool,
        /// Perform the mutation. Without this flag the command is a preview.
        #[arg(long)]
        commit: bool,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Preview or clear the default CPU Sets of one process.
    Clear {
        pid: u32,
        /// Perform the mutation. Without this flag the command is a preview.
        #[arg(long)]
        commit: bool,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Preview or launch a process suspended, assign CPU Sets, and resume it.
    Run {
        /// `auto` or a group-relative `GROUP:LLC` pair, for example `0:1`.
        #[arg(long, default_value = "auto")]
        llc: String,
        /// Keep only the fastest processor efficiency class in the LLC.
        #[arg(long)]
        performance_only: bool,
        /// Launch the process. Without this flag the command is a preview.
        #[arg(long)]
        commit: bool,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
        /// Exact executable path.
        program: PathBuf,
        /// Arguments passed directly to the executable without a shell.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
}

#[derive(Debug, Error)]
enum AppError {
    #[error(transparent)]
    Platform(#[from] platform::PlatformError),
    #[error(transparent)]
    Policy(#[from] winsched_core::PolicyError),
    #[error(transparent)]
    Adaptive(#[from] winsched_core::adaptive::AdaptiveError),
    #[error(transparent)]
    Config(#[from] winsched_config::ConfigError),
    #[error("failed to read configuration: {0}")]
    ConfigIo(#[from] std::io::Error),
    #[error("invalid LLC selector '{0}'; expected 'auto' or 'GROUP:LLC'")]
    InvalidSelector(String),
    #[error("failed to serialize output: {0}")]
    Json(#[from] serde_json::Error),
    #[error("policy-observe requires --pid, --image, or explicit --all-user-processes")]
    PolicyScopeRequired,
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), AppError> {
    match cli.command {
        Command::Topology { json } => {
            let topology = platform::system_topology()?;
            print_topology(&topology, json)?;
        }
        Command::Observe {
            samples,
            interval_ms,
            json,
        } => run_observe(samples, interval_ms, json)?,
        Command::Processes {
            include_excluded,
            json,
        } => run_processes(include_excluded, json)?,
        Command::PolicyObserve {
            samples,
            interval_ms,
            include_unchanged,
            pids,
            images,
            all_user_processes,
            json,
        } => run_policy_observe(
            samples,
            interval_ms,
            include_unchanged,
            &pids,
            &images,
            all_user_processes,
            json,
        )?,
        Command::ConfigCheck { path, json } => run_config_check(&path, json)?,
        Command::Inspect { pid, json } => {
            let snapshot = platform::inspect_process(pid)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&snapshot)?);
            } else {
                println!("PID: {}", snapshot.pid);
                println!("Default CPU Sets: {:?}", snapshot.default_cpu_set_ids);
                print_topology(&snapshot.topology, false)?;
            }
        }
        Command::Apply {
            pid,
            llc,
            performance_only,
            commit,
            json,
        } => {
            let snapshot = platform::inspect_process(pid)?;
            let plan = make_plan(&snapshot.topology, &llc, performance_only)?;
            if commit {
                let report = platform::apply_process(pid, &plan.cpu_set_ids)?;
                print_report(&report, json)?;
            } else {
                print_preview("apply", pid, &snapshot.default_cpu_set_ids, &plan, json)?;
            }
        }
        Command::Clear { pid, commit, json } => {
            let snapshot = platform::inspect_process(pid)?;
            if commit {
                let report = platform::clear_process(pid)?;
                print_report(&report, json)?;
            } else {
                let preview = platform::MutationReport::preview_clear(
                    pid,
                    snapshot.default_cpu_set_ids.clone(),
                );
                print_report(&preview, json)?;
            }
        }
        Command::Run {
            llc,
            performance_only,
            commit,
            json,
            program,
            args,
        } => {
            let topology = platform::system_topology()?;
            let plan = make_plan(&topology, &llc, performance_only)?;
            if commit {
                let report = platform::run_assigned(&program, &args, &plan.cpu_set_ids)?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                } else {
                    println!("Started PID {}", report.pid);
                    println!("CPU Sets: {:?}", report.cpu_set_ids);
                }
            } else if json {
                println!("{}", serde_json::to_string_pretty(&plan)?);
            } else {
                println!("Preview only; process was not started.");
                println!("Program: {}", program.display());
                println!("Arguments: {args:?}");
                print_plan(&plan);
            }
        }
    }
    Ok(())
}

fn run_observe(samples: NonZeroU16, interval_ms: NonZeroU64, json: bool) -> Result<(), AppError> {
    let topology = platform::system_topology()?;
    let mut load_sampler = platform::LoadSampler::new(&topology)?;
    load_sampler.prime()?;
    for sample in 1..=samples.get() {
        std::thread::sleep(Duration::from_millis(interval_ms.get()));
        let loads = load_sampler.sample()?;
        if json {
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "sample": sample,
                    "domain_loads": loads,
                }))?
            );
        } else {
            println!("sample={sample}");
            for load in loads {
                println!(
                    "  group={} llc={} utility={:.2}%",
                    load.domain.group,
                    load.domain.last_level_cache_index,
                    f64::from(load.utilization_bps) / 100.0
                );
            }
        }
    }
    Ok(())
}

fn run_config_check(path: &PathBuf, json: bool) -> Result<(), AppError> {
    let config = winsched_config::ControllerConfig::from_toml(&fs::read_to_string(path)?)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&config)?);
    } else {
        println!(
            "configuration valid: mode={:?}, interval={}ms, rules={}, all_user_processes={}",
            config.controller_mode,
            config.sample_interval_ms,
            config.rules.len(),
            config.all_user_processes
        );
    }
    Ok(())
}

fn run_processes(include_excluded: bool, json: bool) -> Result<(), AppError> {
    let topology = platform::system_topology()?;
    let mut processes = platform::observe_processes(&topology)?;
    if !include_excluded {
        processes.retain(|process| process.exclusion.is_none());
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&processes)?);
    } else {
        for process in processes {
            println!(
                "pid={} session={:?} image={} threads={} domain={:?} cpu_sets={:?} excluded={:?}",
                process.key.pid,
                process.session_id,
                process.image_name,
                process.thread_count,
                process.current_domain,
                process.default_cpu_set_ids,
                process.exclusion
            );
        }
    }
    Ok(())
}

fn run_policy_observe(
    samples: NonZeroU16,
    interval_ms: NonZeroU64,
    include_unchanged: bool,
    pids: &[u32],
    images: &[String],
    all_user_processes: bool,
    json: bool,
) -> Result<(), AppError> {
    if !all_user_processes && pids.is_empty() && images.is_empty() {
        return Err(AppError::PolicyScopeRequired);
    }
    let topology = platform::system_topology()?;
    let mut load_sampler = platform::LoadSampler::new(&topology)?;
    let mut engine = PolicyEngine::new(PolicyConfig::default())?;
    load_sampler.prime()?;

    for sample in 1..=samples.get() {
        std::thread::sleep(Duration::from_millis(interval_ms.get()));
        let loads = load_sampler.sample()?;
        let mut processes = platform::observe_processes(&topology)?;
        processes.retain(|process| {
            process.exclusion.is_none()
                && (all_user_processes
                    || pids.contains(&process.key.pid)
                    || images
                        .iter()
                        .any(|image| process.image_name.eq_ignore_ascii_case(image)))
        });
        let observations = processes
            .iter()
            .map(|process| {
                process.policy_observation(PlacementMode::Auto, EnforcementMode::Observe)
            })
            .collect::<Vec<_>>();
        let decisions = engine.evaluate(
            u64::from(sample) * interval_ms.get(),
            &topology,
            &loads,
            &observations,
        )?;

        if json {
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "sample": sample,
                    "domain_loads": loads,
                    "processes": processes,
                    "decisions": decisions,
                }))?
            );
            continue;
        }

        println!("sample={sample} processes={}", processes.len());
        for decision in decisions {
            if !include_unchanged
                && matches!(
                    decision.action,
                    PolicyAction::Ignore | PolicyAction::Keep { .. }
                )
            {
                continue;
            }
            let image = processes
                .iter()
                .find(|process| process.key == decision.process)
                .map_or("<exited>", |process| process.image_name.as_str());
            println!(
                "  pid={} image={} enforce={} action={:?} reason={:?}",
                decision.process.pid, image, decision.enforce, decision.action, decision.reason
            );
        }
    }
    Ok(())
}

fn make_plan(
    topology: &Topology,
    selector: &str,
    performance_only: bool,
) -> Result<AssignmentPlan, AppError> {
    plan_assignment(topology, parse_selector(selector)?, performance_only).map_err(Into::into)
}

fn parse_selector(value: &str) -> Result<DomainSelector, AppError> {
    if value.eq_ignore_ascii_case("auto") {
        return Ok(DomainSelector::Auto);
    }
    let Some((group, llc)) = value.split_once(':') else {
        return Err(AppError::InvalidSelector(value.to_owned()));
    };
    let group = group
        .parse::<u16>()
        .map_err(|_| AppError::InvalidSelector(value.to_owned()))?;
    let last_level_cache_index = llc
        .parse::<u8>()
        .map_err(|_| AppError::InvalidSelector(value.to_owned()))?;
    Ok(DomainSelector::Exact(LlcDomainKey {
        group,
        last_level_cache_index,
    }))
}

fn print_topology(topology: &Topology, json: bool) -> Result<(), serde_json::Error> {
    if json {
        println!("{}", serde_json::to_string_pretty(topology)?);
        return Ok(());
    }

    println!(
        "CPU Sets: {}, LLC domains: {}",
        topology.cpu_sets.len(),
        topology.llc_domains.len()
    );
    for domain in &topology.llc_domains {
        println!(
            "group={} llc={} numa={:?} cores={:?} efficiency={:?} cpu_sets={:?}",
            domain.key.group,
            domain.key.last_level_cache_index,
            domain.numa_nodes,
            domain.core_indices,
            domain.efficiency_classes,
            domain.cpu_sets.iter().map(|cpu| cpu.id).collect::<Vec<_>>()
        );
    }
    Ok(())
}

fn print_preview(
    operation: &str,
    pid: u32,
    previous: &[u32],
    plan: &AssignmentPlan,
    json: bool,
) -> Result<(), serde_json::Error> {
    let report = platform::MutationReport::preview_apply(
        operation,
        pid,
        previous.to_vec(),
        plan.cpu_set_ids.clone(),
    );
    print_report(&report, json)
}

fn print_report(report: &platform::MutationReport, json: bool) -> Result<(), serde_json::Error> {
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
    } else {
        println!("Operation: {}", report.operation);
        println!("PID: {}", report.pid);
        println!("Committed: {}", report.committed);
        println!("Previous CPU Sets: {:?}", report.previous_cpu_set_ids);
        println!("Requested CPU Sets: {:?}", report.requested_cpu_set_ids);
        println!("Observed CPU Sets: {:?}", report.observed_cpu_set_ids);
    }
    Ok(())
}

fn print_plan(plan: &AssignmentPlan) {
    println!(
        "LLC: group={}, index={}",
        plan.domain.group, plan.domain.last_level_cache_index
    );
    println!("CPU Sets: {:?}", plan.cpu_set_ids);
    println!("Performance class only: {}", plan.performance_only);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_auto_selector_case_insensitively() {
        assert_eq!(parse_selector("AUTO").unwrap(), DomainSelector::Auto);
    }

    #[test]
    fn parses_exact_selector() {
        assert_eq!(
            parse_selector("2:7").unwrap(),
            DomainSelector::Exact(LlcDomainKey {
                group: 2,
                last_level_cache_index: 7,
            })
        );
    }

    #[test]
    fn rejects_invalid_selector() {
        assert!(matches!(
            parse_selector("llc0"),
            Err(AppError::InvalidSelector(_))
        ));
    }
}
