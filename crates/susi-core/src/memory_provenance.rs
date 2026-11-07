//! Authoritative memory provenance and retention (VC-201-081).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(u8)]
pub enum DataClassification {
    Public = 0,
    #[default]
    Internal = 1,
    Confidential = 2,
    Restricted = 3,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "details")]
pub enum ProvenanceSource {
    Evidence {
        receipt_id: String,
        verifier: String,
    },
    ModelAssertion {
        model: String,
        prompt_hash: String,
    },
    User {
        user_id: String,
    },
    System {
        subsystem: String,
    },
    Custom(String),
}

impl ProvenanceSource {
    #[must_use]
    pub fn is_evidence(&self) -> bool {
        matches!(self, Self::Evidence { .. })
    }

    #[must_use]
    pub fn is_model_assertion(&self) -> bool {
        matches!(self, Self::ModelAssertion { .. })
    }

    #[must_use]
    pub fn authoritative_name(&self) -> &str {
        match self {
            Self::Evidence { .. } => "evidence",
            Self::ModelAssertion { .. } => "model-assertion",
            Self::User { .. } => "user",
            Self::System { .. } => "system",
            Self::Custom(s) => s.as_str(),
        }
    }
}

impl From<&str> for ProvenanceSource {
    fn from(s: &str) -> Self {
        match s {
            "user" => Self::User {
                user_id: "default-user".into(),
            },
            "evidence" => Self::Evidence {
                receipt_id: "default-receipt".into(),
                verifier: "default-verifier".into(),
            },
            "model" | "model-assertion" => Self::ModelAssertion {
                model: "default-model".into(),
                prompt_hash: "default-hash".into(),
            },
            "system" => Self::System {
                subsystem: "core".into(),
            },
            other => Self::Custom(other.to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub source: ProvenanceSource,
    #[serde(default)]
    pub workspace: String,
    #[serde(default)]
    pub receipt: Option<String>,
    #[serde(default)]
    pub revision: String,
    #[serde(default)]
    pub classification: DataClassification,
    #[serde(default)]
    pub owner: String,
    pub recorded_unix: u64,
    pub retention_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryAccessError {
    Expired {
        expired_at: u64,
        now: u64,
    },
    Unauthorized {
        owner: String,
        caller: String,
    },
    ClassificationDenied {
        required: DataClassification,
        granted: DataClassification,
    },
}

impl std::fmt::Display for MemoryAccessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Expired { expired_at, now } => {
                write!(f, "record expired at {expired_at} (current time {now})")
            }
            Self::Unauthorized { owner, caller } => {
                write!(f, "access denied: owned by '{owner}', caller is '{caller}'")
            }
            Self::ClassificationDenied { required, granted } => {
                write!(
                    f,
                    "classification denied: required {required:?}, caller granted {granted:?}"
                )
            }
        }
    }
}

impl std::error::Error for MemoryAccessError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub id: String,
    pub body: String,
    pub provenance: Provenance,
}

impl MemoryRecord {
    pub fn new(id: impl Into<String>, body: impl Into<String>, provenance: Provenance) -> Self {
        Self {
            id: id.into(),
            body: body.into(),
            provenance,
        }
    }

    #[must_use]
    pub fn expired(&self, now: u64) -> bool {
        if self.provenance.retention_secs == 0 {
            // Retention of 0 denotes permanent/indefinite retention; never born-expired.
            return false;
        }
        now.saturating_sub(self.provenance.recorded_unix) >= self.provenance.retention_secs
    }

    #[must_use]
    pub fn authoritative_source(&self) -> &str {
        self.provenance.source.authoritative_name()
    }

    /// Read the memory body with strict expiration and authorization gates.
    pub fn read_body<'a>(
        &'a self,
        caller: &str,
        clearance: DataClassification,
        now: u64,
    ) -> Result<&'a str, MemoryAccessError> {
        if self.expired(now) {
            return Err(MemoryAccessError::Expired {
                expired_at: self.provenance.recorded_unix + self.provenance.retention_secs,
                now,
            });
        }
        if !self.provenance.owner.is_empty()
            && self.provenance.owner != caller
            && caller != "admin"
            && caller != "system"
        {
            return Err(MemoryAccessError::Unauthorized {
                owner: self.provenance.owner.clone(),
                caller: caller.to_string(),
            });
        }
        if self.provenance.classification > clearance {
            return Err(MemoryAccessError::ClassificationDenied {
                required: self.provenance.classification,
                granted: clearance,
            });
        }
        Ok(&self.body)
    }
}

#[derive(Debug, Default)]
pub struct MemoryStore {
    records: BTreeMap<String, MemoryRecord>,
}

