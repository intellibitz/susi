//! Verdicts about model files — a GGUF payload is whole, its chat template
//! takes tools, a tokenizer parses — remembered by file identity (path, size,
//! modification time, and which file it is: inode and change time on Unix,
//! creation time on Windows) in this process and across processes on disk.
//!
//! Each verdict reads the whole file's metadata or parses it outright (a
//! tokenizer is megabytes of JSON), and every process that selects a local
//! model — `susi status` among them — asked again: over a second per model.
//! A file whose size and modification time are unchanged gets the verdict
//! already reached for it, the identity the in-process caches here always
//! trusted. Only a verdict the file's bytes decided is remembered: one an
//! I/O error ended (the file could not be opened or read — locked by another
//! process, gone mid-read) says nothing about the file and is asked again,
//! as is one whose file changed while it was judged, or was written so
//! recently that another write within the same timestamp tick could leave
//! its identity unchanged.

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

/// Bumped whenever a verdict's meaning changes.
const FORMAT: u32 = 2;

/// Beyond this many remembered files the memory starts over, as the
/// per-verdict caches it replaces did.
const MAX_ENTRIES: usize = 256;

/// A file modified less than this long before it was judged is not
/// remembered: filesystem timestamps are coarse (a clock tick on NTFS and
/// Linux, two seconds on FAT), and a rewrite of the same size within one
/// tick would leave the identity a remembered verdict is keyed by unchanged.
const RACY_WINDOW: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Remembered {
    probe: String,
    path: PathBuf,
    len: u64,
    mtime_ns: u128,
    /// [`node_of`]: which file it was, beyond size and mtime.
    #[serde(default)]
    node: String,
    verdict: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct VerdictFile {
    format: u32,
    /// The binary whose parsers reached these verdicts ([`binary`]); another
    /// build judges for itself.
    #[serde(default)]
    binary: String,
    entries: Vec<Remembered>,
}

/// The running binary's identity (path, size and modification time), taken
/// once, at first use: a process whose binary is replaced on disk later
/// (self-update, `cargo install` over it) still judges with the parsers it
/// started with, and must not tag its verdicts with the new build's
/// identity. Empty when it cannot be read (on Linux a replaced binary's
/// path ends in "(deleted)"): such a process neither reads nor writes the
/// file on disk.
fn binary() -> &'static str {
    static BINARY: OnceLock<String> = OnceLock::new();
    BINARY.get_or_init(|| {
        std::env::current_exe()
            .ok()
            .and_then(|exe| {
                let meta = std::fs::metadata(&exe).ok()?;
                let modified = mtime_ns(meta.modified().ok()?);
                Some(format!("{} {} {modified}", exe.display(), meta.len()))
            })
            .unwrap_or_default()
    })
}

type Memory = DashMap<(String, PathBuf), Remembered>;

/// A file's identity: its size and modification time.
pub(crate) type FileIdentity = (u64, SystemTime);

/// The identity of the file at `path` now, if it can be read.
pub(crate) fn identity_of(path: &Path) -> Option<FileIdentity> {
    let meta = path.metadata().ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

/// Which file is at `path` now beyond its size and mtime
/// ([`crate::susi_config::file_node`]): a file replaced by another of the
/// same size whose mtime was restored is not the file a verdict was reached
/// for. Empty when it cannot be read.
fn node_of(path: &Path) -> String {
    path.metadata()
        .map(|meta| crate::susi_config::file_node(&meta))
        .unwrap_or_default()
}

fn mtime_ns(modified: SystemTime) -> u128 {
    modified
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos())
}

/// Whether an error from reading a file says something about its bytes (a
/// file shorter than its own header claims, text that is not UTF-8) rather
/// than about the read: only then may the verdict it led to be remembered.
pub(crate) fn decided_by_content(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::InvalidData
    )
}

/// Remembered verdicts, and the file they persist to (none in unit tests,
/// which must not read or write the operator's cache).
struct Verdicts {
    memory: Memory,
    disk: Option<PathBuf>,
    /// [`binary`], the build these verdicts are reached by.
    binary: String,
}

impl Verdicts {
    fn load(disk: Option<PathBuf>) -> Self {
        Self::load_as(disk, binary())
    }

