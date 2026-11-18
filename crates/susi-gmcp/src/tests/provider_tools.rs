use super::*;
use crate::susi_core::plane_bus::{topics, PlaneBus, PlaneHandler};
use std::sync::Arc;

/// Echoes the payload; `{"vendor": "refused"}` comes back as an error.
struct Echo;
impl PlaneHandler for Echo {
    fn handle(&self, _topic: &str, payload: Value) -> Result<Value, String> {
        if payload["vendor"] == "refused" {
            return Err("refused by plane".to_string());
        }
        if payload["action"] == "benchmark" {
            return Ok(json!({ "report": "# bench" }));
        }
        Ok(json!({ "payload": payload }))
    }
}

fn sent(result: EaiResult<String>) -> Value {
    serde_json::from_str::<Value>(&result.unwrap()).unwrap()["payload"].clone()
}

#[test]
fn provider_tools_forward_their_action_and_typed_arguments() {
    let _isolated = crate::test_isolation::enter_isolated_home();
    PlaneBus::global().register(topics::DAEMON_PROVIDERS, Arc::new(Echo));
    let ws = Path::new(".");
    let list = sent(CoreTools::provider_list(&json!({"vendor": "frontier"}), ws));
    assert_eq!(list, json!({"action": "list", "vendor": "frontier"}));
    let doctor = sent(CoreTools::provider_doctor(
        &json!({"vendor": "openrouter", "live": true, "extra": 1}),
        ws,
    ));
    assert_eq!(
        doctor,
        json!({"action": "doctor", "vendor": "openrouter", "live": true})
    );
    let configure = sent(CoreTools::provider_configure(
        &json!({"vendor": "frontier", "model": "m", "override": {"engine": "e"}}),
        ws,
    ));
    assert_eq!(configure["override"], json!({"engine": "e"}));
    let probe = sent(CoreTools::provider_probe(
        &json!({"vendor": "coding", "model": "m", "prompt": "hi"}),
        ws,
    ));
    assert_eq!(probe["action"], "probe");
    assert_eq!(
        sent(CoreTools::provider_models(&json!({"limit": 3}), ws))["action"],
        "openrouter_models"
    );
    assert_eq!(
        sent(CoreTools::keys_status(&json!({}), ws)),
        json!({"action": "keys_status"})
    );
    assert_eq!(
        sent(CoreTools::brain_budget(
            &json!({"allow_paid": true, "max_spend_microusd": 5}),
            ws
        )),
        json!({"action": "brain_budget", "allow_paid": true, "max_spend_microusd": 5})
    );
    assert_eq!(
        sent(CoreTools::verify_download_agent(&json!({}), ws))["action"],
        "verify_download_agent"
    );
    assert_eq!(CoreTools::benchmark_run(&json!({}), ws).unwrap(), "# bench");
    // Wrong types are refused before anything is sent; plane refusals
    // come back as errors.
    assert!(CoreTools::provider_doctor(&json!({"vendor": "x", "live": "yes"}), ws).is_err());
    assert!(CoreTools::brain_status(&json!({"top": -1}), ws).is_err());
    assert!(CoreTools::provider_configure(&json!({"override": []}), ws).is_err());
    // Caller-named files are absolute and policy-confined.
    for refused in [
        CoreTools::provider_configure(&json!({"vendor": "frontier", "file": "o.json"}), ws),
        CoreTools::brain_export(&json!({"out": "b.json"}), ws),
        CoreTools::brain_import(&json!({"file": "b.json"}), ws),
        CoreTools::brain_bench(&json!({"out": "e.jsonl"}), ws),
    ] {
        assert!(refused.unwrap_err().to_string().contains("absolute"));
    }
    let out = std::env::temp_dir().join("susi-provider-tool-bundle.json");
    let exported = sent(CoreTools::brain_export(
        &json!({"out": out.display().to_string()}),
        ws,
    ));
    assert!(
        Path::new(exported["out"].as_str().unwrap()).ends_with("susi-provider-tool-bundle.json")
    );
    let refused = CoreTools::provider_list(&json!({"vendor": "refused"}), ws).unwrap_err();
    assert!(refused.to_string().contains("refused by plane"));
}

