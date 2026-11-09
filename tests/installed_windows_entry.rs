//! Exercise the actual Susi entry executable and Windows Shell with private fixture
//! releases. Fixture receipts exercise dispatch only; production qualification is separate.
#![cfg(windows)]
#![cfg_attr(test, allow(clippy::unwrap_used))] // Fixture setup and Windows assertions must fail the test immediately.
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[path = "../crates/susi-harness/src/channel_bindings/shell_link.rs"]
mod shell_link;

fn hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|n| format!("{n:02x}"))
        .collect()
}

fn fixture(root: &Path, name: &str) -> (String, PathBuf) {
    release(
        root,
        name,
        &format!(
            r#"use std::io::BufRead;
fn main(){{
 let args:Vec<_>=std::env::args().skip(1).collect();
 let exe=std::env::current_exe().unwrap();
 if args.is_empty(){{
   std::fs::write(exe.with_extension("opened"),format!("{{}}\n{{}}\n{{}}", "{name}",args.len(),std::env::current_dir().unwrap().display())).unwrap();
   return;
 }}
 let data=std::io::stdin().lock().lines().next().unwrap().unwrap();
 println!("{{}}|{{}}|{{}}|{{}}","{name}",args[0],data,std::env::var("SUSI_HOME").unwrap());
 eprintln!("stderr-{name}");
 std::process::exit(37);
}}"#
        ),
    )
}

/// Admit compiled `program` as release slot `name`, with fixture evidence.
fn release(root: &Path, name: &str, program: &str) -> (String, PathBuf) {
    let source = root.join(format!("{name}.rs"));
    fs::write(&source, program).unwrap();
    let exe = root.join(format!("{name}.exe"));
    assert!(Command::new("rustc")
        .arg(&source)
        .arg("-o")
        .arg(&exe)
        .status()
        .unwrap()
        .success());
    let bytes = fs::read(&exe).unwrap();
    let digest = hex(&bytes);
    let other = hex(b"fixture evidence");
    let oid = "a".repeat(40);
    let manifest=format!("susi-local-release-v1\nsource={oid}\ntree={oid}\nartifact-sha256={digest}\nartifact-bytes={}\nhost={}-pc-windows-msvc\nintegration-receipt={oid}\nlock-sha256={other}\ncompiler-sha256={other}\ngates-sha256={other}\nsbom-sha256={other}\nnotices-sha256={other}\nnative-audit-sha256={other}\n",bytes.len(),std::env::consts::ARCH);
    let id = hex(manifest.as_bytes());
    let slot = root.join("releases").join(&id);
    fs::create_dir_all(&slot).unwrap();
    fs::write(slot.join("manifest.txt"), manifest).unwrap();
    fs::rename(exe, slot.join("susi.exe")).unwrap();
    (id, slot)
}

/// Admit `exe` itself as a release slot, as `install` does for the real product.
fn admit(root: &Path, exe: &Path) -> String {
    let bytes = fs::read(exe).unwrap();
    let other = hex(b"fixture evidence");
    let oid = "a".repeat(40);
    let manifest = format!(
        "susi-local-release-v1
source={oid}
tree={oid}
artifact-sha256={}
artifact-bytes={}
host={}-pc-windows-msvc
integration-receipt={oid}
lock-sha256={other}
compiler-sha256={other}
gates-sha256={other}
sbom-sha256={other}
notices-sha256={other}
native-audit-sha256={other}
",
        hex(&bytes),
        bytes.len(),
        std::env::consts::ARCH
    );
    let id = hex(manifest.as_bytes());
    let slot = root.join("releases").join(&id);
    fs::create_dir_all(&slot).unwrap();
    fs::write(slot.join("manifest.txt"), manifest).unwrap();
    fs::copy(exe, slot.join("susi.exe")).unwrap();
    id
}

/// When the entry is the current release, commands run in the entry process
/// itself: no second 51 MB image is hashed or spawned. The one-time proof is
/// keyed to the entry file and repeats when the file changes.
#[test]
fn entry_that_is_the_current_release_runs_in_place() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let profile = std::env::temp_dir().join(format!("susi-inplace-{stamp}"));
    let root = profile.join("AppData/Local/SusiLocal/production");
    let entry = root.join("entry/susi.exe");
    fs::create_dir_all(entry.parent().unwrap()).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_susi"), &entry).unwrap();
    let id = admit(&root, &entry);
    select(&root, &id, None);
    for _ in 0..2 {
        let out = command(&entry, &profile).arg("--version").output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(String::from_utf8(out.stdout).unwrap().starts_with("susi "));
    }
    let receipt = fs::read_to_string(root.join("entry-receipt-v1.txt")).unwrap();
    assert!(
        receipt.starts_with(&format!(
            "release={id}
"
        )),
        "{receipt}"
    );
    // Removing the release slot proves no child is launched on the fast path.
    fs::remove_file(root.join("releases").join(&id).join("susi.exe")).unwrap();
    let out = command(&entry, &profile).arg("--version").output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    fs::remove_dir_all(profile).unwrap();
}

