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
//! Provides unified background service lifecycle, release promotion, and
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

/// Result of attempting to trigger a local release promotion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotionOutcome {
    pub success: bool,
    pub manager: ServiceManagerKind,
    pub message: String,
}

/// Trigger local release promotion using the host's platform-native mechanism.
#[must_use]
pub fn promote_release_cross_platform() -> PromotionOutcome {
    let platform = PlatformKind::current();
    let manager = ServiceManagerKind::detect(platform);

    match manager {
        ServiceManagerKind::Systemd => {
            const UNIT: &str = "susi-release-sync.service";
            let started = Command::new("systemctl")
                .args(["--user", "start", "--no-block", UNIT])
                .output()
                .is_ok_and(|out| out.status.success());

            if started {
                PromotionOutcome {
                    success: true,
                    manager,
                    message: format!(
                        "Local promotion queued ({UNIT}): the release is built for this host and \
                         installed once this command exits. Follow: journalctl --user -fu {UNIT}"
                    ),
                }
            } else {
                PromotionOutcome {
                    success: false,
                    manager,
                    message: "Local susi not updated: the release-sync timer is not installed. \
                              Run scripts/susi-release-sync.sh (once) or --install-timer (always)."
                        .to_string(),
                }
            }
        }
        ServiceManagerKind::Launchd => {
            const LABEL: &str = "com.intellibitz.susi.release-sync";
            let started = Command::new("launchctl")
                .args(["start", LABEL])
                .output()
                .is_ok_and(|out| out.status.success());

            if started {
                PromotionOutcome {
                    success: true,
                    manager,
                    message: format!(
                        "Local promotion queued via launchd ({LABEL}): the release will install in background. \
                         Follow: log stream --predicate 'subsystem == \"com.intellibitz.susi\"'"
                    ),
                }
            } else {
                // Attempt direct background fallback invocation
                let fallback = spawn_background_script("scripts/susi-release-sync.sh");
                PromotionOutcome {
                    success: fallback,
                    manager,
                    message: if fallback {
                        "Local promotion spawned via background shell worker. Follow: ~/.susi/release-sync.log".to_string()
                    } else {
                        "Local susi not updated: launchd service not registered and background script spawn failed. \
                         Run scripts/susi-release-sync.sh manually."
                            .to_string()
                    },
                }
            }
        }
        ServiceManagerKind::WindowsTask => {
            const TASK: &str = "susi-release-sync";
            let started = Command::new("schtasks.exe")
                .args(["/run", "/tn", TASK])
                .output()
                .is_ok_and(|out| out.status.success());

            if started {
                PromotionOutcome {
                    success: true,
                    manager,
                    message: format!(
                        "Local promotion queued via Windows Scheduled Task ({TASK}). Follow: ~/.susi/release-sync.log"
                    ),
                }
            } else {
                // Attempt powershell background spawn fallback
                let fallback = spawn_windows_background_script();
                PromotionOutcome {
                    success: fallback,
                    manager,
                    message: if fallback {
                        "Local promotion queued via Windows background process. Follow: ~/.susi/release-sync.log".to_string()
                    } else {
                        "Local susi not updated: Windows task not found and background process spawn failed. \
                         Run scripts/susi-release-sync.sh from Git Bash or install scheduled task via install.ps1 -Service."
                            .to_string()
                    },
                }
            }
        }
        ServiceManagerKind::ProcessFallback => {
            let fallback = spawn_background_script("scripts/susi-release-sync.sh");
            PromotionOutcome {
                success: fallback,
                manager,
                message: if fallback {
                    "Local promotion spawned in background. Follow: ~/.susi/release-sync.log"
                        .to_string()
                } else {
                    "Local susi not updated: no native service manager detected. \
                     Run scripts/susi-release-sync.sh manually."
                        .to_string()
                },
            }
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

/// Helper to launch a shell script detached in the background on POSIX systems.
fn spawn_background_script(script_path: &str) -> bool {
    if let Some(path) = find_script_path(script_path) {
        Command::new("sh")
            .args(["-c", &format!("{} >/dev/null 2>&1 &", path.display())])
            .status()
            .is_ok_and(|s| s.success())
    } else {
        false
    }
}

/// Helper to launch background release-sync on Windows hosts.
fn spawn_windows_background_script() -> bool {
    let git_bash = Path::new("C:\\Program Files\\Git\\bin\\bash.exe");
    if git_bash.exists() {
        if let Some(script) = find_script_path("scripts/susi-release-sync.sh") {
            return Command::new(git_bash).arg(script).spawn().is_ok();
        }
    }
    Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            "Start-Process -FilePath 'susi.exe' -ArgumentList 'release --sync' -WindowStyle Hidden",
        ])
        .spawn()
        .is_ok()
}

/// Render a Linux systemd user service unit definition.
#[must_use]
pub fn render_systemd_unit(binary_path: &Path) -> String {
    format!(
        "[Unit]\n\
         Description=SUSI Autonomous Agent Daemon\n\
         After=network.target\n\n\
         [Service]\n\
         Type=simple\n\
         ExecStart={}\n\
         Restart=always\n\
         RestartSec=5s\n\n\
         [Install]\n\
         WantedBy=default.target\n",
        binary_path.display()
    )
}

/// Render a macOS launchd plist definition.
#[must_use]
pub fn render_launchd_plist(binary_path: &Path) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
             <key>Label</key>\n\
             <string>com.intellibitz.susi.daemon</string>\n\
             <key>ProgramArguments</key>\n\
             <array>\n\
                 <string>{}</string>\n\
                 <string>daemon</string>\n\
             </array>\n\
             <key>KeepAlive</key>\n\
             <true/>\n\
             <key>RunAtLoad</key>\n\
             <true/>\n\
         </dict>\n\
         </plist>\n",
        binary_path.display()
    )
}

/// Render a Windows PowerShell command to register the daemon as a Scheduled Task.
#[must_use]
pub fn render_windows_schtasks_cmd(binary_path: &Path) -> String {
    format!(
        "$action = New-ScheduledTaskAction -Execute '{}' -Argument 'daemon'; \
         $trigger = New-ScheduledTaskTrigger -AtLogOn; \
         Register-ScheduledTask -TaskName 'susi-daemon' -Action $action -Trigger $trigger -Force",
        binary_path.display()
    )
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
        assert!(rendered.contains("C:\\Users\\user\\.susi\\bin\\susi.exe"));
    }

    #[test]
    fn promote_release_cross_platform_returns_outcome() {
        let outcome = promote_release_cross_platform();
        assert!(!outcome.message.is_empty());
    }

    #[test]
    fn platform_diagnostics_inspects_host_capabilities() {
        let diag = PlatformDiagnostics::inspect();
        assert_eq!(diag.platform, PlatformKind::current());
        assert!(diag.gpu_script_available);
    }
}
