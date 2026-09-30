//! Vendor pack: extra OpenAI-compatible providers.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompatVendor {
    pub id: String,
    pub base_url: String,
}

/// Known OpenAI-compatible vendor pack.
#[must_use]
pub fn openai_compat_pack() -> Vec<CompatVendor> {
    [
        ("cerebras", "https://api.cerebras.ai/v1"),
        ("sambanova", "https://api.sambanova.ai/v1"),
        ("nvidia_nim", "https://integrate.api.nvidia.com/v1"),
        ("hyperbolic", "https://api.hyperbolic.xyz/v1"),
        ("perplexity", "https://api.perplexity.ai"),
        ("cohere", "https://api.cohere.ai/compatibility/v1"),
    ]
    .into_iter()
    .map(|(id, base_url)| CompatVendor {
        id: id.into(),
        base_url: base_url.into(),
    })
    .collect()
}

#[cfg(test)]
mod more_openai_compat_tests {
    use super::*;

    #[test]
    fn more_openai_compat_lists_vendor_pack() {
        let pack = openai_compat_pack();
        assert!(pack.len() >= 6);
        assert!(pack.iter().any(|v| v.id == "cerebras"));
        assert!(pack.iter().any(|v| v.id == "cohere"));
    }
}