/// A release that detaches a background child the way `susi start` starts the
/// daemon: its own standard handles stop being inheritable first. Through the
/// dispatcher the caller's pipes must still close when the release exits, not
/// when the detached child does.
#[test]
fn dispatched_release_does_not_leak_caller_pipes_into_detached_children() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let profile = std::env::temp_dir().join(format!("susi-detach-{stamp}"));
    let root = profile.join("AppData/Local/SusiLocal/production");
    let entry = root.join("entry/susi.exe");
    fs::create_dir_all(entry.parent().unwrap()).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_susi"), &entry).unwrap();
    let (id, slot) = release(
        &root,
        "D",
        r#"use std::os::windows::io::AsRawHandle;
#[link(name = "kernel32")]
extern "system" { fn SetHandleInformation(h: *mut std::ffi::c_void, mask: u32, flags: u32) -> i32; }
fn main() {
    if std::env::args().nth(1).as_deref() == Some("linger") {
        let pid = std::env::current_exe().unwrap().with_extension("pid");
        std::fs::write(pid, std::process::id().to_string()).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(180));
        return;
    }
    for h in [std::io::stdin().as_raw_handle(), std::io::stdout().as_raw_handle(), std::io::stderr().as_raw_handle()] {
        unsafe { SetHandleInformation(h as _, 1, 0) };
    }
    std::process::Command::new(std::env::current_exe().unwrap())
        .arg("linger")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    println!("detached");
}"#,
    );
    select(&root, &id, None);
    let started = Instant::now();
    let out = command(&entry, &profile)
        .arg("start")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let elapsed = started.elapsed();
    // The detached child outlives the release by minutes; the launcher's own
    // checks take seconds (more in a debug build), never that long.
    // The detached child writes its pid once it is running, which on a busy
    // host can be after the launcher has already returned: wait for it.
    let pid_file = slot.join("susi.pid");
    let waited = Instant::now();
    let pid = loop {
        match fs::read_to_string(&pid_file) {
            Ok(pid) if pid.trim().parse::<u32>().is_ok() => break pid,
            _ => {
                assert!(
                    waited.elapsed() < Duration::from_secs(60),
                    "the detached child never wrote {}",
                    pid_file.display()
                );
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    let _ = susi_paths::console::background_command("taskkill")
        .args(["/F", "/PID", pid.trim()])
        .output();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), "detached");
    assert!(
        elapsed < Duration::from_secs(90),
        "caller pipes stayed open {elapsed:?} until the detached child exited"
    );
    std::thread::sleep(Duration::from_millis(500));
    fs::remove_dir_all(profile).unwrap();
}

fn select(root: &Path, current: &str, previous: Option<&str>) {
    let record = format!(
        r#"{{"current":"{current}","previous":{},"dispatcher":"{current}"}}"#,
        serde_json::to_string(&previous).unwrap()
    );
    let receipt = format!(
        r#"{{"value":{record},"sha256":"{}"}}"#,
        hex(record.as_bytes())
    );
    let staged = root.join("next-selector");
    fs::write(&staged, receipt).unwrap();
    fs::rename(staged, root.join("admitted-release-v1.json")).unwrap();
}

fn command(exe: &Path, profile: &Path) -> Command {
    let mut command = Command::new(exe);
    command
        .env_clear()
        .current_dir(profile)
        .creation_flags(0x08000000);
    let system = std::env::var_os("SystemRoot").unwrap();
    let drive = Path::new(&system)
        .ancestors()
        .last()
        .unwrap()
        .to_string_lossy();
    let program_data = profile.join("ProgramData");
    fs::create_dir_all(&program_data).unwrap();
    command
        .env("SystemDrive", drive.trim_end_matches(['\\', '/']))
        .env("ProgramData", &program_data)
        .env("ALLUSERSPROFILE", &program_data)
        .env("SystemRoot", &system)
        .env("WINDIR", &system)
        .env("PATH", Path::new(&system).join("System32"))
        .env("TEMP", profile)
        .env("TMP", profile);
    susi_paths::test_env::isolate_home(&mut command, profile);
    command
}

