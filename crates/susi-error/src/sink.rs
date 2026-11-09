//! Append-only writer for `error_metrics.jsonl`, used by the `susi-error`
//! service and by every client's local fallback so both write the same way.

/// The sink is append-only and error paths are hot — without a cap the
/// file grows without bound (178MB observed). Past the cap it rotates one
/// generation (`error_metrics.jsonl.1`); a racing writer may lose a line
/// across the rename, which a metrics sink tolerates.
pub const METRICS_CAP_BYTES: u64 = 64 * 1024 * 1024;

/// Append one JSONL record to the metrics file at `path`.
///
/// The record goes out in a single `write_all` on an append handle so
/// concurrent writers (service plus local fallbacks) do not interleave
/// inside a line — `writeln!` could split a record across writes. Error text
/// is redacted, but only with the bundled patterns (see
/// `contract::redact_for_metrics`), so a file this call creates is
/// owner-only from birth: `0600` on Unix, and on Windows a protected DACL
/// for the process token's user applied inside the create call itself
/// (`susi_vendor_winapi::acl::open_owner_only_append`, no `icacls` process,
/// which opened a console window from a windowless caller). Create-or-open
/// is one call, so no concurrent writer or reader can open the file before
/// it is restricted, and a failure creates nothing, so nothing is ever
/// deleted (a delete by name could remove a replacement another writer made
/// after a rotation). An existing file is appended to as it is.
pub fn append_metrics_line(
    path: &std::path::Path,
    entry: &serde_json::Value,
) -> std::io::Result<()> {
    append_with(path, entry, open_owner_only_append)
}

/// Create-or-open `path` for appending, owner-only when created.
fn open_owner_only_append(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    #[cfg(windows)]
    {
        winapi::acl::open_owner_only_append(path)
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
    }
}

/// [`append_metrics_line`] with the opener passed in, so tests can inject a
/// failing or racing open. Whatever the opener returns, an error is passed
/// back as it is: this function never removes or rewrites a path.
fn append_with(
    path: &std::path::Path,
    entry: &serde_json::Value,
    open: impl FnOnce(&std::path::Path) -> std::io::Result<std::fs::File>,
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
    let mut file = open(path)?;
    file.write_all(format!("{entry}\n").as_bytes())
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

    /// Failure injection for the race the old create-then-restrict sink lost:
    /// between this writer's open and its failure, another writer rotates
    /// the file and makes a replacement at the path. The failure is reported
    /// and both files are left exactly as the other writer left them (the
    /// old cleanup removed the path by name, deleting the replacement).
    #[test]
    fn a_failed_open_never_deletes_what_another_writer_put_at_the_path() {
        let dir = scratch("inject-race");
        let path = dir.join("error_metrics.jsonl");
        let rotated = dir.join("error_metrics.jsonl.1");
        super::append_metrics_line(&path, &serde_json::json!({"gen": 1})).unwrap();
        let error = super::append_with(&path, &serde_json::json!({"gen": 9}), |p| {
            // The other writer, inside this writer's open.
            std::fs::rename(p, p.with_file_name("error_metrics.jsonl.1")).unwrap();
            super::append_metrics_line(p, &serde_json::json!({"gen": 2})).unwrap();
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "injected: the access list could not be applied",
            ))
        })
        .expect_err("the injected failure is reported");
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"gen\":2}\n");
        assert_eq!(std::fs::read_to_string(&rotated).unwrap(), "{\"gen\":1}\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A failed open of a new sink leaves no file behind, so nothing is ever
    /// appended to a file whose access list was not applied.
    #[test]
    fn a_failed_open_creates_nothing() {
        let dir = scratch("inject-fail");
        let path = dir.join("error_metrics.jsonl");
        let result = super::append_with(&path, &serde_json::json!({"n": 1}), |_| {
            Err(std::io::Error::other("injected"))
        });
        assert!(result.is_err());
        assert!(!path.exists());
        assert!(super::append_metrics_line(
            std::path::Path::new("bad\0name"),
            &serde_json::json!({"n": 1})
        )
        .is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The exact interleaving behind the review finding, made deterministic:
    /// writer A holds the newly created sink open and has written nothing;
    /// writer B appends. B's line lands in a file that is already owner-only.
    /// Then the file passes the cap while A still holds it: B's next append
    /// rotates it (A's handle shares delete) and starts an owner-only file.
    #[cfg(windows)]
    #[test]
    fn a_concurrent_writer_only_ever_appends_to_an_owner_only_file() {
        use std::io::Write;
        let dir = scratch("interleave");
        let path = dir.join("error_metrics.jsonl");
        let owner_only = vec![winapi::acl::Ace {
            sid: winapi::acl::current_user_sid().unwrap(),
            deny: false,
            inherited: false,
            mask: winapi::acl::FILE_ALL_ACCESS,
        }];
        let mut writer_a = winapi::acl::open_owner_only_append(&path).unwrap();
        assert_eq!(winapi::acl::read_dacl(&path).unwrap().unwrap(), owner_only);
        super::append_metrics_line(&path, &serde_json::json!({"w": "b"})).unwrap();
        writer_a.write_all(b"{\"w\":\"a\"}\n").unwrap();
        assert_eq!(winapi::acl::read_dacl(&path).unwrap().unwrap(), owner_only);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{\"w\":\"b\"}\n{\"w\":\"a\"}\n"
        );

        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(super::METRICS_CAP_BYTES + 1)
            .unwrap();
        super::append_metrics_line(&path, &serde_json::json!({"w": "b2"})).unwrap();
        let rotated = dir.join("error_metrics.jsonl.1");
        assert_eq!(
            std::fs::metadata(&rotated).unwrap().len(),
            super::METRICS_CAP_BYTES + 1
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"w\":\"b2\"}\n");
        assert_eq!(winapi::acl::read_dacl(&path).unwrap().unwrap(), owner_only);
        drop(writer_a);
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
