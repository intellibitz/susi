//! Gateway parity: `/v1/models` advertises exactly what the brain can
//! route, and `/v1/embeddings` honours the embedding router's pick.
//!
//! Both rules are pure functions over the same data the handlers already
//! produce — keeping them here keeps the contract testable without a
//! live router or model store.

use serde_json::Value;

/// Keep only model entries the brain can actually route. An entry is
/// routable when its display id OR internal id appears in `routable`.
/// The list order is preserved — clients diff by id, not position.
#[must_use]
pub fn routable_only(models: Vec<Value>, routable: &[String]) -> Vec<Value> {
    models
        .into_iter()
        .filter(|m| {
            let id = m.get("id").and_then(Value::as_str).unwrap_or_default();
            let mid = m
                .get("model_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            routable.iter().any(|r| r == id || r == mid)
        })
        .collect()
}

/// Model ids the brain reports as currently routable (order preserved —
/// the router's failover order IS the preference order).
#[must_use]
pub fn routable_ids(failover_order: &[String], local_models: &[String]) -> Vec<String> {
    let mut out = failover_order.to_vec();
    out.extend(local_models.iter().cloned());
    out
}

/// Embedding-capable backends in preference order. `requested` wins when
/// it can embed; otherwise the first capable backend is used (the
/// embedding router's decision, not the chat router's).
#[must_use]
pub fn pick_embedding_backend(requested: Option<&str>, capable: &[String]) -> Option<String> {
    if let Some(r) = requested {
        if capable.iter().any(|c| c == r) {
            return Some(r.to_string());
        }
        // an explicit request for a NON-embedding model is an error the
        // handler reports — encode as None so callers 400/422 it.
        return None;
    }
    capable.first().cloned()
}

/// Whether `model` is a known embedding model/engine.
#[must_use]
pub fn is_embedding_capable(model: &str, capable: &[String]) -> bool {
    capable.iter().any(|c| c == model)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(id: &str, mid: &str) -> Value {
        json!({"id": id, "model_id": mid, "object": "model", "owned_by": "susi"})
    }

    #[test]
    fn gateway_embeddings_models_list_only_routable() {
        let list = vec![
            entry("qwen3b", "/models/qwen-3b.gguf"),
            entry("ghost", "/models/ghost.gguf"),
            entry("openai", "openai"),
        ];
        let routable = routable_ids(
            &["openai".to_string()],
            &["/models/qwen-3b.gguf".to_string()],
        );
        let kept = routable_only(list, &routable);
        assert_eq!(kept.len(), 2);
        assert!(
            kept.iter().all(|m| m["id"] != "ghost"),
            "unroutable filtered"
        );
    }

    #[test]
    fn gateway_embeddings_display_or_internal_id_matches() {
        let list = vec![entry("qwen3b", "/models/qwen-3b.gguf")];
        // router reports the internal path — display id still matches via model_id
        let kept = routable_only(list, &["/models/qwen-3b.gguf".to_string()]);
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn gateway_embeddings_picks_requested_when_capable() {
        let capable = vec!["embed-local".to_string(), "embed-cloud".to_string()];
        assert_eq!(
            pick_embedding_backend(Some("embed-cloud"), &capable).as_deref(),
            Some("embed-cloud")
        );
    }

    #[test]
    fn gateway_embeddings_honours_router_order_unspecified() {
        let capable = vec!["embed-local".to_string(), "embed-cloud".to_string()];
        assert_eq!(
            pick_embedding_backend(None, &capable).as_deref(),
            Some("embed-local"),
            "router's first capable backend wins"
        );
    }

    #[test]
    fn gateway_embeddings_non_embedding_model_rejected() {
        let capable = vec!["embed-local".to_string()];
        assert_eq!(pick_embedding_backend(Some("chat-model"), &capable), None);
        assert!(!is_embedding_capable("chat-model", &capable));
    }

    #[test]
    fn gateway_embeddings_empty_capability_is_none() {
        assert_eq!(pick_embedding_backend(None, &[]), None);
    }
}
