#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use serde_json::{json, Value};
use std::fs;
use susi_gemi_models::{
    coding_models::{CodingModelManager, CodingModelOverride},
    frontier::{FrontierManager, FrontierOverride},
    open_weight::{OpenWeightManager, OpenWeightOverride},
    openrouter::OpenRouterManager,
};

#[test]
fn local_catalogs_keep_family_specific_lookup_precedence_files_and_credentials() {
    let tmp = susi_paths::tempfile::tempdir().unwrap();
    let mut env = susi_paths::test_env::EnvGuard::isolated();
    env.set("SUSI_HOME", tmp.path())
        .remove("SUSI_EXTENSION_PACK");
    env.set("OPENROUTER_API_KEY", "local-fixture-key");
    let config = susi_paths::SusiDirs::config_dir();
    assert!(config.starts_with(tmp.path()));
    let coding = CodingModelManager::new().unwrap();
    let frontier = FrontierManager::new().unwrap();
    let weight = OpenWeightManager::new().unwrap();
    let router = OpenRouterManager::new().unwrap();

    let opus = CodingModelManager::definition("CLAUDE OPUS").unwrap();
    for invalid in ["CLAUDE-OPUS", " claude-opus ", &opus.model] {
        assert!(
            CodingModelManager::definition(invalid).is_err(),
            "{invalid}"
        );
    }
    assert_eq!(frontier.effective(" GPT-4O ").unwrap().model, "gpt-4o");
    assert_eq!(
        weight.effective(" LLAMA3.2 ").unwrap().ollama_tag,
        "llama3.2"
    );
    assert_eq!(
        OpenRouterManager::definition("OPENROUTER AUTO").unwrap().id,
        "auto"
    );
    for invalid in ["AUTO", "OPENROUTER/AUTO", " auto "] {
        assert!(OpenRouterManager::definition(invalid).is_err(), "{invalid}");
    }
    assert_eq!(router.resolve_api_key(), "local-fixture-key");
    assert_eq!(router.effective_model(), "openrouter/auto");
    let denied = weight.pull("llama3.2").unwrap_err().to_string();
    assert!(denied.contains("[MODEL LICENSE]") && denied.contains("llama3.2"));

    for (dir, id, expected) in [
        (
            "coding-models",
            "claude-opus",
            "invalid coding-model override",
        ),
        ("frontier-models", "gpt", "invalid frontier-model override"),
        ("open-weight", "llama-3", "invalid open-weight override"),
    ] {
        fs::create_dir_all(config.join(dir)).unwrap();
        fs::write(config.join(dir).join(format!("{id}.json")), b"{").unwrap();
        let message = match dir {
            "coding-models" => coding.effective(id).unwrap_err().to_string(),
            "frontier-models" => frontier.effective(id).unwrap_err().to_string(),
            _ => weight.effective(id).unwrap_err().to_string(),
        };
        assert!(message.contains(expected), "{message}");
    }
    let patch = CodingModelOverride {
        protocol_type: Some(String::new()),
        ..Default::default()
    };
    assert_eq!(
        coding
            .configure("Claude Opus", &patch)
            .unwrap()
            .protocol_type,
        ""
    );
    let frontier_patch = FrontierOverride {
        model: Some("operator-model".into()),
        ..Default::default()
    };
    assert_eq!(
        frontier.configure("gpt-4o", &frontier_patch).unwrap().model,
        "operator-model"
    );
    assert_eq!(frontier.reset(" gpt-4o ").unwrap().model, "gpt-5");
    let weight_patch = OpenWeightOverride {
        ollama_tag: Some("private:tag".into()),
        ..Default::default()
    };
    assert_eq!(
        weight
            .configure("LLAMA3.2", &weight_patch)
            .unwrap()
            .ollama_tag,
        "private:tag"
    );
    assert_eq!(weight.reset("LLAMA3.2").unwrap().ollama_tag, "llama3.1");

    fs::create_dir_all(config.join("openrouter")).unwrap();
    for (path, value) in [
        ("preferred_coding_model.txt", "coding-pin"),
        ("frontier-models/preferred_model.txt", "frontier-pin"),
        ("open-weight/preferred_model.txt", "weight-pin"),
        ("openrouter/preferred_model.txt", "router-pin"),
    ] {
        fs::write(config.join(path), format!("  {value}\n")).unwrap();
    }
    assert_eq!(coding.preferred().as_deref(), Some("coding-pin"));
    assert_eq!(frontier.preferred().as_deref(), Some("frontier-pin"));
    assert_eq!(weight.preferred().as_deref(), Some("weight-pin"));
    assert_eq!(router.effective_model(), "router-pin");

    // A verified extension pack may deliberately put a matching name ahead of
    // another entry's canonical id. Declaration order must win over rank.
    let pack = config.join("extensions").join("catalog-fixture");
    fs::create_dir_all(&pack).unwrap();
    let filename = "coding-models.json";
    fs::write(pack.join("manifest.json"), serde_json::to_vec(&json!({
        "id":"catalog-fixture", "name":"Catalog fixture", "version":"1", "files":{(filename):filename}
    })).unwrap()).unwrap();
    let row = |id: &str, name: &str, rank: u8| {
        json!({
            "id":id, "name":name, "rank":rank, "engine":"OpenRouter", "model":"openrouter/auto", "documentation":"fixture"
        })
    };
    let custom = json!([row("first", "later", 2), row("later", "Other", 1)]);
    fs::write(pack.join(filename), serde_json::to_vec(&custom).unwrap()).unwrap();
    susi_config::extensions::load_pack("catalog-fixture").unwrap();
    let ordered: Vec<String> = CodingModelManager::catalog()
        .unwrap()
        .into_iter()
        .map(|d| d.id)
        .collect();
    assert_eq!(ordered, ["first", "later"]);
    assert_eq!(CodingModelManager::definition("later").unwrap().id, "first");
    assert_eq!(CodingModelManager::definition("LATER").unwrap().id, "first");
    fs::write(
        pack.join(filename),
        serde_json::to_vec(&Value::Array(vec![])).unwrap(),
    )
    .unwrap();
    assert_eq!(
        CodingModelManager::definition("Claude Opus").unwrap().id,
        "claude-opus"
    );
    susi_config::extensions::unload_pack("catalog-fixture").unwrap();
}
