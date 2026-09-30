//! Acronym disambiguation guard (VC-201-088 / T-CLAUDE-281).
//!
//! The ecosystem space reuses acronyms heavily — ACP names both the Zed Agent
//! Client Protocol and IBM's Agent Communication Protocol; MCP, A2A and ANP
//! collide with similarly-named neighbours. This index maps every acronym an
//! entity could be referenced by to *all* candidate ids, so a query that hits
//! an ambiguous acronym returns every candidate instead of silently picking
//! one.
//!
//! Acronym keys are derived three ways, all deterministic:
//!
//! 1. ALL-CAPS tokens in the display name (`A2A`, `AG-UI`, `IBM`, `GGUF`).
//! 2. Initials of the leading run of Capitalized words — `"Agent Client
//!    Protocol (Zed)"` and `"Agent Communication Protocol (IBM/BeeAI)"` both
//!    yield `ACP`.
//! 3. The entity id uppercased when it is a single short token
//!    (`a2a` -> `A2A`, `mcp` -> `MCP`).
//!
//! Spec versions and capabilities are not indexed: versions share their
//! parent's acronym (a version answer for `MCP` would be wrong) and
//! capabilities already carry canonical `cap-*` ids.

use crate::eco_schema::{Entity, KnowledgeBase};
use std::collections::BTreeMap;

/// Acronym -> sorted candidate entity ids.
#[derive(Debug, Clone, Default)]
pub struct AcronymIndex {
    map: BTreeMap<String, Vec<String>>,
}

impl AcronymIndex {
    /// Build the index over vendors, components, standards and protocols.
    #[must_use]
    pub fn build(kb: &KnowledgeBase) -> Self {
        let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for entity in &kb.entities {
            let indexable = matches!(
                entity,
                Entity::Component(_)
                    | Entity::Vendor(_)
                    | Entity::Standard(_)
                    | Entity::Protocol(_)
            );
            if !indexable {
                continue;
            }
            for key in acronym_keys(entity.id(), entity.name()) {
                map.entry(key).or_default().push(entity.id().to_string());
            }
        }
        for ids in map.values_mut() {
            ids.sort();
            ids.dedup();
        }
        Self { map }
    }

    /// Every entity the acronym can mean. An ambiguous acronym returns all
    /// candidates — it never collapses to a single arbitrary pick.
    #[must_use]
    pub fn lookup(&self, acronym: &str) -> &[String] {
        self.map.get(acronym).map_or(&[], Vec::as_slice)
    }

    /// The disambiguation record: acronyms claimed by more than one entity.
    #[must_use]
    pub fn ambiguous(&self) -> BTreeMap<String, Vec<String>> {
        self.map
            .iter()
            .filter(|(_, ids)| ids.len() > 1)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// `Some(id)` when the acronym names exactly one entity, `None` when it is
    /// unknown or ambiguous — callers must then surface `lookup` candidates.
    #[must_use]
    pub fn unique(&self, acronym: &str) -> Option<&str> {
        let ids = self.lookup(acronym);
        if ids.len() == 1 {
            Some(ids[0].as_str())
        } else {
            None
        }
    }
}

/// All acronym keys an entity answers to, sorted and deduplicated.
fn acronym_keys(id: &str, name: &str) -> Vec<String> {
    let mut keys = Vec::new();
    for token in name.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-')) {
        let shouty = token.len() >= 2
            && token.len() <= 10
            && token.chars().any(|c| c.is_ascii_uppercase())
            && token
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '-');
        if shouty {
            keys.push(token.to_string());
        }
    }
    let mut initials = String::new();
    for word in name.split_whitespace() {
        let word_ok = word.chars().next().is_some_and(|c| c.is_ascii_uppercase())
            && word.chars().nth(1).is_none_or(|c| c.is_ascii_lowercase());
        if word_ok {
            initials.push(word.chars().next().unwrap_or(' '));
        } else {
            break;
        }
    }
    if initials.len() >= 3 {
        keys.push(initials);
    }
    let id_acronym = id.len() <= 6
        && !id.contains('-')
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    if id_acronym {
        keys.push(id.to_uppercase());
    }
    keys.sort();
    keys.dedup();
    keys
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eco_store;

    fn index() -> AcronymIndex {
        let kb = eco_store::load_dir(&eco_store::bundled_source_dir())
            .expect("bundled ecosystem store loads");
        AcronymIndex::build(&kb)
    }

    #[test]
    fn eco_acronym_guard_acp_is_ambiguous_across_vendors() {
        let ix = index();
        let candidates = ix.lookup("ACP");
        assert!(candidates.iter().any(|id| id == "acp"));
        assert!(candidates.iter().any(|id| id == "acp-ibm"));
        assert!(candidates.len() >= 2, "{candidates:?}");
        // ambiguous acronym must never resolve silently
        assert!(ix.unique("ACP").is_none());
    }

    #[test]
    fn eco_acronym_guard_disambiguation_record_lists_collisions() {
        let ambiguous = index().ambiguous();
        let acp = ambiguous.get("ACP").expect("ACP collision recorded");
        assert!(acp.contains(&"acp".to_string()));
        assert!(acp.contains(&"acp-ibm".to_string()));
    }

    #[test]
    fn eco_acronym_guard_unambiguous_acronyms_resolve_exactly() {
        let ix = index();
        assert_eq!(ix.unique("A2A"), Some("a2a"));
        assert_eq!(ix.unique("AG-UI"), Some("ag-ui"));
        // the ANP *protocol* and the ANP *community project* share the acronym
        let anp = ix.lookup("ANP");
        assert!(anp.contains(&"anp".to_string()));
        assert!(anp.contains(&"anp-project".to_string()));
        assert!(ix.unique("ANP").is_none());
    }

    #[test]
    fn eco_acronym_guard_mcp_lookup_covers_the_protocol() {
        let ix = index();
        assert!(ix.lookup("MCP").iter().any(|id| id == "mcp"));
    }

    #[test]
    fn eco_acronym_guard_index_is_deterministic() {
        let a = index();
        let b = index();
        assert_eq!(a.map, b.map);
        for ids in a.map.values() {
            let mut sorted = ids.clone();
            sorted.sort();
            sorted.dedup();
            assert_eq!(*ids, sorted);
        }
    }

    #[test]
    fn eco_acronym_guard_unknown_acronym_is_empty_not_panicky() {
        let ix = index();
        assert!(ix.lookup("ZZZ-NOT-A-THING").is_empty());
        assert!(ix.unique("ZZZ-NOT-A-THING").is_none());
    }
}
