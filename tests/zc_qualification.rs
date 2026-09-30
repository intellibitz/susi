//! Zero-config qualification gate: a release is qualified only if the
//! fresh-HOME journey passes with nothing but the install command and an
//! optional key, the config-debt score did not rise, and the remaining
//! limits are stated in the release notes.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

use std::path::{Path, PathBuf};

use susi_config::zc_debt_ratchet::{DebtBaseline, RatchetVerdict};
use susi_config::zc_qualification::{qualify, qualify_repo, DEBT_BASELINE_REL};

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zc-qual-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn zc_qualification_repo_passes_with_pinned_baseline() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let home = scratch("repo");
    let q = qualify_repo(root, &home).expect("baseline pinned in repo");
    assert!(q.journey_ok, "fresh-HOME journey must pass");
    assert!(
        q.qualified(),
        "release is qualified when journey passes and debt holds"
    );
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn zc_qualification_release_notes_state_every_remaining_limit() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let home = scratch("notes");
    let q = qualify_repo(root, &home).expect("baseline pinned in repo");
    let notes = q.release_notes_section();
    assert!(notes.contains("Zero-config qualification"));
    assert!(notes.contains("Remaining limits"));
    // Every live debt item must be named — limits cannot drift silently.
    for l in &q.limits {
        assert!(
            notes.contains(&l.description),
            "release notes omit limit {}",
            l.id
        );
    }
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn zc_qualification_fails_when_debt_regresses() {
    let home = scratch("regress");
    let dir = scratch("base");
    // A baseline pinned below the live count forces Regressed.
    let mut base = DebtBaseline::from_scan(&[]);
    assert!(base.max_required > 0, "test needs nonzero live debt");
    base.max_required = 0;
    base.known_ids.clear();
    let path = dir.join("baseline.json");
    base.save(&path).unwrap();
    let q = qualify(&home, &path).unwrap();
    assert!(matches!(q.ratchet, RatchetVerdict::Regressed { .. }));
    assert!(!q.qualified(), "regressed debt must block qualification");
    // ...and the failure must still be stated in the notes section.
    assert!(q.release_notes_section().contains("REGRESSED"));
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn zc_qualification_missing_baseline_fails_the_gate() {
    let home = scratch("nobase");
    let empty_repo = scratch("emptyroot");
    let r = qualify_repo(&empty_repo, &home);
    assert!(r.is_err(), "unpinned ratchet is a failed gate, not a pass");
    assert!(DEBT_BASELINE_REL.contains("config-debt-baseline"));
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&empty_repo).ok();
}
