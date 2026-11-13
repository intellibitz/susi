use super::*;
use std::cell::{Cell, RefCell};

const TEST_REGISTRY: &str = r#"{
  "providers": [
    {
      "id": "alpha",
      "wire": "openai",
      "endpoint": "https://alpha.example/v1/chat/completions",
      "key_env": [],
      "models": [
        {"id": "alpha-one", "backend": "alpha-1"},
        {"id": "alpha-two", "backend": "retired-2"}
      ]
    },
    {
      "id": "beta",
      "wire": "openai",
      "endpoint": "https://beta.example/v1/chat/completions",
      "key_env": [],
      "models": [{"id": "beta-one", "backend": "beta-1"}]
    },
    {
      "id": "closed",
      "wire": "anthropic",
      "endpoint": "https://closed.example/v1/messages",
      "key_env": [],
      "models": [{"id": "claude", "backend": "claude-x"}]
    },
    {
      "id": "keyed",
      "wire": "openai",
      "endpoint": "https://keyed.example/v1/chat/completions",
      "key_env": ["SUSI_HARNESS_VERIFY_KEY_7F3A9C_UNSET"],
      "models": [{"id": "keyed-one", "backend": "keyed-1"}]
    }
  ]
}"#;

fn test_registry() -> Registry {
    Registry::parse(TEST_REGISTRY).unwrap()
}

#[test]
fn gemini_and_openrouter_lists_derive_from_their_chat_endpoints() {
    let registry = Registry::builtin();
    let provider = |id: &str| {
        registry
            .providers()
            .iter()
            .find(|candidate| candidate.id == id)
            .unwrap()
    };
    assert_eq!(
        models_endpoint(provider("gemini")).as_deref(),
        Some("https://generativelanguage.googleapis.com/v1beta/openai/models")
    );
    assert_eq!(
        models_endpoint(provider("openrouter")).as_deref(),
        Some("https://openrouter.ai/api/v1/models")
    );
    assert_eq!(
        models_endpoint(provider("anthropic")).as_deref(),
        Some("https://api.anthropic.com/v1/models"),
        "the Messages API publishes its list beside /messages"
    );
}

#[test]
fn parse_upstream_ids_reads_openai_data_and_ignores_nameless_entries() {
    let ids = parse_upstream_ids(
        r#"{"object":"list","data":[{"id":"a"},{"id":" b "},{"object":"model"},{"id":""}]}"#,
    )
    .unwrap();
    assert_eq!(ids.iter().cloned().collect::<Vec<_>>(), vec!["a", "b"]);
}

#[test]
fn a_configured_backend_absent_upstream_is_drift() {
    let fetch = |_: &str, _: &[(&str, &str)]| {
        Ok((
            200,
            r#"{"data":[{"id":"alpha-1"},{"id":"alpha-3"}]}"#.to_string(),
        ))
    };
    let report = verify_registry(&test_registry(), Some("alpha"), &fetch).unwrap();
    assert!(!report.ok);
    let alpha = &report.providers[0];
    assert_eq!(alpha.missing, vec!["retired-2"]);
    assert!(alpha.reachable);
    assert!(!alpha.ok);
    assert!(alpha.note.contains("1 of 2"), "{}", alpha.note);
}

#[test]
fn every_configured_backend_present_verifies() {
    let fetch = |_: &str, _: &[(&str, &str)]| {
        Ok((
            200,
            r#"{"data":[{"id":"alpha-1"},{"id":"retired-2"}]}"#.to_string(),
        ))
    };
    let report = verify_registry(&test_registry(), Some("alpha"), &fetch).unwrap();
    assert!(report.ok);
    assert!(report.providers[0].ok);
    assert!(report.providers[0].missing.is_empty());
    assert!(
        report.providers[0].note.contains("all 2"),
        "{}",
        report.providers[0].note
    );
}

