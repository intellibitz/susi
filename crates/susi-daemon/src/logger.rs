//! Centralized Rolling Logger (Swarm OS Bullet 7)
//!
//! A unified logging architecture that captures cell stdout, stderr, and trace-level
//! events to a centralized highly compressed (or standard) rotating sink.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

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
        };

        logger.rotate()?;
        Ok(logger)
    }

    /// Rotates the log file when the maximum size is reached.
    fn rotate(&self) -> std::io::Result<()> {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let file_path = self.config.log_dir.join(format!("swarm_{}.log", ts));

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file_path)?;

        let mut file_guard = self.current_file.lock().unwrap_or_else(|e| e.into_inner());
        *file_guard = Some(file);

        let mut size_guard = self.current_size.lock().unwrap_or_else(|e| e.into_inner());
        *size_guard = 0;

        // In a full implementation, we would also clean up old files > max_files
        Ok(())
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

        // Verify files exist
        let entries = fs::read_dir(&config.log_dir).unwrap().count();
        assert!(entries >= 2); // Should have at least 2 log files now

        fs::remove_dir_all(&config.log_dir).unwrap();
    }
}
