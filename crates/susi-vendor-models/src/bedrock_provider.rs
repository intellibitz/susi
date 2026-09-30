//! AWS Bedrock provider with SigV4 markers.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BedrockRequest {
    pub region: String,
    pub model_id: String,
    pub auth: String,
}

/// Build a Bedrock invoke descriptor that requires SigV4.
#[must_use]
pub fn bedrock_invoke(region: &str, model_id: &str) -> BedrockRequest {
    BedrockRequest {
        region: region.into(),
        model_id: model_id.into(),
        auth: "aws_sigv4".into(),
    }
}

#[cfg(test)]
mod bedrock_provider_tests {
    use super::*;

    #[test]
    fn bedrock_provider_uses_sigv4() {
        let r = bedrock_invoke("us-east-1", "anthropic.claude-v2");
        assert_eq!(r.auth, "aws_sigv4");
        assert_eq!(r.region, "us-east-1");
    }
}
