// Optional POSIX/hosted compatibility fixtures execute shell scripts.
// Native Windows delivery is covered by the Rust coordinator, bootstrap,
// installer and release suites; it must not require a POSIX userland.
#![allow(missing_docs)]
#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! `scripts/auto-merge-pr.sh` and `scripts/reconcile-prs.sh` through a fake
//! `gh`: a green PR always merges, a refused one is explained once, and no PR
//! is left waiting forever.

use std::path::{Path, PathBuf};
use std::process::Command;

fn script_command(script: &Path) -> Command {
    #[cfg(windows)]
    {
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "git() { \"$SUSI_TEST_GIT\" \"$@\"; }; export -f git; \"$1\" \"${@:2}\"",
            "susi-test",
        ]);
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

fn host_git_binary() -> String {
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            #[cfg(windows)]
            let candidate = dir.join("git.exe");
            #[cfg(not(windows))]
            let candidate = dir.join("git");
            if candidate.is_file() {
                return candidate.to_string_lossy().replace('\\', "/");
            }
        }
    }
    if cfg!(windows) {
        "git.exe".to_string()
    } else {
        "/usr/bin/git".to_string()
    }
}

struct Fake {
    dir: PathBuf,
}

impl Fake {
    /// `prs`: JSON array of PRs; `runs`: JSON object sha -> {status,conclusion,url}.
    fn new(tag: &str, prs: serde_json::Value, runs: serde_json::Value) -> Self {
        let dir = std::env::temp_dir().join(format!("susi-am-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("prs.json"), prs.to_string()).unwrap();
        std::fs::write(dir.join("runs.json"), runs.to_string()).unwrap();
        std::fs::write(dir.join("comments.txt"), "").unwrap();
        let gh = dir.join("gh");
        std::fs::write(
            &gh,
            r#"#!/usr/bin/env bash
D="$(dirname "$0")"
echo "$*" >> "$D/calls"
jqexpr() { local p=; for a in "$@"; do [ "$p" = --jq ] && { echo "$a"; return; }; p=$a; done; }
argafter() { local flag=$1; shift; local p=; for a in "$@"; do [ "$p" = "$flag" ] && { echo "$a"; return; }; p=$a; done; }
case "$1 $2" in
api\ *)
  for a in "$@"; do case "$a" in
    *"compare/main"*)
      [ -f "$D/compare_fail" ] && { echo "HTTP 502" >&2; exit 1; }
      spec=$(echo "${a##*compare/}" | tr '/.' '__')
      cat "$D/cmp-$spec" 2>/dev/null || cat "$D/comparison" 2>/dev/null || echo ahead ;;
    *"compare/"*)
      cat "$D/treecmp" 2>/dev/null || echo different ;;
    *"pulls/"*)
      cat "$D/prcommits" 2>/dev/null || echo '[]' ;;
  esac; done ;;
"pr list")
  [ -f "$D/prlist_fail" ] && { echo "HTTP 502" >&2; exit 1; }
  jq -r "$(jqexpr "$@")" "$D/prs.json" ;;
"pr merge") if [ -f "$D/merge_fail" ]; then echo "merge refused" >&2; exit 1; fi ;;
"pr update-branch") if [ -f "$D/update_fail" ]; then echo "branch is not mergeable" >&2; exit 1; fi ;;
"pr view")
  case "$*" in
  *"--json mergeCommit"*) printf '{"mergeCommit":{"oid":"%s"}}\n' "$(cat "$D/mergesha" 2>/dev/null)" | jq -r "$(jqexpr "$@")" ;;
  *"--json mergeable"*) echo "{\"mergeable\":\"$(cat "$D/mergeable" 2>/dev/null || echo MERGEABLE)\"}" | jq -r "$(jqexpr "$@")" ;;
  *"--json comments"*) jq -Rs '{comments:[{body:.}]}' "$D/comments.txt" | jq -r "$(jqexpr "$@")" ;;
  esac ;;
"pr comment") printf '%s\n' "$3 :: $(argafter --body "$@")" >> "$D/comments.txt" ;;
"pr close") ;;
"workflow run") ;;
"run list")
  [ -f "$D/runlist_fail" ] && { echo "HTTP 502" >&2; exit 1; }
  sha=$(argafter --commit "$@")
  if jq -e --arg s "$sha" 'has($s)' "$D/runs.json" >/dev/null; then arr="[$(jq -c --arg s "$sha" '.[$s]' "$D/runs.json")]"; else arr='[]'; fi
  echo "$arr" | jq -r "$(jqexpr "$@")" ;;
esac
"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // Ref attestations push with git: record them instead of letting a
        // test write refs to the real remote. The merge-gate commands are
        // faked the same way — flag files decide their answers — and
        // everything else is real git: `hash-object -w` needs a real object
        // store.
        let git = dir.join("git");
        let real_git = host_git_binary();
        let shim = format!(
            r#"#!/usr/bin/env bash
D="$(dirname "$0")"
case "$1" in
push) echo "$*" >> "$D/gitpushes"; [ -f "$D/pushfail" ] && exit 1; exit 0 ;;
fetch) echo "git $*" >> "$D/calls"; exit 0 ;;
rev-parse)
  case "$*" in
  *"^{{tree}}"*) cat "$D/srvtree" 2>/dev/null || echo srctree; exit 0 ;;
  *"origin/main"*) cat "$D/basesha" 2>/dev/null || echo base001; exit 0 ;;
  *) echo "${{!#}}"; exit 0 ;;
  esac ;;
