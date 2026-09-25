//! Zero-config ecosystem discovery.
//!
//! Delegated to Swarm OS cells (`susi-gawd` / `susi-gmcp`) over IPC.

use std::path::Path;

/// Orchestrate dynamic capabilities on engine boot.
#[allow(clippy::unwrap_used)] // SAFETY: current_exe and parent will always exist in a valid build
pub fn auto_prime_ecosystem(substrate: &Path) {
    let cells_dir = substrate.join("cells");
    if !cells_dir.exists() {
        let _ = std::fs::create_dir_all(&cells_dir);
        return;
    }

    if let Ok(entries) = std::fs::read_dir(&cells_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            #[allow(clippy::collapsible_if)] // inner if-let guards a multi-branch match
            if path.is_file() {
                // If the file is executable (or just a file in the cells dir), spawn it as a Swarm Cell
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if name.starts_with("susi-cell-") || name.ends_with(".cell") {
                        std::thread::spawn(move || {
                            let _ = std::process::Command::new(path).spawn();
                        });
                    } else if name.ends_with(".json") {
                        static PORT_COUNTER: std::sync::atomic::AtomicU16 =
                            std::sync::atomic::AtomicU16::new(10000);
                        let port = PORT_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let bind_addr = format!("127.0.0.1:{}", port);
                        let universal_cell_path = std::env::current_exe()
                            .unwrap_or_else(|_| "susi-daemon".into())
                            .parent()
                            .unwrap()
                            .join("susi-universal-cell");

                        let path_clone = path.clone();
                        std::thread::spawn(move || {
                            let _ = std::process::Command::new(universal_cell_path)
                                .arg(bind_addr)
                                .arg(path_clone)
                                .spawn();
                        });
                    } else if name.ends_with(".wasm") {
                        // Spawn WASM cell via sandbox_wasm in a background thread.
                        // The cell is loaded and its `_start` entry point is executed.
                        let wasm_path = path.clone();
                        std::thread::spawn(move || {
                            match crate::sandbox_wasm::spawn_wasm_cell(wasm_path.clone()) {
                                Ok(mut cell) => {
                                    tracing::info!(
                                        "[auto_discovery] Loaded WASM cell: {}",
                                        wasm_path.display()
                                    );
                                    if let Err(e) = cell.execute("_start") {
                                        tracing::warn!(
                                            "[auto_discovery] WASM cell _start failed: {:#}",
                                            e
                                        );
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "[auto_discovery] Failed to load WASM cell {}: {:#}",
                                        wasm_path.display(),
                                        e
                                    );
                                }
                            }
                        });
                    }
                }
            }
        }
    }
}
