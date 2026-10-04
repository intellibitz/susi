//! Mastery verification for VC-201-013: autonomous patches fenced into
//! isolated workspaces that cannot write the installed release, user
//! files, or another experiment's workspace.

use crate::patch_fence::PatchFence;
use std::time::{SystemTime, UNIX_EPOCH};

fn tmp_root(tag: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("susi-fence-mastery-{tag}-{nanos}"))
}

/// Fixed: `patch_id` is validated as a single path segment before it is
/// ever joined onto the fence root, so `"../escape"` — which carries a
/// separator — is refused outright instead of building a workspace that
/// resolves outside `root/patches`.
#[test]
fn vc_201_013_mastery_patch_id_escapes_the_fence_root() {
    let root = tmp_root("esc");
    let err = PatchFence::isolate(&root, "../escape")
        .expect_err("a patch id containing a path separator must be refused");
    assert!(err.contains("path segment") || err.contains('.'), "{err}");
    assert!(
        !root.join("escape").exists(),
        "no workspace should have been created outside the fence root"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Fixed: `is_inside_fence` resolves `.`/`..` components lexically before
/// comparing, so `workspace/../outside.patch` — which resolves outside the
/// workspace — now correctly reports outside instead of matching on a raw
/// `starts_with` of the unresolved path.
#[test]
fn vc_201_013_mastery_dotdot_inside_reports_inside() {
    let root = tmp_root("dd");
    let fence = PatchFence::isolate(&root, "p1").unwrap();
    let sneaky = fence.workspace.join("..").join("outside.patch");
    assert!(
        !fence.is_inside_fence(&sneaky),
        "a path that resolves OUTSIDE the workspace must not report inside"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Fixed: `apply_in_isolation` now takes the target path and the content
/// being applied, validates the resolved target stays inside the fence,
/// and actually writes it — a target that climbs out with `..` is refused
/// rather than silently accepted with nothing checked.
#[test]
fn vc_201_013_mastery_apply_checks_nothing() {
    let root = tmp_root("ap");
    let mut fence = PatchFence::isolate(&root, "p1").unwrap();
    fence.ensure_isolated().unwrap();

    // A well-behaved target is actually written inside the fence.
    fence.apply_in_isolation("diff.patch", b"hello").unwrap();
    assert!(fence.applied);
    assert_eq!(
        std::fs::read(fence.workspace.join("diff.patch")).unwrap(),
        b"hello"
    );

    // A target that climbs out of the workspace is refused, and nothing
    // is written outside the fence.
    let mut escapee = PatchFence::isolate(&root, "p2").unwrap();
    escapee.ensure_isolated().unwrap();
    let outside = root.join("escaped.patch");
    assert!(escapee
        .apply_in_isolation("../escaped.patch", b"evil")
        .is_err());
    assert!(!outside.exists(), "escape target must never be written");

    let _ = std::fs::remove_dir_all(&root);
}

/// Fixed: two fences built from the same `patch_id` now get distinct
/// workspaces (a per-call nonce is mixed in), and a `patch_id` that tries
/// to walk onto a sibling experiment's directory (`"x/../victim"`) is
/// refused outright because it carries a path separator.
#[test]
fn vc_201_013_mastery_same_id_shares_workspace() {
    let root = tmp_root("sh");
    let a = PatchFence::isolate(&root, "shared").unwrap();
    let b = PatchFence::isolate(&root, "shared").unwrap();
    assert_ne!(
        a.workspace, b.workspace,
        "two fences for the same patch_id must not share one workspace"
    );

    let hostile = PatchFence::isolate(&root, "x/../victim");
    assert!(
        hostile.is_err(),
        "a patch id with a path separator must be refused, not resolved onto a sibling"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// What holds: a well-formed patch_id nests under root/patches, the
/// workspace is created, and a missing workspace refuses apply.
#[test]
fn vc_201_013_mastery_basics_hold() {
    let root = tmp_root("ok");
    let mut fence = PatchFence::isolate(&root, "p1").unwrap();
    assert!(fence.workspace.starts_with(root.join("patches")));
    assert!(fence.apply_in_isolation("diff.patch", b"x").is_err());
    fence.ensure_isolated().unwrap();
    fence.apply_in_isolation("diff.patch", b"x").unwrap();
    let _ = std::fs::remove_dir_all(&root);
}
