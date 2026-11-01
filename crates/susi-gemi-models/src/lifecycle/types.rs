//! Lifecycle verification / agent report types.
use serde::{Deserialize, Serialize};

/// Preparation status is control flow, not an inferred meaning of display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelProvisioningOutcome {
    Active,
    Ready(String),
    Queued,
    DownloadsDisabled,
    OfflineNoModel,
    TooSmall,
}

impl ModelProvisioningOutcome {
    pub fn terminal_reason(&self) -> Option<&'static str> {
        match self {
            Self::DownloadsDisabled => Some("Automatic downloads disabled"),
            Self::OfflineNoModel => Some(
                "No admitted local model is cached; network policy prevents automatic provisioning",
            ),
            Self::TooSmall => {
                Some("Available RAM or disk cannot safely accommodate the small starter")
            }
            Self::Active | Self::Ready(_) | Self::Queued => None,
        }
    }

    pub fn summary(&self) -> String {
        match self {
            Self::Active => "Automatic provisioning already active".into(),
            Self::Ready(model) => format!("Using verified cached local model {model}"),
            Self::Queued => "Audited 0.5B starter queued; larger downloads require task evidence and resource admission".into(),
            Self::DownloadsDisabled | Self::OfflineNoModel | Self::TooSmall => self.terminal_reason().unwrap_or_default().into(),
        }
    }
}

pub struct ModelVerificationResult {
    pub model_id: String,
    pub path: String,
    pub file_size_bytes: u64,
    pub file_size_formatted: String,
    pub is_valid_gguf: bool,
    /// The file's actual first four bytes (lossy), not an assumed "GGUF".
    pub magic_header: String,
    /// `NOT_RUN`: this is a static file check; no inference is executed.
    pub test_inference_status: String,
    pub latency_ms: u128,
    /// Whether a reference checksum is registered for this model.
    pub checksum_known: bool,
    /// True only when a reference checksum exists and matches the file.
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
