#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)]

//! Each composing command primes only the catalogs its output reads, and a
//! command that reads one still primes it. Every prime step is a boot-trace
//! phase (`prime: <catalog>`), so `SUSI_BOOT_TRACE=1` shows exactly which
//! catalogs a run primed; the files each step writes confirm it.
//!
//! These launch the real binary, so the cases that prime the agent scans or
//! leading MCP (`auto prime`, `substrate status`, `crown status`, `mcp list`,
//! an in-process `mcp call`) run the host's real Python import and launcher
//! probes. The suite takes about 45-50 s on a Windows dev host; the time
//! varies with the tooling installed (python, npx, docker), the results do
//! not: assertions check only `prime: …` phases, files and pins. Future
//! tests should check those phases, not `last_auto_prime.json`, which only
//! a full prime writes.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const LEADING_MCP: &str = "prime: leading mcp";
const CODING_MODELS: &str = "prime: coding models";
const EXECUTION_AGENTS: &str = "prime: execution agents";
const FRAMEWORKS: &str = "prime: frameworks";
const EVERY_CATALOG: [&str; 4] = [LEADING_MCP, CODING_MODELS, EXECUTION_AGENTS, FRAMEWORKS];

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
    home: PathBuf,
    /// The fresh home a one-off [`run`] owns (removed with the run);
    /// `None` for a run in a shared [`Host`].
    host: Option<Host>,
}

impl Run {
    /// The `prime: …` phases this run traced, in order.
    fn primed(&self) -> Vec<&str> {
        self.stderr
            .lines()
            .filter(|line| line.starts_with("[boot-trace]"))
            .filter_map(|line| line.find("prime: ").map(|at| line[at..].trim_end()))
            .collect()
    }

    fn traced(&self, phase: &str) -> bool {
        self.stderr
            .lines()
            .any(|line| line.starts_with("[boot-trace]") && line.trim_end().ends_with(phase))
    }

    fn wrote(&self, name: &str) -> bool {
        find(&self.home, name).is_some()
    }

    fn context(&self) -> String {
        format!(
            "exit={}
stdout:
{}
stderr:
{}",
            self.code, self.stdout, self.stderr
        )
    }
}

fn find(dir: &Path, name: &str) -> Option<PathBuf> {
    std::fs::read_dir(dir).ok()?.flatten().find_map(|entry| {
        if entry.file_name() == name {
            Some(entry.path())
        } else if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            find(&entry.path(), name)
        } else {
            None
        }
    })
}

/// A fresh temp home (and workspace) that several runs can share; removed
/// on drop.
struct Host {
    root: PathBuf,
    home: PathBuf,
    workspace: PathBuf,
    /// Extra environment every run in this home gets.
    env: Vec<(&'static str, &'static str)>,
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Host {
    fn new(env: &[(&'static str, &'static str)]) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "susi-lazy-prime-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        let workspace = root.join("workspace");
        for dir in [
            home.join("AppData").join("Roaming"),
            home.join("AppData").join("Local"),
            workspace.clone(),
        ] {
            std::fs::create_dir_all(dir).unwrap();
        }
        Self {
            root,
            home,
            workspace,
            env: env.to_vec(),
        }
    }

    /// Run `susi <args>` with boot tracing on, in this home.
    fn run(&self, args: &[&str]) -> Run {
        let mut command = Command::new(env!("CARGO_BIN_EXE_susi"));
        command
            .args(args)
            .current_dir(&self.workspace)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("APPDATA", self.home.join("AppData").join("Roaming"))
            .env("LOCALAPPDATA", self.home.join("AppData").join("Local"))
            .env("SUSI_HOME", self.home.join("instance"))
            .env("XDG_CONFIG_HOME", self.home.join("xdg"))
            .env("XDG_DATA_HOME", self.home.join("xdg-data"))
            .env("XDG_CACHE_HOME", self.home.join("xdg-cache"))
            // Away from any live instance's ports; nothing here may reach
            // a real daemon.
            .env("SUSI_PORT_OFFSET", "731")
            .env("SUSI_BOOT_TRACE", "1")
            .env_remove("SUSI_VERBOSE")
            .stdin(Stdio::null());
        for (key, value) in &self.env {
            command.env(key, value);
        }
        // Output goes to files, not pipes: a command that starts the daemon
        // hands it the inherited handles, and a pipe would then never close.
        static RUNS: AtomicUsize = AtomicUsize::new(0);
        let tag = RUNS.fetch_add(1, Ordering::Relaxed);
        let out = self.root.join(format!("run-{tag}.out"));
        let err = self.root.join(format!("run-{tag}.err"));
        let status = command
            .stdout(Stdio::from(std::fs::File::create(&out).unwrap()))
            .stderr(Stdio::from(std::fs::File::create(&err).unwrap()))
            .status()
            .unwrap();
        let read =
            |path: &Path| String::from_utf8_lossy(&std::fs::read(path).unwrap()).into_owned();
        Run {
            code: status.code().unwrap_or(-1),
            stdout: read(&out),
            stderr: read(&err),
            home: self.home.clone(),
            host: None,
        }
    }

