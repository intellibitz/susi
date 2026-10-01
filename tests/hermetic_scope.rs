#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! `scripts/check-hermetic-tests.sh`: scoping, and whether the rule has teeth.
//!
//! Mandate 52 says no test touches `~/.susi`, an inherited `SUSI_HOME`, or the
//! developer's own config. The check that enforces it ran the whole workspace
//! suite, which is why it was wired into nothing; it now takes cargo arguments
//! so it can run for the crates a diff touched. A fake `cargo` lets both the
//! scope and the leak assertion be checked without compiling anything.

use std::path::{Path, PathBuf};
use std::process::Command;

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/check-hermetic-tests.sh")
}

struct World {
    root: PathBuf,
    bins: PathBuf,
    home: PathBuf,
}

impl World {
    /// `cargo` that records its argv, and optionally writes into `SUSI_HOME` the
    /// way a test that ignored the throwaway instance would.
    fn new(tag: &str, leaks: bool) -> Self {
        let root = std::env::temp_dir().join(format!(
            "susi-hermetic-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&root);
        // The script prepends `$CARGO_HOME/bin` to PATH, so the fake has to be
        // *there* — a fake merely on PATH is ignored and the real cargo runs the
        // whole workspace suite (which is how this test first ran, three times
        // over, in parallel).
        let bins = root.join("cargo-home").join("bin");
        std::fs::create_dir_all(&bins).unwrap();
        let leak = if leaks {
            "printf 'x' >\"$SUSI_HOME/leaked\"\n"
        } else {
            ""
        };
        let body =
            format!("#!/usr/bin/env bash\necho \"$*\" >>\"$TEST_CARGO_CALLS\"\n{leak}exit 0\n");
        let path = bins.join("cargo");
        std::fs::write(&path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self {
            home: root.join("throwaway.tsv"),
            root,
            bins,
        }
    }

    fn run(&self, args: &[&str]) -> (i32, String) {
        let out = Command::new(script())
            .args(args)
            .env("CARGO_HOME", self.bins.parent().unwrap())
            .env("PATH", format!("{}:/usr/bin:/bin", self.bins.display()))
            .env("TEST_CARGO_CALLS", &self.home)
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }

    fn cargo_calls(&self) -> String {
        std::fs::read_to_string(&self.home).unwrap_or_default()
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn arguments_scope_the_check_to_those_crates() {
    let w = World::new("scoped", false);
    let (code, out) = w.run(&["-p", "susi-gawd", "-p", "susi-config"]);
    assert_eq!(code, 0, "{out}");
    let calls = w.cargo_calls();
    assert!(
        calls.contains("-p susi-gawd -p susi-config"),
        "the packages must reach cargo: {calls}"
    );
    assert!(
        !calls.contains("--workspace"),
        "a scoped check must not ask for the workspace: {calls}"
    );
}

#[test]
fn no_arguments_keeps_the_workspace_sweep() {
    let w = World::new("workspace", false);
    let (code, out) = w.run(&[]);
    assert_eq!(code, 0, "{out}");
    assert!(
        w.cargo_calls().contains("--workspace"),
        "{}",
        w.cargo_calls()
    );
}

/// The rule is only worth wiring if it fails when it is broken: a "test" that
/// writes into the instance it was handed must fail the check.
#[test]
fn a_test_that_writes_into_the_instance_fails_the_check() {
    let w = World::new("leak", true);
    let (code, out) = w.run(&["-p", "susi-gawd"]);
    assert_eq!(code, 1, "a leak must fail: {out}");
    assert!(
        out.contains("tests wrote into the inherited SUSI_HOME"),
        "{out}"
    );
    assert!(out.contains("leaked"), "it names what leaked: {out}");
}
