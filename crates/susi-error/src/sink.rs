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
/// is created owner-only on Unix (`0600`) and Windows (explicit user ACL):
/// error text is redacted, but only with the bundled patterns (see
/// `contract::redact_for_metrics`).
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
    #[cfg(windows)]
    let existed_before = path.exists();
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(format!("{entry}\n").as_bytes())?;
    drop(file);
    // Best-effort, as before. Set natively: an `icacls` process launched
    // from a windowless caller (the daemon) opened a console window.
    #[cfg(windows)]
    if !existed_before {
        let _ = winapi::acl::restrict_to_current_user(path);
    }
    Ok(())
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
        #[cfg(windows)]
        {
            // Exactly one entry: the current user's full control, not
            // inherited; no Everyone, no BUILTIN\Users.
            let aces = winapi::acl::read_dacl(&path).unwrap().unwrap();
            assert_eq!(
                aces,
                vec![winapi::acl::Ace {
                    sid: winapi::acl::current_user_sid().unwrap(),
                    deny: false,
                    inherited: false,
                    mask: winapi::acl::FILE_ALL_ACCESS,
                }],
                "unexpected ACL on metrics sink"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotates_to_dot_one_when_existing_file_exceeds_metrics_cap() {
        let dir = std::env::temp_dir().join(format!(
            "susi-metrics-rotate-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("error_metrics.jsonl");
        let rotated = dir.join("error_metrics.jsonl.1");

        let f = std::fs::File::create(&path).unwrap();
        f.set_len(super::METRICS_CAP_BYTES + 1).unwrap();
        drop(f);

        super::append_metrics_line(&path, &serde_json::json!({"seq": 42})).unwrap();
        assert_eq!(
            std::fs::metadata(&rotated).unwrap().len(),
            super::METRICS_CAP_BYTES + 1
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"seq\":42}\n");

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "susi-metrics-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_file_exactly_at_the_cap_appends_in_place_without_rotating() {
        let dir = scratch("at-cap");
        let path = dir.join("error_metrics.jsonl");
        let f = std::fs::File::create(&path).unwrap();
        f.set_len(super::METRICS_CAP_BYTES).unwrap();
        drop(f);

        super::append_metrics_line(&path, &serde_json::json!({"n": 1})).unwrap();
        assert!(
            !dir.join("error_metrics.jsonl.1").exists(),
            "cap is inclusive"
        );
        let line = "{\"n\":1}\n";
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            super::METRICS_CAP_BYTES + line.len() as u64
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotation_past_the_cap_replaces_the_previous_generation() {
        let dir = scratch("replace-gen");
        let path = dir.join("error_metrics.jsonl");
        let rotated = dir.join("error_metrics.jsonl.1");
        std::fs::write(&rotated, "stale generation\n").unwrap();
        let f = std::fs::File::create(&path).unwrap();
        f.set_len(super::METRICS_CAP_BYTES + 1).unwrap();
        drop(f);

        super::append_metrics_line(&path, &serde_json::json!({"n": 2})).unwrap();
        assert_eq!(
            std::fs::metadata(&rotated).unwrap().len(),
            super::METRICS_CAP_BYTES + 1,
            "only one generation is kept; the old .1 is replaced"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"n\":2}\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_directory_at_the_sink_path_is_an_error() {
        let dir = scratch("is-dir");
        let path = dir.join("error_metrics.jsonl");
        std::fs::create_dir_all(&path).unwrap();
        assert!(super::append_metrics_line(&path, &serde_json::json!({"n": 3})).is_err());
        assert!(path.is_dir(), "the directory is left as it was");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
