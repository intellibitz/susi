//! 5th Open-Core Pillar: Agent Client Protocol (`susi acp`) stdio server and discovery.
//!
//! `susi acp` / `susi acp serve` speaks newline-delimited JSON-RPC 2.0 over
//! stdin/stdout (`AcpStdioTransport`) with zero boot/banner output on stdout
//! and lazy substrate composition on the first `session/prompt` turn so the
//! initial protocol handshake completes in a few milliseconds.

use crate::anyhow::Result;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
pub use susi_cli_defs::acp::AcpCommands;
use susi_core::acp::{
    AcpAdmissionPolicy, AcpContentBlock, AcpDiscoveryDocument, AcpEngine, AcpMessageRole,
    AcpStdioTransport, AcpTurnContext, AcpTurnExecutor, ACP_CANONICAL_PORT,
};
use susi_core::untrusted_content::{enforce_action, wrap_tool_output, ActionClass};

const HISTORY_BUDGET: usize = 16 * 1024;

type AcpMissionRunner =
    Arc<dyn Fn(&str, &Path) -> std::result::Result<String, String> + Send + Sync>;

struct CliAcpExecutor {
    default_workspace: PathBuf,
    substrate_wired: OnceLock<()>,
    runner: AcpMissionRunner,
}

impl CliAcpExecutor {
    fn new(default_workspace: PathBuf) -> Self {
        Self {
            default_workspace,
            substrate_wired: OnceLock::new(),
            runner: Arc::new(|intent, workspace| {
                susi_core::plane_bus::gawd::solve_mission_for_acp(
                    intent,
                    workspace,
                    susi::SUSI_VERSION,
                )
            }),
        }
    }

    #[cfg(test)]
    fn with_runner(default_workspace: PathBuf, runner: AcpMissionRunner) -> Self {
        Self {
            default_workspace,
            substrate_wired: OnceLock::new(),
            runner,
        }
    }

    fn ensure_substrate(&self) {
        self.substrate_wired.get_or_init(|| {
            let substrate = susi_paths::SusiDirs::substrate_home();
            susi_daemon::privacy::wire_mac_policy(&substrate);
            susi_gawd::init_hooks();
            susi_daemon::composition::wire_mcp_serve_substrate(&substrate);
        });
    }

    fn resolve_workspace(&self, session_workspace: Option<&str>) -> PathBuf {
        match session_workspace {
            None | Some("") | Some(".") => self.default_workspace.clone(),
            Some(rel) => self.default_workspace.join(rel),
        }
    }
}

fn gated_prompt(text: &str) -> std::result::Result<String, String> {
    enforce_action(
        &wrap_tool_output("acp:request", text),
        ActionClass::Consequential,
    )
}

fn build_mission_text(ctx: &AcpTurnContext, prompt: String) -> String {
    let mut earlier = String::new();
    let messages = ctx.inference_messages();
    let prior_len = messages.len().saturating_sub(1);
    for m in &messages[..prior_len] {
        let line = match m.role {
            AcpMessageRole::User if gated_prompt(&m.content).is_ok() => {
                format!("Asked: {}\n", m.content)
            }
            AcpMessageRole::Assistant => format!("Answered: {}\n", m.content),
            AcpMessageRole::System | AcpMessageRole::User | AcpMessageRole::Tool => continue,
        };
        earlier.push_str(&line);
    }
    let mut cut = earlier.len().saturating_sub(HISTORY_BUDGET);
    while !earlier.is_char_boundary(cut) {
        cut += 1;
    }
    match &earlier[cut..] {
        "" => prompt,
        kept => format!("Earlier in this session:\n{kept}\nNow:\n{prompt}"),
    }
}

