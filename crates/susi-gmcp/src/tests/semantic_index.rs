use super::*;

fn temp_workspace(tag: &str) -> PathBuf {
    static CTR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = CTR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("susi_semidx_{}_{}_{}", std::process::id(), tag, id));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join(".susi")).unwrap();
    dir
}

#[test]
fn indexes_memory_jsonl_and_finds_it() {
    let ws = temp_workspace("mem");
    fs::write(
        ws.join(".susi/memory.jsonl"),
        "{\"intent\":\"fix the crash in scheduler\",\"outcome\":\"patched dispatch ordering\"}\n",
    )
    .unwrap();
    let hits = SemanticIndex::search(&ws, "crash scheduler", 5).unwrap();
    assert!(!hits.is_empty(), "memory entry must be searchable");
    assert_eq!(hits[0].source, "memory");
    assert!(hits[0].snippet.contains("patched dispatch"));
    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn refresh_is_incremental() {
    let ws = temp_workspace("incr");
    fs::write(
        ws.join(".susi/memory.jsonl"),
        "{\"intent\":\"a\",\"outcome\":\"b\"}\n",
    )
    .unwrap();
    assert_eq!(SemanticIndex::refresh(&ws).unwrap(), 1);
    // Second refresh adds nothing.
    assert_eq!(SemanticIndex::refresh(&ws).unwrap(), 0);
    // Appending a line adds exactly one doc.
    use std::io::Write;
    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(ws.join(".susi/memory.jsonl"))
        .unwrap();
    writeln!(f, "{{\"intent\":\"c\",\"outcome\":\"d\"}}").unwrap();
    assert_eq!(SemanticIndex::refresh(&ws).unwrap(), 1);
    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn refresh_does_not_watermark_a_torn_jsonl_tail() {
    let ws = temp_workspace("torn_tail");
    let memory = ws.join(".susi/memory.jsonl");
    fs::write(&memory, "{\"intent\":\"unfinished").unwrap();
    assert_eq!(SemanticIndex::refresh(&ws).unwrap(), 0);

    let mut file = fs::OpenOptions::new().append(true).open(&memory).unwrap();
    file.write_all(b"\",\"outcome\":\"now complete\"}\n")
        .unwrap();
    assert_eq!(SemanticIndex::refresh(&ws).unwrap(), 1);
    let hits = SemanticIndex::search(&ws, "unfinished complete", 5).unwrap();
    assert_eq!(hits.len(), 1);
    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn indexes_workspace_files() {
    let ws = temp_workspace("files");
    fs::write(ws.join("notes.md"), "the quorum commit lives in amas.rs").unwrap();
    let hits = SemanticIndex::search(&ws, "quorum commit", 5).unwrap();
    assert!(hits.iter().any(|h| h.doc_id == "file:notes.md"));
    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn watermark_persistence_errors_are_not_reported_as_success() {
    let ws = temp_workspace("watermark_error");
    let path = SemanticIndex::watermark_path(&ws);
    fs::create_dir_all(&path).unwrap();
    assert!(SemanticIndex::save_watermark(&ws, &Watermark::default()).is_err());
    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn embedding_refresh_refuses_a_concurrent_store_writer() {
    let ws = temp_workspace("embedding_lock");
    let _lock =
        crate::susi_config::file_lock::FileLock::acquire(&ws.join(".susi"), "semantic_embeddings")
            .unwrap();
    let err = SemanticIndex::ensure_embeddings(&ws).unwrap_err();
    assert!(err.to_string().contains("busy"), "{err}");
    let _ = fs::remove_dir_all(&ws);
}

#[test]
fn index_refresh_refuses_a_concurrent_watermark_writer() {
    let ws = temp_workspace("index_lock");
    let _lock =
        crate::susi_config::file_lock::FileLock::acquire(&ws.join(".susi"), "semantic_index")
            .unwrap();
    let err = SemanticIndex::refresh(&ws).unwrap_err();
    assert!(err.to_string().contains("busy"), "{err}");
    let _ = fs::remove_dir_all(&ws);
}
