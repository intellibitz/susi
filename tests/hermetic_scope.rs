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

fn script_command(script: &Path) -> Command {
    #[cfg(windows)]
    {
        let mut command = Command::new("sh");
        command.arg(script);
        command
    }
    #[cfg(not(windows))]
    {
        Command::new(script)
    }
}

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/check-hermetic-tests.sh")
}

fn path_with_prefix(prefix: &Path) -> std::ffi::OsString {
    let mut paths = vec![prefix.to_path_buf()];
    if let Some(path) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&path));
    }
    std::env::join_paths(paths).unwrap()
}

struct World {
    root: PathBuf,
    bins: PathBuf,
    home: PathBuf,
    paths: PathBuf,
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
        // `metadata` is answered by the real cargo: the nextest runner reads the
        // workspace to pick the doc-test packages, and a fake has no workspace.
        let body = format!(
            "#!/usr/bin/env bash\nif [ \"$1\" = metadata ]; then exec \"$TEST_REAL_CARGO\" \"$@\"; fi\necho \"$*\" >>\"$TEST_CARGO_CALLS\"\nprintf '%s\\n' \"${{APPDATA:-}}\" \"${{LOCALAPPDATA:-}}\" \"${{ORT_CACHE_DIR:-}}\" >\"$TEST_CARGO_PATHS\"\n{leak}exit 0\n"
        );
        let path = bins.join("cargo");
        std::fs::write(&path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self {
            home: root.join("throwaway.tsv"),
            paths: root.join("native-paths.txt"),
            root,
            bins,
        }
    }

    fn run(&self, args: &[&str]) -> (i32, String) {
        self.run_with(&[], args)
    }

