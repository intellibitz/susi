//! Tier-1 generative reflex when the Candle engine is not in this build.
//!
//! Both entry points fail. `SusiPulse` then escalates instead of serving a
//! fabricated answer.

use std::path::Path;
use susi_error::{eai_err as anyhow, EaiResult as Result};

const LOCAL_ENGINE_ABSENT: &str =
    "built-in local model engine is not in this core; rebuild with --features local-engine";

pub struct GenerativeReflexEngine;

impl GenerativeReflexEngine {
    pub fn global() -> &'static Self {
        static ENGINE: GenerativeReflexEngine = GenerativeReflexEngine;
        &ENGINE
    }

    pub fn try_solve(&self, _intent: &str, _workspace: &Path) -> Result<String> {
        Err(anyhow!(LOCAL_ENGINE_ABSENT))
    }

    pub fn try_generate_answer(&self, _intent: &str, _workspace: &Path) -> Result<String> {
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
        assert!(err.to_string().contains("local-engine"));
        assert!(engine.try_solve("status", Path::new(".")).is_err());
    }
}
