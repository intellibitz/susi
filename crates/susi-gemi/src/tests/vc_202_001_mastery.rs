//! Mastery verification for VC-202-001: every configured provider
//! credential is enumerated, probed with the cheapest possible call, and
//! classified live / expired / unauthorized / rate-limited / unknown with
//! the evidence recorded — and keys are never deleted.
//!
//! `credential_scout` splits the hermetic core (`scout_targets` over an
//! injected `CheapCall`) from the production wiring
//! (`scout_configured_once`, called from
//! `register_configured_cloud_endpoints` on the daemon registration path).

use std::cell::RefCell;
use std::collections::HashMap;

use crate::credential_scout::{
    classify_probe, probe_request, scout_targets, CheapCall, CredentialTarget, Liveness,
};
use crate::susi_core::inference_wire::InferenceProtocol;

/// One observed request: URL plus owned header pairs.
type RecordedRequest = (String, Vec<(String, String)>);

/// Records every request and answers from a per-host response table so no
/// test ever touches the network.
struct FakeCall {
    responses: HashMap<String, Result<(u16, String), String>>,
    requests: RefCell<Vec<RecordedRequest>>,
}

impl FakeCall {
    fn new() -> Self {
        Self {
            responses: HashMap::new(),
            requests: RefCell::new(Vec::new()),
        }
    }
}

impl CheapCall for FakeCall {
    fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<(u16, String), String> {
        self.requests.borrow_mut().push((
            url.to_string(),
            headers
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        ));
        match self.responses.get(url) {
            Some(Ok((s, b))) => Ok((*s, b.clone())),
            Some(Err(e)) => Err(e.clone()),
            None => Err(format!("no fixture for {url}")),
        }
    }
}

fn target(
    vendor: &str,
    env: &str,
    base: &str,
    protocol: InferenceProtocol,
    key: &str,
) -> CredentialTarget {
    CredentialTarget {
        vendor: vendor.to_string(),
        env: env.to_string(),
        api_base: base.to_string(),
        protocol,
        api_key: key.to_string(),
    }
}

/// Every configured credential lands in the report exactly once — none
/// skipped, none added, none deleted — and each was probed with the
/// zero-token `GET {base}/models` carrying its credential in the
/// vendor's auth header.
#[test]
fn vc_202_001_mastery_every_configured_credential_is_enumerated_and_probed() {
    let targets = vec![
        target(
            "openai",
            "OPENAI_API_KEY",
            "https://api.openai.com/v1",
            InferenceProtocol::OpenAiChat,
            "sk-openai-secretvalue",
        ),
        target(
            "anthropic",
            "ANTHROPIC_API_KEY",
            "https://api.anthropic.com/v1",
            InferenceProtocol::Anthropic,
            "sk-ant-secretvalue",
        ),
        target(
            "gemini",
            "GEMINI_API_KEY",
            "https://generativelanguage.googleapis.com/v1beta",
            InferenceProtocol::Gemini,
            "AIza-secretvalue",
        ),
    ];
    let mut fake = FakeCall::new();
    for t in &targets {
        let (url, _) = probe_request(t.protocol, &t.api_base, &t.api_key);
        fake.responses
            .insert(url, Ok((200, "{\"data\":[]}".to_string())));
    }

    let report = scout_targets(&fake, &targets);

    // One verdict per configured credential — the full inventory.
    assert_eq!(report.verdicts.len(), targets.len());
    for (t, v) in targets.iter().zip(&report.verdicts) {
        assert_eq!(v.vendor, t.vendor);
        assert_eq!(v.env, t.env);
        assert_eq!(v.liveness, Liveness::Live);
    }

    // Each credential got exactly one cheapest-possible probe.
    let requests = fake.requests.borrow();
    assert_eq!(requests.len(), targets.len());
    for (t, (url, headers)) in targets.iter().zip(requests.iter()) {
        assert_eq!(
            *url,
            format!("{}/models", t.api_base),
            "the cheapest call is the zero-token models listing"
        );
        let auth = headers
            .iter()
            .find(|(k, _)| matches!(k.as_str(), "Authorization" | "x-api-key" | "x-goog-api-key"))
            .unwrap_or_else(|| panic!("{}: probe carried no credential header", t.vendor));
        assert!(
            auth.1.contains(&t.api_key),
            "{}: probe authenticated with the configured key",
            t.vendor
        );
    }
}

