#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

//! Cross-platform service and lifecycle manager.
//!
//! Provides unified background service lifecycle and
//! service definition templates across Linux (`systemd`), macOS (`launchd`),
//! and Windows (`schtasks` / background worker).

use std::path::Path;
use std::process::Command;

/// Host operating system classification for service orchestration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformKind {
    Linux,
    MacOS,
    Windows,
    Other,
}

impl PlatformKind {
    /// Detect the current target platform.
    #[must_use]
    pub const fn current() -> Self {
        if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(target_os = "macos") {
            Self::MacOS
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Other
        }
    }

    /// Descriptive name of the platform.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::MacOS => "macos",
            Self::Windows => "windows",
            Self::Other => "other",
        }
    }
}

/// The service management subsystem available on the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceManagerKind {
    Systemd,
    Launchd,
    WindowsTask,
    ProcessFallback,
}

impl ServiceManagerKind {
    /// Detect the available service manager on this platform.
    #[must_use]
    pub fn detect(platform: PlatformKind) -> Self {
        match platform {
            PlatformKind::Linux => {
                let has_systemctl = Command::new("systemctl")
                    .arg("--version")
                    .output()
                    .is_ok_and(|out| out.status.success());
                if has_systemctl {
                    Self::Systemd
                } else {
                    Self::ProcessFallback
                }
            }
            PlatformKind::MacOS => {
                let has_launchctl = Command::new("launchctl")
                    .arg("help")
                    .output()
                    .is_ok_and(|out| out.status.success());
                if has_launchctl {
                    Self::Launchd
                } else {
                    Self::ProcessFallback
                }
            }
            PlatformKind::Windows => {
                let has_schtasks = Command::new("schtasks.exe")
                    .arg("/?")
                    .output()
                    .is_ok_and(|out| out.status.success());
                if has_schtasks {
                    Self::WindowsTask
                } else {
                    Self::ProcessFallback
                }
            }
            PlatformKind::Other => Self::ProcessFallback,
        }
    }

    /// Primary unit or job name for the susi daemon.
    #[must_use]
    pub const fn daemon_unit_name(self) -> &'static str {
        match self {
            Self::Systemd => "susi.service",
            Self::Launchd => "com.intellibitz.susi.daemon",
            Self::WindowsTask => "susi-daemon",
            Self::ProcessFallback => "susi-daemon-fallback",
        }
    }

    /// Primary unit or job name for local release sync.
    #[must_use]
    pub const fn release_sync_unit_name(self) -> &'static str {
        match self {
            Self::Systemd => "susi-release-sync.service",
            Self::Launchd => "com.intellibitz.susi.release-sync",
            Self::WindowsTask => "susi-release-sync",
            Self::ProcessFallback => "susi-release-sync-fallback",
        }
    }
}

/// Diagnostic report for cross-platform background persistence and OS readiness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformDiagnostics {
    pub platform: PlatformKind,
    pub service_manager: ServiceManagerKind,
    pub is_wsl: bool,
    pub wsl_systemd_enabled: bool,
    pub linux_session_linger: bool,
    pub gpu_script_available: bool,
}

impl PlatformDiagnostics {
    /// Inspect the host OS and environment to report persistence and GPU capabilities.
    #[must_use]
    pub fn inspect() -> Self {
        let platform = PlatformKind::current();
        let service_manager = ServiceManagerKind::detect(platform);

        let is_wsl = if platform == PlatformKind::Linux {
            std::fs::read_to_string("/proc/version").is_ok_and(|v| {
                let lower = v.to_ascii_lowercase();
                lower.contains("microsoft") || lower.contains("wsl")
            })
        } else {
            false
        };

        let wsl_systemd_enabled = if is_wsl {
            service_manager == ServiceManagerKind::Systemd
        } else {
            true
        };

        let linux_session_linger = if platform == PlatformKind::Linux {
            let user = std::env::var("USER").unwrap_or_default();
            Command::new("loginctl")
                .args(["show-user", &user])
                .output()
                .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains("Linger=yes"))
        } else {
            true
        };

