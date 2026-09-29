//! Gemini generateContent / Live / caching profile — facts live in `config/ecosystem/profiles/gemini-generatecontent.json` plus the
//! bundled entity files it details; this module is the typed handle and the
//! offline test contract.
use crate::eco_profile::{self, Profile};
use susi_error::EaiResult;

/// Profile id under `config/ecosystem/profiles/`.
pub const PROFILE_ID: &str = "gemini-generatecontent";
/// Knowledge-base entity this profile details.
pub const SUBJECT_ID: &str = "gemini-generatecontent";

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
    fn eco_gemini_profile_is_valid() {
        profile().expect("profile validates");
    }

    #[test]
    fn eco_gemini_store_stays_consistent() {
        let issues = eco_consistency::check(&store());
        assert!(issues.is_empty(), "{issues:?}");
    }

    #[test]
    fn eco_gemini_capabilities_are_canonical() {
        let p = profile().unwrap();
        for c in &p.capabilities {
            assert!(eco_taxonomy::canonical(c).is_some(), "{c}");
        }
    }

    #[test]
    fn eco_gemini_facts_cover_the_required_surface() {
        let p = profile().unwrap();
        assert!(p
            .endpoints
            .iter()
            .any(|e| e.path.contains(":generateContent")));
        assert!(p
            .endpoints
            .iter()
            .any(|e| e.path.contains("BidiGenerateContent")));
        assert!(p
            .endpoints
            .iter()
            .any(|e| e.path.contains("cachedContents")));
        assert!(p.auth.iter().any(|a| a.header == "x-goog-api-key"));
        assert!(p.errors.iter().any(|e| e.code == "RESOURCE_EXHAUSTED"));
        let kb = store();
        assert!(kb.entity("gemini-live-2025").is_some());
    }
}
