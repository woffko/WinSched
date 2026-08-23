//! Platform-independent tray menu presentation model.

#![forbid(unsafe_code)]

use winsched_config::ControllerMode;
use winsched_control::{ControllerPhase, ControllerStatus};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceViewState {
    Missing,
    Stopped,
    Running,
    StartPending,
    StopPending,
    Other(String),
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuModel {
    pub header: String,
    pub scheduling_action: String,
    pub scheduling_action_enabled: bool,
    pub service_action: String,
    pub service_action_enabled: bool,
    pub mode: String,
    pub managed: String,
    pub activity: String,
    pub error: String,
    pub tooltip: String,
}

#[must_use]
pub fn build_menu_model(
    service: &ServiceViewState,
    status: Option<&ControllerStatus>,
    ui_error: Option<&str>,
) -> MenuModel {
    let running = matches!(service, ServiceViewState::Running);
    let scheduling_enabled = status.is_some_and(|status| status.scheduling_enabled);
    let scheduling_is_configured =
        status.is_some_and(|status| matches!(status.configured_mode, ControllerMode::Auto));
    let (service_action, service_action_enabled) = match service {
        ServiceViewState::Running => ("Stop Service", true),
        ServiceViewState::Stopped => ("Start Service", true),
        ServiceViewState::Missing => ("Service Not Installed", false),
        ServiceViewState::StartPending => ("Service Starting...", false),
        ServiceViewState::StopPending => ("Service Stopping...", false),
        ServiceViewState::Other(_) | ServiceViewState::Error(_) => ("Service Unavailable", false),
    };
    let service_label = service_label(service);
    let effective_label = status.map_or("Status unavailable".to_owned(), effective_label);
    let configured_mode = status.map_or("Unknown", |status| mode_label(status.configured_mode));
    let managed = status.map_or(0, |status| status.managed_processes);
    let activity = status
        .and_then(|status| status.last_activity.as_deref())
        .map_or_else(
            || "Last activity: none".to_owned(),
            |value| format!("Last activity: {}", menu_text(value, 96)),
        );
    let error = ui_error
        .map(ToOwned::to_owned)
        .or_else(|| status.and_then(|status| status.last_error.clone()))
        .or_else(|| match service {
            ServiceViewState::Error(error) => Some(error.clone()),
            _ => None,
        });

    MenuModel {
        header: format!("WinSched — {service_label} / {effective_label}"),
        scheduling_action: if !scheduling_is_configured {
            status.map_or("Scheduling Unavailable".to_owned(), |status| {
                match status.configured_mode {
                    ControllerMode::Off => "Scheduling Disabled (Off Mode)".to_owned(),
                    ControllerMode::Observe => "Scheduling Disabled (Observe Mode)".to_owned(),
                    ControllerMode::Auto => unreachable!("auto handled above"),
                }
            })
        } else if scheduling_enabled {
            "Disable Scheduling".to_owned()
        } else {
            "Enable Scheduling".to_owned()
        },
        scheduling_action_enabled: running && scheduling_is_configured,
        service_action: service_action.to_owned(),
        service_action_enabled,
        mode: format!("Mode: {configured_mode}"),
        managed: format!("Managed processes: {managed}"),
        activity,
        error: format!(
            "Last error: {}",
            error
                .as_deref()
                .map_or("none".to_owned(), |value| menu_text(value, 96))
        ),
        tooltip: menu_text(
            &format!("WinSched: {service_label}; {effective_label}; managed {managed}"),
            120,
        ),
    }
}

fn service_label(service: &ServiceViewState) -> String {
    match service {
        ServiceViewState::Missing => "Service not installed".to_owned(),
        ServiceViewState::Stopped => "Service stopped".to_owned(),
        ServiceViewState::Running => "Service running".to_owned(),
        ServiceViewState::StartPending => "Service starting".to_owned(),
        ServiceViewState::StopPending => "Service stopping".to_owned(),
        ServiceViewState::Other(state) => format!("Service {state}"),
        ServiceViewState::Error(_) => "Service status error".to_owned(),
    }
}

fn effective_label(status: &ControllerStatus) -> String {
    if !status.scheduling_enabled || status.phase == ControllerPhase::Disabled {
        return "Scheduling disabled".to_owned();
    }
    match status.configured_mode {
        ControllerMode::Off => "Configured off".to_owned(),
        ControllerMode::Observe => "Observe only".to_owned(),
        ControllerMode::Auto => "Scheduling enabled".to_owned(),
    }
}

const fn mode_label(mode: ControllerMode) -> &'static str {
    match mode {
        ControllerMode::Off => "Off",
        ControllerMode::Observe => "Observe",
        ControllerMode::Auto => "Auto",
    }
}

fn menu_text(value: &str, max_chars: usize) -> String {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= max_chars {
        return compact;
    }
    let keep = max_chars.saturating_sub(1);
    let mut truncated = compact.chars().take(keep).collect::<String>();
    truncated.push('…');
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;
    use winsched_control::{ControllerStatus, STATUS_SCHEMA_VERSION};

    fn status(enabled: bool, mode: ControllerMode) -> ControllerStatus {
        ControllerStatus {
            schema_version: STATUS_SCHEMA_VERSION,
            phase: if enabled {
                ControllerPhase::Running
            } else {
                ControllerPhase::Disabled
            },
            service_pid: 42,
            scheduling_enabled: enabled,
            configured_mode: mode,
            iteration: 7,
            managed_processes: 3,
            llc_domains: 2,
            last_activity: Some("assigned game.exe to LLC 1".to_owned()),
            last_error: None,
            updated_at_unix_ms: 100,
        }
    }

    #[test]
    fn running_auto_service_exposes_disable_and_stop_actions() {
        let status = status(true, ControllerMode::Auto);
        let model = build_menu_model(&ServiceViewState::Running, Some(&status), None);
        assert_eq!(model.scheduling_action, "Disable Scheduling");
        assert!(model.scheduling_action_enabled);
        assert_eq!(model.service_action, "Stop Service");
        assert!(model.header.contains("Scheduling enabled"));
        assert_eq!(model.managed, "Managed processes: 3");
    }

    #[test]
    fn disabled_runtime_exposes_enable_without_losing_configured_mode() {
        let status = status(false, ControllerMode::Auto);
        let model = build_menu_model(&ServiceViewState::Running, Some(&status), None);
        assert_eq!(model.scheduling_action, "Enable Scheduling");
        assert_eq!(model.mode, "Mode: Auto");
        assert!(model.header.contains("Scheduling disabled"));
    }

    #[test]
    fn stopped_service_only_exposes_start() {
        let model = build_menu_model(&ServiceViewState::Stopped, None, None);
        assert!(!model.scheduling_action_enabled);
        assert_eq!(model.service_action, "Start Service");
        assert!(model.service_action_enabled);
    }

    #[test]
    fn ui_error_takes_precedence_and_is_bounded() {
        let long = "failure ".repeat(30);
        let model = build_menu_model(
            &ServiceViewState::Error("SCM".to_owned()),
            None,
            Some(&long),
        );
        assert!(model.error.starts_with("Last error: failure"));
        assert!(model.error.ends_with('…'));
        assert!(model.error.chars().count() <= "Last error: ".chars().count() + 96);
    }
}
