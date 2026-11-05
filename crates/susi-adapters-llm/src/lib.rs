#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

//! SUSI-authored LLM provider adapters.
//!
//! Request/response wire shapes and extractors for OpenAI-compatible chat
//! and completions, Anthropic Messages, Gemini `generateContent`, and
//! Triton generate. This is not vendored vendor SDK source — it is the
//! first-party client contract GEMI and GAWD share. Genuinely vendored
//! GPU tensors live in `susi-vendor-candle`.

pub mod inference_wire;

#[cfg(test)]
mod tests {
    use crate::inference_wire::{
        anthropic_cache_payload, anthropic_text, gemini_model_path, gemini_text, openai_chat_body,
        openai_chat_text, openai_completions_body, openai_completions_text,
        openai_to_anthropic_messages, triton_body, triton_text, InferenceProtocol,
    };
    use serde_json::json;

    #[test]
    fn protocol_names_map_through_the_crate_root() {
        assert_eq!(
            InferenceProtocol::from_config("Anthropic"),
            InferenceProtocol::Anthropic
        );
        assert_eq!(
            InferenceProtocol::from_config("gemini"),
            InferenceProtocol::Gemini
        );
        assert_eq!(
            InferenceProtocol::from_config("TRITON"),
            InferenceProtocol::Triton
        );
        assert_eq!(
            InferenceProtocol::from_config("completions"),
            InferenceProtocol::OpenAiCompletions
        );
        // Unknown values default to the OpenAI-compatible majority.
        assert_eq!(
            InferenceProtocol::from_config("chat"),
            InferenceProtocol::OpenAiChat
        );
        assert_eq!(
            InferenceProtocol::from_config(""),
            InferenceProtocol::OpenAiChat
        );
    }

    #[test]
    fn request_bodies_and_reply_extractors_form_a_wire_round_trip() {
        let chat = openai_chat_body("gpt-test", "hi", 32);
        assert_eq!(chat["model"], "gpt-test");
        assert_eq!(
            chat["messages"][0],
            json!({"role": "user", "content": "hi"})
        );
        assert_eq!(chat["max_tokens"], 32);
        let reply = json!({"choices": [{"message": {"content": "hello"}}]});
        assert_eq!(openai_chat_text(&reply).unwrap(), "hello");
        let refused = json!({"choices": [{"message": {"refusal": "no"}}]});
        assert!(openai_chat_text(&refused).unwrap_err().contains("refused"));

        let completions = openai_completions_body("m", "p", 8);
        assert_eq!(
            completions,
            json!({"model": "m", "prompt": "p", "max_tokens": 8})
        );
        assert_eq!(
            openai_completions_text(&json!({"choices": [{"text": "done"}]})).unwrap(),
            "done"
        );

        let triton = triton_body("q", 4);
        assert_eq!(triton["text_input"], "q");
        assert_eq!(triton["parameters"]["max_tokens"], 4);
        assert_eq!(triton_text(&json!({"text_output": "t"})).unwrap(), "t");
        assert_eq!(
            triton_text(&json!({"outputs": [{"data": ["v2"]}]})).unwrap(),
            "v2"
        );
        assert!(triton_text(&json!({})).is_err());
    }

    #[test]
    fn mixed_case_protocols_kimi_thinking_envelope_and_empty_choices_rejection() {
        assert_eq!(
            InferenceProtocol::from_config("AnThRoPiC"),
            InferenceProtocol::Anthropic
        );
        assert_eq!(
            InferenceProtocol::from_config("GeMiNi"),
            InferenceProtocol::Gemini
        );
        assert_eq!(
            InferenceProtocol::from_config("TrItOn"),
            InferenceProtocol::Triton
        );
        assert_eq!(
            InferenceProtocol::from_config("CoMpLeTiOnS"),
            InferenceProtocol::OpenAiCompletions
        );

        let kimi = openai_chat_body("moonshot-v1-8k", "reason", 64);
        assert_eq!(kimi["thinking"], json!({"type": "enabled"}));

        assert!(openai_chat_text(&json!({})).is_err());
        assert!(openai_chat_text(&json!({"choices": []})).is_err());
        assert!(openai_completions_text(&json!({})).is_err());
        assert!(openai_completions_text(&json!({"choices": []})).is_err());
    }

    #[test]
    fn anthropic_and_gemini_bodies_and_text_extractors_round_trip_through_inference_wire() {
        let anthropic_req =
            openai_to_anthropic_messages(&openai_chat_body("claude-sonnet-4-6", "hello", 1024))
                .unwrap();
        assert_eq!(anthropic_req["model"], "claude-sonnet-4-6");
        assert_eq!(anthropic_req["max_tokens"], 1024);
        assert_eq!(
            anthropic_req["messages"],
            json!([{"role": "user", "content": "hello"}])
        );

        let cached_req = anthropic_cache_payload("claude-sonnet-4-6", "sys", "hello", None, 1024);
        assert_eq!(cached_req["model"], "claude-sonnet-4-6");
        assert_eq!(cached_req["max_tokens"], 1024);

        assert_eq!(
            anthropic_text(&json!({"content": [{"type": "text", "text": "world"}]})).unwrap(),
            "world"
        );
        assert_eq!(
            anthropic_text(&json!({})).unwrap_err(),
            "response carried no content[].text text"
        );

        assert_eq!(gemini_model_path("models/gemini-2.5-pro"), "gemini-2.5-pro");
        assert_eq!(
            gemini_text(&json!({
                "candidates": [{
                    "content": {
                        "parts": [
                            {"thought": true, "text": "skip-thought"},
                            {"text": "gem-out"}
                        ]
                    }
                }]
            }))
            .unwrap(),
            "gem-out"
        );
        assert_eq!(
            gemini_text(&json!({})).unwrap_err(),
            "response carried no candidates[0].content.parts[].text text"
        );
    }

    #[test]
    fn gemini_model_path_preserves_bare_id_and_inference_protocol_copy_eq_holds() {
        assert_eq!(gemini_model_path("gemini-2.5-flash"), "gemini-2.5-flash");
        let p = InferenceProtocol::OpenAiChat;
        let copied = p;
        assert_eq!(copied, p);
        assert!(!format!("{p:?}").is_empty());
    }
}