    /// [`Self::load`] for a process whose binary is `binary`. With no
    /// identity there is nothing to tell its verdicts from another build's,
    /// so the file on disk is neither read nor written.
    fn load_as(disk: Option<PathBuf>, binary: &str) -> Self {
        let disk = disk.filter(|_| !binary.is_empty());
        let memory = Memory::new();
        let file = disk
            .as_deref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<VerdictFile>(&bytes).ok())
            .filter(|file| file.format == FORMAT && file.binary == binary);
        for entry in file.map(|f| f.entries).unwrap_or_default() {
            memory.insert((entry.probe.clone(), entry.path.clone()), entry);
        }
        Self {
            memory,
            disk,
            binary: binary.to_string(),
        }
    }

    fn save(&self) {
        let Some(disk) = &self.disk else {
            return;
        };
        let mut entries: Vec<Remembered> = self.memory.iter().map(|e| e.value().clone()).collect();
        entries.sort_by(|a, b| (&a.probe, &a.path).cmp(&(&b.probe, &b.path)));
        let file = VerdictFile {
            format: FORMAT,
            binary: self.binary.clone(),
            entries,
        };
        if let Ok(bytes) = serde_json::to_vec(&file) {
            // Best effort: an unwritable cache only costs the next process
            // the parse.
            let _ = crate::susi_config::atomic_write_bytes(disk, &bytes);
        }
    }

    /// `judge`'s verdict on `path` for `probe`, reused while the file's
    /// identity is `identity`. `judge` answers `None` when an I/O error ended
    /// the judgement: the caller gets `false`, as it always did, and nothing
    /// is remembered. `now` is when the judgement starts.
    fn remembered(
        &self,
        (probe, path): (&str, &Path),
        identity: FileIdentity,
        now: SystemTime,
        judge: impl FnOnce() -> Option<bool>,
    ) -> bool {
        let (len, modified) = identity;
        let key = (probe.to_string(), path.to_path_buf());
        let mtime_ns = mtime_ns(modified);
        let node = node_of(path);
        if let Some(entry) = self.memory.get(&key) {
            if entry.len == len && entry.mtime_ns == mtime_ns && entry.node == node {
                return entry.verdict;
            }
        }
        let Some(verdict) = judge() else {
            return false;
        };
        let settled = now
            .duration_since(modified)
            .is_ok_and(|age| age >= RACY_WINDOW);
        // The verdict is about the file as it was judged: one that changed
        // meanwhile (a download still being written) is asked again.
        if !settled || identity_of(path) != Some(identity) || node_of(path) != node {
            return verdict;
        }
        if self.memory.len() >= MAX_ENTRIES {
            self.memory.clear();
        }
        self.memory.insert(
            key,
            Remembered {
                probe: probe.to_string(),
                path: path.to_path_buf(),
                len,
                mtime_ns,
                node,
                verdict,
            },
        );
        self.save();
        verdict
    }
}

