// The live core installer must keep the native service compatibility launcher.
#![allow(clippy::expect_used)] // Original installer contract assertion.
use std::path::PathBuf;
use susi_gawd::cross_platform_service::render_windows_schtasks_cmd;

#[test]
fn windows_schtasks_renders_cleanly() {
    let bin = PathBuf::from("C:\\Users\\user\\.susi\\bin\\susi.exe");
    let rendered = render_windows_schtasks_cmd(&bin);
    assert!(rendered.contains("Register-ScheduledTask -TaskName 'susi-daemon'"));
    assert!(rendered.contains("-RunLevel Limited"));
    assert!(rendered.contains("-LogonType Interactive"));
    assert!(rendered.contains("wscript.exe"));
    let script = include_str!("../../../install.ps1");
    let compat = script
        .lines()
        .find_map(|line| line.trim().strip_prefix("$Script = '")?.strip_suffix('\''))
        .expect("installer embeds the windowless compatibility launcher");
    assert!(rendered.contains(compat));
    assert!(!rendered.contains("-RunLevel Highest"));
    assert!(rendered.contains("C:\\Users\\user\\.susi\\bin\\susi.exe"));
}