#[test]
fn provider_setup_reset_pull_and_prefer_forward_expected_actions() {
    let _isolated = crate::test_isolation::enter_isolated_home();
    PlaneBus::global().register(topics::DAEMON_PROVIDERS, Arc::new(Echo));
    let ws = Path::new(".");
    assert_eq!(
        sent(CoreTools::provider_setup(
            &json!({"vendor": "frontier", "model": "gpt"}),
            ws
        )),
        json!({"action": "setup", "vendor": "frontier", "model": "gpt"})
    );
    assert_eq!(
        sent(CoreTools::provider_reset(
            &json!({"vendor": "frontier", "model": "gpt"}),
            ws
        )),
        json!({"action": "reset", "vendor": "frontier", "model": "gpt"})
    );
    assert_eq!(
        sent(CoreTools::provider_pull(&json!({"model": "qwen2.5"}), ws)),
        json!({"action": "pull", "model": "qwen2.5"})
    );
    assert_eq!(
        sent(CoreTools::provider_prefer(
            &json!({"vendor": "openweight", "model": "qwen2.5", "clear": false}),
            ws
        )),
        json!({"action": "prefer", "vendor": "openweight", "model": "qwen2.5", "clear": false})
    );
}

#[test]
fn keys_prefer_check_and_models_forward_vendor_and_clear_flags() {
    let _isolated = crate::test_isolation::enter_isolated_home();
    PlaneBus::global().register(topics::DAEMON_PROVIDERS, Arc::new(Echo));
    let ws = Path::new(".");
    assert_eq!(
        sent(CoreTools::keys_prefer(
            &json!({"vendor": "anthropic", "clear": true}),
            ws
        )),
        json!({"action": "keys_prefer", "vendor": "anthropic", "clear": true})
    );
    assert_eq!(
        sent(CoreTools::keys_check(&json!({"vendor": "openai"}), ws)),
        json!({"action": "keys_check", "vendor": "openai"})
    );
    assert_eq!(
        sent(CoreTools::keys_models(&json!({"vendor": "gemini"}), ws)),
        json!({"action": "keys_models", "vendor": "gemini"})
    );
}

#[test]
fn brain_classify_explain_matrix_cache_reset_and_probe_forward_expected_actions() {
    let _isolated = crate::test_isolation::enter_isolated_home();
    PlaneBus::global().register(topics::DAEMON_PROVIDERS, Arc::new(Echo));
    let ws = Path::new(".");
    assert_eq!(
        sent(CoreTools::brain_classify(&json!({"prompt": "fix bug"}), ws)),
        json!({"action": "brain_classify", "prompt": "fix bug"})
    );
    assert_eq!(
        sent(CoreTools::brain_explain(&json!({"prompt": "fix bug"}), ws)),
        json!({"action": "brain_explain", "prompt": "fix bug"})
    );
    assert_eq!(
        sent(CoreTools::brain_matrix(&json!({}), ws))["action"],
        "brain_matrix"
    );
    assert_eq!(
        sent(CoreTools::brain_cache(&json!({}), ws))["action"],
        "brain_cache"
    );
    assert_eq!(
        sent(CoreTools::brain_reset(&json!({}), ws))["action"],
        "brain_reset"
    );
    assert_eq!(
        sent(CoreTools::brain_probe(
            &json!({"provider": "local", "no_record": true}),
            ws
        )),
        json!({"action": "brain_probe", "provider": "local", "no_record": true})
    );
}

#[test]
fn brain_status_and_provider_models_forward_optional_filters_and_limits() {
    let _isolated = crate::test_isolation::enter_isolated_home();
    PlaneBus::global().register(topics::DAEMON_PROVIDERS, Arc::new(Echo));
    let ws = Path::new(".");
    assert_eq!(
        sent(CoreTools::brain_status(
            &json!({"top": 5, "class": "code"}),
            ws
        )),
        json!({"action": "brain_status", "top": 5, "class": "code"})
    );
    assert_eq!(
        sent(CoreTools::provider_models(
            &json!({"live": true, "limit": 10}),
            ws
        )),
        json!({"action": "openrouter_models", "live": true, "limit": 10})
    );
}
