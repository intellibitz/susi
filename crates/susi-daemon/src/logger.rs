//! Centralized Rolling Logger (Swarm OS Bullet 7)
//!
//! A unified logging architecture that captures cell stdout, stderr, and trace-level
//! events to a centralized highly compressed (or standard) rotating sink.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const FILE_PREFIX: &str = "swarm_";
const FILE_SUFFIX: &str = ".log";

/// Configuration for the rotating logger.
#[derive(Debug, Clone)]
pub struct LoggerConfig {
    pub max_file_size_bytes: u64,
    pub max_files: usize,
    pub log_dir: PathBuf,
}

impl Default for LoggerConfig {
    fn default() -> Self {
        Self {
            max_file_size_bytes: 10 * 1024 * 1024, // 10 MiB
            max_files: 5,
            log_dir: std::env::temp_dir().join("susi_logs"),
        }
    }
}

/// A centralized sink for cell logs.
pub struct RollingLogger {
    config: LoggerConfig,
    current_file: Mutex<Option<fs::File>>,
    current_size: Mutex<u64>,
    /// Distinguishes rotations that land in the same wall-clock millisecond
    /// (e.g. a test forcing rapid rotation, or a burst of large entries) so
    /// they never collide on the same file name.
    rotation_seq: AtomicU64,
}

impl RollingLogger {
    pub fn new(config: LoggerConfig) -> std::io::Result<Self> {
        if !config.log_dir.exists() {
            fs::create_dir_all(&config.log_dir)?;
        }

        let logger = Self {
            config,
            current_file: Mutex::new(None),
            current_size: Mutex::new(0),
            rotation_seq: AtomicU64::new(0),
        };

        logger.rotate()?;
        Ok(logger)
    }

    /// Rotates the log file when the maximum size is reached, then prunes
    /// the oldest rotated files beyond `max_files`.
    fn rotate(&self) -> std::io::Result<()> {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let seq = self.rotation_seq.fetch_add(1, Ordering::AcqRel);
        let file_path = self
            .config
            .log_dir
            .join(format!("{FILE_PREFIX}{ts}_{seq}{FILE_SUFFIX}"));

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file_path)?;

        let mut file_guard = self.current_file.lock().unwrap_or_else(|e| e.into_inner());
        *file_guard = Some(file);

        let mut size_guard = self.current_size.lock().unwrap_or_else(|e| e.into_inner());
        *size_guard = 0;

        self.prune_old_files();
        Ok(())
    }

    /// Deletes the oldest rotated log files beyond `config.max_files`.
    /// Millisecond-epoch timestamps stay a fixed digit width for centuries,
    /// so a lexicographic sort of `swarm_<ts>_<seq>.log` names is also a
    /// time-ascending sort.
    fn prune_old_files(&self) {
        let Ok(entries) = fs::read_dir(&self.config.log_dir) else {
            return;
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(FILE_PREFIX) && n.ends_with(FILE_SUFFIX))
            })
            .collect();
        if files.len() <= self.config.max_files {
            return;
        }
        files.sort();
        for stale in &files[..files.len() - self.config.max_files] {
            let _ = fs::remove_file(stale);
        }
    }

    /// Writes a log entry to the sink.
    pub fn log(&self, cell_id: &str, level: &str, message: &str) -> std::io::Result<()> {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let entry = format!("[{}] [{}] [{}]: {}\n", ts, cell_id, level, message);

        let mut file_guard = self.current_file.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(file) = file_guard.as_mut() {
            let bytes = entry.as_bytes();
            file.write_all(bytes)?;

            let mut size_guard = self.current_size.lock().unwrap_or_else(|e| e.into_inner());
            *size_guard += bytes.len() as u64;

            if *size_guard >= self.config.max_file_size_bytes {
                // Drop the locks before calling rotate
                drop(file_guard);
                drop(size_guard);
                self.rotate()?;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rolling_logger() {
        let config = LoggerConfig {
            max_file_size_bytes: 50, // very small to force rotation
            max_files: 2,
            log_dir: std::env::temp_dir().join(format!("susi_logs_test_{}", std::process::id())),
        };

        let logger = RollingLogger::new(config.clone()).unwrap();

        // Write enough to trigger rotation
        logger.log("cell-1", "INFO", "First message").unwrap();
        logger.log("cell-1", "INFO", "Second message").unwrap(); // This should trigger rotation
        logger.log("cell-1", "INFO", "Third message").unwrap();

        // Verify files exist. Deterministic now that rotation file names
        // include a monotonic sequence number: two rotations always
        // produce two distinct files, even within the same millisecond.
        let entries = fs::read_dir(&config.log_dir).unwrap().count();
        assert_eq!(entries, 2);

        fs::remove_dir_all(&config.log_dir).unwrap();
    }

    #[test]
    fn old_rotations_are_pruned_beyond_max_files() {
        let config = LoggerConfig {
            max_file_size_bytes: 10, // rotate on almost every write
            max_files: 2,
            log_dir: std::env::temp_dir()
                .join(format!("susi_logs_prune_test_{}", std::process::id())),
        };

        let logger = RollingLogger::new(config.clone()).unwrap();
        for i in 0..10 {
            logger
                .log("cell-1", "INFO", &format!("message {i}"))
                .unwrap();
        }

        let entries = fs::read_dir(&config.log_dir).unwrap().count();
        assert_eq!(entries, config.max_files);

        fs::remove_dir_all(&config.log_dir).unwrap();
    }
}
