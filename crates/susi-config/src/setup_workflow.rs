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
        if home.join("bin").join(executable_name("susi")).is_file() {
            available_engines.push("susi".into());
        }
        if home
            .join("bin")
            .join(executable_name("susi-native"))
            .is_file()
        {
            available_engines.push("susi-native".into());
        }
        let mut engines = Vec::new();
        if which("llama-server") {
            engines.push("local-inference".into());
        }
        engines.push("susi-native".into());
        let mut keys_present = Vec::new();
        let mut registered = crate::cloud_env::load_cloud_env_from(home);
        if registered.is_empty() {
            registered = std::fs::read_to_string(home.join("cloud.env"))
                .map(|text| crate::cloud_env::parse_env_file(&text))
                .unwrap_or_default();
        }
        // Core's compiled aliases, not the active pack: `detect` reads
        // `home` and must not touch (or rewrite) the host's pack state.
        let vendors = crate::extensions::compiled_cloud_vendors();
        let names = vendors.iter().flat_map(|vendor| {
            std::iter::once(&vendor.api_key_env).chain(vendor.api_key_env_alts.iter())
        });
        for name in names {
            let in_env = std::env::var(name)
                .ok()
                .is_some_and(|v| !v.trim().is_empty());
            let in_vault_or_cloud = registered
                .iter()
                .any(|(key, value)| key == name && !value.trim().is_empty());
            if (in_env || in_vault_or_cloud) && !keys_present.iter().any(|k| k == name) {
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
                    let cfg = home.join("config.json");
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
        let mut command = Command::new(&executable);
        command
            .env_clear()
            .env("PATH", path)
            .env("HOME", home)
            .env("SUSI_HOME", home)
            .env("SUSI_SETUP_PROBE", "1")
            .arg("setup completion probe")
            .current_dir(home);
        let output = susi_paths::test_env::inherit_registry_isolation(&mut command)
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
        ("susi", home.join("bin").join(executable_name("susi"))),
        (
            "susi-native",
            home.join("bin").join(executable_name("susi-native")),
        ),
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
        .map(|p| {
            std::env::split_paths(&p)
                .any(|dir| dir.is_absolute() && dir.join(executable_name(bin)).is_file())
        })
        .unwrap_or(false)
}

fn executable_name(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn unique_temp_dir(label: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("susi-setup-wf-{label}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn detect_and_apply_seed_dirs_bundled_config_tools_policy_and_handle_cloud_consent_idempotently(
    ) {
        let home = unique_temp_dir("detect-apply");
        std::fs::write(home.join("cloud.env"), "OPENAI_API_KEY=sk-test-12345678\n")
            .expect("write cloud.env");
        let agents_dir = home.join("execution-agents");
        std::fs::create_dir_all(&agents_dir).expect("create execution-agents dir");
        std::fs::write(agents_dir.join("ready.json"), r#"["codex"]"#).expect("write ready.json");

        let mut plan = SetupPlan::detect(&home);
        assert!(plan.keys_present.iter().any(|k| k == "OPENAI_API_KEY"));
        assert_eq!(plan.agents, vec!["codex".to_string()]);

        let first_summary = plan.apply(&home).expect("first apply");
        assert!(home.join("config").is_dir());
        assert!(home.join("data").is_dir());
        assert!(home.join("logs").is_dir());
        assert!(home.join("config.json").is_file());
        assert!(home.join("config").join("tools.json").is_file());
        assert!(home.join("config").join("policy.json").is_file());
        assert!(!home.join("config").join("cloud_opt_in").exists());
        assert!(first_summary
            .iter()
            .any(|s| s.contains("defer cloud_opt_in")));

        plan.grant_consent(ConsentKind::CloudApiKey);
        plan.grant_consent(ConsentKind::CloudApiKey);
        assert_eq!(plan.consents_granted, vec![ConsentKind::CloudApiKey]);

        let second_summary = plan.apply(&home).expect("second apply with consent");
        assert!(home.join("config").join("cloud_opt_in").is_file());
        assert!(second_summary.iter().any(|s| s == "applied cloud_opt_in"));

        let third_summary = plan.apply(&home).expect("third apply idempotent");
        assert!(third_summary
            .iter()
            .all(|s| s.starts_with("skip ") && s.ends_with("already applied")));

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn completion_check_blocks_when_runtime_or_cloud_key_prerequisite_is_missing_and_validate_catches_bad_plan(
    ) {
        let home = unique_temp_dir("completion-blocked");
        let mut plan = SetupPlan::detect(&home);
        plan.available_engines.clear();
        plan.keys_present.clear();
        plan.grant_consent(ConsentKind::CloudApiKey);

        let completion = plan
            .completion_check(&home)
            .expect("completion_check should succeed");
        assert!(!completion.ready());
        assert_eq!(completion.mission.status, MissionStatus::Blocked);
        assert_eq!(
            completion.missing_prerequisites,
            vec![
                "an available local runtime".to_string(),
                "a cloud API key for cloud opt-in".to_string(),
            ]
        );
        let cloud_step = plan
            .steps
            .iter()
            .find(|s| s.id == "cloud_opt_in")
            .expect("cloud_opt_in step");
        assert!(cloud_step.blocked);
        assert!(!cloud_step.applied);

        let mut empty_tools_plan = plan.clone();
        empty_tools_plan.tools.clear();
        let tools_err = empty_tools_plan
            .apply(&home)
            .expect_err("empty tools must fail validation");
        assert!(tools_err.contains("setup plan must include tools and policy defaults"));

        let mut dup_step_plan = plan.clone();
        dup_step_plan.steps.push(SetupStep {
            id: "dirs".into(),
            description: "dup".into(),
            requires_consent: None,
            applied: false,
            blocked: false,
        });
        let dup_err = dup_step_plan
            .apply(&home)
            .expect_err("duplicate step id must fail validation");
        assert!(dup_err.contains("duplicate or empty step ids"));

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn truncate_output_multibyte_boundaries_and_json_validation_helpers() {
        // 1. truncate_output leaves strings <= 4096 bytes unchanged and truncates on a UTF-8 char boundary.
        let exact = "a".repeat(4096);
        assert_eq!(truncate_output(&exact), exact);

        // Build a string where byte 4096 lands inside a 4-byte emoji ('🦀').
        let prefix = "x".repeat(4094);
        let over = format!("{prefix}🦀tail");
        assert!(over.len() > 4096);
        let truncated = truncate_output(&over);
        assert!(truncated.ends_with('…'));
        let without_ellipsis = truncated.strip_suffix('…').expect("trailing ellipsis");
        assert_eq!(without_ellipsis.len(), 4094);

        // 2. validate_json_object accepts JSON objects and rejects arrays, primitives, and malformed JSON.
        assert!(validate_json_object("{}", "tools").is_ok());
        assert!(validate_json_object(r#"{"k":1}"#, "tools").is_ok());
        let arr_err = validate_json_object("[1, 2]", "tools").expect_err("array is not object");
        assert!(arr_err.contains("tools must be a JSON object"));
        let num_err = validate_json_object("42", "policy").expect_err("primitive is not object");
        assert!(num_err.contains("policy must be a JSON object"));
        let bad_err = validate_json_object("{bad", "config").expect_err("malformed JSON");
        assert!(bad_err.contains("config are not valid JSON"));

        // 3. write_or_validate_json preserves an existing valid JSON file and fails on corrupt JSON.
        let dir = unique_temp_dir("json-helpers");
        let target = dir.join("custom.json");
        write_or_validate_json(&target, &serde_json::json!({"seeded": true}), "custom")
            .expect("write initial json");
        std::fs::write(&target, r#"{"user_preserved": 99}"#).expect("write user json");
        write_or_validate_json(&target, &serde_json::json!({"seeded": true}), "custom")
            .expect("validate existing valid json without overwriting");
        let preserved = std::fs::read_to_string(&target).expect("read preserved json");
        assert!(preserved.contains("user_preserved"));

        std::fs::write(&target, b"{corrupt").expect("write corrupt json");
        let corrupt_err =
            write_or_validate_json(&target, &serde_json::json!({}), "custom").unwrap_err();
        assert!(corrupt_err.contains("custom is not valid JSON"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cloud_opt_in_blocks_without_key_then_unblocks_when_key_supplied_and_rejects_unknown_step() {
        let home = unique_temp_dir("cloud-unblock-and-unknown");
        let mut plan = SetupPlan::detect(&home);
        plan.keys_present.clear();
        plan.grant_consent(ConsentKind::CloudApiKey);

        let first = plan.apply(&home).expect("apply without cloud key");
        assert!(first
            .iter()
            .any(|s| s == "applied cloud_opt_in: missing prerequisite API key"));
        let cloud_step = plan
            .steps
            .iter()
            .find(|s| s.id == "cloud_opt_in")
            .expect("cloud_opt_in step");
        assert!(cloud_step.blocked);
        assert!(!cloud_step.applied);
        assert!(!home.join("config").join("cloud_opt_in").exists());

        let second = plan
            .apply(&home)
            .expect("re-apply while cloud key still absent");
        assert!(second
            .iter()
            .any(|s| s == "skip cloud_opt_in: missing prerequisite"));

        plan.keys_present.push("OPENAI_API_KEY".into());
        let third = plan.apply(&home).expect("apply after cloud key supplied");
        assert!(third.iter().any(|s| s == "applied cloud_opt_in"));
        let unblocked = plan
            .steps
            .iter()
            .find(|s| s.id == "cloud_opt_in")
            .expect("cloud_opt_in step");
        assert!(!unblocked.blocked);
        assert!(unblocked.applied);
        assert!(home.join("config").join("cloud_opt_in").is_file());

        plan.steps.push(SetupStep {
            id: "mystery_step".into(),
            description: "unknown step".into(),
            requires_consent: None,
            applied: false,
            blocked: false,
        });
        let unknown_err = plan.apply(&home).expect_err("unknown step id must error");
        assert!(unknown_err.contains("unknown step mystery_step"));

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn setup_step_serde_roundtrip_and_clone_preserve_all_fields() {
        let step = SetupStep {
            id: "cloud_opt_in".into(),
            description: "enable cloud".into(),
            requires_consent: Some(ConsentKind::CloudApiKey),
            applied: true,
            blocked: false,
        };
        let json = serde_json::to_string(&step).expect("serialize SetupStep");
        let decoded: SetupStep = serde_json::from_str(&json).expect("deserialize SetupStep");
        assert_eq!(decoded, step.clone());
    }
}
