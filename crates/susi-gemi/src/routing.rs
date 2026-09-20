//! Latency-aware inference routing: escalate slow local models to cloud,
//! remember sticky preferences, and ask once when multiple clouds are live.

use serde::{Deserialize, Serialize};
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::time::Duration;

use susi_sandbox::manager::{InferenceRoutingConfig, SusiConfig};

/// Sticky user choice under `~/.susi/routing_preference.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RoutingPreference {
    /// Optional override: `local_only` | `cloud_first` | `auto` | `ask`
    pub policy_override: Option<String>,
    /// Provider name (or substring) to prefer, e.g. `googlegemini-gemini-2.0-flash`
    pub preferred_cloud: Option<String>,
    /// When set in the future, force local until then (escape hatch).
    pub force_local_until_unix: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LocalInferenceStats {
    pub ema_tokens_per_sec: f32,
    pub last_latency_ms: u64,
    pub samples: u64,
}

#[derive(Debug, Clone)]
pub struct CloudEscalation {
    pub provider: String,
    pub reason: String,
}

pub struct InferenceRouter;

impl InferenceRouter {
    fn preference_path() -> PathBuf {
        susi_paths::SusiDirs::config_dir().join("routing_preference.json")
    }

    fn stats_path() -> PathBuf {
        susi_paths::SusiDirs::config_dir().join("local_inference_stats.json")
    }

