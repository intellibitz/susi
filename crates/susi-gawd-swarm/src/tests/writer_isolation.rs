use super::*;

fn temp_root(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "wiso-{tag}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|t| t.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

use std::time::{SystemTime, UNIX_EPOCH};

fn base_ws(tag: &str) -> PathBuf {
    let base = temp_root(tag);
    std::fs::write(base.join("shared.txt"), "base\n").unwrap();
    std::fs::create_dir_all(base.join("src")).unwrap();
    std::fs::write(base.join("src/main.rs"), "fn main() {}\n").unwrap();
    base
}

/// Disjoint parallel writes: every worker's change lands in the shared
/// workspace after its fold.
#[test]
fn writer_isolation_disjoint_writes_all_fold() {
    let base = base_ws("disjoint");
    let iso = WriterIsolation::new(&base).unwrap();
    let a = iso.scope("worker-a").unwrap();
    let b = iso.scope("worker-b").unwrap();
    std::fs::write(a.dir.join("a.txt"), "from a\n").unwrap();
    std::fs::write(b.dir.join("b.txt"), "from b\n").unwrap();
    std::fs::write(b.dir.join("src/b.rs"), "pub fn b() {}\n").unwrap();
    assert_eq!(
        iso.fold(&a).unwrap(),
        FoldOutcome::Clean {
            wrote: vec!["a.txt".to_string()],
            deleted: vec![]
        }
    );
    let out = iso.fold(&b).unwrap();
    match out {
        FoldOutcome::Clean { wrote, .. } => {
            assert!(wrote.contains(&"b.txt".to_string()));
            assert!(wrote.contains(&"src/b.rs".to_string()));
        }
        FoldOutcome::Conflict { .. } => panic!("disjoint writes must not conflict"),
    }
    assert_eq!(
        std::fs::read_to_string(base.join("a.txt")).unwrap(),
        "from a\n"
    );
    assert_eq!(
        std::fs::read_to_string(base.join("b.txt")).unwrap(),
        "from b\n"
    );
    assert_eq!(
        std::fs::read_to_string(base.join("src/b.rs")).unwrap(),
        "pub fn b() {}\n"
    );
    iso.cleanup(&a);
    iso.cleanup(&b);
    std::fs::remove_dir_all(&base).unwrap();
}

/// A worker's write is invisible in the base until folded — no
/// concurrent reader ever sees a half-written file.
#[test]
fn writer_isolation_writes_stay_private_until_fold() {
    let base = base_ws("private");
    let iso = WriterIsolation::new(&base).unwrap();
    let scope = iso.scope("worker-x").unwrap();
    std::fs::write(scope.dir.join("draft.txt"), "wip\n").unwrap();
    // While the worker runs, the shared base is untouched.
    assert!(!base.join("draft.txt").exists());
    assert!(matches!(
        iso.fold(&scope).unwrap(),
        FoldOutcome::Clean { .. }
    ));
    assert_eq!(
        std::fs::read_to_string(base.join("draft.txt")).unwrap(),
        "wip\n"
    );
    std::fs::remove_dir_all(&base).unwrap();
}

/// Two workers editing the same path: first fold wins, the second is a
/// reviewable conflict — never a silent overwrite.
#[test]
fn writer_isolation_same_path_conflict_is_reviewed() {
    let base = base_ws("conflict");
    let iso = WriterIsolation::new(&base).unwrap();
    let a = iso.scope("worker-a").unwrap();
    let b = iso.scope("worker-b").unwrap();
    std::fs::write(a.dir.join("shared.txt"), "version a\n").unwrap();
    std::fs::write(b.dir.join("shared.txt"), "version b\n").unwrap();
    std::fs::write(b.dir.join("ok.txt"), "unrelated\n").unwrap();
    assert!(matches!(iso.fold(&a).unwrap(), FoldOutcome::Clean { .. }));
    match iso.fold(&b).unwrap() {
        FoldOutcome::Conflict { paths } => {
            assert!(paths.contains("shared.txt"));
        }
        FoldOutcome::Clean { .. } => panic!("diverged same-path write must conflict"),
    }
    // The conflicting path was NOT overwritten; unrelated writes landed.
    assert_eq!(
        std::fs::read_to_string(base.join("shared.txt")).unwrap(),
        "version a\n"
    );
    assert_eq!(
        std::fs::read_to_string(base.join("ok.txt")).unwrap(),
        "unrelated\n"
    );
    std::fs::remove_dir_all(&base).unwrap();
}

/// The scope is synced with the workspace at creation — a worker sees
/// existing content, including uncommitted files.
#[test]
fn writer_isolation_scope_is_synced_snapshot() {
    let base = base_ws("synced");
    let iso = WriterIsolation::new(&base).unwrap();
    let scope = iso.scope("worker-s").unwrap();
    assert_eq!(
        std::fs::read_to_string(scope.dir.join("shared.txt")).unwrap(),
        "base\n"
    );
    assert_eq!(
        std::fs::read_to_string(scope.dir.join("src/main.rs")).unwrap(),
        "fn main() {}\n"
    );
    std::fs::remove_dir_all(&base).unwrap();
}

/// Deleting a file in the scope deletes it in the base — unless the
/// base version drifted, which conflicts instead of losing data.
#[test]
fn writer_isolation_delete_folds_and_drift_conflicts() {
    let base = base_ws("delete");
    let iso = WriterIsolation::new(&base).unwrap();
    let a = iso.scope("worker-a").unwrap();
    std::fs::remove_file(a.dir.join("src/main.rs")).unwrap();
    assert!(matches!(iso.fold(&a).unwrap(), FoldOutcome::Clean { .. }));
    assert!(!base.join("src/main.rs").exists());

    // Drift under an in-flight scope conflicts rather than deleting.
    let b = iso.scope("worker-b").unwrap();
    std::fs::write(base.join("shared.txt"), "drifted\n").unwrap();
    std::fs::remove_file(b.dir.join("shared.txt")).unwrap();
    match iso.fold(&b).unwrap() {
        FoldOutcome::Conflict { paths } => assert!(paths.contains("shared.txt")),
        FoldOutcome::Clean { .. } => panic!("delete of drifted file must conflict"),
    }
    assert_eq!(
        std::fs::read_to_string(base.join("shared.txt")).unwrap(),
        "drifted\n"
    );
    std::fs::remove_dir_all(&base).unwrap();
}

/// Two workers creating the same path with identical content is a
/// clean no-op, not a conflict (idempotent convergent writes).
#[test]
fn writer_isolation_identical_writes_converge() {
    let base = base_ws("converge");
    let iso = WriterIsolation::new(&base).unwrap();
    let a = iso.scope("worker-a").unwrap();
    let b = iso.scope("worker-b").unwrap();
    std::fs::write(a.dir.join("new.txt"), "same\n").unwrap();
    std::fs::write(b.dir.join("new.txt"), "same\n").unwrap();
    assert!(matches!(iso.fold(&a).unwrap(), FoldOutcome::Clean { .. }));
    assert!(matches!(iso.fold(&b).unwrap(), FoldOutcome::Clean { .. }));
    assert_eq!(
        std::fs::read_to_string(base.join("new.txt")).unwrap(),
        "same\n"
    );
    std::fs::remove_dir_all(&base).unwrap();
}

/// Many workers writing concurrently into their own scopes never
/// collide — the shared base sees each batch only via fold.
#[test]
fn writer_isolation_concurrent_writers_never_share_a_directory() {
    let base = base_ws("parallel");
    let iso = WriterIsolation::new(&base).unwrap();
    let scopes: Vec<WorkerScope> = (0..4)
        .map(|i| iso.scope(&format!("worker-{i}")).unwrap())
        .collect();
    let dirs: Vec<PathBuf> = scopes.iter().map(|s| s.dir.clone()).collect();
    assert!(dirs.iter().all(|d| d != &base));
    let handles: Vec<_> = dirs
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let d = d.clone();
            std::thread::spawn(move || {
                for n in 0..8 {
                    std::fs::write(d.join(format!("w{i}-{n}.txt")), format!("{i}:{n}\n")).unwrap();
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    // Not one worker file leaked into the base mid-flight.
    assert!(std::fs::read_dir(&base).unwrap().all(|e| !e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with("w")));
    for scope in &scopes {
        assert!(matches!(
            iso.fold(scope).unwrap(),
            FoldOutcome::Clean { .. }
        ));
        iso.cleanup(scope);
    }
    for i in 0..4 {
        for n in 0..8 {
            assert_eq!(
                std::fs::read_to_string(base.join(format!("w{i}-{n}.txt"))).unwrap(),
                format!("{i}:{n}\n")
            );
        }
    }
    std::fs::remove_dir_all(&base).unwrap();
}

/// `.git`, `target` and the scopes' own subtree never enter a snapshot.
#[test]
fn writer_isolation_excludes_vcs_build_and_self() {
    let base = base_ws("excl");
    std::fs::create_dir_all(base.join(".git/objects")).unwrap();
    std::fs::write(base.join(".git/HEAD"), "ref: main\n").unwrap();
    std::fs::create_dir_all(base.join("target/debug")).unwrap();
    std::fs::write(base.join("target/debug/app"), "bin\n").unwrap();
    let iso = WriterIsolation::new(&base).unwrap();
    let scope = iso.scope("worker-e").unwrap();
    assert!(!scope.dir.join(".git").exists());
    assert!(!scope.dir.join("target").exists());
    assert!(!scope.dir.join(".susi/dag-scopes").exists());
    // `.susi` worker-visible content still syncs.
    std::fs::create_dir_all(base.join(".susi/evidence")).unwrap();
    std::fs::write(base.join(".susi/evidence/e1.json"), "{}\n").unwrap();
    let scope2 = iso.scope("worker-f").unwrap();
    assert!(scope2.dir.join(".susi/evidence/e1.json").exists());
    std::fs::remove_dir_all(&base).unwrap();
}
