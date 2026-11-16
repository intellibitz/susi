//! Shared lifecycle for the curated model catalogs' explicit probes.
use crate::susi_core::provider::Provider;
use susi_error::{eai_bail as bail, EaiResult as Result, ResultExt as Context};

pub(crate) fn run<P: Provider>(
    prompt: &str,
    runtime_context: &str,
    provider: impl FnOnce() -> Result<P>,
) -> Result<String> {
    if prompt.trim().is_empty() {
        bail!("probe prompt must not be empty");
    }
    // Validation precedes manager construction, endpoint resolution and keys.
    let provider = provider()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context(runtime_context)?;
    let _permit = crate::key_arbitration::try_acquire(provider.name())
        .map_err(|d| susi_error::eai_err!(format!("{}: {}", provider.name(), d.describe())))?;
    let text = runtime
        .block_on(provider.generate(prompt))
        .map_err(|e| susi_error::eai_err!(e.to_string()))?;
    Ok(crate::susi_config::redact_credentials(&text))
}

#[cfg(test)]
#[path = "tests/model_probe.rs"]
mod tests;
