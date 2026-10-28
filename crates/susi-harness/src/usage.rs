//! Provider usage extraction.
//!
//! The proxy sees a provider's own JSON. DeepSeek reports cache hits as
//! `prompt_cache_hit_tokens`, OpenAI-compatible providers as
//! `prompt_tokens_details.cached_tokens`, and native Gemini as
//! `usageMetadata.cachedContentTokenCount`. All three collapse into
//! [`TokenUsage`], which is what the price arithmetic consumes.

use crate::pricing::TokenUsage;
use serde_json::Value;
use susi_error::{EaiError, EaiResult};

/// Extract token usage from a provider response body.
///
/// Accepts an OpenAI-compatible `usage` object (including DeepSeek's cache
/// fields) or a native Gemini `usageMetadata` object.
///
/// # Errors
///
/// Returns `EaiError::config` when neither shape is present, or when the input
/// count is missing. Output defaults to zero only when the provider omits it,
/// which is legitimate for an empty completion.
pub fn token_usage_from_json(body: &Value) -> EaiResult<TokenUsage> {
    if let Some(usage) = body.get("usage") {
        openai_usage(usage)
    } else if let Some(usage) = body.get("usageMetadata") {
        gemini_usage(usage)
    } else {
        Err(EaiError::config(
            "provider response carries neither `usage` nor `usageMetadata`",
        ))
    }
}

fn openai_usage(usage: &Value) -> EaiResult<TokenUsage> {
    let input = required_u64(usage, "prompt_tokens")?;
    let output = optional_u64(usage, "completion_tokens").unwrap_or(0);
    let hit = optional_u64(usage, "prompt_cache_hit_tokens").or_else(|| {
        usage
            .get("prompt_tokens_details")
            .and_then(|details| details.get("cached_tokens"))
            .and_then(Value::as_u64)
    });
    Ok(TokenUsage::new(
        input,
        hit.unwrap_or(0),
        billed_output(output, optional_u64(usage, "total_tokens"), input),
    ))
}

fn gemini_usage(usage: &Value) -> EaiResult<TokenUsage> {
    let input = required_u64(usage, "promptTokenCount")?;
    let output = optional_u64(usage, "candidatesTokenCount")
        .unwrap_or(0)
        .saturating_add(optional_u64(usage, "thoughtsTokenCount").unwrap_or(0));
    let hit = optional_u64(usage, "cachedContentTokenCount");
    Ok(TokenUsage::new(
        input,
        hit.unwrap_or(0),
        billed_output(output, optional_u64(usage, "totalTokenCount"), input),
    ))
}

/// The output tokens a provider bills: the visible output, or whatever the
/// total holds beyond the prompt when that is more. Gemini bills hidden
/// thinking as output and its OpenAI endpoint reports it only in the total:
/// live, an 8-token prompt answered `completion_tokens: 1, total_tokens: 88`,
/// and metering the 1 under-priced that call 80 times over.
fn billed_output(output: u64, total: Option<u64>, input: u64) -> u64 {
    total.map_or(output, |total| output.max(total.saturating_sub(input)))
}

fn required_u64(value: &Value, field: &str) -> EaiResult<u64> {
    optional_u64(value, field)
        .ok_or_else(|| EaiError::config(format!("usage is missing `{field}`")))
}

fn optional_u64(value: &Value, field: &str) -> Option<u64> {
    value.get(field).and_then(Value::as_u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deepseek_reports_cache_hits_explicitly() {
        let usage = serde_json::json!({
            "prompt_tokens": 3_000_000,
            "completion_tokens": 200_000,
            "prompt_cache_hit_tokens": 2_400_000,
            "prompt_cache_miss_tokens": 600_000
        });
        let parsed = TokenUsage::new(3_000_000, 2_400_000, 200_000);
        assert_eq!(
            token_usage_from_json(&serde_json::json!({"usage": usage})).unwrap(),
            parsed
        );
        assert_eq!(parsed.cache_miss(), 600_000);
    }

    #[test]
    fn openai_style_details_are_read() {
        let body = serde_json::json!({
            "usage": {
                "prompt_tokens": 1_000,
                "completion_tokens": 50,
                "prompt_tokens_details": { "cached_tokens": 800 }
            }
        });
        assert_eq!(
            token_usage_from_json(&body).unwrap(),
            TokenUsage::new(1_000, 800, 50)
        );
    }

    #[test]
    fn native_gemini_metadata_is_read() {
        let body = serde_json::json!({
            "usageMetadata": {
                "promptTokenCount": 900,
                "candidatesTokenCount": 100,
                "cachedContentTokenCount": 600
            }
        });
        assert_eq!(
            token_usage_from_json(&body).unwrap(),
            TokenUsage::new(900, 600, 100)
        );
    }

    #[test]
    fn hidden_thinking_in_the_total_is_billed_as_output() {
        // Gemini's OpenAI endpoint, live: thinking only in the total.
        let compat = serde_json::json!({
            "usage": {"prompt_tokens": 8, "completion_tokens": 1, "total_tokens": 88}
        });
        assert_eq!(
            token_usage_from_json(&compat).unwrap(),
            TokenUsage::new(8, 0, 80)
        );
        // Native Gemini names it.
        let native = serde_json::json!({
            "usageMetadata": {
                "promptTokenCount": 8,
                "candidatesTokenCount": 1,
                "thoughtsTokenCount": 79,
                "totalTokenCount": 88
            }
        });
        assert_eq!(
            token_usage_from_json(&native).unwrap(),
            TokenUsage::new(8, 0, 80)
        );
        // A total that is just prompt plus output changes nothing.
        let plain = serde_json::json!({
            "usage": {"prompt_tokens": 1_000, "completion_tokens": 50, "total_tokens": 1_050}
        });
        assert_eq!(
            token_usage_from_json(&plain).unwrap(),
            TokenUsage::new(1_000, 0, 50)
        );
    }

    #[test]
    fn a_missing_usage_object_is_refused() {
        assert!(token_usage_from_json(&serde_json::json!({"id": "x"})).is_err());
    }

    #[test]
    fn a_missing_input_count_is_refused() {
        let body = serde_json::json!({"usage": {"completion_tokens": 5}});
        assert!(token_usage_from_json(&body).is_err());
    }

    #[test]
    fn a_cache_hit_larger_than_the_input_is_clamped() {
        let body = serde_json::json!({
            "usage": {"prompt_tokens": 10, "completion_tokens": 1, "prompt_cache_hit_tokens": 99}
        });
        let usage = token_usage_from_json(&body).unwrap();
        assert_eq!(usage.cache_hit, 10);
        assert_eq!(usage.cache_miss(), 0);
    }
}
