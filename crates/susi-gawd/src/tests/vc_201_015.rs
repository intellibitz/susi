use crate::repo_gate::{
    command_covers_full_gate, evaluate_transcript, required_verify_command, CheckOutcome, GateCheck,
};

#[test]
fn vc_201_015_full_pass_is_promotion_ready() {
    let report = evaluate_transcript("fmt: pass\nclippy: pass\ntest: pass\n");
    assert!(report.promotion_ready());
    assert!(report.blocking_reasons().is_empty());
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
