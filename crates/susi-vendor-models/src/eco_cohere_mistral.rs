//! Cohere v2 and Mistral APIs profile — facts live in `config/ecosystem/profiles/cohere-mistral.json` plus the
//! bundled entity files it details; this module is the typed handle and the
//! offline test contract.
use crate::eco_profile::{self, Profile};
use susi_error::EaiResult;

/// Profile id under `config/ecosystem/profiles/`.
pub const PROFILE_ID: &str = "cohere-mistral";
/// Knowledge-base entity this profile details.
pub const SUBJECT_ID: &str = "cohere-chat-v2";

/// Load and validate the bundled profile.
///
/// # Errors
/// `EaiError::io`/`config` on missing or invalid profile data.
pub fn profile() -> EaiResult<Profile> {
    eco_profile::load_bundled(PROFILE_ID)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eco_store;
    use crate::{eco_consistency, eco_taxonomy};

    fn store() -> crate::eco_schema::KnowledgeBase {
        eco_store::load_dir(&eco_store::bundled_source_dir())
            .expect("bundled ecosystem store loads")
    }

    #[test]
    fn eco_cohere_mistral_profile_is_valid() {
        profile().expect("profile validates");
    }

    #[test]
    fn eco_cohere_mistral_store_stays_consistent() {
        let issues = eco_consistency::check(&store());
        assert!(issues.is_empty(), "{issues:?}");
    }

    #[test]
    fn eco_cohere_mistral_capabilities_are_canonical() {
        let p = profile().unwrap();
        for c in &p.capabilities {
            assert!(eco_taxonomy::canonical(c).is_some(), "{c}");
        }
    }

    #[test]
    fn eco_cohere_mistral_facts_cover_the_required_surface() {
        let p = profile().unwrap();
        assert!(p.endpoints.iter().any(|e| e.path == "/v2/chat"));
        assert!(p.endpoints.iter().any(|e| e.path == "/v2/embed"));
        assert!(p.endpoints.iter().any(|e| e.path == "/v1/chat/completions"));
        assert!(p.deviations.iter().any(|d| d.host == "mistral"));
        let kb = store();
        assert!(kb.entity("cohere-api").is_some() && kb.entity("mistral-api").is_some());
    }
}
