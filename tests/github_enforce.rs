#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! `scripts/github-enforce.sh` through a fake `gh`, so the exact ruleset that
//! would reach GitHub is checked: phases, no bypass actors, relax and status.

use std::path::{Path, PathBuf};
use std::process::Command;

struct Fake {
    dir: PathBuf,
}

impl Fake {
    fn new(tag: &str, existing_id: Option<&str>) -> Self {
        let dir = std::env::temp_dir().join(format!("susi-enf-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        if let Some(id) = existing_id {
            std::fs::write(dir.join("id"), id).unwrap();
        }
        let gh = dir.join("gh");
        std::fs::write(
            &gh,
            r#"#!/usr/bin/env bash
D="$(dirname "$0")"
echo "$*" >> "$D/calls"
case "$*" in
"repo view"*) echo "owner/repo" ;;
"api repos/owner/repo/rulesets --jq"*) [ -f "$D/id" ] && cat "$D/id" || true ;;
"api repos/owner/repo/rulesets/"*"--jq"*) echo "susi-main-workflow [active] rules: deletion, non_fast_forward, pull_request" ;;
"api -X POST"* | "api -X PUT"*) cat > "$D/body" ;;
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

    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/github-enforce.sh");
        let path = format!("{}:{}", self.dir.display(), std::env::var("PATH").unwrap());
        let out = Command::new(script)
            .args(args)
            .env("PATH", path)
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.dir.join("calls")).unwrap_or_default()
    }

    fn body(&self) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(self.dir.join("body")).unwrap()).unwrap()
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn rule_types(v: &serde_json::Value) -> Vec<String> {
    v["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["type"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn dry_run_prints_phase_one_and_changes_nothing() {
    let f = Fake::new("dry", None);
    let (code, out, err) = f.run(&[]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("phase 1 (dry run"), "{out}");
    assert!(
        !f.calls().contains("-X POST") && !f.calls().contains("-X PUT"),
        "{}",
        f.calls()
    );
}

#[test]
fn phase_one_blocks_direct_pushes_but_requires_no_checks_and_has_no_bypass() {
    let f = Fake::new("p1", None);
    let (code, out, err) = f.run(&["--apply"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("created susi-main-workflow (phase 1, active)"),
        "{out}"
    );
    let b = f.body();
    assert_eq!(
        rule_types(&b),
        ["deletion", "non_fast_forward", "pull_request"]
    );
    assert_eq!(b["enforcement"], "active");
    assert_eq!(
        b["bypass_actors"],
        serde_json::json!([]),
        "no bypass: every agent pushes as the admin"
    );
    assert_eq!(
        b["conditions"]["ref_name"]["include"],
        serde_json::json!(["refs/heads/main"])
    );
    assert_eq!(
        b["rules"][2]["parameters"]["required_approving_review_count"],
        0
    );
}

#[test]
fn phase_two_adds_the_required_checks() {
    let f = Fake::new("p2", None);
    let (code, _, err) = f.run(&["--apply", "--phase", "2"]);
    assert_eq!(code, 0, "{err}");
    let b = f.body();
    assert!(rule_types(&b).contains(&"required_status_checks".to_string()));
    let contexts: Vec<&str> = b["rules"][3]["parameters"]["required_status_checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["context"].as_str().unwrap())
        .collect();
    assert!(contexts.contains(&"Workflow Compliance"), "{contexts:?}");
    assert_eq!(contexts.len(), 4);
}

#[test]
fn apply_updates_an_existing_ruleset_instead_of_duplicating() {
    let f = Fake::new("upd", Some("7"));
    let (code, out, err) = f.run(&["--apply"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("updated susi-main-workflow"), "{out}");
    assert!(
        f.calls().contains("-X PUT repos/owner/repo/rulesets/7"),
        "{}",
        f.calls()
    );
    assert!(!f.calls().contains("-X POST"), "{}", f.calls());
}

#[test]
fn relax_disables_the_ruleset_and_says_how_to_restore_it() {
    let f = Fake::new("relax", Some("7"));
    let (code, out, err) = f.run(&["--relax"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("RELAXED") && out.contains("--apply"), "{out}");
    assert_eq!(f.body()["enforcement"], "disabled");
    assert!(f.calls().contains("-X PUT repos/owner/repo/rulesets/7"));
    // Nothing to relax when nothing is enforced.
    let none = Fake::new("relax-none", None);
    let (code, _, err) = none.run(&["--relax"]);
    assert_eq!(code, 1);
    assert!(
        err.contains("no susi-main-workflow ruleset to relax"),
        "{err}"
    );
}

#[test]
fn status_reports_enforcement_or_its_absence() {
    let on = Fake::new("st-on", Some("7"));
    let (code, out, _) = on.run(&["--status"]);
    assert_eq!(code, 0);
    assert!(
        out.contains("[active]") && out.contains("pull_request"),
        "{out}"
    );
    let off = Fake::new("st-off", None);
    let (_, out, _) = off.run(&["--status"]);
    assert!(out.contains("NOT protected"), "{out}");
}

#[test]
fn bad_arguments_are_rejected_before_anything_is_sent() {
    let f = Fake::new("bad", None);
    assert_eq!(f.run(&["--apply", "--phase", "3"]).0, 2);
    assert_eq!(f.run(&["--frobnicate"]).0, 2);
    assert!(!f.calls().contains("-X"), "{}", f.calls());
}
