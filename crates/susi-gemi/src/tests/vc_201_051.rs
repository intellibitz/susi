//! Cloud provider contract probes (VC-201-051).

use crate::provider_contract::{
    probe_credentialed_opt_in, probe_fixture, FixtureResponse, ProbeKind, ProbeStatus,
};

#[test]
fn vc_201_051_fixture_auth_streaming_tools_embeddings_usage_errors() {
    let claims = [
        probe_fixture(
            "fixture-openai",
            "gpt-test",
            "2024-01",
            ProbeKind::Authentication,
            &FixtureResponse {
                http_status: 401,
                body: serde_json::json!({"error": {"message": "bad key"}}),
            },
        ),
        probe_fixture(
            "fixture-openai",
            "gpt-test",
            "2024-01",
            ProbeKind::Streaming,
            &FixtureResponse {
                http_status: 200,
                body: serde_json::json!({"object": "chat.completion.chunk"}),
            },
        ),
        probe_fixture(
            "fixture-openai",
            "gpt-test",
            "2024-01",
            ProbeKind::ToolCalls,
            &FixtureResponse {
                http_status: 200,
                body: serde_json::json!({
                    "choices": [{"message": {"tool_calls": [{"id": "1"}]}}]
                }),
            },
        ),
        probe_fixture(
            "fixture-openai",
            "gpt-test",
            "2024-01",
            ProbeKind::Embeddings,
            &FixtureResponse {
                http_status: 200,
                body: serde_json::json!({"object": "list", "data": [{"embedding": [0.1]}]}),
            },
        ),
        probe_fixture(
            "fixture-openai",
            "gpt-test",
            "2024-01",
            ProbeKind::Usage,
            &FixtureResponse {
                http_status: 200,
                body: serde_json::json!({"usage": {"total_tokens": 12}}),
            },
        ),
        probe_fixture(
            "fixture-openai",
            "gpt-test",
            "2024-01",
            ProbeKind::Errors,
            &FixtureResponse {
                http_status: 500,
                body: serde_json::json!({"error": {"message": "boom"}}),
            },
        ),
    ];
    for c in &claims {
        assert!(!c.provider.is_empty());
        assert!(!c.model.is_empty());
        assert!(!c.version.is_empty());
        assert!(c.observed_unix > 0);
        assert_eq!(c.status, ProbeStatus::Supported);
    }
}

#[test]
fn vc_201_051_unsupported_cases_recorded() {
    let c = probe_fixture(
        "local-stub",
        "none",
        "0",
        ProbeKind::Streaming,
        &FixtureResponse {
            http_status: 200,
            body: serde_json::json!({"unsupported": true}),
        },
    );
    assert_eq!(c.status, ProbeStatus::Unsupported);
}

#[test]
fn vc_201_051_credentialed_opt_in_off_by_default() {
    unsafe {
        std::env::remove_var("SUSI_PROVIDER_PROBE");
    }
    assert!(probe_credentialed_opt_in("x", "y", "z").is_none());
}