#[test]
fn actual_windows_susi_dispatches_pipes_and_windows_opens_its_unicode_shortcut() {
    run_installed_entry(false);
}

#[test]
fn windows_shell_opens_a_deep_unicode_profile_with_a_long_release_slot() {
    run_installed_entry(true);
}

fn run_installed_entry(deep: bool) {
    use std::os::windows::ffi::OsStrExt;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let mut label = format!("susi-普通-{stamp}");
    if deep {
        let length = std::env::temp_dir()
            .join(&label)
            .as_os_str()
            .encode_wide()
            .count();
        label.push_str(&"d".repeat(194usize.saturating_sub(length)));
    }
    let profile = std::env::temp_dir().join(label);
    let root = profile.join("AppData/Local/SusiLocal/production");
    let entry = root.join("entry/susi.exe");
    fs::create_dir_all(entry.parent().unwrap()).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_susi"), &entry).unwrap();
    let (a, slot) = fixture(&root, "A");
    if deep {
        assert!(slot.join("susi.exe").as_os_str().encode_wide().count() > 260);
    }
    let (b, _) = fixture(&root, "B");
    let argument = r#"Δ with spaces and "quotes"\last"#;
    for (tag, current, previous) in [
        ("A", a.as_str(), None),
        ("B", b.as_str(), Some(a.as_str())),
        ("A", a.as_str(), Some(b.as_str())),
    ] {
        select(&root, current, previous);
        let mut child = command(&entry, &profile)
            .arg(argument)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"pipe EOF\n")
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(37));
        assert_eq!(
            String::from_utf8(output.stdout).unwrap().trim(),
            format!(
                "{tag}|{argument}|pipe EOF|{}",
                profile.join(".susi").display()
            )
        );
        assert_eq!(
            String::from_utf8(output.stderr).unwrap().trim(),
            format!("stderr-{tag}")
        );
    }
    let work = root.join("workspace Ω");
    fs::create_dir(&work).unwrap();
    let link = profile.join("Susi 启动.lnk");
    assert!(shell_link::legacy_local_target(b"unrecognized link").is_err());
    let encoded = shell_link::encode(&entry, &work).unwrap();
    // The link opens bare `susi` (usage): no HasArguments flag, no "shell".
    // Susi's retired encoding launched the removed `susi shell` REPL.
    let flags = u32::from_le_bytes(encoded[20..24].try_into().unwrap());
    assert_eq!(flags & 0x20, 0, "Start Menu link must carry no arguments");
    let retired = shell_link::retired_shell_encoding(&entry, &work).unwrap();
    let retired_flags = u32::from_le_bytes(retired[20..24].try_into().unwrap());
    assert_eq!(retired_flags & 0x20, 0x20);
    assert_ne!(encoded, retired);
    fs::write(&link, encoded).unwrap();
    // Windows PowerShell supplies an independent ShellExecute test oracle. No
    // script executes in the installed entry point or in its native installer.
    let powershell = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let mut child=command(&powershell,&profile).env("SUSI_FIXTURE_SHORTCUT",&link)
        .args(["-NoProfile","-NonInteractive","-Command","$ErrorActionPreference='Stop'; Start-Process -FilePath $env:SUSI_FIXTURE_SHORTCUT -WindowStyle Hidden -Wait"]).spawn().unwrap();
    let started = Instant::now();
    while child.try_wait().unwrap().is_none() && started.elapsed() < Duration::from_secs(30) {
        std::thread::sleep(Duration::from_millis(30));
    }
    if child.try_wait().unwrap().is_none() {
        child.kill().unwrap();
    }
    assert!(child.wait().unwrap().success());
    let opened = fs::read_to_string(slot.join("susi.opened")).unwrap();
    let mut observed = opened.lines();
    assert_eq!(observed.next(), Some("A"));
    assert_eq!(observed.next(), Some("0"), "the link passed arguments");
    assert_eq!(
        Path::new(observed.next().unwrap()).canonicalize().unwrap(),
        work.canonicalize().unwrap()
    );
    fs::write(slot.join("susi.exe"), b"tamper").unwrap();
    let failed = command(&entry, &profile).arg("--version").output().unwrap();
    assert!(!failed.status.success());
    assert!(
        !profile.join("%SystemDrive%").exists(),
        "Shell cache escaped its valid OS directories"
    );
    assert!(String::from_utf8(failed.stderr)
        .unwrap()
        .contains("admitted manifest"));
    assert_eq!(
        profile.canonicalize().unwrap().parent(),
        Some(std::env::temp_dir().canonicalize().unwrap().as_path())
    );
    fs::remove_dir_all(profile).unwrap();
}
