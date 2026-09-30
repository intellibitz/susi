//! susi service install unit rendering for systemd/launchd/Windows.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Platform {
    Systemd,
    Launchd,
    Windows,
}

fn render_unit(platform: Platform, bin: &str) -> String {
    match platform {
        Platform::Systemd => format!(
            "[Unit]\nDescription=susi\n[Service]\nExecStart={bin} daemon\n[Install]\nWantedBy=default.target\n"
        ),
        Platform::Launchd => format!(
            "<?xml version=\"1.0\"?>\n<plist><dict><key>Label</key><string>ai.susi</string><key>ProgramArguments</key><array><string>{bin}</string><string>daemon</string></array></dict></plist>\n"
        ),
        Platform::Windows => format!(
            "sc.exe create susi binPath= \"{bin} daemon\" start= demand\n"
        ),
    }
}

#[test]
fn service_install_renders_per_platform() {
    let bin = "/home/u/.susi/bin/susi";
    assert!(render_unit(Platform::Systemd, bin).contains("ExecStart="));
    assert!(render_unit(Platform::Launchd, bin).contains("ai.susi"));
    assert!(render_unit(Platform::Windows, bin).contains("sc.exe create"));
}
