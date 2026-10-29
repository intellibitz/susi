//! API keys from the environment, read without ever showing them.
//!
//! A key is looked up by variable name, in this order: the process
//! environment, then (on Windows) the user scope and the machine scope, then
//! `./.env` and the data home's `.env`. The machine scope matters because a
//! shell started before a key was set does not see it, and a key set by an
//! installer usually lives there. A harness setting ([`setting`]) is read
//! from the same places, except the settings that decide where keys are sent
//! ([`USER_ONLY`]), which `./.env` never sets.
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
    first_fitting_scope(name, shape).map(|(value, _)| value)
}

/// [`first_fitting`] with the scope the value came from.
fn first_fitting_scope(name: &str, shape: Option<(&str, &str)>) -> Option<(String, &'static str)> {
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
                            "⚠ {name} in the {skipped} is not a key for {provider} (it does not start with {prefix}); using the one in the {scope}"
                        );
                    }
                }
                return Some((value, scope));
            }
        }
    }
    if let (Some((provider, prefix)), false) = (shape, passed_over.is_empty()) {
        eprintln!(
            "⚠ {name} holds no key for {provider} anywhere it is read (none starts with {prefix}); {provider} is treated as having no key"
        );
    }
    None
}

/// Where a variable is read from, in order: the environment first (process,
/// then the Windows user and machine scopes), then `.env` files, the
/// bring-your-own-key form most tools accept: `./.env` in the directory the
/// harness runs in, then `.env` in the data home ([`data_home`]).
const SCOPES: [&str; 5] = [
    "process environment",
    "user environment",
    "machine environment",
    WORKING_DOTENV,
    DATA_HOME_DOTENV,
];

/// The scope of `./.env`, in the directory the harness runs in.
const WORKING_DOTENV: &str = "./.env file";

/// The scope of the data home's `.env`.
const DATA_HOME_DOTENV: &str = "data-home .env file";

/// The variable that names the data home ([`data_home`]).
const HOME_ENV: &str = "SUSI_HOME";

/// Settings that decide where keys are sent. They are read only from the
/// user's own places, the environment and the data home's `.env`, never
/// from `./.env`.
///
/// `./.env` belongs to whatever directory the harness runs in, a repository
/// cloned a minute ago as easily as the user's own. A `SUSI_HARNESS_PROVIDERS`
/// there once loaded that repository's registry overlay, and every call for
/// `deepseek-flash` went to the repository's endpoint with the user's
/// DeepSeek key; a `SUSI_HOME` there chose a data home whose `.env` did the
/// same. A key in `./.env` is still read: it is that directory's own key,
/// and it sends no other key anywhere.
pub const USER_ONLY: [&str; 2] = [HOME_ENV, crate::providers::OVERLAY_ENV];

fn scope_value(name: &str, scope: &str) -> Option<String> {
    match scope {
        "process environment" => process_value(name),
        "user environment" => registry_value(name, "User"),
        "machine environment" => registry_value(name, "Machine"),
        WORKING_DOTENV if USER_ONLY.contains(&name) => {
            if dotenv_value(name, DotEnv::WorkingDirectory).is_some() {
                warn_ignored_in_working_dotenv(name);
            }
            None
        }
        WORKING_DOTENV => dotenv_value(name, DotEnv::WorkingDirectory),
        // The data home's `.env` is found through `SUSI_HOME`, so it cannot
        // say where the data home is: reading it there would go in a circle,
        // and `susi-harness env` would report a home the harness never uses.
        _ if name == HOME_ENV => None,
        _ => dotenv_value(name, DotEnv::DataHome),
    }
}

/// Say once per process that `./.env` sets a [`USER_ONLY`] setting the
/// harness does not use, so a value put there on purpose is not lost in
/// silence.
fn warn_ignored_in_working_dotenv(name: &str) {
    static WARNED: Mutex<std::collections::BTreeSet<String>> =
        Mutex::new(std::collections::BTreeSet::new());
    let mut warned = WARNED.lock().unwrap_or_else(PoisonError::into_inner);
    if !warned.insert(name.to_string()) {
        return;
    }
    let elsewhere = if name == HOME_ENV {
        "the environment"
    } else {
        "the environment or the data home's .env"
    };
    eprintln!(
        "⚠ {name} in ./.env is ignored: a directory's .env may not decide where your keys are sent; set it in {elsewhere}"
    );
}

