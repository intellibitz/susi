// SUSI-Pulse: Tier 0 Native Bootstrap Brain
// 100% Neural implementation - Zero Hardcoded Heuristics.

use super::alpha::SusiAlphaModel;
use anyhow::{anyhow, Result};
use once_cell::sync::Lazy;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct SusiPulse;

/// Reflex answers are keyed by workspace as well as prompt: a
/// `list_directory` reflex embeds the workspace path, so a prompt-only key
/// would serve one workspace's listing target to another.
type ReflexKey = (PathBuf, String);

/// Upper bound on cached reflexes. The daemon is long-lived and every
/// distinct prompt used to add an entry forever.
const REFLEX_CACHE_CAP: usize = 1024;

static REFLEX_CACHE: Lazy<Arc<RwLock<HashMap<ReflexKey, String>>>> =
    Lazy::new(|| Arc::new(RwLock::new(HashMap::new())));

static CURRENT_FINGERPRINT: Lazy<Arc<RwLock<String>>> =
    Lazy::new(|| Arc::new(RwLock::new(String::new())));

fn cache_insert(cache: &mut HashMap<ReflexKey, String>, key: ReflexKey, value: String) {
    if cache.len() >= REFLEX_CACHE_CAP && !cache.contains_key(&key) {
        cache.clear();
    }
    cache.insert(key, value);
}

impl SusiPulse {
    /// Pure Neural Intent Resolution.
    ///
    /// Returns `Err` when no tier produced an answer, so the caller can
    /// escalate to deep reasoning instead of serving a canned apology as a
    /// solved reflex. Failures are never cached.
    pub fn reason(prompt: &str, workspace: &Path) -> Result<String> {
        let prompt_trimmed = prompt.trim();
        let key: ReflexKey = (workspace.to_path_buf(), prompt_trimmed.to_string());

        let global_dir = susi_paths::SusiDirs::config_dir();

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
            if let Some(cached_action) = cache.get(&key) {
                return Ok(cached_action.clone());
            }
        }

        // Neural Reflex Attempt (Tier 0 Classifier)
        if let Ok(model) = SusiAlphaModel::cached(&global_dir) {
            if let Ok(neural_action) = model.predict_intent(prompt_trimmed) {
                let mut final_action = neural_action;
                if final_action.contains("list_directory") {
                    final_action = format!("ACTION: list_directory {}", workspace.display());
                }

                cache_insert(&mut REFLEX_CACHE.write(), key, final_action.clone());
                return Ok(final_action);
            }
        }

        // Generative Reflex Attempt (Tier 1 LLM)
        if let Ok(generative_action) = crate::engines::reflex_llm::GenerativeReflexEngine::global()
            .try_solve(prompt_trimmed, workspace)
        {
            // If the reflex returns an ACTION placeholder, attempt full answer generation.
            if generative_action.trim_start().starts_with("ACTION:") {
                if let Ok(full_answer) =
                    crate::engines::reflex_llm::GenerativeReflexEngine::global()
                        .try_generate_answer(prompt_trimmed, workspace)
                {
                    cache_insert(&mut REFLEX_CACHE.write(), key, full_answer.clone());
                    return Ok(full_answer);
                }
            }
            cache_insert(&mut REFLEX_CACHE.write(), key, generative_action.clone());
            return Ok(generative_action);
        }

        // Tier 2 Local Generation Fallback (full answer)
        if let Ok(full_answer) = crate::engines::reflex_llm::GenerativeReflexEngine::global()
            .try_generate_answer(prompt_trimmed, workspace)
        {
            cache_insert(&mut REFLEX_CACHE.write(), key, full_answer.clone());
            return Ok(full_answer);
        }

        Err(anyhow!("no reflex tier produced an answer"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reflex_cache_is_bounded() {
        let mut cache = HashMap::new();
        for i in 0..REFLEX_CACHE_CAP {
            cache_insert(&mut cache, (PathBuf::from("/w"), i.to_string()), "a".into());
        }
        assert_eq!(cache.len(), REFLEX_CACHE_CAP);
        // Overwriting an existing key never flushes.
        cache_insert(&mut cache, (PathBuf::from("/w"), "0".into()), "b".into());
        assert_eq!(cache.len(), REFLEX_CACHE_CAP);
        cache_insert(&mut cache, (PathBuf::from("/w"), "new".into()), "c".into());
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn reflex_cache_separates_workspaces() {
        let mut cache = HashMap::new();
        cache_insert(&mut cache, (PathBuf::from("/a"), "ls".into()), "A".into());
        cache_insert(&mut cache, (PathBuf::from("/b"), "ls".into()), "B".into());
        assert_eq!(
            cache.get(&(PathBuf::from("/a"), "ls".to_string())),
            Some(&"A".to_string())
        );
        assert_eq!(cache.len(), 2);
    }
}
