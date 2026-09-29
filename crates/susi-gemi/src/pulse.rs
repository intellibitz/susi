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

/// A cached reflex and when it stops being servable. Tier-0 actions are a
/// deterministic mapping, valid until the model fingerprint changes (`None`);
/// Tier-1 answers are generated *content* — "what's the weather" — and
/// expire after `TIER1_CACHE_TTL` so they are not served stale for as long
/// as the Tier-0 model happens to stay unchanged.
type CachedReflex = (String, Option<std::time::Instant>);

const TIER1_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(600);

static REFLEX_CACHE: Lazy<Arc<RwLock<HashMap<ReflexKey, CachedReflex>>>> =
    Lazy::new(|| Arc::new(RwLock::new(HashMap::new())));

static CURRENT_FINGERPRINT: Lazy<Arc<RwLock<String>>> =
    Lazy::new(|| Arc::new(RwLock::new(String::new())));

fn cache_insert(cache: &mut HashMap<ReflexKey, CachedReflex>, key: ReflexKey, value: CachedReflex) {
    if cache.len() >= REFLEX_CACHE_CAP && !cache.contains_key(&key) {
        cache.clear();
    }
    cache.insert(key, value);
}

/// The cached reflex for `key` if it has not expired at `now`.
fn cache_lookup(
    cache: &HashMap<ReflexKey, CachedReflex>,
    key: &ReflexKey,
    now: std::time::Instant,
) -> Option<String> {
    let (value, expires) = cache.get(key)?;
    expires
        .is_none_or(|deadline| now < deadline)
        .then(|| value.clone())
}

/// Most recent missions per served action that the suppression rule looks
/// at, and how many of them must have failed to suppress it.
const SUPPRESS_WINDOW: usize = 5;
const SUPPRESS_FAILURES: usize = 3;

/// Tier-0 actions that keep preceding failure. `MissionTrace.reflex_served`
/// (Devin iter7) records which actions a mission was served; among the last
/// `SUPPRESS_WINDOW` missions that were served an action, if at least
/// `SUPPRESS_FAILURES` failed, the reflex precedes failure more often than
/// not and stops being served in this workspace — the prompt escalates to
/// tiers that read language. Governance blocks are not failures of the
/// reflex (policy refused the mission), and `generative` (Tier-1 text) is
/// not a Tier-0 action. Recovers on its own: once successes return to the
/// window the action is served again.
fn suppressed_actions(
    traces: &[crate::susi_core::mission_trace::MissionTrace],
) -> std::collections::HashSet<String> {
    let mut windows: HashMap<String, Vec<bool>> = HashMap::new();
    for trace in traces.iter().rev() {
        let failed = !trace.succeeded() && trace.outcome != "BLOCKED";
        for action in &trace.reflex_served {
            let key = action.to_lowercase();
            if key == "generative" {
                continue;
            }
            let window = windows.entry(key).or_default();
            if window.len() < SUPPRESS_WINDOW {
                window.push(failed);
            }
        }
    }
    windows
        .into_iter()
        .filter(|(_, window)| window.iter().filter(|f| **f).count() >= SUPPRESS_FAILURES)
        .map(|(action, _)| action)
        .collect()
}

/// `suppressed_actions` for a workspace, recomputed only when its
/// `mission_traces.jsonl` changes (keyed by workspace + mtime).
fn workspace_suppressed(workspace: &Path) -> std::collections::HashSet<String> {
    type Entry = (
        Option<std::time::SystemTime>,
        std::collections::HashSet<String>,
    );
    static CACHE: Lazy<RwLock<HashMap<PathBuf, Entry>>> = Lazy::new(|| RwLock::new(HashMap::new()));
    let mtime = std::fs::metadata(workspace.join(".susi").join("mission_traces.jsonl"))
        .and_then(|m| m.modified())
        .ok();
    if let Some((cached_at, set)) = CACHE.read().get(workspace) {
        if *cached_at == mtime {
            return set.clone();
        }
    }
    let set = suppressed_actions(&crate::susi_core::mission_trace::read_all(workspace));
    let mut cache = CACHE.write();
    if cache.len() >= REFLEX_CACHE_CAP {
        cache.clear();
    }
    cache.insert(workspace.to_path_buf(), (mtime, set.clone()));
    set
}

