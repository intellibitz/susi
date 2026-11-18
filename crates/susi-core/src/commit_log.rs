//! Compatibility path for the shared environment fixture serializer.
//! Ledger records, authority checks and state belong to the commercial runtime.

/// Serialize fixtures that mutate process-wide environment and configuration.
#[doc(hidden)]
pub static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
