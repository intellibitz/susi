#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)]

//! VC-201-013: a candidate checkout gets explicit path and command scope
//! for each experiment; concurrent experiments preserve user changes and
//! cannot write the installed release or another experiment's workspace.
//!
//! The full falsification suite lives at
//! `crates/susi-gawd/src/tests/vc_201_013_mastery.rs` (exercised by
//! `cargo nextest run -p susi-gawd -E test(vc_201_013_mastery)`); this is
//! the root-package integration test the task's own bare accept command
//! (`cargo nextest run -E test(vc_201_013_mastery)`) actually discovers —
//! bare `nextest run` without `-p`/`--workspace` only discovers the root
//! `susi` package's own `tests/*.rs`, never a member crate's unit tests.

use susi_gawd::patch_fence::PatchFence;

fn tmp_root(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("susi-fence-mastery-root-{tag}-{nanos}"))
}

#[test]
fn vc_201_013_mastery() {
    let root = tmp_root("basics");

    // A well-formed patch_id nests under root/patches; apply is refused
    // until the workspace is actually isolated.
    let mut fence = PatchFence::isolate(&root, "p1").unwrap();
    assert!(fence.workspace.starts_with(root.join("patches")));
    assert!(fence.apply_in_isolation("diff.patch", b"x").is_err());
    fence.ensure_isolated().unwrap();
    fence.apply_in_isolation("diff.patch", b"x").unwrap();
    assert_eq!(
        std::fs::read(fence.workspace.join("diff.patch")).unwrap(),
        b"x"
    );

    // A patch id carrying a path separator is refused outright — it can
    // never resolve outside root/patches or onto a sibling's workspace.
    assert!(PatchFence::isolate(&root, "../escape").is_err());
    assert!(!root.join("escape").exists());
    assert!(PatchFence::isolate(&root, "x/../victim").is_err());

    // Two fences for the same patch_id never share a workspace (per-call
    // nonce), and a target that climbs out with `..` is refused, not
    // silently written outside the fence.
    let a = PatchFence::isolate(&root, "shared").unwrap();
    let b = PatchFence::isolate(&root, "shared").unwrap();
    assert_ne!(a.workspace, b.workspace);
    let mut escapee = PatchFence::isolate(&root, "p2").unwrap();
    escapee.ensure_isolated().unwrap();
    let outside = root.join("escaped.patch");
    assert!(escapee
        .apply_in_isolation("../escaped.patch", b"evil")
        .is_err());
    assert!(!outside.exists());

    // is_inside_fence resolves `.`/`..` lexically: a path that resolves
    // outside the workspace must never report inside.
    let sneaky = fence.workspace.join("..").join("outside.patch");
    assert!(!fence.is_inside_fence(&sneaky));

    let _ = std::fs::remove_dir_all(&root);
}
