//! Hot‑Pluggable Cell Watcher (Swarm OS Vision – Bullet 6).
//!
//! Monitors `~/.susi/cells/` for filesystem changes using a polling strategy
//! (no `inotify` dep needed — keeps the build portable across Linux / macOS /
//! Windows). When a new `.cell`, `.json`, or `.wasm` file appears, the watcher
//! triggers [`auto_prime_ecosystem`] to pick it up without a daemon restart.
//!
//! The watcher runs in a dedicated background thread and checks for changes
//! at a configurable interval (default: 5 seconds).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Configuration for the cell filesystem watcher.
#[derive(Debug, Clone)]
pub struct CellWatcherConfig {
    /// Directory to monitor for new cells.
    pub cells_dir: PathBuf,
    /// Poll interval.
    pub poll_interval: Duration,
}

impl CellWatcherConfig {
    /// Creates a watcher config for the given substrate home.
    #[must_use]
    pub fn for_substrate(substrate: &Path) -> Self {
        Self {
            cells_dir: substrate.join("cells"),
            poll_interval: Duration::from_secs(5),
        }
    }
}

/// Handle to a running cell watcher. Dropping it (or calling `stop`) signals
/// the background thread to exit.
pub struct CellWatcherHandle {
    shutdown: Arc<AtomicBool>,
    #[allow(dead_code)] // the JoinHandle is kept to ensure the thread outlives the caller
    thread: Option<std::thread::JoinHandle<()>>,
}

impl CellWatcherHandle {
    /// Signals the watcher thread to stop.
    pub fn stop(&self) {
        self.shutdown.store(true, Ordering::Release);
    }
}

impl Drop for CellWatcherHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Starts the hot‑plug cell watcher in a background thread.
///
/// Returns a [`CellWatcherHandle`] that can be used to stop the watcher.
/// The watcher polls the cells directory for new files and triggers
/// auto-discovery when changes are detected.
pub fn start_cell_watcher(config: CellWatcherConfig) -> CellWatcherHandle {
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_clone = shutdown.clone();

    let thread = std::thread::Builder::new()
        .name("susi-cell-watcher".into())
        .spawn(move || {
            watcher_loop(&config, &shutdown_clone);
        })
        .ok();

    CellWatcherHandle { shutdown, thread }
}

/// The main polling loop. Tracks known files and triggers re-discovery
/// when the set changes.
fn watcher_loop(config: &CellWatcherConfig, shutdown: &AtomicBool) {
    let mut known_files: HashSet<PathBuf> = scan_cell_files(&config.cells_dir);

    tracing::info!(
        "[cell_watcher] Watching {} — {} initial cells",
        config.cells_dir.display(),
        known_files.len(),
    );

    while !shutdown.load(Ordering::Acquire) {
        std::thread::sleep(config.poll_interval);

        if shutdown.load(Ordering::Acquire) {
            break;
        }

        let current_files = scan_cell_files(&config.cells_dir);

        // Detect new files
        let new_files: Vec<PathBuf> = current_files.difference(&known_files).cloned().collect();

        // Detect removed files
        let removed_files: Vec<PathBuf> = known_files.difference(&current_files).cloned().collect();

        if !new_files.is_empty() {
            tracing::info!(
                "[cell_watcher] Detected {} new cell(s): {:?}",
                new_files.len(),
                new_files
                    .iter()
                    .filter_map(|p| p.file_name())
                    .collect::<Vec<_>>(),
            );
            // Re-run auto-discovery to pick up new cells.
            // The function is idempotent: already-running cells won't be re-spawned
            // because the process spawn is fire-and-forget and the OS handles the PID.
            if let Some(parent) = config.cells_dir.parent() {
                crate::auto_discovery::auto_prime_ecosystem(parent);
            }
        }

        if !removed_files.is_empty() {
            tracing::info!(
                "[cell_watcher] Detected {} removed cell(s): {:?}",
                removed_files.len(),
                removed_files
                    .iter()
                    .filter_map(|p| p.file_name())
                    .collect::<Vec<_>>(),
            );
            // Future: signal the blackboard to unregister these cells.
        }

        known_files = current_files;
    }

    tracing::info!("[cell_watcher] Watcher stopped");
}

/// Scans the cells directory and returns the set of recognised cell files.
fn scan_cell_files(cells_dir: &Path) -> HashSet<PathBuf> {
    let mut files = HashSet::new();
    if let Ok(entries) = std::fs::read_dir(cells_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file()
                && path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|name| {
                        name.starts_with("susi-cell-")
                            || name.ends_with(".cell")
                            || name.ends_with(".json")
                            || name.ends_with(".wasm")
                    })
            {
                files.insert(path);
            }
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn scan_empty_dir_returns_empty() {
        let tmp = std::env::temp_dir().join("susi-cell-watcher-test-empty");
        let _ = fs::create_dir_all(&tmp);
        let files = scan_cell_files(&tmp);
        assert!(files.is_empty());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn scan_finds_cell_files() {
        let tmp = std::env::temp_dir().join("susi-cell-watcher-test-scan");
        let _ = fs::create_dir_all(&tmp);
        fs::write(tmp.join("susi-cell-infer"), "").unwrap();
        fs::write(tmp.join("my-agent.cell"), "").unwrap();
        fs::write(tmp.join("config.json"), "").unwrap();
        fs::write(tmp.join("reflex.wasm"), "").unwrap();
        fs::write(tmp.join("readme.txt"), "").unwrap(); // should be ignored

        let files = scan_cell_files(&tmp);
        assert_eq!(files.len(), 4);
        assert!(files.contains(&tmp.join("susi-cell-infer")));
        assert!(files.contains(&tmp.join("my-agent.cell")));
        assert!(files.contains(&tmp.join("config.json")));
        assert!(files.contains(&tmp.join("reflex.wasm")));
        assert!(!files.contains(&tmp.join("readme.txt")));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn watcher_handle_stops_cleanly() {
        let tmp = std::env::temp_dir().join("susi-cell-watcher-test-stop");
        let _ = fs::create_dir_all(&tmp);
        let config = CellWatcherConfig {
            cells_dir: tmp.clone(),
            poll_interval: Duration::from_millis(50),
        };
        let handle = start_cell_watcher(config);
        // Give it a tick
        std::thread::sleep(Duration::from_millis(100));
        handle.stop();
        // Give the thread time to exit
        std::thread::sleep(Duration::from_millis(100));
        let _ = fs::remove_dir_all(&tmp);
    }
}
