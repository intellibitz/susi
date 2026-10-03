//! Mastery verification for VC-201-015: promotion readiness gated on the
//! full repository checks actually run against the candidate.

use crate::repo_gate::{command_covers_full_gate, evaluate_transcript};

/// Falsification: the gate never RUNS fmt/clippy/test — it parses a
/// caller-supplied transcript. A candidate that never ran a single check
/// can submit a fabricated 'all pass' transcript and be promotion-ready.
/// 'Run the checks against the candidate' is the claim; nothing runs.
#[test]
fn vc_201_015_mastery_fabricated_transcript_promotes() {
    let fabricated = "fmt: pass\nclippy: pass\ntest: pass\n";
    assert!(evaluate_transcript(fabricated).promotion_ready());
    // The candidate wrote that string itself — no command was executed.
}

/// Falsification: last line wins. A transcript containing a REAL failure
/// followed by a forged 'pass' line overwrites the failure — promote
/// after fail. An adversary appends, it never has to hide the failure.
#[test]
fn vc_201_015_mastery_later_line_overwrites_failure() {
    let t = "fmt: fail\nclippy: pass\ntest: pass\nfmt: pass\n";
    assert!(
        evaluate_transcript(t).promotion_ready(),
        "a recorded failure followed by a forged pass promotes"
    );
}

/// Falsification: command_covers_full_gate is substring matching.
/// 'cargo test --lib' — which skips most of the workspace — 'covers' the
/// full gate; and the command is never executed, only inspected, so a
/// string that merely mentions the checks passes.
#[test]
fn vc_201_015_mastery_substring_gate_accepts_non_gate() {
    // Never executed — only scanned for substrings.
    assert!(command_covers_full_gate(
        "echo cargo fmt && echo clippy -d warnings && echo cargo test"
    ));
    // 'cargo test --lib' skips integration/doc/workspace tests but counts.
    assert!(command_covers_full_gate(
        "cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -d warnings && cargo test --lib"
    ));
    // Even 'cargo fmt' without --all --check (which rewrites files
    // instead of verifying them) satisfies the fmt substring.
    assert!(command_covers_full_gate(
        "cargo fmt; cargo clippy -d warnings; cargo test --workspace --locked"
    ));
}

/// What holds: genuinely missing/skipped/failed outcomes block, and a
/// non-covering command string is refused.
#[test]
fn vc_201_015_mastery_blocking_holds() {
    for t in [
        "fmt: pass\nclippy: pass\n",
        "fmt: pass\nclippy: skipped\ntest: pass\n",
        "fmt: fail\nclippy: pass\ntest: pass\n",
    ] {
        assert!(!evaluate_transcript(t).promotion_ready());
    }
    assert!(!command_covers_full_gate("cargo test"));
}
