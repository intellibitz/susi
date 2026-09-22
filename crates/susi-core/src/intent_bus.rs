//! Semantic intent bus — provider/consumer matching via shared intent.
//!
//! Replaces rigid byte-pipe IPC *inside the substrate* with intent advertisements
//! and needs. Matching uses token Jaccard plus an optional bag-of-hashes
//! embedding (no ML runtime in `susi-core`). Real fastembed vectors may be
//! attached by upper layers when available.

use crate::bus::global_bus;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::OnceLock;

/// Provide vs need.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentKind {
    Provide,
    Need,
}

/// One intent advertisement or need on the semantic bus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentMessage {
    pub id: String,
    pub from: String,
    pub kind: IntentKind,
    /// Natural-language goal or capability description.
    pub intent: String,
    /// Optional dense embedding (e.g. from fastembed); when absent, hash-bag is used.
    #[serde(default)]
    pub embedding: Option<Vec<f32>>,
    #[serde(default)]
    pub payload: serde_json::Value,
    pub created_at: u64,
}

/// A scored match between a need and a provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentMatch {
    pub need_id: String,
    pub provider_id: String,
    pub provider: String,
    pub score: f32,
    pub provider_intent: String,
}

/// Event published on the typed bus when a match is found.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentRouted {
    pub r#match: IntentMatch,
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn tokenize(text: &str) -> HashSet<String> {
    text.to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| t.len() > 2)
        .map(|t| t.to_string())
        .collect()
}

/// Token Jaccard similarity in \[0, 1\].
pub fn jaccard_similarity(a: &str, b: &str) -> f32 {
    let ta = tokenize(a);
    let tb = tokenize(b);
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    let inter = ta.intersection(&tb).count() as f32;
    let union = ta.union(&tb).count() as f32;
    if union == 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// Fixed-size bag-of-hashes embedding (dim 64) for cheap vector similarity.
pub fn hash_embed(text: &str) -> Vec<f32> {
    const DIM: usize = 64;
    let mut v = vec![0.0f32; DIM];
    for tok in tokenize(text) {
        let mut h: u64 = 0xcbf29ce484222325;
        for b in tok.bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x100000001b3);
        }
        let idx = (h as usize) % DIM;
        let sign = if (h >> 32) & 1 == 0 { 1.0 } else { -1.0 };
        v[idx] += sign;
    }
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
    for x in &mut v {
        *x /= norm;
    }
    v
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || b.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    let denom = na.sqrt() * nb.sqrt();
    if denom < 1e-6 {
        0.0
    } else {
        (dot / denom).clamp(-1.0, 1.0)
    }
}

/// Combined score: 0.55 * Jaccard + 0.45 * cosine(embeddings).
pub fn intent_similarity(a: &IntentMessage, b: &IntentMessage) -> f32 {
    let jac = jaccard_similarity(&a.intent, &b.intent);
    let ea = a.embedding.clone().unwrap_or_else(|| hash_embed(&a.intent));
    let eb = b.embedding.clone().unwrap_or_else(|| hash_embed(&b.intent));
    let cos = cosine(&ea, &eb).max(0.0);
    0.55 * jac + 0.45 * cos
}

/// Process-wide semantic intent bus.
pub struct IntentBus {
    providers: DashMap<String, IntentMessage>,
    needs: DashMap<String, IntentMessage>,
    next_id: std::sync::atomic::AtomicU64,
}

impl IntentBus {
    pub fn new() -> Self {
        Self {
            providers: DashMap::new(),
            needs: DashMap::new(),
            next_id: std::sync::atomic::AtomicU64::new(1),
        }
    }

