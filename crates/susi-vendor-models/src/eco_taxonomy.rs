//! Canonical capability taxonomy: one vocabulary for what AI ecosystem
//! components can do, shared by every vendor profile and by routing.
//!
//! Vendors name the same thing differently — "function calling",
//! "tool use" and "tools" are one capability; "context caching" and
//! "prompt caching" are one capability. The taxonomy is the normalization
//! layer: [`TAXONOMY`] defines each canonical capability (id, class,
//! definition, known vendor spellings), and [`canonical`] maps any spelling
//! to it. Profile modules translate vendor feature names through this table
//! instead of inventing per-vocab terms.
use crate::eco_schema::CapabilityClass;

/// A canonical capability: stable id, its [`CapabilityClass`], a one-line
/// definition, and the spellings vendors give it.
#[derive(Debug, Clone, Copy)]
pub struct Canonical {
    /// Slug used as the capability entity id in the knowledge base.
    pub id: &'static str,
    pub class: CapabilityClass,
    pub definition: &'static str,
    /// Lowercase spellings seen in vendor docs/APIs (without the canonical
    /// id itself — that is always accepted).
    pub aliases: &'static [&'static str],
}

/// The shared vocabulary. Ids are `cap-*` so capability entities are
/// identifiable at a glance.
pub const TAXONOMY: &[Canonical] = &[
    Canonical {
        id: "cap-chat",
        class: CapabilityClass::TextGeneration,
        definition: "Conversational text generation over a message list.",
        aliases: &[
            "chat",
            "chat completions",
            "chat completion",
            "messages",
            "converse",
            "generatecontent",
            "generate content",
            "completion",
            "text generation",
        ],
    },
    Canonical {
        id: "cap-completion",
        class: CapabilityClass::TextGeneration,
        definition: "Raw text completion from a prompt (no chat roles).",
        aliases: &["completions", "legacy completion", "prompt completion"],
    },
    Canonical {
        id: "cap-tool-calling",
        class: CapabilityClass::ToolCalling,
        definition: "Model selects and arguments tools/functions the host executes.",
        aliases: &[
            "tool calling",
            "function calling",
            "tool use",
            "tools",
            "function tools",
            "external functions",
        ],
    },
    Canonical {
        id: "cap-structured-output",
        class: CapabilityClass::StructuredOutput,
        definition: "Responses constrained to a caller-supplied JSON Schema.",
        aliases: &[
            "structured output",
            "json mode",
            "json schema",
            "response format",
            "controlled generation",
        ],
    },
    Canonical {
        id: "cap-streaming",
        class: CapabilityClass::Streaming,
        definition: "Token- or event-level streaming responses.",
        aliases: &[
            "streaming",
            "sse",
            "stream",
            "server sent events",
            "ndjson streaming",
        ],
    },
    Canonical {
        id: "cap-embeddings",
        class: CapabilityClass::Embeddings,
        definition: "Dense vector embeddings for text or multimodal input.",
        aliases: &[
            "embeddings",
            "embedding",
            "text embedding",
            "embed",
            "vector embeddings",
        ],
    },
    Canonical {
        id: "cap-vision",
        class: CapabilityClass::Vision,
        definition: "Image inputs: understanding, description, extraction.",
        aliases: &[
            "vision",
            "image input",
            "multimodal input",
            "image understanding",
            "images",
        ],
    },
    Canonical {
        id: "cap-image-generation",
        class: CapabilityClass::Vision,
        definition: "Image generation or editing from prompts.",
        aliases: &[
            "image generation",
            "images api",
            "text to image",
            "image edit",
        ],
    },
    Canonical {
        id: "cap-audio",
        class: CapabilityClass::Audio,
        definition: "Audio input or output: transcription, speech, translation.",
        aliases: &[
            "audio",
            "speech",
            "speech to text",
            "text to speech",
            "transcription",
            "whisper",
            "tts",
            "stt",
        ],
    },
    Canonical {
        id: "cap-reasoning",
        class: CapabilityClass::Reasoning,
        definition: "Deliberative reasoning traces or thinking budgets.",
        aliases: &[
            "reasoning",
            "extended thinking",
            "thinking",
            "chain of thought",
            "reasoning effort",
            "thought summaries",
        ],
    },
    Canonical {
        id: "cap-batch",
        class: CapabilityClass::Batch,
        definition: "Asynchronous batch submission of many requests at reduced cost.",
        aliases: &["batch", "batch api", "batch inference", "batch prediction"],
    },
    Canonical {
        id: "cap-realtime",
        class: CapabilityClass::Realtime,
        definition: "Bidirectional low-latency sessions (WebSocket/WebRTC).",
        aliases: &[
            "realtime",
            "live api",
            "live",
            "webrtc",
            "bidirectional streaming",
        ],
    },
    Canonical {
        id: "cap-prompt-caching",
        class: CapabilityClass::PromptCaching,
        definition: "Server-side reuse of a long prompt prefix across requests.",
        aliases: &[
            "prompt caching",
            "context caching",
            "cached content",
            "cache control",
            "cache creation",
        ],
    },
    Canonical {
        id: "cap-files",
        class: CapabilityClass::Files,
        definition: "Hosted file upload, storage and retrieval for requests.",
        aliases: &[
            "files",
            "files api",
            "file search",
            "uploads",
            "attachments",
        ],
    },
    Canonical {
        id: "cap-moderation",
        class: CapabilityClass::Moderation,
        definition: "Content classification / safety filtering of inputs or outputs.",
        aliases: &[
            "moderation",
            "safety",
            "content filter",
            "guardrails",
            "safety settings",
        ],
    },
    Canonical {
        id: "cap-agent-delegation",
        class: CapabilityClass::AgentDelegation,
        definition: "Delegating work to or orchestrating sub-agents.",
        aliases: &[
            "agents",
            "agent delegation",
            "sub agents",
            "handoff",
            "a2a",
            "agent mode",
        ],
    },
    Canonical {
        id: "cap-resource-access",
        class: CapabilityClass::ResourceAccess,
        definition: "Access to tools/resources beyond the model: code, search, MCP.",
        aliases: &[
            "code execution",
            "code interpreter",
            "web search",
            "grounding",
            "computer use",
            "mcp",
            "builtin tools",
        ],
    },
    Canonical {
        id: "cap-discovery",
        class: CapabilityClass::Discovery,
        definition: "Enumerating available models, endpoints or registries.",
        aliases: &[
            "list models",
            "model catalog",
            "registry",
            "model discovery",
            "models",
        ],
    },
];