#[test]
fn google_namespaced_ids_match_the_bare_backend() {
    // Gemini's endpoint needs a key; the comparison under test does not, so
    // the provider is declared without one rather than reading the host env.
    let registry = Registry::parse(
        r#"{"providers":[{
          "id":"googleish",
          "wire":"openai",
          "endpoint":"https://generativelanguage.googleapis.com/v1beta/openai/chat/completions",
          "key_env":[],
          "models":[{"id":"flash","backend":"gemini-3.8-flash"}]
        }]}"#,
    )
    .unwrap();
    let fetch = |_: &str, _: &[(&str, &str)]| {
        Ok((
            200,
            r#"{"data":[{"id":"models/gemini-3.8-flash"}]}"#.to_string(),
        ))
    };
    let report = verify_registry(&registry, Some("googleish"), &fetch).unwrap();
    assert!(report.ok);
    assert!(report.providers[0].missing.is_empty());
}

#[test]
fn a_non_2xx_response_is_not_a_verification() {
    let fetch = |_: &str, _: &[(&str, &str)]| Ok((404, "not found".to_string()));
    let report = verify_registry(&test_registry(), Some("alpha"), &fetch).unwrap();
    assert!(!report.ok);
    let alpha = &report.providers[0];
    assert!(!alpha.reachable);
    assert!(!alpha.ok);
    assert!(alpha.note.contains("HTTP 404"), "{}", alpha.note);
}

#[test]
fn an_unreadable_list_is_not_a_verification() {
    let fetch = |_: &str, _: &[(&str, &str)]| Ok((200, "<html>".to_string()));
    let report = verify_registry(&test_registry(), Some("alpha"), &fetch).unwrap();
    let alpha = &report.providers[0];
    assert!(alpha.reachable);
    assert!(!alpha.ok);
    assert!(alpha.note.contains("unreadable"), "{}", alpha.note);
}

/// The registry's own `closed` provider names no key, so the Anthropic tests
/// carry their own: a list request without a key is not an Anthropic
/// request at all.
const ANTHROPIC_TEST_KEY: &str = "SUSI_HARNESS_VERIFY_ANTHROPIC_KEY";

fn anthropic_registry(backend: &str) -> Registry {
    Registry::parse(&format!(
        r#"{{"providers":[{{"id":"closed","wire":"anthropic",
            "endpoint":"https://closed.example/v1/messages",
            "key_env":["{ANTHROPIC_TEST_KEY}"],
            "models":[{{"id":"claude","backend":"{backend}"}}]}}]}}"#
    ))
    .unwrap()
}

fn with_anthropic_key<T>(body: impl FnOnce() -> T) -> T {
    std::env::set_var(ANTHROPIC_TEST_KEY, "test-key");
    let out = body();
    std::env::remove_var(ANTHROPIC_TEST_KEY);
    out
}

#[test]
fn anthropic_is_verified_through_its_models_endpoint() {
    let seen = RefCell::new(None::<(String, Vec<(String, String)>)>);
    let fetch = |url: &str, headers: &[(&str, &str)]| {
        seen.borrow_mut().replace((
            url.to_string(),
            headers
                .iter()
                .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                .collect(),
        ));
        Ok((
            200,
            r#"{"data":[{"id":"claude-x"},{"id":"claude-new"}]}"#.to_string(),
        ))
    };
    let report = with_anthropic_key(|| {
        verify_registry(&anthropic_registry("claude-x"), Some("closed"), &fetch).unwrap()
    });
    assert!(report.ok, "{:?}", report.providers);
    assert!(report.providers[0].reachable);
    assert!(report.providers[0].missing.is_empty());

    let (url, headers) = seen.borrow().clone().expect("the fetch must have run");
    assert_eq!(
        url, "https://closed.example/v1/models",
        "the list sits beside /messages"
    );
    assert!(
        headers.iter().any(|(name, _)| name == "x-api-key"),
        "an Anthropic list needs the key in x-api-key: {headers:?}"
    );
    assert!(
        headers
            .iter()
            .any(|(name, value)| name == "anthropic-version" && value == "2023-06-01"),
        "{headers:?}"
    );
}

