#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicUsize, Ordering},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}
fn fixture() -> PathBuf {
    let p = root().join("target").join(format!(
        "branch-handoff-test-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(p.join("scripts")).unwrap();
    p
}
fn bash() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(std::env::var_os("ProgramFiles").unwrap()).join("Git/bin/bash.exe")
    } else {
        PathBuf::from("bash")
    }
}
fn git(p: &Path, args: &[&str]) {
    let o = Command::new("git")
        .args(args)
        .current_dir(p)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}
fn init(p: &Path) {
    git(p, &["init", "--quiet", "-b", "main"]);
    git(p, &["config", "user.name", "handoff test"]);
    git(p, &["config", "user.email", "handoff@example.invalid"]);
    git(p, &["config", "commit.gpgsign", "false"]);
    git(p, &["config", "core.autocrlf", "false"]);
}
fn run(p: &Path, script: &str, plan: &str, fail: bool) -> Output {
    let log = p.join("calls.log").to_string_lossy().replace('\\', "/");
    Command::new(bash())
        .arg(root().join(script))
        .current_dir(p)
        .env("SUSI_BRANCH_TEST_PLAN", plan)
        .env("HANDOFF_TEST_LOG", log)
        .env("HANDOFF_TEST_FAIL", if fail { "1" } else { "0" })
        .output()
        .unwrap()
}
#[test]
fn branch_test_selection_runs_the_planned_packages_and_propagates_failure() {
    let p = fixture();
    init(&p);
    fs::write(p.join("scripts/check-hermetic-tests.sh"),"#!/usr/bin/env bash\nprintf '%s|%s|%s\\n' \"$SUSI_HERMETIC_RUNNER\" \"$SUSI_HERMETIC_EXCLUDE_LIVE\" \"$*\" >> \"$HANDOFF_TEST_LOG\"\n[ \"$HANDOFF_TEST_FAIL\" != 1 ]\n").unwrap();
    // The production script is executable on GitHub; make this fixture executable too.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            p.join("scripts/check-hermetic-tests.sh"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let plan = "test:core\ntest:susi\nroot-package:susi\nroot-test:release_workflow\nlint:core";
    let out = run(&p, "scripts/ci-branch-tests.sh", plan, false);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let calls = fs::read_to_string(p.join("calls.log")).unwrap();
    assert!(calls.contains("nextest|1|-p core"), "{calls}");
    assert!(
        calls.contains("nextest|1|-p susi --test release_workflow"),
        "{calls}"
    );
    fs::write(p.join("calls.log"), "").unwrap();
    assert!(run(&p, "scripts/ci-branch-tests.sh", "ALL", false)
        .status
        .success());
    assert!(fs::read_to_string(p.join("calls.log"))
        .unwrap()
        .contains("nextest|1|--workspace"));
    fs::write(p.join("calls.log"), "").unwrap();
    assert!(run(
        &p,
        "scripts/ci-branch-tests.sh",
        "test:susi\nroot-package:susi",
        false
    )
    .status
    .success());
    assert!(fs::read_to_string(p.join("calls.log"))
        .unwrap()
        .contains("nextest|1|-p susi --lib --bins"));
    fs::write(p.join("calls.log"), "").unwrap();
    assert!(run(&p, "scripts/ci-branch-tests.sh", "", false)
        .status
        .success());
    assert_eq!(fs::read_to_string(p.join("calls.log")).unwrap(), "");
    assert!(!run(&p, "scripts/ci-branch-tests.sh", plan, true)
        .status
        .success());
    assert!(!run(
        &p,
        "scripts/ci-branch-tests.sh",
        "unrecognized:input",
        false
    )
    .status
    .success());
    assert!(!run(&p, "scripts/ci-branch-tests.sh", "test:", false)
        .status
        .success());
}
#[test]
fn handoff_requires_a_github_remote_a_trusted_main_marker_and_unchanged_ci() {
    let p = fixture();
    init(&p);
    fs::create_dir_all(p.join(".github/workflows")).unwrap();
    fs::write(
        p.join(".github/workflows/test.yml"),
        "# SUSI_BRANCH_TEST_HANDOFF_V1\n",
    )
    .unwrap();
    fs::write(p.join("scripts/ci-branch-tests.sh"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::write(p.join("code.rs"), "fn original() {}\n").unwrap();
    git(&p, &["add", "."]);
    git(&p, &["commit", "--quiet", "-m", "base"]);
    git(&p, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(
        &p,
        &["remote", "add", "origin", "git@github.com:example/repo.git"],
    );
    fs::write(p.join("code.rs"), "fn changed() {}\n").unwrap();
    git(&p, &["add", "code.rs"]);
    git(&p, &["commit", "--quiet", "-m", "code"]);
    let invoke = || {
        Command::new(bash())
            .arg(root().join("scripts/ci-branch-handoff.sh"))
            .current_dir(&p)
            .env_remove("SUSI_LOCAL_GATE")
            .env_remove("SUSI_GATE_NOCACHE")
            .output()
            .unwrap()
    };
    assert!(invoke().status.success(), "valid branch must hand off");
    let forced_local = Command::new(bash())
        .arg(root().join("scripts/ci-branch-handoff.sh"))
        .current_dir(&p)
        .env("SUSI_LOCAL_GATE", "full")
        .output()
        .unwrap();
    assert!(!forced_local.status.success(), "full gate must stay local");
    git(&p, &["update-ref", "-d", "refs/remotes/origin/main"]);
    assert!(
        !invoke().status.success(),
        "a missing main ref must fall back"
    );
    git(&p, &["update-ref", "refs/remotes/origin/main", "HEAD^1"]);
    fs::write(p.join("scripts/ci-branch-tests.sh"), "changed\n").unwrap();
    assert!(
        !invoke().status.success(),
        "uncommitted CI changes must fall back"
    );
    git(&p, &["add", "scripts/ci-branch-tests.sh"]);
    git(&p, &["commit", "--quiet", "-m", "ci change"]);
    assert!(
        !invoke().status.success(),
        "committed CI changes must fall back"
    );
    git(&p, &["reset", "--hard", "--quiet", "HEAD^1"]);
    git(&p, &["remote", "set-url", "origin", "/tmp/local-only"]);
    assert!(
        !invoke().status.success(),
        "a non-GitHub remote cannot use the handoff"
    );
}

#[test]
fn remote_gate_does_not_claim_local_acceptance_or_cache_a_false_proof() {
    let p = fixture();
    init(&p);
    fs::write(p.join("Cargo.toml"), "[package]\nname = \"susi\"\n").unwrap();
    fs::write(
        p.join("scripts/ci-branch-handoff.sh"),
        "#!/usr/bin/env bash\n[ \"$HANDOFF_TEST_FAIL\" = 0 ]\n",
    )
    .unwrap();
    let source = fs::read_to_string(root().join("scripts/parallel-workflow.sh")).unwrap();
    let function = |name: &str| {
        let start = source.find(&format!("\n{name}() {{\n")).unwrap() + 1;
        let end = source[start..].find("\n}\n").unwrap() + start + 3;
        &source[start..end]
    };
    let harness = format!(
        r#"set -euo pipefail
root=$(pwd -P)
timed_step() {{ shift; "$@"; }}
gate_tests_kept() {{ :; }}
gate_format() {{ echo format >> calls; }}
seed_own_target() {{ echo target >> calls; }}
resolve_gate_plan() {{ gate_pkgs=core; gate_lint=core; gate_root_tests=; gate_rootpkg=susi; }}
gate_stamp() {{ echo uncached; }}
gate_lint_and_test() {{ echo full-gate >> calls; }}
{}
{}
gate
if [ "$HANDOFF_TEST_FAIL" = 0 ]; then
    [ "${{SUSI_BRANCH_CI_HANDOFF:-}}" = 1 ]
    [ -z "${{SUSI_ACCEPTANCE_COVERED:-}}" ]
    [ -z "$gate_pkgs" ]
    if acceptance_covered 'cargo test -p core selected'; then exit 8; fi
    [ ! -f .git/susi-gate-ok ]
    ! grep -q full-gate calls
else
    grep -q full-gate calls
    [ -z "${{SUSI_BRANCH_CI_HANDOFF:-}}" ]
fi
"#,
        function("gate"),
        function("acceptance_covered")
    );
    let path = p.join("harness.sh");
    fs::write(&path, harness).unwrap();
    for fail in [false, true] {
        fs::write(p.join("calls"), "").unwrap();
        let out = Command::new(bash())
            .arg(&path)
            .current_dir(&p)
            .env("SUSI_FINISH_REMOTE_GATE", "1")
            .env("SUSI_ACCEPTANCE_COVERED", "stale")
            .env("HANDOFF_TEST_FAIL", if fail { "1" } else { "0" })
            .env_remove("SUSI_LOCAL_GATE")
            .env_remove("SUSI_GATE_NOCACHE")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "fail={fail}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert!(source.contains("if [ -n \"$accept_cmd\" ] && acceptance_covered \"$accept_cmd\""));
    assert!(source.contains("timed_step close \"$susi_bin\" tasks close \"$task\""));
    let hook = fs::read_to_string(root().join(".githooks/pre-push")).unwrap();
    let remote = hook
        .split("elif [ \"${SUSI_BRANCH_CI_HANDOFF:-0}\" = 1 ]; then")
        .nth(1)
        .and_then(|s| s.split("elif [ -n \"$stamp\" ]").next())
        .expect("pre-push must revalidate a remote handoff");
    assert!(remote.contains("ci-branch-handoff.sh") && remote.contains("exit 1"));
}
