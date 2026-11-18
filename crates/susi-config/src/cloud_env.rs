//! `cloud.env`: the host's registered cloud API keys (dotenv format, 0600).
//!
//! Keys are *read* from the file on demand — never copied into the process
//! environment. `std::env::set_var` is unsound once other threads may read
//! the environment (a daemon always has such threads), so in-process lookups
//! go through [`env_or_cloud_env`] and child processes receive the keys
//! explicitly via [`cloud_env_overlay`].

use std::path::{Path, PathBuf};

/// Location of the host key file.
pub fn cloud_env_path() -> PathBuf {
    susi_paths::SusiDirs::config_dir().join("cloud.env")
}

/// Decode a nonempty value from one line of `cloud.env`. A leading quote
/// must close on that line, so an incomplete setting cannot become a key.
pub fn parse_env_value(raw: &str) -> Option<&str> {
    let raw = raw.trim();
    let value = match raw.chars().next()? {
        quote @ ('\'' | '"') => raw.strip_prefix(quote)?.strip_suffix(quote)?,
        _ => raw,
    };
    (!value.is_empty()).then_some(value)
}

/// Parse a dotenv-style file into key/value pairs (no side effects).
pub fn parse_env_file(content: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || key.contains(char::is_whitespace) {
            continue;
        }
        if let Some(value) = parse_env_value(value) {
            out.push((key.to_string(), value.to_string()));
        }
    }
    out
}

/// Load registered cloud entries from `home`: plaintext `cloud.env` (mtime-cached)
/// merged with any unlocked `vault.sealed.json` `"env"`/`"cloud"` entries (sealed wins).
#[must_use]
pub fn load_cloud_env_from(home: &Path) -> Vec<(String, String)> {
    let mut entries = super::cluster_key::cached_file_bytes(&home.join("cloud.env"))
        .and_then(|raw| String::from_utf8(raw).ok())
        .map(|text| parse_env_file(&text))
        .unwrap_or_default();
    let sealed = super::vault_store::resolve_cloud_env_map(home);
    if sealed.is_empty() {
        return entries;
    }
    let mut merged: std::collections::BTreeMap<String, String> = entries.drain(..).collect();
    for (k, v) in sealed {
        merged.insert(k, v);
    }
    merged.into_iter().collect()
}

/// Current `cloud.env` entries (mtime-cached read; empty when absent).
fn cloud_env_entries() -> Vec<(String, String)> {
    load_cloud_env_from(&susi_paths::SusiDirs::config_dir())
}

/// `std::env::var` with `cloud.env` as the fallback: a non-empty process
/// variable wins; otherwise the registered key, if any; otherwise the
/// original `env::var` result. Same signature, so readers convert 1:1.
pub fn env_or_cloud_env(name: &str) -> Result<String, std::env::VarError> {
    match std::env::var(name) {
        Ok(value) if !value.is_empty() => Ok(value),
        other => cloud_env_entries()
            .into_iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value)
            .map_or(other, Ok),
    }
}

/// Filter `entries` to keys where `!is_already_set(key)`.
#[must_use]
pub fn cloud_env_overlay_from<F>(
    entries: Vec<(String, String)>,
    is_already_set: F,
) -> Vec<(String, String)>
where
    F: Fn(&str) -> bool,
{
    entries
        .into_iter()
        .filter(|(key, _)| !is_already_set(key))
        .collect()
}

/// Registered keys the process environment does not already set — pass to
/// `Command::envs` so spawned agents see the same keys lookups do.
pub fn cloud_env_overlay() -> Vec<(String, String)> {
    cloud_env_overlay_from(cloud_env_entries(), |key| {
        std::env::var_os(key).is_some_and(|v| !v.is_empty())
    })
}

