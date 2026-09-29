//! End-to-end zero-config setup plan (VC-201-069).
//!
//! Detect hardware, engines, keys, and agents; propose one plan with consents
//! batched; apply idempotently; print a summary. Safe to rerun.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsentKind {
    NetworkEgress,
    CloudApiKey,
    Telemetry,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupStep {
    pub id: String,
    pub description: String,
    pub requires_consent: Option<ConsentKind>,
    pub applied: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupPlan {
    pub hardware: String,
    pub engines: Vec<String>,
    pub keys_present: Vec<String>,
    pub agents: Vec<String>,
    pub steps: Vec<SetupStep>,
    pub consents_granted: Vec<ConsentKind>,
}

impl SetupPlan {
    /// Detect current host facts into a plan (no mutation).
    #[must_use]
    pub fn detect(home: &Path) -> Self {
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let hardware = format!("cpus={cpus}");
        let mut engines = Vec::new();
        if home.join("bin").join("susi").exists() || which("llama-server") {
            engines.push("local-inference".into());
        }
        engines.push("susi-native".into());
        let mut keys_present = Vec::new();
        for name in ["OPENAI_API_KEY", "ANTHROPIC_API_KEY", "OPENROUTER_API_KEY"] {
            if std::env::var(name).ok().filter(|v| !v.is_empty()).is_some() {
                keys_present.push(name.to_string());
            }
        }
        let agents_dir = home.join("execution-agents").join("ready.json");
        let agents = std::fs::read_to_string(&agents_dir)
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|v| {
                v.as_array().map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
            })
            .unwrap_or_default();
        let steps = vec![
            SetupStep {
                id: "dirs".into(),
                description: "ensure config/data directories".into(),
                requires_consent: None,
                applied: false,
            },
            SetupStep {
                id: "default_config".into(),
                description: "seed config.json from bundled defaults if missing".into(),
                requires_consent: None,
                applied: false,
            },
            SetupStep {
                id: "cloud_opt_in".into(),
                description: "enable cloud providers when keys present".into(),
                requires_consent: Some(ConsentKind::CloudApiKey),
                applied: false,
            },
        ];
        Self {
            hardware,
            engines,
            keys_present,
            agents,
            steps,
            consents_granted: Vec::new(),
        }
    }

    pub fn grant_consent(&mut self, kind: ConsentKind) {
        if !self.consents_granted.contains(&kind) {
            self.consents_granted.push(kind);
        }
    }

    /// Apply pending steps whose consents (if any) are granted. Idempotent.
    pub fn apply(&mut self, home: &Path) -> Result<Vec<String>, String> {
        std::fs::create_dir_all(home).map_err(|e| e.to_string())?;
        let mut summary = Vec::new();
        for step in &mut self.steps {
            if step.applied {
                summary.push(format!("skip {}: already applied", step.id));
                continue;
            }
            if let Some(need) = &step.requires_consent {
                if !self.consents_granted.contains(need) {
                    summary.push(format!("defer {}: needs consent {need:?}", step.id));
                    continue;
                }
            }
            match step.id.as_str() {
                "dirs" => {
                    for sub in ["config", "data", "logs"] {
                        std::fs::create_dir_all(home.join(sub)).map_err(|e| e.to_string())?;
                    }
                }
                "default_config" => {
                    let cfg = home.join("config").join("config.json");
                    if !cfg.exists() {
                        std::fs::write(&cfg, "{}").map_err(|e| e.to_string())?;
                    }
                }
                "cloud_opt_in" => {
                    // Record opt-in marker only; no secrets written.
                    let marker = home.join("config").join("cloud_opt_in");
                    std::fs::write(&marker, "1").map_err(|e| e.to_string())?;
                }
                other => return Err(format!("unknown step {other}")),
            }
            step.applied = true;
            summary.push(format!("applied {}", step.id));
        }
        Ok(summary)
    }
}

fn which(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|dir| dir.join(bin).is_file()))
        .unwrap_or(false)
}
