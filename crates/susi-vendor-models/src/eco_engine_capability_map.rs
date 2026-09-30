//! Serving-engine capability map (VC-201-088 / T-CLAUDE-316).
//!
//! Answers "which serving engine supports which capability and model format"
//! straight from the knowledge base: `has-capability` relations give the
//! capability side, `implements` relations to format standards give the
//! format side. Everything here is a pure projection of recorded,
//! provenance-cited facts — nothing is probed at runtime.

use crate::eco_schema::{KnowledgeBase, RelationKind};
use std::collections::{BTreeMap, BTreeSet};

/// One engine's recorded surface: canonical capability ids plus the ids of
/// the standards/protocols (formats included) it implements.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EngineSurface {
    /// `cap-*` ids from `has-capability` relations.
    pub capabilities: BTreeSet<String>,
    /// Standard/protocol ids from `implements` relations.
    pub implements: BTreeSet<String>,
    /// Component ids this engine is `compatible-with`.
    pub compatible_with: BTreeSet<String>,
}

/// component id -> recorded surface, for every component that carries at
/// least one `has-capability` relation.
#[must_use]
pub fn capability_map(kb: &KnowledgeBase) -> BTreeMap<String, EngineSurface> {
    let mut map: BTreeMap<String, EngineSurface> = BTreeMap::new();
    for rel in &kb.relations {
        match rel.kind {
            RelationKind::HasCapability => {
                map.entry(rel.from.clone())
                    .or_default()
                    .capabilities
                    .insert(rel.to.clone());
            }
            RelationKind::Implements => {
                map.entry(rel.from.clone())
                    .or_default()
                    .implements
                    .insert(rel.to.clone());
            }
            RelationKind::CompatibleWith => {
                map.entry(rel.from.clone())
                    .or_default()
                    .compatible_with
                    .insert(rel.to.clone());
            }
            RelationKind::Provides
            | RelationKind::Publishes
            | RelationKind::ImplementsVersion
            | RelationKind::VersionOf
            | RelationKind::DependsOn
            | RelationKind::Supersedes => {}
        }
    }
    map.retain(|_, surface| !surface.capabilities.is_empty());
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eco_store;

    fn map() -> BTreeMap<String, EngineSurface> {
        let kb = eco_store::load_dir(&eco_store::bundled_source_dir())
            .expect("bundled ecosystem store loads");
        capability_map(&kb)
    }

    #[test]
    fn eco_engine_capability_map_covers_the_major_engines() {
        let m = map();
        for engine in [
            "vllm",
            "sglang",
            "llama-cpp-server",
            "ollama-engine",
            "triton",
        ] {
            assert!(m.contains_key(engine), "{engine} missing from map");
        }
    }

    #[test]
    fn eco_engine_capability_map_records_chat_and_tools() {
        let m = map();
        for engine in ["vllm", "sglang", "llama-cpp-server", "ollama-engine"] {
            assert!(m[engine].capabilities.contains("cap-chat"), "{engine}");
        }
        for engine in ["vllm", "sglang", "ollama-engine"] {
            assert!(
                m[engine].capabilities.contains("cap-tool-calling"),
                "{engine}"
            );
        }
    }

    #[test]
    fn eco_engine_capability_map_records_model_formats() {
        let m = map();
        assert!(m["llama-cpp-server"].implements.contains("gguf"));
        assert!(m["ollama-engine"].implements.contains("gguf"));
        assert!(m["triton"].implements.contains("open-inference-protocol"));
    }

    #[test]
    fn eco_engine_capability_map_records_openai_compatibility() {
        let m = map();
        for engine in ["vllm", "sglang", "llama-cpp-server", "ollama-engine"] {
            let s = &m[engine];
            assert!(
                s.compatible_with.contains("openai-api")
                    || s.implements.contains("openai-chat-completions"),
                "{engine} lacks recorded OpenAI compatibility"
            );
        }
    }

    #[test]
    fn eco_engine_capability_map_only_lists_capable_components() {
        let m = map();
        // entries exist only where at least one capability is recorded, so a
        // bare vendor or protocol never appears as an "engine"
        for (id, s) in &m {
            assert!(!s.capabilities.is_empty(), "{id}");
            for cap in &s.capabilities {
                assert!(cap.starts_with("cap-"), "{id}: {cap}");
            }
        }
    }
}
