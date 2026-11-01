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

#[cfg(test)]
mod tests {
    use tokenizers::Tokenizer;

    #[test]
    fn pretrained_wordlevel_tokenization_round_trips_without_cpp_training() {
        // A complete, fixed pretrained vocabulary exercises the same JSON load,
        // encode and decode API used for downloaded model tokenizers, without I/O.
        let tokenizer = Tokenizer::from_bytes(
            br#"{
                "version":"1.0",
                "truncation":null,
                "padding":null,
                "added_tokens":[],
                "normalizer":null,
                "pre_tokenizer":{"type":"WhitespaceSplit"},
                "post_processor":null,
                "decoder":null,
                "model":{
                    "type":"WordLevel",
                    "vocab":{"[UNK]":0,"susi":1,"runs":2,"local":3,"ai":4},
                    "unk_token":"[UNK]"
                }
            }"#,
        )
        .unwrap();
        let input = "susi runs local ai";
        let encoded = tokenizer.encode(input, false).unwrap();
        assert_eq!(encoded.get_ids(), &[1, 2, 3, 4]);
        assert_eq!(encoded.get_tokens(), &["susi", "runs", "local", "ai"]);
        assert_eq!(encoded.get_offsets(), &[(0, 4), (5, 9), (10, 15), (16, 18)]);
        assert_eq!(tokenizer.decode(encoded.get_ids(), false).unwrap(), input);
        assert_eq!(
            tokenizer.encode("susi unknown", false).unwrap().get_ids(),
            &[1, 0]
        );

        let serialized = tokenizer.to_string(false).unwrap();
        let restored = Tokenizer::from_bytes(serialized.as_bytes()).unwrap();
        assert_eq!(
            restored.encode(input, false).unwrap().get_ids(),
            encoded.get_ids()
        );
    }
}
