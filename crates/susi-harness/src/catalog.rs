//! The price catalogue.
//!
//! Prices are data, not code: a catalogue file lists each model's per-million
//! rates in micro-USD, and the ledger stores the rate that was in force when a
//! call ran. A model the catalogue cannot price still runs: a working model is
//! never refused for lack of costing. Its tokens are recorded at a zero price,
//! so no budget counts that spend, and an account with a spend cap is refused
//! it at the ingress.

use crate::pricing::ModelPrice;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use susi_error::{EaiError, EaiResult};

/// The schema tag a catalogue file must carry.
pub const PRICE_SCHEMA: &str = "susi/harness/prices/v1";

/// A set of model prices, keyed by the model id the provider is called with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceCatalogue {
    /// Schema tag; a file with another tag is refused rather than guessed at.
    #[serde(default = "default_schema")]
    pub schema: String,
    /// Model id → per-million-token prices in micro-USD.
    #[serde(default)]
    pub models: BTreeMap<String, ModelPrice>,
}

fn default_schema() -> String {
    PRICE_SCHEMA.to_string()
}

impl PriceCatalogue {
    /// Parse a catalogue document, refusing a foreign schema or an empty id.
    ///
    /// # Errors
    ///
    /// Returns `EaiError::config` when the JSON is malformed, the schema tag
    /// differs, or a model id is blank.
    pub fn from_json(text: &str) -> EaiResult<Self> {
        let catalogue: Self = serde_json::from_str(text)
            .map_err(|e| EaiError::config(format!("price catalogue: {e}")))?;
        if catalogue.schema != PRICE_SCHEMA {
            return Err(EaiError::config(format!(
                "price catalogue schema must be {PRICE_SCHEMA}, got {}",
                catalogue.schema
            )));
        }
        if catalogue.models.keys().any(|model| model.trim().is_empty()) {
            return Err(EaiError::config("price catalogue has a blank model id"));
        }
        Ok(catalogue)
    }

    /// Read and parse a catalogue file.
    ///
    /// # Errors
    ///
    /// Returns `EaiError::io` when the file cannot be read, or the same errors
    /// as [`PriceCatalogue::from_json`].
    pub fn load(path: &Path) -> EaiResult<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| EaiError::io(format!("read price catalogue {}: {e}", path.display())))?;
        Self::from_json(&text)
    }

    /// The catalogue compiled into the binary (`deploy/harness/prices.json`
    /// at build time), so the harness can price calls from any directory.
    ///
    /// # Panics
    ///
    /// Never at run time: a test parses the compiled document.
    #[must_use]
    pub fn builtin() -> Self {
        static DOC: &str = include_str!("../../../deploy/harness/prices.json");
        Self::from_json(DOC).unwrap_or_else(|_| Self {
            schema: PRICE_SCHEMA.to_string(),
            models: BTreeMap::new(),
        })
    }

    /// The catalogue at `path` when that file exists, else the built-in one.
    ///
    /// # Errors
    ///
    /// Returns the load error of a file that exists but cannot be read or
    /// parsed: a catalogue the operator wrote is never silently replaced.
    pub fn load_or_builtin(path: &Path) -> EaiResult<Self> {
        if path.exists() {
            Self::load(path)
        } else {
            Ok(Self::builtin())
        }
    }

    /// The price for `model`, when the catalogue knows it.
    #[must_use]
    pub fn price(&self, model: &str) -> Option<ModelPrice> {
        if let Some(price) = self.models.get(model).copied() {
            return Some(price);
        }
        let canonical = crate::providers::Registry::builtin().canonical_id(model);
        self.models.get(canonical).copied()
    }

    /// Number of priced models.
    #[must_use]
    pub fn len(&self) -> usize {
        self.models.len()
    }

    /// True when no model is priced.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }
}

#[cfg(test)]
#[path = "tests/catalog.rs"]
mod tests;
