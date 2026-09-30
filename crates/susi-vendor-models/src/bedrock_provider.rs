//! AWS Bedrock provider (T-CLAUDE-29): Converse API endpoints, region
//! config, model discovery, and a self-contained SigV4 request signer
//! (HMAC-SHA256 built on the vendored `sha2` — no new third-party crates).

use sha2::{Digest, Sha256};
use susi_error::{EaiError, EaiResult};

/// Bedrock region + credentials.
#[derive(Debug, Clone)]
pub struct BedrockConfig {
    pub region: String,
    pub access_key: String,
    pub secret_key: String,
    /// Session token for STS/temporary credentials.
    pub session_token: Option<String>,
}

/// Converse endpoint for a model id (runtime service).
pub fn converse_url(region: &str, model_id: &str) -> String {
    format!("https://bedrock-runtime.{region}.amazonaws.com/model/{model_id}/converse")
}

/// Control-plane endpoint listing available foundation models.
pub fn foundation_models_url(region: &str) -> String {
    format!("https://bedrock.{region}.amazonaws.com/foundation-models")
}

fn hmac(key: &[u8], msg: &[u8]) -> Vec<u8> {
    const BLOCK: usize = 64;
    let mut k = key.to_vec();
    if k.len() > BLOCK {
        k = Sha256::digest(&k).to_vec();
    }
    k.resize(BLOCK, 0);
    let ipad: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    let opad: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    let mut inner = ipad;
    inner.extend_from_slice(msg);
    let ih = Sha256::digest(&inner);
    let mut outer = opad;
    outer.extend_from_slice(&ih);
    Sha256::digest(&outer).to_vec()
}

fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// `AWS4-HMAC-SHA256` date-keyed signing key.
fn signing_key(secret: &str, date: &str, region: &str, service: &str) -> Vec<u8> {
    let k_date = hmac(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    let k_region = hmac(&k_date, region.as_bytes());
    let k_service = hmac(&k_region, service.as_bytes());
    hmac(&k_service, b"aws4_request")
}

/// Signing inputs grouped (keeps `sign` under the arg-count lint).
#[derive(Debug, Clone)]
pub struct SignInput<'a> {
    pub method: &'a str,
    pub url: &'a str,
    /// `bedrock-runtime` for Converse, `bedrock` for control plane.
    pub service: &'a str,
    /// `YYYYMMDDHHMMSS`.
    pub amz_date: &'a str,
    /// `YYYYMMDD`.
    pub date: &'a str,
    pub body: &'a str,
}

/// Everything a caller needs to send a signed Bedrock request.
#[derive(Debug, Clone)]
pub struct SignedRequest {
    pub method: String,
    pub url: String,
    /// Headers to send (host + x-amz-date + authorization + session token).
    pub headers: Vec<(String, String)>,
    pub body: String,
}