/// The harness's per-user home: `SUSI_HOME` when set (process, user or
/// machine environment; never `./.env`, see [`USER_ONLY`], nor the data
/// home's own `.env`, which is found through it), else `.susi` in the user's
/// home directory. The ledger, the `.env` file and anything else the harness
/// keeps across runs live here, so they follow the user, not the directory.
#[must_use]
pub fn data_home() -> std::path::PathBuf {
    // Read the way every other setting is, so the home `susi-harness env`
    // reports is the one in use: a `SUSI_HOME` in `./.env` was once shown as
    // set while the ledger stayed under `~/.susi`.
    if let Some(home) = setting(HOME_ENV) {
        return std::path::PathBuf::from(home);
    }
    let from_environment = |name: &str| {
        process_value(name)
            .or_else(|| registry_value(name, "User"))
            .or_else(|| registry_value(name, "Machine"))
    };
    let user_home = from_environment("USERPROFILE")
        .or_else(|| from_environment("HOME"))
        .unwrap_or_else(|| ".".to_string());
    std::path::Path::new(&user_home).join(".susi")
}

/// Which `.env` file.
#[derive(Clone, Copy)]
enum DotEnv {
    WorkingDirectory,
    DataHome,
}

fn dotenv_value(name: &str, which: DotEnv) -> Option<String> {
    static WORKING: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    static HOME: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    #[cfg(test)]
    if let Some(found) = test_dotenv::value(which, name) {
        return found;
    }
    let file = match which {
        DotEnv::WorkingDirectory => {
            WORKING.get_or_init(|| read_dotenv(std::path::Path::new(".env")))
        }
        DotEnv::DataHome => HOME.get_or_init(|| read_dotenv(&data_home().join(".env"))),
    };
    file.get(name).cloned()
}

/// The pairs in the `.env` file at `path`; none when there is no such file.
///
/// A file that is there but cannot be used gets a one-line warning naming
/// the file and the reason, never what is in it. It once failed in silence:
/// a key in a file Windows PowerShell wrote (UTF-16) was never read, and the
/// harness told the user to put the key in a `.env` file, which they had.
fn read_dotenv(path: &std::path::Path) -> BTreeMap<String, String> {
    // A directory named `.env` is a Python virtual environment, not a key
    // file, so it is passed over without a word, like a missing file.
    if !path.is_file() {
        return BTreeMap::new();
    }
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!(
                "⚠ {} could not be read ({error}); nothing in it is used",
                path.display()
            );
            return BTreeMap::new();
        }
    };
    let (text, problem) = decode_dotenv(&bytes);
    if let Some(problem) = problem {
        eprintln!(
            "⚠ {} {problem}; save it as UTF-8 so every line is read as written",
            path.display()
        );
    }
    parse_dotenv(&text)
}

/// The text of a `.env` file in the encodings editors and shells write:
/// UTF-8 with or without a byte-order mark (Windows PowerShell 5's
/// `-Encoding UTF8`, older Notepad), and UTF-16 with one, little- or
/// big-endian (Windows PowerShell 5's `>`, `Out-File` and `-Encoding
/// Unicode`). Text that does not decode is replaced character by character,
/// so the lines that do still count, and the reason comes back with it.
fn decode_dotenv(bytes: &[u8]) -> (String, Option<&'static str>) {
    let utf16 = |rest: &[u8], unit: fn([u8; 2]) -> u16| {
        let (pairs, odd_byte) = rest.as_chunks::<2>();
        let units = pairs.iter().map(|&pair| unit(pair));
        let mut clean = odd_byte.is_empty();
        let text = char::decode_utf16(units)
            .map(|decoded| {
                decoded.unwrap_or_else(|_| {
                    clean = false;
                    char::REPLACEMENT_CHARACTER
                })
            })
            .collect::<String>();
        (text, (!clean).then_some("is not valid UTF-16 text"))
    };
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return utf16(rest, u16::from_le_bytes);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return utf16(rest, u16::from_be_bytes);
    }
    let rest = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    match std::str::from_utf8(rest) {
        Ok(text) if text.contains('\0') => (
            text.to_string(),
            Some("holds NUL bytes, as UTF-16 written without a byte-order mark does"),
        ),
        Ok(text) => (text.to_string(), None),
        Err(_) => (
            String::from_utf8_lossy(rest).into_owned(),
            Some("is not UTF-8 text"),
        ),
    }
}

