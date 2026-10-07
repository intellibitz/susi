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

//! Candle / CUDA / Metal vendor substrate.
//!
//! Owns the Hugging Face Candle crate graph (and its CUDA / Metal / MKL
//! features) plus the Qwen2 GGUF split that was forked from
//! `candle-transformers` so CPU/GPU layer placement can live next to the
//! vendor tensors it calls. SUSI-authored inference hosts and hardware
//! catalogs depend on this crate; they do not declare Candle themselves.

pub use candle_core;
pub use candle_nn;
pub use candle_transformers;
pub use tokenizers;

pub mod device;
pub mod moe_gguf;
pub mod qwen2_split;
