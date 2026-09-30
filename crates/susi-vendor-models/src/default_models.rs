//! Default model selection per vendor from the live `/models` list —
//! `susi keys add` shouldn't make the user pick a model; we rank the
//! advertised ids deterministically (T-CLAUDE-178).

/// Preference table: earlier entries win. Substring match on the model id.
const PREFERENCES: &[(&str, &[&str])] = &[
    (
        "openai",
        &[
            "gpt-5.1",
            "gpt-5",
            "gpt-4.1",
            "gpt-4o",
            "o4-mini",
            "gpt-4o-mini",
        ],
    ),
    (
        "anthropic",
        &[
            "claude-opus-4",
            "claude-sonnet-4",
            "claude-3-7-sonnet",
            "claude-3-5-sonnet",
        ],
    ),
    (
        "google",
        &[
            "gemini-3-pro",
            "gemini-2.5-pro",
            "gemini-2.5-flash",
            "gemini-2.0-flash",
        ],
    ),
    ("deepseek", &["deepseek-chat", "deepseek-reasoner"]),
    (
        "mistral",
        &["mistral-large", "mistral-medium", "mistral-small"],
    ),
    ("groq", &["llama-3.3-70b", "llama-3.1-70b", "mixtral-8x7b"]),
    ("cohere", &["command-a", "command-r-plus", "command-r"]),
    ("xai", &["grok-4", "grok-3", "grok-2"]),
];

/// Ids that should never be the default even when listed.
const NEVER: &[&str] = &[
    "embedding",
    "whisper",
    "tts",
    "dall-e",
    "moderation",
    "babbage",
    "davinci",
    "instruct",
    "audio",
    "image",
    "realtime",
    "transcribe",
    "search-preview",
];

/// Pick the default model for `vendor` from live `/models` ids.
///
/// Order: explicit preference table → otherwise the lexicographically
/// greatest id that isn't on the never list and (for dated ids) parses as a
/// snapshot. `None` when the list is empty or only utility models exist.
pub fn default_model(vendor: &str, models: &[String]) -> Option<String> {
    let usable: Vec<&String> = models
        .iter()
        .filter(|m| !m.trim().is_empty())
        .filter(|m| !NEVER.iter().any(|n| m.to_lowercase().contains(n)))
        .collect();
    if usable.is_empty() {
        return None;
    }
    if let Some(prefs) = PREFERENCES
        .iter()
        .find(|(v, _)| *v == vendor)
        .map(|(_, p)| p)
    {
        for pref in *prefs {
            // prefer the floating alias (shortest id) so the default tracks
            // the family's current snapshot
            let best = usable
                .iter()
                .filter(|m| m.to_lowercase().contains(pref))
                .min_by_key(|m| (m.len(), *m));
            if let Some(m) = best {
                return Some((*m).clone());
            }
        }
    }
    // no table hit — greatest id deterministically
    usable.iter().max().map(|m| (*m).clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn zc_default_models_openai_prefers_flagship() {
        let m = ids(&[
            "gpt-4o-mini",
            "gpt-4o",
            "text-embedding-3-large",
            "gpt-5-2025-08-07",
            "gpt-5",
        ]);
        assert_eq!(default_model("openai", &m).as_deref(), Some("gpt-5"));
    }

    #[test]
    fn zc_default_models_dated_snapshot_wins_within_family() {
        let m = ids(&["claude-sonnet-4-20250514", "claude-3-5-sonnet-20241022"]);
        let pick = default_model("anthropic", &m).unwrap();
        assert!(pick.contains("sonnet"));
    }

    #[test]
    fn zc_default_models_skips_utility_models() {
        let m = ids(&["text-embedding-3-large", "whisper-1", "gpt-4o"]);
        assert_eq!(default_model("openai", &m).as_deref(), Some("gpt-4o"));
    }

    #[test]
    fn zc_default_models_unknown_vendor_falls_back_deterministically() {
        let m = ids(&["zzz-model", "aaa-model"]);
        assert_eq!(default_model("acme", &m).as_deref(), Some("zzz-model"));
    }

    #[test]
    fn zc_default_models_empty_or_only_utility() {
        assert!(default_model("openai", &[]).is_none());
        let m = ids(&["whisper-1", "tts-1"]);
        assert!(default_model("openai", &m).is_none());
    }
}
