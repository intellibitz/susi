//! First-class MCP extension framework (VC-203-016).
//!
//! One typed registry owns extension discovery, negotiation, versioning,
//! isolation, disablement, collision and downgrade rules. Core protocol code
//! only asks the registry for its capability map; it never special-cases an
//! extension id. Unknown peer extensions are carried as opaque data and
//! round-trip verbatim.
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Extension capability map as carried on the wire (`capabilities.extensions`).
pub type ExtensionMap = BTreeMap<String, Map<String, Value>>;

/// Official extensions are published by the MCP project; experimental ones are not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtensionStatus {
    Official,
    Experimental,
}

impl ExtensionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Official => "official",
            Self::Experimental => "experimental",
        }
    }
}

/// Typed failure of registration or negotiation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExtensionError {
    /// The id is not `vendor-prefix/name` namespaced.
    NotNamespaced(String),
    /// Two registrations claim the same id.
    Collision(String),
    /// A peer requires an extension this server does not (or no longer) serve.
    RequiredUnsupported(Vec<String>),
    /// A peer requires a version older than the served one (downgrade).
    Downgrade {
        id: String,
        required: u32,
        served: u32,
    },
}

impl ExtensionError {
    pub fn message(&self) -> String {
        match self {
            Self::NotNamespaced(id) => format!("extension id '{id}' is not namespaced"),
            Self::Collision(id) => format!("extension '{id}' is already registered"),
            Self::RequiredUnsupported(ids) => {
                format!("required extensions unsupported: {}", ids.join(", "))
            }
            Self::Downgrade {
                id,
                required,
                served,
            } => format!(
                "extension '{id}' downgrade refused: requires v{required}, serves v{served}"
            ),
        }
    }
}

/// One registered extension.
#[derive(Clone, Debug)]
pub struct ExtensionDescriptor {
    pub id: String,
    pub status: ExtensionStatus,
    pub version: u32,
    pub settings: Map<String, Value>,
    pub enabled: bool,
}

/// Result of negotiating with a peer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Negotiated {
    /// Extensions both sides support, with the server's settings.
    pub active: ExtensionMap,
    /// Peer extensions this server does not know; retained verbatim.
    pub unknown: ExtensionMap,
}

#[derive(Clone, Debug, Default)]
pub struct ExtensionRegistry {
    entries: BTreeMap<String, ExtensionDescriptor>,
}

/// Official extensions the pinned authorities publish.
pub const OFFICIAL_EXTENSIONS: &[&str] = &[
    "io.modelcontextprotocol/tasks",
    "io.modelcontextprotocol/ui",
    "io.modelcontextprotocol/enterprise-managed-authorization",
];

fn namespaced(id: &str) -> bool {
    match id.split_once('/') {
        Some((ns, name)) => !ns.is_empty() && !name.is_empty() && !name.contains('/'),
        None => false,
    }
}

