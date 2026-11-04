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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_provisioning_outcome_terminal_reason_and_summary_cover_all_five_variants() {
        let active = ModelProvisioningOutcome::Active;
        assert_eq!(active.terminal_reason(), None);
        assert_eq!(
            active.summary(),
            "The starter model install is already running"
        );

        let ready = ModelProvisioningOutcome::Ready("qwen3-0.6b".to_string());
        assert_eq!(ready.terminal_reason(), None);
        assert_eq!(
            ready.summary(),
            "Using verified cached local model qwen3-0.6b"
        );

        let installed = ModelProvisioningOutcome::Installed("qwen3-0.6b".to_string());
        assert_eq!(installed.terminal_reason(), None);
        assert_eq!(
            installed.summary(),
            "Installed and verified local model qwen3-0.6b"
        );

        let offline = ModelProvisioningOutcome::OfflineNoModel;
        assert_eq!(
            offline.terminal_reason(),
            Some(
                "No admitted local model is cached, and network policy blocks the starter download"
            )
        );
        assert_eq!(offline.summary(), offline.terminal_reason().unwrap());

        let too_small = ModelProvisioningOutcome::TooSmall;
        assert_eq!(
            too_small.terminal_reason(),
            Some("Available RAM or disk cannot safely accommodate the small starter")
        );
        assert_eq!(too_small.summary(), too_small.terminal_reason().unwrap());
    }

    #[test]
    fn no_local_model_installed_starter_formats_tag_size_mb_install_hint_and_converts_to_eai_error()
    {
        let starter = NoLocalModelInstalled::starter();
        assert!(!starter.model.is_empty());
        assert!(starter.size_bytes > 0);
        assert_eq!(
            starter.install_command,
            crate::zc_first_model::STARTER_INSTALL_COMMAND
        );
        assert_eq!(
            starter.size_mb(),
            (starter.size_bytes + 500_000) / 1_000_000
        );

        // Rounding and saturation boundaries
        let sample = |size_bytes| NoLocalModelInstalled {
            model: "test-model".to_string(),
            size_bytes,
            install_command: "susi models install-starter",
        };
        assert_eq!(sample(0).size_mb(), 0);
        assert_eq!(sample(499_999).size_mb(), 0);
        assert_eq!(sample(500_000).size_mb(), 1);
        assert_eq!(sample(1_499_999).size_mb(), 1);
        assert_eq!(sample(1_500_000).size_mb(), 2);
        assert_eq!(sample(u64::MAX).size_mb(), u64::MAX / 1_000_000);

        let hint = starter.install_hint();
        assert!(hint.contains(starter.install_command));
        assert!(hint.contains(&starter.model));
        assert!(hint.contains(&format!("about {} MB", starter.size_mb())));
        assert!(hint.contains("cloud models work without a download"));

        let display = starter.to_string();
        assert!(display.starts_with(NoLocalModelInstalled::TAG));
        assert!(display.contains("does not download one by itself"));
        assert!(display.contains(starter.install_command));
        assert!(display.contains(&starter.model));
        assert!(display.contains("Cloud models work without a download."));

        let eai: crate::susi_error::EaiError = starter.clone().into();
        let eai_msg = eai.to_string();
        assert!(eai_msg.contains(NoLocalModelInstalled::TAG));
        assert!(eai_msg.contains(starter.install_command));
    }

    #[test]
    fn model_provenance_and_agent_report_roundtrip_serde() {
        let prov = ModelProvenance {
            source_url: "https://huggingface.co/Qwen/Qwen3-0.6B-GGUF".to_string(),
            timestamp: 1_730_000_000,
            original_checksum: Some("sha256:deadbeef".to_string()),
        };
        let prov_json = serde_json::to_string(&prov).expect("serialize provenance");
        let prov_rt: ModelProvenance =
            serde_json::from_str(&prov_json).expect("deserialize provenance");
        assert_eq!(prov_rt.source_url, prov.source_url);
        assert_eq!(prov_rt.timestamp, prov.timestamp);
        assert_eq!(prov_rt.original_checksum, prov.original_checksum);

        let report = ModelAgentReport {
            active_step: 1,
            total_steps: 2,
            total_discovered_on_system: 3,
            network_status: "online".to_string(),
            download_agent_active: true,
            steps: vec![ModelAgentStepStatus {
                step: 1,
                model_label: "Starter".to_string(),
                hf_repo: "Qwen/Qwen3-0.6B-GGUF".to_string(),
                status: "downloading".to_string(),
                bytes_downloaded: 250_000_000,
                expected_bytes: 500_000_000,
                percentage: 50.0,
                path: "/tmp/model.gguf".to_string(),
            }],
        };
        let report_json = serde_json::to_string(&report).expect("serialize report");
        let report_rt: ModelAgentReport =
            serde_json::from_str(&report_json).expect("deserialize report");
        assert_eq!(report_rt.active_step, 1);
        assert_eq!(report_rt.total_steps, 2);
        assert_eq!(report_rt.total_discovered_on_system, 3);
        assert_eq!(report_rt.network_status, "online");
        assert!(report_rt.download_agent_active);
        assert_eq!(report_rt.steps.len(), 1);
        assert_eq!(report_rt.steps[0].model_label, "Starter");
        assert_eq!(report_rt.steps[0].percentage, 50.0);

        let bench = ModelBenchmarkResult {
            model_id: "qwen3-0.6b".to_string(),
            name: "Qwen3 0.6B".to_string(),
            is_local: true,
            latency_ms: 42,
            tokens_per_sec: 85.5,
            status: "ok".to_string(),
            memory_used_mb: 512.0,
            peak_memory_mb: 640.0,
        };
        let bench_json = serde_json::to_string(&bench).expect("serialize benchmark");
        let bench_rt: ModelBenchmarkResult =
            serde_json::from_str(&bench_json).expect("deserialize benchmark");
        assert_eq!(bench_rt.model_id, "qwen3-0.6b");
        assert!(bench_rt.is_local);
        assert_eq!(bench_rt.latency_ms, 42);
        assert_eq!(bench_rt.tokens_per_sec, 85.5);

        let ver = ModelVerificationResult {
            model_id: "qwen3-0.6b".to_string(),
            path: "/tmp/model.gguf".to_string(),
            file_size_bytes: 1024,
            file_size_formatted: "1.0 KB".to_string(),
            is_valid_gguf: true,
            magic_header: "GGUF".to_string(),
            test_inference_status: "NOT_RUN".to_string(),
            latency_ms: 0,
            checksum_known: true,
            checksum_verified: true,
        };
        assert!(ver.is_valid_gguf);
        assert!(ver.checksum_verified);
        assert_eq!(ver.test_inference_status, "NOT_RUN");
    }
}
