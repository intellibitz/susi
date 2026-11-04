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
        openai_chat_body, openai_chat_text, openai_completions_body, openai_completions_text,
        triton_body, triton_text, InferenceProtocol,
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
}