impl ExtensionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registry preloaded with the extensions the shipped runtime serves.
    pub fn shipped() -> Self {
        let mut registry = Self::new();
        // Static, namespaced, unique ids: registration cannot fail.
        let _ = registry.register(
            "io.modelcontextprotocol/tasks",
            ExtensionStatus::Official,
            1,
            Map::new(),
        );
        registry
    }

    pub fn register(
        &mut self,
        id: &str,
        status: ExtensionStatus,
        version: u32,
        settings: Map<String, Value>,
    ) -> Result<(), ExtensionError> {
        if !namespaced(id) {
            return Err(ExtensionError::NotNamespaced(id.to_string()));
        }
        if self.entries.contains_key(id) {
            return Err(ExtensionError::Collision(id.to_string()));
        }
        // Isolation: a registration owns its settings; nothing is shared.
        self.entries.insert(
            id.to_string(),
            ExtensionDescriptor {
                id: id.to_string(),
                status,
                version,
                settings,
                enabled: true,
            },
        );
        Ok(())
    }

    /// Disable (or re-enable) an extension; returns false for unknown ids.
    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> bool {
        match self.entries.get_mut(id) {
            Some(entry) => {
                entry.enabled = enabled;
                true
            }
            None => false,
        }
    }

    pub fn status(&self, id: &str) -> Option<ExtensionStatus> {
        self.entries.get(id).map(|e| e.status)
    }

    pub fn descriptors(&self) -> impl Iterator<Item = &ExtensionDescriptor> {
        self.entries.values()
    }

    /// Discovery: the capability map advertised on `server/discover`.
    /// Disabled extensions are absent; version is surfaced as `version`.
    pub fn capability_map(&self) -> ExtensionMap {
        self.entries
            .values()
            .filter(|e| e.enabled)
            .map(|e| {
                let mut settings = e.settings.clone();
                if e.version > 1 {
                    settings.insert("version".into(), Value::from(e.version));
                }
                (e.id.clone(), settings)
            })
            .collect()
    }

    /// Discovery with visible status, for operators and the watch process.
    pub fn status_report(&self) -> Value {
        Value::Array(
            self.entries
                .values()
                .map(|e| {
                    serde_json::json!({
                        "id": e.id,
                        "status": e.status.as_str(),
                        "version": e.version,
                        "enabled": e.enabled,
                    })
                })
                .collect(),
        )
    }

    /// Negotiate against a peer's extension map. `required` lists ids the
    /// peer cannot proceed without; an unsupported one fails explicitly.
    /// A peer-declared `version` lower than served is accepted (we serve
    /// the peer's older level); a *required* higher or lower pin that does
    /// not match the served version is refused as a downgrade.
    pub fn negotiate(
        &self,
        peer: &ExtensionMap,
        required: &[String],
    ) -> Result<Negotiated, ExtensionError> {
        let missing: Vec<String> = required
            .iter()
            .filter(|id| !self.entries.get(id.as_str()).is_some_and(|e| e.enabled))
            .cloned()
            .collect();
        if !missing.is_empty() {
            return Err(ExtensionError::RequiredUnsupported(missing));
        }
        let mut out = Negotiated::default();
        for (id, peer_settings) in peer {
            match self.entries.get(id) {
                Some(entry) if entry.enabled => {
                    if required.contains(id) {
                        let pinned = peer_settings
                            .get("version")
                            .and_then(Value::as_u64)
                            .unwrap_or(u64::from(entry.version));
                        if pinned != u64::from(entry.version) {
                            return Err(ExtensionError::Downgrade {
                                id: id.clone(),
                                required: u32::try_from(pinned).unwrap_or(u32::MAX),
                                served: entry.version,
                            });
                        }
                    }
                    let mut settings = entry.settings.clone();
                    if entry.version > 1 {
                        settings.insert("version".into(), Value::from(entry.version));
                    }
                    out.active.insert(id.clone(), settings);
                }
                Some(_) | None => {
                    out.unknown.insert(id.clone(), peer_settings.clone());
                }
            }
        }
        Ok(out)
    }

    /// Watch: official extensions the authorities publish that the registry
    /// has never seen. Each yields a roadmap work item title.
    pub fn watch(&self, published_official: &[&str]) -> Vec<String> {
        published_official
            .iter()
            .filter(|id| !self.entries.contains_key(**id))
            .map(|id| format!("[GMCP] Adopt newly published official extension {id}"))
            .collect()
    }
}

#[cfg(test)]
mod inline_tests {
    use super::*;

    #[test]
    fn register_validates_namespacing_and_prevents_duplicate_collisions() {
        let mut reg = ExtensionRegistry::new();
        assert_eq!(
            reg.register("unnamespaced", ExtensionStatus::Experimental, 1, Map::new()),
            Err(ExtensionError::NotNamespaced("unnamespaced".into()))
        );
        reg.register(
            "io.susi/custom",
            ExtensionStatus::Experimental,
            2,
            Map::new(),
        )
        .unwrap();
        assert_eq!(
            reg.register("io.susi/custom", ExtensionStatus::Official, 2, Map::new()),
            Err(ExtensionError::Collision("io.susi/custom".into()))
        );
        assert_eq!(
            reg.status("io.susi/custom"),
            Some(ExtensionStatus::Experimental)
        );
    }

    #[test]
    fn set_enabled_and_capability_map_omit_disabled_and_include_version_above_one() {
        let mut reg = ExtensionRegistry::shipped();
        reg.register("io.susi/v2ext", ExtensionStatus::Official, 2, Map::new())
            .unwrap();
        let caps = reg.capability_map();
        assert!(caps.contains_key("io.modelcontextprotocol/tasks"));
        assert_eq!(caps["io.susi/v2ext"]["version"], Value::from(2));

        assert!(reg.set_enabled("io.susi/v2ext", false));
        assert!(!reg.set_enabled("io.susi/missing", false));
        assert!(!reg.capability_map().contains_key("io.susi/v2ext"));
        assert!(reg.status_report().is_array());
    }

    #[test]
    fn negotiate_separates_active_and_unknown_extensions_and_rejects_downgrade() {
        let mut reg = ExtensionRegistry::new();
        reg.register("io.susi/ext", ExtensionStatus::Official, 2, Map::new())
            .unwrap();

        let mut peer = ExtensionMap::new();
        let mut v1 = Map::new();
        v1.insert("version".into(), Value::from(1));
        peer.insert("io.susi/ext".into(), v1);
        peer.insert("io.other/unknown".into(), Map::new());

        let downgrade = reg.negotiate(&peer, &["io.susi/ext".to_string()]);
        assert!(matches!(downgrade, Err(ExtensionError::Downgrade { .. })));

        let ok = reg.negotiate(&peer, &[]).unwrap();
        assert!(ok.active.contains_key("io.susi/ext"));
        assert!(ok.unknown.contains_key("io.other/unknown"));
    }

