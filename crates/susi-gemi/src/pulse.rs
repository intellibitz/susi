// SUSI-Pulse: Tier 0 Native Bootstrap Brain
// 100% Neural implementation - Zero Hardcoded Heuristics.

use crate::engines::alpha::SusiAlphaModel;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::LazyLock as Lazy;
use susi_error::{eai_err as anyhow, EaiResult as Result};

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

/// Tier 1 order: a full answer first, the `ACTION:` routing generation only
/// if that fails. `try_solve` always returns an `ACTION:`-prefixed string,
/// so the old order ran its 64-token generation, saw the prefix, and threw
/// the result away to run the 256-token answer generation — and when both
/// failed it tried the answer again under a "Tier 2" label on the *same*
/// model. Same outcomes, one generation fewer on the common path.
fn generative_tiers(
    answer: impl FnOnce() -> Result<String>,
    action: impl FnOnce() -> Result<String>,
) -> Result<String> {
    answer().or_else(|_| action())
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
        let engine = crate::engines::reflex_llm::GenerativeReflexEngine::global();
        if let Ok(answer) = generative_tiers(
            || engine.try_generate_answer(prompt_trimmed, workspace),
            || engine.try_solve(prompt_trimmed, workspace),
        ) {
            cache_insert(&mut REFLEX_CACHE.write(), key, answer.clone());
            return Ok(answer);
        }

        Err(anyhow!("no reflex tier produced an answer"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier1_generates_an_answer_first_and_routes_only_on_failure() {
        use std::cell::Cell;
        let (answers, actions) = (Cell::new(0), Cell::new(0));
        let ok = |text: &str| -> Result<String> { Ok(text.to_string()) };
        let fail = || -> Result<String> { Err(anyhow!("no model")) };

        let served = generative_tiers(
            || {
                answers.set(answers.get() + 1);
                ok("the answer")
            },
            || {
                actions.set(actions.get() + 1);
                ok("ACTION: status")
            },
        );
        assert_eq!(served.unwrap(), "the answer");
        assert_eq!(
            (answers.get(), actions.get()),
            (1, 0),
            "no wasted action generation"
        );

        let routed = generative_tiers(fail, || ok("ACTION: status"));
        assert_eq!(routed.unwrap(), "ACTION: status");
        assert!(generative_tiers(fail, fail).is_err());
    }

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
