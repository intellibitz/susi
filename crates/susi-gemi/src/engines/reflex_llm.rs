// SUSI-Reflex: Tier 1 Generative Reflex Engine
// 100% Rust implementation using Candle for Sub-1B LLM fast-path triage.

use anyhow::{anyhow, Result};
use std::path::Path;
use std::sync::OnceLock;

use super::runtime::InferenceHost;
use crate::hardware::HardwareProfiler;
use crate::models::ModelManager;
use crate::susi_core::task_manager::SwarmTaskManager;
use tokenizers::Tokenizer;

pub struct GenerativeReflexEngine;

impl GenerativeReflexEngine {
    const REFLEX_MODEL_ID: &'static str = "qwen2.5-0.5b-instruct";

    pub fn global() -> &'static Self {
        static ENGINE: OnceLock<GenerativeReflexEngine> = OnceLock::new();
        ENGINE.get_or_init(|| GenerativeReflexEngine)
    }

    /// Attempts to route the intent using an embedded sub-1B parameter model.
    pub fn try_solve(&self, intent: &str, _workspace: &Path) -> Result<String> {
        let task_handle =
            SwarmTaskManager::global().register_task("tier1_reflex", "Generative Reflex Fast-Path");

        let model_path = ModelManager::get_model_path(Self::REFLEX_MODEL_ID).ok_or_else(|| {
            anyhow!(
                "Tier 1 Reflex Model '{}' not provisioned.",
                Self::REFLEX_MODEL_ID
            )
        })?;
        let tokenizer_path = ModelManager::get_tokenizer_path(Self::REFLEX_MODEL_ID)
            .ok_or_else(|| anyhow!("Tokenizer missing for Reflex Model."))?;

        let file_size = std::fs::metadata(&model_path)
            .map(|m| m.len() as usize)
            .unwrap_or(0);
        let device = HardwareProfiler::get_dynamic_device(file_size);

        // This utilizes the ambient caching in InferenceHost to keep the 0.5B model resident
        let substrate_shared = InferenceHost::get_model(&model_path, &device, &task_handle)
            .map_err(|e| anyhow!("Failed to load reflex model: {}", e))?;

        let mut substrate = substrate_shared.write();

        let tokenizer =
            Tokenizer::from_file(tokenizer_path).map_err(|e| anyhow!("Tokenizer Error: {}", e))?;

        // Fast-path prompt design: strictly limited output
        let reflex_prompt = format!(
            "<|im_start|>system\nYou are a fast reflex engine. Output exactly the single ACTION command for the intent. Do not output anything else.<|im_end|>\n<|im_start|>user\nIntent: {}<|im_end|>\n<|im_start|>assistant\nACTION:",
            intent
        );

        let tokens = tokenizer
            .encode(reflex_prompt, true)
            .map_err(|e| anyhow!("Tokenization Error: {}", e))?;

        let prompt_tokens = tokens.get_ids();
        let mut all_tokens = vec![];
        let mut tokens_to_process = prompt_tokens.to_vec();

        let cfg = crate::susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
        let max_tokens = 64; // Strict limit for reflex speed
        let mut eos_token_ids = cfg.eos_token_ids();
        eos_token_ids.extend(substrate.eos_token_ids.iter().copied());

        for i in 0..max_tokens {
            let input = candle_core::Tensor::new(tokens_to_process.as_slice(), &device)
                .map_err(|e| anyhow!("Tensor error: {}", e))?
                .unsqueeze(0)
                .map_err(|e| anyhow!("Tensor Error: {}", e))?;

            let pos = if i == 0 {
                0
            } else {
                prompt_tokens.len() + i - 1
            };

            let logits = substrate
                .weights
                .forward(&input, pos)
                .map_err(|e| anyhow!("Model forward failed: {}", e))?;

            let logits_slice = logits
                .squeeze(0)
                .map_err(|e| anyhow!("Squeeze Error: {}", e))?;
            let last_logits_tensor = if logits_slice.rank() == 2 {
                let seq_len = logits_slice
                    .dim(0)
                    .map_err(|e| anyhow!("Dim Error: {}", e))?;
                logits_slice
                    .get(seq_len - 1)
                    .map_err(|e| anyhow!("Get Error: {}", e))?
            } else if logits_slice.rank() == 1 {
                logits_slice
            } else {
                logits_slice
                    .flatten_all()
                    .map_err(|e| anyhow!("Flatten Error: {}", e))?
            };

            let logits_v = last_logits_tensor
                .to_vec1::<f32>()
                .map_err(|e| anyhow!("ToVec Error: {}", e))?;

            let mut next_token = 0u32;
            let mut max_logit = f32::NEG_INFINITY;
            for (id, &logit) in logits_v.iter().enumerate() {
                if logit > max_logit {
                    max_logit = logit;
                    next_token = id as u32;
                }
            }

            all_tokens.push(next_token);

            if eos_token_ids.contains(&next_token) {
                break;
            }
            tokens_to_process = vec![next_token];
        }

        let output = tokenizer
            .decode(&all_tokens, true)
            .map_err(|e| anyhow!("Decoding Error: {}", e))?;

        task_handle.mark_completed("Reflex routing successful.");
        Ok(format!("ACTION: {}", output.trim()))
    }
}
