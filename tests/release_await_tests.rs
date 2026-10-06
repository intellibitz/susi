#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test fixtures/assertions
#![allow(missing_docs)] // integration test crate

//! `scripts/release-await-tests.sh` through a fake `gh`.
//!
//! A release builds four binaries for half an hour and publishes them, so
//! nothing builds until the commit they come from has passed the full suite.
//! The script reuses a passed run when one exists, otherwise dispatches the Test
//! workflow on the tag and waits - and what counts as "a run that executed the
//! full suite" is the part worth pinning: a branch-push run only compiles and
//! lints, so a green one proves nothing about the tests.

use std::path::{Path, PathBuf};
use std::process::Command;

const SHA: &str = "abc1234";
/// Far enough ahead that the script's "created after the dispatch" test holds.
const LATER: &str = "2099-01-01T00:00:00Z";

struct Fake {
    dir: PathBuf,
}

impl Fake {
    /// `before`: what `gh run list` returns until the workflow is dispatched;
    /// `after`: what it returns afterwards; `states`: one `status/conclusion`
    /// per `gh run view` poll, the last repeating.
    fn new(
        tag: &str,
        before: serde_json::Value,
        after: serde_json::Value,
        states: &[&str],
    ) -> Self {
        let dir = std::env::temp_dir().join(format!("susi-relwait-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("before.json"), before.to_string()).unwrap();
        std::fs::write(dir.join("after.json"), after.to_string()).unwrap();
        std::fs::write(dir.join("states"), states.join("\n")).unwrap();
        std::fs::write(
            dir.join("jobs.json"),
            r#"{"jobs":[{"name":"Test (gemi)","conclusion":"failure"},{"name":"Format Check","conclusion":"success"}]}"#,
        )
        .unwrap();
        let gh = dir.join("gh");
        std::fs::write(
            &gh,
            r#"#!/usr/bin/env bash
D="$(dirname "$0")"
echo "$*" >> "$D/calls"
jqexpr() { local p=; for a in "$@"; do [ "$p" = --jq ] && { echo "$a"; return; }; p=$a; done; }
case "$1 $2" in
"run list") if [ -f "$D/dispatched" ]; then cat "$D/after.json"; else cat "$D/before.json"; fi ;;
"workflow run")
  if [ -f "$D/dispatch_fail" ]; then echo "HTTP 403" >&2; exit 1; fi
  : > "$D/dispatched" ;;
"run view")
  case "$*" in
  *"--json jobs"*) jq -r "$(jqexpr "$@")" "$D/jobs.json" ;;
  *)
    n=$(cat "$D/n" 2>/dev/null || echo 0); echo $((n + 1)) > "$D/n"
    line=$(sed -n "$((n + 1))p" "$D/states"); [ -n "$line" ] || line=$(tail -n 1 "$D/states")
    status=${line%%/*}; conclusion=${line#*/}
    jq -n --arg s "$status" --arg c "$conclusion" '{status:$s, conclusion:(if $c == "" then null else $c end)}' | jq -r "$(jqexpr "$@")" ;;
  esac ;;