/// Normalize a vendor spelling to its canonical capability. Matching is
/// case-insensitive, treats `-`/`_`/`/` as spaces, and collapses repeats.
/// `None` when no taxonomy entry claims the term — callers record those for
/// review rather than inventing ids.
#[must_use]
pub fn canonical(term: &str) -> Option<&'static Canonical> {
    let norm = normalize(term);
    TAXONOMY
        .iter()
        .find(|c| normalize(c.id) == norm || c.aliases.iter().any(|a| *a == norm))
}

fn normalize(term: &str) -> String {
    let mut out = String::with_capacity(term.len());
    let mut last_space = false;
    for c in term.trim().chars() {
        let c = if matches!(c, '-' | '_' | '/' | '.') {
            ' '
        } else {
            c.to_ascii_lowercase()
        };
        if c == ' ' {
            if !last_space && !out.is_empty() {
                out.push(' ');
            }
            last_space = true;
        } else {
            out.push(c);
            last_space = false;
        }
    }
    out
}

/// The class a canonical capability belongs to, by id — for building
/// `capability` entities that agree with the taxonomy.
#[must_use]
pub fn class_of(canonical_id: &str) -> Option<CapabilityClass> {
    TAXONOMY
        .iter()
        .find(|c| c.id == canonical_id)
        .map(|c| c.class)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taxonomy_covers_the_required_vocabulary() {
        // Task list: chat, tool calling, vision, audio, embeddings, batch,
        // realtime, prompt caching, structured output, reasoning, files,
        // moderation.
        for term in [
            "chat",
            "tool calling",
            "vision",
            "audio",
            "embeddings",
            "batch",
            "realtime",
            "prompt caching",
            "structured output",
            "reasoning",
            "files",
            "moderation",
        ] {
            assert!(canonical(term).is_some(), "{term} must resolve");
        }
    }

    #[test]
    fn vendor_spellings_map_to_canonical() {
        assert_eq!(
            canonical("function calling").map(|c| c.id),
            Some("cap-tool-calling")
        );
        assert_eq!(
            canonical("Tool Use").map(|c| c.id),
            Some("cap-tool-calling")
        );
        assert_eq!(
            canonical("context caching").map(|c| c.id),
            Some("cap-prompt-caching")
        );
        assert_eq!(
            canonical("extended thinking").map(|c| c.id),
            Some("cap-reasoning")
        );
        assert_eq!(
            canonical("JSON mode").map(|c| c.id),
            Some("cap-structured-output")
        );
        assert_eq!(canonical("Files API").map(|c| c.id), Some("cap-files"));
        assert_eq!(canonical("live api").map(|c| c.id), Some("cap-realtime"));
        assert_eq!(canonical("converse").map(|c| c.id), Some("cap-chat"));
        assert_eq!(
            canonical("computer use").map(|c| c.id),
            Some("cap-resource-access")
        );
        assert_eq!(canonical("Batch API").map(|c| c.id), Some("cap-batch"));
    }

    #[test]
    fn normalization_is_insensitive_and_collapses_separators() {
        assert_eq!(
            canonical("  TOOL-CALLING ").map(|c| c.id),
            Some("cap-tool-calling")
        );
        assert_eq!(
            canonical("text_embedding").map(|c| c.id),
            Some("cap-embeddings")
        );
        assert_eq!(
            canonical("function_calling").map(|c| c.id),
            Some("cap-tool-calling")
        );
        assert_eq!(
            canonical("structured.output").map(|c| c.id),
            Some("cap-structured-output")
        );
    }

    #[test]
    fn unknown_terms_return_none() {
        assert!(canonical("quantum entanglement").is_none());
        assert!(canonical("").is_none());
        assert!(canonical("   ").is_none());
    }

    #[test]
    fn every_entry_is_well_formed() {
        for c in TAXONOMY {
            assert!(c.id.starts_with("cap-"), "{}", c.id);
            assert!(!c.definition.is_empty(), "{}", c.id);
            // ids resolve through the same path aliases do
            assert!(canonical(c.id).is_some(), "{}", c.id);
            for a in c.aliases {
                assert_eq!(*a, normalize(a), "alias {a:?} must be pre-normalized");
            }
        }
        // canonical ids are unique
        let mut ids: Vec<_> = TAXONOMY.iter().map(|c| c.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), TAXONOMY.len());
    }

    #[test]
    fn class_of_returns_the_declared_class() {
        assert_eq!(
            class_of("cap-tool-calling"),
            Some(CapabilityClass::ToolCalling)
        );
        assert_eq!(class_of("cap-batch"), Some(CapabilityClass::Batch));
        assert_eq!(class_of("cap-nothing"), None);
    }
}
