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
    let mut state = load_scan_state(workspace);
    let changed = scan_with_state(workspace, &mut state);
    save_scan_state(workspace, &state);
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

/// Per-workspace mtime state, persisted under substrate so a daemon
/// restart or a one-shot `pulse` doesn't re-record every tracked file
/// as "changed" — without it each boot flooded the graph with a
/// full-tree ingest of files that never changed.
fn scan_state_path() -> std::path::PathBuf {
    crate::susi_paths::SusiDirs::substrate_home().join("ambient_scan_state.json")
}

fn load_scan_state(workspace: &Path) -> HashMap<String, u64> {
    let ws = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf())
        .display()
        .to_string();
    let Ok(text) = std::fs::read_to_string(scan_state_path()) else {
        return HashMap::new();
    };
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v.get(ws).cloned())
        .and_then(|v| serde_json::from_value::<HashMap<String, u64>>(v).ok())
        .unwrap_or_default()
}

fn save_scan_state(workspace: &Path, state: &HashMap<String, u64>) {
    let ws = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf())
        .display()
        .to_string();
    let path = scan_state_path();
    let mut root = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    root[ws] = serde_json::to_value(state).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string(&root) {
        let _ = std::fs::write(path, text);
    }
}

fn ambient_loop(workspace: PathBuf) {
    let mut last = load_scan_state(&workspace);
    loop {
        let changed = scan_with_state(&workspace, &mut last);
        if changed > 0 {
            save_scan_state(&workspace, &last);
            let _ = susi_gmcp::tools::semantic_index::SemanticIndex::refresh(&workspace);
        }
        std::thread::sleep(Duration::from_secs(15));
    }
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
            // Never follow symlinks: a link to `/` or `~/.ssh` would pull
            // outside content into the index, and a link cycle never ends.
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                // `build-cache` is install.sh's persistent CARGO_TARGET_DIR —
                // its fingerprint churn used to dominate the graph (~98% of
                // external_context nodes were build artifacts).
                if name != ".git"
                    && name != "target"
                    && name != ".susi"
                    && name != "node_modules"
                    && name != "build-cache"
                {
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
