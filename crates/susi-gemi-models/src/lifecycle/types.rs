//! Lifecycle verification / agent report types.
use serde::{Deserialize, Serialize};

pub struct ModelVerificationResult {
    pub model_id: String,
    pub path: String,
    pub file_size_bytes: u64,
    pub file_size_formatted: String,
    pub is_valid_gguf: bool,
    pub magic_header: String,
    pub test_inference_status: String,
    pub latency_ms: u128,
    pub checksum_verified: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelProvenance {
    pub source_url: String,
    pub timestamp: u64,
    pub original_checksum: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelAgentStepStatus {
    pub step: usize,
    pub model_label: String,
    pub hf_repo: String,
    pub status: String,
    pub bytes_downloaded: u64,
    pub expected_bytes: u64,
    pub percentage: f32,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelAgentReport {
    pub active_step: usize,
    pub total_steps: usize,
    pub total_discovered_on_system: usize,
    pub network_status: String,
    pub download_agent_active: bool,
    pub steps: Vec<ModelAgentStepStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelBenchmarkResult {
    pub model_id: String,
    pub name: String,
    pub is_local: bool,
    pub latency_ms: u128,
    pub tokens_per_sec: f32,
    pub status: String,
    pub memory_used_mb: f32,
    pub peak_memory_mb: f32,
}