/// `judge`'s verdict on `path` for `probe`, reused while the file's
/// identity is the one it was reached for; `judge` answers `None` when an
/// I/O error ended the judgement, which is reported as `false` and never
/// remembered.
pub(crate) fn remembered(
    probe: &str,
    path: &Path,
    identity: FileIdentity,
    judge: impl FnOnce() -> Option<bool>,
) -> bool {
    static VERDICTS: OnceLock<Verdicts> = OnceLock::new();
    VERDICTS
        .get_or_init(|| {
            Verdicts::load(
                (!cfg!(test))
                    .then(|| susi_paths::SusiDirs::config_dir().join("model_probe.cache.json")),
            )
        })
        .remembered((probe, path), identity, SystemTime::now(), judge)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn scratch(tag: &str) -> PathBuf {
        let nanos = mtime_ns(SystemTime::now());
        let dir = std::env::temp_dir().join(format!(
            "susi-file-verdicts-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn identity(path: &Path) -> FileIdentity {
        identity_of(path).unwrap()
    }

    /// A moment well after every file a test wrote: none of them is racy.
    fn later() -> SystemTime {
        SystemTime::now() + Duration::from_secs(60)
    }

    #[test]
    fn a_verdict_is_reached_once_per_file_identity_and_survives_the_process() {
        let dir = scratch("survives");
        let model = dir.join("m.gguf");
        let disk = dir.join("verdicts.json");
        std::fs::write(&model, b"v1").unwrap();
        let parses = Cell::new(0);
        let ask = |verdicts: &Verdicts| {
            verdicts.remembered(("gguf", &model), identity(&model), later(), || {
                parses.set(parses.get() + 1);
                Some(true)
            })
        };

        let first = Verdicts::load(Some(disk.clone()));
        assert!(ask(&first));
        assert!(ask(&first));
        assert_eq!(parses.get(), 1, "reused within the process");

        // A later process starts from the file on disk.
        assert!(ask(&Verdicts::load(Some(disk.clone()))));
        assert_eq!(parses.get(), 1, "reused across processes");

        // A different verdict on the same file is its own question.
        let tools = first.remembered(("tools", &model), identity(&model), later(), || Some(false));
        assert!(!tools);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_changed_file_is_judged_again() {
        let dir = scratch("changed");
        let model = dir.join("m.gguf");
        let disk = dir.join("verdicts.json");
        std::fs::write(&model, b"truncated").unwrap();
        let first = Verdicts::load(Some(disk.clone()));
        let truncated =
            first.remembered(("gguf", &model), identity(&model), later(), || Some(false));
        assert!(!truncated);
        std::fs::write(&model, b"the whole download").unwrap();
        let later_process = Verdicts::load(Some(disk.clone()));
        let whole =
            later_process.remembered(("gguf", &model), identity(&model), later(), || Some(true));
        assert!(
            whole,
            "a rewritten file is parsed again, not answered from disk"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A file replaced by another of the same size whose modification time
    /// was set back to the old one's (`cp -p`, `rsync -a`, a restore) is
    /// another file: its verdict is reached afresh, not read from disk.
    #[test]
    fn a_replaced_file_with_the_old_size_and_mtime_is_judged_again() {
        let dir = scratch("replaced");
        let model = dir.join("m.gguf");
        let disk = dir.join("verdicts.json");
        std::fs::write(&model, b"truncated v1").unwrap();
        let old = std::fs::metadata(&model).unwrap();
        let key = ("gguf", model.as_path());
        assert!(!Verdicts::load(Some(disk.clone())).remembered(
            key,
            identity(&model),
            later(),
            || Some(false)
        ));
        let staged = dir.join("m.gguf.restored");
        std::fs::write(&staged, b"complete v2!").unwrap();
        let times = std::fs::FileTimes::new().set_modified(old.modified().unwrap());
        // A restore gives the file its own creation time.
        #[cfg(windows)]
        let times = {
            use std::os::windows::fs::FileTimesExt;
            times.set_created(old.created().unwrap() - Duration::from_secs(3600))
        };
        std::fs::File::options()
            .write(true)
            .open(&staged)
            .unwrap()
            .set_times(times)
            .unwrap();
        std::fs::rename(&staged, &model).unwrap();
        assert_eq!(identity(&model), (old.len(), old.modified().unwrap()));
        assert!(
            Verdicts::load(Some(disk.clone()))
                .remembered(key, identity(&model), later(), || Some(true)),
            "the replacement is judged, not answered from the old file's verdict"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A judgement an I/O error ended — the file locked by another process,
    /// or gone mid-read — reports `false` as before, but is remembered
    /// neither in the process nor on disk: the next ask reads the file again
    /// and reaches the verdict its bytes decide.
    #[test]
    fn a_verdict_an_io_error_ended_is_never_remembered() {
        let dir = scratch("io-error");
        let model = dir.join("m.gguf");
        let disk = dir.join("verdicts.json");
        std::fs::write(&model, b"a whole model").unwrap();
        let first = Verdicts::load(Some(disk.clone()));
        let key = ("gguf", model.as_path());
        assert!(!first.remembered(key, identity(&model), later(), || None));
        assert!(!disk.exists(), "an unread verdict was written to disk");
        assert!(
            first.remembered(key, identity(&model), later(), || Some(true)),
            "the process asks again"
        );
        assert!(
            Verdicts::load(Some(disk.clone())).remembered(key, identity(&model), later(), || {
                panic!("the clean verdict is remembered")
            }),
            "the clean verdict is what later processes get"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A file written less than [`RACY_WINDOW`] before it is judged may be
    /// rewritten within the same timestamp tick at the same size, leaving
    /// its identity as it was: its verdict is not remembered.
    #[test]
    fn a_file_written_moments_ago_is_not_remembered() {
        let dir = scratch("racy");
        let model = dir.join("m.gguf");
        let disk = dir.join("verdicts.json");
        std::fs::write(&model, b"version one").unwrap();
        let id = identity(&model);
        let verdicts = Verdicts::load(Some(disk.clone()));
        let just_after = id.1 + Duration::from_millis(1);
        let key = ("gguf", model.as_path());
        assert!(verdicts.remembered(key, id, just_after, || Some(true)));
        assert!(!disk.exists(), "a racy verdict was written to disk");
        assert!(
            !verdicts.remembered(key, id, just_after, || Some(false)),
            "the same identity is judged again while it is racy"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A file that changed while it was judged (a download still being
    /// written) gets the verdict for this ask only.
    #[test]
    fn a_file_that_changed_while_judged_is_not_remembered() {
        let dir = scratch("moving");
        let model = dir.join("m.gguf");
        let disk = dir.join("verdicts.json");
        std::fs::write(&model, b"part").unwrap();
        let before = identity(&model);
        let verdicts = Verdicts::load(Some(disk.clone()));
        let key = ("gguf", model.as_path());
        assert!(!verdicts.remembered(key, before, later(), || {
            std::fs::write(&model, b"partial download, longer now").unwrap();
            Some(false)
        }));
        assert!(!disk.exists(), "a verdict for a moving file was written");
        assert!(verdicts.remembered(key, before, later(), || Some(true)));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_damaged_or_foreign_verdict_file_is_empty() {
        let dir = scratch("damaged");
        let disk = dir.join("verdicts.json");
        std::fs::write(&disk, "{not json").unwrap();
        assert!(Verdicts::load(Some(disk.clone())).memory.is_empty());
        std::fs::write(&disk, r#"{"format":999,"entries":[]}"#).unwrap();
        assert!(Verdicts::load(Some(disk.clone())).memory.is_empty());

        // Verdicts another build reached are its own: its parsers may differ.
        let model = dir.join("m.gguf");
        std::fs::write(&model, b"v1").unwrap();
        let ours = Verdicts::load(Some(disk.clone()));
        assert!(ours.remembered(("gguf", &model), identity(&model), later(), || Some(true)));
        assert_eq!(Verdicts::load(Some(disk.clone())).memory.len(), 1);
        let text = std::fs::read_to_string(&disk).unwrap();
        let theirs = text.replace(&binary().replace('\\', "\\\\"), "another build");
        assert_ne!(text, theirs);
        std::fs::write(&disk, theirs).unwrap();
        assert!(Verdicts::load(Some(disk.clone())).memory.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A process that cannot name its own binary cannot tell its verdicts
    /// from another build's: it keeps them in memory only, and neither reads
    /// nor writes the file on disk.
    #[test]
    fn a_process_without_a_binary_identity_keeps_nothing_on_disk() {
        let dir = scratch("no-identity");
        let model = dir.join("m.gguf");
        let disk = dir.join("verdicts.json");
        std::fs::write(&model, b"v1").unwrap();
        let key = ("gguf", model.as_path());
        let anonymous = Verdicts::load_as(Some(disk.clone()), "");
        assert!(anonymous.remembered(key, identity(&model), later(), || Some(true)));
        assert!(!disk.exists(), "an untagged verdict was written to disk");
        assert!(
            anonymous.remembered(key, identity(&model), later(), || panic!("asked again")),
            "kept in the process"
        );
        // Nor does it read what an untagged writer might have left.
        let tagged = Verdicts::load_as(Some(disk.clone()), "some build");
        assert!(tagged.remembered(key, identity(&model), later(), || Some(true)));
        let text = std::fs::read_to_string(&disk).unwrap();
        std::fs::write(&disk, text.replace("some build", "")).unwrap();
        assert!(Verdicts::load_as(Some(disk.clone()), "").memory.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn only_content_errors_decide_a_verdict() {
        use std::io::{Error, ErrorKind};
        assert!(decided_by_content(&Error::from(ErrorKind::UnexpectedEof)));
        assert!(decided_by_content(&Error::from(ErrorKind::InvalidData)));
        for kind in [
            ErrorKind::NotFound,
            ErrorKind::PermissionDenied,
            ErrorKind::Interrupted,
            ErrorKind::Other,
        ] {
            assert!(!decided_by_content(&Error::from(kind)), "{kind:?}");
        }
    }
}
