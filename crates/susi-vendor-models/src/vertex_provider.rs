//! Google Vertex AI provider (T-CLAUDE-31): service-account JWT bearer
//! exchange, access-token caching, regional endpoints, model discovery.
//!
//! RS256 signing is injected via [`RsaSigner`] — no RSA crate exists in the
//! vendored set, so the caller supplies the signature (KMS sign, a vendor
//! crypto facade, or `gcloud`); tests use a fake signer. Everything else —
//! JWT assembly, exchange request, cache, URL building — lives here.

use base64::Engine;
use susi_error::EaiResult;

fn b64url(data: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
}

/// Signs the JWT signing-input with RSA-SHA256 for the service account.
pub trait RsaSigner {
    fn sign_rs256(&self, signing_input: &[u8]) -> EaiResult<Vec<u8>>;
}

/// Service-account fields needed for the JWT bearer grant.
#[derive(Debug, Clone)]
pub struct ServiceAccount {
    pub client_email: String,
    /// OAuth scope, e.g. `https://www.googleapis.com/auth/cloud-platform`.
    pub scope: String,
}

#[derive(Debug, Clone)]
pub struct VertexConfig {
    pub project: String,
    /// e.g. `us-central1`.
    pub region: String,
}

/// Mint an unsigned-then-signed RS256 JWT for the OAuth2 JWT bearer grant.
pub fn mint_jwt(
    sa: &ServiceAccount,
    signer: &dyn RsaSigner,
    issued_at: i64,
    lifetime_secs: i64,
) -> EaiResult<String> {
    let header = b64url(br#"{"alg":"RS256","typ":"JWT"}"#);
    let claims = format!(
        r#"{{"iss":"{}","scope":"{}","aud":"https://oauth2.googleapis.com/token","iat":{},"exp":{}}}"#,
        sa.client_email,
        sa.scope,
        issued_at,
        issued_at + lifetime_secs
    );
    let claims = b64url(claims.as_bytes());
    let signing_input = format!("{header}.{claims}");
    let sig = signer.sign_rs256(signing_input.as_bytes())?;
    Ok(format!("{signing_input}.{}", b64url(&sig)))
}

/// The `application/x-www-form-urlencoded` token-exchange request body.
pub fn token_request_body(assertion: &str) -> String {
    format!(
        "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion={assertion}"
    )
}

/// Cached access token with refresh-window logic.
#[derive(Debug, Clone, Default)]
pub struct TokenCache {
    pub token: Option<String>,
    /// unix seconds when the token stops being usable.
    pub expires_at: i64,
}

impl TokenCache {
    /// Refresh margin: renew when less than 60s remains.
    pub const MARGIN: i64 = 60;

    /// `true` when a fetch is required (`now` within the margin).
    pub fn needs_refresh(&self, now: i64) -> bool {
        self.token.is_none() || self.expires_at - now < Self::MARGIN
    }

    pub fn store(&mut self, token: String, expires_in_secs: i64, now: i64) {
        self.token = Some(token);
        self.expires_at = now + expires_in_secs;
    }
}

impl VertexConfig {
    /// generateContent endpoint for a Google-publisher model.
    pub fn generate_url(&self, model: &str) -> String {
        format!(
            "https://{}-aiplatform.googleapis.com/v1/projects/{}/locations/{}/publishers/google/models/{}:generateContent",
            self.region, self.project, self.region, model
        )
    }

    /// streamGenerateContent variant.
    pub fn stream_url(&self, model: &str) -> String {
        self.generate_url(model)
            .replace(":generateContent", ":streamGenerateContent")
    }

    /// Model garden listing for the project/region (discovery + health).
    pub fn models_url(&self) -> String {
        format!(
            "https://{}-aiplatform.googleapis.com/v1/projects/{}/locations/{}/publishers/google/models",
            self.region, self.project, self.region
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeSigner;
    impl RsaSigner for FakeSigner {
        fn sign_rs256(&self, input: &[u8]) -> EaiResult<Vec<u8>> {
            // deterministic fake: reversed input
            Ok(input.iter().rev().copied().collect())
        }
    }

    fn sa() -> ServiceAccount {
        ServiceAccount {
            client_email: "svc@proj.iam.gserviceaccount.com".into(),
            scope: "https://www.googleapis.com/auth/cloud-platform".into(),
        }
    }

    #[test]
    fn vertex_provider_jwt_three_segments() {
        let jwt = mint_jwt(&sa(), &FakeSigner, 1_000, 3_600).unwrap();
        let segs: Vec<&str> = jwt.split('.').collect();
        assert_eq!(segs.len(), 3);
        // claims decode
        let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(segs[1])
            .unwrap();
        let c = String::from_utf8(claims).unwrap();
        assert!(c.contains("\"iss\":\"svc@proj.iam.gserviceaccount.com\""));
        assert!(c.contains("\"exp\":4600"));
        assert!(c.contains("oauth2.googleapis.com/token"));
    }

    #[test]
    fn vertex_provider_exchange_body_is_form_encoded() {
        let b = token_request_body("X.Y.Z");
        assert!(b.contains("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer"));
        assert!(b.contains("assertion=X.Y.Z"));
    }

    #[test]
    fn vertex_provider_token_cache_refresh_window() {
        let mut c = TokenCache::default();
        assert!(c.needs_refresh(100));
        c.store("tok".into(), 3600, 100);
        assert!(!c.needs_refresh(100 + 3500));
        assert!(c.needs_refresh(100 + 3541)); // inside the 60s margin
        assert!(c.needs_refresh(100 + 3600));
    }

    #[test]
    fn vertex_provider_regional_endpoints() {
        let v = VertexConfig {
            project: "p1".into(),
            region: "europe-west4".into(),
        };
        assert_eq!(
            v.generate_url("gemini-2.5-pro"),
            "https://europe-west4-aiplatform.googleapis.com/v1/projects/p1/locations/europe-west4/publishers/google/models/gemini-2.5-pro:generateContent"
        );
        assert!(v.stream_url("m").contains(":streamGenerateContent"));
        assert!(v.models_url().contains("publishers/google/models"));
    }
}
