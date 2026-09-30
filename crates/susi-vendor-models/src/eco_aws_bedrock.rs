//! AWS Bedrock profile: Converse / InvokeModel, streaming, agents and guardrails behind SigV4-signed regional endpoints.
use crate::eco_profile::{self, Profile};
use susi_error::EaiResult;

/// Profile id under `config/ecosystem/profiles/`.
pub const PROFILE_ID: &str = "aws-bedrock";
/// Knowledge-base entity this profile details.
pub const SUBJECT_ID: &str = "bedrock-runtime";

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
    fn eco_aws_bedrock_profile_is_valid() {
        profile().expect("profile validates");
    }

    #[test]
    fn eco_aws_bedrock_store_stays_consistent() {
        let issues = eco_consistency::check(&store());
        assert!(issues.is_empty(), "{issues:?}");
    }

    #[test]
    fn eco_aws_bedrock_capabilities_are_canonical() {
        let p = profile().unwrap();
        for c in &p.capabilities {
            assert!(eco_taxonomy::canonical(c).is_some(), "{c}");
        }
    }

    #[test]
    fn eco_aws_bedrock_facts_cover_the_required_surface() {
        let p = profile().unwrap();
        let kb = store();
        assert!(kb.entity("aws-bedrock").is_some());
        assert!(p
            .endpoints
            .iter()
            .any(|e| e.path.contains("converse-stream")));
        assert!(p.auth.iter().any(|a| a.prefix.contains("AWS4-HMAC-SHA256")));
        assert!(p.errors.iter().any(|e| e.code == "ThrottlingException"));
    }
}
