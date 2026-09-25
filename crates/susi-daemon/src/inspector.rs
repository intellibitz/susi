//! Visual inspector payload (Swarm OS Bullets 63 and 69)
//!
//! A text topology plus a JSON document an editor can render: registered
//! cells and pheromone counts by topic. There is no separate GUI process;
//! this is the document that surface would show.

use std::collections::BTreeMap;

use susi_abi::swarm::SwarmCellManifest;

pub fn render(cells: &[SwarmCellManifest], topic_counts: &BTreeMap<String, usize>) -> String {
    let mut out = String::from("# Swarm inspector\n## Cells\n");
    if cells.is_empty() {
        out.push_str("none\n");
    } else {
        for cell in cells {
            out.push_str(&format!(
                "- {} trust={:.2} caps={}\n",
                cell.cell_id,
                cell.trust_score,
                cell.capabilities.len()
            ));
        }
    }
    out.push_str("## Pheromones\n");
    if topic_counts.is_empty() {
        out.push_str("none\n");
    } else {
        for (topic, count) in topic_counts {
            out.push_str(&format!("- {topic}: {count}\n"));
        }
    }
    out
}

/// JSON snapshot for an IDE (VS Code, Cursor, JetBrains) to fetch.
pub fn editor_snapshot(
    cells: &[SwarmCellManifest],
    topic_counts: &BTreeMap<String, usize>,
) -> String {
    let cell_rows: Vec<serde_json::Value> = cells
        .iter()
        .map(|cell| {
            serde_json::json!({
                "cell_id": cell.cell_id,
                "trust_score": cell.trust_score,
                "capabilities": cell.capabilities,
            })
        })
        .collect();
    serde_json::json!({
        "cells": cell_rows,
        "pheromones": topic_counts,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use susi_abi::swarm::{CapabilityBloom, SwarmRole};

    #[test]
    fn the_editor_snapshot_names_the_cell_and_a_topic_count() {
        let cells = vec![SwarmCellManifest {
            cell_id: "cell-a".into(),
            role: SwarmRole::ReflexCell,
            capabilities: vec!["infer".into()],
            bloom_filter: CapabilityBloom::default(),
            endpoint: "ipc:///tmp/test.sock".into(),
            trust_score: 0.5,
            last_heartbeat: 0,
        }];
        let mut counts = BTreeMap::new();
        counts.insert("intent.deploy".into(), 2);
        let json = editor_snapshot(&cells, &counts);
        assert!(json.contains("cell-a"));
        assert!(json.contains("intent.deploy"));
        let text = render(&cells, &counts);
        assert!(text.contains("cell-a"));
        assert!(text.contains("intent.deploy: 2"));
    }
}
