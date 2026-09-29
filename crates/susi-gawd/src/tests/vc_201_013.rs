use crate::patch_fence::PatchFence;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn vc_201_013_patches_apply_only_in_isolated_workspace() {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("susi-fence-{nanos}"));
    let mut fence = PatchFence::isolate(&root, "p1");
    fence.ensure_isolated().unwrap();
    assert!(fence.is_inside_fence(&fence.workspace.join("diff.patch")));
    assert!(!fence.is_inside_fence(&root.join("elsewhere")));
    fence.apply_in_isolation().unwrap();
    assert!(fence.applied);
    let _ = std::fs::remove_dir_all(&root);
}
