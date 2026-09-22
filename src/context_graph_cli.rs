//! Inspect and query the Universal Context Graph.
use anyhow::{bail, Result};
use clap::Subcommand;
use std::path::Path;

#[derive(Debug, Subcommand)]
pub enum ContextGraphCommands {
    /// Show the most recent context-graph events for this workspace.
    Show {
        /// Maximum events to display.
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
    },
    /// Show nodes/edges related to a specific node id.
    Related {
        /// Node id to expand from.
        #[arg(short, long)]
        node: String,
        /// Expansion depth.
        #[arg(short, long, default_value_t = 2)]
        depth: usize,
    },
    /// Summary statistics for the loaded graph.
    Stats,
    /// Compact the append-only context-graph log.
    Compact,
    /// Ingest external context events from a JSONL file.
    Ingest {
        /// Path to a JSONL file where each line is {"source":"...","label":"...","payload":{...}}.
        file: std::path::PathBuf,
        /// Optional workspace to associate with every ingested event.
        #[arg(short, long)]
        workspace: Option<std::path::PathBuf>,
    },
    /// Sample OS-level context (active window, process list) and record it.
    #[command(name = "sample-os")]
    SampleOs {
        /// Optional user name to associate with the sample.
        #[arg(short, long)]
        user: Option<String>,
    },
}

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
                .map_err(|e| anyhow::anyhow!("cannot read ingest file: {e}"))?;
            let mut count = 0;
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let event: serde_json::Value = serde_json::from_str(line)
                    .map_err(|e| anyhow::anyhow!("invalid ingest JSON: {e}"))?;
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
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "sampled": count,
                }))?
            );
        }
        ContextGraphCommands::Show { limit } => {
            let subgraph = graph.workspace_subgraph(workspace);
            let events: Vec<_> = subgraph
                .nodes
                .into_iter()
                .map(|n| {
                    serde_json::json!({
                        "type": "node",
                        "id": n.id.0,
                        "kind": format!("{:?}", n.kind),
                        "label": n.label,
                        "created_at": n.created_at,
                    })
                })
                .chain(subgraph.edges.into_iter().map(|e| {
                    serde_json::json!({
                        "type": "edge",
                        "id": e.id,
                        "source": e.source.0,
                        "target": e.target.0,
                        "kind": format!("{:?}", e.kind),
                        "created_at": e.created_at,
                    })
                }))
                .collect();
            let out = if events.is_empty() {
                serde_json::json!({
                    "workspace": workspace.display().to_string(),
                    "note": "no context-graph activity recorded for this workspace yet",
                    "events": events,
                })
            } else {
                serde_json::json!({
                    "workspace": workspace.display().to_string(),
                    "event_count": events.len().min(limit),
                    "events": events.into_iter().take(limit).collect::<Vec<_>>(),
                })
            };
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
