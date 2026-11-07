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

//! Candle CPU / Metal vendor substrate.
//!
//! Owns the supported Hugging Face Candle CPU / Metal crate graph plus the Qwen2 GGUF split that was forked from
//! `candle-transformers` so CPU/GPU layer placement can live next to the
//! vendor tensors it calls. SUSI-authored inference hosts and hardware
//! catalogs depend on this crate; they do not declare Candle themselves.

pub use candle_core;
pub use tokenizers;

/// Neural-network operations used by Susi's reviewed model routes.
///
/// Candle 0.11.1's CPU causal flash-attention API does not validate all tensor
/// dimensions before reconstructing raw slices. Susi's CPU models use explicit
/// matrix multiplication and softmax. Keep that unrelated upstream API outside
/// this facade; adding another operation requires a new boundary review.
///
/// ```compile_fail,E0432
/// use susi_vendor_candle::candle_nn::attention::flash_attn;
/// ```
///
/// ```compile_fail,E0432
/// use susi_vendor_candle::candle_nn::cpu_flash_attention;
/// ```
///
/// ```compile_fail,E0432
/// use susi_vendor_candle::candle_nn::varlen_attention;
/// ```
pub mod candle_nn {
    pub use ::candle_nn::{
        linear, AdamW, Embedding, Init, Linear, Module, Optimizer, ParamsAdamW, VarBuilder, VarMap,
    };

    /// Operations exercised by the current Qwen2, MoE and reflex models.
    pub mod ops {
        pub use ::candle_nn::ops::{log_softmax, sigmoid, silu, softmax, softmax_last_dim};
    }
}

/// Model implementations used by Susi's selected model dispatch.
///
/// The upstream dense Qwen3 model calls the unqualified CPU flash-attention
/// API. Susi's MoE implementation uses only its GGUF and rotary helper types;
/// those helpers are private to this crate. Exposing the upstream model catalog
/// would permit callers to bypass the reviewed dispatch.
///
/// ```compile_fail,E0432
/// use susi_vendor_candle::candle_transformers::models::quantized_qwen3::ModelWeights;
/// ```
///
/// ```compile_fail,E0432
/// use susi_vendor_candle::candle_transformers::models::qwen3;
/// ```
pub mod candle_transformers {
    pub mod models {
        pub mod bert {
            pub use ::candle_transformers::models::bert::{BertModel, Config};
        }

        pub mod quantized_llama {
            pub use ::candle_transformers::models::quantized_llama::ModelWeights;
        }
    }
}

#[cfg(test)]
mod tests {
    use tokenizers::Tokenizer;

    #[test]
    fn reviewed_nn_exports_execute_reflex_and_attention_operations() {
        use crate::candle_core::{DType, Device, Tensor};
        use crate::candle_nn::{
            linear, ops, AdamW, Module, Optimizer, ParamsAdamW, VarBuilder, VarMap,
        };
        let variables = VarMap::new();
        let builder = VarBuilder::from_varmap(&variables, DType::F32, &Device::Cpu);
        let layer = linear(2, 2, builder).unwrap();
        let input = Tensor::new(&[[1_f32, -1_f32]], &Device::Cpu).unwrap();
        let logits = layer.forward(&input).unwrap();
        let probabilities = ops::softmax_last_dim(&logits).unwrap();
        let sum = probabilities.sum_all().unwrap().to_scalar::<f32>().unwrap();
        assert!((sum - 1.0).abs() < 1e-6);
        let loss = ops::log_softmax(&logits, 1).unwrap().sum_all().unwrap();
        let mut optimizer = AdamW::new(variables.all_vars(), ParamsAdamW::default()).unwrap();
        optimizer.backward_step(&loss).unwrap();
        let after = layer.forward(&input).unwrap().to_vec2::<f32>().unwrap();
        assert!(after[0].iter().all(|value| value.is_finite()));
    }

    #[test]
    fn owned_safetensors_survive_source_drop_and_owner_move() {
        use candle_core::{safetensors::BufferedSafetensors, Device, Tensor};
        let path = std::env::temp_dir().join(format!(
            "susi-tensor-owner-{}.safetensors",
            std::process::id()
        ));
        let tensor = Tensor::new(&[1.0_f32, -2.5, 3.25], &Device::Cpu).unwrap();
        tensor.save_safetensors("weights", &path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        let owner = BufferedSafetensors::new(bytes).unwrap();
        let moved = Box::new(owner);
        assert_eq!(
            moved
                .load("weights", &Device::Cpu)
                .unwrap()
                .to_vec1::<f32>()
                .unwrap(),
            [1.0, -2.5, 3.25]
        );
        assert!(moved.get("missing").is_err());
        assert!(BufferedSafetensors::new(vec![0, 1, 2]).is_err());
    }

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
