#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

// Exercise the actual Windows process filter with disposable binaries. A
// checkout-built daemon serving a real workspace must survive the test sweep.
#[cfg(windows)]
#[test]
fn cleanup_stops_only_the_current_hermetic_daemon() {
    use std::{
        fs,
        path::PathBuf,
        process::{Child, Command},
        thread,
        time::Duration,
    };
    struct Owned(Child);
    impl Drop for Owned {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let fixture = root
        .join("target")
        .join(format!("hermetic-cleanup-fixture-{}", std::process::id()));
    let bin = fixture.join("susi.exe");
    let src = fixture.join("stub.rs");
    let scratch = fixture.join("scratch");
    let test_home = fixture.join("test-home");
    let user_workspace = fixture.join("user-owned-workspace");
    for dir in [&scratch, &test_home, &user_workspace] {
        fs::create_dir_all(dir).unwrap();
    }
    fs::write(
        &src,
        "fn main() { std::thread::sleep(std::time::Duration::from_secs(90)); }",
    )
    .unwrap();
    assert!(Command::new("rustc")
        .args(["--crate-name", "susi_cleanup_fixture"])
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .status()
        .unwrap()
        .success());
    let spawn = |workspace: &std::path::Path| {
        Owned(
            Command::new(&bin)
                .args(["daemon-start", "--workspace"])
                .arg(workspace)
                .spawn()
                .unwrap(),
        )
    };
    let mut owned = spawn(&test_home.join(".susi"));
    let mut user = spawn(&user_workspace);
    thread::sleep(Duration::from_millis(300));
    let bash = PathBuf::from(std::env::var_os("ProgramFiles").unwrap()).join("Git/bin/bash.exe");
    let without_marker = Command::new(&bash)
        .arg(root.join("scripts/kill-leftover-service-runs.sh"))
        .current_dir(&root)
        .env("SUSI_HOME", &scratch)
        .env_remove("SUSI_HERMETIC_FORBIDDEN")
        .env("USERPROFILE", &test_home)
        .output()
        .unwrap();
    assert!(without_marker.status.success());
    assert!(
        owned.0.try_wait().unwrap().is_none(),
        "an unmarked sweep killed a daemon"
    );
    let output = Command::new(bash)
        .arg(root.join("scripts/kill-leftover-service-runs.sh"))
        .current_dir(&root)
        .env("SUSI_HOME", &scratch)
        .env("SUSI_HERMETIC_FORBIDDEN", &scratch)
        .env("USERPROFILE", &test_home)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for _ in 0..30 {
        if owned.0.try_wait().unwrap().is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    assert!(
        owned.0.try_wait().unwrap().is_some(),
        "the hermetic daemon survived: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        user.0.try_wait().unwrap().is_none(),
        "the user-owned daemon was killed"
    );
}

// The Linux process filter, with disposable binaries built into this
// checkout's target. A temp `SUSI_HOME` alone is not ownership: every daemon
// test gives its daemon one, so the sweep must also see its own invocation's
// marker. Without that, one test binary's cleanup killed the daemons of the
// test binaries running beside it in the release gate's parallel lanes.
#[cfg(target_os = "linux")]
#[test]
fn cleanup_stops_only_this_hermetic_runs_daemons() {
    use std::{
        fs,
        path::{Path, PathBuf},
        process::{Child, Command},
        thread,
        time::Duration,
    };
    struct Owned(Child);
    impl Drop for Owned {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let fixture = Fixture(
        root.join("target")
            .join(format!("hermetic-cleanup-fixture-{}", std::process::id())),
    );
    fs::create_dir_all(&fixture.0).unwrap();
    let bin = fixture.0.join("susi");
    let src = fixture.0.join("stub.rs");
    fs::write(
        &src,
        "fn main() { std::thread::sleep(std::time::Duration::from_secs(90)); }",
    )
    .unwrap();
    assert!(Command::new("rustc")
        .args(["--crate-name", "susi_cleanup_fixture"])
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .status()
        .unwrap()
        .success());
    let temp = std::env::temp_dir();
    let home = |tag: &str| temp.join(format!("susi-cleanup-{tag}-{}", std::process::id()));
    let mine = home("mine");
    let theirs = home("theirs");
    let spawn = |susi_home: &Path, marker: Option<&Path>| {
        let mut command = Command::new(&bin);
        command
            .args(["daemon-start", "--workspace"])
            .arg(susi_home)
            .env("SUSI_HOME", susi_home)
            .env_remove("SUSI_HERMETIC_FORBIDDEN");
        if let Some(marker) = marker {
            command.env("SUSI_HERMETIC_FORBIDDEN", marker);
        }
        Owned(command.spawn().unwrap())
    };
    // A daemon of this run that a test gave a private temp home, a daemon of
    // another hermetic run, and a daemon of a test that runs with no wrapper.
    let mut owned = spawn(&home("private"), Some(&mine));
    let mut other_run = spawn(&theirs, Some(&theirs));
    let mut unwrapped = spawn(&home("unwrapped"), None);
    thread::sleep(Duration::from_millis(300));
    let sweep = |marker: Option<&Path>| {
        let mut command = Command::new("bash");
        command
            .arg(root.join("scripts/kill-leftover-service-runs.sh"))
            .current_dir(&root)
            .env_remove("SUSI_HERMETIC_FORBIDDEN");
        if let Some(marker) = marker {
            command.env("SUSI_HERMETIC_FORBIDDEN", marker);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stderr).into_owned()
    };
    sweep(None);
    thread::sleep(Duration::from_millis(300));
    assert!(
        owned.0.try_wait().unwrap().is_none(),
        "an unmarked sweep killed a daemon"
    );
    let log = sweep(Some(&mine));
    for _ in 0..30 {
        if owned.0.try_wait().unwrap().is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    assert!(
        owned.0.try_wait().unwrap().is_some(),
        "this run's daemon survived: {log}"
    );
    assert!(
        other_run.0.try_wait().unwrap().is_none(),
        "another hermetic run's daemon was killed"
    );
    assert!(
        unwrapped.0.try_wait().unwrap().is_none(),
        "an unwrapped test's daemon was killed"
    );
}
