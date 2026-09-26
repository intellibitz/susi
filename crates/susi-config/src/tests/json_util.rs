//! Tests for `json_util` — kept out of the canonical source so crates that
//! `#[path]`-mount it never compile or run them.

use crate::json_util::confined_workspace_join;

fn workspace(tag: &str) -> std::path::PathBuf {
    let ws = std::env::temp_dir().join(format!("susi_confine_{}_{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ws);
    std::fs::create_dir_all(&ws).unwrap();
    ws.canonicalize().unwrap()
}

#[test]
fn missing_parent_dirs_are_preserved() {
    let ws = workspace("nested");
    let p = confined_workspace_join(&ws, "newdir/deeper/file.rs").unwrap();
    assert_eq!(p, ws.join("newdir/deeper/file.rs"));
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn rejects_absolute_parent_and_empty_paths() {
    let ws = workspace("reject");
    assert!(confined_workspace_join(&ws, "/etc/passwd").is_err());
    assert!(confined_workspace_join(&ws, "a/../../etc").is_err());
    assert!(confined_workspace_join(&ws, "").is_err());
    let _ = std::fs::remove_dir_all(&ws);
}

#[cfg(unix)]
#[test]
fn symlinked_leaf_or_dir_cannot_escape() {
    let ws = workspace("symlink");
    let outside = workspace("symlink_outside");
    std::fs::write(outside.join("secret"), "x").unwrap();
    std::os::unix::fs::symlink(outside.join("secret"), ws.join("link")).unwrap();
    std::os::unix::fs::symlink(&outside, ws.join("dirlink")).unwrap();
    assert!(confined_workspace_join(&ws, "link").is_err());
    assert!(confined_workspace_join(&ws, "dirlink/new.txt").is_err());
    // A symlink that stays inside the workspace is fine.
    std::fs::write(ws.join("real"), "y").unwrap();
    std::os::unix::fs::symlink(ws.join("real"), ws.join("inner")).unwrap();
    assert_eq!(
        confined_workspace_join(&ws, "inner").unwrap(),
        ws.join("real")
    );
    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::remove_dir_all(&outside);
}
