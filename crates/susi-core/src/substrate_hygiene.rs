//! SUSI-owned residue: what `susi os status` warns about and what
//! `susi os clean` (alias `susi os-clean`) and the MCP `clean` tool remove.
//! One implementation, so the CLI and the socket report the same bytes.
//!
//! Only SUSI's own leftovers are touched: stray multi-hundred-MiB files in
//! `substrate_home/bin` (never the installed `susi` executable or `lib/`),
//! an oversized rotated metrics file and a stale pre-rotation audit log.
//! Host or other applications' caches are never removed.

use serde::Serialize;
use std::path::PathBuf;

/// A file in `substrate_home/bin` at least this large is stray residue.
const STRAY_WARN_BYTES: u64 = 256 * 1024 * 1024;

/// Human-readable byte count (`1.5 GiB`).
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} {}", UNITS[i])
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

/// One residue file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Residue {
    /// `rotated_metrics`, `stale_audit_log` or `stray_bin`.
    pub kind: &'static str,
    pub path: PathBuf,
    pub bytes: u64,
}

/// Every residue file `clean` would remove, in removal order.
pub fn residue() -> Vec<Residue> {
    let mut out = Vec::new();
    // Pre-cap rotated metrics residue: the sink rotates one generation
    // at 64MiB, so a `.1` larger than that can only be residue —
    // independent of the bin sweep so a missing bin dir cannot skip it.
    if let Some((path, bytes)) = susi_error::oversized_rotated_metrics() {
        out.push(Residue {
            kind: "rotated_metrics",
            path,
            bytes,
        });
    }
    // Same residue class: the flat audit.log is dead once dated rotation
    // files exist and are newer (the fallback sink would be newer instead).
    if let Some((path, bytes)) = susi_error::stale_flat_audit_log() {
        out.push(Residue {
            kind: "stale_audit_log",
            path,
            bytes,
        });
    }
    let bin_dir = susi_paths::SusiDirs::substrate_home().join("bin");
    if let Ok(read) = std::fs::read_dir(&bin_dir) {
        for entry in read.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            // The installed executable and its library dir are not residue.
            if matches!(name.as_str(), "susi" | "susi.exe" | "lib") {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_file() && meta.len() >= STRAY_WARN_BYTES {
                out.push(Residue {
                    kind: "stray_bin",
                    path: entry.path(),
                    bytes: meta.len(),
                });
            }
        }
    }
    out
}

/// The `susi os status` warnings for current residue.
pub fn residue_warnings() -> Vec<String> {
    residue()
        .into_iter()
        .map(|r| {
            let size = human_bytes(r.bytes);
            match r.kind {
                "rotated_metrics" => format!(
                    "oversized rotated metrics {} ({size}) — `susi os clean` removes it",
                    r.path.display()
                ),
                "stale_audit_log" => format!(
                    "stale pre-rotation audit log {} ({size}) — `susi os clean` removes it",
                    r.path.display()
                ),
                _ => format!(
                    "stray {} ({size}) in {} — `susi os clean` removes it",
                    r.path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default(),
                    r.path
                        .parent()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default()
                ),
            }
        })
        .collect()
}

/// What one `clean` pass did.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CleanReport {
    pub removed: Vec<Residue>,
    /// `(path, error)` for residue that could not be removed.
    pub failed: Vec<(PathBuf, String)>,
    pub removed_count: usize,
    pub reclaimed_bytes: u64,
    pub reclaimed: String,
}

/// Remove SUSI-owned residue and report bytes freed.
pub fn clean() -> CleanReport {
    let mut report = CleanReport::default();
    for item in residue() {
        match std::fs::remove_file(&item.path) {
            Ok(()) => {
                report.reclaimed_bytes += item.bytes;
                report.removed.push(item);
            }
            Err(e) => report.failed.push((item.path, e.to_string())),
        }
    }
    report.removed_count = report.removed.len();
    report.reclaimed = human_bytes(report.reclaimed_bytes);
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_bytes_scales_units() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(3 * 1024 * 1024 * 1024), "3.0 GiB");
    }

    #[test]
    fn clean_removes_only_large_strays_and_keeps_the_executable() {
        let _lock = crate::commit_log::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = std::env::temp_dir().join(format!(
            "susi-hygiene-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let mut env = susi_paths::test_env::EnvGuard::isolated();
        env.set("SUSI_HOME", &home);
        let bin = susi_paths::SusiDirs::substrate_home().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let stray = bin.join("old-build.exe");
        let file = std::fs::File::create(&stray).unwrap();
        file.set_len(STRAY_WARN_BYTES).unwrap();
        drop(file);
        let exe = std::fs::File::create(bin.join("susi.exe")).unwrap();
        exe.set_len(STRAY_WARN_BYTES).unwrap();
        drop(exe);
        std::fs::write(bin.join("small.txt"), b"x").unwrap();

        assert!(residue_warnings()
            .iter()
            .any(|w| w.contains("old-build.exe")));
        let report = clean();
        assert_eq!(report.removed_count, 1, "{report:?}");
        assert_eq!(report.reclaimed_bytes, STRAY_WARN_BYTES);
        assert!(!stray.exists());
        assert!(bin.join("susi.exe").exists());
        assert!(bin.join("small.txt").exists());
        assert_eq!(clean().removed_count, 0);
        drop(env);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn human_bytes_formats_zero_mib_and_tib_boundaries_accurately() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1024 * 1024), "1.0 MiB");
        assert_eq!(human_bytes(1024_u64.pow(4)), "1.0 TiB");
    }

    #[test]
    fn clean_report_default_and_residue_struct_serialization_contract() {
        let def = CleanReport::default();
        assert_eq!(def.removed_count, 0);
        assert_eq!(def.reclaimed_bytes, 0);
        assert!(def.removed.is_empty());

        let residue = Residue {
            kind: "stray_bin",
            path: PathBuf::from("old.bin"),
            bytes: 300,
        };
        let val = serde_json::to_value(&residue).unwrap();
        assert_eq!(val["kind"], "stray_bin");
        assert_eq!(val["bytes"], 300);
    }
}
