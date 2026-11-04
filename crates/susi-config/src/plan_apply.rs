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
        "[REDACTED]".to_string()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_produces_ops_only_for_diffs_with_distinct_transition_hashes_and_redacts_secrets() {
        let mut store = ConfigStore::default();
        store.values.insert("port".into(), "9090".into());
        store.values.insert("log_level".into(), "info".into());

        let mut desired = BTreeMap::new();
        desired.insert("port".into(), "9090".into()); // unchanged
        desired.insert("log_level".into(), "debug".into()); // changed
        desired.insert("openai_api_key".into(), "sk-secret-123".into()); // absent -> secret
        desired.insert("endpoint".into(), "uri=env://X resolved=topsecret".into());

        let plan1 = store.plan(&desired);
        assert_eq!(plan1.ops.len(), 3);
        assert_eq!(
            plan1.preconditions.get("log_level"),
            Some(&Some("info".into()))
        );
        assert_eq!(plan1.preconditions.get("openai_api_key"), Some(&None));

        let rendered = plan1.render();
        assert!(!rendered.contains("sk-secret-123"));
        assert!(!rendered.contains("topsecret"));
        assert!(rendered.contains("openai_api_key: <absent> -> [REDACTED]"));
        assert!(rendered.contains("resolved=[REDACTED]"));

        // Different target value on the same key yields a distinct op.id hash.
        let mut desired2 = BTreeMap::new();
        desired2.insert("log_level".into(), "trace".into());
        let plan2 = store.plan(&desired2);
        let id1 = &plan1.ops.iter().find(|o| o.key == "log_level").unwrap().id;
        let id2 = &plan2.ops[0].id;
        assert_ne!(id1, id2);
    }

    #[test]
    fn apply_mutates_store_replays_idempotently_and_rejects_stale_plans() {
        let mut store = ConfigStore::default();
        store.values.insert("mode".into(), "dev".into());

        let mut desired = BTreeMap::new();
        desired.insert("mode".into(), "prod".into());
        desired.insert("region".into(), "us-east".into());

        let plan = store.plan(&desired);
        let first = store.apply(&plan).expect("initial apply succeeds");
        assert!(first.iter().all(|s| s.starts_with("applied op-")));
        assert_eq!(store.values.get("mode"), Some(&"prod".into()));
        assert_eq!(store.values.get("region"), Some(&"us-east".into()));

        // Replay of the exact same plan is a no-op ("skip op-...").
        let replay = store.apply(&plan).expect("replay succeeds");
        assert!(replay.iter().all(|s| s.starts_with("skip op-")));

        // Stale plan when an existing key changed after planning.
        let mut desired_next = BTreeMap::new();
        desired_next.insert("mode".into(), "staging".into());
        let stale_plan = store.plan(&desired_next);
        store.values.insert("mode".into(), "concurrent-edit".into());
        let err = store.apply(&stale_plan).unwrap_err();
        assert!(err.contains("stale plan: mode changed"));

        // Stale plan when a key absent at plan time is inserted before apply.
        let mut desired_new = BTreeMap::new();
        desired_new.insert("new_key".into(), "v1".into());
        let absent_plan = store.plan(&desired_new);
        store.values.insert("new_key".into(), "interloper".into());
        let err = store.apply(&absent_plan).unwrap_err();
        assert!(err.contains("stale plan: new_key changed"));

        // Missing precondition entry is also rejected as stale.
        let broken_plan = ConfigPlan {
            ops: vec![PlanOp {
                id: "op-missing-precond".into(),
                key: "orphan".into(),
                from: None,
                to: "val".into(),
            }],
            preconditions: BTreeMap::new(),
        };
        let err = store.apply(&broken_plan).unwrap_err();
        assert!(err.contains("stale plan: orphan has no precondition"));
    }

    #[test]
    fn plan_is_empty_when_desired_matches_store_and_render_masks_credential_from_and_to() {
        let mut store = ConfigStore::default();
        store.values.insert("port".into(), "9090".into());
        store
            .values
            .insert("anthropic_api_key".into(), "old-secret".into());

        let mut desired = store.values.clone();
        let noop_plan = store.plan(&desired);
        assert!(noop_plan.ops.is_empty());
        assert!(noop_plan.preconditions.is_empty());
        assert!(noop_plan.render().is_empty());

        desired.insert("anthropic_api_key".into(), "new-secret".into());
        let rot_plan = store.plan(&desired);
        assert_eq!(rot_plan.ops.len(), 1);
        let op = &rot_plan.ops[0];
        assert_eq!(op.from, Some("old-secret".into()));
        assert_eq!(op.to, "new-secret");
        let rendered = op.render();
        assert!(rendered.ends_with("anthropic_api_key: [REDACTED] -> [REDACTED]"));
        assert!(!rendered.contains("old-secret"));
        assert!(!rendered.contains("new-secret"));
    }

    #[test]
    fn partial_replay_skips_applied_ops_and_applies_new_ops_and_config_plan_serde_roundtrip() {
        let mut store = ConfigStore::default();
        store.values.insert("k1".into(), "v1_old".into());
        store.values.insert("k2".into(), "v2_old".into());

        let mut desired = BTreeMap::new();
        desired.insert("k1".into(), "v1_new".into());
        desired.insert("k2".into(), "v2_new".into());
        let plan = store.plan(&desired);
        assert_eq!(plan.ops.len(), 2);

        let plan_json = serde_json::to_string(&plan).unwrap();
        let roundtrip: ConfigPlan = serde_json::from_str(&plan_json).unwrap();
        assert_eq!(roundtrip, plan);

        // Simulate op1 already applied while k1 holds its post-apply state and k2 still holds v2_old.
        let op1_id = plan.ops[0].id.clone();
        let op2_id = plan.ops[1].id.clone();
        store.values.insert("k1".into(), "already_mutated".into());
        store.applied_ops.insert(op1_id.clone(), true);

        let actions = store.apply(&plan).expect("partial replay succeeds");
        assert_eq!(
            actions,
            vec![format!("skip {op1_id}"), format!("applied {op2_id}")]
        );
        assert_eq!(store.values.get("k1"), Some(&"already_mutated".into()));
        assert_eq!(store.values.get("k2"), Some(&"v2_new".into()));
    }

    #[test]
    fn config_store_default_is_empty_and_empty_desired_on_empty_store_produces_noop_apply() {
        let mut store = ConfigStore::default();
        assert!(store.values.is_empty());
        let plan = store.plan(&BTreeMap::new());
        assert!(plan.ops.is_empty());
        assert!(store.apply(&plan).unwrap().is_empty());
    }
}