merge-tree) echo "git $*" >> "$D/calls"; [ -f "$D/conflict" ] && exit 1; cat "$D/treefile" 2>/dev/null || echo tree999; exit 0 ;;
commit-tree)
  # A CI runner has no git identity: refuse as git does unless the gate
  # supplies its own.
  [ -n "${{GIT_AUTHOR_NAME:-}}" ] && [ -n "${{GIT_AUTHOR_EMAIL:-}}" ] \
    && [ -n "${{GIT_COMMITTER_NAME:-}}" ] && [ -n "${{GIT_COMMITTER_EMAIL:-}}" ] \
    || {{ echo "fatal: empty ident name not allowed" >&2; exit 128; }}
  echo "git $*" >> "$D/calls"; cat "$D/mcfile" 2>/dev/null || echo mc999; exit 0 ;;
checkout) echo "git $*" >> "$D/calls"; [ -f "$D/checkoutfail" ] && exit 1; exit 0 ;;
esac
exec "{}" "$@"
"#,
            real_git
        );
        std::fs::write(&git, shim).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self { dir }
    }

    fn script(&self, name: &str, args: &[&str]) -> (i32, String) {
        self.script_env(name, args, &[])
    }

    fn script_env(&self, name: &str, args: &[&str], envs: &[(&str, &str)]) -> (i32, String) {
        let path = path_with_prefix(&self.dir);
        let out = script_command(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("scripts")
                .join(name),
        )
        .args(args)
        .env("PATH", path)
        .env("SUSI_TEST_GIT", self.dir.join("git"))
        // As on a CI runner: no git identity unless the script supplies one.
        .env_remove("GIT_AUTHOR_NAME")
        .env_remove("GIT_AUTHOR_EMAIL")
        .env_remove("GIT_COMMITTER_NAME")
        .env_remove("GIT_COMMITTER_EMAIL")
        // The merge bot's switches and the gate's wrappers come from the
        // test, never from the shell that runs it.
        .env_remove("SUSI_INLINE_MERGE_GATE")
        .env_remove("SUSI_NO_RESYNC")
        .env_remove("SUSI_RECONCILING")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .envs(envs.iter().copied())
        .output()
        .unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned()
                + &String::from_utf8_lossy(&out.stderr),
        )
    }

    /// A stub for merge-gate.sh: records its argv, exits 1 when `gatefail` is
    /// set, 0 otherwise.
    fn gate_stub(&self) -> String {
        let stub = self.dir.join("gate-stub");
        std::fs::write(
            &stub,
            "#!/usr/bin/env bash\nD=\"$(dirname \"$0\")\"\necho \"$*\" >> \"$D/gatecalls\"\n[ -f \"$D/gatefail\" ] && exit 1\nexit 0\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        stub.to_string_lossy().into_owned()
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.dir.join("calls")).unwrap_or_default()
    }

    fn comments(&self) -> String {
        std::fs::read_to_string(self.dir.join("comments.txt")).unwrap()
    }

    fn git_pushes(&self) -> String {
        std::fs::read_to_string(self.dir.join("gitpushes")).unwrap_or_default()
    }

    fn flag(&self, name: &str, content: &str) {
        std::fs::write(self.dir.join(name), content).unwrap();
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn pr(n: u32, branch: &str, sha: &str, updated: &str, draft: bool) -> serde_json::Value {
    serde_json::json!({"number": n, "headRefName": branch, "headRefOid": sha, "updatedAt": updated, "isDraft": draft})
}

const NOW: &str = "2999-01-01T00:00:00Z"; // never stale
const OLD: &str = "2001-01-01T00:00:00Z"; // always stale

fn green() -> serde_json::Value {
    serde_json::json!({"status":"completed","conclusion":"success","url":"https://x/run/1"})
}

fn red() -> serde_json::Value {
    serde_json::json!({"status":"completed","conclusion":"failure","url":"https://x/run/2"})
}

#[test]
fn a_green_pr_merges_pinned_to_the_tested_commit_and_the_main_suite_runs() {
    let f = Fake::new(
        "merge",
        serde_json::json!([pr(5, "feat", "aaa", NOW, false)]),
        serde_json::json!({}),
    );
    let (code, out) = f.script("auto-merge-pr.sh", &["o/r", "feat", "aaa"]);
    assert_eq!(
        code,
        0,
        "{out}\npushes: {}\ncalls: {}",
        f.git_pushes(),
        f.calls()
    );
    assert!(
        f.calls()
            .contains("pr merge 5 --repo o/r --merge --match-head-commit aaa"),
        "{}",
        f.calls()
    );
    assert!(
        f.calls()
            .contains("workflow run test.yml --repo o/r --ref main"),
        "{}",
        f.calls()
    );
    assert_eq!(f.comments(), "");
}

/// The common merge is content-identical to the tested head — `finish`
/// integrates origin/main before pushing, so the merge only gains a parent —
/// and `close` already ran the task's acceptance on those exact bytes, so the
/// post-merge acceptance is attested by identity. That proves the acceptance
/// and nothing else: the branch run compiles and lints without executing a
/// test, so skipping the main suite here would leave the merged tree's tests
/// to run nowhere in CI. The suite dispatches regardless.
#[test]
fn a_merge_identical_to_the_tested_head_is_attested_and_still_runs_the_main_suite() {
    let f = Fake::new(
        "ident",
        serde_json::json!([pr(5, "feat", "aaa", NOW, false)]),
        serde_json::json!({}),
    );
    f.flag("mergesha", "m1");
    f.flag("treecmp", "identical");
    f.flag("prcommits", "feat work\n\nTask: T-FIXTURE-7\n");
    let (code, out) = f.script("auto-merge-pr.sh", &["o/r", "feat", "aaa"]);
    assert_eq!(
        code,
        0,
        "{out}\npushes: {}\ncalls: {}",
        f.git_pushes(),
        f.calls()
    );
    assert!(
        out.contains("identical"),
        "{out}\npushes: {}\ncalls: {}",
        f.git_pushes(),
        f.calls()
    );
    assert!(
        f.calls()
            .contains("workflow run test.yml --repo o/r --ref main"),
        "the branch run executes no tests, so the suite must run on main: {}",
        f.calls()
    );
    let pushes = f.git_pushes();
    assert!(pushes.contains("refs/merged/T-FIXTURE-7"), "{pushes}");
    assert!(pushes.contains("refs/verified/T-FIXTURE-7"), "{pushes}");
}

/// A merge that raced another produces a tree nobody has built — it is not
/// attested by identity, and the suite dispatches for it as for every merge.
#[test]
fn a_merge_that_differs_from_the_tested_head_still_runs_the_main_suite() {
    let f = Fake::new(
        "difftree",
        serde_json::json!([pr(5, "feat", "aaa", NOW, false)]),
        serde_json::json!({}),
    );
    f.flag("mergesha", "m1");
    f.flag("treecmp", "ahead");
    let (code, out) = f.script("auto-merge-pr.sh", &["o/r", "feat", "aaa"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        f.calls()
            .contains("workflow run test.yml --repo o/r --ref main"),
        "{}",
        f.calls()
    );
}

/// A failed `refs/verified` write is best effort: it neither fails the merge
/// nor skips the suite, and the main run's `verify` job records the acceptance.
#[test]
fn a_failed_attestation_neither_fails_the_merge_nor_skips_the_suite() {
    let f = Fake::new(
        "attestfail",
        serde_json::json!([pr(5, "feat", "aaa", NOW, false)]),
        serde_json::json!({}),
    );
    f.flag("mergesha", "m1");
    f.flag("treecmp", "identical");
    f.flag("prcommits", "feat work\n\nTask: T-FIXTURE-7\n");
    f.flag("pushfail", "1");
    let (code, out) = f.script("auto-merge-pr.sh", &["o/r", "feat", "aaa"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        f.calls()
            .contains("workflow run test.yml --repo o/r --ref main"),
        "{}",
        f.calls()
    );
}

#[test]
fn a_stale_or_missing_pr_is_a_quiet_no_op_not_a_failure() {
    let f = Fake::new(
        "stale",
        serde_json::json!([pr(5, "feat", "newer", NOW, false)]),
        serde_json::json!({}),
    );
    let (code, out) = f.script("auto-merge-pr.sh", &["o/r", "feat", "older"]);
    assert_eq!(code, 0, "{out}");
    assert!(!f.calls().contains("pr merge"), "{}", f.calls());
}

#[test]
fn a_conflicting_pr_is_explained_exactly_once() {
    let f = Fake::new(
        "conflict",
        serde_json::json!([pr(5, "feat", "aaa", NOW, false)]),
        serde_json::json!({}),
    );
    f.flag("merge_fail", "1");
    f.flag("mergeable", "CONFLICTING");
    let (code, _) = f.script("auto-merge-pr.sh", &["o/r", "feat", "aaa"]);
    assert_eq!(code, 1);
    assert!(
        f.comments().contains("<!-- susi-auto-merge --> aaa"),
        "{}",
        f.comments()
    );
    assert!(
        f.comments().contains("conflicts with main"),
        "{}",
        f.comments()
    );
    let (code, _) = f.script("auto-merge-pr.sh", &["o/r", "feat", "aaa"]);
    assert_eq!(code, 1);
    assert_eq!(
        f.comments()
            .matches("Auto-merge could not complete")
            .count(),
        1,
        "no comment spam"
    );
}

#[test]
fn the_reconciler_merges_green_prs_it_was_never_told_about() {
    let f = Fake::new(
        "rec-green",
        serde_json::json!([pr(1, "feat-a", "s1", NOW, false)]),
        serde_json::json!({"s1": green()}),
    );
    let (code, out) = f.script("reconcile-prs.sh", &["o/r"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        f.calls()
            .contains("pr merge 1 --repo o/r --merge --match-head-commit s1"),
        "{}",
        f.calls()
    );
}

/// Main moved, so every other green PR may now be mergeable: a merge made
/// outside a reconcile pass starts one, instead of leaving the queue to the
/// scheduled pass that GitHub throttles to hours.
#[test]
fn a_merge_starts_a_reconcile_pass() {
    let f = Fake::new(
        "merge-reconcile",
        serde_json::json!([pr(5, "feat", "aaa", NOW, false)]),
        serde_json::json!({}),
    );
    let (code, out) = f.script("auto-merge-pr.sh", &["o/r", "feat", "aaa"]);
    assert_eq!(code, 0, "{out}\ncalls: {}", f.calls());
    assert!(
        f.calls()
            .contains("workflow run auto-merge.yml --repo o/r --ref main"),
        "{}",
        f.calls()
    );
}

/// A reconcile pass that merges several PRs starts exactly one follow-up pass
/// (main moved under the PRs it looked at first), not one per merge; a pass
/// that merges nothing starts none, so the queue drains and the chain stops.
#[test]
fn a_reconcile_pass_that_merges_starts_one_follow_up_and_an_idle_one_none() {
    let f = Fake::new(
        "rec-follow-up",
        serde_json::json!([
            pr(1, "feat-a", "s1", NOW, false),
            pr(2, "feat-b", "s2", NOW, false)
        ]),
        serde_json::json!({"s1": green(), "s2": green()}),
    );
    // A pass that carries the gate, so it processes the whole queue.
    let (code, out) = f.script_env(
        "reconcile-prs.sh",
        &["o/r"],
        &[("SUSI_INLINE_MERGE_GATE", "1")],
    );
    assert_eq!(code, 0, "{out}\ncalls: {}", f.calls());
    for n in [1, 2] {
        assert!(
            f.calls().contains(&format!("pr merge {n} --repo o/r")),
            "{}",
            f.calls()
        );
    }
    assert_eq!(
        f.calls()
            .matches("workflow run auto-merge.yml --repo o/r --ref main")
            .count(),
        1,
        "{}",
        f.calls()
    );
    let idle = Fake::new(
        "rec-idle",
        serde_json::json!([pr(3, "feat-c", "s3", NOW, false)]),
        serde_json::json!({"s3": red()}),
    );
    idle.script("reconcile-prs.sh", &["o/r"]);
    assert!(
        !idle.calls().contains("workflow run auto-merge.yml"),
        "{}",
        idle.calls()
    );
}

/// The resync path dispatches the retest of a head it updated (a token-made
/// push starts nothing). That dispatched run must be able to merge the PR when
/// it passes, or the PR waits for the throttled scheduled pass.
#[test]
fn a_dispatched_green_run_can_merge_its_pr() {
    let workflow = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/auto-merge.yml"),
    )
    .unwrap();
    assert!(
        workflow.contains("github.event.workflow_run.event == 'workflow_dispatch'"),
        "merge-pr must accept a dispatched Test run"
    );
}

#[test]
fn a_red_pr_gets_one_comment_with_the_run_link_and_stays_open() {
    let f = Fake::new(
        "rec-red",
        serde_json::json!([pr(2, "feat-b", "s2", NOW, false)]),
        serde_json::json!({"s2": red()}),
    );
    f.script("reconcile-prs.sh", &["o/r"]);
    f.script("reconcile-prs.sh", &["o/r"]);
    assert_eq!(
        f.comments().matches("https://x/run/2").count(),
        1,
        "{}",
        f.comments()
    );
    assert!(
        !f.calls().contains("pr merge") && !f.calls().contains("pr close"),
        "{}",
        f.calls()
    );
}

#[test]
fn running_optout_and_draft_prs_are_left_alone() {
    let f = Fake::new(
        "rec-skip",
        serde_json::json!([
            pr(3, "feat-running", "s3", NOW, false),
            pr(4, "wip/experiment", "s4", NOW, false),
            pr(5, "feat-draft", "s5", NOW, true),
            pr(6, "dependabot/x", "s6", NOW, false),
        ]),
        serde_json::json!({
            "s3": {"status":"in_progress","conclusion":"","url":"u"},
            "s4": green(), "s5": green(), "s6": green()
        }),
    );
    let (code, out) = f.script("reconcile-prs.sh", &["o/r"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        !f.calls().contains("pr merge")
            && !f.calls().contains("pr close")
            && !f.calls().contains("pr comment"),
        "{}",
        f.calls()
    );
}

#[test]
fn a_pr_idle_for_a_week_and_not_green_is_closed_but_a_green_one_never_is() {
    let f = Fake::new(
        "rec-stale",
        serde_json::json!([
            pr(7, "feat-dead", "s7", OLD, false),
            pr(8, "feat-old-green", "s8", OLD, false)
        ]),
        serde_json::json!({"s7": red(), "s8": green()}),
    );
    f.script("reconcile-prs.sh", &["o/r"]);
    assert!(f.calls().contains("pr close 7 --repo o/r"), "{}", f.calls());
    assert!(f.calls().contains("pr merge 8 --repo o/r"), "{}", f.calls());
    assert!(!f.calls().contains("pr close 8"), "{}", f.calls());
}

#[test]
fn the_workflow_wires_every_path_to_the_shared_scripts() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let wf = std::fs::read_to_string(root.join(".github/workflows/auto-merge.yml")).unwrap();
    for needle in [
        "schedule:",
        "cron: '*/30 * * * *'",
        "branches-ignore: [main]",
        "workflow_dispatch:",
        "scripts/auto-merge-pr.sh",
        "scripts/reconcile-prs.sh",
        "Reconcile if this commit already went green",
    ] {
        assert!(
            wf.contains(needle),
            "auto-merge.yml must contain `{needle}`"
        );
    }
    // The merge logic must not be re-inlined: one implementation for every path.
    assert!(
        !wf.contains("gh pr merge"),
        "merge logic belongs in scripts/auto-merge-pr.sh"
    );
}

#[test]
fn a_tested_branch_missing_current_main_is_resynced_not_merged_or_failed() {
    for status in ["behind", "diverged"] {
        let f = Fake::new(
            &format!("resync-{status}"),
            serde_json::json!([pr(5, "feat", "aaa", NOW, false)]),
            serde_json::json!({}),
        );
        f.flag("comparison", status);
        let (code, out) = f.script("auto-merge-pr.sh", &["o/r", "feat", "aaa"]);
        assert_eq!(code, 0, "{out}");
        assert!(!f.calls().contains("pr merge"), "{}", f.calls());
        assert!(
            f.calls().contains("pr update-branch 5 --repo o/r"),
            "{}",
            f.calls()
        );
        // A token-made update fires no push event — the gate must be
        // dispatched or the resynced head is never tested. It is the branch
        // gate a push would have run, not the full suite.
        assert!(
            f.calls()
                .contains("workflow run test.yml --repo o/r --ref feat -f gate=branch"),
            "{}",
            f.calls()
        );
    }
}

#[test]
fn a_behind_pr_that_cannot_be_resynced_is_explained_once_and_fails() {
    for status in ["behind", "diverged"] {
        let f = Fake::new(
            &format!("resyncfail-{status}"),
            serde_json::json!([pr(5, "feat", "aaa", NOW, false)]),
            serde_json::json!({}),
        );
        f.flag("comparison", status);
        f.flag("update_fail", "1");
        let (code, _) = f.script("auto-merge-pr.sh", &["o/r", "feat", "aaa"]);
        assert_eq!(code, 1);
        assert!(
            f.comments().contains("<!-- susi-auto-merge --> aaa"),
            "{}",
            f.comments()
        );
        let (code, _) = f.script("auto-merge-pr.sh", &["o/r", "feat", "aaa"]);
        assert_eq!(code, 1);
        assert_eq!(
            f.comments()
                .matches("Auto-merge could not complete")
                .count(),
            1,
            "no comment spam"
        );
    }
}

#[test]
fn the_reconciler_resyncs_a_green_but_behind_pr_without_failing() {
    let f = Fake::new(
        "rec-behind",
        serde_json::json!([pr(9, "feat-behind", "s9", NOW, false)]),
        serde_json::json!({"s9": green()}),
    );
    f.flag("comparison", "behind");
    let (code, out) = f.script("reconcile-prs.sh", &["o/r"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        f.calls().contains("pr update-branch 9 --repo o/r"),
        "{}",
        f.calls()
    );
    assert!(
        f.calls()
            .contains("workflow run test.yml --repo o/r --ref feat-behind"),
        "{}",
        f.calls()
    );
    assert!(!f.calls().contains("pr merge"), "{}", f.calls());
}

/// Merge storms used to rebase every green-but-behind PR in one pass — each
/// retest the next merge invalidated, so a queue of n PRs burned O(n²) runs.
/// The reconciler now serializes: oldest candidate first, one rebase per pass.
#[test]
fn the_reconciler_rebases_only_the_oldest_green_candidate_per_pass() {
    let f = Fake::new(
        "rec-queue",
        serde_json::json!([
            pr(9, "feat-newer", "s9", NOW, false),
            pr(3, "feat-older", "s3", NOW, false),
        ]),
        serde_json::json!({"s9": green(), "s3": green()}),
    );
    f.flag("comparison", "behind");
    let (code, out) = f.script("reconcile-prs.sh", &["o/r"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        f.calls().contains("pr update-branch 3 --repo o/r"),
        "oldest first: {}",
        f.calls()
    );
    assert!(
        !f.calls().contains("pr update-branch 9 --repo o/r"),
        "the merge after this retest invalidates it anyway: {}",
        f.calls()
    );
}

/// A pass whose probe installed the gate merges the followers its own first
/// merge turned stale in place — merge tree, gate, merge — with no rebase.
#[test]
fn a_pass_with_the_gate_merges_the_followers_its_own_merge_made_stale() {
    let f = Fake::new(
        "rec-mix-gated",
        serde_json::json!([
            pr(12, "feat-newest", "s12", NOW, false),
            pr(3, "feat-older", "s3", NOW, false),
            pr(9, "feat-newer", "s9", NOW, false),
        ]),
        serde_json::json!({"s3": green(), "s9": green(), "s12": green()}),
    );
    // s3 is current with main; s9 and s12 are behind whatever lands first.
    f.flag("cmp-main___s3", "ahead");
    f.flag("cmp-main___s9", "behind");
    f.flag("cmp-main___s12", "behind");
    let gate = f.gate_stub();
    let (code, out) = f.script_env(
        "reconcile-prs.sh",
        &["o/r"],
        &[("SUSI_INLINE_MERGE_GATE", "1"), ("SUSI_MERGE_GATE", &gate)],
    );
    assert_eq!(code, 0, "{out}");
    let calls = f.calls();
    for n in [3, 9, 12] {
        assert!(
            calls.contains(&format!("pr merge {n} --repo o/r")),
            "{calls}"
        );
    }
    assert!(!calls.contains("update-branch"), "{calls}");
    let gatecalls = std::fs::read_to_string(f.dir.join("gatecalls")).unwrap();
    assert_eq!(gatecalls.lines().count(), 2, "{gatecalls}");
}

/// A pass whose probe found nothing stale installed no toolchain, and its
/// first merge makes every other green head stale. Gating them there failed in
/// clippy (no sccache) and told each PR its merge "fails the compile gate";
/// rebasing them costs each a retest. The pass stops at its first merge and
/// the follow-up pass it dispatches probes again and gates the rest.
#[test]
fn a_pass_without_the_gate_merges_one_pr_and_leaves_the_rest_to_the_follow_up_pass() {
    let f = Fake::new(
        "rec-mix",
        serde_json::json!([
            pr(12, "feat-newest", "s12", NOW, false),
            pr(3, "feat-older", "s3", NOW, false),
            pr(9, "feat-newer", "s9", NOW, false),
        ]),
        serde_json::json!({"s3": green(), "s9": green(), "s12": green()}),
    );
    f.flag("cmp-main___s3", "ahead");
    f.flag("cmp-main___s9", "diverged");
    f.flag("cmp-main___s12", "behind");
    let gate = f.gate_stub();
    // The pass's probe answered false: the workflow hands the script `0`.
    let (code, out) = f.script_env(
        "reconcile-prs.sh",
        &["o/r"],
        &[("SUSI_INLINE_MERGE_GATE", "0"), ("SUSI_MERGE_GATE", &gate)],
    );
    assert_eq!(code, 0, "{out}");
    let calls = f.calls();
    assert!(calls.contains("pr merge 3 --repo o/r"), "{calls}");
    for n in [9, 12] {
        assert!(!calls.contains(&format!("pr merge {n}")), "{calls}");
        assert!(!calls.contains(&format!("update-branch {n}")), "{calls}");
    }
    assert!(
        !f.dir.join("gatecalls").exists(),
        "no gate runs where no toolchain was installed"
    );
    assert_eq!(f.comments(), "", "nothing is wrong with the other PRs");
    assert_eq!(
        calls
            .matches("workflow run auto-merge.yml --repo o/r --ref main")
            .count(),
        1,
        "the follow-up pass takes the rest: {calls}"
    );
}

/// The image ships cargo, so cargo alone does not say the job installed the
/// gate's toolchain. A wrapper the job env names but never installed is an
/// environment that cannot gate — the resync path — not a red gate on the PR.
#[test]
fn a_gate_whose_compiler_wrapper_was_never_installed_is_not_a_red_gate() {
    let f = Fake::new(
        "inline-nowrapper",
        serde_json::json!([pr(5, "feat", "aaa", NOW, false)]),
        serde_json::json!({}),
    );
    f.flag("comparison", "diverged");
    let gate = f.gate_stub();
    let (code, out) = f.script_env(
        "auto-merge-pr.sh",
        &["o/r", "feat", "aaa"],
        &[
            ("SUSI_INLINE_MERGE_GATE", "1"),
            ("SUSI_MERGE_GATE", &gate),
            ("RUSTC_WRAPPER", "susi-test-no-such-sccache"),
            ("RUSTC_WORKSPACE_WRAPPER", "susi-test-no-such-sccache"),
        ],
    );
    assert_eq!(code, 0, "{out}");
    assert!(
        !f.comments().contains("fails the compile gate"),
        "{}",
        f.comments()
    );
    assert!(
        !f.dir.join("gatecalls").exists(),
        "the gate must not run without its wrapper"
    );
    assert!(
        f.calls().contains("pr update-branch 5 --repo o/r"),
        "{}",
        f.calls()
    );
}

/// The retest a resync dispatched merges its own head (test.yml `merge`).
/// When that head fell behind again it used to resync it — another
/// update-branch and another dispatched retest, whose merge job found main
/// moved again — so the PR re-dispatched itself, a branch gate per lap, for as
/// long as merges kept landing. The retest gates the stale merge in place, and
/// a head it cannot gate goes to a reconcile pass, never to another retest.
#[test]
fn a_retest_that_fell_behind_again_never_dispatches_another_retest() {
    // Gate not installed (probe false), and gate installed but unable to run.
    for (tag, gate_flag, broken) in [("off", "0", false), ("broken", "1", true)] {
        let f = Fake::new(
            &format!("retest-{tag}"),
            serde_json::json!([pr(5, "feat", "aaa", NOW, false)]),
            serde_json::json!({}),
        );
        f.flag("comparison", "diverged");
        if broken {
            f.flag("checkoutfail", "1");
        }
        let gate = f.gate_stub();
        let (code, out) = f.script_env(
            "auto-merge-pr.sh",
            &["o/r", "feat", "aaa"],
            &[
                ("SUSI_NO_RESYNC", "1"),
                ("SUSI_INLINE_MERGE_GATE", gate_flag),
                ("SUSI_MERGE_GATE", &gate),
            ],
        );
        assert_eq!(code, 0, "{tag}: {out}");
        let calls = f.calls();
        assert!(!calls.contains("update-branch"), "{tag}: {calls}");
        assert!(
            !calls.contains("workflow run test.yml"),
            "{tag}: the retest must not dispatch another retest: {calls}"
        );
        assert!(!calls.contains("pr merge"), "{tag}: {calls}");
        assert!(
            calls.contains("workflow run auto-merge.yml --repo o/r --ref main"),
            "{tag}: a reconcile pass gates the merge instead: {calls}"
        );
        assert_eq!(f.comments(), "", "{tag}: nothing is wrong with the PR");
    }
    // With the gate installed the retest merges the stale head itself.
    let f = Fake::new(
        "retest-gated",
        serde_json::json!([pr(5, "feat", "aaa", NOW, false)]),
        serde_json::json!({}),
    );
    f.flag("comparison", "behind");
    let gate = f.gate_stub();
    let (code, out) = f.script_env(
        "auto-merge-pr.sh",
        &["o/r", "feat", "aaa"],
        &[
            ("SUSI_NO_RESYNC", "1"),
            ("SUSI_INLINE_MERGE_GATE", "1"),
            ("SUSI_MERGE_GATE", &gate),
        ],
    );
    assert_eq!(code, 0, "{out}");
    let calls = f.calls();
    assert!(
        calls.contains("pr merge 5 --repo o/r --merge --match-head-commit aaa"),
        "{calls}"
    );
    assert!(!calls.contains("update-branch"), "{calls}");
}

/// The probe decides whether the merge job installs the gate's toolchain, so
/// a wrong `false` is what sends a stale head to a job that cannot gate it.
/// Its contract is that any API failure answers `true`; a failed listing or
/// compare used to answer `false`.
#[test]
fn the_merge_queue_probe_answers_true_whenever_it_cannot_tell() {
    let probe = |f: &Fake, args: &[&str]| {
        let (code, out) = f.script("merge-queue-probe.sh", args);
        assert_eq!(code, 0, "{out}");
        out.lines().last().unwrap_or_default().trim().to_string()
    };
    let f = Fake::new(
        "probe",
        serde_json::json!([
            pr(1, "feat-a", "s1", NOW, false),
            pr(2, "feat-b", "s2", NOW, false)
        ]),
        serde_json::json!({"s1": green(), "s2": {"status":"in_progress","conclusion":"","url":"u"}}),
    );
    f.flag("cmp-main___s1", "ahead");
    f.flag("cmp-main___s2", "behind");
    assert_eq!(probe(&f, &["o/r"]), "false", "nothing green is stale");
    assert_eq!(probe(&f, &["o/r", "feat-a", "s1"]), "false");
    assert_eq!(probe(&f, &["o/r", "feat-b", "s2"]), "true");
    f.flag("cmp-main___s1", "diverged");
    assert_eq!(probe(&f, &["o/r"]), "true", "a green head is stale");

    f.flag("cmp-main___s1", "ahead");
    f.flag("compare_fail", "1");
    assert_eq!(probe(&f, &["o/r"]), "true", "a failed compare");
    assert_eq!(
        probe(&f, &["o/r", "feat-a", "s1"]),
        "true",
        "a failed compare"
    );
    std::fs::remove_file(f.dir.join("compare_fail")).unwrap();

    f.flag("runlist_fail", "1");
    assert_eq!(probe(&f, &["o/r"]), "true", "a failed run listing");
    std::fs::remove_file(f.dir.join("runlist_fail")).unwrap();

    f.flag("prlist_fail", "1");
    assert_eq!(probe(&f, &["o/r"]), "true", "a failed PR listing");

    // Two green PRs in one sweep: even when both are currently ahead of main,
    // merging the first makes the second green-but-behind inside the same
    // `reconcile` pass, so the probe must answer `true` to install the gate.
    let f2 = Fake::new(
        "probe-two-green",
        serde_json::json!([
            pr(1, "feat-a", "s1", NOW, false),
            pr(2, "feat-b", "s2", NOW, false)
        ]),
        serde_json::json!({"s1": green(), "s2": green()}),
    );
    f2.flag("cmp-main___s1", "ahead");
    f2.flag("cmp-main___s2", "ahead");
    assert_eq!(
        probe(&f2, &["o/r"]),
        "true",
        "two green PRs need the gate for the second after the first merges"
    );
}

#[test]
fn integration_lock_never_serializes_branch_pr_creation() {
    let workflow = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/auto-merge.yml"),
    )
    .unwrap();
    let open = workflow
        .split("  open-pr:")
        .nth(1)
        .unwrap()
        .split("  merge-pr:")
        .next()
        .unwrap();
    assert!(!open.contains("group: auto-merge-integration"));
    assert!(!open.contains("bash \"$RUNNER_TEMP/auto-merge-pr.sh\""));
    assert!(open.contains("gh workflow run auto-merge.yml"));
    for job in ["merge-pr", "reconcile"] {
        let body = workflow.split(&format!("  {job}:")).nth(1).unwrap();
        assert!(body.contains("group: auto-merge-integration"));
        assert!(body.contains("queue: max"));
    }
}

/// The merge queue's stale-head retest is the unit of queue latency: with the
/// inline gate the serialized job proves the merge tree itself — merge-tree,
/// gate, merge — instead of an update-branch plus a whole retest run.
#[test]
fn a_behind_green_head_merges_on_the_gated_merge_tree() {
    let f = Fake::new(
        "inline-merge",
        serde_json::json!([pr(5, "feat", "aaa", NOW, false)]),
        serde_json::json!({}),
    );
    f.flag("comparison", "behind");
    let gate = f.gate_stub();
    let (code, out) = f.script_env(
        "auto-merge-pr.sh",
        &["o/r", "feat", "aaa"],
        &[("SUSI_INLINE_MERGE_GATE", "1"), ("SUSI_MERGE_GATE", &gate)],
    );
    assert_eq!(code, 0, "{out}");
    let calls = f.calls();
    assert!(
        calls.contains("pr merge 5 --repo o/r --merge --match-head-commit aaa"),
        "{calls}"
    );
    assert!(
        !calls.contains("update-branch"),
        "a gated merge must not pay for a resync and retest: {calls}"
    );
    assert!(
        calls.contains("merge-tree --write-tree origin/main aaa"),
        "{calls}"
    );
    let gatecalls = std::fs::read_to_string(f.dir.join("gatecalls")).unwrap();
    assert!(
        gatecalls.contains("base001"),
        "the gate must run against the merge's base: {gatecalls}"
    );
    assert!(
        calls.contains("checkout --quiet -f --detach mc999"),
        "the gate runs in place on the trial merge commit: {calls}"
    );
    assert!(
        calls.contains("workflow run test.yml --repo o/r --ref main"),
        "{calls}"
    );
}

/// The merge commit GitHub produces for a clean merge has exactly the gated
/// tree; the script must compare and say so.
#[test]
fn a_merged_head_reports_the_server_tree_matches_the_gated_tree() {
    let f = Fake::new(
        "inline-trees",
        serde_json::json!([pr(5, "feat", "aaa", NOW, false)]),
        serde_json::json!({}),
    );
    f.flag("comparison", "behind");
    f.flag("mergesha", "m1");
    f.flag("treefile", "tree999");
    f.flag("srvtree", "tree999");
    let gate = f.gate_stub();
    let (code, out) = f.script_env(
        "auto-merge-pr.sh",
        &["o/r", "feat", "aaa"],
        &[("SUSI_INLINE_MERGE_GATE", "1"), ("SUSI_MERGE_GATE", &gate)],
    );
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("server merge tree identical"),
        "the merge must confirm GitHub produced the gated tree: {out}"
    );
}

/// A merge that fails the compile gate is a fix-the-branch problem: explain
/// it once per sha — resyncing would only retest the same red tree.
#[test]
fn a_merge_that_fails_the_gate_is_explained_not_resynced() {
    let f = Fake::new(
        "inline-red",
        serde_json::json!([pr(5, "feat", "aaa", NOW, false)]),
        serde_json::json!({}),
    );
    f.flag("comparison", "behind");
    f.flag("gatefail", "1");
    let gate = f.gate_stub();
    let (code, _) = f.script_env(
        "auto-merge-pr.sh",
        &["o/r", "feat", "aaa"],
        &[("SUSI_INLINE_MERGE_GATE", "1"), ("SUSI_MERGE_GATE", &gate)],
    );
    assert_eq!(code, 1);
    let calls = f.calls();
    assert!(!calls.contains("pr merge"), "{calls}");
    assert!(!calls.contains("update-branch"), "{calls}");
    assert!(
        f.comments().contains("fails the compile gate"),
        "{}",
        f.comments()
    );
    let (code, _) = f.script_env(
        "auto-merge-pr.sh",
        &["o/r", "feat", "aaa"],
        &[("SUSI_INLINE_MERGE_GATE", "1"), ("SUSI_MERGE_GATE", &gate)],
    );
    assert_eq!(code, 1);
    assert_eq!(
        f.comments()
            .matches("Auto-merge could not complete")
            .count(),
        1,
        "no comment spam"
    );
}

/// merge-tree answers a conflict without a runner or a retest.
#[test]
fn a_conflicting_merge_is_explained_without_a_retest() {
    let f = Fake::new(
        "inline-conflict",
        serde_json::json!([pr(5, "feat", "aaa", NOW, false)]),
        serde_json::json!({}),
    );
    f.flag("comparison", "behind");
    f.flag("conflict", "1");
    let gate = f.gate_stub();
    let (code, _) = f.script_env(
        "auto-merge-pr.sh",
        &["o/r", "feat", "aaa"],
        &[("SUSI_INLINE_MERGE_GATE", "1"), ("SUSI_MERGE_GATE", &gate)],
    );
    assert_eq!(code, 1);
    assert!(
        f.comments().contains("conflicts with main"),
        "{}",
        f.comments()
    );
    let calls = f.calls();
    assert!(!calls.contains("update-branch"), "{calls}");
    assert!(
        !f.dir.join("gatecalls").exists(),
        "a conflicted merge never reaches the gate"
    );
}

/// The flag off — or any caller without a checkout — keeps the resync path.
#[test]
fn the_inline_gate_is_opt_in_per_environment() {
    let f = Fake::new(
        "inline-off",
        serde_json::json!([pr(5, "feat", "aaa", NOW, false)]),
        serde_json::json!({}),
    );
    f.flag("comparison", "behind");
    let gate = f.gate_stub();
    let (code, out) = f.script_env(
        "auto-merge-pr.sh",
        &["o/r", "feat", "aaa"],
        // The gate stub is reachable but the flag is not set: resync, don't gate.
        &[("SUSI_MERGE_GATE", &gate)],
    );
    assert_eq!(code, 0, "{out}");
    assert!(
        f.calls().contains("pr update-branch 5 --repo o/r"),
        "{}",
        f.calls()
    );
}

/// The merge jobs must set the flag and carry the toolchain the gate needs —
/// gated on the probe so a nothing-stale pass costs no setup.
#[test]
fn the_integration_jobs_carry_the_inline_gate() {
    let wf = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/auto-merge.yml"),
    )
    .unwrap();
    for job in ["merge-pr", "reconcile"] {
        let body = wf.split(&format!("  {job}:")).nth(1).unwrap();
        for needle in [
            "SUSI_INLINE_MERGE_GATE",
            "fetch-depth: 0",
            "merge-queue-probe.sh",
            "steps.probe.outputs.need_gate == 'true'",
            "timeout-minutes:",
        ] {
            assert!(body.contains(needle), "job `{job}` is missing `{needle}`");
        }
    }
}

/// The probe runs once, before the job does anything; the gate runs on any
/// head that is stale when the script reaches it. A job-wide
/// `SUSI_INLINE_MERGE_GATE: "1"` therefore gated heads that went stale after
/// the probe (a reconcile pass's own first merge does that to every other
/// green PR) on a runner with no sccache, and clippy's failure reached the PR
/// as "the merge with current main fails the compile gate". The switch is the
/// probe's answer, set on the step that merges.
#[test]
fn the_merge_step_gates_only_where_the_probe_installed_the_toolchain() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let probe_flag =
        "SUSI_INLINE_MERGE_GATE: ${{ steps.probe.outputs.need_gate == 'true' && '1' || '0' }}";
    let auto = std::fs::read_to_string(root.join(".github/workflows/auto-merge.yml")).unwrap();
    for (job, step) in [
        ("merge-pr", "Merge the PR for the tested commit"),
        ("reconcile", "Reconcile open PRs"),
    ] {
        let body = auto.split(&format!("  {job}:")).nth(1).unwrap();
        // The step's own lines: up to the next step or the next job.
        let step_body: String = body
            .split(&format!("- name: {step}"))
            .nth(1)
            .unwrap_or_else(|| panic!("job `{job}` has no step `{step}`"))
            .lines()
            .skip(1)
            .take_while(|l| {
                let indent = l.len() - l.trim_start().len();
                l.trim().is_empty() || (indent > 6 && !l.trim_start().starts_with("- "))
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            step_body.contains(probe_flag),
            "job `{job}`: the merge step must take the gate switch from the probe"
        );
    }
    let test = std::fs::read_to_string(root.join(".github/workflows/test.yml")).unwrap();
    let merge = test
        .split("\n  merge:\n")
        .nth(1)
        .expect("test.yml has the retest's merge job");
    // That job's merge step also runs after a failed setup step, so the
    // switch needs the last setup step to have succeeded as well.
    assert!(
        merge.contains(
            "SUSI_INLINE_MERGE_GATE: ${{ steps.probe.outputs.need_gate == 'true' \
             && steps.mold.outcome == 'success' && '1' || '0' }}"
        ),
        "test.yml `merge`: {merge}"
    );
    for wf in [&auto, &test] {
        assert!(
            !wf.contains("SUSI_INLINE_MERGE_GATE: \"1\""),
            "no job may switch the gate on regardless of what its probe installed"
        );
    }
}
