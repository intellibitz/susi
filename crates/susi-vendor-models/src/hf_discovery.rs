use crate::susi_sandbox::manager::ModelLadderConfigStep;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::sync::{Mutex, OnceLock};

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

fn hf_auth_headers() -> Vec<(String, String)> {
    crate::susi_config::env_or_cloud_env("HF_TOKEN")
        .ok()
        .filter(|t| !t.is_empty())
        .map(|token| vec![("Authorization".into(), format!("Bearer {token}"))])
        .unwrap_or_default()
}

fn hf_header_refs(owned: &[(String, String)]) -> Vec<(&str, &str)> {
    owned
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect()
}

fn hf_get_json<T: serde::de::DeserializeOwned>(url: &str, headers: &[(&str, &str)]) -> Option<T> {
    let call = susi_http_transport::http_call("GET", url, headers, 8, 10).ok()?;
    if !(200..300).contains(&call.status) {
        return None;
    }
    crate::susi_core::bounded_io::json_capped(
        call.into_reader(),
        crate::susi_core::bounded_io::JSON_BODY_CAP,
    )
    .ok()
}

fn hf_head_ok(url: &str, headers: &[(&str, &str)]) -> bool {
    susi_http_transport::http_call("HEAD", url, headers, 8, 10)
        .is_ok_and(|c| (200..300).contains(&c.status))
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
    susi_paths::SusiDirs::data_dir().join("model-discovery.json")
}

/// GGUF is a container format, not a quantization guarantee: repos do publish
/// F16/BF16/F32 weights inside `.gguf`, costing 2-4x the bytes for a quality
/// gain this substrate cannot exploit. Quantization is per-tensor, so the
/// practical contract is the llama.cpp filename token — `Q4_K_M`, `Q8_0`,
/// `IQ4_XS`, `TQ2_0` all carry `Q` immediately followed by a digit, and
/// `Qwen`'s Q is always followed by `w`, so model names never collide.
/// Applies to discovered and configured steps alike: an explicit `model_ladder`
/// entry naming unquantized weights is a config error, not an opt-out.
pub(crate) fn is_quantized_gguf(path: &str) -> bool {
    path.as_bytes()
        .windows(2)
        .any(|w| w[0].eq_ignore_ascii_case(&b'Q') && w[1].is_ascii_digit())
}

/// The ladder to actually use: whatever `cfg` explicitly configures, or the
/// dynamic HF-discovered ladder when the config leaves it empty. Lives here
/// (not as a `SusiConfig` method) because `sandbox::manager::SusiConfig` must
/// stay a pure config accessor with no dependency on `gemi`.
/// Drops steps whose file is not recognizably quantized. An explicit ladder
/// that filters to nothing stays empty — silently substituting the discovered
/// ladder would hide the config error.
fn retain_quantized(steps: Vec<ModelLadderConfigStep>) -> Vec<ModelLadderConfigStep> {
    steps
        .into_iter()
        .filter(|s| is_quantized_gguf(&s.hf_file))
        .collect()
}

/// HF `model_type` (config.json) strings the local substrate implements a
/// backend for. Not the GGUF `general.architecture` spelling: HF writes
/// `qwen3_moe` where the GGUF header carries `qwen3moe`.
fn supported_model_type(model_type: Option<&str>) -> bool {
    matches!(model_type, Some("qwen2" | "llama" | "qwen3_moe"))
}