impl MemoryStore {
    pub fn global() -> &'static std::sync::RwLock<MemoryStore> {
        static INSTANCE: OnceLock<std::sync::RwLock<MemoryStore>> = OnceLock::new();
        INSTANCE.get_or_init(|| std::sync::RwLock::new(MemoryStore::default()))
    }

    pub fn insert(&mut self, record: MemoryRecord) {
        self.records.insert(record.id.clone(), record);
    }

    pub fn get_record(&self, id: &str) -> Option<&MemoryRecord> {
        self.records.get(id)
    }

    pub fn read(
        &self,
        id: &str,
        caller: &str,
        clearance: DataClassification,
        now: u64,
    ) -> Result<&str, MemoryAccessError> {
        let record = self
            .records
            .get(id)
            .ok_or_else(|| MemoryAccessError::Unauthorized {
                owner: "nonexistent".into(),
                caller: caller.into(),
            })?;
        record.read_body(caller, clearance, now)
    }
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    fn sample_provenance(
        owner: &str,
        classification: DataClassification,
        retention_secs: u64,
    ) -> Provenance {
        Provenance {
            source: ProvenanceSource::from("evidence"),
            workspace: "ws-1".into(),
            receipt: Some("rcpt-1".into()),
            revision: "r1".into(),
            classification,
            owner: owner.into(),
            recorded_unix: 1_000,
            retention_secs,
        }
    }

    #[test]
    fn provenance_source_from_str_and_authoritative_names_match() {
        let ev = ProvenanceSource::from("evidence");
        assert!(ev.is_evidence());
        assert!(!ev.is_model_assertion());
        assert_eq!(ev.authoritative_name(), "evidence");

        let ma = ProvenanceSource::from("model-assertion");
        assert!(ma.is_model_assertion());
        assert_eq!(ma.authoritative_name(), "model-assertion");

        let custom = ProvenanceSource::from("audit-hook");
        assert_eq!(custom.authoritative_name(), "audit-hook");
    }

    #[test]
    fn memory_record_expired_respects_zero_indefinite_and_elapsed_ttl() {
        let permanent = MemoryRecord::new(
            "perm",
            "forever",
            sample_provenance("alice", DataClassification::Public, 0),
        );
        assert!(!permanent.expired(999_999_999));

        let bounded = MemoryRecord::new(
            "ttl",
            "ephemeral",
            sample_provenance("alice", DataClassification::Public, 60),
        );
        assert!(!bounded.expired(1_059));
        assert!(bounded.expired(1_060));
        let err = bounded
            .read_body("alice", DataClassification::Public, 1_060)
            .unwrap_err();
        assert!(err.to_string().contains("expired"));
    }

    #[test]
    fn read_body_enforces_owner_authorization_and_admin_override() {
        let rec = MemoryRecord::new(
            "sec-1",
            "payload",
            sample_provenance("alice", DataClassification::Internal, 300),
        );
        assert_eq!(
            rec.read_body("alice", DataClassification::Internal, 1_100)
                .unwrap(),
            "payload"
        );
        assert_eq!(
            rec.read_body("admin", DataClassification::Internal, 1_100)
                .unwrap(),
            "payload"
        );
        let unauth = rec
            .read_body("bob", DataClassification::Restricted, 1_100)
            .unwrap_err();
        assert!(matches!(unauth, MemoryAccessError::Unauthorized { .. }));
    }

    #[test]
    fn memory_store_insert_and_read_enforce_classification_clearance() {
        let mut store = MemoryStore::default();
        store.insert(MemoryRecord::new(
            "conf-1",
            "secret-body",
            sample_provenance("alice", DataClassification::Confidential, 300),
        ));
        assert!(store.get_record("conf-1").is_some());
        let denied = store
            .read("conf-1", "alice", DataClassification::Internal, 1_100)
            .unwrap_err();
        assert!(matches!(
            denied,
            MemoryAccessError::ClassificationDenied { .. }
        ));
        assert_eq!(
            store
                .read("conf-1", "alice", DataClassification::Restricted, 1_100)
                .unwrap(),
            "secret-body"
        );
    }

    #[test]
    fn authoritative_source_and_error_display_messages_format_all_variants() {
        let user_src = ProvenanceSource::from("user");
        assert_eq!(user_src.authoritative_name(), "user");
        let sys_src = ProvenanceSource::from("system");
        assert_eq!(sys_src.authoritative_name(), "system");

        let mut prov = sample_provenance("alice", DataClassification::Confidential, 120);
        prov.source = sys_src;
        let rec = MemoryRecord::new("sys-rec", "body", prov);
        assert_eq!(rec.authoritative_source(), "system");
        assert_eq!(
            rec.read_body("system", DataClassification::Confidential, 1_050)
                .unwrap(),
            "body"
        );

        let unauth_msg = MemoryAccessError::Unauthorized {
            owner: "alice".into(),
            caller: "mallory".into(),
        }
        .to_string();
        assert!(unauth_msg.contains("alice") && unauth_msg.contains("mallory"));

        let class_msg = MemoryAccessError::ClassificationDenied {
            required: DataClassification::Restricted,
            granted: DataClassification::Public,
        }
        .to_string();
        assert!(class_msg.contains("Restricted") && class_msg.contains("Public"));
    }
}
