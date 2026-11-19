#![cfg(unix)]
// Integration fixture asserts: failed tempdir/fs ops must fail the test loudly.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use susi_paths::tempfile::tempdir;

#[test]
fn tempdir_creates_0700_directory_and_cleans_readonly_files_on_drop() {
    let captured: PathBuf;
    {
        let dir = tempdir().unwrap();
        captured = dir.path().to_path_buf();
        assert_eq!(
            fs::metadata(&captured).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let ro_file = captured.join("readonly.pack");
        fs::write(&ro_file, b"immutable").unwrap();
        let mut perms = fs::metadata(&ro_file).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&ro_file, perms).unwrap();
    }
    assert!(!captured.exists());
}
