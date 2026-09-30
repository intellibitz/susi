//! Key age tracking and rotation reminders.
//!
//! Every key write stamps `key-ages.json` next to `cloud.env`; `susi keys
//! list` flags entries older than a configurable age; `susi keys rotate
//! <vendor>` swaps the value atomically (write tmp + rename) and clears
//! the failure quarantine the vendor's repeated 401s may have set.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::cloud_env::parse_env_file;
use crate::susi_error::{EaiError, EaiResult};

/// Default age after which a key is flagged for rotation (90 days).
pub const DEFAULT_MAX_AGE_DAYS: u64 = 90;

/// When each registered key was set: env-var name → unix seconds.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct KeyAges {
    #[serde(default)]
    pub set_unix: BTreeMap<String, u64>,
}

impl KeyAges {
    #[must_use]
    pub fn path_in(config_dir: &Path) -> PathBuf {
        config_dir.join("key-ages.json")
    }

    /// Load; missing file is an empty ledger.
    ///
    /// # Errors
    /// [`EaiError::io`] on unreadable/corrupt files.
    pub fn load(path: &Path) -> EaiResult<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| EaiError::io(e.to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(EaiError::io(e.to_string())),
        }
    }

    /// # Errors
    /// [`EaiError::io`] on encode/write failures.
    pub fn save(&self, path: &Path) -> EaiResult<()> {
        let text = serde_json::to_string_pretty(self).map_err(|e| EaiError::io(e.to_string()))?;
        std::fs::write(path, text).map_err(|e| EaiError::io(e.to_string()))
    }

    /// Stamp `key` as set at `now_unix`.
    pub fn stamp(&mut self, key: &str, now_unix: u64) {
        self.set_unix.insert(key.to_string(), now_unix);
    }

    /// Age in whole days; `None` when the key was never stamped.
    #[must_use]
    pub fn age_days(&self, key: &str, now_unix: u64) -> Option<u64> {
        self.set_unix
            .get(key)
            .map(|set| now_unix.saturating_sub(*set) / 86_400)
    }
}

/// One row of `susi keys list`: the var, its age, whether it needs
/// rotation. The secret value is never surfaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyStatus {
    pub key: String,
    pub set_unix: Option<u64>,
    pub age_days: Option<u64>,
    pub needs_rotation: bool,
}

/// Report each key present in `env_text` with its recorded age, flagging
/// those past `max_age_days`.
#[must_use]
pub fn key_statuses(
    env_text: &str,
    ages: &KeyAges,
    now_unix: u64,
    max_age_days: u64,
) -> Vec<KeyStatus> {
    parse_env_file(env_text)
        .into_iter()
        .map(|(key, _)| {
            let age = ages.age_days(&key, now_unix);
            KeyStatus {
                set_unix: ages.set_unix.get(&key).copied(),
                age_days: age,
                needs_rotation: age.is_some_and(|d| d > max_age_days),
                key,
            }
        })
        .collect()
}

/// Render `susi keys list` output — ages + rotation hints, no values.
#[must_use]
pub fn format_keys_list(statuses: &[KeyStatus]) -> String {
    let mut out = String::new();
    for s in statuses {
        let age = s
            .age_days
            .map_or_else(|| "?".to_string(), |d| d.to_string());
        let flag = if s.needs_rotation {
            "  <- ROTATE (key older than policy)"
        } else {
            ""
        };
        out.push_str(&format!("{}  set {}d ago{flag}\n", s.key, age));
    }
    out
}

/// Quarantine flag written when a vendor repeatedly rejects a key; a
/// successful rotation clears it.
#[must_use]
pub fn quarantine_path_in(config_dir: &Path, vendor: &str) -> PathBuf {
    config_dir
        .join("key-quarantine")
        .join(format!("{vendor}.flag"))
}

/// Everything a rotation needs, grouped so the call reads like a request
/// rather than seven positional arguments.
pub struct RotateKey<'a> {
    /// The env file holding the key (`cloud.env`).
    pub env_path: &'a Path,
    /// `key-ages.json` ledger path.
    pub ages_path: &'a Path,
    /// Config dir holding the `key-quarantine/` flags.
    pub config_dir: &'a Path,
    /// Env-var name to rotate.
    pub key: &'a str,
    /// Vendor the key belongs to (clears its quarantine flag).
    pub vendor: &'a str,
    /// Replacement secret value.
    pub new_value: &'a str,
    /// Stamp for the new age entry.
    pub now_unix: u64,
}

/// Rotate `req.key` to `req.new_value` inside `req.env_path`, keeping
/// every other line byte-identical. Atomic: the new file replaces the old
/// via rename; the age stamp updates and `req.vendor`'s quarantine flag is
/// removed.
///
/// # Errors
/// [`EaiError::config`] when the key is absent; [`EaiError::io`] on I/O.
pub fn rotate_key(req: &RotateKey<'_>) -> EaiResult<()> {
    let text = std::fs::read_to_string(req.env_path).unwrap_or_default();
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let mut replaced = false;
    for line in &mut lines {
        let probe = line.trim().strip_prefix("export ").unwrap_or(line.trim());
        if let Some((name, _)) = probe.split_once('=') {
            if name.trim() == req.key {
                *line = format!("{}={}", req.key, req.new_value);
                replaced = true;
            }
        }
    }
    if !replaced {
        return Err(EaiError::config(format!(
            "{} is not set in {}",
            req.key,
            req.env_path.display()
        )));
    }
    let tmp = req.env_path.with_extension("env.tmp");
    std::fs::write(&tmp, lines.join("\n") + "\n").map_err(|e| EaiError::io(e.to_string()))?;
    std::fs::rename(&tmp, req.env_path).map_err(|e| EaiError::io(e.to_string()))?;

    let mut ages = KeyAges::load(req.ages_path)?;
    ages.stamp(req.key, req.now_unix);
    ages.save(req.ages_path)?;

    let q = quarantine_path_in(req.config_dir, req.vendor);
    if q.exists() {
        std::fs::remove_file(&q).map_err(|e| EaiError::io(e.to_string()))?;
    }
    Ok(())
}