        let gpu_script_available = find_script_path("build-gpu.sh").is_some()
            || find_script_path("build-gpu.ps1").is_some();

        Self {
            platform,
            service_manager,
            is_wsl,
            wsl_systemd_enabled,
            linux_session_linger,
            gpu_script_available,
        }
    }
}

/// Helper to locate a repository script relative to the current working directory.
fn find_script_path(script: &str) -> Option<std::path::PathBuf> {
    let p = Path::new(script);
    if p.exists() {
        return Some(p.to_path_buf());
    }
    let p_up = Path::new("../..").join(script);
    if p_up.exists() {
        return Some(p_up);
    }
    None
}

const SERVICE_ENV: [(&str, &str); 4] = [
    ("SUSI_UNATTENDED", "1"),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GCM_INTERACTIVE", "never"),
    ("SSH_ASKPASS_REQUIRE", "never"),
];

/// Render a Linux systemd user service unit definition.
#[must_use]
pub fn render_systemd_unit(binary_path: &Path) -> String {
    let env: String = SERVICE_ENV
        .iter()
        .map(|(k, v)| format!("Environment=\"{k}={v}\"\n"))
        .collect();
    let bin = binary_path.display();
    format!(
        "[Unit]\nDescription=SUSI Autonomous Agent Daemon\nAfter=network.target\n\n\
         [Service]\nType=simple\n{env}EnvironmentFile=-%h/.susi/service.env\n\
         ExecStart={bin}\nRestart=always\nRestartSec=5s\n\n[Install]\nWantedBy=default.target\n"
    )
}

/// Render a macOS launchd plist definition.
#[must_use]
pub fn render_launchd_plist(binary_path: &Path) -> String {
    let env: String = SERVICE_ENV
        .iter()
        .map(|(k, v)| format!("        <key>{k}</key><string>{v}</string>\n"))
        .collect();
    let bin = binary_path.display();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n\
         \x20   <key>Label</key><string>com.intellibitz.susi.daemon</string>\n\
         \x20   <key>ProgramArguments</key><array><string>{bin}</string><string>daemon</string></array>\n\
         \x20   <key>EnvironmentVariables</key><dict>\n{env}    </dict>\n\
         \x20   <key>KeepAlive</key><true/>\n    <key>RunAtLoad</key><true/>\n\
         </dict>\n</plist>\n"
    )
}

/// Alias for [`PlatformKind`] used by service installation/uninstallation helpers.
pub type ServicePlatform = PlatformKind;

/// Declarative configuration for a background daemon service unit or scheduled task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceConfig {
    pub name: String,
    pub binary_path: std::path::PathBuf,
}

impl ServiceConfig {
    #[must_use]
    pub fn new(name: impl Into<String>, binary_path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            name: name.into(),
            binary_path: binary_path.into(),
        }
    }

    #[must_use]
    pub fn default_daemon(binary_path: impl Into<std::path::PathBuf>) -> Self {
        Self::new("susi-daemon", binary_path)
    }
}

/// Render a Windows PowerShell command to register the daemon as a Scheduled Task
/// under least privilege (`-RunLevel Limited`, matching `install.ps1`).
#[must_use]
pub fn render_windows_schtasks_cmd(binary_path: &Path) -> String {
    format!(
        "$action = New-ScheduledTaskAction -Execute '{}' -Argument 'daemon'; \
         $trigger = New-ScheduledTaskTrigger -AtLogOn; \
         $principal = New-ScheduledTaskPrincipal -UserId $env:USERNAME -RunLevel Limited; \
         Register-ScheduledTask -TaskName 'susi-daemon' -Action $action -Trigger $trigger -Principal $principal -Force",
        binary_path.display()
    )
}

/// Render the Windows `schtasks` command to delete a registered Scheduled Task.
#[must_use]
pub fn render_windows_schtasks_delete_cmd(config: &ServiceConfig) -> String {
    format!("schtasks /Delete /TN \"{}\" /F", config.name)
}

