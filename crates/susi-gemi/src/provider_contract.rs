//! Reproducible cloud provider contract probes (VC-201-051).
//!
//! Opt-in credentialed checks and fixture-backed checks for auth, streaming,
//! tool calls, embeddings, usage, and errors. Capability claims always include
//! provider/model/version, observation time, and unsupported cases.

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeKind {
    Authentication,
    Streaming,
    ToolCalls,
    Embeddings,
    Usage,
    Errors,
}

impl ProbeKind {
    /// Every probe kind, in contract order.
    pub const ALL: [Self; 6] = [
        Self::Authentication,
        Self::Streaming,
        Self::ToolCalls,
        Self::Embeddings,
        Self::Usage,
        Self::Errors,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeStatus {
    Supported,
    Unsupported,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityClaim {
    pub provider: String,
    pub model: String,
    pub version: String,
    pub observed_unix: u64,
    pub kind: ProbeKind,
    pub status: ProbeStatus,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FixtureResponse {
    pub http_status: u16,
    pub body: serde_json::Value,
}

/// Run a fixture-backed probe (no network).
pub fn probe_fixture(
    provider: &str,
    model: &str,
    version: &str,
    kind: ProbeKind,
    fixture: &FixtureResponse,
) -> CapabilityClaim {
    let (status, detail) = match kind {
        ProbeKind::Authentication => {
            if fixture.http_status == 401 || fixture.http_status == 403 {
                (ProbeStatus::Supported, "auth rejection observed".into())
            } else if fixture.http_status == 200 {
                (ProbeStatus::Supported, "auth accepted".into())
            } else {
                (
                    ProbeStatus::Failed,
                    format!("unexpected status {}", fixture.http_status),
                )
            }
        }
        ProbeKind::Streaming => {
            if fixture.body.get("object").and_then(|v| v.as_str()) == Some("chat.completion.chunk")
                || fixture.body.get("stream").and_then(|v| v.as_bool()) == Some(true)
            {
                (ProbeStatus::Supported, "stream chunk shape ok".into())
            } else if fixture.body.get("unsupported").and_then(|v| v.as_bool()) == Some(true) {
                (
                    ProbeStatus::Unsupported,
                    "provider reports no streaming".into(),
                )
            } else {
                (ProbeStatus::Failed, "stream shape unrecognized".into())
            }
        }
        ProbeKind::ToolCalls => {
            if fixture
                .body
                .pointer("/choices/0/message/tool_calls")
                .is_some()
            {
                (ProbeStatus::Supported, "tool_calls present".into())
            } else if fixture.body.get("unsupported").and_then(|v| v.as_bool()) == Some(true) {
                (ProbeStatus::Unsupported, "no tool calling".into())
            } else {
                (ProbeStatus::Failed, "missing tool_calls".into())
            }
        }
        ProbeKind::Embeddings => {
            if fixture
                .body
                .get("data")
                .and_then(|v| v.as_array())
                .is_some()
                && fixture.body.get("object").and_then(|v| v.as_str()) == Some("list")
            {
                (ProbeStatus::Supported, "embedding list ok".into())
            } else if fixture.body.get("unsupported").and_then(|v| v.as_bool()) == Some(true) {
                (ProbeStatus::Unsupported, "no embeddings".into())
            } else {
                (ProbeStatus::Failed, "embedding shape bad".into())
            }
        }
        ProbeKind::Usage => {
            if fixture.body.pointer("/usage/total_tokens").is_some()
                || fixture.body.pointer("/usage/completion_tokens").is_some()
            {
                (ProbeStatus::Supported, "usage present".into())
            } else {
                (ProbeStatus::Unsupported, "usage omitted".into())
            }
        }
        ProbeKind::Errors => {
            if fixture.body.get("error").is_some() || fixture.http_status >= 400 {
                (ProbeStatus::Supported, "error envelope observed".into())
            } else {
                (ProbeStatus::Failed, "expected error fixture".into())
            }
        }
    };
    CapabilityClaim {
        provider: provider.to_string(),
        model: model.to_string(),
        version: version.to_string(),
        observed_unix: now_unix(),
        kind,
        status,
        detail,
    }
}

/// Probe kinds that need a generation call — the read-only credentialed
/// contract check deliberately never spends tokens, so these are recorded
/// `Unsupported` rather than claimed (or skipped, which would hide them).
const GENERATION_ONLY_KINDS: [ProbeKind; 4] = [
    ProbeKind::Streaming,
    ProbeKind::ToolCalls,
    ProbeKind::Embeddings,
    ProbeKind::Usage,
];

fn claim(
    provider: &str,
    model: &str,
    version: &str,
    kind: ProbeKind,
    status: ProbeStatus,
    detail: impl Into<String>,
) -> CapabilityClaim {
    CapabilityClaim {
        provider: provider.to_string(),
        model: model.to_string(),
        version: version.to_string(),
        observed_unix: now_unix(),
        kind,
        status,
        detail: detail.into(),
    }
}

/// The canned response corpus the fixture half of the contract exercises:
/// one representative response per probe kind, plus an `unsupported`
/// marker response so `Unsupported` claims are exercised too.
#[must_use]
pub fn fixture_corpus() -> Vec<(ProbeKind, FixtureResponse)> {
    vec![
        (
            ProbeKind::Authentication,
            FixtureResponse {
                http_status: 200,
                body: serde_json::json!({"data": []}),
            },
        ),
        (
            ProbeKind::Streaming,
            FixtureResponse {
                http_status: 200,
                body: serde_json::json!({"object": "chat.completion.chunk"}),
            },
        ),
        (
            ProbeKind::ToolCalls,
            FixtureResponse {
                http_status: 200,
                body: serde_json::json!({"choices": [{"message": {"tool_calls": [{"id": "c1"}]}}]}),
            },
        ),
        (
            ProbeKind::Embeddings,
            FixtureResponse {
                http_status: 200,
                body: serde_json::json!({"object": "list", "data": [{"embedding": [0.0]}]}),
            },
        ),
        (
            ProbeKind::Usage,
            FixtureResponse {
                http_status: 200,
                body: serde_json::json!({"usage": {"total_tokens": 12}}),
            },
        ),
        (
            ProbeKind::Errors,
            FixtureResponse {
                http_status: 429,
                body: serde_json::json!({"error": {"type": "rate_limit"}}),
            },
        ),
    ]
}

/// Every fixture-backed claim, in corpus order — the deterministic half
/// of the contract check an operator can always run.
#[must_use]
pub fn fixture_checks(provider: &str, model: &str, version: &str) -> Vec<CapabilityClaim> {
    fixture_corpus()
        .iter()
        .map(|(kind, fixture)| probe_fixture(provider, model, version, *kind, fixture))
        .collect()
}

/// Read-only credentialed probe over one configured target, with the
/// transport injected so tests never touch the network. Exercises the
/// checks a GET can honestly prove — authentication and the error
/// envelope — and records the generation-dependent kinds as
/// `Unsupported` instead of guessing.
#[must_use]
pub fn probe_credentialed_with(
    call: &dyn crate::credential_scout::CheapCall,
    provider: &str,
    model: &str,
    version: &str,
    target: &crate::credential_scout::CredentialTarget,
) -> Vec<CapabilityClaim> {
    let redact = |text: &str| {
        susi_error::redact::redact_patterns(std::slice::from_ref(&target.api_key), text)
    };
    let mut claims = Vec::new();

    // Authentication: the vendor's zero-token auth probe. A rejection is
    // still a contract answer — the auth path was exercised.
    let (url, headers) =
        crate::credential_scout::probe_request(target.protocol, &target.api_base, &target.api_key);
    let refs: Vec<(&str, &str)> = headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    claims.push(match call.get(&url, &refs) {
        Ok((s, _)) if (200..300).contains(&s) => claim(
            provider,
            model,
            version,
            ProbeKind::Authentication,
            ProbeStatus::Supported,
            "credential accepted",
        ),
        Ok((401 | 403, _)) => claim(
            provider,
            model,
            version,
            ProbeKind::Authentication,
            ProbeStatus::Supported,
            "auth rejection observed",
        ),
        Ok((s, body)) => {
            let snippet: String = body.chars().take(120).collect();
            claim(
                provider,
                model,
                version,
                ProbeKind::Authentication,
                ProbeStatus::Failed,
                redact(&format!("unexpected auth status {s}: {snippet}")),
            )
        }
        Err(e) => claim(
            provider,
            model,
            version,
            ProbeKind::Authentication,
            ProbeStatus::Failed,
            redact(&format!("transport failure: {e}")),
        ),
    });

    // Errors: a deliberately missing model must surface as an error
    // envelope, never a silent 200. Triton has no per-model path to miss.
    match target.protocol {
        crate::susi_core::inference_wire::InferenceProtocol::Triton => claims.push(claim(
            provider,
            model,
            version,
            ProbeKind::Errors,
            ProbeStatus::Unsupported,
            "raw-URL endpoint has no model path to miss",
        )),
        _ => {
            let miss_url = format!(
                "{}/models/__susi_contract_probe_missing__",
                target.api_base.trim_end_matches('/')
            );
            claims.push(match call.get(&miss_url, &refs) {
                Ok((s, _body)) if s >= 400 => claim(
                    provider,
                    model,
                    version,
                    ProbeKind::Errors,
                    ProbeStatus::Supported,
                    format!("error envelope observed (HTTP {s})"),
                ),
                Ok((s, body)) => {
                    let snippet: String = body.chars().take(120).collect();
                    claim(
                        provider,
                        model,
                        version,
                        ProbeKind::Errors,
                        ProbeStatus::Failed,
                        redact(&format!(
                            "missing model returned {s}, not an error: {snippet}"
                        )),
                    )
                }
                Err(e) => claim(
                    provider,
                    model,
                    version,
                    ProbeKind::Errors,
                    ProbeStatus::Failed,
                    redact(&format!("transport failure: {e}")),
                ),
            });
        }
    }

    for kind in GENERATION_ONLY_KINDS {
        claims.push(claim(
            provider,
            model,
            version,
            kind,
            ProbeStatus::Unsupported,
            "needs a generation call; the read-only contract check never spends tokens",
        ));
    }
    claims
}

/// Credentialed live probe — only runs when `SUSI_PROVIDER_PROBE=1`.
/// `provider == "*"` probes every configured credentialed endpoint;
/// otherwise the endpoint whose vendor name matches is probed (a name
/// with no configured endpoint yields honest `Unsupported` claims, never
/// silence).
#[must_use]
pub fn probe_credentialed_opt_in(
    provider: &str,
    model: &str,
    version: &str,
) -> Option<Vec<CapabilityClaim>> {
    if std::env::var("SUSI_PROVIDER_PROBE").as_deref() != Ok("1") {
        return None;
    }
    let transport = crate::credential_scout::TransportCall;
    let targets = crate::credential_scout::configured_targets();
    let selected: Vec<crate::credential_scout::CredentialTarget> = if provider == "*" {
        targets
    } else {
        targets
            .into_iter()
            .filter(|t| t.vendor == provider)
            .collect()
    };
    let claims = if selected.is_empty() {
        ProbeKind::ALL
            .iter()
            .map(|kind| {
                claim(
                    provider,
                    model,
                    version,
                    *kind,
                    ProbeStatus::Unsupported,
                    "no configured credentialed endpoint for this provider",
                )
            })
            .collect()
    } else {
        selected
            .iter()
            .flat_map(|t| probe_credentialed_with(&transport, provider, model, version, t))
            .collect()
    };
    Some(claims)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::susi_core::inference_wire::InferenceProtocol;

    struct Fake {
        /// (url substring, response) pairs checked in order; first match wins.
        replies: Vec<(String, Result<(u16, String), String>)>,
    }

    impl crate::credential_scout::CheapCall for Fake {
        fn get(&self, url: &str, _headers: &[(&str, &str)]) -> Result<(u16, String), String> {
            for (pat, r) in &self.replies {
                if url.contains(pat.as_str()) {
                    return r.clone();
                }
            }
            Err(format!("no canned reply for {url}"))
        }
    }

    fn target() -> crate::credential_scout::CredentialTarget {
        crate::credential_scout::CredentialTarget {
            vendor: "openai".into(),
            env: "OPENAI_API_KEY".into(),
            api_base: "https://api.example.test/v1".into(),
            protocol: InferenceProtocol::OpenAiChat,
            api_key: "sk-fake-secret-000".into(),
        }
    }

    #[test]
    fn provider_contract_credentialed_probe_exercises_auth_and_errors() {
        let fake = Fake {
            replies: vec![
                (
                    "__susi_contract_probe_missing__".into(),
                    Ok((404, r#"{"error":"no such model"}"#.into())),
                ),
                ("/models".into(), Ok((200, r#"{"data":[]}"#.into()))),
            ],
        };
        let claims = probe_credentialed_with(&fake, "openai", "gpt-x", "1", &target());
        assert_eq!(claims.len(), 6);
        let at = |k: ProbeKind| claims.iter().find(|c| c.kind == k).unwrap();
        assert_eq!(at(ProbeKind::Authentication).status, ProbeStatus::Supported);
        assert_eq!(at(ProbeKind::Errors).status, ProbeStatus::Supported);
        // Generation-only kinds are honest Unsupported, never guessed.
        for k in [
            ProbeKind::Streaming,
            ProbeKind::ToolCalls,
            ProbeKind::Embeddings,
            ProbeKind::Usage,
        ] {
            assert_eq!(at(k).status, ProbeStatus::Unsupported, "{k:?}");
        }
    }

    #[test]
    fn provider_contract_credentialed_probe_redacts_the_key_from_details() {
        let fake = Fake {
            replies: vec![(
                "/models".into(),
                Ok((500, "server broke on sk-fake-secret-000 tail".into())),
            )],
        };
        let claims = probe_credentialed_with(&fake, "openai", "gpt-x", "1", &target());
        for c in &claims {
            assert!(
                !c.detail.contains("sk-fake-secret-000"),
                "claim detail leaked the credential: {c:?}"
            );
        }
    }

    #[test]
    fn provider_contract_opt_in_gate() {
        // Without SUSI_PROVIDER_PROBE the public entry stays silent.
        // (Set inside the test: env mutation is #[allow]-gated repo-wide.)
        #[allow(clippy::disallowed_methods)]
        unsafe {
            std::env::remove_var("SUSI_PROVIDER_PROBE");
        }
        assert!(probe_credentialed_opt_in("openai", "gpt-x", "1").is_none());
    }
}
