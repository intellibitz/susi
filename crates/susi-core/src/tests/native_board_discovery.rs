#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "isolated Git contract fixtures"
)]

use super::*;

struct Fixture {
    env: susi_paths::test_env::EnvGuard,
    root: susi_paths::tempfile::TempDir,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Fixture {
    fn new() -> Self {
        let lock = crate::commit_log::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let root = susi_paths::tempfile::tempdir().unwrap();
        let mut env = susi_paths::test_env::EnvGuard::isolated();
        for key in [
            BOARD_URL_ENV,
            REMOTE_URL_ENV,
            REMOTE_ENV,
            "GIT_TRACE2_EVENT",
        ] {
            env.remove(key);
        }
        let global = root.path().join("empty-global-config");
        fs::write(&global, "").unwrap();
        env.set("GIT_CONFIG_GLOBAL", &global)
            .set("GIT_CONFIG_NOSYSTEM", "1");
        git(root.path(), &["init", "--quiet"]).unwrap();
        Self {
            env,
            root,
            _lock: lock,
        }
    }

    fn run(&self, args: &[&str]) {
        git(self.root.path(), args).unwrap();
    }

    fn trace(&mut self) {
        self.env
            .set("GIT_TRACE2_EVENT", self.root.path().join("trace.jsonl"));
    }

    fn commands(&self) -> Vec<serde_json::Value> {
        fs::read_to_string(self.root.path().join("trace.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .filter(|event| event["event"] == "start")
            .collect()
    }

    fn configuration_reads(&self) -> usize {
        self.commands()
            .iter()
            .filter(|event| {
                event["argv"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|word| word == "--get-regexp")
            })
            .count()
    }
}

#[test]
fn board_discovery_batches_configuration_and_refreshes_next_resolution() {
    let mut fx = Fixture::new();
    fx.run(&["config", "--add", BOARD_URL_CONFIG, "http://first.invalid/"]);
    fx.run(&[
        "config",
        "--add",
        BOARD_URL_CONFIG,
        " http://second.invalid/// ",
    ]);
    fx.env.set(BOARD_URL_ENV, " ");
    fx.trace();
    let BoardTarget::Http { base_url, .. } = resolve_board_target(Some(fx.root.path())).unwrap()
    else {
        panic!("configured HTTP target");
    };
    assert_eq!(base_url, "http://second.invalid");
    assert_eq!(fx.configuration_reads(), 1);

    fx.run(&[
        "config",
        "--replace-all",
        BOARD_URL_CONFIG,
        "http://changed.invalid/",
    ]);
    let BoardTarget::Http { base_url, .. } = resolve_board_target(Some(fx.root.path())).unwrap()
    else {
        panic!("fresh configured HTTP target");
    };
    assert_eq!(base_url, "http://changed.invalid");
    assert_eq!(fx.configuration_reads(), 2);
}

#[test]
fn board_discovery_environment_precedence_needs_no_git_configuration() {
    let mut fx = Fixture::new();
    fx.run(&["config", BOARD_URL_CONFIG, "http://configured.invalid/"]);
    fx.env.set(BOARD_URL_ENV, "http://environment.invalid/");
    fx.trace();
    let BoardTarget::Http { base_url, .. } = resolve_board_target(Some(fx.root.path())).unwrap()
    else {
        panic!("environment HTTP target");
    };
    assert_eq!(base_url, "http://environment.invalid");
    assert!(fx.commands().is_empty());
}

#[test]
fn board_discovery_keeps_live_public_remote_refusal() {
    let fx = Fixture::new();
    fx.run(&["config", REMOTE_CONFIG, "board"]);
    fx.run(&[
        "remote",
        "add",
        "board",
        "https://github.com/intellibitz/susi-commercial-apps.git",
    ]);
    assert!(
        matches!(resolve_board_target(Some(fx.root.path())).unwrap(), BoardTarget::Git { remote, .. } if remote == "board")
    );
    fx.run(&[
        "remote",
        "set-url",
        "board",
        "https://github.com/intellibitz/susi.git",
    ]);
    let error = resolve_board_target(Some(fx.root.path())).unwrap_err();
    assert!(
        error.contains("forbidden on public intellibitz/susi"),
        "{error}"
    );
}

#[cfg(unix)]
#[test]
fn board_discovery_absolute_paths_do_not_probe_named_remotes() {
    let mut fx = Fixture::new();
    let remote = fx.root.path().join("private board.git");
    // Even a hand-written config section cannot make an absolute path a
    // remote nickname. If Git ever changes that rule, this shortcut must go.
    fx.run(&[
        "config",
        &format!("remote.{}.url", remote.display()),
        "https://github.com/intellibitz/susi.git",
    ]);
    assert!(git(
        fx.root.path(),
        &["remote", "get-url", remote.to_str().unwrap()]
    )
    .is_err());
    fx.trace();
    verify_non_public_remote(fx.root.path(), remote.to_str().unwrap()).unwrap();
    assert!(fx.commands().is_empty());
    assert!(
        verify_non_public_remote(fx.root.path(), "https://github.com/intellibitz/susi.git")
            .is_err()
    );
}

#[cfg(windows)]
#[test]
fn board_discovery_absolute_backslash_remote_alias_keeps_public_refusal() {
    let fx = Fixture::new();
    let remote = fx.root.path().join("private board.git");
    fx.run(&[
        "config",
        &format!("remote.{}.url", remote.display()),
        "https://github.com/intellibitz/susi.git",
    ]);
    assert!(git(
        fx.root.path(),
        &["remote", "get-url", remote.to_str().unwrap()]
    )
    .is_ok());
    let error = verify_non_public_remote(fx.root.path(), remote.to_str().unwrap()).unwrap_err();
    assert!(
        error.contains("forbidden on public intellibitz/susi"),
        "{error}"
    );
}
