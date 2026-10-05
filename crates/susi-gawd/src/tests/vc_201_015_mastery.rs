//! Mastery verification for VC-201-015: promotion readiness gated on the
//! full repository checks actually run against the candidate.

use crate::repo_gate::{
    command_covers_full_gate, evaluate_transcript, executed_report, CheckOutcome,
};

#[test]
fn vc_201_015_mastery_fabricated_transcript_never_promotes() {
    let fabricated = "fmt: pass\nclippy: pass\ntest: pass\n";
    let report = evaluate_transcript(fabricated);
    assert!(!report.promotion_ready());
    assert!(report.blocking_reasons().contains(&"execution"));
}

#[test]
fn vc_201_015_mastery_conflicting_transcript_lines_fail_closed() {
    let t = "fmt: fail\nclippy: pass\ntest: pass\nfmt: pass\n";
    let report = evaluate_transcript(t);
    assert_eq!(report.fmt, CheckOutcome::Fail);
    assert!(!report.promotion_ready());
}

#[test]
fn vc_201_015_mastery_exact_gate_command_rejects_partial_or_wrapped_commands() {
    assert!(!command_covers_full_gate(
        "echo cargo fmt && echo clippy -d warnings && echo cargo test"
    ));
    assert!(!command_covers_full_gate(
        "cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -d warnings && cargo test --lib"
    ));
    assert!(!command_covers_full_gate(
        "cargo fmt; cargo clippy -d warnings; cargo test --workspace --locked"
    ));
}

#[test]
fn vc_201_015_mastery_executed_failures_still_block() {
    let report = executed_report(CheckOutcome::Pass, CheckOutcome::Fail, CheckOutcome::Pass);
    assert!(!report.promotion_ready());
}

#[test]
fn vc_201_015_mastery_transcripts_cannot_replace_execution() {
    for t in [
        "fmt: pass\nclippy: pass\n",
        "fmt: pass\nclippy: skipped\ntest: pass\n",
        "fmt: fail\nclippy: pass\ntest: pass\n",
    ] {
        assert!(!evaluate_transcript(t).promotion_ready());
    }
    assert!(!command_covers_full_gate("cargo test"));
}

/// The gate is on the production patch path, not a transcript a caller
/// supplies: `apply_patch_cycle` — the seam `susi patch` and the gmcp
/// `apply_patch_cycle` tool run — executes `run_repository_gate` against
/// the candidate workspace when the target is the susi repo itself, a
/// caller-supplied `test_command` cannot bypass it, and the failing
/// executed report rolls the patch back.
#[test]
fn vc_201_015_mastery_production_patch_path_executes_the_gate() {
    use crate::patch_cycle::{apply_patch_cycle, FilePatch, PatchRequest};

    let ws = std::env::temp_dir().join(format!("susi-vc015-gate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ws);
    std::fs::create_dir_all(&ws).unwrap();
    // The `.agents` identity marker plus a package literally named
    // `susi` is what marks the repo as the self-build target, so the
    // full repository gate applies.
    std::fs::create_dir_all(ws.join(".agents")).unwrap();
    std::fs::write(ws.join(".agents/identity.json"), "{}").unwrap();
    std::fs::write(
        ws.join("Cargo.toml"),
        "[package]\nname = \"susi\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(ws.join("src")).unwrap();
    std::fs::write(ws.join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
    std::fs::write(ws.join("a.txt"), "old").unwrap();

    let req = PatchRequest {
        files: vec![FilePatch {
            path: "a.txt".into(),
            old: "old".into(),
            new: "new".into(),
        }],
        // An override that would trivially "pass" — the executed gate
        // must still run and its verdict must win.
        test_command: Some("true".into()),
        auto_apply: true,
        description: "mastery".into(),
        isolate: None,
    };
    let out = apply_patch_cycle(&ws, &req, "autonomous").unwrap();
    assert!(
        out.test_stdout.contains("repository gate executed"),
        "the production path must execute the gate, not parse a transcript"
    );
    assert!(
        !out.test_passed,
        "a failing executed report blocks the patch"
    );
    assert!(out.reverted);
    assert_eq!(std::fs::read_to_string(ws.join("a.txt")).unwrap(), "old");

    let _ = std::fs::remove_dir_all(&ws);
}
