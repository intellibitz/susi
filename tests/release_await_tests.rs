#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test fixtures/assertions
#![allow(missing_docs)] // integration test crate

//! `scripts/release-await-tests.sh` through a fake `gh`.
//!
//! A release builds four binaries for half an hour and publishes them, so
//! nothing builds until the commit they come from has passed the full suite.
//! The script reuses a passed run when one exists, otherwise dispatches the Test
//! workflow on the tag and waits - and what counts as "a run that executed the
//! full suite" is the part worth pinning: a branch-push run only compiles and
//! lints, so a green one proves nothing about the tests, and neither does the
//! merge bot's `gate=branch` retest, which is a dispatch run like the suite.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A full-gate run's jobs: the suite ran and passed.
const SUITE_PASSED: &str = r#"{"jobs":[{"name":"Doc Tests + Clippy","conclusion":"success"},{"name":"Test (gemi)","conclusion":"success"},{"name":"Format Check","conclusion":"success"}]}"#;
/// A full-gate run with one red shard.
const SHARD_FAILED: &str = r#"{"jobs":[{"name":"Doc Tests + Clippy","conclusion":"success"},{"name":"Test (gemi)","conclusion":"failure"},{"name":"Format Check","conclusion":"success"}]}"#;
/// The merge bot's `gate=branch` retest of a resynced head: green, but it ran
/// what a branch push runs - the shards, lint and e2e were all skipped.
const BRANCH_GATE: &str = r#"{"jobs":[{"name":"Compile Check (branch pushes)","conclusion":"success"},{"name":"Workflow Compliance","conclusion":"success"},{"name":"Doc Tests + Clippy","conclusion":"skipped"},{"name":"Merge the retested head","conclusion":"success"}]}"#;

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

fn path_with_prefix(prefix: &Path) -> std::ffi::OsString {
    let mut paths = vec![prefix.to_path_buf()];
    if let Some(path) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&path));
    }
    std::env::join_paths(paths).unwrap()
}

const SHA: &str = "abc1234";
/// Far enough ahead that the script's "created after the dispatch" test holds.
const LATER: &str = "2099-01-01T00:00:00Z";

struct Fake {
    dir: PathBuf,
}

