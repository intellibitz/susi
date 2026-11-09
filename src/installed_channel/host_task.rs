//! One-shot Windows Task Scheduler handoff for packaged installation callers.
//! The child performs the same admission and ordinary-host checks as a direct
//! invocation. Registration starts one least-privilege task for the current user.
use super::package::{id, Result};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const PREFIX: &str = "SusiInstallHost-";
const REPLY: &str = "host-result.json";
type Outcome = std::result::Result<String, String>;

fn xml_path(path: &Path) -> Result<String> {
    let text = path.to_str().ok_or("non-Unicode scheduler path")?;
    if !path.is_absolute() || text.contains('%') || text.chars().any(char::is_control) {
        return Err("scheduler path cannot be represented literally".into());
    }
    Ok(text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;"))
}

fn scheduler(args: &[&std::ffi::OsStr]) -> Result<()> {
    let root = susi_harness::channel_bindings::windows_root()?;
    let system = root.join("System32");
    let mut command = Command::new(system.join("schtasks.exe"));
    command
        .current_dir(&system)
        .env_clear()
        .env("SystemRoot", &root)
        .env("WINDIR", &root)
        .env("PATH", &system)
        .args(args)
        .stdin(Stdio::null());
    susi_paths::console::without_window(&mut command);
    let output = command.output()?;
    if !output.status.success() {
        return Err(format!(
            "Windows Task Scheduler {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(())
}

pub(super) fn reply(result: &Result<String>) -> Result<()> {
    let directory = std::env::current_dir()?;
    let nonce = directory
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_prefix(PREFIX));
    if !nonce.is_some_and(|n| n.len() == 32 && n.bytes().all(|b| b.is_ascii_hexdigit())) {
        return Err("invalid host worker response directory".into());
    }
    // Windows holds the process's working directory open. Release it before
    // publishing completion so the client can remove the response directory.
    std::env::set_current_dir(directory.parent().ok_or("missing host task parent")?)?;
    let name = directory.file_name().ok_or("missing task name")?;
    // The registering client may already have exited. The running task can
    // remove its own definition without interrupting its current Windows process.
    let removed = scheduler(&["/delete".as_ref(), "/tn".as_ref(), name, "/f".as_ref()]);
    let outcome = result
        .as_ref()
        .map_err(ToString::to_string)
        .and_then(|selected| removed.map(|()| selected).map_err(|e| e.to_string()));
    let pending = directory.join("result-pending");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&pending)?;
    file.write_all(&serde_json::to_vec(&outcome)?)?;
    file.sync_all()?;
    drop(file);
    // Publish only complete bytes, and never overwrite an existing response.
    fs::hard_link(&pending, directory.join(REPLY))?;
    fs::remove_file(pending)?;
    Ok(())
}

fn await_result(directory: &Path) -> Result<String> {
    let started = Instant::now();
    loop {
        let opened = fs::File::open(directory.join(REPLY));
        match opened {
            Ok(_) if directory.join("result-pending").try_exists()? => {}
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(65_537).read_to_end(&mut bytes)?;
                if bytes.len() > 65_536 {
                    return Err("oversized host worker response".into());
                }
                let outcome: Outcome = serde_json::from_slice(&bytes)?;
                return Ok(outcome?);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        if started.elapsed() >= Duration::from_secs(600) {
            return Err("Windows host task did not confirm installation within ten minutes".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub(super) fn run(
    release: Option<&str>,
    previous: Option<&str>,
    rollback: bool,
    status: bool,
    expected: Option<&str>,
) -> Result<String> {
    let mut arguments = String::from("install-release --host-worker");
    for (flag, value) in [
        ("--id", release),
        ("--previous", previous),
        ("--expect", expected),
    ] {
        if let Some(value) = value {
            if !id(value) {
                return Err("invalid release ID in host handoff".into());
            }
            arguments.push_str(&format!(" {flag} {value}"));
        }
    }
    if rollback {
        arguments.push_str(" --rollback");
    }
    if status {
        arguments.push_str(" --status");
    }
    let selected = schedule(&std::env::current_exe()?, &arguments)?;
    if !id(&selected)
        || release
            .or(expected)
            .is_some_and(|wanted| wanted != selected)
    {
        return Err("host worker reported a different release".into());
    }
    Ok(selected)
}

pub(super) fn schedule(executable: &Path, arguments: &str) -> Result<String> {
    let name = format!("{PREFIX}{}", susi_config::cluster_key::random_nonce_hex());
    let scratch = std::env::temp_dir().join(&name);
    fs::create_dir(&scratch)?;
    let directory = scratch.canonicalize()?;
    let exe = xml_path(&executable.canonicalize()?)?;
    let arguments = arguments
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let working = xml_path(&directory)?;
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-16"?><Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task"><Triggers><RegistrationTrigger><Enabled>true</Enabled><Delay>PT0S</Delay></RegistrationTrigger></Triggers><Principals><Principal id="user"><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals><Settings><DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>false</StopIfGoingOnBatteries><ExecutionTimeLimit>PT10M</ExecutionTimeLimit></Settings><Actions Context="user"><Exec><Command>{exe}</Command><Arguments>{arguments}</Arguments><WorkingDirectory>{working}</WorkingDirectory></Exec></Actions></Task>"#
    );
    let spec = directory.join("task.xml");
    let utf16 = std::iter::once(0xfeff)
        .chain(xml.encode_utf16())
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    fs::write(&spec, utf16)?;
    scheduler(&[
        "/create".as_ref(),
        "/tn".as_ref(),
        name.as_ref(),
        "/xml".as_ref(),
        spec.as_os_str(),
    ])?;
    let observed = await_result(&directory);
    if directory.join(REPLY).try_exists()? {
        fs::remove_file(spec)?;
        fs::remove_file(directory.join(REPLY))?;
        fs::remove_dir(directory)?;
    } else {
        // Handle an action that never started or exited before writing a reply.
        scheduler(&[
            "/delete".as_ref(),
            "/tn".as_ref(),
            name.as_ref(),
            "/f".as_ref(),
        ])?;
    }
    observed
}
