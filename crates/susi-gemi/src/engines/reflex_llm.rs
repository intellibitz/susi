//! Tier-1 generative reflex served through [`crate::engines::local_plug::LocalEnginePlug`]
//! (or the built-in local engine when compiled with `--features local-engine`).
//!
//! When neither a plug nor `--features local-engine` is active, both entry
//! points fail closed so `SusiPulse` escalates instead of serving a fabricated
//! answer.

use crate::engines::local_plug::{
    call_bus_plug, local_engine_plug, TOPIC_LOCAL_REFLEX_ANSWER, TOPIC_LOCAL_REFLEX_SOLVE,
};
use serde_json::json;
use std::path::Path;
use susi_error::{eai_err as anyhow, EaiResult as Result};

const LOCAL_ENGINE_ABSENT: &str =
    "local model engine is not attached; register a LocalEnginePlug (susi-gpu-fabric) or build with --features local-engine";

pub struct GenerativeReflexEngine;

impl GenerativeReflexEngine {
    pub fn global() -> &'static Self {
        static ENGINE: GenerativeReflexEngine = GenerativeReflexEngine;
        &ENGINE
    }

    pub fn try_solve(&self, intent: &str, workspace: &Path) -> Result<String> {
        if let Some(plug) = local_engine_plug() {
            return plug.reflex_try_solve(intent, workspace);
        }
        if let Some(res) = call_bus_plug(
            TOPIC_LOCAL_REFLEX_SOLVE,
            json!({
                "intent": intent,
                "workspace": workspace.display().to_string(),
            }),
        ) {
            let val = res?;
            if let Some(text) = val.get("text").and_then(serde_json::Value::as_str) {
                return Ok(text.to_string());
            }
        }
        Err(anyhow!(LOCAL_ENGINE_ABSENT))
    }

    pub fn try_generate_answer(&self, intent: &str, workspace: &Path) -> Result<String> {
        if let Some(plug) = local_engine_plug() {
            return plug.reflex_try_generate_answer(intent, workspace);
        }
        if let Some(res) = call_bus_plug(
            TOPIC_LOCAL_REFLEX_ANSWER,
            json!({
                "intent": intent,
                "workspace": workspace.display().to_string(),
            }),
        ) {
            let val = res?;
            if let Some(text) = val.get("text").and_then(serde_json::Value::as_str) {
                return Ok(text.to_string());
            }
        }
        Err(anyhow!(LOCAL_ENGINE_ABSENT))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier1_refuses_without_the_local_engine() {
        let engine = GenerativeReflexEngine::global();
        let err = engine
            .try_generate_answer("status", Path::new("."))
            .unwrap_err();
        assert!(
            err.to_string().contains("local-engine") || err.to_string().contains("no published"),
            "{err}"
        );
        assert!(engine.try_solve("status", Path::new(".")).is_err());
    }
}
