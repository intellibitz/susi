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
    let graph = susi_core::context_graph::ContextGraph::global();
    let _ = graph.replay();
    match action.unwrap_or(ContextGraphCommands::Show { limit: 20 }) {
        ContextGraphCommands::Compact => {
            let (old_lines, new_lines) = graph.compact()?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "compacted": true,
                    "old_lines": old_lines,
                    "new_lines": new_lines,
                }))?
            );
        }
        ContextGraphCommands::Ingest { file, workspace } => {
            let text = std::fs::read_to_string(&file)
                .map_err(|e| crate::anyhow::anyhow!("cannot read ingest file: {e}"))?;
            let mut count = 0;
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let event: serde_json::Value = serde_json::from_str(line)
                    .map_err(|e| crate::anyhow::anyhow!("invalid ingest JSON: {e}"))?;
                let source = event["source"].as_str().unwrap_or("unknown");
                let label = event["label"].as_str().unwrap_or("external context");
                let payload = &event["payload"];
                let ws = workspace
                    .as_deref()
                    .or_else(|| event["workspace"].as_str().map(std::path::Path::new));
                let user = event["user"].as_str();
                graph.record_external_context(source, label, payload, ws, user);
                count += 1;
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "ingested": count,
                }))?
            );
        }
        ContextGraphCommands::SampleOs { user } => {
            let count = susi_daemon::context_adapters::sample_os_context_once(
                Some(workspace),
                user.as_deref(),
            );
            let unavailable: serde_json::Map<String, serde_json::Value> =
                susi_daemon::context_adapters::unavailable_os_context()
                    .into_iter()
                    .map(|(source, reason)| (source.to_string(), reason.into()))
                    .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "sampled": count,
                    "os": std::env::consts::OS,
                    "unavailable": unavailable,
                }))?
            );
        }
        ContextGraphCommands::Show { limit } => {
            let out = graph.workspace_show(workspace, limit);
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        ContextGraphCommands::Related { node, depth } => {
            let node_id = susi_core::context_graph::NodeId(node);
            if graph.node(&node_id).is_none() {
                bail!("node {} not found in context graph", node_id.0);
            }
            let subgraph = graph.related(&node_id, depth);
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "start": node_id.0,
                    "depth": depth,
                    "node_count": subgraph.nodes.len(),
                    "edge_count": subgraph.edges.len(),
                    "nodes": subgraph.nodes,
                    "edges": subgraph.edges,
                }))?
            );
        }
        ContextGraphCommands::Stats => {
            let stats = graph.stats();
            println!("{}", serde_json::to_string_pretty(&stats)?);
        }
    }
    Ok(())
}
