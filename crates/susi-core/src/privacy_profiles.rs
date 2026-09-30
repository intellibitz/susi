//! Per-workspace privacy posture profiles with egress allowlists.
//!
//! Named profiles — `air-gapped`, `local-only`, `allowlisted-clouds`,
//! `open` — are selected per workspace and consulted by the router BEFORE
//! an outbound call, alongside `mac_policy`'s global posture. A profile can
//! only tighten egress, never loosen what `mac_policy::egress_permitted`
//! already denies.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

use crate::mac_policy::url_stays_local;
use crate::susi_error::{EaiError, EaiResult};
use crate::zc_egress_allowlist::EgressAllowlist;

/// Egress posture a profile imposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Posture {
    /// No outbound traffic at all beyond loopback.
    AirGapped,
    /// This host plus LAN/private ranges (see `url_stays_local`).
    LocalOnly,
    /// Local traffic plus the hosts in the profile's allowlist.
    AllowlistedClouds,
    /// No profile-level restriction (mac_policy still applies).
    Open,
}

/// A named privacy profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrivacyProfile {
    pub name: String,
    pub posture: Posture,
    /// Additional permitted egress hosts for `AllowlistedClouds`.
    #[serde(default)]
    pub allowlist: EgressAllowlist,
}

/// The built-in profiles. `allowlisted-clouds` starts with an empty
/// allowlist — workspaces add vendor hosts explicitly.
#[must_use]
pub fn builtin_profiles() -> Vec<PrivacyProfile> {
    vec![
        PrivacyProfile {
            name: "air-gapped".to_string(),
            posture: Posture::AirGapped,
            allowlist: EgressAllowlist::default(),
        },
        PrivacyProfile {
            name: "local-only".to_string(),
            posture: Posture::LocalOnly,
            allowlist: EgressAllowlist::default(),
        },
        PrivacyProfile {
            name: "allowlisted-clouds".to_string(),
            posture: Posture::AllowlistedClouds,
            allowlist: EgressAllowlist::default(),
        },
        PrivacyProfile {
            name: "open".to_string(),
            posture: Posture::Open,
            allowlist: EgressAllowlist::default(),
        },
    ]
}

/// Strip `url` to its host for allowlist matching.
#[must_use]
pub fn url_host(url: &str) -> String {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = authority.rsplit('@').next().unwrap_or_default();
    if let Some(b) = host_port.strip_prefix('[') {
        b.split(']').next().unwrap_or_default().to_string()
    } else {
        host_port.split(':').next().unwrap_or_default().to_string()
    }
}

/// Whether `profile` permits egress to `url`. Loopback is always allowed
/// except under `AirGapped` (which still allows loopback — the model has
/// to be reachable); air-gapped refuses anything beyond loopback.
#[must_use]
pub fn profile_permits(profile: &PrivacyProfile, url: &str) -> bool {
    match profile.posture {
        Posture::AirGapped => is_loopback(url),
        Posture::LocalOnly => url_stays_local(url),
        Posture::AllowlistedClouds => {
            url_stays_local(url) || profile.allowlist.hosts.contains(&url_host(url))
        }
        Posture::Open => true,
    }
}

