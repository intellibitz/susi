//! Windowless Windows logon entry point for the adjacent installed Susi.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
fn start() -> std::io::Result<std::process::ExitStatus> {
    let engine = std::env::current_exe()?.with_file_name("susi.exe");
    let mut command = susi_paths::console::background_command(engine);
    command
        .arg("start")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    susi_paths::stdio::spawn_without_inheriting_stdio(&mut command)?.wait()
}

#[cfg(windows)]
fn main() {
    match start() {
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(error) => {
            eprintln!("Could not start installed Susi: {error}");
            std::process::exit(1);
        }
    }
}

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!("susi-background is the Windows logon entry point.");
    std::process::ExitCode::FAILURE
}
