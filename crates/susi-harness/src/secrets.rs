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
//! Native test and qualification launchers set `SUSI_NO_REGISTRY_ENV` in
//! each child's process environment before startup. Its presence (including
//! an empty value) disables both persistent Windows environment scopes for
//! keys, settings and home discovery, before the registry cache is touched.
//! It is never read from a `.env` file or the registry. Callers must still
//! scrub inherited credentials and use private working/data-home directories.
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
    pub(crate) fn from_workload(value: String) -> Self {
        Self(value)
    }

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

/// A credential supplied by trusted configuration, resolved at dispatch time.
#[derive(Clone)]
pub enum Credential {
    /// A variable name in the local key store or workload broker.
    Named(String),
    /// A credential explicitly supplied by a local embedding application.
    Local(ApiKey),
}

impl Credential {
    /// Read the current credential. Managed workloads only accept named grants.
    pub(crate) fn resolve(&self) -> Option<ApiKey> {
        match self {
            Self::Named(name) => lookup(name),
            Self::Local(key) if !crate::workload::enabled() => Some(key.clone()),
            Self::Local(_) => None,
        }
    }
}

impl From<String> for Credential {
    fn from(value: String) -> Self {
        Self::Local(ApiKey(value))
    }
}

impl From<&str> for Credential {
    fn from(value: &str) -> Self {
        Self::from(value.to_owned())
    }
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Credential(<redacted>)")
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
    if crate::workload::enabled() {
        return crate::workload::lookup(name).map(ApiKey);
    }
    let disabled = registry_environment_disabled();
    let stamp = sources_stamp(&data_home(), name);
    let cache = CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut cache = cache.lock().unwrap_or_else(PoisonError::into_inner);
    let cache_key = (name.to_string(), disabled, stamp);
    if let Some(found) = cache.get(&cache_key) {
        return found.clone();
    }
    let shape = crate::providers::Registry::builtin().key_prefix_for(name);
    let found = first_fitting(name, shape).map(ApiKey);
    if found.is_some() {
        cache.insert(cache_key, found.clone());
    }
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
const SCOPES: [&str; 6] = [
    "process environment",
    "user environment",
    "machine environment",
    WORKING_DOTENV,
    DATA_HOME_DOTENV,
    SEALED_VAULT_CLOUD,
];

/// The scope of `./.env`, in the directory the harness runs in.
const WORKING_DOTENV: &str = "./.env file";

/// The scope of the data home's `.env`.
const DATA_HOME_DOTENV: &str = "data-home .env file";

/// The scope of the data home's AEAD-sealed vault (`vault.sealed.json`) and `cloud.env`.
const SEALED_VAULT_CLOUD: &str = "data-home sealed vault / cloud.env";

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
        _ if scope == SEALED_VAULT_CLOUD
            && !USER_ONLY.contains(&name)
            && !registry_environment_disabled() =>
        {
            susi_config::cloud_env::load_cloud_env_from(&data_home())
                .into_iter()
                .find_map(|(k, v)| (k == name).then_some(v))
        }
        _ if scope == SEALED_VAULT_CLOUD => None,
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
    let dot_susi = std::path::Path::new(&user_home).join(".susi");
    if !dot_susi.is_dir() && !registry_environment_disabled() {
        let xdg = susi_paths::SusiDirs::config_dir();
        if ["vault.sealed.json", "cloud.env", ".env"]
            .iter()
            .any(|f| xdg.join(f).is_file())
        {
            return xdg;
        }
    }
    dot_susi
}

/// Which `.env` file.
#[derive(Clone, Copy)]
enum DotEnv {
    WorkingDirectory,
    DataHome,
}

type DotEnvCacheEntry = (
    std::path::PathBuf,
    Option<(u128, u64)>,
    BTreeMap<String, String>,
);

fn file_mtime_len(path: &std::path::Path) -> Option<(u128, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    Some((mtime, meta.len()))
}

fn sources_stamp(home: &std::path::Path, name: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (
        process_value(name),
        home,
        file_mtime_len(std::path::Path::new(".env")),
    )
        .hash(&mut h);
    let files = [
        ".env",
        "cloud.env",
        "vault.sealed.json",
        "vault.unlock.key",
        "vault.unlock.cred",
        "vault.unlock.dpapi",
    ];
    for file in files {
        file_mtime_len(&home.join(file)).hash(&mut h);
    }
    let vars = [
        "SUSI_HOME",
        "SUSI_VAULT_KEY",
        "SUSI_VAULT_KEY_FILE",
        "SUSI_VAULT_PASSPHRASE",
        "SUSI_VAULT_PASSPHRASE_FILE",
        "CREDENTIALS_DIRECTORY",
        "SUSI_CREDENTIALS_DIRECTORY",
    ];
    for var in vars {
        std::env::var(var).ok().hash(&mut h);
    }
    h.finish()
}

