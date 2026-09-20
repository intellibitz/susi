use std::any::Any;

use susi_core::provider::{BoxFuture, Provider};
use susi_error::{EaiError, EaiResult};

pub struct HttpProvider {
    pub name: String,
    pub api_base: String,
    pub model: String,
}

impl Provider for HttpProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn is_healthy(&self) -> BoxFuture<'_, EaiResult<bool>> {
        let api_base = self.api_base.clone();
        Box::pin(async move {
            let client = reqwest::Client::new();
            let url = format!("{}/models", api_base);
            let res = client
                .get(&url)
                .send()
                .await
                .map_err(|e| EaiError::network(e.to_string()))?;
            Ok(res.status().is_success())
        })
    }

    fn generate(&self, prompt: &str) -> BoxFuture<'_, EaiResult<String>> {
        let prompt = prompt.to_string();
        let api_base = self.api_base.clone();
        let model = self.model.clone();

        Box::pin(async move {
            let client = reqwest::Client::new();
            let url = format!("{}/chat/completions", api_base);

            let body = serde_json::json!({
                "model": model,
                "messages": [
                    {"role": "user", "content": prompt}
                ]
            });

            let res = client
                .post(&url)
                .json(&body)
                .send()
                .await
                .map_err(|e| EaiError::network(e.to_string()))?;
            if !res.status().is_success() {
                return Err(EaiError::process(format!("HTTP Error: {}", res.status())));
            }

            let json: serde_json::Value = res
                .json()
                .await
                .map_err(|e| EaiError::network(e.to_string()))?;
            let content = json["choices"][0]["message"]["content"]
                .as_str()
                .unwrap_or_default()
                .to_string();

            Ok(content)
        })
    }

    fn embed(&self, text: &str) -> BoxFuture<'_, EaiResult<Vec<f32>>> {
        let text = text.to_string();
        let api_base = self.api_base.clone();
        let model = self.model.clone();

        Box::pin(async move {
            let client = reqwest::Client::new();
            let url = format!("{}/embeddings", api_base);

            let body = serde_json::json!({
                "model": model,
                "input": text
            });

            let res = client
                .post(&url)
                .json(&body)
                .send()
                .await
                .map_err(|e| EaiError::network(e.to_string()))?;
            if !res.status().is_success() {
                return Err(EaiError::process(format!("HTTP Error: {}", res.status())));
            }

            let json: serde_json::Value = res
                .json()
                .await
                .map_err(|e| EaiError::network(e.to_string()))?;
            let embedding = json["data"][0]["embedding"]
                .as_array()
                .unwrap_or(&Vec::new())
                .iter()
                .filter_map(|v| v.as_f64().map(|f| f as f32))
                .collect();

            Ok(embedding)
        })
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use susi_core::registry::CapabilityRegistry;

    #[test]
    fn test_universal_engine_registration() {
        let registry = CapabilityRegistry::new();

        // 1. Ollama (Local)
        registry.register_provider(HttpProvider {
            name: "ollama-llama3".to_string(),
            api_base: "http://localhost:11434/v1".to_string(),
            model: "llama3".to_string(),
        });

        // 2. vLLM (Cluster / GPU Rig)
        registry.register_provider(HttpProvider {
            name: "vllm-mixtral".to_string(),
            api_base: "http://localhost:8000/v1".to_string(),
            model: "mistralai/Mixtral-8x7B-Instruct-v0.1".to_string(),
        });

        // 3. llama.cpp (Local CPU/GPU hybrid)
        registry.register_provider(HttpProvider {
            name: "llama.cpp-phi3".to_string(),
            api_base: "http://localhost:8080/v1".to_string(),
            model: "phi3-mini-4k-instruct".to_string(),
        });

        // 4. sglang (High-throughput structured generation)
        registry.register_provider(HttpProvider {
            name: "sglang-llama3-70b".to_string(),
            api_base: "http://localhost:30000/v1".to_string(),
            model: "meta-llama/Meta-Llama-3-70B-Instruct".to_string(),
        });

        let providers = registry.list_providers();

        assert_eq!(providers.len(), 4);
        assert!(providers.contains(&"ollama-llama3".to_string()));
        assert!(providers.contains(&"vllm-mixtral".to_string()));
        assert!(providers.contains(&"llama.cpp-phi3".to_string()));
        assert!(providers.contains(&"sglang-llama3-70b".to_string()));
    }
}
