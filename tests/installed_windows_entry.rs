#![cfg(windows)]
//! Exercise the actual Susi entry executable and Windows Shell with private fixture
//! releases. Fixture receipts exercise dispatch only; production qualification is separate.
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
    let source = root.join(format!("{name}.rs"));
    fs::write(&source,format!(r#"use std::io::BufRead;
fn main(){{
 let args:Vec<_>=std::env::args().skip(1).collect();
 let exe=std::env::current_exe().unwrap();
 if args.as_slice()==["shell"]{{
   std::fs::write(exe.with_extension("opened"),format!("{{}}\n{{}}\n{{}}", "{name}",args[0],std::env::current_dir().unwrap().display())).unwrap();
   return;
 }}
 let data=std::io::stdin().lock().lines().next().unwrap().unwrap();
 println!("{{}}|{{}}|{{}}|{{}}","{name}",args[0],data,std::env::var("SUSI_HOME").unwrap());
 eprintln!("stderr-{name}");
 std::process::exit(37);
}}"#)).unwrap();
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
    command.env_clear().creation_flags(0x08000000);
    let system = std::env::var_os("SystemRoot").unwrap();
    command
        .env("SystemRoot", &system)
        .env("WINDIR", &system)
        .env("PATH", Path::new(&system).join("System32"))
        .env("USERPROFILE", profile)
        .env("HOME", profile)
        .env("LOCALAPPDATA", profile.join("AppData/Local"))
        .env("APPDATA", profile.join("AppData/Roaming"))
        .env("TEMP", profile)
        .env("TMP", profile);
    command
}

#[test]
fn actual_windows_susi_dispatches_pipes_and_windows_opens_its_unicode_shortcut() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let profile = std::env::temp_dir().join(format!("susi-普通-{stamp}"));
    let root = profile.join("AppData/Local/SusiLocal/production");
    let entry = root.join("entry/susi.exe");
    fs::create_dir_all(entry.parent().unwrap()).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_susi"), &entry).unwrap();
    let (a, slot) = fixture(&root, "A");
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
    fs::write(&link, shell_link::encode(&entry, &work).unwrap()).unwrap();
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
    assert_eq!(observed.next(), Some("shell"));
    assert_eq!(
        Path::new(observed.next().unwrap()).canonicalize().unwrap(),
        work.canonicalize().unwrap()
    );
    fs::write(slot.join("susi.exe"), b"tamper").unwrap();
    let failed = command(&entry, &profile).arg("--version").output().unwrap();
    assert!(!failed.status.success());
    assert!(String::from_utf8(failed.stderr)
        .unwrap()
        .contains("admitted manifest"));
    assert_eq!(
        profile.canonicalize().unwrap().parent(),
        Some(std::env::temp_dir().canonicalize().unwrap().as_path())
    );
    fs::remove_dir_all(profile).unwrap();
}