impl AcpTurnExecutor for CliAcpExecutor {
    fn execute_turn(
        &self,
        ctx: &AcpTurnContext,
        cancelled: &AtomicBool,
        emit_delta: &mut dyn FnMut(&str),
    ) -> std::result::Result<Vec<AcpContentBlock>, String> {
        let content = ctx.joined_prompt_text();
        if content.trim().is_empty() {
            return Err("ACP prompt carried no text to run as a mission".to_string());
        }
        let content = build_mission_text(ctx, gated_prompt(&content)?);
        if cancelled.load(Ordering::SeqCst) {
            return Ok(Vec::new());
        }
        if self.substrate_wired.get().is_none() {
            self.ensure_substrate();
        }
        let target_workspace = self.resolve_workspace(ctx.workspace.as_deref());
        let answer = (self.runner)(&content, &target_workspace)
            .map_err(|error| wrap_tool_output("acp:mission-error", &error).redacted_for_sink())?;
        if cancelled.load(Ordering::SeqCst) {
            return Ok(Vec::new());
        }
        let answer = wrap_tool_output("acp:mission-reply", &answer).redacted_for_sink();
        emit_delta(&answer);
        Ok(vec![AcpContentBlock::text(answer)])
    }
}

/// Build the host's canonical `susi-acp-discovery/v1` document for an explicit `port_offset`.
pub(crate) fn discovery_document_with_offset(offset: u16) -> AcpDiscoveryDocument {
    let mut doc = AcpEngine::default().discovery_document();
    doc.agent.version = susi::SUSI_VERSION.to_owned();
    doc.agent.canonical_port = ACP_CANONICAL_PORT.saturating_add(offset);
    doc
}

/// Build the host's canonical `susi-acp-discovery/v1` document, respecting `SUSI_PORT_OFFSET`.
pub(crate) fn host_discovery_document() -> AcpDiscoveryDocument {
    discovery_document_with_offset(susi_paths::ports::env_port_offset())
}

pub(crate) fn format_discovery_human(doc: &AcpDiscoveryDocument) -> String {
    let port = doc.agent.canonical_port;
    format!(
        "SUSI ACP — {} ({})\n  protocol:   {} (compatible: {})\n  agent:      {} v{} ({})\n  http:       http://127.0.0.1:{port}\n  discovery:  {}\n  rpc:        {}\n  stream:     {}\n  stdio:      {} (`susi acp serve`)\n  auth:       {} ({}, realm={:?})\n",
        doc.agent.name,
        doc.schema,
        doc.protocol_version,
        doc.compatible_versions.join(", "),
        doc.agent.id,
        doc.agent.version,
        doc.agent.license,
        doc.endpoints.discovery,
        doc.endpoints.rpc,
        doc.endpoints.stream,
        doc.endpoints.stdio,
        doc.auth.scheme,
        doc.auth.header,
        doc.auth.realm,
    )
}

fn serve_stdio(workspace: &Path) -> Result<()> {
    let engine = AcpEngine::new(
        AcpAdmissionPolicy::default(),
        Arc::new(CliAcpExecutor::new(workspace.to_path_buf())),
    );
    let stdin = io::stdin();
    let stdout = io::stdout();
    match AcpStdioTransport::serve_reader_writer(
        &engine,
        io::BufReader::new(stdin.lock()),
        stdout.lock(),
    ) {
        Ok(_) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(err) => Err(crate::anyhow::Error::from(err)),
    }
}

pub(crate) fn format_discovery_json(doc: &AcpDiscoveryDocument) -> Result<String> {
    Ok(serde_json::to_string_pretty(doc)?)
}

