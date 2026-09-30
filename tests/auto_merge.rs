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
api\ *) cat "$D/comparison" 2>/dev/null || echo ahead ;;
"pr list") jq -r "$(jqexpr "$@")" "$D/prs.json" ;;
"pr merge") if [ -f "$D/merge_fail" ]; then echo "merge refused" >&2; exit 1; fi ;;
"pr view")
  case "$*" in
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
        Self { dir }
    }

    fn script(&self, name: &str, args: &[&str]) -> (i32, String) {
        let path = format!("{}:{}", self.dir.display(), std::env::var("PATH").unwrap());
        let out = Command::new(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("scripts")
                .join(name),
        )
        .args(args)
        .env("PATH", path)
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

    fn comments(&self) -> String {
        std::fs::read_to_string(self.dir.join("comments.txt")).unwrap()
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
        "cron: '*/15 * * * *'",
        "workflow_dispatch:",
        "scripts/auto-merge-pr.sh",
        "scripts/reconcile-prs.sh",
        "Merge now if this commit already went green",
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
fn a_tested_branch_missing_current_main_cannot_merge() {
    for status in ["behind", "diverged"] {
        let f = Fake::new(
            status,
            serde_json::json!([pr(5, "feat", "aaa", NOW, false)]),
            serde_json::json!({}),
        );
        f.flag("comparison", status);
        let (code, out) = f.script("auto-merge-pr.sh", &["o/r", "feat", "aaa"]);
        assert_eq!(code, 1, "{out}");
        assert!(!f.calls().contains("pr merge"), "{}", f.calls());
        assert!(out.contains("sync and retest"));
    }
}