    pub fn load_preference() -> RoutingPreference {
        let path = Self::preference_path();
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save_preference(pref: &RoutingPreference) {
        let path = Self::preference_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(body) = serde_json::to_string_pretty(pref) {
            let _ = std::fs::write(path, body);
        }
    }

    pub fn load_stats() -> LocalInferenceStats {
        let path = Self::stats_path();
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn record_local_sample(latency: Duration, output_chars: usize) {
        let latency_ms = latency.as_millis() as u64;
        // Rough token estimate (~4 chars/token) — gate only, not billing.
        let approx_tokens = (output_chars / 4).max(1) as f32;
        let secs = latency.as_secs_f32().max(0.001);
        let tps = approx_tokens / secs;

        let mut stats = Self::load_stats();
        if stats.samples == 0 {
            stats.ema_tokens_per_sec = tps;
        } else {
            // EMA so one outlier doesn't permanently lock cloud-on or cloud-off.
            stats.ema_tokens_per_sec = stats.ema_tokens_per_sec * 0.7 + tps * 0.3;
        }
        stats.last_latency_ms = latency_ms;
        stats.samples = stats.samples.saturating_add(1);

        let path = Self::stats_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(body) = serde_json::to_string_pretty(&stats) {
            let _ = std::fs::write(path, body);
        }
    }

    pub fn is_interactive() -> bool {
        io::stdin().is_terminal() && io::stdout().is_terminal()
    }

    pub fn is_cloud_provider_name(name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        lower.contains("openai")
            || lower.contains("anthropic")
            || lower.contains("gemini")
            || lower.contains("googlegemini")
            || (lower.contains("google") && lower.contains("gemini"))
    }

    pub fn list_cloud_providers(names: &[String]) -> Vec<String> {
        let mut clouds: Vec<String> = names
            .iter()
            .filter(|n| Self::is_cloud_provider_name(n))
            .cloned()
            .collect();
        clouds.sort();
        clouds
    }

    fn effective_policy(cfg: &InferenceRoutingConfig, pref: &RoutingPreference) -> String {
        if let Some(until) = pref.force_local_until_unix {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            if now < until {
                return "local_only".to_string();
            }
        }
        pref.policy_override
            .clone()
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| cfg.policy.clone())
    }

    fn local_is_slow(cfg: &InferenceRoutingConfig, stats: &LocalInferenceStats) -> bool {
        if stats.samples == 0 {
            return false;
        }
        stats.ema_tokens_per_sec < cfg.local_min_tokens_per_sec
            || stats.last_latency_ms > cfg.local_max_latency_ms
    }

    fn cpu_only_host() -> bool {
        let profile = crate::hardware::HardwareProfiler::get_profile();
        profile.gpu_vram_gb == 0
            && !profile.gpu_info.to_ascii_lowercase().contains("cuda")
            && !profile.gpu_info.to_ascii_lowercase().contains("metal")
    }

    /// Decide whether to escalate to cloud before local inference.
    /// Returns `None` when local path should proceed as usual.
    pub fn maybe_escalate_to_cloud(available_providers: &[String]) -> Option<CloudEscalation> {
        let cfg = SusiConfig::load_global()
            .unwrap_or_default()
            .inference_routing();
        let pref = Self::load_preference();
        let policy = Self::effective_policy(&cfg, &pref);
        let clouds = Self::list_cloud_providers(available_providers);

        if clouds.is_empty() {
            return None;
        }

        match policy.as_str() {
            "local_only" => return None,
            "cloud_first" => {
                let provider = Self::pick_cloud(&clouds, &pref, &cfg);
                return Some(CloudEscalation {
                    provider,
                    reason: "cloud_first policy".to_string(),
                });
            }
            "ask" => {
                let provider = Self::pick_cloud(&clouds, &pref, &cfg);
                return Some(CloudEscalation {
                    provider,
                    reason: "ask policy — using cloud".to_string(),
                });
            }
            _ => {} // auto
        }

        let stats = Self::load_stats();
        let slow = Self::local_is_slow(&cfg, &stats);
        let cpu_only = cfg.prefer_cloud_when_cpu_only && Self::cpu_only_host();

        if !slow && !cpu_only {
            return None;
        }

        let reason = if slow {
            format!(
                "local ~{:.1} tok/s (min {:.1}) / last {}ms",
                stats.ema_tokens_per_sec, cfg.local_min_tokens_per_sec, stats.last_latency_ms
            )
        } else {
            "CPU-only host — local GGUF typically too slow".to_string()
        };

        let provider = Self::pick_cloud(&clouds, &pref, &cfg);
        Some(CloudEscalation { provider, reason })
    }

    fn pick_cloud(
        clouds: &[String],
        pref: &RoutingPreference,
        cfg: &InferenceRoutingConfig,
    ) -> String {
        if let Some(preferred) = pref.preferred_cloud.as_ref() {
            let pref_l = preferred.to_ascii_lowercase();
            if let Some(hit) = clouds.iter().find(|c| {
                c.to_ascii_lowercase().contains(&pref_l) || pref_l.contains(&c.to_ascii_lowercase())
            }) {
                return hit.clone();
            }
            if clouds.iter().any(|c| c == preferred) {
                return preferred.clone();
            }
        }

        if cfg.ask_when_multiple_clouds
            && clouds.len() > 1
            && pref.preferred_cloud.is_none()
            && Self::is_interactive()
        {
            if let Some(chosen) = Self::prompt_cloud_choice(clouds) {
                let mut next = pref.clone();
                next.preferred_cloud = Some(chosen.clone());
                Self::save_preference(&next);
                return chosen;
            }
        }

        // Non-interactive / single cloud / declined prompt: prefer openai→anthropic→gemini order.
        let mut ranked = clouds.to_vec();
        ranked.sort_by_key(|n| Self::cloud_rank(n));
        ranked
            .into_iter()
            .next()
            .unwrap_or_else(|| clouds[0].clone())
    }

    fn cloud_rank(name: &str) -> u8 {
        let lower = name.to_ascii_lowercase();
        if lower.contains("openai") {
            0
        } else if lower.contains("anthropic") {
            1
        } else if lower.contains("gemini") || lower.contains("google") {
            2
        } else {
            9
        }
    }

    fn prompt_cloud_choice(clouds: &[String]) -> Option<String> {
        let _ = writeln!(
            io::stderr(),
            "\n[SUSI ROUTING] Local inference is constrained. Cloud backends available:"
        );
        for (i, name) in clouds.iter().enumerate() {
            let _ = writeln!(io::stderr(), "  [{}] {}", i + 1, name);
        }
        let _ = writeln!(
            io::stderr(),
            "  [Enter] = {} (default)   [0] = keep trying local",
            clouds[0]
        );
        let _ = write!(io::stderr(), "Select cloud backend: ");
        let _ = io::stderr().flush();

        let mut line = String::new();
        if io::stdin().read_line(&mut line).is_err() {
            return Some(clouds[0].clone());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Some(clouds[0].clone());
        }
        if trimmed == "0" {
            let mut pref = Self::load_preference();
            pref.policy_override = Some("local_only".to_string());
            Self::save_preference(&pref);
            let _ = writeln!(
                io::stderr(),
                "[SUSI ROUTING] Sticky preference set to local_only. Clear ~/.susi/routing_preference.json to reset."
            );
            return None;
        }
        if let Ok(idx) = trimmed.parse::<usize>() {
            if idx >= 1 && idx <= clouds.len() {
                return Some(clouds[idx - 1].clone());
            }
        }
        // Substring match
        let lower = trimmed.to_ascii_lowercase();
        clouds
            .iter()
            .find(|c| c.to_ascii_lowercase().contains(&lower))
            .cloned()
            .or_else(|| Some(clouds[0].clone()))
    }

    /// Announce a glass-box escalation line (once per decision).
    pub fn announce(escalation: &CloudEscalation) {
        let _ = writeln!(
            io::stderr(),
            "[SUSI ROUTING] {} — using cloud `{}`. Say sticky local via routing_preference or pick [0] when prompted.",
            escalation.reason, escalation.provider
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloud_name_detection() {
        assert!(InferenceRouter::is_cloud_provider_name(
            "googlegemini-gemini-2.0-flash"
        ));
        assert!(InferenceRouter::is_cloud_provider_name(
            "openai-gpt-4o-mini"
        ));
        assert!(InferenceRouter::is_cloud_provider_name(
            "anthropic-claude-3-5-haiku-20241022"
        ));
        assert!(!InferenceRouter::is_cloud_provider_name("ollama-llama3"));
        assert!(!InferenceRouter::is_cloud_provider_name("vllm-mistral"));
    }

    #[test]
    fn list_clouds_filters_locals() {
        let names = vec![
            "ollama-llama3".into(),
            "openai-gpt-4o-mini".into(),
            "googlegemini-gemini-2.0-flash".into(),
        ];
        let clouds = InferenceRouter::list_cloud_providers(&names);
        assert_eq!(clouds.len(), 2);
        assert!(clouds
            .iter()
            .all(|c| InferenceRouter::is_cloud_provider_name(c)));
    }

    #[test]
    fn local_slow_gate() {
        let cfg = InferenceRoutingConfig {
            local_min_tokens_per_sec: 8.0,
            local_max_latency_ms: 15_000,
            ..Default::default()
        };
        let slow = LocalInferenceStats {
            ema_tokens_per_sec: 2.0,
            last_latency_ms: 1_000,
            samples: 3,
        };
        assert!(InferenceRouter::local_is_slow(&cfg, &slow));
        let fast = LocalInferenceStats {
            ema_tokens_per_sec: 40.0,
            last_latency_ms: 200,
            samples: 3,
        };
        assert!(!InferenceRouter::local_is_slow(&cfg, &fast));
        let cold = LocalInferenceStats::default();
        assert!(!InferenceRouter::local_is_slow(&cfg, &cold));
    }
}