    pub fn global() -> &'static Self {
        static BUS: OnceLock<IntentBus> = OnceLock::new();
        BUS.get_or_init(IntentBus::new)
    }

    fn alloc_id(&self) -> String {
        format!(
            "intent-{}",
            self.next_id
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        )
    }

    /// Advertise a capability / offer (provider side).
    pub fn advertise(
        &self,
        from: &str,
        intent: &str,
        payload: serde_json::Value,
        embedding: Option<Vec<f32>>,
    ) -> IntentMessage {
        let msg = IntentMessage {
            id: self.alloc_id(),
            from: from.into(),
            kind: IntentKind::Provide,
            intent: intent.into(),
            embedding: embedding.or_else(|| Some(hash_embed(intent))),
            payload,
            created_at: now(),
        };
        self.providers.insert(msg.id.clone(), msg.clone());
        self.route_pending_for_provider(&msg);
        msg
    }

    /// Publish a need; returns best matches (score ≥ threshold).
    pub fn need(
        &self,
        from: &str,
        intent: &str,
        payload: serde_json::Value,
        embedding: Option<Vec<f32>>,
        min_score: f32,
        limit: usize,
    ) -> (IntentMessage, Vec<IntentMatch>) {
        let msg = IntentMessage {
            id: self.alloc_id(),
            from: from.into(),
            kind: IntentKind::Need,
            intent: intent.into(),
            embedding: embedding.or_else(|| Some(hash_embed(intent))),
            payload,
            created_at: now(),
        };
        self.needs.insert(msg.id.clone(), msg.clone());
        let matches = self.match_need(&msg, min_score, limit);
        for m in &matches {
            global_bus().publish(IntentRouted { r#match: m.clone() });
        }
        (msg, matches)
    }

    pub fn match_need(
        &self,
        need: &IntentMessage,
        min_score: f32,
        limit: usize,
    ) -> Vec<IntentMatch> {
        let mut scored: Vec<IntentMatch> = self
            .providers
            .iter()
            .map(|e| {
                let p = e.value();
                IntentMatch {
                    need_id: need.id.clone(),
                    provider_id: p.id.clone(),
                    provider: p.from.clone(),
                    score: intent_similarity(need, p),
                    provider_intent: p.intent.clone(),
                }
            })
            .filter(|m| m.score >= min_score)
            .collect();
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(limit.max(1));
        scored
    }

    /// Match a free-text goal against registered agent capability strings.
    pub fn match_goal_to_capabilities(
        &self,
        goal: &str,
        capabilities: &[(String, String)],
        min_score: f32,
        limit: usize,
    ) -> Vec<(String, f32)> {
        let need = IntentMessage {
            id: "ad-hoc".into(),
            from: "scheduler".into(),
            kind: IntentKind::Need,
            intent: goal.into(),
            embedding: Some(hash_embed(goal)),
            payload: serde_json::Value::Null,
            created_at: now(),
        };
        let mut out: Vec<(String, f32)> = capabilities
            .iter()
            .map(|(name, desc)| {
                let provide = IntentMessage {
                    id: name.clone(),
                    from: name.clone(),
                    kind: IntentKind::Provide,
                    intent: desc.clone(),
                    embedding: Some(hash_embed(desc)),
                    payload: serde_json::Value::Null,
                    created_at: now(),
                };
                (name.clone(), intent_similarity(&need, &provide))
            })
            .filter(|(_, s)| *s >= min_score)
            .collect();
        out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        out.truncate(limit.max(1));
        out
    }

    fn route_pending_for_provider(&self, provider: &IntentMessage) {
        for need_entry in self.needs.iter() {
            let need = need_entry.value();
            let score = intent_similarity(need, provider);
            if score >= 0.25 {
                let m = IntentMatch {
                    need_id: need.id.clone(),
                    provider_id: provider.id.clone(),
                    provider: provider.from.clone(),
                    score,
                    provider_intent: provider.intent.clone(),
                };
                global_bus().publish(IntentRouted { r#match: m });
            }
        }
    }

    pub fn providers(&self) -> Vec<IntentMessage> {
        self.providers.iter().map(|e| e.value().clone()).collect()
    }

    pub fn needs(&self) -> Vec<IntentMessage> {
        self.needs.iter().map(|e| e.value().clone()).collect()
    }

    pub fn clear(&self) {
        self.providers.clear();
        self.needs.clear();
    }
}

impl Default for IntentBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jaccard_and_hash_embed_score_related_intents() {
        let a = IntentMessage {
            id: "1".into(),
            from: "coder".into(),
            kind: IntentKind::Provide,
            intent: "I can write and patch Rust unit tests".into(),
            embedding: None,
            payload: serde_json::Value::Null,
            created_at: 0,
        };
        let b = IntentMessage {
            id: "2".into(),
            from: "user".into(),
            kind: IntentKind::Need,
            intent: "need help writing Rust unit tests for patches".into(),
            embedding: None,
            payload: serde_json::Value::Null,
            created_at: 0,
        };
        let score = intent_similarity(&a, &b);
        assert!(score > 0.2, "score={score}");
        let unrelated = IntentMessage {
            id: "3".into(),
            from: "x".into(),
            kind: IntentKind::Provide,
            intent: "bake sourdough bread recipes".into(),
            embedding: None,
            payload: serde_json::Value::Null,
            created_at: 0,
        };
        assert!(intent_similarity(&b, &unrelated) < score);
    }

    #[test]
    fn bus_matches_need_to_provider() {
        let bus = IntentBus::new();
        bus.advertise(
            "TestAgent",
            "run cargo tests and report failures",
            serde_json::json!({}),
            None,
        );
        let (_need, matches) = bus.need(
            "planner",
            "please run the cargo test suite",
            serde_json::json!({}),
            None,
            0.15,
            5,
        );
        assert!(!matches.is_empty());
        assert_eq!(matches[0].provider, "TestAgent");
    }
}
