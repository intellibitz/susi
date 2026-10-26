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