/// Render the OS-appropriate service uninstallation command for `platform`.
#[must_use]
pub fn uninstall_service_command(platform: ServicePlatform, config: &ServiceConfig) -> String {
    match platform {
        PlatformKind::Linux => format!("systemctl --user disable --now {}.service", config.name),
        PlatformKind::MacOS => format!(
            "launchctl unload ~/Library/LaunchAgents/com.intellibitz.susi.{}.plist",
            config.name
        ),
        PlatformKind::Windows => render_windows_schtasks_delete_cmd(config),
        PlatformKind::Other => format!("pkill -f {}", config.name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn platform_detection_is_consistent() {
        let p = PlatformKind::current();
        assert!(!p.as_str().is_empty());
        if cfg!(target_os = "windows") {
            assert_eq!(p, PlatformKind::Windows);
        } else if cfg!(target_os = "linux") {
            assert_eq!(p, PlatformKind::Linux);
        } else if cfg!(target_os = "macos") {
            assert_eq!(p, PlatformKind::MacOS);
        }
    }

    #[test]
    fn service_unit_names_are_structured() {
        for manager in [
            ServiceManagerKind::Systemd,
            ServiceManagerKind::Launchd,
            ServiceManagerKind::WindowsTask,
            ServiceManagerKind::ProcessFallback,
        ] {
            assert!(!manager.daemon_unit_name().is_empty());
            assert!(!manager.release_sync_unit_name().is_empty());
        }
    }

    #[test]
    fn systemd_unit_renders_cleanly() {
        let bin = PathBuf::from("/usr/local/bin/susi");
        let rendered = render_systemd_unit(&bin);
        assert!(rendered.contains("Description=SUSI Autonomous Agent Daemon"));
        assert!(rendered.contains("/usr/local/bin/susi"));
        assert!(rendered.contains("Restart=always"));
    }

    #[test]
    fn launchd_plist_renders_cleanly() {
        let bin = PathBuf::from("/usr/local/bin/susi");
        let rendered = render_launchd_plist(&bin);
        assert!(rendered.contains("com.intellibitz.susi.daemon"));
        assert!(rendered.contains("<string>/usr/local/bin/susi</string>"));
        assert!(rendered.contains("<key>KeepAlive</key>"));
    }

    #[test]
    fn windows_schtasks_renders_cleanly() {
        let bin = PathBuf::from("C:\\Users\\user\\.susi\\bin\\susi.exe");
        let rendered = render_windows_schtasks_cmd(&bin);
        assert!(rendered.contains("Register-ScheduledTask -TaskName 'susi-daemon'"));
        assert!(rendered.contains("-RunLevel Limited"));
        assert!(!rendered.contains("-RunLevel Highest"));
        assert!(rendered.contains("C:\\Users\\user\\.susi\\bin\\susi.exe"));
    }

    #[test]
    fn windows_schtasks_delete_and_cross_platform_uninstall_render_cleanly() {
        let cfg = ServiceConfig::default_daemon("C:\\Users\\user\\.susi\\bin\\susi.exe");
        assert_eq!(
            render_windows_schtasks_delete_cmd(&cfg),
            "schtasks /Delete /TN \"susi-daemon\" /F"
        );
        assert_eq!(
            uninstall_service_command(PlatformKind::Windows, &cfg),
            "schtasks /Delete /TN \"susi-daemon\" /F"
        );
        assert!(uninstall_service_command(PlatformKind::Linux, &cfg)
            .contains("systemctl --user disable --now susi-daemon.service"));
        assert!(uninstall_service_command(PlatformKind::MacOS, &cfg).contains("launchctl unload"));
    }

    #[test]
    fn platform_diagnostics_inspects_host_capabilities() {
        let diag = PlatformDiagnostics::inspect();
        assert_eq!(diag.platform, PlatformKind::current());
        assert!(diag.gpu_script_available);
    }
}
