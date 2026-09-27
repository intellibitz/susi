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

#[test]
fn concurrent_atomic_writers_never_tear_or_collide() {
    let ws = workspace("atomic");
    let path = ws.join("fresh/nested/state.json");
    let payloads: Vec<Vec<u8>> = (0..16u8).map(|i| vec![b'a' + i; 64 * 1024]).collect();
    std::thread::scope(|scope| {
        for payload in &payloads {
            let path = &path;
            scope.spawn(move || crate::json_util::atomic_write_bytes(path, payload).unwrap());
        }
    });
    let written = std::fs::read(&path).unwrap();
    assert!(
        payloads.contains(&written),
        "final file must be one whole payload"
    );
    let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name() != "state.json")
        .collect();
    assert!(leftovers.is_empty(), "staging files leaked: {leftovers:?}");
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn concurrent_secret_creation_converges_on_one_secret() {
    let ws = workspace("secret_race");
    let path = ws.join("fresh/audit.hmac.key");
    let keys: Vec<[u8; 32]> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..16)
            .map(|_| scope.spawn(|| crate::json_util::load_or_create_secret(&path).unwrap()))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert!(keys.iter().all(|k| *k == keys[0]));
    assert_eq!(std::fs::read(&path).unwrap(), keys[0].to_vec());
    assert_eq!(
        std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
        1
    );
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn wrong_length_secret_is_an_error_not_a_regeneration() {
    let ws = workspace("secret_len");
    for len in [5usize, 33, 64] {
        let path = ws.join(format!("key{len}"));
        std::fs::write(&path, vec![7u8; len]).unwrap();
        let err = crate::json_util::load_or_create_secret(&path).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(std::fs::read(&path).unwrap(), vec![7u8; len]);
    }
    let _ = std::fs::remove_dir_all(&ws);
}