fn is_loopback(url: &str) -> bool {
    let h = url_host(url).to_ascii_lowercase();
    h == "localhost"
        || h.ends_with(".localhost")
        || h == "::1"
        || h.parse::<std::net::Ipv4Addr>()
            .is_ok_and(|ip| ip.is_loopback())
        || h.parse::<std::net::Ipv6Addr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Workspace → profile-name mapping, persisted in the config dir.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct WorkspaceProfiles {
    /// Workspace path → profile name (must exist in `builtin_profiles` or
    /// the caller's custom set).
    #[serde(default)]
    pub by_workspace: BTreeMap<String, String>,
    /// Profile used when a workspace has no selection. Default is the
    /// safest reasonable posture.
    #[serde(default = "default_profile_name")]
    pub default_profile: String,
    /// Custom profiles layered over the builtins.
    #[serde(default)]
    pub custom: Vec<PrivacyProfile>,
}

fn default_profile_name() -> String {
    "local-only".to_string()
}

impl WorkspaceProfiles {
    #[must_use]
    pub fn new() -> Self {
        Self {
            by_workspace: BTreeMap::new(),
            default_profile: default_profile_name(),
            custom: Vec::new(),
        }
    }

    /// Select `profile` for `workspace`; fails on unknown profile names.
    pub fn select(&mut self, workspace: &str, profile: &str) -> EaiResult<()> {
        if self.profile(profile).is_none() {
            return Err(EaiError::config(format!(
                "unknown privacy profile '{profile}'"
            )));
        }
        self.by_workspace
            .insert(workspace.to_string(), profile.to_string());
        Ok(())
    }

    /// Resolve a profile by name (custom shadows builtin).
    #[must_use]
    pub fn profile(&self, name: &str) -> Option<PrivacyProfile> {
        self.custom
            .iter()
            .find(|p| p.name == name)
            .cloned()
            .or_else(|| builtin_profiles().into_iter().find(|p| p.name == name))
    }

    /// The profile in force for `workspace`.
    #[must_use]
    pub fn for_workspace(&self, workspace: &str) -> PrivacyProfile {
        let name = self
            .by_workspace
            .get(workspace)
            .map_or(self.default_profile.as_str(), String::as_str);
        self.profile(name)
            .unwrap_or_else(|| builtin_profiles()[1].clone())
    }

    /// The router-side check: may a request from `workspace` reach `url`?
    /// Both the profile AND the global mac posture must permit it.
    #[must_use]
    pub fn egress_permitted(&self, workspace: &str, url: &str) -> bool {
        profile_permits(&self.for_workspace(workspace), url)
            && crate::mac_policy::egress_permitted(url)
    }

    /// Persist as pretty JSON.
    ///
    /// # Errors
    /// [`EaiError::io`] on encode/write failures.
    pub fn save(&self, path: &Path) -> EaiResult<()> {
        let text = serde_json::to_string_pretty(self).map_err(|e| EaiError::io(e.to_string()))?;
        std::fs::write(path, text).map_err(|e| EaiError::io(e.to_string()))
    }

    /// Load; missing file yields defaults.
    ///
    /// # Errors
    /// [`EaiError::io`] on unreadable/corrupt files.
    pub fn load(path: &Path) -> EaiResult<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| EaiError::io(e.to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::new()),
            Err(e) => Err(EaiError::io(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prof(name: &str) -> PrivacyProfile {
        builtin_profiles()
            .into_iter()
            .find(|p| p.name == name)
            .unwrap_or_else(|| panic!("profile {name}"))
    }

    #[test]
    fn privacy_profiles_air_gapped_allows_only_loopback() {
        let p = prof("air-gapped");
        assert!(profile_permits(&p, "http://127.0.0.1:11434/api"));
        assert!(profile_permits(&p, "http://localhost:1234/v1"));
        assert!(!profile_permits(&p, "http://192.168.1.20:11434"));
        assert!(!profile_permits(&p, "https://api.openai.com/v1"));
    }

    #[test]
    fn privacy_profiles_local_only_allows_lan_not_cloud() {
        let p = prof("local-only");
        assert!(profile_permits(&p, "http://192.168.1.20:11434"));
        assert!(profile_permits(&p, "http://printer.local/x"));
        assert!(!profile_permits(&p, "https://api.openai.com/v1"));
    }

    #[test]
    fn privacy_profiles_allowlisted_clouds() {
        let mut p = prof("allowlisted-clouds");
        p.allowlist.hosts.insert("api.openai.com".to_string());
        assert!(profile_permits(&p, "https://api.openai.com/v1/chat"));
        assert!(!profile_permits(&p, "https://api.anthropic.com/v1"));
        assert!(profile_permits(&p, "http://127.0.0.1:11434"));
    }

    #[test]
    fn privacy_profiles_workspace_selection_and_fallback() {
        let mut w = WorkspaceProfiles::new();
        assert!(w.select("/ws/secret", "air-gapped").is_ok());
        assert!(w.select("/ws/bad", "nonsense").is_err());
        assert_eq!(w.for_workspace("/ws/secret").posture, Posture::AirGapped);
        assert_eq!(w.for_workspace("/ws/other").posture, Posture::LocalOnly);
    }

    #[test]
    fn privacy_profiles_egress_permitted_binds_workspace() {
        let mut w = WorkspaceProfiles::new();
        w.select("/ws/secret", "air-gapped")
            .unwrap_or_else(|e| panic!("select: {e}"));
        assert!(w.egress_permitted("/ws/secret", "http://127.0.0.1:11434/api"));
        assert!(!w.egress_permitted("/ws/secret", "https://api.openai.com"));
        // default workspace is local-only
        assert!(w.egress_permitted("/ws/lan", "http://10.0.0.5:8080"));
        assert!(!w.egress_permitted("/ws/lan", "https://api.openai.com"));
    }

    #[test]
    fn privacy_profiles_custom_shadows_builtin() {
        let mut w = WorkspaceProfiles::new();
        w.custom.push(PrivacyProfile {
            name: "local-only".to_string(),
            posture: Posture::AirGapped, // tightened variant
            allowlist: EgressAllowlist::default(),
        });
        assert_eq!(
            w.profile("local-only").map(|p| p.posture),
            Some(Posture::AirGapped)
        );
    }

    #[test]
    fn privacy_profiles_persist_roundtrip() {
        let dir = std::env::temp_dir().join(format!("privprof-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("p.json");
        let mut w = WorkspaceProfiles::new();
        w.select("/a", "air-gapped")
            .unwrap_or_else(|e| panic!("{e}"));
        w.save(&path).unwrap();
        let loaded = WorkspaceProfiles::load(&path).unwrap();
        assert_eq!(loaded.for_workspace("/a").posture, Posture::AirGapped);
        assert!(WorkspaceProfiles::load(&dir.join("none.json"))
            .unwrap()
            .by_workspace
            .is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn privacy_profiles_url_host_parsing() {
        assert_eq!(url_host("https://api.openai.com:443/v1"), "api.openai.com");
        assert_eq!(url_host("http://user@host.local:80/x"), "host.local");
        assert_eq!(url_host("http://[::1]:11434/"), "::1");
    }
}
