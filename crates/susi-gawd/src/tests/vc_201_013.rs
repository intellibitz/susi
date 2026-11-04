use crate::patch_fence::PatchFence;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn vc_201_013_patches_apply_only_in_isolated_workspace() {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("susi-fence-{nanos}"));
    let mut fence = PatchFence::isolate(&root, "p1").unwrap();
    fence.ensure_isolated().unwrap();
    assert!(fence.is_inside_fence(&fence.workspace.join("diff.patch")));
    assert!(!fence.is_inside_fence(&root.join("elsewhere")));
    fence.apply_in_isolation("diff.patch", b"content").unwrap();
    assert!(fence.applied);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn vc_201_013_isolate_creates_unapplied_fence_and_rejects_invalid_patch_ids() {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("susi-fence-init-{nanos}"));
    let fence = PatchFence::isolate(&root, "patch-alpha").unwrap();
    assert!(!fence.applied);
    assert_eq!(fence.patch_id, "patch-alpha");
    fence.ensure_isolated().unwrap();
    assert!(fence.workspace.is_dir());

    assert!(PatchFence::isolate(&root, "").is_err());
    assert!(PatchFence::isolate(&root, "..").is_err());
    assert!(PatchFence::isolate(&root, "a/b").is_err());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn vc_201_013_apply_in_isolation_writes_exact_bytes_inside_fenced_workspace() {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("susi-fence-write-{nanos}"));
    let mut fence = PatchFence::isolate(&root, "patch-write").unwrap();
    fence.ensure_isolated().unwrap();

    let payload = b"--- a/src/lib.rs\n+++ b/src/lib.rs\n";
    fence
        .apply_in_isolation("nested/change.patch", payload)
        .unwrap();
    let written = std::fs::read(fence.workspace.join("nested/change.patch")).unwrap();
    assert_eq!(written, payload);
    assert!(fence.applied);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn vc_201_013_apply_in_isolation_rejects_parent_dir_traversal() {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("susi-fence-escape-{nanos}"));
    let mut fence = PatchFence::isolate(&root, "patch-escape").unwrap();
    fence.ensure_isolated().unwrap();

    let err = fence
        .apply_in_isolation("../escape.patch", b"blocked")
        .expect_err("path traversal must be rejected");
    assert!(err.contains("escapes the fence"), "err={err}");
    assert!(!fence.applied);
    let _ = std::fs::remove_dir_all(&root);
}
