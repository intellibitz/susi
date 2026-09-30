//! Gateway embeddings and model listing parity with OpenAI shapes.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmbeddingRequest {
    pub model: String,
    pub input: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmbeddingData {
    pub object: String,
    pub index: usize,
    pub embedding: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmbeddingResponse {
    pub object: String,
    pub data: Vec<EmbeddingData>,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCard {
    pub id: String,
    pub object: String,
    pub owned_by: String,
}

/// Deterministic stub embedding for gateway parity tests (hash of input).
#[must_use]
pub fn embed(req: &EmbeddingRequest) -> EmbeddingResponse {
    let mut h: u32 = 0x811c_9dc5;
    for b in req.input.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    let dims = 8usize;
    let embedding: Vec<f32> = (0..dims)
        .map(|i| ((h.wrapping_add(i as u32) % 1000) as f32) / 1000.0)
        .collect();
    EmbeddingResponse {
        object: "list".into(),
        data: vec![EmbeddingData {
            object: "embedding".into(),
            index: 0,
            embedding,
        }],
        model: req.model.clone(),
    }
}

/// List models in OpenAI `/v1/models` shape.
#[must_use]
pub fn list_models(ids: &[&str]) -> Vec<ModelCard> {
    ids.iter()
        .map(|id| ModelCard {
            id: (*id).into(),
            object: "model".into(),
            owned_by: "susi".into(),
        })
        .collect()
}

#[cfg(test)]
mod gateway_embeddings_tests {
    use super::*;

    #[test]
    fn gateway_embeddings_and_models_parity() {
        let e = embed(&EmbeddingRequest {
            model: "text-embedding-3-small".into(),
            input: "hello".into(),
        });
        assert_eq!(e.object, "list");
        assert_eq!(e.data.len(), 1);
        assert_eq!(e.data[0].embedding.len(), 8);
        let models = list_models(&["a", "b"]);
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].object, "model");
        assert_eq!(models[0].owned_by, "susi");
    }
}
