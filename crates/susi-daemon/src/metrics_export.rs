//! Prometheus Metrics Export (Swarm OS Bullet 9)
//!
//! A minimal counter-backed scrape endpoint payload: real cell
//! registration/deregistration events drive `active_cells` rather than a
//! hardcoded sample value.

use std::sync::atomic::{AtomicI64, Ordering};

pub struct PrometheusExporter {
    active_cells: AtomicI64,
}

impl Default for PrometheusExporter {
    fn default() -> Self {
        Self::new()
    }
}

impl PrometheusExporter {
    pub fn new() -> Self {
        Self {
            active_cells: AtomicI64::new(0),
        }
    }

    pub fn cell_registered(&self) {
        self.active_cells.fetch_add(1, Ordering::AcqRel);
    }

    pub fn cell_deregistered(&self) {
        self.active_cells.fetch_sub(1, Ordering::AcqRel);
    }

    /// Renders the current counters in Prometheus text exposition format.
    pub fn generate_scrape_payload(&self) -> String {
        format!(
            "# HELP susi_cells_active Number of active cells\n# TYPE susi_cells_active gauge\nsusi_cells_active {}\n",
            self.active_cells.load(Ordering::Acquire)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrape_payload_reflects_registrations() {
        let exporter = PrometheusExporter::new();
        exporter.cell_registered();
        exporter.cell_registered();
        exporter.cell_deregistered();
        assert!(
            exporter
                .generate_scrape_payload()
                .contains("susi_cells_active 1\n")
        );
    }
}
