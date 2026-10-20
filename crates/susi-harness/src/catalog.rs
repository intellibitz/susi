//! The price catalogue.
//!
//! Prices are data, not code: a catalogue file lists each model's per-million
//! rates in micro-USD, and the ledger stores the rate that was in force when a
//! call ran. A model the catalogue cannot price is refused at dispatch — an
//! unpriced call is spend nobody can account for.

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

    /// The price for `model`, when the catalogue knows it.
    #[must_use]
    pub fn price(&self, model: &str) -> Option<ModelPrice> {
        self.models.get(model).copied()
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
mod tests {
    use super::*;
    use crate::money::MicroUsd;
    use crate::pricing::TokenUsage;

    const DOC: &str = r#"{
        "schema": "susi/harness/prices/v1",
        "models": {
            "deepseek-flash": {
                "input_miss": 150000,
                "input_hit": 3000,
                "output": 600000
            }
        }
    }"#;

    #[test]
    fn a_catalogue_prices_its_models() {
        let catalogue = PriceCatalogue::from_json(DOC).unwrap();
        assert_eq!(catalogue.len(), 1);
        let price = catalogue.price("deepseek-flash").unwrap();
        assert_eq!(price.input_miss, MicroUsd::from_micros(150_000));
        assert_eq!(
            price.cost(TokenUsage::new(3_000_000, 2_400_000, 200_000)),
            MicroUsd::from_micros(217_200)
        );
        assert!(catalogue.price("unknown-model").is_none());
    }

    #[test]
    fn a_foreign_schema_is_refused() {
        let err = PriceCatalogue::from_json(r#"{"schema":"other","models":{}}"#).unwrap_err();
        assert!(err.to_string().contains("schema"));
    }

    #[test]
    fn a_blank_model_id_is_refused() {
        let doc = r#"{"schema":"susi/harness/prices/v1","models":{"":{"input_miss":1,"input_hit":1,"output":1}}}"#;
        assert!(PriceCatalogue::from_json(doc).is_err());
    }

    #[test]
    fn a_catalogue_loads_from_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prices.json");
        std::fs::write(&path, DOC).unwrap();
        let catalogue = PriceCatalogue::load(&path).unwrap();
        assert_eq!(catalogue.len(), 1);
    }

    #[test]
    fn a_missing_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(PriceCatalogue::load(&dir.path().join("absent.json")).is_err());
    }
}
