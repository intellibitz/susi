#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

//! Host path and port contract.
//!
//! [`SusiDirs`] asks the standalone `susi-paths` service (`127.0.0.1:18080`,
//! override via `SUSI_PATHS_PORT`) and falls back to the local XDG/legacy
//! resolver when the service is unreachable, so path lookup never hard-fails.
//! The service itself answers from the same local resolver.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// Host directory contract, resolved via the `susi-paths` service with a
/// local XDG/legacy fallback when it is unreachable.
pub struct SusiDirs;

const SERVICE_TIMEOUT: Duration = Duration::from_millis(200);

impl SusiDirs {
    /// Cached while the service is healthy; retries (bounded) while it is down.
    fn fetch_paths() -> HashMap<String, PathBuf> {
        static CACHE: OnceLock<HashMap<String, PathBuf>> = OnceLock::new();
        if let Some(cached) = CACHE.get() {
            return cached.clone();
        }
        let map = Self::fetch_paths_uncached();
        if !map.is_empty() {
            let _ = CACHE.set(map.clone());
        }
        map
    }

    fn fetch_paths_uncached() -> HashMap<String, PathBuf> {
        // Explicit SUSI_PATHS_PORT wins, else the default rides the instance
        // offset like every other susi port.
        let port = std::env::var("SUSI_PATHS_PORT")
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or_else(|| ports::effective(ports::PATHS_SERVICE));
        let fetch = || -> Option<HashMap<String, PathBuf>> {
            let ep = loopback::Endpoint {
                port,
                timeout: SERVICE_TIMEOUT,
                auth: loopback::Auth::None,
            };
            let resp = loopback::request(&ep, "GET", "/paths", None)?;
            serde_json::from_str::<HashMap<String, PathBuf>>(resp.ok_body()?).ok()
        };
        let map = fetch().unwrap_or_default();
        // The service answers for whichever user/HOME started it. Only trust
        // it when it resolves *our* home — otherwise another user's (or
        // another HOME's) daemon would silently hand us its substrate paths.
        match map.get("home_dir") {
            Some(home) if same_path(home, &LocalDirs::home_dir()) => map,
            _ => HashMap::new(),
        }
    }

    /// Explicit local env config (`SUSI_HOME`, `SUSI_XDG`, `XDG_*_HOME`)
    /// beats the service — a second instance's root must not resolve to the
    /// primary host substrate's paths.
    fn local_override() -> bool {
        [
            "SUSI_HOME",
            "SUSI_XDG",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_CACHE_HOME",
        ]
        .iter()
        .any(|v| std::env::var_os(v).is_some())
    }

    fn get(key: &str) -> PathBuf {
        if Self::local_override() {
            return LocalDirs::get(key);
        }
        Self::fetch_paths()
            .remove(key)
            .unwrap_or_else(|| LocalDirs::get(key))
    }

