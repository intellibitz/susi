use super::super::package::{digest_bytes, SIDECARS};
use super::*;
use std::io::Write;
use std::os::windows::fs::OpenOptionsExt;
use std::process::Stdio;

fn fixture_root() -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = env::temp_dir().join(format!(
        "susi-ordinary-fixture-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir(&dir).unwrap();
    dir
}

fn package(base: &Path, tag: &str) -> Package {
    let dir = base.join(tag);
    fs::create_dir(&dir).unwrap();
    let fixture = dir.join("fixture.rs");
    fs::write(
        &fixture,
        format!(
            r#"use std::io::BufRead;
fn main() {{
    let mut args=std::env::args().skip(1);
    if args.next().as_deref()==Some("hold") {{let _=std::io::stdin().lock().lines().next();}}
    println!("susi {tag}");
}}"#
        ),
    )
    .unwrap();
    let exe = dir.join("susi.exe");
    assert!(Command::new("rustc")
        .args(["--edition", "2021"])
        .arg(fixture)
        .arg("-o")
        .arg(&exe)
        .status()
        .unwrap()
        .success());
    let data = fs::read(&exe).unwrap();
    let side = b"fixture notice or evidence";
    let hash = digest_bytes(side);
    let gates = [super::super::package::QUALIFICATION_POLICY, side.as_slice()].concat();
    let gate_hash = digest_bytes(&gates);
    fs::write(dir.join("gates.txt"), gates).unwrap();
    for (name, _) in SIDECARS {
        fs::write(dir.join(name), side).unwrap();
    }
    let oid = "a".repeat(40);
    let manifest=format!("susi-local-release-v1\nsource={oid}\ntree={oid}\nartifact-sha256={}\nartifact-bytes={}\nhost={}-pc-windows-msvc\nintegration-receipt={oid}\nlock-sha256={hash}\ncompiler-sha256={hash}\ngates-sha256={gate_hash}\nsbom-sha256={hash}\nnotices-sha256={hash}\nnative-audit-sha256={hash}\n",digest_bytes(&data),data.len(),env::consts::ARCH);
    fs::write(dir.join("manifest.txt"), &manifest).unwrap();
    Package::open(&dir, &digest_bytes(manifest.as_bytes())).unwrap()
}

fn ok(_: &Path) -> Result<()> {
    Ok(())
}
fn refused(_: &Path) -> Result<()> {
    Err("injected registration failure".into())
}
fn selected(root: &Path) -> Selection {
    get(&root.join(ACTIVE)).unwrap().unwrap()
}
fn launch(root: &Path) -> String {
    let chosen = selected(root);
    let release = installed(root, &chosen.current).unwrap();
    let _pinned = release.pinned_executable().unwrap();
    String::from_utf8(
        Command::new(release.executable())
            .arg("--version")
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .into()
}

#[test]
#[ignore = "subprocess stopped by the committed-selector interruption regression"]
fn interruptible_installer_child() {
    let scratch = PathBuf::from(std::env::var_os("SUSI_CHANNEL_TEST_FIXTURE").unwrap());
    let dir = scratch.join("B");
    let manifest = fs::read(dir.join("manifest.txt")).unwrap();
    let b = Package::open(&dir, &digest_bytes(&manifest)).unwrap();
    let root = scratch.join("SusiLocal/production");
    apply(
        &root,
        Some((&b, None)),
        &|_| {
            fs::write(
                scratch.join("entered-registration"),
                b"pending transition is durable",
            )?;
            std::thread::sleep(std::time::Duration::from_secs(45));
            Err("parent did not interrupt the fixture".into())
        },
        None,
    )
    .unwrap();
}

fn interrupt_after_staging(scratch: &Path) {
    let module = module_path!().split_once("::").unwrap().1;
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            &format!("{module}::interruptible_installer_child"),
        ])
        .env("SUSI_CHANNEL_TEST_FIXTURE", scratch)
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let started = std::time::Instant::now();
    while !scratch.join("entered-registration").exists()
        && started.elapsed() < std::time::Duration::from_secs(30)
        && child.try_wait().unwrap().is_none()
    {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let reached = scratch.join("entered-registration").exists();
    if child.try_wait().unwrap().is_none() {
        child.kill().unwrap();
    }
    child.wait().unwrap();
    assert!(
        reached,
        "installer did not reach the pre-commit interruption point"
    );
}

#[test]
fn qualified_ordinary_channel_preserves_live_images_recovers_and_rejects_tampering() {
    let scratch = fixture_root();
    let a = package(&scratch, "A");
    let b = package(&scratch, "B");
    let root = scratch.join("SusiLocal/production");
    let sa = apply(&root, Some((&a, None)), &ok, None).unwrap();
    assert_eq!(sa.current, a.release);
    assert_eq!(launch(&root), "susi A");
    let mut held = Command::new(installed(&root, &a.release).unwrap().executable())
        .arg("hold")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    assert!(apply(&root, Some((&b, Some(&a))), &refused, None).is_err());
    assert_eq!(selected(&root), sa);
    assert!(root.join(PENDING).exists());
    assert_eq!(apply(&root, None, &ok, None).unwrap().current, a.release);
    assert!(!root.join(PENDING).exists());
    interrupt_after_staging(&scratch);
    assert_eq!(selected(&root), sa);
    assert_eq!(
        apply(&root, None, &ok, Some(&a.release)).unwrap().current,
        a.release
    );
    let locked = OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(root.join(ACTIVE))
        .unwrap();
    assert!(apply(&root, Some((&b, Some(&a))), &ok, None).is_err());
    assert_eq!(selected(&root).current, a.release);
    drop(locked);
    let sb = apply(&root, Some((&b, Some(&a))), &ok, None).unwrap();
    assert_eq!(sb.previous.as_deref(), Some(a.release.as_str()));
    assert_eq!(launch(&root), "susi B");
    assert!(held.try_wait().unwrap().is_none());
    assert!(apply(&root, None, &ok, Some(&"0".repeat(64))).is_err());
    assert_eq!(selected(&root), sb);
    assert!(apply(&root, None, &refused, None).is_err());
    assert_eq!(apply(&root, None, &ok, None).unwrap().current, a.release);
    assert_eq!(launch(&root), "susi A");
    assert!(!root
        .join("releases")
        .join(&b.release)
        .join("gates.txt")
        .exists());
    held.stdin.take().unwrap().write_all(b"done\n").unwrap();
    assert!(held.wait().unwrap().success());
    let a_file = installed(&root, &a.release).unwrap().executable();
    fs::write(&a_file, b"changed").unwrap();
    assert!(dispatch(&root.join("entry/susi.exe")).is_err());
    let restored = apply(&root, None, &ok, None).unwrap();
    assert_eq!(restored.current, b.release);
    assert_eq!(restored.previous, None);
    let receipt = root.join(ACTIVE);
    let original = fs::read_to_string(&receipt).unwrap();
    fs::write(&receipt, original.replacen(&b.release, &a.release, 1)).unwrap();
    assert!(dispatch(&root.join("entry/susi.exe")).is_err());
    fs::write(receipt, original).unwrap();
    fs::write(b.root.join("gates.txt"), b"altered producer evidence").unwrap();
    assert!(b.stage(&root).is_err());
    assert_eq!(
        scratch.canonicalize().unwrap().parent(),
        Some(env::temp_dir().canonicalize().unwrap().as_path())
    );
    fs::remove_dir_all(scratch).unwrap();
}
