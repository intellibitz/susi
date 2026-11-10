//! Pure-std bounded non-blocking daily rolling writer for `audit.log`,
//! replacing `susi-vendor-tracing-appender` (`tracing-appender`,
//! `crossbeam-channel`, `crossbeam-utils`, `thiserror`, `thiserror-impl`).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, SyncSender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const CHANNEL_CAPACITY: usize = 4096;

enum AuditMsg {
    Record(Vec<u8>),
    Shutdown,
}

#[derive(Clone)]
pub(crate) struct AuditWriterFactory {
    tx: Arc<SyncSender<AuditMsg>>,
}

pub(crate) struct AuditEventWriter {
    buf: Vec<u8>,
    tx: Arc<SyncSender<AuditMsg>>,
}

pub(crate) struct AuditWorkerGuard {
    tx: Arc<SyncSender<AuditMsg>>,
    handle: Option<JoinHandle<()>>,
}

impl AuditWriterFactory {
    pub(crate) fn writer(&self) -> AuditEventWriter {
        AuditEventWriter {
            buf: Vec::with_capacity(256),
            tx: Arc::clone(&self.tx),
        }
    }
}

impl Write for AuditEventWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.buf.is_empty() {
            let payload = std::mem::take(&mut self.buf);
            let _ = self.tx.try_send(AuditMsg::Record(payload));
        }
        Ok(())
    }
}

impl Drop for AuditEventWriter {
    fn drop(&mut self) {
        if !self.buf.is_empty() {
            let payload = std::mem::take(&mut self.buf);
            let _ = self.tx.try_send(AuditMsg::Record(payload));
        }
    }
}

impl Drop for AuditWorkerGuard {
    fn drop(&mut self) {
        let _ = self.tx.send(AuditMsg::Shutdown);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub(crate) fn spawn_daily_audit_writer(
    dir: &Path,
    prefix: &str,
    suffix: &str,
    max_files: usize,
) -> (AuditWriterFactory, AuditWorkerGuard) {
    let (tx, rx) = mpsc::sync_channel::<AuditMsg>(CHANNEL_CAPACITY);
    let tx = Arc::new(tx);
    let dir = dir.to_path_buf();
    let prefix = prefix.to_string();
    let suffix = suffix.to_string();
    let keep = max_files.max(1);

    let handle = thread::Builder::new()
        .name("susi-audit-log".into())
        .spawn(move || {
            let mut state = RollingFileState::new(dir, prefix, suffix, keep);
            while let Ok(msg) = rx.recv() {
                match msg {
                    AuditMsg::Record(bytes) => state.write_record(&bytes),
                    AuditMsg::Shutdown => {
                        while let Ok(AuditMsg::Record(bytes)) = rx.try_recv() {
                            state.write_record(&bytes);
                        }
                        state.flush();
                        break;
                    }
                }
            }
        })
        .ok();

    (
        AuditWriterFactory {
            tx: Arc::clone(&tx),
        },
        AuditWorkerGuard { tx, handle },
    )
}

struct RollingFileState {
    dir: PathBuf,
    prefix: String,
    suffix: String,
    max_files: usize,
    current_day: String,
    file: Option<File>,
}

impl RollingFileState {
    fn new(dir: PathBuf, prefix: String, suffix: String, max_files: usize) -> Self {
        let mut s = Self {
            dir,
            prefix,
            suffix,
            max_files,
            current_day: String::new(),
            file: None,
        };
        let today = utc_date_ymd(SystemTime::now());
        s.rotate_if_needed(&today);
        s
    }

    fn rotate_if_needed(&mut self, day: &str) {
        if self.current_day == day && self.file.is_some() {
            return;
        }
        let _ = fs::create_dir_all(&self.dir);
        let dated_name = format!("{}.{day}.{}", self.prefix, self.suffix);
        let opened = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join(dated_name))
            .or_else(|_| {
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(self.dir.join(format!("{}.{}", self.prefix, self.suffix)))
            })
            .ok();
        self.current_day = day.to_string();
        self.file = opened;
        prune_rotated_files(&self.dir, &self.prefix, &self.suffix, self.max_files);
    }

    fn write_record(&mut self, bytes: &[u8]) {
        let today = utc_date_ymd(SystemTime::now());
        self.rotate_if_needed(&today);
        if let Some(file) = self.file.as_mut() {
            let _ = file.write_all(bytes);
        }
    }

    fn flush(&mut self) {
        if let Some(file) = self.file.as_mut() {
            let _ = file.flush();
        }
    }
}

fn prune_rotated_files(dir: &Path, prefix: &str, suffix: &str, max_files: usize) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let head = format!("{prefix}.");
    let tail = format!(".{suffix}");
    let mut matched: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let date_part = name.strip_prefix(&head)?.strip_suffix(&tail)?;
            if is_iso_date(date_part) && e.file_type().ok()?.is_file() {
                Some((name, e.path()))
            } else {
                None
            }
        })
        .collect();
    matched.sort_by(|a, b| a.0.cmp(&b.0));
    if matched.len() > max_files {
        let remove_count = matched.len() - max_files;
        for (_, path) in matched.into_iter().take(remove_count) {
            let _ = fs::remove_file(path);
        }
    }
}

fn is_iso_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b[0..4].iter().all(u8::is_ascii_digit)
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[8..10].iter().all(u8::is_ascii_digit)
}

/// Howard Hinnant civil-from-days UTC `YYYY-MM-DD`.
fn utc_date_ymd(now: SystemTime) -> String {
    let secs = now
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    let days = (secs / 86_400) as i64;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    format!("{year:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_date_formats_epoch_and_leap_day() {
        assert_eq!(utc_date_ymd(UNIX_EPOCH), "1970-01-01");
        assert_eq!(
            utc_date_ymd(UNIX_EPOCH + Duration::from_secs(1_709_164_800)),
            "2024-02-29"
        );
    }

    #[test]
    fn writer_flushes_records_on_guard_drop_and_prunes_beyond_max_files() {
        let dir = std::env::temp_dir().join(format!(
            "susi-audit-writer-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).expect("temp dir");
        for day in 1..=9 {
            let p = dir.join(format!("audit.2026-10-{day:02}.log"));
            fs::write(p, b"old\n").expect("seed old file");
        }
        fs::write(dir.join("unrelated.txt"), b"keep").expect("seed unrelated");

        let (factory, guard) = spawn_daily_audit_writer(&dir, "audit", "log", 7);
        {
            let mut w = factory.writer();
            w.write_all(b"{\"event\":\"one\"}\n").expect("write 1");
        }
        {
            let mut w = factory.writer();
            w.write_all(b"{\"event\":\"two\"}\n").expect("write 2");
        }
        drop(guard);

        assert!(dir.join("unrelated.txt").exists());
        let rotated: Vec<String> = fs::read_dir(&dir)
            .expect("read dir")
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.starts_with("audit.") && n.ends_with(".log"))
            .collect();
        assert_eq!(rotated.len(), 7, "rotated files: {rotated:?}");

        let today = utc_date_ymd(SystemTime::now());
        let content =
            fs::read_to_string(dir.join(format!("audit.{today}.log"))).expect("today log");
        assert!(content.contains("{\"event\":\"one\"}"));
        assert!(content.contains("{\"event\":\"two\"}"));

        let _ = fs::remove_dir_all(&dir);
    }
}