/// A console user key is refused unless the request names its workspace, so
/// the list request must carry the header when it is configured.
#[test]
fn an_anthropic_workspace_travels_with_the_list_request() {
    let _guard = crate::secrets::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let previous = std::env::var("ANTHROPIC_WORKSPACE_ID").ok();
    std::env::set_var("ANTHROPIC_WORKSPACE_ID", " wrkspc_test ");
    let workspace = RefCell::new(None::<String>);
    let fetch = |_: &str, headers: &[(&str, &str)]| {
        if let Some((_, value)) = headers
            .iter()
            .find(|(name, _)| *name == "anthropic-workspace-id")
        {
            workspace.borrow_mut().replace((*value).to_string());
        }
        Ok((200, r#"{"data":[{"id":"claude-x"}]}"#.to_string()))
    };
    with_anthropic_key(|| {
        verify_registry(&anthropic_registry("claude-x"), Some("closed"), &fetch).unwrap()
    });
    assert_eq!(
        workspace.borrow().as_deref(),
        Some("wrkspc_test"),
        "the configured workspace must be trimmed and sent"
    );
    std::env::remove_var("ANTHROPIC_WORKSPACE_ID");
    if let Some(value) = previous {
        std::env::set_var("ANTHROPIC_WORKSPACE_ID", value);
    }
}

#[test]
fn anthropic_without_a_messages_suffix_stays_not_verifiable() {
    let registry = Registry::parse(
        r#"{"providers":[{"id":"odd","wire":"anthropic","endpoint":"https://odd.example/v1/chat",
            "key_env":[],"models":[{"id":"m","backend":"m"}]}]}"#,
    )
    .unwrap();
    let calls = Cell::new(0);
    let fetch = |_: &str, _: &[(&str, &str)]| {
        calls.set(calls.get() + 1);
        Ok((200, "{}".to_string()))
    };
    let report = verify_registry(&registry, Some("odd"), &fetch).unwrap();
    assert_eq!(calls.get(), 0);
    assert!(report.ok);
    assert!(!report.providers[0].reachable);
    assert!(
        report.providers[0].note.contains("not verifiable"),
        "{}",
        report.providers[0].note
    );
}

#[test]
fn a_provider_without_its_key_is_not_verified() {
    let calls = Cell::new(0);
    let fetch = |_: &str, _: &[(&str, &str)]| {
        calls.set(calls.get() + 1);
        Ok((200, "{}".to_string()))
    };
    let report = verify_registry(&test_registry(), Some("keyed"), &fetch).unwrap();
    assert_eq!(calls.get(), 0);
    let keyed = &report.providers[0];
    assert!(!keyed.ok);
    assert!(!keyed.reachable);
    assert!(keyed.note.contains("no key"), "{}", keyed.note);
    assert!(
        keyed.note.contains("SUSI_HARNESS_VERIFY_KEY_7F3A9C_UNSET"),
        "{}",
        keyed.note
    );
}

