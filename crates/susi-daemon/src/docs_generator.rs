//! Auto-Generated Cell Documentation (Swarm OS Bullet 70)
//!
//! Renders each registered cell's manifest and recent trace spans as
//! Markdown — "documentation is auto-generated from cell manifests, code
//! comments, and runtime traces." Code comments stay in the source; this
//! covers the manifest and runtime-trace half from live daemon state
//! rather than a hand-maintained doc going stale.

use susi_abi::swarm::SwarmCellManifest;

use crate::tracing::TraceSpan;

/// Renders one cell's manifest and its spans as a Markdown section.
pub fn render_cell_doc(cell: &SwarmCellManifest, spans: &[TraceSpan]) -> String {
    let mut doc = format!("## {}\n\n", cell.cell_id);
    doc.push_str(&format!("- **Role**: {:?}\n", cell.role));
    doc.push_str(&format!("- **Endpoint**: `{}`\n", cell.endpoint));
    doc.push_str(&format!("- **Trust score**: {:.2}\n", cell.trust_score));
    doc.push_str(&format!("- **Last heartbeat**: {}\n", cell.last_heartbeat));

    if cell.capabilities.is_empty() {
        doc.push_str("- **Capabilities**: _none declared_\n");
    } else {
        doc.push_str(&format!(
            "- **Capabilities**: {}\n",
            cell.capabilities.join(", ")
        ));
    }

    if spans.is_empty() {
        doc.push_str("\n_No recorded trace spans._\n");
    } else {
        doc.push_str("\n### Recent operations\n\n");
        doc.push_str("| Operation | Duration (ms) |\n|---|---|\n");
        for span in spans {
            doc.push_str(&format!("| {} | {} |\n", span.operation, span.duration_ms));
        }
    }
    doc.push('\n');
    doc
}

/// Renders every cell in `cells` as one Markdown document, each cell's
/// spans pulled from `spans` by matching `cell_id`.
pub fn render_fleet_doc(cells: &[SwarmCellManifest], spans: &[TraceSpan]) -> String {
    let mut doc = "# Swarm Cell Fleet\n\n".to_string();
    for cell in cells {
        let cell_spans: Vec<TraceSpan> = spans
            .iter()
            .filter(|s| s.cell_id == cell.cell_id)
            .cloned()
            .collect();
        doc.push_str(&render_cell_doc(cell, &cell_spans));
    }
    doc
}

#[cfg(test)]
mod tests {
    use super::*;
    use susi_abi::swarm::{CapabilityBloom, SwarmRole};

    fn manifest(cell_id: &str) -> SwarmCellManifest {
        SwarmCellManifest {
            cell_id: cell_id.to_string(),
            role: SwarmRole::ReflexCell,
            capabilities: vec!["infer".to_string(), "tool:exec".to_string()],
            bloom_filter: CapabilityBloom::default(),
            endpoint: "ipc:///tmp/test.sock".to_string(),
            trust_score: 0.75,
            last_heartbeat: 42,
        }
    }

    #[test]
    fn cell_doc_includes_manifest_fields() {
        let doc = render_cell_doc(&manifest("cell-a"), &[]);
        assert!(doc.contains("## cell-a"));
        assert!(doc.contains("ipc:///tmp/test.sock"));
        assert!(doc.contains("infer, tool:exec"));
        assert!(doc.contains("No recorded trace spans"));
    }

    #[test]
    fn cell_doc_includes_matching_spans() {
        let span = TraceSpan {
            trace_id: "t1".to_string(),
            cell_id: "cell-a".to_string(),
            operation: "infer".to_string(),
            duration_ms: 120,
        };
        let doc = render_cell_doc(&manifest("cell-a"), &[span]);
        assert!(doc.contains("| infer | 120 |"));
    }

    #[test]
    fn fleet_doc_routes_spans_to_the_right_cell_only() {
        let cells = vec![manifest("cell-a"), manifest("cell-b")];
        let spans = vec![TraceSpan {
            trace_id: "t1".to_string(),
            cell_id: "cell-a".to_string(),
            operation: "infer".to_string(),
            duration_ms: 5,
        }];

        let doc = render_fleet_doc(&cells, &spans);
        let cell_a_section = doc.split("## cell-b").next().unwrap();
        assert!(cell_a_section.contains("infer"));
        let cell_b_section = doc.split("## cell-b").nth(1).unwrap();
        assert!(cell_b_section.contains("No recorded trace spans"));
    }
}
