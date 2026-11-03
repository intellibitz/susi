//! Lifecycle verification / agent report types.
use serde::{Deserialize, Serialize};

/// Outcome of the explicit starter install (`STARTER_INSTALL_COMMAND`).
/// Control flow, not an inferred meaning of display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelProvisioningOutcome {
    /// Another install of the starter is already running in this process.
    Active,
    /// A verified local model was already installed; nothing was downloaded.
    Ready(String),
    /// The starter was downloaded and verified by this request.
    Installed(String),
    OfflineNoModel,
    TooSmall,
}

impl ModelProvisioningOutcome {
    /// Why the request could not install a model, when it could not.
    pub fn terminal_reason(&self) -> Option<&'static str> {
        match self {
            Self::OfflineNoModel => Some(
                "No admitted local model is cached, and network policy blocks the starter download",
            ),
            Self::TooSmall => {
                Some("Available RAM or disk cannot safely accommodate the small starter")
            }
            Self::Active | Self::Ready(_) | Self::Installed(_) => None,
        }
    }

    pub fn summary(&self) -> String {
        match self {
            Self::Active => "The starter model install is already running".into(),
            Self::Ready(model) => format!("Using verified cached local model {model}"),
            Self::Installed(model) => format!("Installed and verified local model {model}"),
            Self::OfflineNoModel | Self::TooSmall => {
                self.terminal_reason().unwrap_or_default().into()
            }
        }
    }
}

/// No verified local model is installed. Core never downloads one by itself
/// (CEO decision 2026-11-04, "No, ask first"): a request that needs a local
/// model gets this error, which names the one command that downloads the
/// audited starter and its size. Cloud models need no download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoLocalModelInstalled {
    /// The model the install command downloads.
    pub model: String,
    /// Its download size in bytes.
    pub size_bytes: u64,
    /// The exact command that downloads it.
    pub install_command: &'static str,
}

impl NoLocalModelInstalled {
    /// Leading tag, so callers that carry the error as text can detect it.
    pub const TAG: &'static str = "[NO_LOCAL_MODEL]";

    /// The audited starter the install command downloads.
    #[must_use]
    pub fn starter() -> Self {
        let starter = crate::zc_first_model::audited_starter();
        Self {
            model: starter.id,
            size_bytes: starter.size_bytes,
            install_command: crate::zc_first_model::STARTER_INSTALL_COMMAND,
        }
    }

    /// Decimal megabytes, rounded: what a download dialog shows.
    #[must_use]
    pub fn size_mb(&self) -> u64 {
        self.size_bytes.saturating_add(500_000) / 1_000_000
    }

    /// One line for status surfaces: the command, the model and its size.
    #[must_use]
    pub fn install_hint(&self) -> String {
        format!(
            "run `{}` to download {} (about {} MB); cloud models work without a download",
            self.install_command,
            self.model,
            self.size_mb()
        )
    }
}

impl std::fmt::Display for NoLocalModelInstalled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} No verified local model is ready (a model file needs its tokenizer and \
             license verified before it can answer), and Susi does not download one by itself. \
             To answer locally, run `{}` to download {} (about {} MB). \
             Cloud models work without a download.",
            Self::TAG,
            self.install_command,
            self.model,
            self.size_mb()
        )
    }
}

impl std::error::Error for NoLocalModelInstalled {}

impl From<NoLocalModelInstalled> for crate::susi_error::EaiError {
    fn from(missing: NoLocalModelInstalled) -> Self {
        Self::inference(missing.to_string())
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
