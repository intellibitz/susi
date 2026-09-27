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
