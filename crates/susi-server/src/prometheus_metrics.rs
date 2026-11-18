//! Prometheus /metrics endpoint payload helpers.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricSample {
    pub name: String,
    pub value: f64,
    pub help: String,
}

/// Render a minimal Prometheus text exposition.
#[must_use]
pub fn render_prometheus(samples: &[MetricSample]) -> String {
    let mut out = String::new();
    for s in samples {
        out.push_str(&format!("# HELP {} {}\n", s.name, s.help));
        out.push_str(&format!("# TYPE {} gauge\n", s.name));
        out.push_str(&format!("{} {}\n", s.name, s.value));
    }
    out
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod prometheus_metrics_tests {
    use super::*;

    #[test]
    fn prometheus_metrics_renders_exposition() {
        let body = render_prometheus(&[MetricSample {
            name: "susi_up".into(),
            value: 1.0,
            help: "daemon liveness".into(),
        }]);
        assert!(body.contains("# TYPE susi_up gauge"));
        assert!(body.contains("susi_up 1"));
    }

    #[test]
    fn render_prometheus_empty_slice_returns_empty_string() {
        assert!(render_prometheus(&[]).is_empty());
    }

    #[test]
    fn render_prometheus_renders_multiple_samples_in_order() {
        let samples = [
            MetricSample {
                name: "susi_queue_depth".into(),
                value: 3.0,
                help: "queued tasks".into(),
            },
            MetricSample {
                name: "susi_peers".into(),
                value: 5.0,
                help: "connected peers".into(),
            },
        ];
        let body = render_prometheus(&samples);
        let first_pos = body.find("susi_queue_depth 3").expect("first metric");
        let second_pos = body.find("susi_peers 5").expect("second metric");
        assert!(first_pos < second_pos);
    }

    #[test]
    fn metric_sample_serde_roundtrip_preserves_fields() {
        let sample = MetricSample {
            name: "susi_latency_ms".into(),
            value: 42.5,
            help: "request latency".into(),
        };
        let json = serde_json::to_string(&sample).expect("serialize");
        let decoded: MetricSample = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, sample);
    }

    #[test]
    fn render_prometheus_includes_help_line_for_each_metric_sample() {
        let body = render_prometheus(&[MetricSample {
            name: "susi_active_workers".into(),
            value: 4.0,
            help: "active worker count".into(),
        }]);
        assert!(body.contains("# HELP susi_active_workers active worker count\n"));
        assert_eq!(body.lines().count(), 3);
    }
}
