//! Crash-safe side-effect intent journal (T-DEVIN-10).
//!
//! Intent is persisted BEFORE dispatch: a crash between record and execution
//! leaves a `Pending` entry, which reconcile marks `Uncertain` — the worker
//! may have run it, may not have. Replay is refused until the entry is
//! explicitly reconciled, so an arbitrary `exec_command` can never be
//! re-dispatched blindly. Classification is honest: shell commands and
//! patches are `Reconcilable`, never `Idempotent` by tool name.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentState {
    /// Recorded before dispatch, outcome unknown.
    Pending,
    /// Dispatched and observed to complete.
    Executed,
    /// Crash window: recorded but never observed — the side effect may or
    /// may not have happened. Blocks replay until reconciled.
    Uncertain,
    /// Reconciled against external state — the effect is known.
    Reconciled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SideEffectIntent {
    pub id: String,
    pub node_id: String,
    pub tool: String,
    /// Digest of the dispatched arguments — the replay dedupe key: a retry
    /// of the same call replays the same intent, a different call is a new
    /// intent needing its own durability record.
    pub args_digest: String,
    pub state: IntentState,
    pub recorded_unix: u64,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntentJournal {
    #[serde(default)]
    entries: BTreeMap<String, SideEffectIntent>,
    #[serde(default)]
    next_seq: u64,
}

impl IntentJournal {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Persist intent before dispatch. Returns the intent id the worker
    /// marks executed on success; on crash it stays `Pending`.
    pub fn record_intent(
        &mut self,
        node_id: &str,
        tool: &str,
        args: &str,
        now_unix: u64,
    ) -> String {
        // Replay dedupe: same node+tool+args replays under the existing
        // intent instead of minting a duplicate record.
        let digest = args_digest(args);
        if let Some(existing) = self
            .entries
            .values()
            .find(|e| e.node_id == node_id && e.tool == tool && e.args_digest == digest)
        {
            return existing.id.clone();
        }
        self.next_seq = self.next_seq.saturating_add(1);
        let id = format!("intent-{}-{}", node_id, self.next_seq);
        self.entries.insert(
            id.clone(),
            SideEffectIntent {
                id: id.clone(),
                node_id: node_id.to_string(),
                tool: tool.to_string(),
                args_digest: digest,
                state: IntentState::Pending,
                recorded_unix: now_unix,
            },
        );
        id
    }

    /// Observe execution completed — the only path to `Executed`.
    pub fn mark_executed(&mut self, intent_id: &str) {
        if let Some(e) = self.entries.get_mut(intent_id) {
            if matches!(e.state, IntentState::Pending | IntentState::Uncertain) {
                e.state = IntentState::Executed;
            }
        }
    }

    /// After a crash: everything still `Pending` is `Uncertain` — the
    /// dispatch may or may not have run. Returns the ids needing
    /// reconciliation before any replay.
    pub fn reconcile_after_crash(&mut self) -> Vec<String> {
        let mut uncertain = Vec::new();
        for e in self.entries.values_mut() {
            if e.state == IntentState::Pending {
                e.state = IntentState::Uncertain;
                uncertain.push(e.id.clone());
            }
        }
        uncertain
    }

    /// Resolve an uncertain intent after checking external state.
    pub fn resolve(&mut self, intent_id: &str, executed: bool) {
        if let Some(e) = self.entries.get_mut(intent_id) {
            if e.state == IntentState::Uncertain {
                e.state = if executed {
                    IntentState::Executed
                } else {
                    IntentState::Reconciled
                };
            }
        }
    }

    /// Replaying a node is refused while any of its intents are uncertain —
    /// an unverified side effect must be reconciled first.
    #[must_use]
    pub fn may_replay_node(&self, node_id: &str) -> bool {
        !self
            .entries
            .values()
            .any(|e| e.node_id == node_id && e.state == IntentState::Uncertain)
    }

    /// Intents still blocking replay for a node.
    #[must_use]
    pub fn unreconciled(&self, node_id: &str) -> Vec<String> {
        self.entries
            .values()
            .filter(|e| e.node_id == node_id && e.state == IntentState::Uncertain)
            .map(|e| e.id.clone())
            .collect()
    }

    #[must_use]
    pub fn get(&self, intent_id: &str) -> Option<&SideEffectIntent> {
        self.entries.get(intent_id)
    }

    /// Adopt persisted journal state on resume; counters stay monotonic.
    pub fn adopt(&mut self, persisted: &IntentJournal) {
        self.next_seq = self.next_seq.max(persisted.next_seq);
        for (id, e) in &persisted.entries {
            self.entries.entry(id.clone()).or_insert_with(|| e.clone());
        }
    }
}

/// Stable content digest of call arguments — replay identity, not secrecy.
#[must_use]
pub fn args_digest(args: &str) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(args.as_bytes());
    let mut hex = String::with_capacity(16);
    for b in digest.iter().take(8) {
        hex.push_str(&format!("{b:02x}"));
    }
    hex
}