/// The five required classes are produced from real provider evidence:
/// 2xx live, 401 unauthorized, quota/credit death expired, 429
/// rate-limited, transport failure unknown — never guessed.
#[test]
fn vc_202_001_mastery_classification_covers_all_required_classes() {
    let cases = [
        (Some(200), "{\"data\":[]}", Liveness::Live),
        (
            Some(401),
            "{\"error\":{\"message\":\"invalid api key\"}}",
            Liveness::Unauthorized,
        ),
        (
            Some(403),
            "{\"error\":\"forbidden\"}",
            Liveness::Unauthorized,
        ),
        (
            Some(429),
            "{\"error\":{\"code\":\"insufficient_quota\"}}",
            Liveness::Expired,
        ),
        (Some(402), "credit balance too low", Liveness::Expired),
        (Some(429), "rate limit reached", Liveness::RateLimited),
        (Some(503), "upstream down", Liveness::Unknown),
        (None, "connection refused", Liveness::Unknown),
    ];
    for (status, body, expected) in cases {
        assert_eq!(
            classify_probe(status, body),
            expected,
            "status={status:?} body={body}"
        );
    }
}

/// Evidence is recorded per verdict — status, redacted detail, observation
/// time and a key fingerprint — while no secret material ever reaches the
/// report, even when the provider echoes the key back in its error body.
#[test]
fn vc_202_001_mastery_report_records_evidence_without_secret_material() {
    let echo = "Authorization failed for key sk-openai-secretvalue — check the dashboard";
    let targets = vec![
        target(
            "openai",
            "OPENAI_API_KEY",
            "https://api.openai.com/v1",
            InferenceProtocol::OpenAiChat,
            "sk-openai-secretvalue",
        ),
        target(
            "groq",
            "GROQ_API_KEY",
            "https://api.groq.com/openai/v1",
            InferenceProtocol::OpenAiChat,
            "gsk-groq-secretvalue",
        ),
    ];
    let mut fake = FakeCall::new();
    fake.responses.insert(
        "https://api.openai.com/v1/models".to_string(),
        Ok((401, format!("{{\"error\": \"{echo}\"}}"))),
    );
    fake.responses.insert(
        "https://api.groq.com/openai/v1/models".to_string(),
        Ok((200, "{\"data\":[]}".to_string())),
    );

    let report = scout_targets(&fake, &targets);
    let rendered = report.format();

    // Every verdict carries recorded evidence.
    for v in &report.verdicts {
        assert!(v.observed_unix > 0);
        assert!(!v.fingerprint.is_empty());
        assert!(!v.detail.is_empty());
        assert_eq!(v.fingerprint.len(), 16, "fingerprint is truncated sha-256");
    }

    // The report answers "which credentials work today" in one artifact.
    assert!(rendered.contains("openai"));
    assert!(rendered.contains("live"));
    assert!(rendered.contains("2 credential(s) scouted: 1 live"));

    // No secret material anywhere — not in detail, not in the report.
    for t in &targets {
        assert!(
            !rendered.contains(&t.api_key),
            "report must not contain the key for {}",
            t.vendor
        );
        for v in &report.verdicts {
            assert!(
                !v.detail.contains(&t.api_key),
                "detail must not contain the key: {}",
                v.detail
            );
            assert_ne!(
                v.fingerprint, t.api_key,
                "fingerprint is a hash, not the key"
            );
        }
    }
}

/// A dead credential is still enumerated and reported — scouting marks it,
/// it never deletes it. The only mutation is recorded evidence.
#[test]
fn vc_202_001_mastery_dead_keys_are_marked_never_deleted() {
    let targets = vec![
        target(
            "mistral",
            "MISTRAL_API_KEY",
            "https://api.mistral.ai/v1",
            InferenceProtocol::OpenAiChat,
            "mistral-secretvalue",
        ),
        target(
            "together",
            "TOGETHER_API_KEY",
            "https://api.together.xyz/v1",
            InferenceProtocol::OpenAiChat,
            "together-secretvalue",
        ),
    ];
    let mut fake = FakeCall::new();
    fake.responses.insert(
        "https://api.mistral.ai/v1/models".to_string(),
        Ok((401, "unauthorized".to_string())),
    );
    fake.responses.insert(
        "https://api.together.xyz/v1/models".to_string(),
        Ok((200, "{\"data\":[]}".to_string())),
    );

    let report = scout_targets(&fake, &targets);

    // The unauthorized key remains in the inventory, classified — a scout
    // report can only shrink the working set by marking, never by removal.
    assert_eq!(report.verdicts.len(), 2);
    let dead = report
        .verdicts
        .iter()
        .find(|v| v.vendor == "mistral")
        .expect("dead key still enumerated");
    assert_eq!(dead.liveness, Liveness::Unauthorized);
    assert!(report
        .verdicts
        .iter()
        .any(|v| v.vendor == "together" && v.liveness == Liveness::Live));
}
