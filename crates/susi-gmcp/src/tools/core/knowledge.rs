//! CoreTools handlers for agent ranking, semantic/AST search, RAG, context graph, browser capture, and IPC.

use std::path::Path;

use super::CoreTools;
#[cfg(feature = "tools-rich")]
use super::TOPIC_BROWSER_CAPTURE;
use crate::susi_core::broker::{IpcBroker, PermissionScope};
use crate::susi_core::context_graph::ContextGraph;
use crate::susi_core::plane_bus::gawd_hooks;
use crate::susi_error::{EaiError, EaiResult};
use crate::tool_registry::GmcpClient;
use crate::tools::args;
#[cfg(feature = "tools-rich")]
use crate::tools::helpers::secure_external_url;

impl CoreTools {
    crate::tool_def!(meta_scout_agents_tool_attr {
        name: "meta_scout_agents",
        description: "Discover agent capabilities from connected remotes",
        input_schema: args::no_args(),
    });
    pub fn meta_scout_agents(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let remotes = GmcpClient::list_external_tools();
        let mut report = "Discovered Meta-Agent Capabilities:\n\n".to_string();
        for r in remotes {
            if r.name.contains("agent") || r.name.contains("swarm") {
                report.push_str(&format!("- [REMOTE] {}: {}\n", r.name, r.description));
            }
        }
        Ok(report)
    }

    crate::tool_def!(meta_rank_agents_tool_attr {
        name: "meta_rank_agents",
        description: "Report current agent expertise hierarchy",
        input_schema: args::no_args(),
    });
    pub fn meta_rank_agents(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let registry = crate::susi_core::AgentMetaRegistry::global();
        let agents = registry.list_agents();
        let mut report = "SUSI Expertise Hierarchy:\n\n".to_string();
        for a in agents {
            report.push_str(&format!(
                "- [AGENT] {} (Base Rank: {:.2}): {}\n",
                a.name, a.base_rank, a.description
            ));
        }
        Ok(report)
    }

    #[cfg(feature = "tools-rich")]
    crate::tool_def!(ast_analyze_tool_attr {
        name: "ast_analyze",
        description:
            "Structural AST analysis of Rust source (syn); other languages report size only",
        input_schema: args::typed::<args::AstAnalyze>(),
    });
    #[cfg(feature = "tools-rich")]
    pub fn ast_analyze(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let path_s = arg.get("path").and_then(|v| v.as_str());
        let code_s = arg.get("code").and_then(|v| v.as_str());

        let code = if let Some(c) = code_s {
            c.to_string()
        } else if let Some(p) = path_s {
            let path = secure_path(workspace, p)?;
            read_file_nofollow(&path)?
        } else {
            return Err(EaiError::protocol(
                "Usage: ast_analyze {path: <path>} OR {code: <code>}",
            ));
        };

        let ext = path_s
            .and_then(|p| Path::new(p).extension())
            .and_then(|e| e.to_str())
            .unwrap_or("rs");
        let mut report = format!("AST Analysis ({}): {} bytes\n", ext, code.len());

        // Structural analysis: Rust only (susi-vendor-syn). Other languages
        // get the size line and an explicit "not parsed" note — no parser
        // for them is linked, so none is claimed.
        if ext == "rs" {
            match susi_vendor_syn::outline_checked(&code).map_err(EaiError::protocol)? {
                Some(items) => {
                    report.push_str(&format!(
                        "Converged AST (syn): {} top-level items.\n",
                        items.len()
                    ));
                    for (kind, name) in items.iter().take(5) {
                        let label = match kind {
                            susi_vendor_syn::ItemKind::Function => "Function",
                            susi_vendor_syn::ItemKind::Struct => "Struct",
                            susi_vendor_syn::ItemKind::Enum => "Enum",
                            susi_vendor_syn::ItemKind::Other => continue,
                        };
                        report.push_str(&format!("  - {label}: {name}\n"));
                    }
                }
                None => report.push_str("Rust source did not parse.\n"),
            }
        } else {
            report.push_str(&format!(
                "Structural parsing is available for Rust only; `.{ext}` was not parsed.\n"
            ));
        }

        Ok(report)
    }

    #[cfg(feature = "tools-rich")]
    crate::tool_def!(semantic_search_tool_attr {
        name: "semantic_search",
        description:
            "Unified BM25 recall over .susi memory, experience, audit log, and workspace files",
        input_schema: args::typed::<args::Query>(),
    });
    #[cfg(feature = "tools-rich")]
    pub fn semantic_search(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let query_str = arg
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::protocol("Missing query"))?;

        let hits = super::semantic_index::SemanticIndex::search(workspace, query_str, 5)?;

