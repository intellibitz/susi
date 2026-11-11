#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // Test assertions.

//! Promoting an accept-verified distilled candidate to a qualified release
//! (`docs/releases.md`, "Promoting a distilled candidate").
//!
//! `release-qualify --from-distill <name>` refuses before any gate unless the
//! candidate's committed receipt proves a complete, accept-verified candidate
//! and the release tree is that candidate plus only its version bump; the
//! qualification record then carries a `distillation` block, and push-policy
//! re-proves that block from the tagged tree before it admits the tag. The
//! built `release-qualify` and `push-policy` binaries are the ones exercised.

#[allow(dead_code)] // The test uses the verifier, not the record helpers.
#[path = "../src/release_qualification.rs"]
mod release_qualification;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

const ZERO: &str = "0000000000000000000000000000000000000000";
const NAME: &str = "v1.2.0-distilled-rc1";

fn null_config() -> &'static str {
    if cfg!(windows) {
        "NUL"
    } else {
        "/dev/null"
    }
}

/// Git with no system or global configuration and a fixed identity.
fn git_command(dir: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", null_config())
        .env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@localhost")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@localhost");
    command
}

struct Repo(PathBuf);

impl Repo {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "susi-distill-promotion-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        let r = Self(root);
        r.git(&["init", "-q", "-b", "main"]);
        r
    }
    fn git(&self, args: &[&str]) -> String {
        let out = git_command(&self.0).args(args).output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }
    fn write(&self, file: &str, text: &str) {
        let path = self.0.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    fn commit_all(&self, message: &str) -> String {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-qm", message]);
        self.git(&["rev-parse", "HEAD"])
    }
    /// The built `push-policy` (the pre-push hook's verifier) on `input`.
    fn check(&self, input: &str) -> Result<(), String> {
        use std::io::Write;
        use std::process::Stdio;
        let mut child = Command::new(env!("CARGO_BIN_EXE_push-policy"))
            .current_dir(&self.0)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", null_config())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
        }
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The files a release tree at `version` carries, the bump files among them.
fn write_tree(r: &Repo, version: &str) {
    r.write(
        "Cargo.toml",
        &format!("[package]\nname = \"susi\"\nversion = \"{version}\"\n\n[dependencies]\n"),
    );
    r.write(
        "Cargo.lock",
        &format!("[[package]]\nname = \"susi\"\nversion = \"{version}\"\n"),
    );
    r.write(
        "README.md",
        &format!("![SUSI Version](https://img.shields.io/badge/version-v{version}-blue.svg)\n"),
    );
    r.write(
        "crates/susi-gmcp/Cargo.toml",
        &format!("[package]\nname = \"susi-gmcp\"\nversion = \"{version}\"\n"),
    );
    r.write(
        ".agents/identity.json",
        &format!("{{\n  \"version\": \"{version}\"\n}}\n"),
    );
    let pin = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../rust-toolchain.toml"),
    )
    .unwrap();
    r.write("rust-toolchain.toml", &pin);
}

/// A distilled candidate's receipt, before its digest is sealed in.
fn receipt(base: &str, composed: &str, tree: &str) -> Value {
    json!({
        "schema": "susi-distillation-receipt-v1",
        "name": NAME,
        "mode": "orchestrate",
        "qualification_status": "accept_verified_candidate",
        "base_commit": base,
        "branch_ref": format!("refs/heads/release/{NAME}"),
        "snapshot_tag_ref": format!("refs/tags/susi-distill/{NAME}"),
        "accept_verification": [
            {"task_id": "T-FEATURE-1", "cmd": ["cargo", "test"], "exit_code": 0, "passed": true, "output_sha256": "a".repeat(64)}
        ],
        "included": [{"task_id": "T-FEATURE-1", "reason_code": "selected_and_composed"}],
        "satisfied_in_base": [],
        "deferred": [],
        "rejected": [],
        "git_host_identities": ["noreply@github.com"],
        "git_host_created_in_stream": [],
        "adopted_host_commits": {},
        "final_commit": composed,
        "final_tree": tree,
    })
}

fn seal(mut receipt: Value) -> Value {
    receipt["receipt_sha256"] = release_qualification::distill_receipt_digest(&receipt)
        .unwrap()
        .into();
    receipt
}

/// What the release branch holds after `local-distill` and the version bump.
struct Candidate {
    repo: Repo,
    composed: String,
    envelope: String,
}