#[test]
fn selecting_one_provider_fetches_only_its_endpoint() {
    let urls = RefCell::new(Vec::new());
    let fetch = |url: &str, _: &[(&str, &str)]| {
        urls.borrow_mut().push(url.to_string());
        Ok((200, r#"{"data":[{"id":"beta-1"}]}"#.to_string()))
    };
    let report = verify_registry(&test_registry(), Some("beta"), &fetch).unwrap();
    assert!(report.ok);
    assert_eq!(report.providers.len(), 1);
    assert_eq!(report.providers[0].provider, "beta");
    assert_eq!(
        urls.borrow().as_slice(),
        ["https://beta.example/v1/models".to_string()]
    );
}

#[test]
fn selecting_an_unknown_provider_is_refused() {
    let fetch = |_: &str, _: &[(&str, &str)]| -> Result<(u16, String), String> {
        Ok((200, "{}".to_string()))
    };
    let error = verify_registry(&test_registry(), Some("nope"), &fetch)
        .unwrap_err()
        .to_string();
    assert!(error.contains("nope"), "{error}");
    assert!(error.contains("alpha"), "{error}");
}

/// DeepInfra's list says what each model is: discovery adds the chat
/// models the registry lacks, with the published context, cache, vision
/// and prices, tiers them by output price, and leaves the rest alone.
#[test]
fn discovery_adds_tagged_chat_models_with_their_published_prices() {
    let registry = Registry::parse(
        r#"{"providers":[{"id":"di","wire":"openai","endpoint":"https://di.example/v1/openai/chat/completions","key_env":[],
            "models":[{"id":"di-known","backend":"vendor/Known-1","tools":true,"tier":"worker"}]}]}"#,
    )
    .unwrap();
    let urls = RefCell::new(Vec::new());
    let fetch = |url: &str, _: &[(&str, &str)]| {
        urls.borrow_mut().push(url.to_string());
        Ok((200, r#"{"data":[
            {"id":"vendor/Known-1","metadata":{"tags":["chat"],"pricing":{"input_tokens":0.1,"output_tokens":0.2}}},
            {"id":"vendor/Flash-2.1","metadata":{"context_length":1048576,"tags":["chat","vision","prompt_cache"],"pricing":{"input_tokens":0.2,"output_tokens":0.6,"cache_read_tokens":0.006}}},
            {"id":"vendor/Pro","metadata":{"context_length":262144,"tags":["chat"],"pricing":{"input_tokens":1.3,"output_tokens":2.6}}},
            {"id":"vendor/Whisper","metadata":{"tags":["audio"],"pricing":{"input_tokens":0.1,"output_tokens":0.1}}},
            {"id":"vendor/Unpriced","metadata":{"tags":["chat"]}},
            {"id":"","metadata":{"tags":["chat"]}}
        ]}"#.to_string()))
    };
    let discovery = discover(&registry, None, &fetch).unwrap();
    assert_eq!(
        urls.borrow().as_slice(),
        ["https://di.example/v1/openai/models"]
    );
    assert_eq!(discovery.providers.len(), 1);
    let di = &discovery.providers[0];
    assert_eq!((di.id.as_str(), di.wire.as_str()), ("di", "openai"));
    assert_eq!(di.endpoint, "https://di.example/v1/openai/chat/completions");
    let ids = di
        .models
        .iter()
        .map(|model| model.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        ["di-vendor-flash-2-1", "di-vendor-pro", "di-vendor-unpriced"]
    );
    let flash = &di.models[0];
    assert_eq!(flash.backend, "vendor/Flash-2.1");
    assert_eq!(flash.context, Some(1_048_576));
    assert!(flash.tools && flash.streaming && flash.vision && flash.cache);
    assert_eq!(flash.tier, "worker");
    let pro = &di.models[1];
    assert_eq!(
        pro.tier, "rescue",
        "a dollar or more per million output tokens"
    );
    assert!(!pro.vision && !pro.cache);
    assert_eq!(di.models[2].tier, "worker");
    let flash_price = discovery.prices.get("di-vendor-flash-2-1").unwrap();
    assert_eq!(flash_price.input_miss.micros(), 200_000);
    assert_eq!(flash_price.input_hit.micros(), 6_000);
    assert_eq!(flash_price.output.micros(), 600_000);
    let pro_price = discovery.prices.get("di-vendor-pro").unwrap();
    assert_eq!(
        pro_price.input_hit, pro_price.input_miss,
        "no cache rate: the input rate"
    );
    assert!(!discovery.prices.contains_key("di-vendor-unpriced"));
    assert_eq!(
        discovery.notes,
        ["di: 1 configured, 3 more chat model(s) listed"]
    );
    // The document is a valid overlay for the registry.
    let overlay = serde_json::to_string(&discovery).unwrap();
    let merged = registry.merged(&overlay).unwrap();
    assert_eq!(
        merged.resolve("di-vendor-pro").unwrap().model_id,
        "vendor/Pro"
    );
}

