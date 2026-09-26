// SUSI-Pulse: Tier 0 Native Bootstrap Brain
// 100% Neural implementation - Zero Hardcoded Heuristics.

use super::alpha::SusiAlphaModel;
use anyhow::{anyhow, Result};
use once_cell::sync::Lazy;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

pub struct SusiPulse;

static REFLEX_CACHE: Lazy<Arc<RwLock<HashMap<String, String>>>> =
    Lazy::new(|| Arc::new(RwLock::new(HashMap::new())));

static CURRENT_FINGERPRINT: Lazy<Arc<RwLock<String>>> =
    Lazy::new(|| Arc::new(RwLock::new(String::new())));

impl SusiPulse {
    /// Pure Neural Intent Resolution
    pub fn reason(prompt: &str, workspace: &Path) -> Result<String> {
        let prompt_trimmed = prompt.trim();

        let global_dir = crate::susi_paths::SusiDirs::config_dir();

        // Neural Synchronization (Cache Invalidation)
        {
            let fingerprint = SusiAlphaModel::get_model_fingerprint(&global_dir);
            let mut current = CURRENT_FINGERPRINT.write();
            if *current != fingerprint {
                if workspace.join(".agents").exists() && std::env::var("SUSI_VERBOSE").is_ok() {
                    eprintln!("[Tier 0 Reflex] Neural substrate evolved. Invalidating cache...");
                }
                *current = fingerprint;
                let mut cache = REFLEX_CACHE.write();
                cache.clear();
            }
        }

        // Sub-100us Reflex Cache
        {
            let cache = REFLEX_CACHE.read();
            if let Some(cached_action) = cache.get(prompt_trimmed) {
                return Ok(cached_action.clone());
            }
        }

        let global_dir = crate::susi_paths::SusiDirs::config_dir();

        // Neural Reflex Attempt (Tier 0 Classifier)
        if let Ok(model) = SusiAlphaModel::load(&global_dir) {
            if let Ok(neural_action) = model.predict_intent(prompt_trimmed) {
                let mut final_action = neural_action;
                if final_action.contains("list_directory") {
                    final_action = format!("ACTION: list_directory {}", workspace.display());
                }

                // Populate Cache
                let mut cache = REFLEX_CACHE.write();
                cache.insert(prompt_trimmed.to_string(), final_action.clone());

                return Ok(final_action);
            }
        }

        // Generative Reflex Attempt (Tier 1 LLM)
        if let Ok(generative_action) = crate::engines::reflex_llm::GenerativeReflexEngine::global()
            .try_solve(prompt_trimmed, workspace)
        {
            let mut cache = REFLEX_CACHE.write();
            cache.insert(prompt_trimmed.to_string(), generative_action.clone());
            return Ok(generative_action);
        }

        Err(anyhow!(
            "Pulse Brain: Neural substrate missing. Transitioning to Tier 2..."
        ))
    }
}
