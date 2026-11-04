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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn unique_temp_dir(label: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("susi-key-rot-{label}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn key_ages_load_save_stamp_and_key_statuses_rotation_policy() {
        let dir = unique_temp_dir("ages-status");
        let ages_path = KeyAges::path_in(&dir);

        let mut ages = KeyAges::load(&ages_path).expect("load missing key-ages.json");
        assert!(ages.set_unix.is_empty());

        let now_unix = 200 * 86_400;
        let fresh_set = now_unix - (DEFAULT_MAX_AGE_DAYS * 86_400);
        let expired_set = now_unix - ((DEFAULT_MAX_AGE_DAYS + 5) * 86_400);

        ages.stamp("FRESH_API_KEY", fresh_set);
        ages.stamp("EXPIRED_API_KEY", expired_set);
        ages.save(&ages_path).expect("save key-ages.json");

        let loaded = KeyAges::load(&ages_path).expect("reload key-ages.json");
        assert_eq!(
            loaded.age_days("FRESH_API_KEY", now_unix),
            Some(DEFAULT_MAX_AGE_DAYS)
        );
        assert_eq!(
            loaded.age_days("EXPIRED_API_KEY", now_unix),
            Some(DEFAULT_MAX_AGE_DAYS + 5)
        );
        assert_eq!(loaded.age_days("UNSTAMPED_API_KEY", now_unix), None);
        // Saturating sub when now_unix < set_unix returns 0 days.
        assert_eq!(loaded.age_days("FRESH_API_KEY", fresh_set - 100), Some(0));

        let env_text = "\
FRESH_API_KEY=secret_fresh_val_123
EXPIRED_API_KEY=secret_expired_val_456
UNSTAMPED_API_KEY=secret_unstamped_val_789
";
        let statuses = key_statuses(env_text, &loaded, now_unix, DEFAULT_MAX_AGE_DAYS);
        assert_eq!(
            statuses,
            vec![
                KeyStatus {
                    key: "FRESH_API_KEY".to_string(),
                    set_unix: Some(fresh_set),
                    age_days: Some(90),
                    needs_rotation: false,
                },
                KeyStatus {
                    key: "EXPIRED_API_KEY".to_string(),
                    set_unix: Some(expired_set),
                    age_days: Some(95),
                    needs_rotation: true,
                },
                KeyStatus {
                    key: "UNSTAMPED_API_KEY".to_string(),
                    set_unix: None,
                    age_days: None,
                    needs_rotation: false,
                },
            ]
        );

        let rendered = format_keys_list(&statuses);
        assert!(
            rendered.contains("EXPIRED_API_KEY  set 95d ago  <- ROTATE (key older than policy)\n")
        );
        assert!(rendered.contains("FRESH_API_KEY  set 90d ago\n"));
        assert!(rendered.contains("UNSTAMPED_API_KEY  set ?d ago\n"));
        assert_eq!(
            rendered
                .matches("<- ROTATE (key older than policy)")
                .count(),
            1
        );
        assert!(!rendered.contains("secret_fresh_val_123"));
        assert!(!rendered.contains("secret_expired_val_456"));
        assert!(!rendered.contains("secret_unstamped_val_789"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotate_key_updates_target_preserves_other_lines_stamps_age_and_clears_quarantine() {
        let dir = unique_temp_dir("rotate");
        let env_path = dir.join("cloud.env");
        let ages_path = KeyAges::path_in(&dir);
        let q_path = quarantine_path_in(&dir, "openai");

        std::fs::create_dir_all(q_path.parent().expect("quarantine parent"))
            .expect("create quarantine dir");
        std::fs::write(&q_path, b"1").expect("write quarantine flag");
        assert!(q_path.exists());

        let initial_env =
            "# Managed cloud keys\nexport OPENAI_API_KEY=old_val\nANTHROPIC_API_KEY=keep_me\n";
        std::fs::write(&env_path, initial_env).expect("write initial cloud.env");

        rotate_key(&RotateKey {
            env_path: &env_path,
            ages_path: &ages_path,
            config_dir: &dir,
            key: "OPENAI_API_KEY",
            vendor: "openai",
            new_value: "rotated_val_999",
            now_unix: 1_700_000_000,
        })
        .expect("rotate OPENAI_API_KEY");

        let updated_env = std::fs::read_to_string(&env_path).expect("read updated cloud.env");
        assert_eq!(
            updated_env,
            "# Managed cloud keys\nOPENAI_API_KEY=rotated_val_999\nANTHROPIC_API_KEY=keep_me\n"
        );

        let ages = KeyAges::load(&ages_path).expect("load key-ages.json after rotate");
        assert_eq!(ages.set_unix.get("OPENAI_API_KEY"), Some(&1_700_000_000));
        assert!(
            !q_path.exists(),
            "quarantine flag should be cleared on rotation"
        );

        let err = rotate_key(&RotateKey {
            env_path: &env_path,
            ages_path: &ages_path,
            config_dir: &dir,
            key: "MISSING_KEY",
            vendor: "other",
            new_value: "unused",
            now_unix: 1_700_000_100,
        })
        .expect_err("rotating absent key must fail");
        assert!(err.to_string().contains("MISSING_KEY is not set"));

        let after_err_env = std::fs::read_to_string(&env_path).expect("read cloud.env after err");
        assert_eq!(after_err_env, updated_env);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_keys_list_export_prefix_parsing_and_corrupt_ages_load_fail_cleanly() {
        assert_eq!(format_keys_list(&[]), "");

        let dir = unique_temp_dir("corrupt-and-export");
        let ages_path = KeyAges::path_in(&dir);
        std::fs::write(&ages_path, b"{not valid json").expect("write corrupt key-ages.json");
        assert!(KeyAges::load(&ages_path).is_err());

        let mut ages = KeyAges::default();
        let now_unix = 100 * 86_400;
        ages.stamp("GEMINI_API_KEY", now_unix - 10 * 86_400);

        let env_text =
            "\n# leading comment\n   \nexport GEMINI_API_KEY=ai-secret-value\n# trailing comment\n";
        let statuses = key_statuses(env_text, &ages, now_unix, DEFAULT_MAX_AGE_DAYS);
        assert_eq!(
            statuses,
            vec![KeyStatus {
                key: "GEMINI_API_KEY".to_string(),
                set_unix: Some(now_unix - 10 * 86_400),
                age_days: Some(10),
                needs_rotation: false,
            }]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotate_key_succeeds_without_quarantine_flag_and_boundary_age_equals_max_age_is_not_flagged()
    {
        let dir = unique_temp_dir("no-quarantine-and-boundary");
        let env_path = dir.join("cloud.env");
        let ages_path = KeyAges::path_in(&dir);
        assert!(!quarantine_path_in(&dir, "groq").exists());

        std::fs::write(&env_path, "GROQ_API_KEY=initial_groq_key\n").expect("write cloud.env");
        rotate_key(&RotateKey {
            env_path: &env_path,
            ages_path: &ages_path,
            config_dir: &dir,
            key: "GROQ_API_KEY",
            vendor: "groq",
            new_value: "updated_groq_key",
            now_unix: 100 * 86_400,
        })
        .expect("rotate_key without quarantine flag");

        assert_eq!(
            std::fs::read_to_string(&env_path).expect("read cloud.env"),
            "GROQ_API_KEY=updated_groq_key\n"
        );
        let mut ages = KeyAges::load(&ages_path).expect("load key-ages.json");
        assert_eq!(ages.set_unix.get("GROQ_API_KEY"), Some(&(100 * 86_400)));

        let now_unix = 200 * 86_400;
        ages.stamp("EXACT_BOUNDARY_KEY", now_unix - 30 * 86_400);
        ages.stamp("PAST_BOUNDARY_KEY", now_unix - 31 * 86_400);
        let statuses = key_statuses(
            "EXACT_BOUNDARY_KEY=val1\nPAST_BOUNDARY_KEY=val2\n",
            &ages,
            now_unix,
            30,
        );
        assert_eq!(statuses.len(), 2);
        assert_eq!(statuses[0].age_days, Some(30));
        assert!(!statuses[0].needs_rotation);
        assert_eq!(statuses[1].age_days, Some(31));
        assert!(statuses[1].needs_rotation);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
