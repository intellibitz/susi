//! Bring-your-own-key wiring: map the provider key the user registered with
//! `susi keys set` onto the env names a launched agent actually reads.
//!
//! Agents disagree on naming (OpenHands reads `LLM_API_KEY`/`LLM_BASE_URL`,
//! not `ANTHROPIC_API_KEY`), so a registered key alone leaves a headless run
//! unauthenticated. Explicit user values always win; nothing is invented —
//! in particular no model id is chosen for the user.

/// Provider key env → optional OpenAI-compatible base URL the agent needs to
/// reach that provider. Order is the fallback preference.
const PROVIDERS: &[(&str, Option<&str>)] = &[
    ("ANTHROPIC_API_KEY", None),
    ("OPENAI_API_KEY", None),
    ("OPENROUTER_API_KEY", Some("https://openrouter.ai/api/v1")),
    ("DEEPSEEK_API_KEY", Some("https://api.deepseek.com")),
];

fn non_empty(v: Option<String>) -> Option<String> {
    v.filter(|s| !s.trim().is_empty())
}

/// `LLM_*` variables OpenHands needs, derived through `lookup` (process env,
/// then `cloud.env`). Empty when the user already set `LLM_API_KEY`.
pub(super) fn openhands_env(lookup: &dyn Fn(&str) -> Option<String>) -> Vec<(String, String)> {
    if non_empty(lookup("LLM_API_KEY")).is_some() {
        return Vec::new();
    }
    let Some((key, base)) = PROVIDERS
        .iter()
        .find_map(|(env, base)| non_empty(lookup(env)).map(|k| (k, *base)))
    else {
        return Vec::new();
    };
    let mut out = vec![("LLM_API_KEY".to_string(), key)];
    if let Some(base) = base {
        if non_empty(lookup("LLM_BASE_URL")).is_none() {
            out.push(("LLM_BASE_URL".to_string(), base.to_string()));
        }
    }
    out
}

/// Lookup backed by the real environment with `cloud.env` fallback.
pub(super) fn system_lookup(name: &str) -> Option<String> {
    crate::susi_config::env_or_cloud_env(name).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup<'a>(map: &'a HashMap<&'a str, &'a str>) -> impl Fn(&str) -> Option<String> + 'a {
        |k| map.get(k).map(|v| v.to_string())
    }

    #[test]
    fn explicit_llm_key_wins_and_nothing_is_added() {
        let m = HashMap::from([("LLM_API_KEY", "x"), ("OPENAI_API_KEY", "y")]);
        assert!(openhands_env(&lookup(&m)).is_empty());
    }

    #[test]
    fn anthropic_key_maps_without_base_url() {
        let m = HashMap::from([("ANTHROPIC_API_KEY", "sk-a")]);
        assert_eq!(
            openhands_env(&lookup(&m)),
            vec![("LLM_API_KEY".to_string(), "sk-a".to_string())]
        );
    }

    #[test]
    fn openrouter_key_also_sets_base_url_unless_user_set_one() {
        let m = HashMap::from([("OPENROUTER_API_KEY", "sk-r")]);
        let env = openhands_env(&lookup(&m));
        assert!(env.contains(&(
            "LLM_BASE_URL".to_string(),
            "https://openrouter.ai/api/v1".into()
        )));
        let m = HashMap::from([("OPENROUTER_API_KEY", "sk-r"), ("LLM_BASE_URL", "http://l")]);
        assert!(!openhands_env(&lookup(&m))
            .iter()
            .any(|(k, _)| k == "LLM_BASE_URL"));
    }

    #[test]
    fn blank_and_absent_keys_yield_nothing() {
        let m = HashMap::from([("OPENAI_API_KEY", "  ")]);
        assert!(openhands_env(&lookup(&m)).is_empty());
    }

    #[test]
    fn earlier_provider_is_preferred() {
        let m = HashMap::from([("OPENAI_API_KEY", "o"), ("ANTHROPIC_API_KEY", "a")]);
        assert_eq!(openhands_env(&lookup(&m))[0].1, "a");
    }
}
