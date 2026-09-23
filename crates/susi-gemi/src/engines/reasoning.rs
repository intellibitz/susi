// SUSI-Reason: Native Neural Reasoning Substrate
// 100% Rust implementation using Candle for Tier 2 Logic Distillation

use anyhow::{anyhow, Result};
use candle_core::{DType, Tensor};
use candle_nn::{Linear, Module, VarBuilder};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningSample {
    pub intent: String,
    pub blackboard_context: String,
    pub successful_outcome: String,
    pub timestamp: u64,
}

/// SUSI-Reason Native Tier 2 Model (Distilled Logic)
/// 4-Layer Deep Logic Substrate for high-fidelity reasoning emulation.
pub struct SusiReasoningModel {
    l1: Linear,
    l2: Linear,
    l3: Linear,
    l4: Linear,
}

impl SusiReasoningModel {
    pub const DIM: usize = 256;

    #[allow(unsafe_code)]
    pub fn load(global_dir: &Path) -> Result<Self> {
        let weights_path = global_dir.join("models/susi-reason.safetensors");
        if !weights_path.exists() {
            return Err(anyhow!(
                "SUSI-Reason weights not found. Run 'susi train_reason'."
            ));
        }

        let device = crate::hardware::HardwareProfiler::get_candle_device();
        let vb =
            unsafe { VarBuilder::from_mmaped_safetensors(&[weights_path], DType::F32, &device)? };

        let l1 = candle_nn::linear(Self::DIM, Self::DIM, vb.pp("logic_1"))?;
        let l2 = candle_nn::linear(Self::DIM, Self::DIM, vb.pp("logic_2"))?;
        let l3 = candle_nn::linear(Self::DIM, Self::DIM, vb.pp("logic_3"))?;
        let l4 = candle_nn::linear(Self::DIM, Self::DIM, vb.pp("logic_out"))?;

        Ok(Self { l1, l2, l3, l4 })
    }

    fn project_features(intent: &str, context: &str) -> Result<Vec<f32>> {
        let mut vec = vec![0.0f32; Self::DIM];
        let i_vec = crate::alpha::SusiAlphaModel::semantic_centroid_projection(intent, None)?;
        let c_vec = crate::alpha::SusiAlphaModel::semantic_centroid_projection(context, None)?;

        // Interleave for high-density feature mapping
        for (i, &v) in i_vec.iter().enumerate() {
            vec[i] = v;
        }
        for (i, &v) in c_vec.iter().enumerate() {
            vec[i + 128] = v;
        }

        Ok(vec)
    }

    pub fn reason(&self, intent: &str, context: &str) -> Result<Vec<f32>> {
        let device = crate::hardware::HardwareProfiler::get_candle_device();
        let features = Self::project_features(intent, context)?;
        let input = Tensor::from_vec(features, (1, Self::DIM), &device)?;

        let out = self.l1.forward(&input)?.relu()?;
        let out = self.l2.forward(&out)?.relu()?;
        let out = self.l3.forward(&out)?.relu()?;
        let out = self.l4.forward(&out)?;

        Ok(out.to_vec2::<f32>()?[0].clone())
    }
}