fn read_cached_dotenv(
    slot: &OnceLock<Mutex<Option<DotEnvCacheEntry>>>,
    path: std::path::PathBuf,
) -> BTreeMap<String, String> {
    let stamp = file_mtime_len(&path);
    let mutex = slot.get_or_init(|| Mutex::new(None));
    let mut guard = mutex.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((cached_path, cached_stamp, map)) = guard.as_ref() {
        if *cached_path == path && *cached_stamp == stamp {
            return map.clone();
        }
    }
    let fresh = read_dotenv(&path);
    *guard = Some((path, stamp, fresh.clone()));
    fresh
}

fn dotenv_value(name: &str, which: DotEnv) -> Option<String> {
    static WORKING: OnceLock<Mutex<Option<DotEnvCacheEntry>>> = OnceLock::new();
    static HOME: OnceLock<Mutex<Option<DotEnvCacheEntry>>> = OnceLock::new();
    #[cfg(test)]
    if let Some(found) = test_dotenv::value(which, name) {
        return found;
    }
    let file = match which {
        DotEnv::WorkingDirectory => {
            read_cached_dotenv(&WORKING, std::path::Path::new(".env").to_path_buf())
        }
        DotEnv::DataHome => read_cached_dotenv(&HOME, data_home().join(".env")),
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
    if crate::workload::enabled() {
        return process_value(name);
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
    if crate::workload::enabled() {
        return process_value(name).map(std::path::PathBuf::from);
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

// Partition cached keys too: enabling isolation must not return a key that
// an earlier non-isolated lookup obtained from a persistent environment, and
// rotating `.env` / `vault.sealed.json` invalidates stale cache entries.
type KeyCache = BTreeMap<(String, bool, u64), Option<ApiKey>>;
static CACHE: OnceLock<Mutex<KeyCache>> = OnceLock::new();

#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: Mutex<()> = Mutex::new(());

fn registry_environment_disabled() -> bool {
    std::env::var_os("SUSI_NO_REGISTRY_ENV").is_some()
}

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

/// The value in a native Windows environment scope, cached without spawning
/// an interpreter. Registry strings are read literally, as persistent User and
/// Machine environment values; secrets remain in memory and are never logged.
#[cfg(windows)]
fn registry_value(name: &str, scope: &str) -> Option<String> {
    // Check before touching the snapshot, even when it was initialized by a
    // previous lookup. Do not use `setting`: it calls back into this function.
    if registry_environment_disabled() {
        return None;
    }
    windows_scopes()
        .get(scope)?
        .get(&name.to_ascii_uppercase())
        .cloned()
}

#[cfg(any(windows, test))]
fn normalize_scope(values: impl IntoIterator<Item = (String, String)>) -> BTreeMap<String, String> {
    values
        .into_iter()
        .filter_map(|(name, value)| {
            let name = name.to_ascii_uppercase();
            let value = value.trim();
            (valid_name(&name) && !value.is_empty()).then(|| (name, value.to_string()))
        })
        .collect()
}

#[cfg(windows)]
fn read_registry_scope(root: &windows_registry::Key, path: &str) -> BTreeMap<String, String> {
    let Ok(key) = root.open(path) else {
        return BTreeMap::new();
    };
    let Ok(values) = key.values() else {
        return BTreeMap::new();
    };
    normalize_scope(
        values.filter_map(|(name, value)| String::try_from(value).ok().map(|value| (name, value))),
    )
}

// A child-process regression installs synthetic scopes before any lookup;
// it never opens the user's real registry, even if isolation regresses.
#[cfg(all(windows, test))]
static TEST_WINDOWS_SCOPES: OnceLock<BTreeMap<String, BTreeMap<String, String>>> = OnceLock::new();
#[cfg(all(windows, test))]
static TEST_WINDOWS_READS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(windows)]
fn windows_scopes() -> &'static BTreeMap<String, BTreeMap<String, String>> {
    static SCOPES: OnceLock<BTreeMap<String, BTreeMap<String, String>>> = OnceLock::new();
    #[cfg(test)]
    if let Some(scopes) = TEST_WINDOWS_SCOPES.get() {
        TEST_WINDOWS_READS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        return scopes;
    }
    SCOPES.get_or_init(|| {
        BTreeMap::from([
            (
                "User".into(),
                read_registry_scope(windows_registry::CURRENT_USER, "Environment"),
            ),
            (
                "Machine".into(),
                read_registry_scope(
                    windows_registry::LOCAL_MACHINE,
                    r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
                ),
            ),
        ])
    })
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
#[path = "tests/secrets.rs"]
mod tests;

#[cfg(test)]
#[path = "tests/secrets_vault.rs"]
mod secrets_vault_tests;
