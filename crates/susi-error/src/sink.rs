//! Append-only writer for `error_metrics.jsonl`, used by the `susi-error`
//! service and by every client's local fallback so both write the same way.

/// The sink is append-only and error paths are hot — without a cap the
/// file grows without bound (178MB observed). Past the cap it rotates one
/// generation (`error_metrics.jsonl.1`); a racing writer may lose a line
/// across the rename, which a metrics sink tolerates.
pub const METRICS_CAP_BYTES: u64 = 64 * 1024 * 1024;

/// Append one JSONL record to the metrics file at `path`.
///
/// The record goes out in a single `write_all` on an `O_APPEND` handle so
/// concurrent writers (service plus local fallbacks) do not interleave
/// inside a line — `writeln!` could split a record across writes. The file
/// is created owner-only on Unix: error text is redacted, but only with
/// the bundled patterns (see `contract::redact_for_metrics`).
pub fn append_metrics_line(
    path: &std::path::Path,
    entry: &serde_json::Value,
) -> std::io::Result<()> {
    use std::io::Write;
    if std::fs::metadata(path)
        .map(|m| m.len() > METRICS_CAP_BYTES)
        .unwrap_or(false)
    {
        let _ = std::fs::rename(path, path.with_file_name("error_metrics.jsonl.1"));
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)?
        .write_all(format!("{entry}\n").as_bytes())
}

#[cfg(test)]
mod tests {
    #[test]
    fn records_append_as_whole_owner_only_lines() {
        let dir = std::env::temp_dir().join(format!(
            "susi-metrics-sink-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("error_metrics.jsonl");
        super::append_metrics_line(&path, &serde_json::json!({"n": 1})).unwrap();
        super::append_metrics_line(&path, &serde_json::json!({"n": 2})).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, "{\"n\":1}\n{\"n\":2}\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
