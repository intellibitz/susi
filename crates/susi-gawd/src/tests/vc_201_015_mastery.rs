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
