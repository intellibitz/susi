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

#[test]
fn systemd_unit_includes_unit_service_and_install_sections() {
    let unit = render_unit(Platform::Systemd, "/opt/susi/bin/susi");
    assert!(unit.contains("[Unit]"));
    assert!(unit.contains("[Service]"));
    assert!(unit.contains("ExecStart=/opt/susi/bin/susi daemon"));
    assert!(unit.contains("[Install]"));
    assert!(unit.contains("WantedBy=default.target"));
}

#[test]
fn launchd_plist_renders_xml_header_and_program_arguments_array() {
    let plist = render_unit(Platform::Launchd, "/usr/local/bin/susi");
    assert!(plist.starts_with("<?xml version=\"1.0\"?>"));
    assert!(plist.contains("<key>ProgramArguments</key>"));
    assert!(plist.contains("<string>/usr/local/bin/susi</string><string>daemon</string>"));
}

#[test]
fn windows_sc_command_quotes_binary_path_and_sets_demand_start() {
    assert_eq!(Platform::Windows, Platform::Windows);
    assert_ne!(Platform::Windows, Platform::Systemd);
    let cmd = render_unit(Platform::Windows, "C:\\Program Files\\susi\\susi.exe");
    assert_eq!(
        cmd,
        "sc.exe create susi binPath= \"C:\\Program Files\\susi\\susi.exe daemon\" start= demand\n"
    );
}

#[test]
fn platform_renders_distinct_output_for_identical_binary_path() {
    let bin = "/opt/susi/susi";
    let sys = render_unit(Platform::Systemd, bin);
    let mac = render_unit(Platform::Launchd, bin);
    let win = render_unit(Platform::Windows, bin);
    assert_ne!(sys, mac);
    assert_ne!(sys, win);
    assert_ne!(mac, win);
}
