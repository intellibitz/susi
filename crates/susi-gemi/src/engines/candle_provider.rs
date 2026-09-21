use crate::engine::GemiEngine;
use std::any::Any;

use susi_core::provider::{BoxFuture, Provider};
use susi_error::EaiResult;

pub struct CandleProvider;

impl Provider for CandleProvider {
    fn name(&self) -> &str {
        "Candle (Local)"
    }

    fn is_healthy(&self) -> BoxFuture<'_, EaiResult<bool>> {
        Box::pin(async {
            // Check hardware/candle status
            Ok(true)
        })
    }

    fn generate(&self, prompt: &str) -> BoxFuture<'_, EaiResult<String>> {
        let prompt = prompt.to_string();
        Box::pin(async move {
            // `Provider::generate` (susi-core/src/provider.rs) has no workspace
            // parameter, so there's no real per-request workspace to thread
            // through here; fall back to the process's actual cwd instead of a
            // hardcoded ".".
            let workspace =
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            // Wrapping blocking Candle inference in a spawn_blocking is usually a good idea
            let res = tokio::task::spawn_blocking(move || {
                GemiEngine::generate_reasoning_deep(&prompt, &workspace)
            })
            .await
            .map_err(|e| susi_error::EaiError::process(e.to_string()))?;
            Ok(res)
        })
    }

    fn embed(&self, _text: &str) -> BoxFuture<'_, EaiResult<Vec<f32>>> {
        Box::pin(async move {
            Err(susi_error::EaiError::inference(
                "Embedding not implemented natively in CandleProvider yet",
            ))
        })
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