    /// User home directory.
    #[must_use]
    pub fn home_dir() -> PathBuf {
        Self::get("home_dir")
    }
    /// Substrate config dir (`~/.susi` legacy or the platform config dir).
    #[must_use]
    pub fn config_dir() -> PathBuf {
        Self::get("config_dir")
    }
    /// Substrate data dir (`~/.susi` legacy or the platform data dir).
    #[must_use]
    pub fn data_dir() -> PathBuf {
        Self::get("data_dir")
    }
    /// Substrate cache dir (`~/.susi` legacy or the platform cache dir).
    #[must_use]
    pub fn cache_dir() -> PathBuf {
        Self::get("cache_dir")
    }
    /// Host substrate root the daemon binds to (the data dir). This is
    /// **not** a project workspace — CLI intents use the caller's cwd.
    #[must_use]
    pub fn substrate_home() -> PathBuf {
        Self::get("substrate_home")
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    a == b
        || matches!(
            (std::fs::canonicalize(a), std::fs::canonicalize(b)),
            (Ok(x), Ok(y)) if x == y
        )
}

/// Host API bearer token (`<config_dir>/api_token`), when seeded. The
/// susi-config and susi-sandbox leaf services require it on every request.
#[must_use]
pub fn host_token() -> Option<String> {
    std::fs::read_to_string(SusiDirs::config_dir().join("api_token"))
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// Constant-time check of an `Authorization` header value against the host
/// token. Fails closed when no token is seeded.
#[must_use]
pub fn bearer_authorized(header: Option<&str>) -> bool {
    let (Some(expected), Some(presented)) =
        (host_token(), header.and_then(|h| h.strip_prefix("Bearer ")))
    else {
        return false;
    };
    let (a, b) = (presented.trim().as_bytes(), expected.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Supervisor bearer check for leaf services the daemon spawns: the token
/// arrives in `SUSI_HOST_TOKEN`. A bare instance started without it (local
/// dev, CI harness) stays open; with it, the header must match in constant
/// time. Contrast [`bearer_authorized`], which fails closed.
#[must_use]
pub fn supervisor_bearer_authorized(header: Option<&str>) -> bool {
    let Some(expected) = std::env::var("SUSI_HOST_TOKEN")
        .ok()
        .filter(|t| !t.trim().is_empty())
    else {
        return true;
    };
    let Some(presented) = header.and_then(|h| h.strip_prefix("Bearer ")) else {
        return false;
    };
    let (a, b) = (presented.trim().as_bytes(), expected.trim().as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Local XDG/legacy resolver: the rule the service answers with and the
/// fallback every client uses when the service is unreachable.
struct LocalDirs;

impl LocalDirs {
    fn get(key: &str) -> PathBuf {
        match key {
            "home_dir" => Self::home_dir(),
            "config_dir" => Self::config_dir(),
            "cache_dir" => Self::cache_dir(),
            "substrate_home" => Self::substrate_home(),
            _ => Self::data_dir(),
        }
    }

    /// `SUSI_HOME` selects a fully isolated instance root — the multi-instance
    /// knob: `SUSI_HOME=~/.susi-b SUSI_PORT_OFFSET=100 susi start` runs a
    /// second node beside the primary with its own config, lock, and state.
    ///
    /// Mandate 52: an inherited launch-time `SUSI_HOME` (e.g. `~/.susi-dev`)
    /// must not receive test writes. `scripts/check-hermetic-tests.sh` exports
    /// `SUSI_HERMETIC_FORBIDDEN` to the throwaway it set as `SUSI_HOME`; that
    /// path is ignored here so unit tests fall back to the (also throwaway)
    /// `HOME`/`XDG_*` the script provides. A test that points `SUSI_HOME` at a
    /// *different* absolute path still gets a real instance root.
    fn instance_root() -> Option<PathBuf> {
        let p = std::env::var_os("SUSI_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())?;
        if std::env::var_os("SUSI_HERMETIC_FORBIDDEN")
            .map(PathBuf::from)
            .is_some_and(|f| f == p)
        {
            return None;
        }
        Some(p)
    }

    fn legacy_base() -> PathBuf {
        Self::instance_root().unwrap_or_else(|| Self::home_dir().join(".susi"))
    }

    fn home_dir() -> PathBuf {
        xdg::home_dir().unwrap_or_else(|| PathBuf::from("."))
    }

    fn use_xdg() -> bool {
        if Self::instance_root().is_some() {
            return false;
        }
        if Self::legacy_present(&Self::legacy_base()) {
            std::env::var("SUSI_XDG")
                .map(|v| v == "1" || v == "true")
                .unwrap_or(false)
        } else {
            true
        }
    }

    /// Whether the legacy `~/.susi` root existed the first time this process
    /// resolved it. Pinned per path: workspace-scoped state (`<ws>/.susi`)
    /// creates `~/.susi` whenever the workspace is `$HOME`, and re-checking
    /// on every call would then flip an XDG install to the legacy layout
    /// mid-process, so state written before the flip is read from a
    /// different file after it. Keyed by path (not one global answer) so a
    /// repointed `HOME` is decided afresh; `SUSI_XDG` stays live.
    fn legacy_present(legacy: &Path) -> bool {
        static SEEN: OnceLock<Mutex<HashMap<PathBuf, bool>>> = OnceLock::new();
        let mut seen = SEEN
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *seen
            .entry(legacy.to_path_buf())
            .or_insert_with(|| legacy.is_dir())
    }

    #[must_use]
    fn config_dir() -> PathBuf {
        if Self::use_xdg() {
            if let Some(p) = xdg::config_dir() {
                return p;
            }
        }
        Self::legacy_base()
    }

    #[must_use]
    fn data_dir() -> PathBuf {
        if Self::use_xdg() {
            if let Some(p) = xdg::data_dir() {
                return p;
            }
        }
        Self::legacy_base()
    }

    #[must_use]
    fn cache_dir() -> PathBuf {
        if Self::use_xdg() {
            if let Some(p) = xdg::cache_dir() {
                return p;
            }
        }
        Self::legacy_base()
    }

    /// Host substrate root the background daemon is always bound to
    /// (`~/.susi` or the XDG data dir). This is **not** a project workspace —
    /// CLI intents use the caller's cwd; the daemon only owns host-global
    /// state (models, ports, lock, rediscovery).
    #[must_use]
    fn substrate_home() -> PathBuf {
        Self::data_dir()
    }
}

pub mod loopback;
pub mod ports;
#[doc(hidden)]
pub mod test_env;
mod xdg;
pub mod zc_ports_file;

/// Percent-encode a string for a query component (RFC 3986 unreserved
/// plus the extra bytes in `keep`). Used by live search and sandbox IPC
/// so those callers do not each keep a forked encoder.
#[must_use]
pub fn percent_encode(s: &str, keep: &[u8]) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        let unreserved = matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~')
            || keep.contains(&b);
        if unreserved {
            out.push(b as char);
        } else if b == b' ' {
            out.push_str("%20");
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Query-string component encoding (spaces and commas encoded).
#[must_use]
pub fn percent_encode_query(s: &str) -> String {
    percent_encode(s, &[])
}

/// Path encoding that leaves `/` intact (sandbox IPC workspace paths).
#[must_use]
pub fn percent_encode_path(s: &str) -> String {
    percent_encode(s, b"/")
}

/// The host path contract as the `susi-paths` service answers it: always
/// the local resolver (the service *is* the source of truth).
#[must_use]
pub fn local_paths_json() -> serde_json::Value {
    serde_json::json!({
        "home_dir": LocalDirs::home_dir(),
        "config_dir": LocalDirs::config_dir(),
        "data_dir": LocalDirs::data_dir(),
        "cache_dir": LocalDirs::cache_dir(),
        "substrate_home": LocalDirs::substrate_home(),
    })
}

/// The effective host port contract (after the instance offset).
#[must_use]
pub fn ports_json() -> serde_json::Value {
    serde_json::json!({
        "gmcp": ports::effective(ports::GMCP),
        "gemi": ports::effective(ports::GEMI),
        "udp_discovery": ports::effective(ports::UDP_DISCOVERY),
        "gmcp_http": ports::effective(ports::GMCP_HTTP),
        "a2a_http": ports::effective(ports::A2A_HTTP),
    })
}

/// `true` while this process IS the substrate service (daemon or a leaf
/// service): local client surfaces resolve locally and never post back to
/// themselves. One flag in the leaf crate — `susi-error`, `susi-config`,
/// and `susi-sandbox-client` delegate their `enter_service_mode` here.
static SERVICE_MODE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Marks this process as the substrate service.
pub fn enter_service_mode() {
    SERVICE_MODE.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// `true` when running as the substrate service.
#[must_use]
pub fn is_service_mode() -> bool {
    SERVICE_MODE.load(std::sync::atomic::Ordering::Relaxed)
}

/// Serialises tests that read or mutate process-wide env (`HOME`, `SUSI_HOME`).
#[cfg(test)]
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_contract_ports_are_stable() {
        assert_eq!(ports::GMCP, 9090);
        assert_eq!(ports::GEMI, 9091);
        assert_eq!(ports::UDP_DISCOVERY, 9092);
        assert_eq!(ports::GMCP_HTTP, 9093);
        assert_eq!(ports::A2A_HTTP, 9094);
        assert_eq!(ports::ALL.len(), 5);
        let mut seen = std::collections::BTreeSet::new();
        for (port, _) in ports::ALL {
            assert!(seen.insert(port), "duplicate host-contract port {port}");
        }
    }

    #[test]
    fn substrate_home_is_not_a_project_cwd() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = SusiDirs::substrate_home();
        // A pinned instance root (`SUSI_HOME`, e.g. a dev instance) is the
        // substrate home verbatim, whatever it is named.
        if let Some(root) = std::env::var_os("SUSI_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
        {
            assert_eq!(home, root);
            return;
        }
        assert!(
            home.ends_with(".susi") || home.to_string_lossy().contains("susi"),
            "substrate_home should be the host substrate root, got {}",
            home.display()
        );
        // Distinct from a typical project folder under github.com/...
        assert!(
            !home.to_string_lossy().contains("/github.com/"),
            "{}",
            home.display()
        );
    }

    #[test]
    fn percent_encode_query_and_path() {
        assert_eq!(percent_encode_query("Chennai, India"), "Chennai%2C%20India");
        assert_eq!(percent_encode_path("/tmp/my ws"), "/tmp/my%20ws");
    }
}

#[cfg(test)]
mod hermetic_forbidden_tests {
    use super::*;
    #[test]
    fn hermetic_forbidden_susi_home_is_ignored() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let forbidden =
            std::env::temp_dir().join(format!("susi_hermetic_forbid_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&forbidden);
        let home = std::env::temp_dir().join(format!("susi_hermetic_home_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&home);
        let prev_home = std::env::var_os("HOME");
        let prev_susi = std::env::var_os("SUSI_HOME");
        let prev_forbid = std::env::var_os("SUSI_HERMETIC_FORBIDDEN");
        let prev_xdg = std::env::var_os("XDG_CONFIG_HOME");
        std::env::set_var("HOME", &home);
        std::env::set_var("SUSI_HOME", &forbidden);
        std::env::set_var("SUSI_HERMETIC_FORBIDDEN", &forbidden);
        std::env::remove_var("XDG_CONFIG_HOME");
        // Without a pre-existing ~/.susi, XDG layout applies under HOME.
        let cfg = LocalDirs::config_dir();
        assert_ne!(
            cfg, forbidden,
            "forbidden SUSI_HOME must not be the config root"
        );
        // An explicit different SUSI_HOME still pins the instance.
        let other =
            std::env::temp_dir().join(format!("susi_hermetic_other_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&other);
        std::env::set_var("SUSI_HOME", &other);
        assert_eq!(LocalDirs::config_dir(), other);
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match prev_susi {
            Some(v) => std::env::set_var("SUSI_HOME", v),
            None => std::env::remove_var("SUSI_HOME"),
        }
        match prev_forbid {
            Some(v) => std::env::set_var("SUSI_HERMETIC_FORBIDDEN", v),
            None => std::env::remove_var("SUSI_HERMETIC_FORBIDDEN"),
        }
        match prev_xdg {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        let _ = std::fs::remove_dir_all(&forbidden);
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&other);
    }
}