    fn run_with(&self, envs: &[(&str, &str)], args: &[&str]) -> (i32, String) {
        let path = path_with_prefix(&self.bins);
        let out = script_command(&script())
            .args(args)
            .env("CARGO_HOME", self.bins.parent().unwrap())
            .env("PATH", path)
            .env("TEST_CARGO_CALLS", &self.home)
            .env("TEST_CARGO_PATHS", &self.paths)
            .env("TEST_REAL_CARGO", env!("CARGO"))
            .env_remove("SUSI_HERMETIC_RUNNER")
            .env_remove("SUSI_HERMETIC_RETRIES")
            .envs(envs.iter().copied())
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

#[cfg(windows)]
#[test]
fn windows_native_config_and_build_caches_are_redirected() {
    let w = World::new("windows-paths", false);
    let (code, out) = w.run(&["-p", "susi"]);
    assert_eq!(code, 0, "{out}");
    let paths = std::fs::read_to_string(&w.paths).unwrap();
    let real_appdata = std::env::var("APPDATA").unwrap_or_default().to_lowercase();
    let real_local = std::env::var("LOCALAPPDATA")
        .unwrap_or_default()
        .to_lowercase();
    let paths: Vec<_> = paths.lines().map(str::to_lowercase).collect();
    assert_eq!(paths.len(), 3, "{paths:?}");
    assert_ne!(paths[0], real_appdata, "{paths:?}");
    assert_ne!(paths[1], real_local, "{paths:?}");
    assert!(paths[0].ends_with("\\appdata\\roaming"), "{paths:?}");
    assert!(paths[1].ends_with("\\appdata\\local"), "{paths:?}");
    assert!(
        paths[2].ends_with("\\appdata\\local\\ort-cache"),
        "{paths:?}"
    );
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

/// The local finish gate's single test run. nextest retries a failing *test*
/// alone and runs one process per test, so the gate does not rerun a whole crate
/// for one flake; the doc-tests nextest never runs follow for the selected crates
/// that have a library (`-p` on a bin-only package is a cargo error).
#[test]
fn the_nextest_runner_retries_per_test_and_runs_doc_tests_for_library_crates_only() {
    let w = World::new("nextest", false);
    let (code, out) = w.run_with(
        &[("SUSI_HERMETIC_RUNNER", "nextest")],
        &["-p", "susi-paths", "-p", "xtask"],
    );
    assert_eq!(code, 0, "{out}");
    let calls = w.cargo_calls();
    assert!(
        calls.contains(
            "nextest run --locked --no-fail-fast --no-tests=pass --status-level slow --final-status-level slow --retries 2 -p susi-paths -p xtask"
        ),
        "one hermetic nextest pass over the selection, a crate with no tests passing: {calls}"
    );
    let doc_calls: Vec<&str> = calls.lines().filter(|l| l.contains("--doc")).collect();
    assert_eq!(
        doc_calls,
        ["test --locked --doc -p susi-paths"],
        "doc-tests for the library crate only - xtask is bin-only, and asking cargo for \
         its doc-tests is an error: {calls}"
    );
    assert!(
        !calls.contains("test --locked --no-fail-fast"),
        "the tests run once, under nextest, not again under cargo test: {calls}"
    );
}

#[test]
fn the_nextest_runner_keeps_the_leak_assertion() {
    let w = World::new("nextest-leak", true);
    let (code, out) = w.run_with(
        &[("SUSI_HERMETIC_RUNNER", "nextest")],
        &["-p", "susi-paths"],
    );
    assert_eq!(code, 1, "a leak must fail under nextest too: {out}");
    assert!(
        out.contains("tests wrote into the inherited SUSI_HOME"),
        "{out}"
    );
}

#[test]
fn the_retry_count_is_configurable() {
    let w = World::new("retries", false);
    let (code, out) = w.run_with(
        &[
            ("SUSI_HERMETIC_RUNNER", "nextest"),
            ("SUSI_HERMETIC_RETRIES", "5"),
        ],
        &["-p", "susi-paths"],
    );
    assert_eq!(code, 0, "{out}");
    assert!(
        w.cargo_calls().contains("--retries 5"),
        "{}",
        w.cargo_calls()
    );
}

#[test]
fn an_unknown_runner_is_refused_before_anything_runs() {
    let w = World::new("badrunner", false);
    let (code, out) = w.run_with(&[("SUSI_HERMETIC_RUNNER", "bazel")], &["-p", "susi-paths"]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("SUSI_HERMETIC_RUNNER"), "{out}");
    assert_eq!(w.cargo_calls(), "", "no cargo call before the refusal");
}

/// sccache's client starts a server when none runs, and that server keeps the
/// client's environment: one started by this run's first cargo kept the run's
/// throwaway TMP and failed every compile on the machine once the EXIT trap
/// removed it. So the script has the server started from the real environment
/// (`scripts/setup-sccache.sh --ensure-server`) before it switches HOME and
/// TMP: the fake records the TMP and the cache settings it was started with,
/// and the order against the fake cargo.
#[test]
fn the_sccache_server_is_started_from_the_real_environment_before_the_switch() {
    let w = World::new("sccache-server", false);
    // The fake `sccache` sits beside the fake cargo in CARGO_HOME/bin, which the
    // script puts first on PATH; it logs into the same file as cargo, so the
    // order of the two is one list.
    let fake = w.bins.join("sccache");
    std::fs::write(
        &fake,
        "#!/usr/bin/env bash\necho \"sccache $* TMP=${TMP:-} DIR=${SCCACHE_DIR:-} SIZE=${SCCACHE_CACHE_SIZE:-} IDLE=${SCCACHE_IDLE_TIMEOUT:-}\" >>\"$TEST_CARGO_CALLS\"\nexit 0\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    // The block scripts/setup-sccache.sh writes, with a cache of this test's own.
    let cache = w.root.join("cache");
    let cache_toml = cache.to_string_lossy().replace('\\', "/");
    std::fs::write(
        w.bins.parent().unwrap().join("config.toml"),
        format!(
            "# >>> susi sccache (scripts/setup-sccache.sh) >>>\n[build]\nrustc-wrapper = \"sccache\"\n\n[env]\nSCCACHE_DIR = \"{cache_toml}\"\nSCCACHE_CACHE_SIZE = \"7G\"\n# <<< susi sccache <<<\n"
        ),
    )
    .unwrap();
    let real_tmp = w.root.join("real-tmp");
    std::fs::create_dir_all(&real_tmp).unwrap();
    let real_tmp_s = real_tmp.to_string_lossy().into_owned();
    let (code, out) = w.run_with(
        &[
            ("TMP", real_tmp_s.as_str()),
            ("TMPDIR", real_tmp_s.as_str()),
        ],
        &["-p", "susi"],
    );
    assert_eq!(code, 0, "{out}");
    let calls = w.cargo_calls();
    let lines: Vec<&str> = calls.lines().collect();
    let server = lines
        .iter()
        .position(|l| l.starts_with("sccache --start-server"))
        .unwrap_or_else(|| panic!("the server was not started: {calls}"));
    let cargo = lines
        .iter()
        .position(|l| !l.starts_with("sccache"))
        .unwrap_or_else(|| panic!("cargo never ran: {calls}"));
    assert!(
        server < cargo,
        "the server must be started before any cargo: {calls}"
    );
    let started = lines[server];
    // The shell may spell TMP its own way (MSYS turns a Windows path into a
    // POSIX one), and this test itself may be running under the gate's own
    // throwaway temp, so judge the directory's last name: the one this test
    // passed in, not the run's `.../appdata/local/temp`.
    assert!(
        started.contains("real-tmp DIR="),
        "the server must keep the real TMP, not the run's throwaway: {started}"
    );
    assert!(
        started.contains(&format!("DIR={cache_toml}")) && started.contains("SIZE=7G"),
        "the server takes the cache's place and size from the written block: {started}"
    );
    assert!(started.contains("IDLE=0"), "no idle exit: {started}");
    assert_eq!(
        lines.iter().filter(|l| l.starts_with("sccache")).count(),
        1,
        "started once, before the switch: {calls}"
    );
}

/// Where the cache was never set up (no block in the cargo config), nothing
/// starts a server: without the wrapper no cargo would use it.
#[test]
fn no_sccache_block_means_no_server_start() {
    let w = World::new("sccache-no-block", false);
    let fake = w.bins.join("sccache");
    std::fs::write(
        &fake,
        "#!/usr/bin/env bash\necho \"sccache $*\" >>\"$TEST_CARGO_CALLS\"\nexit 0\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let (code, out) = w.run(&["-p", "susi"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        !w.cargo_calls().contains("sccache"),
        "no block, no server: {}",
        w.cargo_calls()
    );
}
