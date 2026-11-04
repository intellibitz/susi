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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_handler() -> TestAgentsHandler {
        TestAgentsHandler {
            profiles: Mutex::new(vec![
                AgentProfile {
                    name: "ContextAgent".into(),
                    description: "Workspace context analyzer.".into(),
                    categories: vec!["context".into()],
                    semantic_anchors: vec!["ls".into(), "pwd".into()],
                    base_rank: 1.0,
                    is_core: true,
                },
                AgentProfile {
                    name: "RouterAgent".into(),
                    description: "Swarm routing coordinator.".into(),
                    categories: vec!["routing".into()],
                    semantic_anchors: vec!["route".into()],
                    base_rank: 0.8,
                    is_core: true,
                },
            ]),
        }
    }

    #[test]
    fn test_agents_handler_lists_registers_idempotently_clamps_ranks_and_rejects_unknown_topic() {
        let handler = sample_handler();

        // 1. AGENTS_META_LIST returns the seeded AgentProfile array.
        let initial = handler
            .handle(topics::AGENTS_META_LIST, json!({}))
            .expect("list initial profiles");
        let arr = initial.as_array().expect("profiles array");
        assert_eq!(arr.len(), 2);
        assert!(arr.iter().any(|p| p["name"] == "ContextAgent"));
        assert!(arr.iter().any(|p| p["name"] == "RouterAgent"));

        // 2. AGENTS_META_REGISTER accepts both {"profile": ...} and direct AgentProfile JSON,
        // and duplicate registration of the same agent name is idempotent.
        let wrapped_profile = json!({
            "profile": {
                "name": "WrappedAgent",
                "description": "Registered via profile wrapper.",
                "categories": ["test"],
                "semantic_anchors": ["wrap"],
                "base_rank": 0.6,
                "is_core": false
            }
        });
        let reg1 = handler
            .handle(topics::AGENTS_META_REGISTER, wrapped_profile.clone())
            .expect("register wrapped profile");
        assert_eq!(reg1, json!({ "ok": true }));

        let direct_profile = json!({
            "name": "DirectAgent",
            "description": "Registered directly.",
            "categories": ["test"],
            "semantic_anchors": ["direct"],
            "base_rank": 0.5,
            "is_core": false
        });
        let reg2 = handler
            .handle(topics::AGENTS_META_REGISTER, direct_profile.clone())
            .expect("register direct profile");
        assert_eq!(reg2, json!({ "ok": true }));

        // Re-registering WrappedAgent must not create a duplicate entry.
        let _ = handler
            .handle(topics::AGENTS_META_REGISTER, wrapped_profile)
            .expect("duplicate register is idempotent");
        let after_reg: Vec<AgentProfile> = serde_json::from_value(
            handler
                .handle(topics::AGENTS_META_LIST, json!({}))
                .expect("list after register"),
        )
        .expect("deserialize AgentProfile vec");
        assert_eq!(after_reg.len(), 4);
        assert_eq!(
            after_reg
                .iter()
                .filter(|p| p.name == "WrappedAgent")
                .count(),
            1
        );

        // 3. AGENTS_META_UPDATE_RANK clamps base_rank to [0.1, 1.0] and requires "name".
        let up_res = handler
            .handle(
                topics::AGENTS_META_UPDATE_RANK,
                json!({ "name": "DirectAgent", "delta": 1.8 }),
            )
            .expect("update rank upward");
        assert_eq!(up_res, json!({ "ok": true }));
        let after_up: Vec<AgentProfile> = serde_json::from_value(
            handler
                .handle(topics::AGENTS_META_LIST, json!({}))
                .expect("list after upward clamp"),
        )
        .expect("deserialize profiles");
        let direct_up = after_up
            .iter()
            .find(|p| p.name == "DirectAgent")
            .expect("find DirectAgent");
        assert!((direct_up.base_rank - 1.0).abs() < f32::EPSILON);

        let down_res = handler
            .handle(
                topics::AGENTS_META_UPDATE_RANK,
                json!({ "name": "DirectAgent", "delta": -2.5 }),
            )
            .expect("update rank downward");
        assert_eq!(down_res, json!({ "ok": true }));
        let after_down: Vec<AgentProfile> = serde_json::from_value(
            handler
                .handle(topics::AGENTS_META_LIST, json!({}))
                .expect("list after downward clamp"),
        )
        .expect("deserialize profiles");
        let direct_down = after_down
            .iter()
            .find(|p| p.name == "DirectAgent")
            .expect("find DirectAgent");
        assert!((direct_down.base_rank - 0.1).abs() < f32::EPSILON);

        let missing_name_err = handler
            .handle(topics::AGENTS_META_UPDATE_RANK, json!({ "delta": 0.2 }))
            .expect_err("missing name must return Err");
        assert!(
            missing_name_err.contains("name required"),
            "unexpected error: {missing_name_err}"
        );

        // 4. Unknown topic returns an error naming the unhandled topic.
        let unknown_err = handler
            .handle("agents.meta.nonexistent", json!({}))
            .expect_err("unknown topic must fail");
        assert!(
            unknown_err.contains("test agents handler: unhandled topic"),
            "unexpected error: {unknown_err}"
        );
    }

    #[test]
    fn wire_registers_agents_prefix_on_global_plane_bus_idempotently() {
        wire();
        wire();
        let list = PlaneBus::global()
            .request(topics::AGENTS_META_LIST, json!({}))
            .expect("request AGENTS_META_LIST on global PlaneBus");
        let profiles: Vec<AgentProfile> =
            serde_json::from_value(list).expect("decode AgentProfile list");
        assert!(profiles.iter().any(|p| p.name == "SelfHealingAgent"));
    }
}
