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
            if path.is_file() {
                // If the file is executable (or just a file in the cells dir), spawn it as a Swarm Cell
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    #[allow(clippy::collapsible_if)]
                    if name.starts_with("susi-cell-") || name.ends_with(".cell") {
                        std::thread::spawn(move || {
                            let _ = std::process::Command::new(path).spawn();
                        });
                    } else if name.ends_with(".json") {
                        static PORT_COUNTER: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(10000);
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
                    }
                }
            }
        }
    }
}

pub fn bootstrap_zero_config_substrate(_substrate: &Path) {
    // Delegated to Swarm OS.
}