        let mut out = format!("Semantic recall results for '{}':\n", query_str);
        if hits.is_empty() {
            out.push_str("No matches found in the unified index.\n");
        }
        for hit in hits {
            out.push_str(&format!(
                "- [{:.3}] ({}:{}) {}\n",
                hit.score, hit.source, hit.doc_id, hit.snippet
            ));
        }
        Ok(out)
    }

    crate::tool_def!(sandbox_exec_tool_attr {
        name: "sandbox_exec",
        description: "Isolated Docker execution via bollard",
        input_schema: args::typed::<args::SandboxExec>(),
    });
    pub fn sandbox_exec(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let cmd = arg
            .get("cmd")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::protocol("Missing cmd"))?;

        gawd_hooks::audit_action("sandbox_exec", cmd, workspace)?;

        Self::shared_runtime()?
            .block_on(async {
                crate::susi_sandbox::manager::SandboxManager::execute_in_docker(cmd).await
            })
            .map_err(|e| {
                EaiError::process(format!(
                    "[CAPABILITY_GAP] Docker execution failed: {e}. \
                 Start the susi-sandbox service and ensure Docker is running."
                ))
            })
    }

    #[cfg(feature = "tools-rich")]
    crate::tool_def!(browser_automate_tool_attr {
        name: "browser_automate",
        description: "DOM access and web automation via headless Chrome",
        input_schema: args::typed::<args::BrowserAutomate>(),
    });
    #[cfg(feature = "tools-rich")]
    pub fn browser_automate(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let url = arg
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::protocol("Missing url"))?;
        let validated_url = secure_external_url(url)?;
        let url = validated_url.as_str();
        let _ = workspace;
        let bus = crate::susi_core::plane_bus::PlaneBus::global();
        if bus.is_wired(TOPIC_BROWSER_CAPTURE) {
            let capture = bus
                .request(TOPIC_BROWSER_CAPTURE, serde_json::json!({ "url": url }))
                .map_err(EaiError::process)?;
            return serde_json::to_string(&capture).map_err(|e| {
                EaiError::process(format!("browser plug reply is not JSON text: {e}"))
            });
        }
        Err(EaiError::process(format!(
            "[CAPABILITY_GAP] Headless Chrome automation for {url} lives in susi-commercial-apps (susi-desktop); start the susi-plug-host beside this core."
        )))
    }

    #[cfg(feature = "tools-rich")]
    crate::tool_def!(rag_query_tool_attr {
        name: "rag_query",
        description:
            "Vector recall over the unified .susi index via local fastembed cosine similarity",
        input_schema: args::typed::<args::Query>(),
    });
    #[cfg(feature = "tools-rich")]
    pub fn rag_query(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let query = arg
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::protocol("Missing query"))?;

        let hits = super::semantic_index::SemanticIndex::vector_recall(workspace, query, 3)?;

        let mut out = "Top 3 Semantic Matches:\n".to_string();
        if hits.is_empty() {
            out.push_str("No embedded documents in the unified index yet.\n");
        }
        for hit in hits {
            out.push_str(&format!(
                "- [Score: {:.3}] ({}:{}) {}\n",
                hit.score, hit.source, hit.doc_id, hit.snippet
            ));
        }
        Ok(out)
    }

    crate::tool_def!(context_graph_query_tool_attr {
        name: "context_graph_query",
        description:
            "Query the Universal Context Graph for the current workspace or a specific node id",
        input_schema: args::typed::<args::ContextGraphQuery>(),
    });
    pub fn context_graph_query(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let graph = crate::susi_core::context_graph::ContextGraph::global();
        let _ = graph.replay();
        if let Some(node_id) = arg.get("node_id").and_then(|v| v.as_str()) {
            let depth = arg.get("depth").and_then(|v| v.as_u64()).unwrap_or(2) as usize;
            let node = crate::susi_core::context_graph::NodeId(node_id.to_string());
            let subgraph = graph.related(&node, depth);
            Ok(serde_json::to_string_pretty(&subgraph).unwrap_or_else(|_| "{}".to_string()))
        } else {
            let events = graph.workspace_events(workspace);
            Ok(serde_json::to_string_pretty(&serde_json::json!({
                "workspace": workspace.display().to_string(),
                "event_count": events.len(),
                "events": events,
            }))
            .unwrap_or_else(|_| "{}".to_string()))
        }
    }

    crate::tool_def!(context_graph_ingest_tool_attr {
        name: "context_graph_ingest",
        description: "Ingest an external context event into the Universal Context Graph. Args: source (string), label (string), payload (object), optional workspace, optional user.",
        input_schema: args::typed::<args::ContextGraphIngest>(),
    });
    pub fn context_graph_ingest(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let source = arg
            .get("source")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let label = arg
            .get("label")
            .and_then(|v| v.as_str())
            .unwrap_or("external context");
        let payload = arg.get("payload").unwrap_or(&serde_json::Value::Null);
        let ws = arg
            .get("workspace")
            .and_then(|v| v.as_str())
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| workspace.to_path_buf());
        let user = arg.get("user").and_then(|v| v.as_str());
        let id =
            ContextGraph::global().record_external_context(source, label, payload, Some(&ws), user);
        Ok(serde_json::to_string_pretty(&serde_json::json!({
            "ingested": true,
            "node_id": id.0,
        }))
        .unwrap_or_else(|_| "{}".to_string()))
    }

    crate::tool_def!(context_graph_compact_tool_attr {
        name: "context_graph_compact",
        description: "Compact the append-only Universal Context Graph log",
        input_schema: args::no_args(),
    });
    pub fn context_graph_compact(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let graph = ContextGraph::global();
        let _ = graph.replay();
        let (old_lines, new_lines) = graph.compact()?;
        Ok(serde_json::to_string_pretty(&serde_json::json!({
            "compacted": true,
            "old_lines": old_lines,
            "new_lines": new_lines,
        }))
        .unwrap_or_else(|_| "{}".to_string()))
    }

    crate::tool_def!(ipc_grant_tool_attr {
        name: "ipc_grant",
        description: "Grant an inter-app permission scope. Args: grantor, grantee, resource, action, optional ttl_secs.",
        input_schema: args::typed::<args::IpcGrant>(),
    });
    pub fn ipc_grant(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let grantor = arg
            .get("grantor")
            .and_then(|v| v.as_str())
            .unwrap_or("susi");
        let grantee = arg
            .get("grantee")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::governance("ipc_grant requires grantee"))?;
        let resource = arg
            .get("resource")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::governance("ipc_grant requires resource"))?;
        let action = arg
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::governance("ipc_grant requires action"))?;
        let ttl_secs = arg.get("ttl_secs").and_then(|v| v.as_u64());
        let scope = PermissionScope::new(resource, action);
        let grant = IpcBroker::global().grant_and_record(
            grantor,
            grantee,
            scope,
            ttl_secs,
            Some(workspace),
        );
        Ok(serde_json::to_string_pretty(&grant).unwrap_or_else(|_| "{}".into()))
    }

    crate::tool_def!(ipc_request_tool_attr {
        name: "ipc_request",
        description: "Request an inter-app permission (opens negotiation). Args: requester, resource, action, optional grantor, optional ttl_secs.",
        input_schema: args::typed::<args::IpcRequest>(),
    });
    pub fn ipc_request(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let requester = arg
            .get("requester")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::governance("ipc_request requires requester"))?;
        let grantor = arg
            .get("grantor")
            .and_then(|v| v.as_str())
            .unwrap_or("susi");
        let resource = arg
            .get("resource")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::governance("ipc_request requires resource"))?;
        let action = arg
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::governance("ipc_request requires action"))?;
        let ttl_secs = arg.get("ttl_secs").and_then(|v| v.as_u64());
        let scope = PermissionScope::new(resource, action);
        let req = IpcBroker::global().request(requester, grantor, scope, ttl_secs);
        Ok(serde_json::to_string_pretty(&req).unwrap_or_else(|_| "{}".into()))
    }

    crate::tool_def!(ipc_negotiate_tool_attr {
        name: "ipc_negotiate",
        description:
            "Approve or deny a pending permission request. Args: request_id, actor, approve (bool).",
        input_schema: args::typed::<args::IpcNegotiate>(),
    });
    pub fn ipc_negotiate(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let request_id = arg
            .get("request_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::governance("ipc_negotiate requires request_id"))?;
        let actor = arg.get("actor").and_then(|v| v.as_str()).unwrap_or("susi");
        let approve = arg
            .get("approve")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let resolved = IpcBroker::global()
            .negotiate(request_id, actor, approve, Some(workspace))
            .map_err(EaiError::governance)?;
        Ok(serde_json::to_string_pretty(&resolved).unwrap_or_else(|_| "{}".into()))
    }

    crate::tool_def!(ipc_send_tool_attr {
        name: "ipc_send",
        description: "Send a broker message (requires dispatch grant unless from=susi). Args: from, to, topic, payload.",
        input_schema: args::typed::<args::IpcSend>(),
    });
    pub fn ipc_send(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let from = arg
            .get("from")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::governance("ipc_send requires from"))?;
        let to = arg
            .get("to")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::governance("ipc_send requires to"))?;
        let topic = arg
            .get("topic")
            .and_then(|v| v.as_str())
            .unwrap_or("message");
        let payload = arg
            .get("payload")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let msg = IpcBroker::global()
            .send(from, to, topic, payload)
            .map_err(EaiError::governance)?;
        Ok(serde_json::to_string_pretty(&msg).unwrap_or_else(|_| "{}".into()))
    }

    crate::tool_def!(ipc_receive_tool_attr {
        name: "ipc_receive",
        description: "Receive broker messages for an identity. Args: recipient, optional limit.",
        input_schema: args::typed::<args::IpcReceive>(),
    });
    pub fn ipc_receive(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let recipient = arg
            .get("recipient")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::governance("ipc_receive requires recipient"))?;
        let limit = arg.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
        let msgs = IpcBroker::global().receive(recipient, limit);
        Ok(serde_json::to_string_pretty(&serde_json::json!({
            "recipient": recipient,
            "messages": msgs,
        }))
        .unwrap_or_else(|_| "{}".into()))
    }
}