/// SiliconFlow's list is ids alone, so its registry record names the
/// query that narrows it to chat models; a provider with neither a query
/// nor tags has its count reported and nothing added.
#[test]
fn discovery_narrows_with_the_models_query_or_adds_nothing_without_one() {
    let registry = Registry::parse(
        r#"{"aliases":{"sf-taken-1":"sf-one"},"providers":[
            {"id":"sf","wire":"openai","endpoint":"https://sf.example/v1/chat/completions","key_env":[],"models_query":"type=text&sub_type=chat",
             "models":[{"id":"sf-one","backend":"Taken/1","tools":true,"tier":"worker"}]},
            {"id":"plain","wire":"openai","endpoint":"https://plain.example/v1/chat/completions","key_env":[],
             "models":[{"id":"plain-one","backend":"p-1","tools":true,"tier":"worker"}]}]}"#,
    )
    .unwrap();
    let fetch = |url: &str, _: &[(&str, &str)]| {
        if url.starts_with("https://sf.example/") {
            assert_eq!(url, "https://sf.example/v1/models?type=text&sub_type=chat");
            return Ok((
                200,
                r#"{"data":[{"id":"Taken/1"},{"id":"vendor/New-Model"},{"id":"taken-1"}]}"#
                    .to_string(),
            ));
        }
        Ok((
            200,
            r#"{"data":[{"id":"p-1"},{"id":"p-2"},{"id":"p-3"}]}"#.to_string(),
        ))
    };
    let discovery = discover(&registry, None, &fetch).unwrap();
    assert_eq!(discovery.providers.len(), 1);
    let sf = &discovery.providers[0];
    assert_eq!(sf.id, "sf");
    let ids = sf
        .models
        .iter()
        .map(|model| model.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, ["sf-vendor-new-model"]);
    assert_eq!(sf.models[0].tier, "worker");
    assert!(
        discovery.prices.is_empty(),
        "nothing published, nothing priced"
    );
    assert_eq!(discovery.notes.len(), 2);
    assert!(
        discovery.notes[0].contains("1 more chat model(s)"),
        "{}",
        discovery.notes[0]
    );
    assert!(
        discovery.notes[0].contains("skipped, their registry id is taken: taken-1"),
        "{}",
        discovery.notes[0]
    );
    assert!(
        discovery.notes[1].contains("plain: lists 3 model(s)"),
        "{}",
        discovery.notes[1]
    );
    assert!(
        discovery.notes[1].contains("nothing added"),
        "{}",
        discovery.notes[1]
    );
}

/// An unkeyed provider is skipped unless it is the one named; an unknown
/// name is refused; a failed fetch is a note, not an error.
#[test]
fn discovery_skips_unkeyed_providers_and_reports_failures_as_notes() {
    let registry = test_registry();
    let calls = Cell::new(0);
    let fetch = |_: &str, _: &[(&str, &str)]| {
        calls.set(calls.get() + 1);
        Err("offline".to_string())
    };
    let discovery = discover(&registry, None, &fetch).unwrap();
    assert!(discovery.providers.is_empty());
    assert!(
        discovery
            .notes
            .iter()
            .any(|note| note == "keyed: skipped, no key"),
        "{:?}",
        discovery.notes
    );
    assert!(
        discovery
            .notes
            .iter()
            .any(|note| note.starts_with("alpha: ") && note.contains("offline")),
        "{:?}",
        discovery.notes
    );
    assert!(
        discovery
            .notes
            .iter()
            .any(|note| note.starts_with("closed: ") && note.contains("offline")),
        "{:?}",
        discovery.notes
    );
    assert_eq!(
        calls.get(),
        3,
        "alpha, beta and closed were asked; keyed was not"
    );
    let named = discover(&registry, Some("keyed"), &fetch).unwrap();
    assert!(named.notes[0].contains("no key in"), "{:?}", named.notes);
    assert!(discover(&registry, Some("nobody"), &fetch).is_err());
}

#[test]
fn a_backend_id_becomes_a_registry_id_suffix() {
    assert_eq!(
        slug("deepseek-ai/DeepSeek-V4.1-Flash"),
        "deepseek-ai-deepseek-v4-1-flash"
    );
    assert_eq!(slug("Qwen/Qwen3.8-Flash"), "qwen-qwen3-8-flash");
    assert_eq!(slug("--odd__name//"), "odd-name");
}
