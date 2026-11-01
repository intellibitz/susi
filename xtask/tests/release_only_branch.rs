#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Exercise the release classifier against actual Git trees.
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

const NAMES: [&str; 7] = [
    "susi",
    "susi-daemon",
    "susi-gawd",
    "susi-gemi",
    "susi-gmcp",
    "susi-harness",
    "susi-server",
];
const META: [&str; 3] = [
    ".agents/evidence.json",
    ".agents/identity.json",
    ".agents/roadmap.json",
];
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    root: PathBuf,
    base: String,
}
impl Fixture {
    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.root)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }
    fn put(&self, path: &str, data: &str) {
        let path = self.root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, data).unwrap();
    }
    fn paths() -> Vec<String> {
        let mut paths = vec!["Cargo.toml".into()];
        paths.extend(
            NAMES
                .iter()
                .skip(1)
                .map(|name| format!("crates/{name}/Cargo.toml")),
        );
        paths.extend(META.map(str::to_owned));
        paths.extend(["README.md".into(), "Cargo.lock".into()]);
        paths
    }
    fn commit(&self) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-qm", "change"]);
    }
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "susi-release-classifier-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut f = Self {
            root,
            base: String::new(),
        };
        f.git(&["init", "-q", "-b", "main"]);
        f.git(&["config", "user.name", "fixture"]);
        f.git(&["config", "user.email", "fixture@example.test"]);
        for (path, name) in Self::paths().iter().zip(NAMES) {
            f.put(
                path,
                &format!("[package]\nname = \"{name}\"\nversion = \"1.2.3\"\n"),
            );
        }
        for path in META {
            f.put(path, "{\"version\":\"1.2.3\",\"meaning\":\"preserve\"}\n");
        }
        f.put("README.md", "release 1.2.3\n");
        let mut lock = "version = 4\n".to_string();
        for name in NAMES {
            lock.push_str(&format!(
                "\n[[package]]\nname = \"{name}\"\nversion = \"1.2.3\"\n"
            ));
        }
        f.put("Cargo.lock", &lock);
        f.commit();
        f.base = f.git(&["rev-parse", "HEAD"]);
        for path in Self::paths() {
            let p = f.root.join(path);
            let s = std::fs::read_to_string(&p).unwrap();
            std::fs::write(p, s.replace("1.2.3", "1.2.4")).unwrap();
        }
        f.commit();
        f
    }
    fn accepted(&self) -> bool {
        Command::new(env!("CARGO_BIN_EXE_ci-release-only"))
            .current_dir(&self.root)
            .env("BASE_REF", &self.base)
            .status()
            .unwrap()
            .success()
    }
    fn append(&self, path: &str, extra: &str) {
        let p = self.root.join(path);
        let mut s = std::fs::read_to_string(&p).unwrap();
        s.push_str(extra);
        std::fs::write(p, s).unwrap();
        self.commit();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn exact_version_bump_is_accepted() {
    assert!(Fixture::new().accepted());
}
#[test]
fn source_and_dependency_changes_are_rejected() {
    let f = Fixture::new();
    f.put("src/lib.rs", "pub fn changed() {}\n");
    f.commit();
    assert!(!f.accepted());
    let f = Fixture::new();
    f.append(
        "Cargo.lock",
        "\n[[package]]\nname = \"unexpected\"\nversion = \"9.0.0\"\n",
    );
    assert!(!f.accepted());
}
#[test]
fn metadata_and_manifest_changes_are_rejected() {
    for path in META
        .into_iter()
        .chain(["README.md", "crates/susi-gawd/Cargo.toml"])
    {
        let f = Fixture::new();
        f.append(path, "extra\n");
        assert!(!f.accepted(), "{path}");
    }
}
#[test]
fn missing_version_change_and_wrong_lock_package_are_rejected() {
    let f = Fixture::new();
    f.git(&["checkout", &f.base, "--", "crates/susi-gawd/Cargo.toml"]);
    f.commit();
    assert!(!f.accepted());
    let f = Fixture::new();
    let p = f.root.join("Cargo.lock");
    let s = std::fs::read_to_string(&p).unwrap();
    std::fs::write(p, s.replace("name = \"susi-gawd\"", "name = \"other\"")).unwrap();
    f.commit();
    assert!(!f.accepted());
}
#[test]
fn invalid_base_and_downgrade_are_rejected() {
    let f = Fixture::new();
    assert!(!Command::new(env!("CARGO_BIN_EXE_ci-release-only"))
        .current_dir(&f.root)
        .env("BASE_REF", "refs/heads/missing")
        .status()
        .unwrap()
        .success());
    for path in Fixture::paths() {
        let p = f.root.join(path);
        let s = std::fs::read_to_string(&p).unwrap();
        std::fs::write(p, s.replace("1.2.4", "1.2.2")).unwrap();
    }
    f.commit();
    assert!(!f.accepted());
}
#[test]
fn changed_file_mode_is_rejected() {
    let f = Fixture::new();
    f.git(&["update-index", "--chmod=+x", "README.md"]);
    f.git(&["commit", "-qm", "change mode"]);
    assert!(!f.accepted());
}

#[test]
fn classifier_runs_before_sccache_with_wrappers_disabled() {
    let workflow = include_str!("../../.github/workflows/test.yml");
    assert!(workflow.contains("RUSTC_WRAPPER= RUSTC_WORKSPACE_WRAPPER= cargo run --locked --quiet -p xtask --bin ci-release-only"));
}