    /// The persisted coding-model override, if any.
    fn selected_override(&self) -> Option<String> {
        find(&self.home, "selected_model_override.txt")
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| text.trim().to_owned())
    }
}

/// Run `susi <args>` with boot tracing on, in a fresh home of its own.
fn run(args: &[&str]) -> Run {
    let host = Host::new(&[]);
    let mut run = host.run(args);
    run.host = Some(host);
    run
}

/// Control planes whose output reads no primed state prime nothing, and
/// leave none of the files a prime writes.
#[test]
fn listing_planes_that_read_no_catalog_prime_nothing() {
    for args in [
        &["agents", "list"][..],
        &["frameworks", "list"],
        &["frontier", "list"],
    ] {
        let run = run(args);
        assert_eq!(run.code, 0, "{args:?}\n{}", run.context());
        assert!(!run.stdout.trim().is_empty(), "{args:?}\n{}", run.context());
        assert!(
            run.primed().is_empty(),
            "{args:?} primed {:?}\n{}",
            run.primed(),
            run.context()
        );
        assert!(!run.traced("plane: prime catalogs"), "{}", run.context());
        for file in ["last_auto_prime.json", "ready.json", "mcp_config.json"] {
            assert!(!run.wrote(file), "{args:?} wrote {file}\n{}", run.context());
        }
    }
}

/// The CLI composition root still composes for `keys` and `review`, but
/// primes nothing: neither reads a primed catalog.
#[test]
fn composing_commands_that_read_no_catalog_prime_nothing() {
    for args in [&["keys", "list"][..], &["review"]] {
        let run = run(args);
        assert_eq!(run.code, 0, "{args:?}\n{}", run.context());
        assert!(
            run.traced("composition: cli"),
            "{args:?}\n{}",
            run.context()
        );
        assert!(
            !run.traced("cli: prime catalogs"),
            "{args:?}\n{}",
            run.context()
        );
        assert!(
            run.primed().is_empty(),
            "{args:?} primed {:?}\n{}",
            run.primed(),
            run.context()
        );
        assert!(!run.wrote("last_auto_prime.json"), "{}", run.context());
        assert!(!run.wrote("ready.json"), "{}", run.context());
    }
}

/// `models list` prints the preferred coding model, so it primes coding-model
/// auto-prefer — and only that.
#[test]
fn models_list_primes_only_coding_models() {
    let run = run(&["models", "list"]);
    assert_eq!(run.code, 0, "{}", run.context());
    assert_eq!(run.primed(), vec![CODING_MODELS], "{}", run.context());
    assert!(run.traced("plane: prime catalogs"), "{}", run.context());
    // A partial prime never writes the full-prime glass-box report.
    assert!(!run.wrote("last_auto_prime.json"), "{}", run.context());
    assert!(!run.wrote("ready.json"), "{}", run.context());
}

/// `mcp list` prints the enabled set that MCP auto-enable writes, so it
/// primes leading MCP — and only that.
#[test]
fn mcp_list_primes_only_leading_mcp() {
    let run = run(&["mcp", "list"]);
    assert_eq!(run.code, 0, "{}", run.context());
    assert_eq!(run.primed(), vec![LEADING_MCP], "{}", run.context());
    assert!(!run.wrote("last_auto_prime.json"), "{}", run.context());
    assert!(!run.wrote("ready.json"), "{}", run.context());
}

/// Regression: `substrate status` counts the agent capabilities the agent
/// scans register, so it still primes both scans (and writes their
/// readiness files) — without the MCP and coding-model steps it never reads.
#[test]
fn substrate_status_still_primes_the_agent_scans() {
    let run = run(&["substrate", "status"]);
    assert_eq!(run.code, 0, "{}", run.context());
    assert_eq!(
        run.primed(),
        vec![EXECUTION_AGENTS, FRAMEWORKS],
        "{}",
        run.context()
    );
    assert!(run.wrote("ready.json"), "{}", run.context());
    assert!(!run.wrote("last_auto_prime.json"), "{}", run.context());
}

/// Regression: `auto prime` exists to prime everything, and still does —
/// every catalog, in order, plus the glass-box report.
#[test]
fn auto_prime_still_primes_every_catalog() {
    let run = run(&["auto", "prime"]);
    assert_eq!(run.code, 0, "{}", run.context());
    assert_eq!(run.primed(), EVERY_CATALOG.to_vec(), "{}", run.context());
    assert!(run.wrote("last_auto_prime.json"), "{}", run.context());
    let report: serde_json::Value = serde_json::from_str(&run.stdout)
        .unwrap_or_else(|e| panic!("auto prime must print its report: {e}\n{}", run.context()));
    assert_eq!(report["kind"], "auto_prime", "{}", run.context());
    for key in [
        "mcp_enabled",
        "preferred_coding_model",
        "agents_ready",
        "frameworks_ready",
    ] {
        assert!(report.get(key).is_some(), "{key}\n{}", run.context());
    }
}