/// `NAME=value` lines, as `.env` files write them: a byte-order mark, `#`
/// comments and blank lines skipped, an optional `export ` prefix, CRLF or
/// LF line ends, and the value read as [`dotenv_value_text`] reads it.
/// Names that are not variable names, and blank values, are left out.
fn parse_dotenv(text: &str) -> BTreeMap<String, String> {
    text.trim_start_matches('\u{feff}')
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let line = line
                .strip_prefix("export")
                .filter(|rest| rest.starts_with([' ', '\t']))
                .map_or(line, str::trim_start);
            let (name, value) = line.split_once('=')?;
            let name = name.trim();
            let value = dotenv_value_text(value);
            (valid_name(name) && !value.is_empty()).then(|| (name.to_string(), value.to_string()))
        })
        .collect()
}

/// The value after a `.env` line's `=`: inside its quotes when it is quoted
/// (anything after the closing quote must be a comment), else the text up to
/// a `#` that follows a space or tab. Every common dotenv reader (python-dotenv,
/// Node dotenv, docker compose, a shell's `source`) ends a value there, and no
/// provider key holds whitespace before a `#`; reading on once sent
/// `sk-… # personal` as the key, and every call got 401. Escape sequences are
/// not decoded: a Windows path in double quotes means what it says.
fn dotenv_value_text(raw: &str) -> &str {
    let value = raw.trim();
    for quote in ['"', '\''] {
        let Some(inner) = value.strip_prefix(quote) else {
            continue;
        };
        if let Some(end) = inner.find(quote) {
            let after = inner[end + quote.len_utf8()..].trim_start();
            if after.is_empty() || after.starts_with('#') {
                return &inner[..end];
            }
        }
    }
    let comment = raw
        .char_indices()
        .find(|&(at, c)| c == '#' && raw[..at].ends_with([' ', '\t']))
        .map_or(raw.len(), |(at, _)| at);
    raw[..comment].trim()
}

/// A harness setting that is not a secret (a budget, an expiry date, the
/// registry overlay's path), read from the same places as a key, first match
/// wins: the process, the Windows user and machine environment (so a value
/// set after a shell started is still seen), `./.env`, then the data home's
/// `.env`. A setting that decides where keys are sent ([`USER_ONLY`]) is
/// never read from `./.env`.
///
/// It reads no key form and keeps no cache, so it never reaches the provider
/// registry: the registry reads its own overlay path through it
/// ([`crate::providers::OVERLAY_ENV`]), and a lookup that went back to the
/// registry would re-enter the registry while it is being built. Outside the
/// key cache, a setting is also never masked by [`redact`] as if it were a key.
#[must_use]
pub fn setting(name: &str) -> Option<String> {
    if !valid_name(name) {
        return None;
    }
    first_fitting(name, None)
}

/// A [`setting`] that names a file. A relative path in the data home's
/// `.env` is taken from the data home, where that file lives; one set in the
/// environment is relative to the directory the harness runs in, as a shell
/// would read it.
#[must_use]
pub fn setting_path(name: &str) -> Option<std::path::PathBuf> {
    if !valid_name(name) {
        return None;
    }
    let (value, scope) = first_fitting_scope(name, None)?;
    let path = std::path::PathBuf::from(value);
    if scope == DATA_HOME_DOTENV && path.is_relative() {
        return Some(data_home().join(path));
    }
    Some(path)
}