    #[test]
    fn watch_reports_unregistered_official_extensions_and_formats_error_messages() {
        let reg = ExtensionRegistry::shipped();
        let items = reg.watch(OFFICIAL_EXTENSIONS);
        assert_eq!(items.len(), 2);
        assert!(items
            .iter()
            .any(|s| s.contains("io.modelcontextprotocol/ui")));
        assert!(ExtensionError::RequiredUnsupported(vec!["a/b".into()])
            .message()
            .contains("a/b"));
    }

    #[test]
    fn negotiate_fails_closed_when_required_extension_is_disabled_or_missing() {
        let mut reg = ExtensionRegistry::shipped();
        assert!(reg.set_enabled("io.modelcontextprotocol/tasks", false));
        let err = reg
            .negotiate(
                &ExtensionMap::new(),
                &["io.modelcontextprotocol/tasks".to_string()],
            )
            .unwrap_err();
        assert_eq!(
            err,
            ExtensionError::RequiredUnsupported(vec!["io.modelcontextprotocol/tasks".to_string()])
        );
        assert_eq!(reg.status("io.susi/nonexistent"), None);
    }
}

// ---- Wave 13 (T-CLAUDE-412) structural invariant verifiers ----

impl ExtensionDescriptor {
    /// A descriptor carries a `vendor/name` namespaced id, a version of at least 1,
    /// and a status consistent with the published official-extensions list: an id in
    /// [`OFFICIAL_EXTENSIONS`] is Official, any other id is Experimental.
    pub fn verify_descriptor_invariants(&self) -> Result<(), String> {
        if !namespaced(&self.id) {
            return Err(format!(
                "ExtensionDescriptor id `{}` is not vendor/name namespaced",
                self.id
            ));
        }
        if self.version < 1 {
            return Err("ExtensionDescriptor version must be at least 1".to_string());
        }
        let official = OFFICIAL_EXTENSIONS.contains(&self.id.as_str());
        match (official, self.status) {
            (true, ExtensionStatus::Official) => Ok(()),
            (false, ExtensionStatus::Experimental) => Ok(()),
            (true, ExtensionStatus::Experimental) => Err(format!(
                "official extension `{}` must carry Official status",
                self.id
            )),
            (false, ExtensionStatus::Official) => Err(format!(
                "non-official extension `{}` must carry Experimental status",
                self.id
            )),
        }
    }
}

impl Negotiated {
    /// A negotiation never classifies the same extension id as both active and unknown.
    pub fn verify_negotiation_invariants(&self) -> Result<(), String> {
        for id in self.active.keys() {
            if self.unknown.contains_key(id) {
                return Err(format!("extension `{id}` is both active and unknown"));
            }
        }
        Ok(())
    }
}

impl ExtensionRegistry {
    /// Every entry is keyed by its own id and verifies as a descriptor.
    pub fn verify_registry_invariants(&self) -> Result<(), String> {
        for (key, desc) in &self.entries {
            if key != &desc.id {
                return Err(format!(
                    "registry key `{key}` does not match descriptor id `{}`",
                    desc.id
                ));
            }
            desc.verify_descriptor_invariants()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod wave13_verifier_tests {
    use super::*;

    #[test]
    fn descriptor_negotiated_and_registry_verifiers_reject_corrupt_state() {
        let official = ExtensionDescriptor {
            id: "io.modelcontextprotocol/tasks".to_string(),
            status: ExtensionStatus::Official,
            version: 1,
            settings: Map::new(),
            enabled: true,
        };
        assert_eq!(official.verify_descriptor_invariants(), Ok(()));

        let mut mislabelled = official.clone();
        mislabelled.status = ExtensionStatus::Experimental;
        assert!(mislabelled.verify_descriptor_invariants().is_err());

        let mut not_namespaced = official.clone();
        not_namespaced.id = "tasks".to_string();
        assert!(not_namespaced.verify_descriptor_invariants().is_err());

        let experimental = ExtensionDescriptor {
            id: "com.acme/widget".to_string(),
            status: ExtensionStatus::Experimental,
            version: 2,
            settings: Map::new(),
            enabled: true,
        };
        assert_eq!(experimental.verify_descriptor_invariants(), Ok(()));
        let mut bad_version = experimental.clone();
        bad_version.version = 0;
        assert!(bad_version.verify_descriptor_invariants().is_err());

        assert_eq!(
            ExtensionRegistry::shipped().verify_registry_invariants(),
            Ok(())
        );

        let mut active = ExtensionMap::new();
        active.insert("com.acme/widget".to_string(), Map::new());
        let mut unknown = ExtensionMap::new();
        unknown.insert("org.other/thing".to_string(), Map::new());
        let good = Negotiated {
            active: active.clone(),
            unknown,
        };
        assert_eq!(good.verify_negotiation_invariants(), Ok(()));
        let clash = Negotiated {
            active: active.clone(),
            unknown: active,
        };
        assert!(clash.verify_negotiation_invariants().is_err());
    }
}
