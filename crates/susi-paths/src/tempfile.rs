//! Pure-std temporary directory leaf (`TempDir`, `Builder`, `tempdir`),
//! consolidated into `susi_paths::tempfile` with zero external dependencies.
#![forbid(unsafe_code)]

use std::collections::hash_map::RandomState;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, DirBuilder};
use std::hash::{BuildHasher, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_PREFIX: &str = ".tmp";
const DEFAULT_RAND_BYTES: usize = 6;
const MAX_COLLISION_RETRIES: u32 = 4_096;
const BASE36: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

static NEXT_TEMP_SEQ: AtomicU64 = AtomicU64::new(1);

/// Builder for exclusively reserved temporary directories.
#[derive(Clone, Debug)]
pub struct Builder {
    prefix: OsString,
}

impl Default for Builder {
    fn default() -> Self {
        Self::new()
    }
}

impl Builder {
    /// Create a new temporary directory builder with default `.tmp` prefix and 6 random chars.
    #[must_use]
    pub fn new() -> Self {
        Self {
            prefix: OsString::from(DEFAULT_PREFIX),
        }
    }

    /// Set a custom filename prefix.
    pub fn prefix<S: AsRef<OsStr> + ?Sized>(&mut self, prefix: &S) -> &mut Self {
        self.prefix = prefix.as_ref().to_os_string();
        self
    }

    /// Atomically create a new temporary directory inside [`std::env::temp_dir`].
    pub fn tempdir(&self) -> io::Result<TempDir> {
        self.create_in(&std::env::temp_dir())
    }

    fn create_in(&self, base: &Path) -> io::Result<TempDir> {
        let pid = std::process::id();
        for attempt in 0..MAX_COLLISION_RETRIES {
            let seq = NEXT_TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
            let mut name = OsString::with_capacity(self.prefix.len() + DEFAULT_RAND_BYTES + 8);
            name.push(&self.prefix);
            name.push(generate_token(DEFAULT_RAND_BYTES, pid, seq, attempt));

            let candidate = base.join(name);
            match create_exclusive_private_dir(&candidate) {
                Ok(()) => {
                    return Ok(TempDir {
                        path: candidate,
                        disarmed: false,
                    });
                }
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(err),
            }
        }

        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "exhausted retries reserving unique temporary directory",
        ))
    }
}

/// RAII temporary directory removed automatically when dropped unless [`TempDir::keep`] is called.
pub struct TempDir {
    path: PathBuf,
    disarmed: bool,
}

impl TempDir {
    /// Create a new temporary directory inside [`std::env::temp_dir`].
    pub fn new() -> io::Result<Self> {
        Builder::new().tempdir()
    }

    /// Create a new temporary directory with `prefix` inside [`std::env::temp_dir`].
    pub fn with_prefix<S: AsRef<OsStr>>(prefix: S) -> io::Result<Self> {
        Builder::new().prefix(&prefix).tempdir()
    }

    /// Path to the temporary directory.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Persist the temporary directory on disk and return its [`PathBuf`] without deleting it.
    #[must_use]
    pub fn keep(mut self) -> PathBuf {
        self.disarmed = true;
        std::mem::take(&mut self.path)
    }

    /// Deterministically remove the temporary directory and surface any I/O error.
    pub fn close(mut self) -> io::Result<()> {
        self.disarmed = true;
        remove_dir_robust(&self.path)
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
        if !self.disarmed {
            let _ = remove_dir_robust(&self.path);
        }
    }
}

/// Create a new temporary directory inside [`std::env::temp_dir`].
pub fn tempdir() -> io::Result<TempDir> {
    TempDir::new()
}

#[cfg(unix)]
fn create_exclusive_private_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    DirBuilder::new().mode(0o700).create(path)
}

#[cfg(not(unix))]
fn create_exclusive_private_dir(path: &Path) -> io::Result<()> {
    DirBuilder::new().create(path)
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
            let idx = (word % 36) as usize;
            out.push(char::from(BASE36[idx]));
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
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = perms.mode();
                    perms.set_mode(mode | 0o200);
                }
                #[cfg(not(unix))]
                {
                    #[allow(clippy::permissions_set_readonly_false)]
                    perms.set_readonly(false);
                }
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
// Unit tests assert directly on ephemeral temporary directory fixtures via unwrap/expect.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::sync::{Arc, Mutex};
    use std::thread;

    #[test]
    fn tempdir_creates_directory_and_removes_on_drop() {
        let captured: PathBuf;
        {
            let dir = tempdir().unwrap();
            captured = dir.path().to_path_buf();
            assert!(captured.is_dir());
            fs::write(captured.join("probe.txt"), b"hello").unwrap();
        }
        assert!(!captured.exists());
    }

    #[test]
    fn keep_disarms_drop_cleanup() {
        let dir = Builder::new().prefix("susi-keep-").tempdir().unwrap();
        let marker = dir.path().join("kept.txt");
        fs::write(&marker, b"persisted").unwrap();
        let kept_path = dir.keep();
        assert!(kept_path.is_dir());
        assert_eq!(fs::read(&marker).unwrap(), b"persisted");
        fs::remove_dir_all(&kept_path).unwrap();
    }

    #[test]
    fn close_removes_directory_deterministically() {
        let dir = TempDir::with_prefix("susi-close-").unwrap();
        let path = dir.path().to_path_buf();
        fs::write(path.join("nested.bin"), [1_u8, 2, 3, 4]).unwrap();
        dir.close().unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn builder_prefix_reserves_6_char_random_token() {
        let parent = tempdir().unwrap();
        let child = Builder::new()
            .prefix("alpha-")
            .create_in(parent.path())
            .unwrap();
        let name = child
            .path()
            .file_name()
            .unwrap()
            .to_str()
            .expect("utf-8 temp name");
        assert!(name.starts_with("alpha-"));
        assert_eq!(name.len(), "alpha-".len() + DEFAULT_RAND_BYTES);
    }

    #[test]
    fn concurrent_threads_reserve_distinct_directories() {
        let parent = Arc::new(tempdir().unwrap());
        let seen = Arc::new(Mutex::new(BTreeSet::new()));
        let mut handles = Vec::new();
        for _ in 0..16 {
            let parent = Arc::clone(&parent);
            let seen = Arc::clone(&seen);
            handles.push(thread::spawn(move || {
                let mut local = Vec::with_capacity(32);
                for _ in 0..32 {
                    let td = Builder::new()
                        .prefix("race-")
                        .create_in(parent.path())
                        .unwrap();
                    assert!(seen.lock().unwrap().insert(td.path().to_path_buf()));
                    local.push(td);
                }
                assert_eq!(local.len(), 32);
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(seen.lock().unwrap().len(), 512);
    }

    #[test]
    fn readonly_nested_file_is_cleaned_up_on_drop() {
        let captured: PathBuf;
        {
            let dir = tempdir().unwrap();
            captured = dir.path().to_path_buf();
            let ro_file = captured.join("readonly.pack");
            fs::write(&ro_file, b"immutable").unwrap();
            let mut perms = fs::metadata(&ro_file).unwrap().permissions();
            perms.set_readonly(true);
            fs::set_permissions(&ro_file, perms).unwrap();
        }
        assert!(!captured.exists());
    }

    #[cfg(unix)]
    #[test]
    fn unix_tempdir_mode_is_0700() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let mode = fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }
}