pub fn execute(action: Option<AcpCommands>, workspace: &Path) -> Result<()> {
    match action.unwrap_or(AcpCommands::Serve) {
        AcpCommands::Serve => serve_stdio(workspace),
        AcpCommands::Discover { json } => {
            let doc = host_discovery_document();
            if json {
                println!("{}", format_discovery_json(&doc)?);
            } else {
                print!("{}", format_discovery_human(&doc));
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use susi_core::acp::{
        ACP_DISCOVERY_SCHEMA, ACP_PROTOCOL_VERSION, ACP_RPC_PATH, ACP_STREAM_PATH,
    };

    #[test]
    fn discovery_document_respects_port_offset_and_schema_contract() {
        let base_doc = discovery_document_with_offset(0);
        assert_eq!(base_doc.schema, ACP_DISCOVERY_SCHEMA);
        assert_eq!(base_doc.protocol_version, ACP_PROTOCOL_VERSION);
        assert_eq!(base_doc.agent.version, susi::SUSI_VERSION);
        assert_eq!(base_doc.agent.canonical_port, 9094);
        assert_eq!(base_doc.endpoints.rpc, ACP_RPC_PATH);
        assert_eq!(base_doc.endpoints.stream, ACP_STREAM_PATH);
        assert_eq!(base_doc.endpoints.stdio, "ndjson");

        let shifted = discovery_document_with_offset(200);
        assert_eq!(shifted.agent.canonical_port, 9294);
        let json_text = format_discovery_json(&shifted).expect("valid discovery JSON");
        let parsed: serde_json::Value =
            serde_json::from_str(&json_text).expect("discovery --json must be valid JSON");
        assert_eq!(parsed["schema"], ACP_DISCOVERY_SCHEMA);
        assert_eq!(parsed["agent"]["canonical_port"], 9294);
        assert_eq!(parsed["auth"]["scheme"], "Bearer");
        assert_eq!(parsed["endpoints"]["stream"], ACP_STREAM_PATH);

        let saturated = discovery_document_with_offset(u16::MAX);
        assert_eq!(saturated.agent.canonical_port, u16::MAX);

        let human = format_discovery_human(&shifted);
        assert!(human.contains(ACP_DISCOVERY_SCHEMA), "{human}");
        assert!(human.contains("http://127.0.0.1:9294"), "{human}");
        assert!(human.contains("/acp/stream"), "{human}");
    }

    #[test]
    fn cli_acp_stdio_emits_pure_ndjson_frames_and_executes_session_turns() {
        let executor = CliAcpExecutor::with_runner(
            PathBuf::from("/tmp/acp-cli-test"),
            Arc::new(|intent, ws| Ok(format!("ran in {}: {intent}", ws.display()))),
        );
        // Pre-populate OnceLock so the unit test stays hermetic without touching SusiDirs.
        let _ = executor.substrate_wired.set(());
        let engine = AcpEngine::new(AcpAdmissionPolicy::default(), Arc::new(executor));

        let requests = [
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {"protocolVersion": ACP_PROTOCOL_VERSION}
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "session/new",
                "params": {"workspace": "sub"}
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "session/prompt",
                "params": {"session_id": "acp-sess-000001", "prompt": "verify acp pillar"}
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "session/list",
                "params": {}
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 5,
                "method": "session/close",
                "params": {"session_id": "acp-sess-000001"}
            }),
        ];
        let mut input = String::new();
        for req in &requests {
            input.push_str(&req.to_string());
            input.push('\n');
        }
        let mut output = Vec::new();
        let handled = AcpStdioTransport::serve_reader_writer(
            &engine,
            io::Cursor::new(input.into_bytes()),
            &mut output,
        )
        .unwrap();
        assert_eq!(handled, 5);

        let lines: Vec<serde_json::Value> = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).expect("every stdout line must be JSON-RPC"))
            .collect();
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[0]["result"]["protocol_version"], ACP_PROTOCOL_VERSION);
        assert_eq!(lines[1]["result"]["session_id"], "acp-sess-000001");
        assert!(lines[2]["result"]["output"][0]["text"]
            .as_str()
            .unwrap()
            .contains("ran in /tmp/acp-cli-test/sub: verify acp pillar"));
        assert_eq!(lines[3]["result"]["sessions"].as_array().unwrap().len(), 1);
        assert_eq!(lines[4]["result"]["closed"], true);
    }
}
