//! T-CLAUDE-351 — risk and compliance questions answered from the knowledge
//! base, with citations and honest "mapping, not certification" wording.
#[cfg(test)]
mod eco_risk_answers {
    use susi_vendor_models::{eco_profile, eco_store};

    fn profile_json(id: &str) -> serde_json::Value {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../config/ecosystem/profiles")
            .join(format!("{id}.json"));
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    /// "Does <vendor> document <region> residency?" — look for a residency
    /// fact row naming the vendor; the citation is the row itself.
    fn residency_answer(vendor: &str) -> Option<String> {
        let p = profile_json("data-residency");
        p["request_shapes"]
            .as_array()
            .unwrap()
            .iter()
            .find_map(|s| {
                s["fields"].as_array().unwrap().iter().find_map(|f| {
                    let row = f.as_str().unwrap();
                    row.to_lowercase().contains(vendor).then(|| row.to_string())
                })
            })
    }

    /// "What licence class covers <model>?" — catalog rows, never inferred.
    fn licence_answer(token: &str) -> Option<String> {
        let p = profile_json("model-licences");
        p["request_shapes"]
            .as_array()
            .unwrap()
            .iter()
            .find_map(|s| {
                s["fields"].as_array().unwrap().iter().find_map(|f| {
                    let row = f.as_str().unwrap();
                    row.to_lowercase().contains(token).then(|| row.to_string())
                })
            })
    }

    /// "Is susi <compliant/certified> by a framework?" — always mapping.
    fn framework_answer(profile: &str) -> String {
        let p = profile_json(profile);
        let deviations = p["deviations"].as_array().unwrap();
        let scope = deviations
            .iter()
            .find_map(|d| d["behavior"].as_str())
            .unwrap_or("");
        assert!(
            scope.contains("mapping"),
            "answer must be a mapping: {scope}"
        );
        scope.to_string()
    }

    #[test]
    fn eco_risk_answers_residency_questions_cite_rows() {
        let eu = residency_answer("mistral").expect("mistral residency row");
        assert!(eu.to_lowercase().contains("eu"));
        let aws = residency_answer("aws-bedrock").expect("bedrock residency row");
        assert!(aws.to_lowercase().contains("regional"));
        // unknown vendors are honest gaps, not invented answers
        assert!(residency_answer("totally-unknown-vendor").is_none());
    }

    #[test]
    fn eco_risk_answers_licence_questions_map_to_classes() {
        let llama = licence_answer("llama").expect("llama licence row");
        assert!(llama.contains("community"));
        let rail = licence_answer("openrail").expect("openrail row");
        assert!(rail.to_lowercase().contains("restrict"));
        assert!(licence_answer("not-a-licence").is_none());
    }

    #[test]
    fn eco_risk_answers_framework_questions_are_mappings_not_certification() {
        for p in ["nist-ai-rmf", "owasp-llm-top10"] {
            let answer = framework_answer(p);
            assert!(answer.contains("mapping"));
            assert!(!answer.contains("certified"));
            assert!(!answer.contains("compliant\""));
        }
    }

    #[test]
    fn eco_risk_answers_every_catalog_profile_is_loadable() {
        for id in [
            "data-residency",
            "model-licences",
            "ai-disclosures",
            "nist-ai-rmf",
            "owasp-llm-top10",
        ] {
            eco_profile::load_bundled(id).expect("profile validates");
        }
        let kb = eco_store::load_dir(&eco_store::bundled_source_dir())
            .expect("bundled ecosystem store loads");
        assert!(kb.entity("nist-ai-rmf").is_some());
        assert!(kb.entity("owasp-llm-top10").is_some());
    }
}