/// `text` with every key in it replaced by `<redacted>`: any value this
/// process has read for a key variable, and anything shaped like a provider
/// key (`sk-…`, `AIza…`), masked forms included (`sk-abcd****1234`).
///
/// For text that came from outside and is going back out: a provider's error
/// body can echo the key it was sent (OpenAI's 401 names a masked prefix and
/// suffix of it), and the gateway hands error bodies to its callers, remote
/// ones too under `--accounts`.
#[must_use]
pub fn redact(text: &str) -> String {
    let mut out = text.to_string();
    if let Some(cache) = CACHE.get() {
        let cache = cache.lock().unwrap_or_else(PoisonError::into_inner);
        for key in cache.values().flatten() {
            if key.0.len() >= 8 {
                out = out.replace(&key.0, "<redacted>");
            }
        }
    }
    for prefix in ["sk-", "AIza"] {
        out = redact_shaped(&out, prefix);
    }
    out
}

/// A character a key is made of, masked forms (`*`) included.
fn key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '*' | '.')
}

/// Replace each run that starts with `prefix` and continues with key
/// characters (letters, digits, `-`, `_`, `*`, `.`) for at least eight more.
///
/// A key is masked wherever it sits: after a space, a quote, `=` or `:`, a
/// digit, a `-` (a deleted line in a diff), `**`, or an escape (`%20` and
/// `%3D` in a URL, `\n` in JSON). The one run left alone is a word's own
/// tail ([`glued_to_a_word`] and [`reads_as_words`]): command output a
/// worker reads is masked too, and `task-runner-tests` is a file name, not
/// `ta` and a key. A rule that skipped every run after a letter or a digit
/// let `Bearer%20sk-…`, `key%3DAIza…` and `"…\nsk-…"` through unmasked.
fn redact_shaped(text: &str, prefix: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(prefix) {
        let (before, from) = rest.split_at(at);
        out.push_str(before);
        let tail = &from[prefix.len()..];
        let run = tail.find(|c: char| !key_char(c)).unwrap_or(tail.len());
        let a_words_tail = glued_to_a_word(&out) && reads_as_words(&tail[..run]);
        if run >= 8 && !a_words_tail {
            out.push_str("<redacted>");
        } else {
            out.push_str(&from[..prefix.len() + run]);
        }
        rest = &tail[run..];
    }
    out.push_str(rest);
    out
}

/// Whether the text before a key prefix ends inside a word: in an ASCII
/// letter that does not end an escape (`\n`, `\u003d`, `\x3d`, `%3D`). An
/// escape's letter stands for a separator, and a key often follows one.
fn glued_to_a_word(before: &str) -> bool {
    let bytes = before.as_bytes();
    let n = bytes.len();
    if !bytes.last().is_some_and(u8::is_ascii_alphabetic) {
        return false;
    }
    let hex = |range: &[u8]| range.iter().all(u8::is_ascii_hexdigit);
    let escaped = (n >= 2 && bytes[n - 2] == b'\\')
        || (n >= 3 && bytes[n - 3] == b'%' && hex(&bytes[n - 2..]))
        || (n >= 4 && bytes[n - 4..n - 2] == *b"\\x" && hex(&bytes[n - 2..]))
        || (n >= 6 && bytes[n - 6..n - 4] == *b"\\u" && hex(&bytes[n - 4..]));
    !escaped
}

/// Whether a run glued to a word reads as more of it (`runner-tests.rs`,
/// `assessment-module`, `SQLAlchemy`) rather than as a key: no `*` (a masked
/// key), and no piece between `-`, `_` and `.` that is 16 characters or
/// longer, or 10 or longer and mixes digits with upper- and lower-case
/// letters. Every provider's key has a random part longer than that.
fn reads_as_words(run: &str) -> bool {
    let key_like = |piece: &str| {
        let mixed = piece.bytes().any(|b| b.is_ascii_digit())
            && piece.bytes().any(|b| b.is_ascii_uppercase())
            && piece.bytes().any(|b| b.is_ascii_lowercase());
        piece.len() >= 16 || (piece.len() >= 10 && mixed)
    };
    !run.contains('*') && !run.split(['-', '_', '.']).any(key_like)
}

