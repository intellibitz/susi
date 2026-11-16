//! Inspect and query the Universal Context Graph.
use crate::anyhow::{bail, Result};
use std::path::Path;
pub use susi_cli_defs::context_graph::ContextGraphCommands;

pub fn execute(action: Option<ContextGraphCommands>, workspace: &Path) -> Result<()> {
    let substrate = susi_paths::SusiDirs::substrate_home();
    let _ = std::fs::create_dir_all(&substrate);
    susi_core::context_graph::ContextGraph::init_global_storage(
        substrate.join("context_graph.jsonl"),
    );
    let graph = susi_core::context_graph::ContextGraph::replayed_global();
    let output = match action.unwrap_or(ContextGraphCommands::Show { limit: 20 }) {
        ContextGraphCommands::Compact => graph.compact_report()?,
        ContextGraphCommands::Ingest { file, workspace } => {
            let text = std::fs::read_to_string(&file)
                .map_err(|e| crate::anyhow::anyhow!("cannot read ingest file: {e}"))?;
            let mut count = 0;
            for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
                let event: serde_json::Value = serde_json::from_str(line)
                    .map_err(|e| crate::anyhow::anyhow!("invalid ingest JSON: {e}"))?;
                let target = workspace
                    .as_deref()
                    .or_else(|| event["workspace"].as_str().map(Path::new));
                graph.record_external_event(&event, target);
                count += 1;
            }
            serde_json::json!({"ingested": count})
        }
        ContextGraphCommands::SampleOs { user } => {
            susi_daemon::context_adapters::sample_os_report(Some(workspace), user.as_deref())
        }
        ContextGraphCommands::Show { limit } => graph.workspace_show(workspace, limit),
        ContextGraphCommands::Related { node, depth } => {
            let node_id = susi_core::context_graph::NodeId(node);
            if graph.node(&node_id).is_none() {
                bail!("node {} not found in context graph", node_id.0);
            }
            let subgraph = graph.related(&node_id, depth);
            serde_json::json!({
                "start": node_id.0,
                "depth": depth,
                "node_count": subgraph.nodes.len(),
                "edge_count": subgraph.edges.len(),
                "nodes": subgraph.nodes,
                "edges": subgraph.edges,
            })
        }
        ContextGraphCommands::Stats => {
            println!("{}", serde_json::to_string_pretty(&graph.stats())?);
            return Ok(());
        }
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
