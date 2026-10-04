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
    pub fn global() -> &'static parking_lot::RwLock<MemoryStore> {
        static INSTANCE: OnceLock<parking_lot::RwLock<MemoryStore>> = OnceLock::new();
        INSTANCE.get_or_init(|| parking_lot::RwLock::new(MemoryStore::default()))
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
