//! Hugging Face gated-model handling (T-CLAUDE-27): build the Authorization
//! header from `HF_TOKEN` (typically read from `cloud.env`), classify
//! licence/gating failures into clear, actionable errors, and guarantee the
//! token value can never leak into logs or error strings.

/// Header pair for an authenticated HF request. The raw token appears only
/// in the returned header value — it is never formatted into messages.
pub fn auth_header(token: &str) -> (&'static str, String) {
    ("Authorization", format!("Bearer {}", token.trim()))
}

/// Redacted token form safe for logs — constant, reveals nothing.
pub fn token_for_log(_token: &str) -> &'static str {
    "hf_***"
}

/// Gated-repo failure classes the UI should distinguish.
#[derive(Debug, Clone, PartialEq)]
pub enum GateError {
    /// 401/403 + a gated body — the user must accept the repo licence on the
    /// hub before any token works. Carries the repo licence URL.
    LicenceRequired { licence_url: String },
    /// 401 — token missing or rejected.
    TokenRequired,
    /// 403 — access requested but still under manual review.
    PendingReview,
    /// 403 — token valid but the account was denied the licence.
    AccessDenied,
    /// Anything else.
    Other { status: u16, summary: String },
}

impl GateError {
    /// One-line, token-free user message.
    pub fn guidance(&self) -> String {
        match self {
            GateError::LicenceRequired { licence_url } => {
                format!("this model is gated — accept its licence at {licence_url}, then retry")
            }
            GateError::TokenRequired => {
                "a Hugging Face token is required — run `susi keys add hf_<token>`".into()
            }
            GateError::PendingReview => {
                "access was requested and is pending review on the hub — check back later".into()
            }
            GateError::AccessDenied => {
                "the hub account was denied access to this repo — try another account or model"
                    .into()
            }
            GateError::Other { status, summary } => format!("hub error {status}: {summary}"),
        }
    }
}

/// Classify a hub response for a gated repo. `status` is the HTTP code;
/// `body` is the (small) error payload — scanned for HF's gating phrases.
/// `repo` builds the licence URL.
pub fn classify(status: u16, body: &str, repo: &str) -> GateError {
    let b = body.to_lowercase();
    let licence_url = format!("https://huggingface.co/{repo}");
    let gated_body = b.contains("gated") || b.contains("access to this model");
    if gated_body && b.contains("accept") && (status == 401 || status == 403) {
        return GateError::LicenceRequired { licence_url };
    }
    if b.contains("awaiting review") || b.contains("under review") || b.contains("pending") {
        return GateError::PendingReview;
    }
    match status {
        401 => GateError::TokenRequired,
        403 if gated_body => GateError::AccessDenied,
        403 => GateError::AccessDenied,
        s => GateError::Other {
            status: s,
            summary: body.chars().take(160).collect(),
        },
    }
}

/// Whether a request should attach the token at all (absent = anonymous).
/// Empty/whitespace tokens count as absent — a blank `HF_TOKEN` must not
/// send `Authorization: Bearer `.
pub fn token_or_none(token: Option<&str>) -> Option<String> {
    token
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hf_gated_models_auth_header_shape() {
        let (k, v) = auth_header("hf_abc123");
        assert_eq!(k, "Authorization");
        assert_eq!(v, "Bearer hf_abc123");
    }

    #[test]
    fn hf_gated_models_token_never_leaks() {
        let secret = "hf_SECRETtokendonotlog";
        let log = token_for_log(secret);
        assert!(!log.contains("SECRET"));
        for g in [
            classify(401, "unauthorized", "org/model").guidance(),
            classify(403, "gated repo, accept the licence", "org/model").guidance(),
            classify(403, "denied", "org/model").guidance(),
        ] {
            assert!(!g.contains(secret));
        }
    }

    #[test]
    fn hf_gated_models_licence_required_surfaces_url() {
        let e = classify(
            403,
            "Access to this model is gated. Accept the licence terms on the model page.",
            "meta-llama/Llama-3.1-8B",
        );
        match e {
            GateError::LicenceRequired { ref licence_url } => {
                assert_eq!(
                    licence_url,
                    "https://huggingface.co/meta-llama/Llama-3.1-8B"
                );
                assert!(e.guidance().contains("accept its licence"));
            }
            other => panic!("expected licence required, got {other:?}"),
        }
    }

    #[test]
    fn hf_gated_models_401_means_token_needed() {
        assert_eq!(
            classify(401, "invalid credentials", "m"),
            GateError::TokenRequired
        );
    }

    #[test]
    fn hf_gated_models_pending_review_distinct() {
        let e = classify(403, "Your request is under review by the repo owners", "m");
        assert_eq!(e, GateError::PendingReview);
    }

    #[test]
    fn hf_gated_models_blank_token_is_anonymous() {
        assert_eq!(token_or_none(Some("   ")), None);
        assert_eq!(token_or_none(Some(" hf_x ")).as_deref(), Some("hf_x"));
        assert_eq!(token_or_none(None), None);
    }

    #[test]
    fn hf_gated_models_other_errors_keep_status() {
        let e = classify(503, "upstream unavailable", "m");
        match e {
            GateError::Other { status, .. } => assert_eq!(status, 503),
            other => panic!("expected other, got {other:?}"),
        }
    }
}