/// Whether any scope sets `name`. The key itself is not returned.
#[must_use]
pub fn is_set(name: &str) -> bool {
    lookup(name).is_some()
}

static CACHE: OnceLock<Mutex<BTreeMap<String, Option<ApiKey>>>> = OnceLock::new();

/// Whether `name` is a variable the harness reads: upper-case letters,
/// digits and `_`. Any other name is never looked up.
pub(crate) fn valid_name(name: &str) -> bool {
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

/// A `./.env` or a data-home `.env` a test stands in for the real one, on
/// its own thread only, so a test can show what a value set there does
/// without touching the files the process reads (or the user's own keys).
#[cfg(test)]
pub(crate) mod test_dotenv {
    use super::DotEnv;
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::thread::LocalKey;

    type Stand = RefCell<Option<BTreeMap<String, String>>>;

    thread_local! {
        static WORKING: Stand = const { RefCell::new(None) };
        static HOME: Stand = const { RefCell::new(None) };
    }

    fn stand(which: DotEnv) -> &'static LocalKey<Stand> {
        match which {
            DotEnv::WorkingDirectory => &WORKING,
            DotEnv::DataHome => &HOME,
        }
    }

    /// `Some` with the stand-in's value for `name` while one is in place.
    pub(super) fn value(which: DotEnv, name: &str) -> Option<Option<String>> {
        stand(which).with(|file| file.borrow().as_ref().map(|pairs| pairs.get(name).cloned()))
    }

    fn standing_in<T>(which: DotEnv, text: &str, body: impl FnOnce() -> T) -> T {
        struct Restore(DotEnv);
        impl Drop for Restore {
            fn drop(&mut self) {
                stand(self.0).with(|file| file.borrow_mut().take());
            }
        }
        stand(which).with(|file| *file.borrow_mut() = Some(super::parse_dotenv(text)));
        let _restore = Restore(which);
        body()
    }

    /// Run `body` with `text` read as `./.env` on this thread.
    pub(crate) fn with<T>(text: &str, body: impl FnOnce() -> T) -> T {
        standing_in(DotEnv::WorkingDirectory, text, body)
    }

    /// Run `body` with `text` read as the data home's `.env` on this thread.
    pub(crate) fn with_home<T>(text: &str, body: impl FnOnce() -> T) -> T {
        standing_in(DotEnv::DataHome, text, body)
    }
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
    fn key_shaped_text_is_redacted_and_ordinary_text_is_not() {
        let body = r#"{"error":{"message":"Incorrect API key provided: sk-litel**************************2026. See https://platform.openai.com/account/api-keys."}}"#;
        let clean = redact(body);
        assert!(!clean.contains("sk-litel"), "{clean}");
        assert!(
            clean.contains("Incorrect API key provided: <redacted>"),
            "{clean}"
        );
        assert!(clean.contains("api-keys"), "ordinary words stay: {clean}");
        assert_eq!(
            redact("key AIzaSyA1234567890abcdef ok"),
            "key <redacted> ok"
        );
        assert_eq!(
            redact("Bearer sk-ant-api03-abcdefghijklmnop end"),
            "Bearer <redacted> end"
        );
        // Short runs are words, not keys.
        assert_eq!(redact("risk-free task-ok sk-1"), "risk-free task-ok sk-1");
        assert_eq!(redact("context length exceeded"), "context length exceeded");
        // A prefix inside a word is not a key: command output a worker reads
        // keeps its file names.
        assert_eq!(
            redact("src/task-runner-tests.rs risk-assessment-module"),
            "src/task-runner-tests.rs risk-assessment-module"
        );
        assert_eq!(
            redact("OPENAI_API_KEY=sk-proj-abcdefghij\n'AIzaSyA1234567890abcdef'"),
            "OPENAI_API_KEY=<redacted>\n'<redacted>'"
        );
    }

    #[test]
    fn a_key_is_masked_after_an_escape_a_digit_or_punctuation_too() {
        // Each of these passed through unmasked while a key had to start a
        // word: the character before it is a letter, a digit or `-`.
        for (text, masked) in [
            (
                "Authorization=Bearer%20sk-proj-abcdefghijklmnop",
                "Authorization=Bearer%20<redacted>",
            ),
            (
                "url?q=key%3DAIzaSyAbcdefghijklmnop",
                "url?q=key%3D<redacted>",
            ),
            (
                r#"{"out":"exit code: 0\nsk-proj-abcdefghijklmnop"}"#,
                r#"{"out":"exit code: 0\n<redacted>"}"#,
            ),
            (
                r#"{"k":"\u003dsk-proj-abcdefghijklmnop"}"#,
                r#"{"k":"\u003d<redacted>"}"#,
            ),
            ("**sk-proj-abcdefghijklmnop**", "**<redacted>"),
            ("-sk-proj-Abc123def456ghi\n+x", "-<redacted>\n+x"),
            ("v2sk-proj-abcdefghijklmnop", "v2<redacted>"),
            ("name_AIzaSyAbcdefghijklmnop", "name_<redacted>"),
        ] {
            assert_eq!(redact(text), masked, "{text}");
        }
        // An escape's letter stands for a separator, so after one even a run
        // short enough to pass for the rest of a word is masked.
        let backslash = '\x5c';
        for escape in [
            format!("{backslash}n"),
            format!("{backslash}t"),
            format!("{backslash}u003d"),
            format!("{backslash}x3d"),
            "%3D".to_string(),
            "%3d".to_string(),
        ] {
            assert_eq!(
                redact(&format!("line{escape}sk-proj-abcdefghij")),
                format!("line{escape}<redacted>"),
                "{escape}"
            );
        }
        // Glued to a word, a run that is plainly a key is masked all the same.
        assert_eq!(
            redact("tokensk-0123456789abcdef0123456789abcdef"),
            "token<redacted>"
        );
        assert_eq!(redact("xsk-abcd****1234"), "x<redacted>");
        assert_eq!(redact("keyAIzaSyA1b2C3d4E5f6"), "key<redacted>");
    }

    #[test]
    fn words_that_contain_a_key_prefix_are_left_alone() {
        for text in [
            "src/task-runner-tests.rs risk-assessment-module",
            "pip install Flask-SQLAlchemy Flask-SocketIO",
            "task-runner-v2-tests disk-usage-report.json",
            "mask-sensitive-data whisk-it-together",
        ] {
            assert_eq!(redact(text), text);
        }
    }

    #[test]
    fn dotenv_files_are_read_as_tools_write_them() {
        let parsed = parse_dotenv(concat!(
            "# keys\n",
            "OPENAI_API_KEY=sk-abc\n",
            "export GEMINI_API_KEY=\"g-1\"\n",
            "EMPTY=\n",
            "lower=x\n",
            "  ANTHROPIC_API_KEY = 'sk-ant-1'  \n",
            "not a line\n",
        ));
        assert_eq!(
            parsed.get("OPENAI_API_KEY").map(String::as_str),
            Some("sk-abc")
        );
        assert_eq!(
            parsed.get("GEMINI_API_KEY").map(String::as_str),
            Some("g-1")
        );
        assert_eq!(
            parsed.get("ANTHROPIC_API_KEY").map(String::as_str),
            Some("sk-ant-1")
        );
        assert!(!parsed.contains_key("EMPTY"));
        assert!(!parsed.contains_key("lower"));
        assert_eq!(parsed.len(), 3);
    }

    /// `text` as Windows PowerShell 5 writes it with `>` or `Out-File`:
    /// UTF-16LE with a byte-order mark and CRLF line ends.
    fn utf16le_with_bom(text: &str) -> Vec<u8> {
        let mut bytes = vec![0xFF, 0xFE];
        for unit in text.replace('\n', "\r\n").encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn a_dotenv_file_in_any_encoding_windows_writes_is_read() {
        let line = "OPENAI_API_KEY=sk-test-encoding\n";
        let mut utf16be = vec![0xFE, 0xFF];
        for unit in line.encode_utf16() {
            utf16be.extend_from_slice(&unit.to_be_bytes());
        }
        let utf8_bom = [b"\xEF\xBB\xBF".as_slice(), line.as_bytes()].concat();
        let crlf = line.replace('\n', "\r\n").into_bytes();
        for (form, bytes) in [
            ("UTF-16LE with a BOM", utf16le_with_bom(line)),
            ("UTF-16BE with a BOM", utf16be),
            ("UTF-8 with a BOM", utf8_bom),
            ("UTF-8 with CRLF", crlf),
            ("UTF-8", line.as_bytes().to_vec()),
        ] {
            let (text, problem) = decode_dotenv(&bytes);
            assert_eq!(problem, None, "{form}");
            assert_eq!(
                parse_dotenv(&text)
                    .get("OPENAI_API_KEY")
                    .map(String::as_str),
                Some("sk-test-encoding"),
                "{form}"
            );
        }
        // Through the file, as the harness reads it.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        std::fs::write(&path, utf16le_with_bom(line)).unwrap();
        assert_eq!(
            read_dotenv(&path).get("OPENAI_API_KEY").map(String::as_str),
            Some("sk-test-encoding")
        );
    }

    #[test]
    fn a_dotenv_file_that_does_not_decode_says_so_and_keeps_the_lines_that_do() {
        // A Windows-1252 comment: the key line still counts.
        let (text, problem) = decode_dotenv(b"# caf\xE9\nOPENAI_API_KEY=sk-test-ansi\n");
        assert_eq!(problem, Some("is not UTF-8 text"));
        assert_eq!(
            parse_dotenv(&text)
                .get("OPENAI_API_KEY")
                .map(String::as_str),
            Some("sk-test-ansi")
        );
        let (_, problem) = decode_dotenv(b"K\0=\0v\0");
        assert!(problem.is_some_and(|p| p.contains("NUL")), "{problem:?}");
        let (_, problem) = decode_dotenv(&[0xFF, 0xFE, 0x41]);
        assert_eq!(problem, Some("is not valid UTF-16 text"));
    }

    #[test]
    fn a_dotenv_directory_is_passed_over() {
        // Python projects keep a virtual environment in a `.env` folder.
        let dir = tempfile::tempdir().unwrap();
        assert!(read_dotenv(dir.path()).is_empty());
        assert!(read_dotenv(&dir.path().join("missing")).is_empty());
    }

    #[test]
    fn an_inline_comment_after_a_dotenv_value_is_not_part_of_the_key() {
        let parsed = parse_dotenv(concat!(
            "OPENAI_API_KEY=sk-abc  # personal\n",
            "ANTHROPIC_API_KEY=\"sk-ant-1\" # work\n",
            "GEMINI_API_KEY='g-1'#c\n",
            "DEEPSEEK_API_KEY=sk-hash#in-the-value\n",
            "MOONSHOT_API_KEY=\t# nothing but a comment\n",
            "export\tKIMI_API_KEY=\"k # quoted\"\n",
            "SUSI_HARNESS_PROVIDERS=\"C:\\cfg\\providers.json\"\n",
        ));
        let get = |name: &str| parsed.get(name).map(String::as_str);
        assert_eq!(get("OPENAI_API_KEY"), Some("sk-abc"));
        assert_eq!(get("ANTHROPIC_API_KEY"), Some("sk-ant-1"));
        assert_eq!(get("GEMINI_API_KEY"), Some("g-1"));
        assert_eq!(
            get("DEEPSEEK_API_KEY"),
            Some("sk-hash#in-the-value"),
            "a # with no space before it is part of the value"
        );
        assert_eq!(get("MOONSHOT_API_KEY"), None);
        assert_eq!(get("KIMI_API_KEY"), Some("k # quoted"));
        assert_eq!(
            get("SUSI_HARNESS_PROVIDERS"),
            Some("C:\\cfg\\providers.json"),
            "no escapes are decoded"
        );
    }

    #[test]
    fn susi_home_in_a_directorys_dotenv_never_moves_the_data_home() {
        // The data home's `.env` is found through SUSI_HOME; it cannot say
        // where it is itself.
        assert_eq!(scope_value(HOME_ENV, DATA_HOME_DOTENV), None);
        let set_elsewhere = process_value(HOME_ENV)
            .or_else(|| registry_value(HOME_ENV, "User"))
            .or_else(|| registry_value(HOME_ENV, "Machine"));
        if set_elsewhere.is_some() {
            eprintln!("SUSI_HOME is set in the environment, which comes first; skipping");
            return;
        }
        // A repository's `.env` that chose the data home chose the `.env`
        // read there too, and with it a registry overlay.
        let home = test_dotenv::with("SUSI_HOME=/srv/susi-test-home\n", data_home);
        assert_ne!(home, std::path::PathBuf::from("/srv/susi-test-home"));
        assert_eq!(home, data_home());
        let reported = test_dotenv::with("SUSI_HOME=/srv/susi-test-home\n", || setting(HOME_ENV));
        assert_eq!(reported, None, "`env` reports the home in use");
    }

    #[test]
    fn a_setting_that_decides_where_keys_go_is_never_read_from_a_directorys_dotenv() {
        let overlay = crate::providers::OVERLAY_ENV;
        assert!(USER_ONLY.contains(&overlay) && USER_ONLY.contains(&HOME_ENV));
        if setting(overlay).is_some() {
            eprintln!("{overlay} is set where this machine reads settings; skipping");
            return;
        }
        let line = format!("{overlay}=/srv/overlay-0002.json\n");
        assert_eq!(test_dotenv::with(&line, || setting(overlay)), None);
        assert_eq!(test_dotenv::with(&line, || setting_path(overlay)), None);
        // The user's own `.env`, in the data home, still sets it.
        assert_eq!(
            test_dotenv::with_home(&line, || setting(overlay)).as_deref(),
            Some("/srv/overlay-0002.json")
        );
    }

    #[test]
    fn a_relative_path_in_the_data_homes_dotenv_is_taken_from_the_data_home() {
        let name = "SUSI_TEST_PATH_SETTING_0001";
        let path =
            test_dotenv::with_home(&format!("{name}=providers.json\n"), || setting_path(name));
        assert_eq!(path, Some(data_home().join("providers.json")));
        let absolute = std::env::temp_dir().join("providers-0001.json");
        let path = test_dotenv::with_home(&format!("{name}='{}'\n", absolute.display()), || {
            setting_path(name)
        });
        assert_eq!(path, Some(absolute));
        // Set in a directory's `.env`, an ordinary path setting is that
        // directory's and stays relative to it.
        let path = test_dotenv::with(&format!("{name}=providers.json\n"), || setting_path(name));
        assert_eq!(path, Some(std::path::PathBuf::from("providers.json")));
    }

    #[test]
    fn a_setting_is_read_from_dotenv_and_never_masked_as_a_key() {
        let name = "SUSI_TEST_SETTING_ONLY_IN_DOTENV_0001";
        let value = test_dotenv::with(&format!("{name}=/srv/overlay-0001.json\n"), || {
            setting(name)
        });
        assert_eq!(value.as_deref(), Some("/srv/overlay-0001.json"));
        assert_eq!(setting(name), None, "the stand-in is gone");
        assert_eq!(
            redact("ledger at /srv/overlay-0001.json"),
            "ledger at /srv/overlay-0001.json"
        );
    }

    #[test]
    fn the_data_home_is_a_susi_directory() {
        let home = data_home();
        assert!(
            home.ends_with(".susi") || std::env::var_os("SUSI_HOME").is_some(),
            "{}",
            home.display()
        );
    }

    #[test]
    fn process_environment_is_read_and_blank_values_are_not_keys() {
        // A variable no scope sets: lookup finds nothing, and is_set agrees.
        assert!(!is_set("SUSI_TEST_SECRET_NEVER_SET_0001"));
        assert!(process_value("SUSI_TEST_SECRET_NEVER_SET_0001").is_none());
    }
}