pub fn resolve_model_ladder(
    cfg: &crate::susi_sandbox::manager::SusiConfig,
) -> Vec<ModelLadderConfigStep> {
    let configured = cfg.model_ladder();
    if configured.is_empty() {
        discover_dynamic_ladder()
    } else {
        retain_quantized(configured)
    }
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
    let cfg = crate::susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
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
            if let Ok(bytes) = serde_json::to_vec(&*guard) {
                let _ = crate::susi_config::atomic_write_bytes(&cache_path(), &bytes);
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

fn fetch_dynamic_ladder(
    cfg: &crate::susi_sandbox::manager::SusiConfig,
) -> Vec<ModelLadderConfigStep> {
    let policy = cfg.model_lifecycle();
    let auth = hf_auth_headers();
    let hdr = hf_header_refs(&auth);
    let base = cfg.hf_base_url();
    // Catalog discovery is still network egress: under a posture that
    // blocks it, return no dynamic steps (the same as being offline).
    if !crate::susi_core::mac_policy::egress_permitted(&base) {
        return Vec::new();
    }
    let url = format!("{base}/api/models?search=Instruct&filter=gguf&sort=downloads&direction=-1&limit={}&full=true&cardData=true", policy.discovery_limit.clamp(1, 50));
    let Some(mut models) = hf_get_json::<Vec<HfModel>>(&url, &hdr) else {
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
                let files = hf_get_json::<Vec<HfTreeEntry>>(
                    &format!("{base}/api/models/{repo}/tree/main?limit=1000"),
                    &hf_header_refs(&auth),
                )?;
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
                if !hf_head_ok(&tokenizer_url, &hf_header_refs(&auth)) {
                    return None;
                }
                // Discovery must stay within the backends actually implemented here.
                let config_repo = base_repo.as_deref().unwrap_or(&tokenizer_repo);
                let config = hf_get_json::<serde_json::Value>(
                    &format!("{base}/{config_repo}/resolve/main/config.json"),
                    &hf_header_refs(&auth),
                )?;
                if !supported_model_type(config.get("model_type").and_then(|v| v.as_str())) {
                    return None;
                }
                let best = files
                    .into_iter()
                    .filter(|f| {
                        f.path.to_ascii_lowercase().ends_with(".gguf")
                            && !f.path.contains("-of-")
                            && !f.path.contains('/')
                            && f.size > 0
                            && is_quantized_gguf(&f.path)
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

    /// The llama.cpp quant token is a `Q` immediately followed by a digit
    /// (`Q4_K_M`, `Q8_0`, `IQ4_XS`, `TQ2_0`); F16/F32/BF16 in a `.gguf`
    /// container must not qualify — the mandate is quantized-only downloads.
    #[test]
    fn quantized_gguf_detection_accepts_quants_and_rejects_full_precision() {
        for ok in [
            "model-Q4_K_M.gguf",
            "model-q5_k_m.gguf",
            "Llama-3.2-1B-Instruct-Q8_0.gguf",
            "foo-IQ4_XS.gguf",
            "bar-TQ2_0.gguf",
            "gemma-3-4b-it-qat-Q4_0.gguf",
        ] {
            assert!(is_quantized_gguf(ok), "{ok} must qualify as quantized");
        }
        for rejected in [
            "model-F16.gguf",
            "model-f32.gguf",
            "Qwen2.5-7B-Instruct-BF16.gguf",
            "model.gguf",
            "model-qat.gguf",
        ] {
            assert!(
                !is_quantized_gguf(rejected),
                "{rejected} must not qualify as quantized"
            );
        }
    }

    /// A configured ladder entry naming full-precision weights is a config
    /// error the mandate drops, and a ladder that filters to nothing stays
    /// empty rather than silently substituting discovered steps.
    #[test]
    fn retain_quantized_drops_unquantized_configured_steps() {
        let step = |file: &str| ModelLadderConfigStep {
            step: 0,
            label: "t".into(),
            hf_repo: "org/repo".into(),
            hf_file: file.into(),
            tokenizer_repo: "org/base".into(),
            min_ram_gb: 1.0,
            min_bytes: 1,
            expected_bytes: 1,
            fields: Default::default(),
        };
        let kept = retain_quantized(vec![step("model-Q4_K_M.gguf"), step("model-F16.gguf")]);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].hf_file, "model-Q4_K_M.gguf");
        assert!(retain_quantized(vec![step("model-BF16.gguf")]).is_empty());
    }

    /// Admission mirrors the substrate's implemented backends — the MoE
    /// entry is HF's `qwen3_moe` spelling, not the GGUF `qwen3moe` arch.
    #[test]
    fn supported_model_type_admits_only_implemented_backends() {
        for ok in ["qwen2", "llama", "qwen3_moe"] {
            assert!(supported_model_type(Some(ok)), "{ok} must be admitted");
        }
        for rejected in ["qwen3", "qwen2moe", "mixtral", "gemma3", "qwen3moe"] {
            assert!(
                !supported_model_type(Some(rejected)),
                "{rejected} has no implemented backend"
            );
        }
        assert!(!supported_model_type(None));
    }
}
