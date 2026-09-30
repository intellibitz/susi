//! API-key vendor inference and canonical env-var aliases.
//!
//! `susi keys add <key>` should not ask which vendor the key belongs to —
//! provider key formats are distinctive enough to infer (T-CLAUDE-174).
//! Likewise every vendor has one canonical env var but users set whatever
//! alias they first saw in docs; `resolve` accepts the whole alias set and
//! reports which name won (T-CLAUDE-185).

/// One recognised vendor key shape, strongest pattern first.
struct Shape {
    vendor: &'static str,
    /// Exact prefix match.
    prefix: &'static str,
    /// Minimum total length to accept the prefix hit.
    min_len: usize,
}

const SHAPES: &[Shape] = &[
    Shape {
        vendor: "anthropic",
        prefix: "sk-ant-",
        min_len: 20,
    },
    Shape {
        vendor: "openrouter",
        prefix: "sk-or-",
        min_len: 20,
    },
    Shape {
        vendor: "openai",
        prefix: "sk-proj-",
        min_len: 20,
    },
    Shape {
        vendor: "openai",
        prefix: "sk-svcacct-",
        min_len: 20,
    },
    Shape {
        vendor: "openai",
        prefix: "sk-",
        min_len: 40,
    },
    Shape {
        vendor: "google",
        prefix: "AIza",
        min_len: 30,
    },
    Shape {
        vendor: "huggingface",
        prefix: "hf_",
        min_len: 20,
    },
    Shape {
        vendor: "github",
        prefix: "github_pat_",
        min_len: 30,
    },
    Shape {
        vendor: "github",
        prefix: "ghp_",
        min_len: 30,
    },
    Shape {
        vendor: "github",
        prefix: "gho_",
        min_len: 30,
    },
    Shape {
        vendor: "groq",
        prefix: "gsk_",
        min_len: 40,
    },
    Shape {
        vendor: "xai",
        prefix: "xai-",
        min_len: 30,
    },
    Shape {
        vendor: "perplexity",
        prefix: "pplx-",
        min_len: 30,
    },
    Shape {
        vendor: "deepseek",
        prefix: "sk-",
        min_len: 30,
    },
    Shape {
        vendor: "mistral",
        prefix: "",
        min_len: 32,
    },
];

#[derive(Debug, Clone, PartialEq)]
pub struct Guess {
    pub vendor: &'static str,
    /// `high` = distinctive prefix; `low` = length-only fallback (mistral).
    pub confidence: &'static str,
    pub why: String,
}

/// Infer the vendor from a key's shape. `None` when nothing matches — the
/// caller asks the human instead of guessing.
pub fn infer_vendor(key: &str) -> Option<Guess> {
    let k = key.trim();
    for s in SHAPES {
        if !s.prefix.is_empty() && k.starts_with(s.prefix) && k.len() >= s.min_len {
            // deepseek shares the bare `sk-` shape; openai wins when long enough.
            if s.vendor == "deepseek" && k.len() >= 40 {
                continue;
            }
            return Some(Guess {
                vendor: s.vendor,
                confidence: "high",
                why: format!("key starts with `{}`", s.prefix),
            });
        }
    }
    // Length-only fallbacks for opaque keys.
    if k.len() >= 32 && k.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Some(Guess {
            vendor: "mistral",
            confidence: "low",
            why: "opaque alphanumeric key — mistral keys have no prefix".into(),
        });
    }
    None
}

/// Canonical env var per vendor.
pub fn canonical_env(vendor: &str) -> Option<&'static str> {
    Some(match vendor {
        "openai" => "OPENAI_API_KEY",
        "anthropic" => "ANTHROPIC_API_KEY",
        "google" => "GEMINI_API_KEY",
        "huggingface" => "HF_TOKEN",
        "github" => "GITHUB_TOKEN",
        "groq" => "GROQ_API_KEY",
        "xai" => "XAI_API_KEY",
        "perplexity" => "PERPLEXITY_API_KEY",
        "openrouter" => "OPENROUTER_API_KEY",
        "deepseek" => "DEEPSEEK_API_KEY",
        "mistral" => "MISTRAL_API_KEY",
        "cohere" => "COHERE_API_KEY",
        _ => return None,
    })
}

/// Env names users already have that we accept for `vendor`.
pub fn env_aliases(vendor: &str) -> &'static [&'static str] {
    match vendor {
        "openai" => &["OPENAI_API_KEY", "OPENAI_KEY", "OPENAI_TOKEN"],
        "anthropic" => &["ANTHROPIC_API_KEY", "ANTHROPIC_KEY", "CLAUDE_API_KEY"],
        "google" => &[
            "GEMINI_API_KEY",
            "GOOGLE_API_KEY",
            "GOOGLE_GENAI_API_KEY",
            "GENAI_API_KEY",
        ],
        "huggingface" => &["HF_TOKEN", "HUGGING_FACE_HUB_TOKEN", "HUGGINGFACE_TOKEN"],
        "github" => &["GITHUB_TOKEN", "GH_TOKEN", "GITHUB_PAT"],
        "groq" => &["GROQ_API_KEY", "GROQ_KEY"],
        "xai" => &["XAI_API_KEY", "GROK_API_KEY"],
        "perplexity" => &["PERPLEXITY_API_KEY", "PPLX_API_KEY"],
        "openrouter" => &["OPENROUTER_API_KEY", "OPENROUTER_KEY"],
        "deepseek" => &["DEEPSEEK_API_KEY", "DEEP_SEEK_API_KEY"],
        "mistral" => &["MISTRAL_API_KEY", "MISTRAL_KEY"],
        "cohere" => &["COHERE_API_KEY", "CO_API_KEY", "COHERE_KEY"],
        _ => &[],
    }
}