#[cfg(test)]
mod malformed_quote_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn unterminated_quoted_values_do_not_hide_following_settings() {
        let parsed =
            parse_env_file("A='unfinished\nB=\"unfinished'\nC='valid'\nD=\"é\"\nE=plain'\n");
        assert_eq!(
            parsed,
            [
                ("C".into(), "valid".into()),
                ("D".into(), "é".into()),
                ("E".into(), "plain'".into())
            ]
        );
    }

    #[test]
    fn lone_quote_is_empty_and_following_entries_survive() {
        assert_eq!(
            parse_env_file("FIRST='\nSECOND=\"\nTHIRD='valid'\nFOURTH=\"é\""),
            [
                ("THIRD".into(), "valid".into()),
                ("FOURTH".into(), "é".into())
            ]
        );
    }

    #[test]
    fn parse_env_file_handles_comments_export_quotes_and_rejects_malformed_keys_or_empty_values() {
        let content = "\
            # comment line\n\
            \n\
            export OPENAI_API_KEY=\"sk-openai-123\"\n\
            ANTHROPIC_API_KEY = 'sk-ant-456'\n\
            GEMINI_API_KEY=ai-gem-789\n\
            =missing_key\n\
            BAD KEY=val\n\
            EMPTY_VAL=\n\
            EMPTY_QUOTED=\"\"\n\
            NO_EQUALS_LINE\n";
        assert_eq!(
            parse_env_file(content),
            vec![
                ("OPENAI_API_KEY".into(), "sk-openai-123".into()),
                ("ANTHROPIC_API_KEY".into(), "sk-ant-456".into()),
                ("GEMINI_API_KEY".into(), "ai-gem-789".into()),
            ]
        );
    }

    #[test]
    fn cloud_env_overlay_from_and_load_cloud_env_from_filter_and_load_deterministically() {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "susi_cloud_env_{}_{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();

        // Absent cloud.env returns empty Vec.
        assert!(load_cloud_env_from(&dir).is_empty());

        std::fs::write(
            dir.join("cloud.env"),
            "export OPENAI_API_KEY='sk-1'\nGEMINI_API_KEY=\"gm-2\"\n",
        )
        .unwrap();
        let loaded = load_cloud_env_from(&dir);
        assert_eq!(
            loaded,
            vec![
                ("OPENAI_API_KEY".into(), "sk-1".into()),
                ("GEMINI_API_KEY".into(), "gm-2".into()),
            ]
        );

        let filtered = cloud_env_overlay_from(loaded, |k| k == "OPENAI_API_KEY");
        assert_eq!(filtered, vec![("GEMINI_API_KEY".into(), "gm-2".into())]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn env_or_cloud_env_prefers_non_empty_process_env_and_cloud_env_path_ends_with_cloud_env() {
        assert_eq!(
            cloud_env_path().file_name().and_then(|n| n.to_str()),
            Some("cloud.env")
        );

        let _guard = crate::env_test_lock();
        let key = "SUSI_CLOUD_ENV_UNIT_TEST_PROBE_1104";
        std::env::set_var(key, "from-process-env");
        assert_eq!(env_or_cloud_env(key).as_deref(), Ok("from-process-env"));

        std::env::set_var(key, "");
        assert_eq!(env_or_cloud_env(key).as_deref(), Ok(""));

        std::env::remove_var(key);
        assert_eq!(env_or_cloud_env(key), Err(std::env::VarError::NotPresent));
    }

    #[test]
    fn parse_env_file_ignores_empty_comment_and_blank_value_lines() {
        assert!(parse_env_file("").is_empty());
        assert!(parse_env_file("# comment\n  \nNO_EQUALS_HERE\nEMPTY_KEY=\n").is_empty());
        assert_eq!(
            parse_env_file("VALID_KEY=ok\n"),
            vec![("VALID_KEY".into(), "ok".into())]
        );
    }

    #[test]
    fn load_cloud_env_from_merges_unlocked_sealed_vault_over_plaintext() {
        let _guard = crate::env_test_lock();
        let dir =
            std::env::temp_dir().join(format!("susi_cloud_env_sealed_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let prev = std::env::var_os(super::super::vault_store::VAULT_KEY_ENV);
        std::env::remove_var(super::super::vault_store::VAULT_KEY_ENV);

        super::super::vault_store::enroll_master_key(&dir, Some("keyfile"), None).unwrap();
        super::super::vault_store::try_store_cloud_key(&dir, "OPENAI_API_KEY", "sk-sealed-wins")
            .unwrap();
        std::fs::write(
            dir.join("cloud.env"),
            "OPENAI_API_KEY=sk-stale-plain\nDEEPSEEK_API_KEY=ds-legacy\n",
        )
        .unwrap();

        let loaded = load_cloud_env_from(&dir);
        assert!(loaded.contains(&("OPENAI_API_KEY".to_string(), "sk-sealed-wins".to_string())));
        assert!(loaded.contains(&("DEEPSEEK_API_KEY".to_string(), "ds-legacy".to_string())));

        if let Some(v) = prev {
            std::env::set_var(super::super::vault_store::VAULT_KEY_ENV, v);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
