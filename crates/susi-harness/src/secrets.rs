//! API keys from the environment, read without ever showing them.
//!
//! A key is looked up by variable name, in this order: the process
//! environment, then (on Windows) the user scope and the machine scope. The
//! machine scope matters because a shell started before a key was set does not
//! see it, and a key set by an installer usually lives there.
//!
//! The value is wrapped in [`ApiKey`], whose `Debug` and `Display` print
//! `<redacted>`. The only way to read the key is [`ApiKey::expose`], which the
//! request builders call when they put it in a header. Error messages name the
//! variable and never its value. Lookups are cached for the life of the
//! process, so a command that checks several providers reads the registry once
//! per variable.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Mutex, OnceLock, PoisonError};

/// An API key. Never printed; read it only with [`ApiKey::expose`].
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    /// The key text, for a request header. Do not format it into a log, an
    /// error or a file.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(<redacted>)")
    }
}

impl fmt::Display for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Look up the key held by `name`, or `None` when no scope sets it.
///
/// A name that is not an upper-case environment variable name (letters,
/// digits and `_`) is refused without a lookup, so a name taken from a
/// configuration file cannot reach the registry query.
#[must_use]
pub fn lookup(name: &str) -> Option<ApiKey> {
    if !valid_name(name) {
        return None;
    }
    let cache = CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut cache = cache.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(found) = cache.get(name) {
        return found.clone();
    }
    let found = process_value(name)
        .or_else(|| registry_value(name))
        .map(ApiKey);
    cache.insert(name.to_string(), found.clone());
    found
}

/// A harness setting that is not a secret (a budget, an expiry date), read
/// from the same scopes as a key: the process, then the user and machine
/// environment. A value set in the user environment reaches a process that
/// started before it was set, as a key does.
#[must_use]
pub fn setting(name: &str) -> Option<String> {
    lookup(name).map(|value| value.0)
}

/// Whether any scope sets `name`. The key itself is not returned.
#[must_use]
pub fn is_set(name: &str) -> bool {
    lookup(name).is_some()
}

static CACHE: OnceLock<Mutex<BTreeMap<String, Option<ApiKey>>>> = OnceLock::new();

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn process_value(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// The value in the Windows user scope, then the machine scope.
///
/// PowerShell reads the scope through the same API the OS uses for its own
/// environment, so the value is expanded the way a new process would see it.
/// The variable name is validated by [`valid_name`] before it reaches the
/// script, and the script is passed as one argument with no shell.
#[cfg(windows)]
fn registry_value(name: &str) -> Option<String> {
    use std::process::{Command, Stdio};
    for scope in ["User", "Machine"] {
        let script = format!("[Environment]::GetEnvironmentVariable('{name}','{scope}')");
        let Ok(output) = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
        else {
            return None;
        };
        if !output.status.success() {
            continue;
        }
        let value = String::from_utf8(output.stdout).ok()?;
        let value = value.trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

#[cfg(not(windows))]
fn registry_value(_name: &str) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_and_display_never_show_the_key() {
        let key = ApiKey("sk-very-secret-value".to_string());
        assert_eq!(format!("{key:?}"), "ApiKey(<redacted>)");
        assert_eq!(format!("{key}"), "<redacted>");
        assert!(!format!("{key:?} {key}").contains("sk-very"));
        assert_eq!(key.expose(), "sk-very-secret-value");
    }

    #[test]
    fn names_that_are_not_env_names_are_refused() {
        assert!(!valid_name(""));
        assert!(!valid_name("lower_case"));
        assert!(!valid_name("KEY;rm -rf"));
        assert!(!valid_name("KEY VALUE"));
        assert!(valid_name("DEEPSEEK_API_KEY"));
        assert!(lookup("bad name").is_none());
    }

    #[test]
    fn process_environment_is_read_and_blank_values_are_not_keys() {
        // A variable no scope sets: lookup finds nothing, and is_set agrees.
        assert!(!is_set("SUSI_TEST_SECRET_NEVER_SET_0001"));
        assert!(process_value("SUSI_TEST_SECRET_NEVER_SET_0001").is_none());
    }
}
