use std::sync::{Arc, Mutex, Once};

use crate::susi_core::plane_bus::{topics, PlaneBus, PlaneHandler};
use crate::susi_core::AgentProfile;
use serde_json::{json, Value};

struct TestAgentsHandler {
    profiles: Mutex<Vec<AgentProfile>>,
}

impl PlaneHandler for TestAgentsHandler {
    fn handle(&self, topic: &str, payload: Value) -> Result<Value, String> {
        let mut profiles = self.profiles.lock().unwrap_or_else(|e| e.into_inner());
        match topic {
            topics::AGENTS_META_LIST => serde_json::to_value(&*profiles).map_err(|e| e.to_string()),
            topics::AGENTS_META_REGISTER => {
                let profile: AgentProfile = if let Some(p) = payload.get("profile") {
                    serde_json::from_value(p.clone()).map_err(|e| e.to_string())?
                } else {
                    serde_json::from_value(payload.clone()).map_err(|e| e.to_string())?
                };
                if !profiles.iter().any(|a| a.name == profile.name) {
                    profiles.push(profile);
                }
                Ok(json!({ "ok": true }))
            }
            topics::AGENTS_META_UPDATE_RANK => {
                let name = payload
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or("name required")?;
                let delta = payload.get("delta").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                if let Some(agent) = profiles.iter_mut().find(|a| a.name == name) {
                    agent.base_rank = (agent.base_rank + delta).clamp(0.1, 1.0);
                }
                Ok(json!({ "ok": true }))
            }
            other => Err(format!("test agents handler: unhandled topic '{other}'")),
        }
    }
}

pub(crate) fn wire() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        PlaneBus::global().register_prefix(
            "agents.",
            Arc::new(TestAgentsHandler {
                profiles: Mutex::new(vec![AgentProfile {
                    name: "SelfHealingAgent".into(),
                    description: "Autonomous repairing.".into(),
                    categories: vec!["heal".into(), "fix".into()],
                    semantic_anchors: vec!["repair".into(), "test".into()],
                    base_rank: 1.0,
                    is_core: false,
                }]),
            }),
        );
    });
}
