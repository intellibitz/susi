//! Webhook Event Subscriptions
//!
//! External systems register a URL and an optional topic filter;
//! `dispatch` POSTs any blackboard pheromone matching that filter to it as
//! JSON. Delivery uses `susi_http_transport::http_call_with_body` so the
//! daemon does not declare its own HTTP client crate.

use std::collections::HashMap;
use std::sync::RwLock;

use crate::susi_abi::swarm::SwarmPheromone;

#[derive(Debug, Clone)]
pub struct WebhookSubscription {
    pub url: String,
    /// `None` subscribes to every topic.
    pub topic_filter: Option<String>,
}

pub struct WebhookDispatcher {
    subscriptions: RwLock<HashMap<String, WebhookSubscription>>,
}

impl Default for WebhookDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl WebhookDispatcher {
    pub fn new() -> Self {
        Self {
            subscriptions: RwLock::new(HashMap::new()),
        }
    }

    pub fn subscribe(&self, subscriber_id: &str, url: &str, topic_filter: Option<&str>) {
        let mut subs = self
            .subscriptions
            .write()
            .unwrap_or_else(|e| e.into_inner());
        subs.insert(
            subscriber_id.to_string(),
            WebhookSubscription {
                url: url.to_string(),
                topic_filter: topic_filter.map(|s| s.to_string()),
            },
        );
    }

    pub fn unsubscribe(&self, subscriber_id: &str) {
        self.subscriptions
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(subscriber_id);
    }

    /// The subscriber IDs whose filter matches `pheromone`'s topic — the
    /// routing decision `dispatch` acts on, exposed separately so it's
    /// testable without a live network call.
    pub fn matching_subscribers(&self, pheromone: &SwarmPheromone) -> Vec<String> {
        let subs = self.subscriptions.read().unwrap_or_else(|e| e.into_inner());
        subs.iter()
            .filter(|(_, sub)| {
                sub.topic_filter
                    .as_deref()
                    .is_none_or(|t| t == pheromone.topic)
            })
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// POSTs `pheromone` as JSON to every matching subscriber's URL.
    /// Best-effort: a delivery failure is logged, never propagated — one
    /// unreachable webhook must not block or fail delivery to the others.
    pub fn dispatch(&self, pheromone: &SwarmPheromone) {
        let subs = self.subscriptions.read().unwrap_or_else(|e| e.into_inner());
        for sub in subs.values() {
            if sub
                .topic_filter
                .as_deref()
                .is_some_and(|t| t != pheromone.topic)
            {
                continue;
            }
            match serde_json::to_vec(pheromone) {
                Ok(body) => {
                    match susi_http_transport::http_call_with_body(
                        "POST",
                        &sub.url,
                        &[("Content-Type", "application/json")],
                        Some(&body),
                        10,
                        0,
                    ) {
                        Ok(response) if !(200..300).contains(&response.status) => {
                            tracing::warn!(
                                "[WebhookDispatcher] delivery to {} returned HTTP {}",
                                sub.url,
                                response.status
                            );
                        }
                        Ok(_) => {}
                        Err(e) => {
                            tracing::warn!(
                                "[WebhookDispatcher] delivery to {} failed: {}",
                                sub.url,
                                e
                            );
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "[WebhookDispatcher] serialize pheromone for {} failed: {}",
                        sub.url,
                        e
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pheromone(topic: &str) -> SwarmPheromone {
        SwarmPheromone {
            id: "p1".to_string(),
            topic: topic.to_string(),
            emitter_id: "cell-a".to_string(),
            kind: crate::susi_abi::swarm::PheromoneKind::Observation,
            intensity: 1.0,
            payload: serde_json::json!({}),
            ttl_ms: 1000,
            deposited_at: 0,
        }
    }

    #[test]
    fn wildcard_subscription_matches_any_topic() {
        let dispatcher = WebhookDispatcher::new();
        dispatcher.subscribe("sub-1", "http://example.invalid/hook", None);
        assert_eq!(
            dispatcher.matching_subscribers(&pheromone("hardware.stress")),
            vec!["sub-1".to_string()]
        );
        assert_eq!(
            dispatcher.matching_subscribers(&pheromone("consensus.vote")),
            vec!["sub-1".to_string()]
        );
    }

    #[test]
    fn filtered_subscription_only_matches_its_topic() {
        let dispatcher = WebhookDispatcher::new();
        dispatcher.subscribe(
            "sub-1",
            "http://example.invalid/hook",
            Some("hardware.stress"),
        );
        assert_eq!(
            dispatcher.matching_subscribers(&pheromone("hardware.stress")),
            vec!["sub-1".to_string()]
        );
        assert!(
            dispatcher
                .matching_subscribers(&pheromone("consensus.vote"))
                .is_empty()
        );
    }

    #[test]
    fn unsubscribe_removes_the_subscriber() {
        let dispatcher = WebhookDispatcher::new();
        dispatcher.subscribe("sub-1", "http://example.invalid/hook", None);
        dispatcher.unsubscribe("sub-1");
        assert!(
            dispatcher
                .matching_subscribers(&pheromone("any.topic"))
                .is_empty()
        );
    }
}
