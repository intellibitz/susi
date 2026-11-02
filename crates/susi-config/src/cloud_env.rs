//! `cloud.env`: the host's registered cloud API keys (dotenv format, 0600).
//!
//! Keys are *read* from the file on demand — never copied into the process
//! environment. `std::env::set_var` is unsound once other threads may read
//! the environment (a daemon always has such threads), so in-process lookups
//! go through [`env_or_cloud_env`] and child processes receive the keys
//! explicitly via [`cloud_env_overlay`].

use std::path::PathBuf;

/// Location of the host key file.
pub fn cloud_env_path() -> PathBuf {
    susi_paths::SusiDirs::config_dir().join("cloud.env")
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
        let mut value = value.trim().to_string();
        if (value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\''))
        {
            value = value
                .get(1..value.len() - 1)
                .unwrap_or_default()
                .to_string();
        }
        if value.is_empty() {
            continue;
        }
        out.push((key.to_string(), value));
    }
    out
}

/// Current `cloud.env` entries (mtime-cached read; empty when absent).
fn cloud_env_entries() -> Vec<(String, String)> {
    super::cluster_key::cached_file_bytes(&cloud_env_path())
        .and_then(|raw| String::from_utf8(raw).ok())
        .map(|text| parse_env_file(&text))
        .unwrap_or_default()
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

/// Registered keys the process environment does not already set — pass to
/// `Command::envs` so spawned agents see the same keys lookups do.
pub fn cloud_env_overlay() -> Vec<(String, String)> {
    cloud_env_entries()
        .into_iter()
        .filter(|(key, _)| std::env::var_os(key).is_none_or(|v| v.is_empty()))
        .collect()
}

#[cfg(test)]
mod malformed_quote_tests {
    use super::parse_env_file;

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
}