/// Resolved key + which env name supplied it.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub vendor: String,
    /// Canonical name the caller should persist under.
    pub canonical: &'static str,
    /// The env var the value was actually found in.
    pub found_in: String,
    pub value: String,
}

/// Find a vendor's key across its alias set, preferring the canonical name.
pub fn resolve<'a>(vendor: &str, env: &dyn Fn(&str) -> Option<&'a str>) -> Option<Resolved> {
    let canonical = canonical_env(vendor)?;
    for name in env_aliases(vendor) {
        if let Some(v) = env(name) {
            let v = v.trim();
            if !v.is_empty() {
                return Some(Resolved {
                    vendor: vendor.into(),
                    canonical,
                    found_in: (*name).into(),
                    value: v.into(),
                });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn zc_key_autodetect_distinctive_prefixes() {
        assert_eq!(
            infer_vendor("sk-ant-api03-xxxxxxxxxxxxxxxx")
                .unwrap()
                .vendor,
            "anthropic"
        );
        assert_eq!(
            infer_vendor("sk-proj-abcdefghijklmnopqrstuvwxyz0123456789")
                .unwrap()
                .vendor,
            "openai"
        );
        assert_eq!(
            infer_vendor("sk-or-v1-abcdefghijklmnopqrstuvwxyz012345")
                .unwrap()
                .vendor,
            "openrouter"
        );
        assert_eq!(
            infer_vendor("AIzaSyAbcdefghijklmnopqrstuvwxyz0123456")
                .unwrap()
                .vendor,
            "google"
        );
        assert_eq!(
            infer_vendor("hf_abcdefghijklmnopqrstuvwxyz")
                .unwrap()
                .vendor,
            "huggingface"
        );
        assert_eq!(
            infer_vendor("gsk_abcdefghijklmnopqrstuvwxyz0123456789ABCDEF")
                .unwrap()
                .vendor,
            "groq"
        );
    }

    #[test]
    fn zc_key_autodetect_bare_sk_needs_length() {
        // short sk- is ambiguous, long is openai
        assert!(infer_vendor("sk-short").is_none());
        let long = format!("sk-{}", "a".repeat(45));
        assert_eq!(infer_vendor(&long).unwrap().vendor, "openai");
    }

    #[test]
    fn zc_key_autodetect_opaque_falls_back_low_confidence() {
        let g = infer_vendor("abcdefghijklmnopqrstuvwxyz012345").unwrap();
        assert_eq!(g.confidence, "low");
        assert_eq!(g.vendor, "mistral");
    }

    #[test]
    fn zc_key_autodetect_unknown_returns_none() {
        assert!(infer_vendor("not a key").is_none());
        assert!(infer_vendor("").is_none());
        assert!(infer_vendor("   ").is_none());
    }

    #[test]
    fn zc_key_aliases_canonical_names() {
        assert_eq!(canonical_env("openai"), Some("OPENAI_API_KEY"));
        assert_eq!(canonical_env("google"), Some("GEMINI_API_KEY"));
        assert_eq!(canonical_env("huggingface"), Some("HF_TOKEN"));
        assert!(canonical_env("nobody").is_none());
    }

    #[test]
    fn zc_key_aliases_resolve_prefers_canonical() {
        let mut m = HashMap::new();
        m.insert("GOOGLE_API_KEY", "alias-val");
        m.insert("GEMINI_API_KEY", "canonical-val");
        let env = |k: &str| m.get(k).copied();
        let r = resolve("google", &env).unwrap();
        assert_eq!(r.found_in, "GEMINI_API_KEY");
        assert_eq!(r.value, "canonical-val");
    }

    #[test]
    fn zc_key_aliases_resolve_accepts_alias_only() {
        let mut m = HashMap::new();
        m.insert("ANTHROPIC_KEY", "sk-ant-xxx");
        let env = |k: &str| m.get(k).copied();
        let r = resolve("anthropic", &env).unwrap();
        assert_eq!(r.found_in, "ANTHROPIC_KEY");
        assert_eq!(r.canonical, "ANTHROPIC_API_KEY");
    }

    #[test]
    fn zc_key_aliases_empty_values_are_skipped() {
        let mut m = HashMap::new();
        m.insert("OPENAI_API_KEY", "   ");
        m.insert("OPENAI_KEY", "sk-real");
        let env = |k: &str| m.get(k).copied();
        let r = resolve("openai", &env).unwrap();
        assert_eq!(r.found_in, "OPENAI_KEY");
    }
}
