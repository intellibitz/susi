//! Inspectable configuration plan and apply (VC-201-063).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanOp {
    pub id: String,
    pub key: String,
    pub from: Option<String>,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigPlan {
    pub ops: Vec<PlanOp>,
    pub preconditions: BTreeMap<String, String>,
}

#[derive(Debug, Default, Clone)]
pub struct ConfigStore {
    pub values: BTreeMap<String, String>,
    pub applied_ops: BTreeMap<String, bool>,
}

impl ConfigStore {
    pub fn plan(&self, desired: &BTreeMap<String, String>) -> ConfigPlan {
        let mut ops = Vec::new();
        let mut preconditions = BTreeMap::new();
        for (k, v) in desired {
            let cur = self.values.get(k).cloned();
            if cur.as_deref() != Some(v.as_str()) {
                let id = format!("op-{k}");
                ops.push(PlanOp {
                    id: id.clone(),
                    key: k.clone(),
                    from: cur.clone(),
                    to: v.clone(),
                });
                if let Some(c) = cur {
                    preconditions.insert(k.clone(), c);
                }
            }
        }
        ConfigPlan { ops, preconditions }
    }

    pub fn apply(&mut self, plan: &ConfigPlan) -> Result<Vec<String>, String> {
        // Stale plan: precondition mismatch.
        for (k, expected) in &plan.preconditions {
            if self.values.get(k) != Some(expected) {
                return Err(format!("stale plan: {k} changed"));
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
