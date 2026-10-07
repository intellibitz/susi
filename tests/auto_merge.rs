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
      spec=$(echo "${a##*compare/}" | tr '/.' '__')
      cat "$D/cmp-$spec" 2>/dev/null || cat "$D/comparison" 2>/dev/null || echo ahead ;;
    *"compare/"*)
      cat "$D/treecmp" 2>/dev/null || echo different ;;
    *"pulls/"*)
      cat "$D/prcommits" 2>/dev/null || echo '[]' ;;
  esac; done ;;
"pr list") jq -r "$(jqexpr "$@")" "$D/prs.json" ;;
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
        std::fs::write(
            &git,
            r#"#!/usr/bin/env bash
D="$(dirname "$0")"
case "$1" in
push) echo "$*" >> "$D/gitpushes"; [ -f "$D/pushfail" ] && exit 1; exit 0 ;;
fetch) echo "git $*" >> "$D/calls"; exit 0 ;;
rev-parse)
  case "$*" in
  *"^{tree}"*) cat "$D/srvtree" 2>/dev/null || echo srctree; exit 0 ;;
  *"origin/main"*) cat "$D/basesha" 2>/dev/null || echo base001; exit 0 ;;
  *) echo "${!#}"; exit 0 ;;
  esac ;;
merge-tree) echo "git $*" >> "$D/calls"; [ -f "$D/conflict" ] && exit 1; cat "$D/treefile" 2>/dev/null || echo tree999; exit 0 ;;
commit-tree) echo "git $*" >> "$D/calls"; cat "$D/mcfile" 2>/dev/null || echo mc999; exit 0 ;;
checkout) echo "git $*" >> "$D/calls"; [ -f "$D/checkoutfail" ] && exit 1; exit 0 ;;
esac
exec /usr/bin/git "$@"
"#,
        )
        .unwrap();
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
        let path = format!("{}:{}", self.dir.display(), std::env::var("PATH").unwrap());
        let out = Command::new(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("scripts")
                .join(name),
        )
        .args(args)
        .env("PATH", path)
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
    assert_eq!(code, 0, "{out}");
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
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("identical"), "{out}");
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
        "cron: '*/5 * * * *'",
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
        // dispatched or the resynced head is never tested.
        assert!(
            f.calls()
                .contains("workflow run test.yml --repo o/r --ref feat"),
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

/// Mergeable PRs still merge in the same pass — but once one lands, at most
/// one follower is rebased; the rest wait for it to land first.
#[test]
fn the_reconciler_merges_ahead_prs_then_rebases_one_follower() {
    let f = Fake::new(
        "rec-mix",
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
    let (code, out) = f.script("reconcile-prs.sh", &["o/r"]);
    assert_eq!(code, 0, "{out}");
    let calls = f.calls();
    assert!(calls.contains("pr merge 3 --repo o/r"), "{calls}");
    assert!(calls.contains("pr update-branch 9 --repo o/r"), "{calls}");
    assert!(!calls.contains("pr update-branch 12"), "{calls}");
    assert!(!calls.contains("pr merge 9"), "{calls}");
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