impl SusiPulse {
    /// Tier-0 actions currently suppressed in `workspace` (sorted), for
    /// operator views: a reflex that stops firing should say why.
    pub fn suppressed_reflexes(workspace: &Path) -> Vec<String> {
        let mut actions: Vec<String> = workspace_suppressed(workspace).into_iter().collect();
        actions.sort();
        actions
    }
}

fn action_name(action: &str) -> Option<String> {
    action
        .strip_prefix("ACTION: ")
        .and_then(|rest| rest.split_whitespace().next())
        .map(str::to_lowercase)
}

/// A cached Tier-0 action (`ACTION: name [args]`) is servable only while
/// its action is still runnable; Tier-1 answers (free text) expire by TTL.
fn still_servable(cached: &str) -> bool {
    match cached.strip_prefix("ACTION: ") {
        Some(rest) => {
            let name = rest.split_whitespace().next().unwrap_or_default();
            crate::engines::alpha::action_available(&format!("ACTION: {name}"))
        }
        None => true,
    }
}

/// `list_directory` needs a target, so the Tier-0 action is bound to the
/// workspace. Exact match only: the old `contains("list_directory")` would
/// rewrite any action merely containing the name (a `list_directory_tree`
/// tool admitted by vocabulary reclamation) into a different action.
fn bind_workspace(action: String, workspace: &Path) -> String {
    if action == "ACTION: list_directory" {
        format!("ACTION: list_directory {}", workspace.display())
    } else {
        action
    }
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
        if let Some(cached) = cache_lookup(&REFLEX_CACHE.read(), &key, std::time::Instant::now()) {
            // Tier-0 entries never expire by time, so a cached action must
            // still be runnable: its tool may have been uninstalled since.
            let suppressed = action_name(&cached)
                .is_some_and(|name| workspace_suppressed(workspace).contains(&name));
            if still_servable(&cached) && !suppressed {
                return Ok(cached);
            }
            REFLEX_CACHE.write().remove(&key);
        }

        // Neural Reflex Attempt (Tier 0 Classifier)
        if let Ok(model) = SusiAlphaModel::cached(&global_dir) {
            if let Ok(neural_action) =
                model
                    .predict_intent(prompt_trimmed)
                    .and_then(|action| match action_name(&action) {
                        Some(name) if workspace_suppressed(workspace).contains(&name) => Err(
                            anyhow!("{name} is suppressed: it keeps preceding failed missions"),
                        ),
                        _ => Ok(action),
                    })
            {
                let final_action = bind_workspace(neural_action, workspace);

                cache_insert(&mut REFLEX_CACHE.write(), key, (final_action.clone(), None));
                return Ok(final_action);
            }
        }

        // Generative Reflex Attempt (Tier 1 LLM)
        let engine = crate::engines::reflex_llm::GenerativeReflexEngine::global();
        if let Ok(answer) = generative_tiers(
            || engine.try_generate_answer(prompt_trimmed, workspace),
            || engine.try_solve(prompt_trimmed, workspace),
        ) {
            let expires = std::time::Instant::now() + TIER1_CACHE_TTL;
            cache_insert(
                &mut REFLEX_CACHE.write(),
                key,
                (answer.clone(), Some(expires)),
            );
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
    fn only_the_list_directory_action_is_bound_to_the_workspace() {
        let ws = Path::new("/w");
        assert_eq!(
            bind_workspace("ACTION: list_directory".into(), ws),
            "ACTION: list_directory /w"
        );
        for other in [
            "ACTION: list_directory_tree",
            "ACTION: status",
            "ACTION: my_list_directory",
        ] {
            assert_eq!(bind_workspace(other.into(), ws), other);
        }
    }

    #[test]
    fn reflexes_that_keep_preceding_failure_are_suppressed() {
        use crate::susi_core::mission_trace::MissionTrace;
        let trace = |outcome: &str, served: &[&str]| {
            let mut t = MissionTrace::new("m", "goal", outcome, "fast-path");
            t.reflex_served = served.iter().map(|s| s.to_string()).collect();
            t
        };
        // Oldest first, as read_all returns them.
        let traces = vec![
            trace("COMPLETE", &["status"]),
            trace("FAILED", &["status"]),
            trace("FAILED", &["status", "generative"]),
            trace("FAILED", &["status", "read_file"]),
            trace("BLOCKED", &["read_file"]),
            trace("BLOCKED", &["read_file"]),
            trace("BLOCKED", &["read_file"]),
            trace("COMPLETE", &["list_directory"]),
        ];
        let suppressed = suppressed_actions(&traces);
        assert!(suppressed.contains("status"), "3 of its last 4 failed");
        assert!(
            !suppressed.contains("read_file"),
            "governance blocks are not failures"
        );
        assert!(!suppressed.contains("generative"));
        assert!(!suppressed.contains("list_directory"));

        // Recovery: successes push the failures out of the window.
        let mut recovered = traces.clone();
        for _ in 0..3 {
            recovered.push(trace("COMPLETE", &["status"]));
        }
        assert!(!suppressed_actions(&recovered).contains("status"));
    }

    #[test]
    fn suppressed_reflexes_are_listed_per_workspace() {
        use crate::susi_core::mission_trace::MissionTrace;
        let dir = tempfile::tempdir().unwrap();
        assert!(SusiPulse::suppressed_reflexes(dir.path()).is_empty());
        for outcome in ["FAILED", "FAILED", "FAILED"] {
            let mut t = MissionTrace::new("m", "goal", outcome, "fast-path");
            t.reflex_served = vec!["status".into()];
            t.emit(dir.path()).unwrap();
        }
        assert_eq!(SusiPulse::suppressed_reflexes(dir.path()), ["status"]);
    }

    #[test]
    fn cached_actions_must_still_be_runnable() {
        assert!(still_servable("ACTION: list_directory /w"));
        assert!(still_servable("ACTION: status"));
        assert!(still_servable("A generated Tier-1 answer."));
        assert!(!still_servable(
            "ACTION: zz_uninstalled_tool_for_test --flag"
        ));
    }

    #[test]
    fn generated_answers_expire_but_model_actions_do_not() {
        let mut cache = HashMap::new();
        let now = std::time::Instant::now();
        let key = |p: &str| (PathBuf::from("/w"), p.to_string());
        cache_insert(&mut cache, key("status"), ("ACTION: status".into(), None));
        cache_insert(
            &mut cache,
            key("weather"),
            ("Sunny.".into(), Some(now + TIER1_CACHE_TTL)),
        );
        let later = now + TIER1_CACHE_TTL + std::time::Duration::from_secs(1);
        assert_eq!(
            cache_lookup(&cache, &key("weather"), now).as_deref(),
            Some("Sunny.")
        );
        assert_eq!(cache_lookup(&cache, &key("weather"), later), None);
        assert_eq!(
            cache_lookup(&cache, &key("status"), later).as_deref(),
            Some("ACTION: status")
        );
    }

    #[test]
    fn reflex_cache_is_bounded() {
        let mut cache = HashMap::new();
        for i in 0..REFLEX_CACHE_CAP {
            cache_insert(
                &mut cache,
                (PathBuf::from("/w"), i.to_string()),
                ("a".into(), None),
            );
        }
        assert_eq!(cache.len(), REFLEX_CACHE_CAP);
        // Overwriting an existing key never flushes.
        cache_insert(
            &mut cache,
            (PathBuf::from("/w"), "0".into()),
            ("b".into(), None),
        );
        assert_eq!(cache.len(), REFLEX_CACHE_CAP);
        cache_insert(
            &mut cache,
            (PathBuf::from("/w"), "new".into()),
            ("c".into(), None),
        );
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn reflex_cache_separates_workspaces() {
        let mut cache = HashMap::new();
        cache_insert(
            &mut cache,
            (PathBuf::from("/a"), "ls".into()),
            ("A".into(), None),
        );
        cache_insert(
            &mut cache,
            (PathBuf::from("/b"), "ls".into()),
            ("B".into(), None),
        );
        assert_eq!(
            cache_lookup(
                &cache,
                &(PathBuf::from("/a"), "ls".to_string()),
                std::time::Instant::now()
            ),
            Some("A".to_string())
        );
        assert_eq!(cache.len(), 2);
    }
}