esac
"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self { dir }
    }

    fn run(&self, envs: &[(&str, &str)]) -> (i32, String) {
        let path = format!("{}:{}", self.dir.display(), std::env::var("PATH").unwrap());
        let out = Command::new(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/release-await-tests.sh"),
        )
        .args(["o/r", "v1.2.3", SHA])
        .env("PATH", path)
        .env("RELEASE_TESTS_POLL", "0")
        .envs(envs.iter().copied())
        .output()
        .unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned()
                + &String::from_utf8_lossy(&out.stderr),
        )
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.dir.join("calls")).unwrap_or_default()
    }

    fn flag(&self, name: &str) {
        std::fs::write(self.dir.join(name), "1").unwrap();
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn run_entry(id: u64, event: &str, branch: &str, conclusion: &str) -> serde_json::Value {
    serde_json::json!({
        "databaseId": id, "event": event, "headBranch": branch,
        "status": "completed", "conclusion": conclusion,
        "headSha": SHA, "createdAt": LATER,
    })
}

/// The dispatched run, as `gh run list --event workflow_dispatch` shows it.
fn dispatched(id: u64, sha: &str) -> serde_json::Value {
    serde_json::json!({ "databaseId": id, "event": "workflow_dispatch", "headBranch": "v1.2.3",
        "status": "queued", "conclusion": "", "headSha": sha, "createdAt": LATER })
}

#[test]
fn a_passed_dispatch_run_for_the_commit_is_reused_not_run_again() {
    let f = Fake::new(
        "reuse-dispatch",
        serde_json::json!([run_entry(11, "workflow_dispatch", "main", "success")]),
        serde_json::json!([]),
        &["completed/success"],
    );
    let (code, out) = f.run(&[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("already passed"), "{out}");
    assert!(
        !f.calls().contains("workflow run"),
        "a passed full suite must not be dispatched again: {}",
        f.calls()
    );
}

#[test]
fn a_passed_push_run_on_main_is_reused() {
    let f = Fake::new(
        "reuse-main",
        serde_json::json!([run_entry(12, "push", "main", "success")]),
        serde_json::json!([]),
        &["completed/success"],
    );
    let (code, out) = f.run(&[]);
    assert_eq!(code, 0, "{out}");
    assert!(!f.calls().contains("workflow run"), "{}", f.calls());
}

/// The branch gate compiles and lints; a green push run on an agent branch says
/// nothing about the tests, so it must never stand in for the suite.
#[test]
fn a_green_branch_push_run_never_counts_as_the_suite() {
    let f = Fake::new(
        "branch-push",
        serde_json::json!([run_entry(13, "push", "claude/some-task", "success")]),
        serde_json::json!([dispatched(77, SHA)]),
        &["completed/success"],
    );
    let (code, out) = f.run(&[]);
    assert_eq!(code, 0, "{out}");
    assert!(
        f.calls()
            .contains("workflow run test.yml --repo o/r --ref v1.2.3"),
        "the suite must be dispatched on the tag: {}",
        f.calls()
    );
}

#[test]
fn a_failed_earlier_run_does_not_count_and_the_suite_is_dispatched() {
    let f = Fake::new(
        "earlier-failed",
        serde_json::json!([run_entry(14, "workflow_dispatch", "main", "failure")]),
        serde_json::json!([dispatched(78, SHA)]),
        &["completed/success"],
    );
    let (code, out) = f.run(&[]);
    assert_eq!(code, 0, "{out}");
    assert!(f.calls().contains("workflow run"), "{}", f.calls());
}

#[test]
fn it_waits_through_queued_and_in_progress_until_the_run_passes() {
    let f = Fake::new(
        "waits",
        serde_json::json!([]),
        serde_json::json!([dispatched(79, SHA)]),
        &[
            "queued/",
            "in_progress/",
            "in_progress/",
            "completed/success",
        ],
    );
    let (code, out) = f.run(&[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("passed on abc1234"), "{out}");
    assert_eq!(
        f.calls().matches("run view 79").count(),
        4,
        "one poll per state: {}",
        f.calls()
    );
}

#[test]
fn a_failed_run_blocks_the_release_and_names_the_failing_job() {
    let f = Fake::new(
        "failed",
        serde_json::json!([]),
        serde_json::json!([dispatched(80, SHA)]),
        &["in_progress/", "completed/failure"],
    );
    let (code, out) = f.run(&[]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("did not pass"), "{out}");
    assert!(
        out.contains("Test (gemi)"),
        "the failing job is named: {out}"
    );
    assert!(!out.contains("Format Check"), "passing jobs are not: {out}");
}

#[test]
fn a_cancelled_run_blocks_the_release() {
    let f = Fake::new(
        "cancelled",
        serde_json::json!([]),
        serde_json::json!([dispatched(81, SHA)]),
        &["completed/cancelled"],
    );
    let (code, out) = f.run(&[]);
    assert_eq!(code, 1, "{out}");
}

#[test]
fn a_dispatch_that_is_refused_blocks_the_release() {
    let f = Fake::new(
        "refused",
        serde_json::json!([]),
        serde_json::json!([]),
        &["x/"],
    );
    f.flag("dispatch_fail");
    let (code, out) = f.run(&[]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("could not dispatch"), "{out}");
}

/// A run for some other commit is not this release's suite.
#[test]
fn a_run_for_another_commit_is_not_taken_for_ours() {
    let f = Fake::new(
        "other-sha",
        serde_json::json!([]),
        serde_json::json!([dispatched(82, "deadbee")]),
        &["completed/success"],
    );
    let (code, out) = f.run(&[("RELEASE_TESTS_FIND_MAX", "0")]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("no Test run appeared"), "{out}");
    assert!(
        !f.calls().contains("run view 82"),
        "it must not watch someone else's run: {}",
        f.calls()
    );
}

#[test]
fn a_suite_that_never_finishes_is_not_waited_on_forever() {
    let f = Fake::new(
        "stuck",
        serde_json::json!([]),
        serde_json::json!([dispatched(83, SHA)]),
        &["in_progress/"],
    );
    let (code, out) = f.run(&[("RELEASE_TESTS_WAIT_MAX", "0")]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("had not finished"), "{out}");
}