/// Build base -> composed -> envelope (receipt edited by `edit_receipt`, after
/// sealing when `after_seal`) -> version bump 1.1.0 -> 1.2.0 (then `edit_bump`).
fn candidate(
    edit_receipt: impl FnOnce(&mut Value),
    after_seal: bool,
    edit_bump: impl FnOnce(&Repo),
) -> Candidate {
    let r = Repo::new();
    write_tree(&r, "1.1.0");
    r.write("source", "base\n");
    let base = r.commit_all("base");
    r.write("source", "base\nfeature\n");
    let composed = r.commit_all("Distill task T-FEATURE-1");
    let tree = r.git(&["rev-parse", "HEAD^{tree}"]);
    let mut value = receipt(&base, &composed, &tree);
    if after_seal {
        value = seal(value);
        edit_receipt(&mut value);
    } else {
        edit_receipt(&mut value);
        value = seal(value);
    }
    r.write(
        &format!("releases/distill/{NAME}.json"),
        &(serde_json::to_string_pretty(&value).unwrap() + "\n"),
    );
    let envelope = r.commit_all("evidence(distill): release candidate receipt");
    r.git(&[
        "update-ref",
        &format!("refs/heads/release/{NAME}"),
        &envelope,
    ]);
    r.git(&["tag", &format!("susi-distill/{NAME}"), &composed]);
    write_tree(&r, "1.2.0");
    r.write(
        ".agents/tasks/T-BUMP-1.json",
        "{\n  \"id\": \"T-BUMP-1\"\n}\n",
    );
    edit_bump(&r);
    r.commit_all("chore: release v1.2.0");
    Candidate {
        repo: r,
        composed,
        envelope,
    }
}

fn verdict(c: &Candidate) -> Result<Value, String> {
    release_qualification::verify_distilled_source(&c.repo.0, NAME, "HEAD")
}

#[test]
fn the_digest_recomputes_for_a_real_published_receipt() {
    // The receipt of `release/v0.21.56-distilled-rc13` exactly as
    // `local-distill` committed it (envelope f938a620413e, preserved as
    // `refs/tags/susi-distill/legacy-release-v0.21.56-distilled-rc13`).
    let bytes = include_bytes!("fixtures/distill-receipt-v0.21.56-distilled-rc13.json");
    let value: Value = serde_json::from_slice(bytes).unwrap();
    assert_eq!(
        release_qualification::distill_receipt_digest(&value).unwrap(),
        value["receipt_sha256"].as_str().unwrap()
    );
    let mut tampered = value.clone();
    tampered["included"][0]["task_id"] = "T-OTHER-1".into();
    assert_ne!(
        release_qualification::distill_receipt_digest(&tampered).unwrap(),
        value["receipt_sha256"].as_str().unwrap()
    );
}

#[test]
fn a_complete_accept_verified_candidate_plus_its_bump_is_a_release_source() {
    let c = candidate(|_| {}, false, |_| {});
    let block = verdict(&c).unwrap();
    assert_eq!(block["candidate"], NAME);
    assert_eq!(block["envelope_commit"], c.envelope.as_str());
    assert_eq!(block["final_commit"], c.composed.as_str());
    assert_eq!(block["included_tasks"], json!(["T-FEATURE-1"]));
    assert_eq!(block["adopted_host_commits"], json!([]));
    assert_eq!(
        block["receipt_path"],
        format!("releases/distill/{NAME}.json")
    );
    // The candidate itself (no bump yet) is a source too.
    assert!(release_qualification::verify_distilled_source(&c.repo.0, NAME, &c.envelope).is_ok());
}

