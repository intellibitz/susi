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

#[test]
fn installation_probe_distinguishes_public_storage_from_a_private_package_cache() {
    let root = fixture_root();
    let private = root.join("Packages/Example/LocalCache/Local");
    fs::create_dir_all(&private).unwrap();
    assert!(!public_host_view(&private).unwrap());
    assert_eq!(fs::read_dir(&private).unwrap().count(), 0);
    assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
    fs::remove_dir_all(root).unwrap();
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
    let arg=args.next();
    if arg.as_deref()==Some("--version") && !std::env::var_os("USERPROFILE").is_some_and(|p|std::path::Path::new(&p).is_absolute()) {{std::process::exit(5);}}
    if arg.as_deref()==Some("hold") {{let _=std::io::stdin().lock().lines().next();}}
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
        Command::new(release.executable().canonicalize().unwrap())
            .arg("--version")
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .into()
}

// Each host process uses the same private test channel as its parent.
// Registration of real PATH entries and shortcuts belongs to the clean guest.
#[test]
#[ignore = "worker launched explicitly by the interactive Windows handoff test"]
fn scheduled_transaction_worker() {
    let result = (|| -> Result<String> {
        let exe = env::current_exe()?;
        let scratch = exe.parent().ok_or("missing fixture directory")?;
        if !scratch
            .file_name()
            .and_then(|p| p.to_str())
            .is_some_and(|p| p.starts_with("susi-ordinary-fixture-"))
        {
            return Err("host worker is outside its fixture".into());
        }
        let local = PathBuf::from(env::var_os("LOCALAPPDATA").ok_or("missing LocalAppData")?);
        if !public_host_view(&local)? {
            return Err("scheduled worker still has a virtualized Windows view".into());
        }
        let open = |tag: &str| -> Result<Package> {
            let path = scratch.join(tag);
            Package::open(&path, &digest_bytes(&fs::read(path.join("manifest.txt"))?))
        };
        let root = scratch.join("SusiLocal/production");
        let a = open("A")?;
        let selected = match fs::read_to_string(scratch.join("fixture-operation"))?.as_str() {
            "A" => apply(&root, Some((&a, None)), &ok, None)?,
            "B" => apply(&root, Some((&open("B")?, Some(&a))), &ok, None)?,
            "rollback" => apply(&root, None, &ok, Some(&a.release))?,
            _ => return Err("unexpected fixture operation".into()),
        };
        Ok(selected.current)
    })();
    super::super::host_task::reply(&result).unwrap();
    result.unwrap();
}

#[test]
#[ignore = "requires a packaged Windows parent and an interactive Task Scheduler"]
fn packaged_host_handoff_installs_two_fixtures_and_restores_the_first() {
    let local = PathBuf::from(env::var_os("LOCALAPPDATA").unwrap());
    assert!(
        !public_host_view(&local).unwrap(),
        "the parent must have a private app view"
    );
    let scratch = fixture_root();
    let a = package(&scratch, "A");
    let b = package(&scratch, "B");
    let worker = scratch.join("host-fixture.exe");
    fs::copy(env::current_exe().unwrap(), &worker).unwrap();
    let module = module_path!().split_once("::").unwrap().1;
    let args = format!("--ignored --exact {module}::scheduled_transaction_worker");
    let root = scratch.join("SusiLocal/production");
    for (operation, expected, version) in [
        ("A", &a.release, "susi A"),
        ("B", &b.release, "susi B"),
        ("rollback", &a.release, "susi A"),
    ] {
        fs::write(scratch.join("fixture-operation"), operation).unwrap();
        let selected = super::super::host_task::schedule(&worker, &args).unwrap();
        assert_eq!(&selected, expected);
        assert_eq!(launch(&root), version);
    }
    fs::remove_dir_all(scratch).unwrap();
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
    let mut held = Command::new(
        installed(&root, &a.release)
            .unwrap()
            .executable()
            .canonicalize()
            .unwrap(),
    )
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

#[test]
fn selection_validate_and_require_enforce_digest_and_target_invariants() {
    let valid = Selection {
        current: "a".repeat(64),
        previous: Some("b".repeat(64)),
        dispatcher: "a".repeat(64),
    };
    assert!(valid.validate().is_ok());
    assert!(valid.require(None).is_ok());
    assert!(valid.require(Some(&"a".repeat(64))).is_ok());
    assert!(valid.require(Some(&"c".repeat(64))).is_err());

    let same_prev = Selection {
        previous: Some("a".repeat(64)),
        ..valid.clone()
    };
    assert!(same_prev.validate().is_err());

    let bad_disp = Selection {
        dispatcher: "invalid".into(),
        ..valid
    };
    assert!(bad_disp.validate().is_err());
}

#[test]
fn receipt_put_and_get_roundtrip_and_detect_tampered_payload() {
    let scratch = fixture_root();
    let path = scratch.join(ACTIVE);
    assert!(get::<Selection>(&path).unwrap().is_none());

    let sel = Selection {
        current: "1".repeat(64),
        previous: None,
        dispatcher: "1".repeat(64),
    };
    put(&path, &sel).unwrap();
    assert_eq!(get::<Selection>(&path).unwrap(), Some(sel));

    let raw = fs::read_to_string(&path).unwrap();
    fs::write(&path, raw.replacen(&"1".repeat(64), &"2".repeat(64), 1)).unwrap();
    assert!(get::<Selection>(&path).is_err());

    fs::remove_dir_all(scratch).unwrap();
}
