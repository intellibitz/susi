use crate::repo_gate::{
    command_covers_full_gate, evaluate_transcript, executed_report, required_verify_command,
    CheckOutcome, GateCheck,
};

#[test]
fn vc_201_015_legacy_three_checks_require_policy() {
    let report = executed_report(CheckOutcome::Pass, CheckOutcome::Pass, CheckOutcome::Pass);
    assert!(!report.promotion_ready());
    assert!(report.blocking_reasons().contains(&"policy"));
    let _ = GateCheck::Fmt;
}

#[test]
fn vc_201_015_missing_skipped_or_failing_block_promotion() {
    for transcript in [
        "fmt: pass\nclippy: pass\n", // test missing
        "fmt: pass\nclippy: skipped\ntest: pass\n",
        "fmt: fail\nclippy: pass\ntest: pass\n",
        "fmt: pass\nclippy: missing\ntest: pass\n",
    ] {
        let report = evaluate_transcript(transcript);
        assert!(!report.promotion_ready(), "should block: {transcript}");
        assert!(!report.blocking_reasons().is_empty());
    }
    assert!(matches!(
        evaluate_transcript("fmt: pass\nclippy: pass\ntest: fail\n").test,
        CheckOutcome::Fail
    ));
}

#[test]
fn vc_201_015_verify_command_covers_fmt_clippy_test() {
    let cmd = required_verify_command();
    assert!(command_covers_full_gate(cmd));
    assert!(!command_covers_full_gate("cargo test"));
}

#[test]
fn vc_201_015_all_four_checks_passing_achieves_promotion_ready() {
    let mut report = executed_report(CheckOutcome::Pass, CheckOutcome::Pass, CheckOutcome::Pass);
    report.policy = CheckOutcome::Pass;
    assert!(report.promotion_ready());
    assert!(report.blocking_reasons().is_empty());
}

#[test]
fn vc_201_015_transcript_parsing_cannot_manufacture_execution_and_flags_conflicts() {
    let report = evaluate_transcript("fmt: pass\nclippy: ok\ntest: passed\n");
    assert!(!report.promotion_ready());
    assert!(report.blocking_reasons().contains(&"execution"));
    assert_eq!(report.fmt, CheckOutcome::Pass);
    assert_eq!(report.clippy, CheckOutcome::Pass);
    assert_eq!(report.test, CheckOutcome::Pass);

    let conflict = evaluate_transcript("fmt: pass\nfmt: fail\nclippy: pass\ntest: pass\n");
    assert_eq!(conflict.fmt, CheckOutcome::Fail);
}
