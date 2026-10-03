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

/// Falsification: patch_id is interpolated into the workspace path with
/// no sanitization. A patch_id of "../escape" builds a workspace that
/// resolves outside root/patches entirely — the fence itself is the
/// escape hatch. Component-normalizing the built path exposes it.
#[test]
fn vc_201_013_mastery_patch_id_escapes_the_fence_root() {
    let root = tmp_root("esc");
    let fence = PatchFence::isolate(&root, "../escape");
    // ensure_isolated happily CREATES the escaped directory.
    fence.ensure_isolated().unwrap();
    let resolved = fence.workspace.canonicalize().unwrap();
    let patches = root.join("patches").canonicalize().unwrap();
    // The 'workspace' is root/patches/../escape = root/escape — outside
    // the patches directory the fence is supposed to confine to.
    assert!(
        !resolved.starts_with(&patches),
        "workspace {:?} escaped the fence root",
        resolved
    );
    // It was actually created on disk outside the fence root.
    assert!(resolved.is_dir());
    let _ = std::fs::remove_dir_all(&root);
}

/// Falsification: is_inside_fence is lexical starts_with. The path
/// workspace/../outside resolves outside the workspace but reports
/// 'inside' — a patch writer walking a crafted path passes the check.
#[test]
fn vc_201_013_mastery_dotdot_inside_reports_inside() {
    let root = tmp_root("dd");
    let fence = PatchFence::isolate(&root, "p1");
    let sneaky = fence.workspace.join("..").join("outside.patch");
    assert!(
        fence.is_inside_fence(&sneaky),
        "lexical starts_with says a path resolving OUTSIDE is inside"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Falsification: apply_in_isolation applies nothing — it checks the
/// directory exists and sets applied=true. There is no patch content, no
/// target path validation, and nothing referencing the installed
/// release, so 'cannot write the installed release' is untestable: the
/// fence never inspects where a patch writes.
#[test]
fn vc_201_013_mastery_apply_checks_nothing() {
    let root = tmp_root("ap");
    let mut fence = PatchFence::isolate(&root, "p1");
    fence.ensure_isolated().unwrap();
    fence.apply_in_isolation().unwrap();
    assert!(fence.applied);
    // workspace is empty — nothing was applied anywhere, inside or out.
    assert!(std::fs::read_dir(&fence.workspace)
        .unwrap()
        .next()
        .is_none());
    let _ = std::fs::remove_dir_all(&root);
}

/// Falsification: 'concurrent experiments cannot write another
/// experiment's workspace' — but two experiments sharing a patch_id get
/// the same workspace path, and nothing prevents one fence's apply from
/// touching a sibling workspace it can compute trivially.
#[test]
fn vc_201_013_mastery_same_id_shares_workspace() {
    let root = tmp_root("sh");
    let a = PatchFence::isolate(&root, "shared");
    let b = PatchFence::isolate(&root, "shared");
    assert_eq!(a.workspace, b.workspace, "two experiments, one workspace");
    // And patch_id "x/../victim" resolves onto the victim experiment's
    // workspace outright — an experiment can point its fence at a
    // sibling's directory.
    let hostile = PatchFence::isolate(&root, "x/../victim");
    let victim = PatchFence::isolate(&root, "victim");
    hostile.ensure_isolated().unwrap();
    victim.ensure_isolated().unwrap();
    assert_eq!(
        hostile.workspace.canonicalize().unwrap(),
        victim.workspace.canonicalize().unwrap(),
        "hostile id resolves into victim's workspace"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// What holds: a well-formed patch_id nests under root/patches, the
/// workspace is created, and a missing workspace refuses apply.
#[test]
fn vc_201_013_mastery_basics_hold() {
    let root = tmp_root("ok");
    let mut fence = PatchFence::isolate(&root, "p1");
    assert!(fence.workspace.starts_with(root.join("patches")));
    assert!(fence.apply_in_isolation().is_err());
    fence.ensure_isolated().unwrap();
    fence.apply_in_isolation().unwrap();
    let _ = std::fs::remove_dir_all(&root);
}
