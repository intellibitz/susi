#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! `scripts/report-main-failure.sh`, against a fake `gh`.
//!
//! A red `main` is a shared signal with no owner when agents merge in parallel:
//! the failing commit is a merge commit, so nothing says whose work it carries.
//! The script points the pull request at its own failure — exactly once, even if
//! the job is retried — and stays silent when it cannot attribute the sha.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A `gh` that answers the three calls the script makes, and records comments in
/// a file so a second invocation can really see the first one.
fn fake_gh(bins: &Path) {
    let script = r#"#!/bin/sh
case "$1 $2" in
"pr list")
    printf '[{"number":%s,"mergeCommit":{"oid":"%s"}}]\n' "${TEST_PR:-0}" "${TEST_MERGED_SHA:-}"
    ;;
"pr view")
    if [ -f "$TEST_COMMENTS" ]; then
        jq -R -s 'split("<<<COMMENT>>>") | map(select(length > 0)) | {comments: map({body: .})}' "$TEST_COMMENTS"
    else
        echo '{"comments":[]}'
    fi
    ;;
"pr comment")
    shift 2
    body=""
    while [ $# -gt 0 ]; do
        case "$1" in
        --body) body="$2"; shift ;;
        esac
        shift
    done
    printf '%s\n<<<COMMENT>>>\n' "$body" >>"$TEST_COMMENTS"
    ;;
*)
    exit 1
    ;;
esac
"#;
    let path = bins.join("gh");
    std::fs::write(&path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

struct World {
    root: PathBuf,
    bins: PathBuf,
    comments: PathBuf,
}

impl World {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "susi-mainfail-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&root);
        let bins = root.join("bins");
        std::fs::create_dir_all(&bins).unwrap();
        fake_gh(&bins);
        Self {
            comments: root.join("comments.txt"),
            root,
            bins,
        }
    }

    fn report(&self, merged: &str, sha: &str, pr: &str, url: &str) -> (i32, String) {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/report-main-failure.sh");
        let out = Command::new(script)
            .args([sha, url])
            .env(
                "PATH",
                format!("{}:{}", self.bins.display(), std::env::var("PATH").unwrap()),
            )
            .env("TEST_MERGED_SHA", merged)
            .env("TEST_PR", pr)
            .env("TEST_COMMENTS", &self.comments)
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

    fn comments(&self) -> String {
        std::fs::read_to_string(&self.comments).unwrap_or_default()
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn a_red_main_is_attributed_to_its_merge_exactly_once() {
    let w = World::new("attributed");
    let sha = "0123456789abcdef0123456789abcdef01234567";

    let (code, out) = w.report(sha, sha, "42", "https://example.test/run/1");
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("commented on #42"), "{out}");
    let posted = w.comments();
    assert!(posted.contains("susi-main-failure"), "{posted}");
    assert!(
        posted.contains(sha),
        "the comment names the merge: {posted}"
    );
    assert!(
        posted.contains("https://example.test/run/1"),
        "the comment links the run: {posted}"
    );

    // A retried job (or the reconciler re-running it) must not comment again.
    let (code, out) = w.report(sha, sha, "42", "https://example.test/run/1");
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("already reported on #42"), "{out}");
    assert_eq!(
        w.comments().matches("susi-main-failure").count(),
        1,
        "exactly one report: {}",
        w.comments()
    );
}

/// A merge commit that is not the failing sha: the run failed for something
/// `gh` cannot tie to a merged pull request.
fn sha_of_another_merge() -> &'static str {
    "1111111111111111111111111111111111111111"
}

#[test]
fn an_unattributable_sha_is_silent_and_successful() {
    let w = World::new("unattributable");
    // The fake reports a different merge commit than the failing sha.
    let (code, out) = w.report(
        sha_of_another_merge(),
        "ffffffffffffffffffffffffffffffffffffffff",
        "7",
        "u",
    );
    assert_eq!(code, 0, "nothing to attribute is not an error: {out}");
    assert!(out.contains("no merged pull request records"), "{out}");
    assert_eq!(w.comments(), "", "no comment may be posted");
}