/// Regression: `mcp call` to a native target dispatches in process (here
/// `os_sysinfo`; `susi_solve` and `reason` read the selected model the same
/// way), so it primes every catalog first, as a mission does.
#[test]
fn in_process_mcp_call_primes_every_catalog() {
    let run = run(&["mcp", "call", "susi", "os_sysinfo"]);
    assert_eq!(run.primed(), EVERY_CATALOG.to_vec(), "{}", run.context());
}

/// `crown status` reads the registered agent capabilities and the selected
/// model, so it primes the agent scans and coding models, not leading MCP.
#[test]
fn crown_status_primes_agents_and_coding_models() {
    let run = run(&["crown", "status"]);
    assert_eq!(
        run.primed(),
        vec![CODING_MODELS, EXECUTION_AGENTS, FRAMEWORKS],
        "{}",
        run.context()
    );
}

/// A run's temp home is removed once the run is dropped.
#[test]
fn runs_clean_up_their_temp_home() {
    let run = run(&["frontier", "list"]);
    let root = run.host.as_ref().unwrap().root.clone();
    assert!(root.exists());
    drop(run);
    assert!(!root.exists(), "{}", root.display());
}

/// `auto status` is a snapshot that only full primes refresh; with none
/// recorded it says so on stderr, keeping stdout pure JSON, and primes nothing.
#[test]
fn auto_status_hints_when_no_full_prime_is_recorded() {
    let run = run(&["auto", "status"]);
    assert_eq!(run.code, 0, "{}", run.context());
    assert!(run.primed().is_empty(), "{}", run.context());
    let status: serde_json::Value = serde_json::from_str(&run.stdout)
        .unwrap_or_else(|e| panic!("auto status must print JSON: {e}\n{}", run.context()));
    assert!(status["last_auto_prime"].is_null(), "{}", run.context());
    assert!(run.stderr.contains("susi auto prime"), "{}", run.context());
}

/// A dummy key (never a real one) that makes the Anthropic coding models
/// ready, so coding-model auto-prefer has a model to pick.
const READY_CODING_MODEL: (&str, &str) = ("ANTHROPIC_API_KEY", "sk-ant-dummy-not-a-real-key");

/// `pick` (a command that pins a model) primed coding models and wrote
/// `expected` as the override; the next coding-model prime (`models list`)
/// leaves it unchanged.
fn assert_override_survives_next_prime(host: &Host, pick: &Run, expected: &str) {
    assert_eq!(pick.code, 0, "{}", pick.context());
    assert_eq!(pick.primed(), vec![CODING_MODELS], "{}", pick.context());
    assert_eq!(
        host.selected_override().as_deref(),
        Some(expected),
        "{}",
        pick.context()
    );
    let list = host.run(&["models", "list"]);
    assert_eq!(list.code, 0, "{}", list.context());
    assert_eq!(list.primed(), vec![CODING_MODELS], "{}", list.context());
    assert_eq!(
        host.selected_override().as_deref(),
        Some(expected),
        "the next coding-model prime overwrote the pin
{}",
        list.context()
    );
}

/// Regression: `select-model` primes coding models before it writes the
/// override, so the preferred model auto-prefer pins is ready and every later
/// prime (the daemon it starts, `models list`) keeps the user's pick instead
/// of `prefer()`-ing over it.
#[test]
fn select_model_pin_survives_the_next_coding_model_prime() {
    // Its own port offset: `select-model` starts a daemon in this home.
    let host = Host::new(&[READY_CODING_MODEL, ("SUSI_PORT_OFFSET", "733")]);
    let pick = host.run(&["select-model", "my-pick"]);
    // Stop the daemon it started before asserting anything, so a failure
    // never leaves it behind.
    let stop = host.run(&["stop"]);
    assert_eq!(stop.code, 0, "{}", stop.context());
    assert_override_survives_next_prime(&host, &pick, "my-pick");
}

/// Regression: `frontier prefer` writes the override (not the preferred
/// coding model), so it primes coding models first and its pin survives.
#[test]
fn frontier_prefer_pin_survives_the_next_coding_model_prime() {
    let host = Host::new(&[READY_CODING_MODEL]);
    let pick = host.run(&["frontier", "prefer", "claude-sonnet"]);
    assert_override_survives_next_prime(&host, &pick, "claude-sonnet-4-6");
}

/// Regression: as `frontier prefer`, for `openweight prefer`.
#[test]
fn openweight_prefer_pin_survives_the_next_coding_model_prime() {
    let host = Host::new(&[READY_CODING_MODEL]);
    let pick = host.run(&["openweight", "prefer", "deepseek-r1"]);
    assert_override_survives_next_prime(&host, &pick, "deepseek-r1");
}