/// One way the candidate or its bump can be wrong, and the refusal expected.
type Case = (&'static str, Candidate);

#[test]
fn an_incomplete_unverified_or_altered_candidate_is_refused_before_any_gate() {
    let cases: Vec<Case> = vec![
        (
            "does not re-compute",
            candidate(|r| r["final_tree"] = "0".repeat(40).into(), true, |_| {}),
        ),
        (
            "expected accept_verified_candidate",
            candidate(
                |r| r["qualification_status"] = "unqualified_candidate".into(),
                false,
                |_| {},
            ),
        ),
        (
            "rejected tasks (T-BAD-1)",
            candidate(
                |r| r["rejected"] = json!([{"task_id": "T-BAD-1"}]),
                false,
                |_| {},
            ),
        ),
        (
            "deferred tasks (T-LATER-1)",
            candidate(
                |r| r["deferred"] = json!([{"task_id": "T-LATER-1"}]),
                false,
                |_| {},
            ),
        ),
        (
            "was created by the Git host and not adopted",
            candidate(
                |r| r["git_host_created_in_stream"] = json!([{"commit": "b".repeat(40)}]),
                false,
                |_| {},
            ),
        ),
        (
            "predates host-commit accounting",
            candidate(
                |r| {
                    r.as_object_mut().unwrap().remove("adopted_host_commits");
                },
                false,
                |_| {},
            ),
        ),
        (
            "the acceptance command of T-FEATURE-1 did not pass",
            candidate(
                |r| r["accept_verification"][0]["exit_code"] = 101.into(),
                false,
                |_| {},
            ),
        ),
        (
            "has no acceptance verification",
            candidate(|r| r["accept_verification"] = json!([]), false, |_| {}),
        ),
        (
            "`name` is",
            candidate(|r| r["name"] = "v9.9.9-other".into(), false, |_| {}),
        ),
        (
            "is not the tree of the composed commit",
            candidate(|r| r["final_tree"] = "c".repeat(40).into(), false, |_| {}),
        ),
        (
            "source (M) is not part of a version bump",
            candidate(
                |_| {},
                false,
                |r| r.write("source", "base\nfeature\nmore\n"),
            ),
        ),
        (
            "Cargo.toml changes more than the version",
            candidate(
                |_| {},
                false,
                |r| {
                    let manifest = std::fs::read_to_string(r.0.join("Cargo.toml")).unwrap();
                    r.write("Cargo.toml", &format!("{manifest}evil = \"1\"\n"));
                },
            ),
        ),
        (
            "README.md changes more than the version",
            candidate(
                |_| {},
                false,
                |r| {
                    r.write(
                        "README.md",
                        "![SUSI Version](https://example.invalid/v1.2.0)\n",
                    )
                },
            ),
        ),
        (
            "more than one task record",
            candidate(
                |_| {},
                false,
                |r| r.write(".agents/tasks/T-BUMP-2.json", "{}\n"),
            ),
        ),
        (
            "(D) is not part of a version bump",
            candidate(
                |_| {},
                false,
                |r| std::fs::remove_file(r.0.join("crates/susi-gmcp/Cargo.toml")).unwrap(),
            ),
        ),
    ];
    for (needle, c) in cases {
        let error = verdict(&c).expect_err(needle);
        assert!(error.contains(needle), "expected {needle:?}, got {error:?}");
    }
}

#[test]
fn an_adopted_host_commit_is_accepted_and_named_in_the_block() {
    let hosted = "d".repeat(40);
    let c = candidate(
        |r| {
            r["git_host_created_in_stream"] = json!([{"commit": hosted, "merge": true}]);
            r["adopted_host_commits"] =
                json!({ hosted.clone(): "adopted on record by T-FEATURE-1" });
        },
        false,
        |_| {},
    );
    assert_eq!(
        verdict(&c).unwrap()["adopted_host_commits"],
        json!([hosted])
    );
}

#[test]
fn a_receipt_whose_envelope_is_not_in_the_release_history_is_refused() {
    // The receipt copied onto a line that never held the candidate: the commit
    // that adds it there is not an envelope over the composed commit.
    let c = candidate(|_| {}, false, |_| {});
    let r = &c.repo;
    let receipt = r.git(&[
        "show",
        &format!("{}:releases/distill/{NAME}.json", c.envelope),
    ]);
    r.git(&[
        "checkout",
        "-q",
        "-b",
        "elsewhere",
        &format!("{}^", c.composed),
    ]);
    r.write(&format!("releases/distill/{NAME}.json"), &(receipt + "\n"));
    r.commit_all("copied receipt");
    let error = verdict(&c).unwrap_err();
    assert!(
        error.contains("must have the composed commit"),
        "got {error:?}"
    );
    // And a tree without the receipt is no distilled source at all.
    r.git(&["checkout", "-q", &c.composed]);
    assert!(verdict(&c).unwrap_err().contains("is not in the tree"));
}

/// Run the built `release-qualify` in `dir` with `args`.
fn release_qualify(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_release-qualify"))
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", null_config())
        .output()
        .unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn verify_only_accepts_the_candidate_and_refuses_a_tampered_one_without_running_gates() {
    let c = candidate(|_| {}, false, |_| {});
    let preserved = format!("susi-distill/legacy-release-{NAME}");
    c.repo.git(&["tag", &preserved, &c.envelope]);
    let (ok, output) = release_qualify(
        &c.repo.0,
        &["--from-distill", NAME, "v1.2.0", "--verify-only"],
    );
    assert!(ok, "{output}");
    assert!(output.contains("no gate ran"), "{output}");
    assert!(output.contains(&c.envelope), "{output}");
    assert!(
        output.contains(&format!("refs/tags/susi-distill/{NAME}"))
            && output.contains(&format!("refs/heads/release/{NAME}")),
        "the local candidate refs corroborate the receipt: {output}"
    );
    assert!(
        output.contains(&format!("refs/tags/{preserved}")),
        "{output}"
    );
    assert!(!c.repo.0.join("target").exists(), "verify-only wrote logs");

    // A preserved envelope tag that names another commit contradicts it too.
    c.repo.git(&["tag", "-f", &preserved, &c.composed]);
    let (ok, output) = release_qualify(
        &c.repo.0,
        &["v1.2.0", "--from-distill", NAME, "--verify-only"],
    );
    assert!(
        !ok && output.contains("but the receipt's envelope commit"),
        "{output}"
    );
    c.repo.git(&["tag", "-f", &preserved, &c.envelope]);
    assert!(c.repo.git(&["tag", "--list", "v1.2.0"]).is_empty());

    // A snapshot tag that names another commit contradicts the receipt.
    c.repo
        .git(&["tag", "-f", &format!("susi-distill/{NAME}"), "HEAD"]);
    let (ok, output) = release_qualify(
        &c.repo.0,
        &["v1.2.0", "--from-distill", NAME, "--verify-only"],
    );
    assert!(
        !ok && output.contains("but the receipt's composed commit"),
        "{output}"
    );

    let tampered = candidate(|r| r["final_tree"] = "0".repeat(40).into(), true, |_| {});
    let (ok, output) = release_qualify(
        &tampered.repo.0,
        &["v1.2.0", "--from-distill", NAME, "--verify-only"],
    );
    assert!(!ok, "{output}");
    assert!(
        output.contains("--from-distill v1.2.0-distilled-rc1 refused")
            && output.contains("does not re-compute"),
        "{output}"
    );
    let (ok, output) = release_qualify(&c.repo.0, &["v1.2.0", "--from-distill"]);
    assert!(!ok && output.contains("usage:"), "{output}");
}

/// A qualification record for the release tree at HEAD (P), committed alone
/// as Q and tagged `v1.2.0`; returns push-policy's verdict on the tag.
fn tag_verdict(r: &Repo, edit: impl FnOnce(&mut Value)) -> Result<(), String> {
    let block = release_qualification::verify_distilled_source(&r.0, NAME, "HEAD").unwrap();
    tag_verdict_with_block(r, block, edit)
}

/// `tag_verdict` with the `distillation` block given rather than proven at HEAD.
fn tag_verdict_with_block(
    r: &Repo,
    block: Value,
    edit: impl FnOnce(&mut Value),
) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    let commit = r.git(&["rev-parse", "HEAD"]);
    let tree = r.git(&["rev-parse", "HEAD^{tree}"]);
    let lock: String = Sha256::digest(std::fs::read(r.0.join("Cargo.lock")).unwrap())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let channel = release_qualification::toolchain_channel(
        &std::fs::read_to_string(r.0.join("rust-toolchain.toml")).unwrap(),
    )
    .unwrap();
    let gate = |name: &str| json!({"name": name, "argv": ["cargo", name], "exit_code": 0});
    let mut gates: Vec<Value> = ["format", "check", "clippy", "licenses", "build", "smoke"]
        .iter()
        .map(|name| gate(name))
        .collect();
    let mut tests = gate("tests");
    tests["tests"] = json!({"binaries": 3, "passed": 40, "failed": 0, "ignored": 1});
    gates.push(tests);
    let mut record = json!({
        "schema": "susi-release-qualification/v1",
        "product": "susi",
        "tag": "v1.2.0",
        "version": "1.2.0",
        "qualified_commit": commit,
        "qualified_tree": tree,
        "cargo_lock_sha256": lock,
        "toolchain": {"channel": channel, "rustc": format!("rustc {channel}")},
        "host": "x86_64-unknown-linux-gnu",
        "gates": gates,
        "assets": [{"file": "susi", "bytes": 1, "sha256": "a".repeat(64), "version_output": "susi 1.2.0"}],
        "source": "distill",
        "distillation": block,
    });
    edit(&mut record);
    r.write(
        "releases/qualification/v1.2.0.json",
        &serde_json::to_string_pretty(&record).unwrap(),
    );
    r.commit_all("chore: release v1.2.0 qualification record");
    r.git(&["tag", "-a", "v1.2.0", "-m", "release"]);
    let tag = r.git(&["rev-parse", "refs/tags/v1.2.0"]);
    let verdict = r.check(&format!("refs/tags/v1.2.0 {tag} refs/tags/v1.2.0 {ZERO}"));
    r.git(&["tag", "-d", "v1.2.0"]);
    r.git(&["reset", "-q", "--hard", "HEAD^"]);
    verdict
}

#[test]
fn push_policy_admits_a_distilled_release_tag_only_with_the_proven_block() {
    let c = candidate(|_| {}, false, |_| {});
    let r = &c.repo;
    assert_eq!(tag_verdict(r, |_| {}), Ok(()));
    // A snapshot release says so; older records without `source` stay valid.
    assert_eq!(
        tag_verdict(r, |rec| {
            rec["source"] = "snapshot".into();
            rec.as_object_mut().unwrap().remove("distillation");
        }),
        Ok(())
    );
    assert_eq!(
        tag_verdict(r, |rec| {
            let map = rec.as_object_mut().unwrap();
            map.remove("source");
            map.remove("distillation");
        }),
        Ok(())
    );
    type Edit = (&'static str, Box<dyn FnOnce(&mut Value)>);
    let refused: Vec<Edit> = vec![
        (
            "does not match",
            Box::new(|rec| rec["distillation"]["envelope_commit"] = "e".repeat(40).into()),
        ),
        (
            "does not match",
            Box::new(|rec| rec["distillation"]["included_tasks"] = json!(["T-OTHER-1"])),
        ),
        (
            "does not match",
            Box::new(|rec| rec["distillation"]["receipt_sha256"] = "f".repeat(64).into()),
        ),
        (
            "has no `distillation`",
            Box::new(|rec| {
                rec.as_object_mut().unwrap().remove("distillation");
            }),
        ),
        (
            "its source is not distill",
            Box::new(|rec| rec["source"] = "snapshot".into()),
        ),
        (
            "must be snapshot or distill",
            Box::new(|rec| {
                rec["source"] = "hosted".into();
                rec.as_object_mut().unwrap().remove("distillation");
            }),
        ),
        (
            "is not a distilled candidate name",
            Box::new(|rec| rec["distillation"]["candidate"] = "../x".into()),
        ),
    ];
    for (needle, edit) in refused {
        let error = tag_verdict(r, edit).expect_err(needle);
        assert!(error.contains(needle), "expected {needle:?}, got {error:?}");
    }
}

#[test]
fn push_policy_refuses_a_distilled_release_whose_tree_moved_past_the_bump() {
    // Qualified from the candidate, then the release tree took more than the
    // bump: the block can no longer be proven from the tagged tree.
    let c = candidate(|_| {}, false, |_| {});
    let r = &c.repo;
    let block = release_qualification::verify_distilled_source(&r.0, NAME, "HEAD").unwrap();
    r.write("source", "base\nfeature\nunreviewed\n");
    r.commit_all("unreviewed change");
    let error = tag_verdict_with_block(r, block, |_| {}).unwrap_err();
    assert!(
        error.contains("source (M) is not part of a version bump"),
        "got {error:?}"
    );
}

/// A release branch re-records the GMCP vector evidence on the candidate
/// (`docs/release-distillation.md`): `*.jsonl` files directly in the evidence
/// directory may be added, changed or removed after the envelope, and the
/// block lists them; the tag check proves that list from the tree. Anything
/// else in that directory is still refused.
#[test]
fn a_gmcp_evidence_rerecord_after_the_candidate_is_admitted_and_named() {
    let plain = candidate(|_| {}, false, |_| {});
    assert!(verdict(&plain)
        .unwrap()
        .get("gmcp_evidence_rerecorded")
        .is_none());

    let c = candidate(
        |_| {},
        false,
        |r| {
            r.write(
                ".agents/gmcp/evidence/VC-203-009.jsonl",
                "{\"tested_commit\":\"b\"}\n",
            );
            r.write(
                ".agents/gmcp/evidence/VC-203-001.jsonl",
                "{\"tested_commit\":\"a\"}\n",
            );
        },
    );
    let block = verdict(&c).unwrap();
    assert_eq!(
        block["gmcp_evidence_rerecorded"],
        json!([
            ".agents/gmcp/evidence/VC-203-001.jsonl",
            ".agents/gmcp/evidence/VC-203-009.jsonl"
        ])
    );
    assert_eq!(tag_verdict(&c.repo, |_| {}), Ok(()));
    // The list is proven from the tree, not taken from the record.
    let forged = tag_verdict(&c.repo, |rec| {
        rec["distillation"]["gmcp_evidence_rerecorded"] =
            json!([".agents/gmcp/evidence/VC-203-001.jsonl"]);
    });
    assert!(forged.is_err(), "{forged:?}");

    for (file, needle) in [
        (
            ".agents/gmcp/evidence/README.md",
            "README.md (A) is not part of a version bump",
        ),
        (
            ".agents/gmcp/evidence/nested/VC-203-001.jsonl",
            "nested/VC-203-001.jsonl (A) is not part of a version bump",
        ),
        (
            ".agents/gmcp/VC-203-001.jsonl",
            "gmcp/VC-203-001.jsonl (A) is not part of",
        ),
    ] {
        let c = candidate(|_| {}, false, |r| r.write(file, "{}\n"));
        let error = verdict(&c).expect_err(file);
        assert!(error.contains(needle), "expected {needle:?}, got {error:?}");
    }
}

#[test]
fn release_qualify_enforces_harvest_audit_before_gates_and_verify_tag_checks_it() {
    // 1. Snapshot tree with an unattributed `Harvest:` trailer is refused by
    // `release-qualify --verify-only` before any build/test gate runs.
    let bad_trailer = Repo::new();
    write_tree(&bad_trailer, "1.2.0");
    bad_trailer.write("src/port.rs", "pub fn demo() -> usize { 1 }\n");
    bad_trailer.commit_all("port demo\n\nHarvest: H-FIXTURE-99\nTask: T-FIXTURE-1");
    let (ok, output) = release_qualify(&bad_trailer.0, &["v1.2.0", "--verify-only"]);
    assert!(
        !ok && output.contains("harvest audit refused") && output.contains("H-FIXTURE-99"),
        "expected harvest audit refusal, got: {output}"
    );

    // 2. Qualified tree carrying `.agents/harvest/*.json` requires a passing
    // `harvest_audit` in the qualification record when checked by `push-policy`.
    let harvested = candidate(|_| {}, false, |_| {});
    // Write a manifest in the base/candidate history by testing `tag_verdict`
    // against a snapshot release tree that carries `.agents/harvest/H-FIX-1.json`.
    let snap = Repo::new();
    write_tree(&snap, "1.2.0");
    snap.write(
        ".agents/harvest/H-FIX-1.json",
        "{\"schema\":\"susi-harvest-manifest-v1\"}\n",
    );
    snap.commit_all("snapshot with harvest manifest");
    let missing_audit = tag_verdict_with_block(&snap, Value::Null, |rec| {
        rec["source"] = "snapshot".into();
        rec.as_object_mut().unwrap().remove("distillation");
    })
    .unwrap_err();
    assert!(
        missing_audit.contains("no `harvest_audit`"),
        "expected missing harvest_audit error, got: {missing_audit}"
    );

    // 3. A forged `harvest_audit` with `ok: false` or non-empty `violations` is refused.
    let failed_audit = tag_verdict(&harvested.repo, |rec| {
        rec["harvest_audit"] = json!({"ok": false, "violations": ["missing NOTICE"]});
    })
    .unwrap_err();
    assert!(
        failed_audit.contains("harvest_audit"),
        "expected failed harvest_audit error, got: {failed_audit}"
    );

    // 4. Clean `harvest_audit` (`ok: true`, empty `violations`) is admitted.
    assert_eq!(
        tag_verdict(&harvested.repo, |rec| {
            rec["harvest_audit"] = json!({
                "ok": true,
                "manifests": 0,
                "files": 0,
                "harvest_trailer_commits": 0,
                "violations": []
            });
        }),
        Ok(())
    );
}
