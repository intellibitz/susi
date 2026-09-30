//! T-CLAUDE-322 — NIST AI RMF mapping checks (offline; facts in `config/ecosystem/`).
//!
//! The mapping is *metadata only*: it records which RMF function a susi
//! control feeds, and deliberately never claims compliance or certification.
#[cfg(test)]
mod eco_nist_rmf {
    use susi_vendor_models::{eco_consistency, eco_profile, eco_store};

    fn profile_json() -> serde_json::Value {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../config/ecosystem/profiles/nist-ai-rmf.json");
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    #[test]
    fn eco_nist_rmf_profile_is_valid_and_framework_kind() {
        let p = eco_profile::load_bundled("nist-ai-rmf").expect("profile validates");
        assert_eq!(p.subject, "nist-ai-rmf");
    }

    #[test]
    fn eco_nist_rmf_entities_are_in_the_store() {
        let kb = eco_store::load_dir(&eco_store::bundled_source_dir())
            .expect("bundled ecosystem store loads");
        assert!(kb.entity("nist").is_some());
        assert!(kb.entity("nist-ai-rmf").is_some());
        assert!(kb.entity("nist-ai-rmf-1.0").is_some());
        let issues = eco_consistency::check(&kb);
        assert!(issues.is_empty(), "{issues:?}");
    }

    #[test]
    fn eco_nist_rmf_covers_all_four_functions() {
        let p = profile_json();
        let functions: Vec<_> = p["request_shapes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap())
            .collect();
        for f in ["govern", "map", "measure", "manage"] {
            assert!(functions.contains(&f), "missing RMF function {f}");
        }
    }

    #[test]
    fn eco_nist_rmf_every_susi_control_maps_to_a_function() {
        let p = profile_json();
        let mappings = p["mappings"].as_array().unwrap();
        assert!(mappings.len() >= 4);
        for m in mappings {
            assert!(m["control"].as_str().unwrap().contains('-'));
            let f = m["rmf_function"].as_str().unwrap();
            assert!(["GOVERN", "MAP", "MEASURE", "MANAGE"].contains(&f), "{f}");
            assert!(!m["rationale"].as_str().unwrap().is_empty());
        }
    }

    #[test]
    fn eco_nist_rmf_never_claims_compliance() {
        let text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../config/ecosystem/profiles/nist-ai-rmf.json"),
        )
        .unwrap();
        assert!(!text.contains("certified"));
        assert!(!text.contains("\"compliant\""));
        assert!(text.contains("mapping only"));
    }
}
