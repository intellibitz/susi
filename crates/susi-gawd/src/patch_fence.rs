//! Fence autonomous patches into isolated workspaces (VC-201-013).
//!
//! `isolate` only ever accepts a `patch_id` that is a single path segment
//! (no `/`, `\`, `.` or `..`), so the computed workspace can never resolve
//! outside `root/patches` and two fences can never collide by construction
//! — no canonicalization of a not-yet-created path is needed to prove it.
//! Every call also mixes in a fresh per-call nonce, so a repeated
//! `patch_id` (two concurrent experiments proposing the same patch) still
//! gets its own workspace rather than silently sharing one. `apply_in_isolation`
//! resolves `.`/`..` components lexically before writing, so a patch that
//! tries to climb out of its workspace is refused rather than silently
//! applied — that containment is also what keeps it off the installed
//! release and every sibling experiment's workspace: nothing outside
//! `self.workspace` is ever a valid write target.

use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchFence {
    pub patch_id: String,
    pub workspace: PathBuf,
    pub applied: bool,
}

/// Reject anything but a single, ordinary path segment: no separators, no
/// `.`/`..`, no embedded NUL. A sanitized id can be joined onto `root/patches`
/// with no further checks — there is no component left that could escape it.
fn sanitize_patch_id(patch_id: &str) -> Result<&str, String> {
    if patch_id.is_empty() {
        return Err("patch id must not be empty".into());
    }
    if patch_id == "." || patch_id == ".." {
        return Err(format!("patch id must not be `.` or `..`: {patch_id:?}"));
    }
    if patch_id.contains('/') || patch_id.contains('\\') || patch_id.contains('\0') {
        return Err(format!(
            "patch id must be a single path segment, not a path: {patch_id:?}"
        ));
    }
    Ok(patch_id)
}

/// A fresh value on every call, even within the same nanosecond: two
/// fences for the same `patch_id` must never land on the same workspace.
fn next_nonce() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    nanos.wrapping_add(COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// Resolve `.`/`..`/`.` components against a stack, with no filesystem
/// access — the path (or its parent) may not exist yet, so `canonicalize`
/// cannot be used here the way it is for paths that are already on disk.
pub(crate) fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                out.push(component.as_os_str());
            }
        }
    }
    out
}

impl PatchFence {
    /// Build a fence rooted at `root/patches/<sanitized patch_id>-<nonce>`.
    /// Refuses a `patch_id` carrying a path separator or a `.`/`..`
    /// component — the only way the join below could otherwise land
    /// outside `root/patches` or collide with a sibling's workspace.
    pub fn isolate(root: &Path, patch_id: &str) -> Result<Self, String> {
        let safe_id = sanitize_patch_id(patch_id)?;
        let dir_name = format!("{safe_id}-{:016x}", next_nonce());
        let workspace = root.join("patches").join(dir_name);
        Ok(Self {
            patch_id: safe_id.to_string(),
            workspace,
            applied: false,
        })
    }

    pub fn ensure_isolated(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.workspace)
    }

    /// Write `content` to `rel_path` inside the fenced workspace. The
    /// workspace must already exist, and the resolved target must stay
    /// inside it — a `rel_path` that tries to climb out (e.g. `../../x`)
    /// is refused instead of silently writing outside the fence.
    pub fn apply_in_isolation(&mut self, rel_path: &str, content: &[u8]) -> Result<(), String> {
        if !self.workspace.exists() {
            return Err("workspace missing".into());
        }
        let target = self.workspace.join(rel_path);
        if !self.is_inside_fence(&target) {
            return Err(format!(
                "patch target escapes the fence: {rel_path:?} resolves to {:?}",
                normalize_lexically(&target)
            ));
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&target, content).map_err(|e| e.to_string())?;
        self.applied = true;
        Ok(())
    }

    /// Lexical containment: resolve `.`/`..` on both sides with no
    /// filesystem access (the target may not exist yet) and compare,
    /// rather than a raw `starts_with` that a crafted `..` can defeat.
    #[must_use]
    pub fn is_inside_fence(&self, path: &Path) -> bool {
        normalize_lexically(path).starts_with(normalize_lexically(&self.workspace))
    }
}

#[cfg(test)]
mod inline_tests {
    use super::*;

    #[test]
    fn isolate_rejects_empty_dot_and_separator_patch_ids() {
        let root = std::env::temp_dir().join("susi-patch-fence-test-ids");
        assert!(PatchFence::isolate(&root, "").is_err());
        assert!(PatchFence::isolate(&root, ".").is_err());
        assert!(PatchFence::isolate(&root, "..").is_err());
        assert!(PatchFence::isolate(&root, "a/b").is_err());
        assert!(PatchFence::isolate(&root, "a\\b").is_err());
        assert!(PatchFence::isolate(&root, "a\0b").is_err());
    }

    #[test]
    fn isolate_allocates_distinct_nonced_workspaces_for_same_patch_id() {
        let root = std::env::temp_dir().join("susi-patch-fence-test-nonce");
        let first = PatchFence::isolate(&root, "patch-alpha").unwrap();
        let second = PatchFence::isolate(&root, "patch-alpha").unwrap();
        assert_eq!(first.patch_id, "patch-alpha");
        assert_ne!(first.workspace, second.workspace);
        assert!(!first.applied);
    }

    #[test]
    fn normalize_lexically_and_is_inside_fence_prevent_parent_traversal() {
        let root = std::env::temp_dir().join("susi-patch-fence-test-lexical");
        let fence = PatchFence::isolate(&root, "patch-beta").unwrap();
        let inside = fence.workspace.join("src").join("..").join("lib.rs");
        let outside = fence.workspace.join("..").join("escape.rs");
        assert!(fence.is_inside_fence(&inside));
        assert!(!fence.is_inside_fence(&outside));
    }

    #[test]
    fn apply_in_isolation_writes_inside_workspace_and_refuses_escape() {
        let root = std::env::temp_dir().join(format!("susi-patch-fence-apply-{}", next_nonce()));
        let mut fence = PatchFence::isolate(&root, "patch-gamma").unwrap();
        assert!(fence.apply_in_isolation("nested/file.txt", b"hi").is_err());

        fence.ensure_isolated().unwrap();
        fence.apply_in_isolation("nested/file.txt", b"ok").unwrap();
        assert!(fence.applied);
        assert_eq!(
            std::fs::read(fence.workspace.join("nested/file.txt")).unwrap(),
            b"ok"
        );
        assert!(fence
            .apply_in_isolation("../../outside.txt", b"no")
            .is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn normalize_lexically_strips_curdir_and_patch_fence_round_trips_serde_json() {
        let normalized = normalize_lexically(Path::new("alpha/./beta/../gamma"));
        assert_eq!(normalized, PathBuf::from("alpha").join("gamma"));

        let root = std::env::temp_dir().join("susi-patch-fence-serde");
        let fence = PatchFence::isolate(&root, "patch-delta").unwrap();
        let encoded = serde_json::to_string(&fence).unwrap();
        let decoded: PatchFence = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, fence);
    }
}
