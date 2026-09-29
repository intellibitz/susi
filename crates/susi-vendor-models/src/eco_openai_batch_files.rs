//! OpenAI Batch and Files profile — facts live in `config/ecosystem/profiles/openai-batch-files.json` plus the
//! bundled entity files it details; this module is the typed handle and the
//! offline test contract.
use crate::eco_profile::{self, Profile};
use susi_error::EaiResult;

/// Profile id under `config/ecosystem/profiles/`.
pub const PROFILE_ID: &str = "openai-batch-files";
/// Knowledge-base entity this profile details.
pub const SUBJECT_ID: &str = "openai-batch";

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
    fn eco_openai_batch_files_profile_is_valid() {
        profile().expect("profile validates");
    }

    #[test]
    fn eco_openai_batch_files_store_stays_consistent() {
        let issues = eco_consistency::check(&store());
        assert!(issues.is_empty(), "{issues:?}");
    }

    #[test]
    fn eco_openai_batch_files_capabilities_are_canonical() {
        let p = profile().unwrap();
        assert!(!p.capabilities.is_empty());
        for c in &p.capabilities {
            assert!(eco_taxonomy::canonical(c).is_some(), "{c}");
        }
    }

    #[test]
    fn eco_openai_batch_files_facts_cover_the_required_surface() {
        let p = profile().unwrap();
        assert!(!p.endpoints.is_empty());
        assert!(!p.auth.is_empty());
        assert!(!p.errors.is_empty());
        assert!(!p.rate_limits.is_empty());
        assert!(p.endpoints.iter().any(|e| e.path == "/v1/batches"));
        assert!(p.endpoints.iter().any(|e| e.path == "/v1/files"));
        assert!(p.endpoints.iter().any(|e| e.path.contains("cancel")));
        let b = p
            .response_shapes
            .iter()
            .find(|s| s.name == "batch-object")
            .unwrap();
        for f in [
            "status",
            "output_file_id",
            "error_file_id",
            "request_counts",
        ] {
            assert!(b.fields.iter().any(|x| x == f), "missing {f}");
        }
        let line = p
            .request_shapes
            .iter()
            .find(|s| s.name == "batch-input-line")
            .unwrap();
        assert_eq!(line.format, "jsonl");
        assert!(p.capabilities.iter().any(|c| c == "cap-batch"));
        assert!(p.capabilities.iter().any(|c| c == "cap-files"));
    }
}
