//! Ambient FS → ContextGraph + SemanticIndex synchronization.
//!
//! Background poller watches workspace text files; on mtime change it records
//! an observation in the context graph and refreshes the semantic index so
//! other agents inherit knowledge without explicit file hand-offs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use susi_core::context_graph::ContextGraph;

static STARTED: OnceLock<AtomicBool> = OnceLock::new();

const FILE_EXTS: &[&str] = &["rs", "md", "txt", "toml", "json", "py", "ts"];
const MAX_FILE_BYTES: u64 = 256 * 1024;

/// Start ambient indexing for `workspace` (idempotent process-wide).
pub fn start_ambient_indexer(workspace: &Path) {
    let flag = STARTED.get_or_init(|| AtomicBool::new(false));
    if flag.swap(true, Ordering::SeqCst) {
        return;
    }
    let ws = workspace.to_path_buf();
    std::thread::Builder::new()
        .name("susi-ambient".into())
        .spawn(move || ambient_loop(ws))
        .ok();
}

/// One-shot ambient pulse (also used by CLI).
pub fn pulse(workspace: &Path) -> AmbientPulseReport {
    let changed = scan_and_record(workspace);
    let indexed = susi_gmcp::tools::semantic_index::SemanticIndex::refresh(workspace).unwrap_or(0);
    AmbientPulseReport {
        files_changed: changed,
        docs_indexed: indexed,
    }
}

#[derive(Debug, Clone)]
pub struct AmbientPulseReport {
    pub files_changed: usize,
    pub docs_indexed: usize,
}

impl AmbientPulseReport {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "files_changed": self.files_changed,
            "docs_indexed": self.docs_indexed,
        })
    }
}

fn ambient_loop(workspace: PathBuf) {
    let mut last: HashMap<String, u64> = HashMap::new();
    loop {
        let changed = scan_with_state(&workspace, &mut last);
        if changed > 0 {
            let _ = susi_gmcp::tools::semantic_index::SemanticIndex::refresh(&workspace);
        }
        std::thread::sleep(Duration::from_secs(15));
    }
}

fn scan_and_record(workspace: &Path) -> usize {
    let mut state = HashMap::new();
    scan_with_state(workspace, &mut state)
}

fn scan_with_state(workspace: &Path, state: &mut HashMap<String, u64>) -> usize {
    let mut changed = 0usize;
    let mut stack = vec![workspace.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                if name != ".git" && name != "target" && name != ".susi" && name != "node_modules" {
                    stack.push(path);
                }
                continue;
            }
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();
            if !FILE_EXTS.contains(&ext.as_str()) {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.len() > MAX_FILE_BYTES {
                continue;
            }
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let rel = path
                .strip_prefix(workspace)
                .unwrap_or(&path)
                .display()
                .to_string();
            if state.get(&rel) == Some(&mtime) {
                continue;
            }
            state.insert(rel.clone(), mtime);
            changed += 1;
            let preview: String = std::fs::read_to_string(&path)
                .unwrap_or_default()
                .chars()
                .take(240)
                .collect();
            ContextGraph::global().record_external_context(
                "ambient_fs",
                &format!("file changed: {rel}"),
                &serde_json::json!({
                    "path": rel,
                    "mtime": mtime,
                    "preview": preview,
                }),
                Some(workspace),
                None,
            );
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pulse_does_not_panic_on_empty_dir() {
        let ws = std::env::temp_dir().join(format!("susi-ambient-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&ws);
        let report = pulse(&ws);
        let _ = report.files_changed;
        let _ = std::fs::remove_dir_all(&ws);
    }
}
