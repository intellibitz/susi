use crate::sandbox::manager::ModelLadderConfigStep;
use rayon::prelude::*;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

#[derive(Deserialize)]
struct HfModel {
    id: String,
    #[serde(rename = "cardData", default)]
    card_data: Option<HfCardData>,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Deserialize)]
struct HfCardData {
    #[serde(default)]
    base_model: Option<HfBaseModel>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum HfBaseModel {
    One(String),
    Many(Vec<String>),
}

impl HfBaseModel {
    fn first(self) -> Option<String> {
        match self {
            Self::One(model) if !model.trim().is_empty() => Some(model),
            Self::Many(models) => {
                let mut nonempty = models.into_iter().filter(|model| !model.trim().is_empty());
                let first = nonempty.next();
                if nonempty.next().is_some() {
                    None
                } else {
                    first
                }
            }
            _ => None,
        }
    }
}

fn declared_base_model(card_data: Option<HfCardData>, tags: &[String]) -> Option<String> {
    card_data
        .and_then(|card| card.base_model)
        .and_then(HfBaseModel::first)
        .or_else(|| {
            tags.iter().find_map(|tag| {
                let candidate = tag
                    .strip_prefix("base_model:quantized:")
                    .or_else(|| tag.strip_prefix("base_model:"))?;
                (!candidate.starts_with("quantized:") && !candidate.trim().is_empty())
                    .then(|| candidate.to_string())
            })
        })
}

#[derive(Deserialize)]
struct HfTreeEntry {
    path: String,
    size: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct DiscoveryCache {
    refreshed_at: u64,
    #[serde(skip)]
    attempted_at: u64,
    #[serde(skip)]
    refreshing: bool,
    steps: Vec<ModelLadderConfigStep>,
}
static CACHED_DYNAMIC_LADDER: OnceLock<Mutex<DiscoveryCache>> = OnceLock::new();

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn cache_path() -> std::path::PathBuf {
    crate::sandbox::xdg::SusiDirs::data_dir().join("model-discovery.json")
}

/// Fresh snapshots return immediately. Stale snapshots remain usable while one
/// worker refreshes them; outages never erase the last successful discovery.
pub fn discover_dynamic_ladder() -> Vec<ModelLadderConfigStep> {
    let cache = CACHED_DYNAMIC_LADDER.get_or_init(|| {
        Mutex::new(
            std::fs::read(cache_path())
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default(),
        )
    });
    let cfg = crate::sandbox::manager::SusiConfig::load_global().unwrap_or_default();
    let policy = cfg.model_lifecycle();
    let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
    let stale = now().saturating_sub(guard.refreshed_at) >= policy.discovery_refresh_secs.max(60);
    let retry_due = now().saturating_sub(guard.attempted_at) >= policy.discovery_retry_secs.max(1);
    if cfg!(test) || guard.refreshing || (!guard.steps.is_empty() && !stale) || !retry_due {
        return guard.steps.clone();
    }
    guard.refreshing = true;
    guard.attempted_at = now();
    let previous = guard.steps.clone();
    drop(guard);
    let refresh = move || {
        let steps = fetch_dynamic_ladder(&cfg);
        let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
        if !steps.is_empty() {
            guard.steps = steps;
            guard.refreshed_at = now();
            let path = cache_path();
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(bytes) = serde_json::to_vec(&*guard) {
                let temporary = path.with_extension("json.tmp");
                if std::fs::write(&temporary, bytes).is_ok() {
                    let _ = std::fs::rename(temporary, path);
                }
            }
        }
        guard.refreshing = false;
        guard.steps.clone()
    };
    if previous.is_empty() {
        std::thread::spawn(refresh).join().unwrap_or_default()
    } else {
        std::thread::spawn(refresh);
        previous
    }
}

/// Keep distinct useful sizes instead of many equivalent quantizations. The
/// hardware filter runs later so a busy machine doesn't poison discovery.
pub(crate) fn compact_ladder(
    mut steps: Vec<ModelLadderConfigStep>,
    limit: usize,
    ratio: f32,
) -> Vec<ModelLadderConfigStep> {
    steps.retain(|s| s.expected_bytes > 0 && s.min_ram_gb.is_finite() && s.min_ram_gb >= 0.0);
    steps.sort_by(|a, b| {
        a.expected_bytes
            .cmp(&b.expected_bytes)
            .then(a.hf_repo.cmp(&b.hf_repo))
            .then(a.hf_file.cmp(&b.hf_file))
    });
    let ratio = if ratio.is_finite() {
        ratio.max(1.05)
    } else {
        1.6
    };
    let mut result: Vec<ModelLadderConfigStep> = Vec::new();
    let mut names = std::collections::HashSet::new();
    for step in steps {
        if names.contains(&step.hf_file) {
            continue;
        }
        if result.last().is_some_and(|last| {
            (step.expected_bytes as f64) < last.expected_bytes as f64 * ratio as f64
        }) {
            continue;
        }
        names.insert(step.hf_file.clone());
        result.push(step);
    }
    let limit = limit.clamp(1, 10);
    if result.len() > limit {
        let count = result.len();
        result = (0..limit)
            .map(|i| {
                result[if limit == 1 {
                    0
                } else {
                    i * (count - 1) / (limit - 1)
                }]
                .clone()
            })
            .collect();
    }
    for (index, step) in result.iter_mut().enumerate() {
        step.step = index + 1;
    }
    result
}

fn fetch_dynamic_ladder(cfg: &crate::sandbox::manager::SusiConfig) -> Vec<ModelLadderConfigStep> {
    let policy = cfg.model_lifecycle();
    let mut headers = reqwest::header::HeaderMap::new();
    if let Ok(token) = std::env::var("HF_TOKEN") {
        if let Ok(value) = format!("Bearer {token}").parse() {
            headers.insert(reqwest::header::AUTHORIZATION, value);
        }
    }
    let Ok(client) = Client::builder()
        .timeout(Duration::from_secs(8))
        .default_headers(headers)
        .build()
    else {
        return Vec::new();
    };
    let base = cfg.hf_base_url();
    let url = format!("{base}/api/models?search=Instruct&filter=gguf&sort=downloads&direction=-1&limit={}&full=true&cardData=true", policy.discovery_limit.clamp(1, 50));
    let Ok(mut models) = client
        .get(url)
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.json::<Vec<HfModel>>())
    else {
        return Vec::new();
    };
    let fallback = cfg.default_fallback_model();
    if !fallback.hf_repo.is_empty() && !models.iter().any(|m| m.id == fallback.hf_repo) {
        models.push(HfModel {
            id: fallback.hf_repo,
            card_data: Some(HfCardData {
                base_model: Some(HfBaseModel::One(fallback.tokenizer_repo)),
            }),
            tags: Vec::new(),
        });
    }
    let Ok(pool) = rayon::ThreadPoolBuilder::new().num_threads(4).build() else {
        return Vec::new();
    };
    let steps = pool.install(|| {
        models
            .into_par_iter()
            .filter_map(|model| {
                let repo = model.id;
                let base_repo = declared_base_model(model.card_data, &model.tags);
                let files = client
                    .get(format!("{base}/api/models/{repo}/tree/main?limit=1000"))
                    .send()
                    .ok()?
                    .error_for_status()
                    .ok()?
                    .json::<Vec<HfTreeEntry>>()
                    .ok()?;
                let tokenizer_repo = if files
                    .iter()
                    .any(|f| f.path == cfg.tokenizer_filename() && f.size > 0)
                {
                    repo.clone()
                } else {
                    base_repo.clone()?
                };
                let tokenizer_url = format!(
                    "{base}/{tokenizer_repo}/resolve/main/{}",
                    cfg.tokenizer_filename()
                );
                if !client
                    .head(tokenizer_url)
                    .send()
                    .is_ok_and(|r| r.status().is_success())
                {
                    return None;
                }
                // Discovery must stay within the backends actually implemented here.
                let config_repo = base_repo.as_deref().unwrap_or(&tokenizer_repo);
                let config = client
                    .get(format!("{base}/{config_repo}/resolve/main/config.json"))
                    .send()
                    .ok()?
                    .error_for_status()
                    .ok()?
                    .json::<serde_json::Value>()
                    .ok()?;
                if !matches!(
                    config.get("model_type").and_then(|v| v.as_str()),
                    Some("qwen2" | "llama")
                ) {
                    return None;
                }
                let best = files
                    .into_iter()
                    .filter(|f| {
                        f.path.to_ascii_lowercase().ends_with(".gguf")
                            && !f.path.contains("-of-")
                            && !f.path.contains('/')
                            && f.size > 0
                    })
                    .min_by_key(|f| (!f.path.to_ascii_uppercase().contains("Q4_K_M"), f.size))?;
                let overhead = policy.memory_overhead_ratio.max(1.0);
                Some(ModelLadderConfigStep {
                    step: 0,
                    label: format!("{} ({:.1} GiB)", repo, best.size as f64 / 1073741824.0),
                    hf_repo: repo,
                    hf_file: best.path,
                    tokenizer_repo,
                    min_ram_gb: best.size as f32 / 1073741824.0 * overhead,
                    min_bytes: best.size,
                    expected_bytes: best.size,
                    fields: Default::default(),
                })
            })
            .collect()
    });
    compact_ladder(steps, policy.max_ladder_tiers, policy.ladder_size_ratio)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_ladder_removes_equivalent_sizes_and_spans_capacity() {
        let steps = [1u64, 1, 2, 4, 8, 16, 32]
            .into_iter()
            .enumerate()
            .map(|(i, size)| ModelLadderConfigStep {
                step: 99,
                hf_repo: format!("org/model-{i}"),
                hf_file: format!("{i}.gguf"),
                tokenizer_repo: "org/base".into(),
                label: "test".into(),
                min_ram_gb: size as f32,
                min_bytes: size,
                expected_bytes: size,
                fields: Default::default(),
            })
            .collect();
        let ladder = compact_ladder(steps, 3, 1.6);
        assert_eq!(
            ladder.iter().map(|s| s.expected_bytes).collect::<Vec<_>>(),
            vec![1, 4, 32]
        );
        assert_eq!(
            ladder.iter().map(|s| s.step).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn test_tokenizer_metadata_does_not_guess_repository_names() {
        let model: HfModel =
            serde_json::from_str(r#"{"id":"publisher/arbitrary-quant-name"}"#).unwrap();
        assert!(declared_base_model(model.card_data, &model.tags).is_none());
        assert_eq!(
            declared_base_model(None, &["base_model:quantized:original/instruct".into()]),
            Some("original/instruct".into())
        );
    }

    #[test]
    fn test_base_model_metadata_supports_single_and_multiple_values() {
        let single: HfModel = serde_json::from_str(
            r#"{"id":"org/model-GGUF","cardData":{"base_model":"org/model"}}"#,
        )
        .unwrap();
        assert_eq!(
            single
                .card_data
                .and_then(|card| card.base_model)
                .and_then(HfBaseModel::first)
                .as_deref(),
            Some("org/model")
        );

        let multiple: HfModel = serde_json::from_str(
            r#"{"id":"org/model-GGUF","cardData":{"base_model":["","org/model"]}}"#,
        )
        .unwrap();
        assert_eq!(
            multiple
                .card_data
                .and_then(|card| card.base_model)
                .and_then(HfBaseModel::first)
                .as_deref(),
            Some("org/model")
        );

        assert_eq!(
            declared_base_model(
                None,
                &[
                    "base_model:quantized:org/model".to_string(),
                    "base_model:org/model".to_string(),
                ],
            )
            .as_deref(),
            Some("org/model")
        );
    }
}
