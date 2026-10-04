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

/// Durable proof that the external operation was observed to finish.
///
/// The receipt deliberately stores a digest rather than the potentially large
/// tool output.  Its presence is the replay guard; callers can use the digest
/// to bind their own audit record to the observed output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SideEffectReceipt {
    pub output_digest: String,
    pub recorded_unix: u64,
}

/// Result of asking the journal whether an operation may cross the tool
/// boundary.  An acknowledged operation is returned as a receipt and must not
/// be executed a second time.  An uncertain operation remains parked until an
/// operation-specific reconciliation proves whether it happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchDecision {
    Dispatch {
        intent_id: String,
    },
    AlreadyExecuted {
        intent_id: String,
        receipt: SideEffectReceipt,
    },
    ReconciliationRequired {
        intent_id: String,
        reason: String,
    },
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntentJournal {
    #[serde(default)]
    entries: BTreeMap<String, SideEffectIntent>,
    #[serde(default)]
    next_seq: u64,
    /// Stable operation ids are separate from the command digest.  A command
    /// may contain per-attempt fields (for example a cancellation scope),
    /// while the operation must retain one identity across restart.
    #[serde(default)]
    idempotency_keys: BTreeMap<String, String>,
    /// Receipts are separate so older mission files can adopt the journal
    /// without requiring fields that did not exist in their schema.
    #[serde(default)]
    receipts: BTreeMap<String, SideEffectReceipt>,
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
        let digest = args_digest(args);
        let key = format!("{node_id}:{tool}:{digest}");
        self.record_intent_with_key(node_id, tool, args, &key, now_unix)
    }

    /// Persist intent with the caller's stable operation identity.
    ///
    /// `record_intent` remains the compatibility helper for callers that only
    /// have an argument digest.  Production mutation paths should use this
    /// method through [`Self::prepare_dispatch`] and provide an operation id
    /// that survives restart and excludes attempt-local metadata.
    #[allow(clippy::too_many_arguments)] // durable identity, payload and test clock are distinct journal fields
    pub fn record_intent_with_key(
        &mut self,
        node_id: &str,
        tool: &str,
        args: &str,
        idempotency_key: &str,
        now_unix: u64,
    ) -> String {
        let digest = args_digest(args);
        if let Some(existing_id) = self.intent_for_key(idempotency_key) {
            return existing_id;
        }
        // Adopt a legacy intent only when it has no operation key.  Once a
        // caller has supplied distinct stable keys, identical commands are
        // still distinct operations (for example two append steps).
        if let Some(existing) = self.entries.values().find(|e| {
            e.node_id == node_id
                && e.tool == tool
                && e.args_digest == digest
                && !self.idempotency_keys.contains_key(&e.id)
        }) {
            self.idempotency_keys
                .entry(existing.id.clone())
                .or_insert_with(|| idempotency_key.to_string());
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
        self.idempotency_keys
            .insert(id.clone(), idempotency_key.to_string());
        id
    }

    /// Prepare an operation for dispatch, applying the durable replay policy.
    ///
    /// Mutating tools require an explicit operation key.  The key is not a
    /// claim that an arbitrary shell command is idempotent: it makes a later
    /// acknowledged receipt addressable and lets the caller reconcile an
    /// uncertain operation instead of silently issuing another mutation.
    #[allow(clippy::too_many_arguments)] // dispatch policy needs node, tool, payload, key and durable timestamp
    pub fn prepare_dispatch(
        &mut self,
        node_id: &str,
        tool: &str,
        args: &str,
        idempotency_key: Option<&str>,
        now_unix: u64,
    ) -> DispatchDecision {
        let class = crate::side_effects::classify_tool_action(tool);
        let supplied_key = idempotency_key.map(str::trim).filter(|key| !key.is_empty());
        if supplied_key.is_none() && class != crate::side_effects::ActionClass::ReadOnly {
            let key = format!("{node_id}:{tool}:{}", args_digest(args));
            let intent_id = self.record_intent_with_key(node_id, tool, args, &key, now_unix);
            return DispatchDecision::ReconciliationRequired {
                intent_id,
                reason: "mutating operation requires a stable idempotency key".to_string(),
            };
        }
        let key = supplied_key
            .map(str::to_string)
            .unwrap_or_else(|| format!("{node_id}:{tool}:{}", args_digest(args)));
        let digest = args_digest(args);

        if let Some(intent_id) = self.intent_for_key(&key) {
            let Some(intent) = self.entries.get(&intent_id) else {
                return DispatchDecision::ReconciliationRequired {
                    intent_id,
                    reason: "idempotency index points to a missing intent".to_string(),
                };
            };
            if intent.node_id != node_id || intent.tool != tool {
                return DispatchDecision::ReconciliationRequired {
                    intent_id,
                    reason: "idempotency key is already owned by another operation".to_string(),
                };
            }
            if intent.args_digest != digest {
                return DispatchDecision::ReconciliationRequired {
                    intent_id,
                    reason: "operation arguments changed after the intent was recorded".to_string(),
                };
            }
            return match intent.state {
                IntentState::Executed => DispatchDecision::AlreadyExecuted {
                    intent_id: intent.id.clone(),
                    receipt: self.receipt_for(&intent.id),
                },
                IntentState::Pending | IntentState::Uncertain => {
                    DispatchDecision::ReconciliationRequired {
                        intent_id: intent.id.clone(),
                        reason: "operation outcome is uncertain; reconcile it before replay"
                            .to_string(),
                    }
                }
                IntentState::Reconciled => {
                    let id = intent.id.clone();
                    if let Some(entry) = self.entries.get_mut(&id) {
                        entry.state = IntentState::Pending;
                    }
                    DispatchDecision::Dispatch { intent_id: id }
                }
            };
        }

        let intent_id = self.record_intent_with_key(node_id, tool, args, &key, now_unix);
        DispatchDecision::Dispatch { intent_id }
    }

    /// Observe execution completed — the only path to `Executed`.
    pub fn mark_executed(&mut self, intent_id: &str) {
        self.mark_executed_with_receipt(intent_id, "", 0);
    }

    /// Attach a durable receipt after the external call returned successfully.
    pub fn mark_executed_with_receipt(&mut self, intent_id: &str, output: &str, now_unix: u64) {
        let executed = self.entries.get_mut(intent_id).is_some_and(|e| {
            if matches!(e.state, IntentState::Pending | IntentState::Uncertain) {
                e.state = IntentState::Executed;
            }
            e.state == IntentState::Executed
        });
        if executed {
            self.receipts.insert(
                intent_id.to_string(),
                SideEffectReceipt {
                    output_digest: args_digest(output),
                    recorded_unix: now_unix,
                },
            );
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
        if executed {
            self.reconcile_applied(intent_id, "", 0);
        } else {
            self.reconcile_not_applied(intent_id);
        }
    }

    /// Resolve an uncertain intent after an operation-specific external-state
    /// check proved that the effect was applied.
    pub fn reconcile_applied(&mut self, intent_id: &str, output: &str, now_unix: u64) {
        let applied = self.entries.get_mut(intent_id).is_some_and(|e| {
            e.state == IntentState::Uncertain && {
                e.state = IntentState::Executed;
                true
            }
        });
        if applied {
            self.receipts.insert(
                intent_id.to_string(),
                SideEffectReceipt {
                    output_digest: args_digest(output),
                    recorded_unix: now_unix,
                },
            );
        }
    }

    /// Resolve an uncertain intent after an operation-specific external-state
    /// check proved that the effect was not applied.
    pub fn reconcile_not_applied(&mut self, intent_id: &str) {
        if let Some(e) = self.entries.get_mut(intent_id) {
            if e.state == IntentState::Uncertain {
                e.state = IntentState::Reconciled;
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

    /// Stable operation id associated with an intent, if one was persisted.
    #[must_use]
    pub fn idempotency_key(&self, intent_id: &str) -> Option<&str> {
        self.idempotency_keys.get(intent_id).map(String::as_str)
    }

    /// Durable receipt associated with an executed intent.  Old journals that
    /// predate receipts still produce a conservative synthetic receipt: the
    /// `Executed` state remains proof that the dispatcher observed completion.
    #[must_use]
    pub fn receipt(&self, intent_id: &str) -> Option<SideEffectReceipt> {
        self.entries
            .get(intent_id)
            .filter(|e| e.state == IntentState::Executed)
            .map(|_| self.receipt_for(intent_id))
    }

    fn intent_for_key(&self, key: &str) -> Option<String> {
        self.idempotency_keys
            .iter()
            .find_map(|(id, stored)| (stored == key).then_some(id.clone()))
    }

    fn receipt_for(&self, intent_id: &str) -> SideEffectReceipt {
        self.receipts
            .get(intent_id)
            .cloned()
            .unwrap_or_else(|| SideEffectReceipt {
                output_digest: String::new(),
                recorded_unix: self
                    .entries
                    .get(intent_id)
                    .map_or(0, |intent| intent.recorded_unix),
            })
    }

    /// Adopt persisted journal state on resume; counters stay monotonic.
    pub fn adopt(&mut self, persisted: &IntentJournal) {
        self.next_seq = self.next_seq.max(persisted.next_seq);
        for (id, e) in &persisted.entries {
            self.entries.entry(id.clone()).or_insert_with(|| e.clone());
        }
        for (id, key) in &persisted.idempotency_keys {
            self.idempotency_keys
                .entry(id.clone())
                .or_insert_with(|| key.clone());
        }
        for (id, receipt) in &persisted.receipts {
            self.receipts
                .entry(id.clone())
                .or_insert_with(|| receipt.clone());
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
