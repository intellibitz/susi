use crate::engines::runtime::GemiEngine;
use std::any::Any;

use crate::susi_core::provider::{run_blocking, BoxFuture, Provider};

pub struct CandleProvider;

impl Provider for CandleProvider {
    fn name(&self) -> &str {
        "Candle (Local)"
    }

    fn is_healthy(&self) -> BoxFuture<'_, crate::susi_core::susi_error::EaiResult<bool>> {
        // Healthy only when a local model is actually selectable (weights on
        // disk that fit this host); without one every generate would fail.
        Box::pin(async {
            run_blocking(|| crate::models::ModelManager::get_selected_model(None).is_some())
                .await
                .map_err(crate::susi_core::susi_error::EaiError::process)
        })
    }

    fn generate(
        &self,
        prompt: &str,
    ) -> BoxFuture<'_, crate::susi_core::susi_error::EaiResult<String>> {
        let prompt = prompt.to_string();
        Box::pin(async move {
            // `Provider::generate` (susi-core/src/provider.rs) has no workspace
            // parameter, so there's no real per-request workspace to thread
            // through here; fall back to the process's actual cwd instead of a
            // hardcoded ".".
            let workspace =
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            // Blocking inference runs on a thread of its own, so the future
            // completes under any executor (or none: `provider::block_on`).
            let res = run_blocking(move || GemiEngine::generate_reasoning_deep(&prompt, &workspace))
                .await
                .map_err(crate::susi_core::susi_error::EaiError::process)?;
            Ok(res)
        })
    }

    fn embed(
        &self,
        _text: &str,
    ) -> BoxFuture<'_, crate::susi_core::susi_error::EaiResult<Vec<f32>>> {
        Box::pin(async move {
            Err(crate::susi_core::susi_error::EaiError::inference(
                "Embedding not implemented natively in CandleProvider yet",
            ))
        })
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    #[test]
    fn provider_name_returns_candle_local_label() {
        let p = CandleProvider;
        assert_eq!(p.name(), "Candle (Local)");
    }

    #[test]
    fn as_any_downcasts_back_to_candle_provider() {
        let p = CandleProvider;
        assert!(p.as_any().downcast_ref::<CandleProvider>().is_some());
    }

    #[test]
    fn embed_returns_unimplemented_inference_error() {
        let p = CandleProvider;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let err = rt
            .block_on(p.embed("hello world"))
            .expect_err("embed is unimplemented");
        assert!(err.to_string().contains("not implemented"));
    }

    #[test]
    fn is_healthy_completes_cleanly_in_tokio_runtime() {
        let p = CandleProvider;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let res = rt.block_on(p.is_healthy());
        assert!(res.is_ok());
    }

    #[test]
    fn is_healthy_completes_without_an_async_runtime() {
        let p = CandleProvider;
        assert!(crate::susi_core::provider::block_on(p.is_healthy()).is_ok());
    }

    #[test]
    fn provider_preserves_name_and_concrete_any_type_id() {
        let p = CandleProvider;
        assert_eq!(p.name(), "Candle (Local)");
        assert!(p.as_any().is::<CandleProvider>());
    }
}