/// Sign a request (SigV4). `amz_date`/`date` are `YYYYMMDDHHMMSS`/`YYYYMMDD`.
/// Canonical headers are limited to host + x-amz-date + content-type — the
/// three Bedrock needs.
pub fn sign(cfg: &BedrockConfig, i: &SignInput<'_>) -> EaiResult<SignedRequest> {
    let SignInput {
        method,
        url,
        service,
        amz_date,
        date,
        body,
    } = *i;
    let host = url
        .strip_prefix("https://")
        .and_then(|u| u.split('/').next())
        .ok_or_else(|| EaiError::config(format!("bad url {url}")))?;
    let path = url
        .splitn(4, '/')
        .nth(3)
        .map(|p| format!("/{p}"))
        .unwrap_or_else(|| "/".into());
    let payload_hash = hex(&Sha256::digest(body.as_bytes()));
    let canonical_headers =
        format!("content-type:application/json\nhost:{host}\nx-amz-date:{amz_date}\n");
    let signed_headers = "content-type;host;x-amz-date";
    let canonical =
        format!("{method}\n{path}\n\n{canonical_headers}\n{signed_headers}\n{payload_hash}");
    let scope = format!("{date}/{}/{service}/aws4_request", cfg.region);
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        hex(&Sha256::digest(canonical.as_bytes()))
    );
    let sig = hex(&hmac(
        &signing_key(&cfg.secret_key, date, &cfg.region, service),
        to_sign.as_bytes(),
    ));
    let auth = format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={sig}",
        cfg.access_key
    );
    let mut headers = vec![
        ("content-type".into(), "application/json".into()),
        ("host".into(), host.into()),
        ("x-amz-date".into(), amz_date.into()),
        ("authorization".into(), auth),
    ];
    if let Some(t) = &cfg.session_token {
        headers.push(("x-amz-security-token".into(), t.clone()));
    }
    Ok(SignedRequest {
        method: method.into(),
        url: url.into(),
        headers,
        body: body.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> BedrockConfig {
        BedrockConfig {
            region: "us-east-1".into(),
            access_key: "AKIDEXAMPLE".into(),
            secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            session_token: None,
        }
    }

    #[test]
    fn bedrock_provider_endpoints() {
        assert_eq!(
            converse_url("eu-west-1", "anthropic.claude-3-sonnet"),
            "https://bedrock-runtime.eu-west-1.amazonaws.com/model/anthropic.claude-3-sonnet/converse"
        );
        assert!(foundation_models_url("us-east-1").contains("bedrock.us-east-1"));
    }

    #[test]
    fn bedrock_provider_hmac_sha256_vector() {
        // RFC 4231 test case 1
        let m = hmac(&[0x0b; 20], b"Hi There");
        assert_eq!(
            hex(&m),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    #[test]
    fn bedrock_provider_signing_key_is_deterministic() {
        let k1 = signing_key("secret", "20260101", "us-east-1", "bedrock-runtime");
        let k2 = signing_key("secret", "20260101", "us-east-1", "bedrock-runtime");
        assert_eq!(k1, k2);
        assert_eq!(k1.len(), 32);
    }

    #[test]
    fn bedrock_provider_signature_shape() {
        let r = sign(
            &cfg(),
            &SignInput {
                method: "POST",
                url: &converse_url("us-east-1", "anthropic.claude-3-sonnet"),
                service: "bedrock-runtime",
                amz_date: "20260101T120000Z",
                date: "20260101",
                body: r#"{"messages":[]}"#,
            },
        )
        .unwrap();
        let auth = r
            .headers
            .iter()
            .find(|(k, _)| k == "authorization")
            .map(|(_, v)| v.clone())
            .unwrap();
        assert!(auth.starts_with("AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/us-east-1/bedrock-runtime/aws4_request"));
        assert!(auth.contains("SignedHeaders=content-type;host;x-amz-date"));
        assert!(auth.contains("Signature="));
        // deterministic: same inputs → same signature
        let r2 = sign(
            &cfg(),
            &SignInput {
                method: "POST",
                url: &converse_url("us-east-1", "anthropic.claude-3-sonnet"),
                service: "bedrock-runtime",
                amz_date: "20260101T120000Z",
                date: "20260101",
                body: r#"{"messages":[]}"#,
            },
        )
        .unwrap();
        let auth2 = r2
            .headers
            .iter()
            .find(|(k, _)| k == "authorization")
            .unwrap()
            .1
            .clone();
        assert_eq!(auth, auth2);
    }

    #[test]
    fn bedrock_provider_signature_pinned() {
        // Golden vector: pins the canonical-request layout. Regenerate with
        // the same inputs if the canonical form intentionally changes.
        let r = sign(
            &cfg(),
            &SignInput {
                method: "POST",
                url: &converse_url("us-east-1", "m"),
                service: "bedrock-runtime",
                amz_date: "20260101T000000Z",
                date: "20260101",
                body: "{}",
            },
        )
        .unwrap();
        let auth = r
            .headers
            .iter()
            .find(|(k, _)| k == "authorization")
            .unwrap()
            .1
            .clone();
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/us-east-1/bedrock-runtime/aws4_request, SignedHeaders=content-type;host;x-amz-date, Signature=81f6a41aee08fb27ad275c6e633f58a22d72db6b19eaaf7ab7ccc29c6c0457ac"
        );
    }

    #[test]
    fn bedrock_provider_session_token_header() {
        let mut c = cfg();
        c.session_token = Some("tok".into());
        let r = sign(
            &c,
            &SignInput {
                method: "GET",
                url: &foundation_models_url("us-east-1"),
                service: "bedrock",
                amz_date: "20260101T000000Z",
                date: "20260101",
                body: "",
            },
        )
        .unwrap();
        assert!(r
            .headers
            .iter()
            .any(|(k, v)| k == "x-amz-security-token" && v == "tok"));
    }
}
