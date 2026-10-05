//! End-to-end zero-config setup plan (VC-201-069).
//!
//! Detect hardware, engines, keys, and agents; propose one plan with consents
//! batched; apply idempotently; print a summary. Safe to rerun.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

const BUNDLED_DEFAULTS: &str = include_str!("../../../config/config.default.json");
const DEFAULT_TOOLS: &[&str] = &["filesystem.read", "mission.status"];
const DEFAULT_POLICY: &[&str] = &["deny.destructive", "deny.secret-disclosure"];

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
    pub blocked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MissionStatus {
    Passed,
    Blocked,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissionCheck {
    pub status: MissionStatus,
    pub engine: Option<String>,
    pub output: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupCompletion {
    pub mission: MissionCheck,
    pub missing_prerequisites: Vec<String>,
    pub summary: Vec<String>,
}

impl SetupCompletion {
    #[must_use]
    pub fn ready(&self) -> bool {
        self.missing_prerequisites.is_empty()
            && matches!(self.mission.status, MissionStatus::Passed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupPlan {
    pub hardware: String,
    /// Candidate engines the setup wizard can explain to the operator.
    pub engines: Vec<String>,
    /// Engines actually present on this host. `engines` is intentionally a
    /// candidate list for the UI; completion checks must use this field.
    pub available_engines: Vec<String>,
    pub keys_present: Vec<String>,
    pub agents: Vec<String>,
    pub tools: Vec<String>,
    pub policy: Vec<String>,
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
        let mut available_engines = Vec::new();
        if home.join("bin").join("susi").is_file() {
            available_engines.push("susi".into());
        }
        if home.join("bin").join("susi-native").is_file() || which("susi-native") {
            available_engines.push("susi-native".into());
        }
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
                blocked: false,
            },
            SetupStep {
                id: "default_config".into(),
                description: "seed config.json from bundled defaults if missing".into(),
                requires_consent: None,
                applied: false,
                blocked: false,
            },
            SetupStep {
                id: "tools".into(),
                description: "enable the built-in read-only setup tools".into(),
                requires_consent: None,
                applied: false,
                blocked: false,
            },
            SetupStep {
                id: "policy".into(),
                description: "seed the safe default mission policy".into(),
                requires_consent: None,
                applied: false,
                blocked: false,
            },
            SetupStep {
                id: "cloud_opt_in".into(),
                description: "enable cloud providers when keys present".into(),
                requires_consent: Some(ConsentKind::CloudApiKey),
                applied: false,
                blocked: false,
            },
        ];
        Self {
            hardware,
            engines,
            available_engines,
            keys_present,
            agents,
            tools: DEFAULT_TOOLS.iter().map(|v| (*v).to_string()).collect(),
            policy: DEFAULT_POLICY.iter().map(|v| (*v).to_string()).collect(),
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
        self.validate()?;
        std::fs::create_dir_all(home).map_err(|e| e.to_string())?;
        let mut summary = Vec::new();
        for step in &mut self.steps {
            if step.applied {
                summary.push(format!("skip {}: already applied", step.id));
                continue;
            }
            if step.blocked {
                if step.id == "cloud_opt_in" && !self.keys_present.is_empty() {
                    step.blocked = false;
                } else {
                    summary.push(format!("skip {}: missing prerequisite", step.id));
                    continue;
                }
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
                        validate_json_object(BUNDLED_DEFAULTS, "bundled defaults")?;
                        std::fs::write(&cfg, BUNDLED_DEFAULTS).map_err(|e| e.to_string())?;
                    } else {
                        let text = std::fs::read_to_string(&cfg)
                            .map_err(|e| format!("read config.json: {e}"))?;
                        validate_json_object(&text, "config.json")?;
                    }
                }
                "tools" => {
                    let path = home.join("config").join("tools.json");
                    write_or_validate_json(&path, &self.tools, "tools.json")?;
                }
                "policy" => {
                    let path = home.join("config").join("policy.json");
                    write_or_validate_json(&path, &self.policy, "policy.json")?;
                }
                "cloud_opt_in" => {
                    if self.keys_present.is_empty() {
                        // Consent alone is not a credential. Remember the
                        // blocked state so reruns stay idempotent while a
                        // later invocation can retry after a key is supplied.
                        step.blocked = true;
                        summary.push("applied cloud_opt_in: missing prerequisite API key".into());
                        continue;
                    }
                    // Record opt-in marker only; no secrets are written.
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

    /// Run the setup completion check after applying the plan. The mission is
    /// a real invocation of the first-party CLI from the isolated setup home;
    /// no inherited credentials or instance selectors are passed to it.
    pub fn completion_check(&mut self, home: &Path) -> Result<SetupCompletion, String> {
        let summary = self.apply(home)?;
        let mut missing_prerequisites = Vec::new();
        if self.available_engines.is_empty() {
            missing_prerequisites.push("an available local runtime".into());
        }
        if self.consents_granted.contains(&ConsentKind::CloudApiKey) && self.keys_present.is_empty()
        {
            missing_prerequisites.push("a cloud API key for cloud opt-in".into());
        }

        let mission = if !missing_prerequisites.is_empty() {
            MissionCheck {
                status: MissionStatus::Blocked,
                engine: None,
                output: format!(
                    "mission not run; missing prerequisite(s): {}",
                    missing_prerequisites.join(", ")
                ),
            }
        } else {
            self.run_mission(home)?
        };
        Ok(SetupCompletion {
            mission,
            missing_prerequisites,
            summary,
        })
    }

    fn run_mission(&self, home: &Path) -> Result<MissionCheck, String> {
        let Some((engine, executable)) = runtime_executable(home) else {
            return Ok(MissionCheck {
                status: MissionStatus::Blocked,
                engine: None,
                output: "mission not run; no first-party runtime executable found".into(),
            });
        };
        let path = std::env::var_os("PATH").unwrap_or_default();
        let output = Command::new(&executable)
            .env_clear()
            .env("PATH", path)
            .env("HOME", home)
            .env("SUSI_HOME", home)
            .env("SUSI_SETUP_PROBE", "1")
            .arg("setup completion probe")
            .current_dir(home)
            .output()
            .map_err(|e| format!("run setup mission with {engine}: {e}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let output_text = truncate_output(if stdout.trim().is_empty() {
            stderr.as_ref()
        } else {
            stdout.as_ref()
        });
        let status = if output.status.success() {
            MissionStatus::Passed
        } else {
            MissionStatus::Failed
        };
        Ok(MissionCheck {
            status,
            engine: Some(engine),
            output: output_text,
        })
    }

    fn validate(&self) -> Result<(), String> {
        let mut ids = BTreeSet::new();
        for step in &self.steps {
            if step.id.trim().is_empty() || !ids.insert(step.id.as_str()) {
                return Err("setup plan contains duplicate or empty step ids".into());
            }
        }
        for required in ["dirs", "default_config", "tools", "policy", "cloud_opt_in"] {
            if !ids.contains(required) {
                return Err(format!("setup plan is missing required step {required}"));
            }
        }
        if self.tools.is_empty() || self.policy.is_empty() {
            return Err("setup plan must include tools and policy defaults".into());
        }
        validate_json_object(BUNDLED_DEFAULTS, "bundled defaults")
    }
}

fn runtime_executable(home: &Path) -> Option<(String, PathBuf)> {
    for (name, path) in [
        ("susi", home.join("bin").join("susi")),
        ("susi-native", home.join("bin").join("susi-native")),
    ] {
        if path.is_file() {
            return Some((name.into(), path));
        }
    }
    None
}

fn validate_json_object(text: &str, label: &str) -> Result<(), String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("{label} are not valid JSON: {e}"))?;
    if !value.is_object() {
        return Err(format!("{label} must be a JSON object"));
    }
    Ok(())
}

fn validate_json_file(path: &Path, label: &str) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("read {label}: {e}"))?;
    serde_json::from_str::<serde_json::Value>(&text)
        .map_err(|e| format!("{label} is not valid JSON: {e}"))?;
    Ok(())
}

fn write_or_validate_json<T: Serialize>(path: &Path, value: &T, label: &str) -> Result<(), String> {
    if path.exists() {
        return validate_json_file(path, label);
    }
    let text = serde_json::to_string_pretty(value).map_err(|e| format!("encode {label}: {e}"))?;
    std::fs::write(path, text).map_err(|e| format!("write {label}: {e}"))
}

fn truncate_output(value: &str) -> String {
    const LIMIT: usize = 4096;
    if value.len() <= LIMIT {
        return value.to_string();
    }
    let mut end = LIMIT;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

fn which(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|dir| dir.join(bin).is_file()))
        .unwrap_or(false)
}
