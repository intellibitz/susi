//! A2A security schemes and extensions profile: AgentCard securitySchemes, authenticated extended cards, mTLS and extension signalling. — offline contract tests; the facts live in `config/ecosystem/`.
#[cfg(test)]
mod tests {
    use susi_vendor_models::{eco_consistency, eco_profile, eco_store, eco_taxonomy};

    const PROFILES: &[&str] = &["a2a-security"];

    fn store() -> susi_vendor_models::eco_schema::KnowledgeBase {
        eco_store::load_dir(&eco_store::bundled_source_dir())
            .expect("bundled ecosystem store loads")
    }

    #[test]
    fn eco_a2a_security_profiles_are_valid() {
        for pid in PROFILES {
            eco_profile::load_bundled(pid).expect("profile validates");
        }
    }

    #[test]
    fn eco_a2a_security_store_stays_consistent() {
        let issues = eco_consistency::check(&store());
        assert!(issues.is_empty(), "{issues:?}");
    }

    #[test]
    fn eco_a2a_security_capabilities_are_canonical() {
        for pid in PROFILES {
            let p = eco_profile::load_bundled(pid).unwrap();
            for c in &p.capabilities {
                assert!(eco_taxonomy::canonical(c).is_some(), "{c}");
            }
        }
    }

    #[test]
    fn eco_a2a_security_facts_cover_the_required_surface() {
        let kb = store();
        assert!(kb.entity("a2a").is_some());
        let p = eco_profile::load_bundled("a2a-security").unwrap();
        assert!(p
            .request_shapes
            .iter()
            .any(|s| s.fields.iter().any(|f| f.contains("mutualTls"))));
        assert!(p
            .request_shapes
            .iter()
            .any(|s| s.fields.iter().any(|f| f.contains("extensions"))));
    }
}
