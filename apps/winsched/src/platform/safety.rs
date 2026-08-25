//! Stable process-name safety boundaries shared by observation and mutation paths.

#![forbid(unsafe_code)]

pub(super) fn is_fixed_system_process(pid: u32, image_name: &str) -> bool {
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
        "vmcompute.exe",
        "vmmem",
        "vmmemwsl",
        "vmwp.exe",
        "wininit.exe",
        "winlogon.exe",
        "winsched-service.exe",
        "winsched-tray.exe",
        "wslhost.exe",
        "wslservice.exe",
    ];
    pid <= 4
        || SYSTEM_IMAGES
            .iter()
            .any(|system| image_name.eq_ignore_ascii_case(system))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_exclusions_cover_shell_service_and_wsl_hosts() {
        for image in [
            "svchost.exe",
            "dwm.exe",
            "Explorer.EXE",
            "RuntimeBroker.exe",
            "SearchHost.exe",
            "ShellExperienceHost.exe",
            "StartMenuExperienceHost.exe",
            "TextInputHost.exe",
            "vmmemWSL",
            "wslhost.exe",
            "wslservice.exe",
            "winsched-tray.exe",
        ] {
            assert!(
                is_fixed_system_process(1_000, image),
                "{image} must be excluded"
            );
        }
        assert!(is_fixed_system_process(4, "unknown"));
        assert!(!is_fixed_system_process(1_000, "game.exe"));
        assert!(!is_fixed_system_process(1_000, "vmware-vmx.exe"));
    }
}
