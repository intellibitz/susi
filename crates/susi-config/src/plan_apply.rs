//! Inspectable configuration plan and apply (VC-201-063).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanOp {
    pub id: String,
    pub key: String,
    pub from: Option<String>,
    pub to: String,
}

impl PlanOp {
    /// One-line, export-safe rendering: the value behind a
    /// credential-named key never appears; every other value still passes
    /// through `redact_for_export` for `resolved=`/credential-field shapes.
    /// Apply consumes the structured fields — rendered text never feeds
    /// back into state.
    #[must_use]
    pub fn render(&self) -> String {
        let from = self
            .from
            .as_deref()
            .map(|v| export_value(&self.key, v))
            .unwrap_or_else(|| "<absent>".to_string());
        format!(
            "{} {}: {} -> {}",
            self.id,
            self.key,
            from,
            export_value(&self.key, &self.to)
        )
    }
}

fn export_value(key: &str, value: &str) -> String {
    if crate::secret_ref::credential_field(key) {
        "<redacted>".to_string()
    } else {
        crate::secret_ref::redact_for_export(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigPlan {
    pub ops: Vec<PlanOp>,
    /// Expected state for every key the plan touches: `Some(v)` requires the
    /// key still hold `v`; `None` requires it still be absent — an insert
    /// must not clobber a value that appeared after the plan was produced.
    pub preconditions: BTreeMap<String, Option<String>>,
}

#[derive(Debug, Default, Clone)]
pub struct ConfigStore {
    pub values: BTreeMap<String, String>,
    pub applied_ops: BTreeMap<String, bool>,
}

impl ConfigPlan {
    /// Plan text for logs/export — plan output never discloses secret
    /// material: credential-named values are masked outright and every
    /// rendered value passes through `redact_for_export`.
    #[must_use]
    pub fn render(&self) -> String {
        self.ops
            .iter()
            .map(PlanOp::render)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// An operation id names the *change*, not just the key: two plans touching
/// the same key with different (from, to) pairs get different ids, so a
/// distinct later change is never mistaken for a replay. Values are hashed
/// rather than inlined — a value may be secret-shaped.
fn op_id(key: &str, from: Option<&str>, to: &str) -> String {
    let mut h = Sha256::new();
    h.update(key.as_bytes());
    match from {
        Some(f) => {
            h.update([1u8]);
            h.update(f.as_bytes());
        }
        None => h.update([0u8]),
    }
    h.update(to.as_bytes());
    let digest = h.finalize();
    format!("op-{key}-{}", hex::encode(&digest[..4]))
}

impl ConfigStore {
    pub fn plan(&self, desired: &BTreeMap<String, String>) -> ConfigPlan {
        let mut ops = Vec::new();
        let mut preconditions = BTreeMap::new();
        for (k, v) in desired {
            let cur = self.values.get(k).cloned();
            if cur.as_deref() != Some(v.as_str()) {
                let id = op_id(k, cur.as_deref(), v);
                ops.push(PlanOp {
                    id: id.clone(),
                    key: k.clone(),
                    from: cur.clone(),
                    to: v.clone(),
                });
                preconditions.insert(k.clone(), cur);
            }
        }
        ConfigPlan { ops, preconditions }
    }

    pub fn apply(&mut self, plan: &ConfigPlan) -> Result<Vec<String>, String> {
        // Stale plan: a pending op's precondition no longer holds — a value
        // changed after planning, or a key absent at plan time now exists.
        // Replayed ops are exempt: their post-state is the precondition's
        // successor, so re-checking them would refuse every replay.
        for op in &plan.ops {
            if self.applied_ops.get(&op.id) == Some(&true) {
                continue;
            }
            let Some(expected) = plan.preconditions.get(&op.key) else {
                return Err(format!("stale plan: {} has no precondition", op.key));
            };
            if self.values.get(&op.key).map(String::as_str) != expected.as_deref() {
                return Err(format!("stale plan: {} changed", op.key));
            }
        }
        let mut done = Vec::new();
        for op in &plan.ops {
            if self.applied_ops.get(&op.id) == Some(&true) {
                // Replay is a no-op, not a duplicate mutation.
                done.push(format!("skip {}", op.id));
                continue;
            }
            self.values.insert(op.key.clone(), op.to.clone());
            self.applied_ops.insert(op.id.clone(), true);
            done.push(format!("applied {}", op.id));
        }
        Ok(done)
    }
}