impl Fake {
    /// `before`: what `gh run list` returns until the workflow is dispatched;
    /// `after`: what it returns afterwards; `states`: one `status/conclusion`
    /// per `gh run view` poll, the last repeating. Every run's jobs are a
    /// passed suite unless [`Fake::jobs`] says otherwise.
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
        std::fs::write(dir.join("jobs.json"), SUITE_PASSED).unwrap();
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
  *"--json jobs"*)
    if [ -f "$D/jobs_fail_once" ]; then rm -f "$D/jobs_fail_once"; echo "HTTP 502" >&2; exit 1; fi
    f="$D/jobs-$3.json"; [ -f "$f" ] || f="$D/jobs.json"
    jq -r "$(jqexpr "$@")" "$f" ;;
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
        let path = path_with_prefix(&self.dir);
        let out = script_command(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/release-await-tests.sh"),
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

    /// What `gh run view <id> --json jobs` reports for run `id`.
    fn jobs(&self, id: u64, jobs: &str) {
        std::fs::write(self.dir.join(format!("jobs-{id}.json")), jobs).unwrap();
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
    dispatched_at(id, sha, LATER)
}

fn dispatched_at(id: u64, sha: &str, created: &str) -> serde_json::Value {
    serde_json::json!({ "databaseId": id, "event": "workflow_dispatch", "headBranch": "v1.2.3",
        "status": "queued", "conclusion": "", "headSha": sha, "createdAt": created })
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

/// The merge bot retests a resynced head by dispatching the Test workflow with
/// `gate=branch`. `gh run list` shows that run as a green `workflow_dispatch`,
/// exactly like the suite, and it was reused as "the full suite already passed"
/// although it ran no test. A release dry run on such a branch then built with
/// no suite behind it.
#[test]
fn a_green_branch_gate_retest_of_the_commit_is_not_taken_for_the_suite() {
    let f = Fake::new(
        "branch-gate-reuse",
        serde_json::json!([run_entry(
            15,
            "workflow_dispatch",
            "claude/some-task",
            "success"
        )]),
        serde_json::json!([dispatched(84, SHA)]),
        &["completed/success"],
    );
    f.jobs(15, BRANCH_GATE);
    let (code, out) = f.run(&[]);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("already passed"), "{out}");
    assert!(
        f.calls()
            .contains("workflow run test.yml --repo o/r --ref v1.2.3 -f gate=full"),
        "the full suite must be dispatched, and asked for as the full gate: {}",
        f.calls()
    );
    assert!(out.contains("passed on abc1234 (run 84)"), "{out}");
}

/// The dispatch returns no run id, so the script finds its run by commit and
/// time. A branch-gate retest dispatched on the same commit in that window is
/// found the same way; when it is the one watched, its green finish must not
/// pass for the suite - the script looks again and waits for its own run.
#[test]
fn a_branch_gate_retest_dispatched_beside_the_suite_is_passed_over() {
    let f = Fake::new(
        "branch-gate-beside",
        serde_json::json!([]),
        serde_json::json!([
            dispatched_at(85, SHA, "2099-01-01T00:00:00Z"),
            dispatched_at(86, SHA, "2099-01-01T00:01:00Z"),
        ]),
        &["completed/success"],
    );
    f.jobs(86, BRANCH_GATE);
    let (code, out) = f.run(&[]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("the full suite passed on abc1234 (run 85)"),
        "{out}"
    );
    assert!(
        !out.contains("(run 86)") && out.contains("run 86 passed without running the suite"),
        "{out}"
    );
}

/// Telling the suite from a branch gate takes one more `gh` call. When that call
/// fails, the run is neither: the script must ask again, not pass over its own
/// green suite and then fail the release for want of another run.
#[test]
fn a_green_run_whose_jobs_cannot_be_read_is_asked_again_not_passed_over() {
    let f = Fake::new(
        "jobs-unreadable",
        serde_json::json!([]),
        serde_json::json!([dispatched(88, SHA)]),
        &["completed/success"],
    );
    f.flag("jobs_fail_once");
    let (code, out) = f.run(&[("RELEASE_TESTS_FIND_MAX", "0")]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("passed on abc1234 (run 88)"), "{out}");
    assert_eq!(
        f.calls()
            .matches("run view 88 --repo o/r --json jobs")
            .count(),
        2,
        "{}",
        f.calls()
    );
}

/// A branch-gate run is never the suite, even when it is the only run there is.
#[test]
fn a_branch_gate_retest_alone_never_passes_the_release() {
    let f = Fake::new(
        "branch-gate-alone",
        serde_json::json!([]),
        serde_json::json!([dispatched(87, SHA)]),
        &["completed/success"],
    );
    f.jobs(87, BRANCH_GATE);
    let (code, out) = f.run(&[("RELEASE_TESTS_FIND_MAX", "0")]);
    assert_eq!(code, 1, "{out}");
    assert!(!out.contains("full suite passed"), "{out}");
    assert!(out.contains("no Test run appeared"), "{out}");
}

/// The script tells a branch-gate dispatch from the suite by one job of
/// test.yml: it must be skipped on the branch gate exactly when the shards are,
/// and run on every other dispatch. Renaming it, or gating it differently,
/// silently turns every reuse into a fresh dispatch or, worse, lets a branch
/// gate through.
#[test]
fn the_job_that_proves_a_dispatch_ran_the_suite_is_skipped_exactly_with_the_shards() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let script = std::fs::read_to_string(root.join("scripts/release-await-tests.sh")).unwrap();
    let name = script
        .lines()
        .find_map(|l| l.strip_prefix("suite_job=\""))
        .and_then(|rest| rest.strip_suffix('"'))
        .expect("release-await-tests.sh names the job that proves the suite ran");
    let workflow = std::fs::read_to_string(root.join(".github/workflows/test.yml")).unwrap();
    // A job's lines, from its two-space-indented id to the next one.
    let job = |id: &str| -> Vec<&str> {
        workflow
            .lines()
            .skip_while(|l| *l != format!("  {id}:"))
            .skip(1)
            .take_while(|l| !(l.starts_with("  ") && !l.starts_with("   ") && l.ends_with(':')))
            .collect()
    };
    let gate = |lines: &[&str]| -> String {
        lines
            .iter()
            .find_map(|l| l.strip_prefix("    if: "))
            .unwrap_or_default()
            .to_string()
    };
    let lint = job("lint");
    assert!(
        lint.contains(&format!("    name: {name}").as_str()),
        "test.yml's `lint` job must be named `{name}`, the job release-await-tests.sh reads"
    );
    let shards = gate(&job("test"));
    assert!(
        shards.contains("inputs.gate != 'branch'"),
        "the shards no longer skip on the branch gate: {shards}"
    );
    assert_eq!(
        gate(&lint),
        shards,
        "`{name}` must run exactly when the shards do, or its success no longer means the \
         suite ran"
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
    // Counted on the status read: the green run's jobs are read once more, to
    // see that its suite ran.
    assert_eq!(
        f.calls()
            .matches("run view 79 --repo o/r --json status")
            .count(),
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
    f.jobs(80, SHARD_FAILED);
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
