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
}
