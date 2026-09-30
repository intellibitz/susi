//! Default model per vendor from a live /models list.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelListing {
    pub id: String,
    pub preferred: bool,
}

#[must_use]
pub fn default_model_from_list(models: &[ModelListing]) -> Option<String> {
    models
        .iter()
        .find(|m| m.preferred)
        .or_else(|| models.first())
        .map(|m| m.id.clone())
}

#[cfg(test)]
mod zc_default_models_tests {
    use super::*;

    #[test]
    fn zc_default_models_picks_preferred_or_first() {
        let list = vec![
            ModelListing {
                id: "a".into(),
                preferred: false,
            },
            ModelListing {
                id: "b".into(),
                preferred: true,
            },
        ];
        assert_eq!(default_model_from_list(&list).as_deref(), Some("b"));
        assert_eq!(
            default_model_from_list(&[ModelListing {
                id: "only".into(),
                preferred: false
            }])
            .as_deref(),
            Some("only")
        );
    }
}
