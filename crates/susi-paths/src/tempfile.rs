//! Pure-std Linux temporary directory leaf (`TempDir`, `tempdir`).
#![forbid(unsafe_code)]

use std::collections::hash_map::RandomState;
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, DirBuilder};
use std::hash::{BuildHasher, Hasher};
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_PREFIX: &str = ".tmp";
const DEFAULT_RAND_BYTES: usize = 6;
const MAX_COLLISION_RETRIES: u32 = 4_096;
const BASE36: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

static NEXT_TEMP_SEQ: AtomicU64 = AtomicU64::new(1);

/// RAII temporary directory removed automatically when dropped.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// Create a new `0700` temporary directory inside [`std::env::temp_dir`].
    pub fn new() -> io::Result<Self> {
        let base = std::env::temp_dir();
        let pid = std::process::id();
        for attempt in 0..MAX_COLLISION_RETRIES {
            let seq = NEXT_TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
            let mut name = OsString::with_capacity(DEFAULT_PREFIX.len() + DEFAULT_RAND_BYTES + 8);
            name.push(DEFAULT_PREFIX);
            name.push(generate_token(DEFAULT_RAND_BYTES, pid, seq, attempt));
            let candidate = base.join(name);
            match DirBuilder::new().mode(0o700).create(&candidate) {
                Ok(()) => return Ok(Self { path: candidate }),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(err),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "exhausted retries reserving unique temporary directory",
        ))
    }

    /// Path to the temporary directory.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        self.path()
    }
}

impl fmt::Debug for TempDir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TempDir")
            .field("path", &self.path())
            .finish()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = remove_dir_robust(&self.path);
    }
}

/// Create a new temporary directory inside [`std::env::temp_dir`].
pub fn tempdir() -> io::Result<TempDir> {
    TempDir::new()
}

fn generate_token(len: usize, pid: u32, seq: u64, attempt: u32) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0_u128, |d| d.as_nanos());
    let mut out = String::with_capacity(len);
    let mut block_idx = 0_u64;
    while out.len() < len {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u32(pid);
        hasher.write_u64(seq);
        hasher.write_u32(attempt);
        hasher.write_u128(nanos);
        hasher.write_u64(block_idx);
        let mut word = hasher.finish();
        for _ in 0..10 {
            if out.len() >= len {
                break;
            }
            out.push(char::from(BASE36[(word % 36) as usize]));
            word /= 36;
        }
        block_idx = block_idx.wrapping_add(1);
    }
    out
}

fn remove_dir_robust(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(first_err) => {
            clear_readonly_recursive(path);
            match fs::remove_dir_all(path) {
                Ok(()) => Ok(()),
                Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(_) => Err(first_err),
            }
        }
    }
}

fn clear_readonly_recursive(path: &Path) {
    if let Ok(meta) = fs::symlink_metadata(path) {
        if !meta.file_type().is_symlink() {
            let mut perms = meta.permissions();
            if perms.readonly() {
                let mode = perms.mode();
                perms.set_mode(mode | 0o200);
                let _ = fs::set_permissions(path, perms);
            }
            if meta.is_dir() {
                if let Ok(entries) = fs::read_dir(path) {
                    for entry in entries.flatten() {
                        clear_readonly_recursive(&entry.path());
                    }
                }
            }
        }
    }
}

#[cfg(test)]
// Unit tests assert directly on ephemeral temporary directory fixtures.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

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
}
