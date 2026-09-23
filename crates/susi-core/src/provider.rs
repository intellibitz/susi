use crate::susi_error::EaiResult;
use std::any::Any;
use std::future::Future;
use std::pin::Pin;

/// A type alias for boxed futures to maintain object safety for the Provider trait.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The core Provider trait that abstracts model backends.
/// This allows pluggable inference engines (e.g., Candle, vLLM, Ollama, remote APIs)
/// while keeping the orchestration layer agnostic.
pub trait Provider: Send + Sync + 'static {
    /// The unique name of the provider (e.g., "Candle", "vLLM", "Ollama")
    fn name(&self) -> &str;

    /// Checks if the provider is healthy and ready to accept requests.
    fn is_healthy(&self) -> BoxFuture<'_, EaiResult<bool>>;

    /// Generates text from the given prompt.
    fn generate(&self, prompt: &str) -> BoxFuture<'_, EaiResult<String>>;

    /// Generates embeddings for the given text.
    fn embed(&self, text: &str) -> BoxFuture<'_, EaiResult<Vec<f32>>>;

    /// Exposes the underlying type for downcasting if provider-specific
    /// features are needed.
    fn as_any(&self) -> &dyn Any;
}
