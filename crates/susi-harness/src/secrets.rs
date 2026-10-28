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
//!
//! A provider that publishes a fixed key form ([`ProviderRecord::key_prefix`],
//! `sk-ant-` for Anthropic) gets only a value of that form. A scope holding
//! something else under the name (a token for another tool, a stale paste) is
//! passed over for the next scope, with a one-line warning that names the
//! variable and the scope, never the value. Without that, one wrong value in
//! the user scope hides a good key in the machine scope and every call fails
//! with 401.
//!
//! [`ProviderRecord::key_prefix`]: crate::providers::ProviderRecord::key_prefix

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
    let shape = crate::providers::Registry::builtin().key_prefix_for(name);
    let found = first_fitting(name, shape).map(ApiKey);
    cache.insert(name.to_string(), found.clone());
    found
}

/// The first scope's value for `name` that has the provider's key form, when
/// `shape` names one, warning about each scope passed over.
fn first_fitting(name: &str, shape: Option<(&str, &str)>) -> Option<String> {
    let mut passed_over = Vec::new();
    for scope in SCOPES {
        let Some(value) = scope_value(name, scope) else {
            continue;
        };
        match shape {
            Some((_, prefix)) if !value.starts_with(prefix) => passed_over.push(scope),
            _ => {
                if let Some((provider, prefix)) = shape {
                    for skipped in &passed_over {
                        eprintln!(
                            "⚠ {name} in the {skipped} environment is not a key for {provider} (it does not start with {prefix}); using the {scope} environment's"
                        );
                    }
                }
                return Some(value);
            }
        }
    }
    if let (Some((provider, prefix)), false) = (shape, passed_over.is_empty()) {
        eprintln!(
            "⚠ {name} holds no key for {provider} in any environment (none starts with {prefix}); {provider} is treated as having no key"
        );
    }
    None
}

/// Where a variable is read from, in order.
const SCOPES: [&str; 3] = ["process", "user", "machine"];

fn scope_value(name: &str, scope: &str) -> Option<String> {
    match scope {
        "process" => process_value(name),
        "user" => registry_value(name, "User"),
        _ => registry_value(name, "Machine"),
    }
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

/// The value in one Windows scope (`User` or `Machine`).
///
/// Both scopes are read once per process, with one PowerShell call through
/// the same API the OS uses for its own environment, so the values are what
/// a new process would see. Reading a scope per variable cost a process
/// spawn per lookup, seconds for a command that checks every provider. The
/// values stay in memory; nothing prints or stores them. Windows variable
/// names are case-insensitive, so they are matched upper-cased.
#[cfg(windows)]
fn registry_value(name: &str, scope: &str) -> Option<String> {
    windows_scopes()
        .get(scope)?
        .get(&name.to_ascii_uppercase())
        .cloned()
}

#[cfg(windows)]
fn windows_scopes() -> &'static BTreeMap<String, BTreeMap<String, String>> {
    use std::process::{Command, Stdio};
    static SCOPES: OnceLock<BTreeMap<String, BTreeMap<String, String>>> = OnceLock::new();
    SCOPES.get_or_init(|| {
        const SCRIPT: &str = "[Console]::OutputEncoding = [Text.Encoding]::UTF8; $all = @{}; foreach ($scope in 'User', 'Machine') { $vars = @{}; $found = [Environment]::GetEnvironmentVariables($scope); foreach ($name in $found.Keys) { $vars[[string]$name] = [string]$found[$name] }; $all[$scope] = $vars }; $all | ConvertTo-Json -Compress -Depth 3";
        let Ok(output) = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
        else {
            return BTreeMap::new();
        };
        if !output.status.success() {
            return BTreeMap::new();
        }
        parse_scopes(&String::from_utf8_lossy(&output.stdout))
    })
}

/// The `{"User": {...}, "Machine": {...}}` document, names upper-cased, with
/// blank values and names that are not variable names left out.
#[cfg_attr(not(windows), allow(dead_code))]
fn parse_scopes(text: &str) -> BTreeMap<String, BTreeMap<String, String>> {
    let Ok(serde_json::Value::Object(document)) = serde_json::from_str(text.trim()) else {
        return BTreeMap::new();
    };
    document
        .into_iter()
        .map(|(scope, variables)| {
            let variables = variables
                .as_object()
                .map(|map| {
                    map.iter()
                        .filter_map(|(name, value)| {
                            let name = name.to_ascii_uppercase();
                            let value = value.as_str()?.trim();
                            (valid_name(&name) && !value.is_empty())
                                .then(|| (name, value.to_string()))
                        })
                        .collect()
                })
                .unwrap_or_default();
            (scope, variables)
        })
        .collect()
}

#[cfg(not(windows))]
fn registry_value(_name: &str, _scope: &str) -> Option<String> {
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
    fn the_registry_names_the_anthropic_key_form_only() {
        let registry = crate::providers::Registry::builtin();
        assert_eq!(
            registry.key_prefix_for("ANTHROPIC_API_KEY"),
            Some(("anthropic", "sk-ant-"))
        );
        assert_eq!(registry.key_prefix_for("DEEPSEEK_API_KEY"), None);
        assert_eq!(
            registry.key_prefix_for("SUSI_HARNESS_BUDGET_ANTHROPIC_USD"),
            None
        );
    }

    #[test]
    fn a_value_of_the_wrong_form_is_passed_over() {
        // The process scope is the one a test can set; a wrong form there is
        // skipped. A name no other scope sets then has no key at all.
        let name = "SUSI_TEST_SHAPED_KEY_0001";
        std::env::set_var(name, "not-the-right-form");
        assert_eq!(first_fitting(name, Some(("example", "sk-ex-"))), None);
        assert_eq!(
            first_fitting(name, None).as_deref(),
            Some("not-the-right-form"),
            "a provider with no published form takes any value"
        );
        std::env::set_var(name, "sk-ex-123");
        assert_eq!(
            first_fitting(name, Some(("example", "sk-ex-"))).as_deref(),
            Some("sk-ex-123")
        );
        std::env::remove_var(name);
    }

    #[test]
    fn scope_documents_are_read_upper_cased_without_blanks() {
        let scopes = parse_scopes(
            r#"{"User":{"Deepseek_Api_Key":" sk-1 ","EMPTY":"","bad name":"x"},"Machine":{"PATH":"C:/x"}}"#,
        );
        assert_eq!(
            scopes["User"].get("DEEPSEEK_API_KEY").map(String::as_str),
            Some("sk-1")
        );
        assert!(!scopes["User"].contains_key("EMPTY"));
        assert!(!scopes["User"].contains_key("BAD NAME"));
        assert_eq!(
            scopes["Machine"].get("PATH").map(String::as_str),
            Some("C:/x")
        );
        assert!(parse_scopes("not json").is_empty());
    }

    #[test]
    fn process_environment_is_read_and_blank_values_are_not_keys() {
        // A variable no scope sets: lookup finds nothing, and is_set agrees.
        assert!(!is_set("SUSI_TEST_SECRET_NEVER_SET_0001"));
        assert!(process_value("SUSI_TEST_SECRET_NEVER_SET_0001").is_none());
    }
}
