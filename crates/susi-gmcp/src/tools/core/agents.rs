//! CoreTools handlers for A2A delegation, managed agents/frameworks/tasks, and MCP server configuration.

use std::path::Path;

use super::CoreTools;
use crate::susi_core::plane_bus::{agents, gawd_hooks, tools as plane_tools};
use crate::susi_core::task_manager::SwarmTaskManager;
use crate::susi_error::{EaiError, EaiResult};
use crate::tool_registry::GmcpClient;
use crate::tools::args;
use crate::tools::forward::forwarded_tools;
use crate::tools::helpers::external_agent_control;

// Task controls share their permissive ID contract (object, bare string or
// missing ID), but retain the selected manager operation and result wording.
fn task_control(
    arg: &serde_json::Value,
    action: fn(&SwarmTaskManager, &str) -> bool,
    completed: &str,
) -> EaiResult<String> {
    let id = arg
        .get("task_id")
        .and_then(|v| v.as_str())
        .or_else(|| arg.as_str())
        .unwrap_or("")
        .trim();
    if action(SwarmTaskManager::global(), id) {
        Ok(format!("Task '{}' {}.", id, completed))
    } else {
        Err(EaiError::protocol(format!("Task '{}' not found.", id)))
    }
}

impl CoreTools {
    /// Resolve an `a2a_delegate` target to `http://{ip}:{port}`. Only
    /// loopback and verified roster members resolve — an agent-supplied
    /// URL otherwise becomes an SSRF primitive posting signed requests
    /// at arbitrary hosts.
    pub(crate) fn a2a_peer_url(peer: &str) -> EaiResult<String> {
        use crate::susi_config::cluster_key;
        use std::net::IpAddr;
        // Effective A2A port — canonical + port_offset. Used both for the
        // local "self" route and as the same-config guess for bare hosts.
        let a2a_port = crate::susi_config::SusiConfig::load_global()
            .map(|c| c.a2a_http_port())
            .unwrap_or(susi_paths::ports::A2A_HTTP);
        let p = peer.trim();
        if p.eq_ignore_ascii_case("self") || p == "localhost" || p == "127.0.0.1" || p == "::1" {
            return Ok(format!("http://127.0.0.1:{a2a_port}"));
        }
        // Split an explicit http(s) URL into host(+port); a bare host[:port]
        // gets the canonical A2A port.
        let stripped = p
            .strip_prefix("http://")
            .or_else(|| p.strip_prefix("https://"))
            .unwrap_or(p);
        let hostport = stripped.split('/').next().unwrap_or("");
        let (host, port) =
            hostport
                .rsplit_once(':')
                .map_or((hostport, a2a_port), |(h, ps)| match ps.parse::<u16>() {
                    Ok(n) => (h, n),
                    Err(_) => (hostport, a2a_port),
                });
        // Operator-configured external A2A agents form a second, explicit
        // allowlist: `external_peer_agents` entries with protocol "a2a" are
        // reachable by name (resolving to their api_base) or by a target
        // URL matching a configured api_base's host[:port]. Runs before the
        // IpAddr parse because configured peers may use DNS names.
        if let Ok(cfg) = crate::susi_config::SusiConfig::load_global() {
            for spec in cfg.external_peer_agents() {
                if spec.protocol != "a2a" || spec.api_base.is_empty() {
                    continue;
                }
                if spec.name == p {
                    return Ok(spec.api_base.trim_end_matches('/').to_string());
                }
                let base = spec.api_base.trim_end_matches('/');
                let base_host = base
                    .strip_prefix("http://")
                    .or_else(|| base.strip_prefix("https://"))
                    .unwrap_or(base)
                    .split('/')
                    .next()
                    .unwrap_or("");
                let (bh, bp) = base_host
                    .rsplit_once(':')
                    .map_or((base_host, a2a_port), |(h, ps)| {
                        ps.parse::<u16>().map_or((base_host, a2a_port), |n| (h, n))
                    });
                if host == bh && port == bp {
                    return Ok(base.to_string());
                }
            }
        }
        let ip: IpAddr = host
            .parse()
            .map_err(|_| EaiError::protocol(format!("a2a_delegate: bad peer address '{p}'")))?;
        if ip.is_loopback() {
            // The request carries the host bearer token: only our own A2A
            // port may receive it, never another local listener (another
            // user's process would harvest the token).
            if port != a2a_port {
                return Err(EaiError::authorization(format!(
                    "a2a_delegate: loopback delegation is limited to the local A2A port {a2a_port}"
                )));
            }
            return Ok(format!("http://{ip}:{port}"));
        }
        // Verified roster members only: the row's `address` is
        // `{ip}:{gmcp_port}` — the A2A surface is the same host on the
        // canonical contract port.
        // A member's A2A surface is its registered GMCP HTTP port + 1 (the
        // contract's fixed shape under any port offset). The bearer token
        // goes to that port only, not to any port the caller names.
        let member_a2a_port = |n: &serde_json::Value| {
            n.get("address")
                .and_then(|v| v.as_str())
                .and_then(|a| a.rsplit_once(':'))
                .and_then(|(_, p)| p.parse::<u16>().ok())
                .map(|p| p.saturating_add(1))
        };
        let member = cluster_key::config_json_rows("peers.json").iter().any(|n| {
            (n.get("node_id").and_then(|v| v.as_str()) == Some(peer)
                || n.get("address")
                    .and_then(|v| v.as_str())
                    .and_then(|a| a.split(':').next())
                    .and_then(|h| h.parse::<IpAddr>().ok())
                    .is_some_and(|pip| pip == ip))
                && n.get("admission").and_then(|v| v.as_str()) == Some("explicit")
                && (port == a2a_port || member_a2a_port(n) == Some(port))
        });
        if !member {
            return Err(EaiError::authorization(format!(
                "a2a_delegate: '{p}' is not a verified cluster member or configured a2a external_peer_agent — delegation is allowlist-gated (SSRF guard)"
            )));
        }
        // A node_id lookup still resolves to the member's registered IP.
        let resolved_ip = cluster_key::config_json_rows("peers.json")
            .iter()
            .find_map(|n| {
                if n.get("node_id").and_then(|v| v.as_str()) == Some(peer) {
                    n.get("address")
                        .and_then(|v| v.as_str())
                        .and_then(|a| a.split(':').next())
                        .map(str::to_string)
                } else {
                    None
                }
            })
            .unwrap_or_else(|| ip.to_string());
        Ok(format!("http://{resolved_ip}:{port}"))
    }

    crate::tool_def!(a2a_delegate_tool_attr {
        name: "a2a_delegate",
        description: "Delegate a task to a remote A2A agent endpoint (susi peer or external). Args: {peer: \"self\" | node_id | ip | http://ip[:port], message: string}. Roster-gated: only loopback or verified cluster members resolve. Every request carries this node's Ed25519 request signature, which is how bound members authorize us; the host Bearer token is sent only to this node's own A2A port. Returns the completed task's agent reply text.",
        input_schema: args::typed::<args::A2aDelegate>(),
    });
    pub fn a2a_delegate(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        gawd_hooks::audit_action("a2a_delegate", &arg.to_string(), workspace)?;
        let peer = arg
            .get("peer")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::protocol("a2a_delegate requires 'peer'".to_string()))?;
        let message = arg
            .get("message")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::protocol("a2a_delegate requires 'message'".to_string()))?;
        let message = crate::susi_core::untrusted_content::enforce_action(
            &crate::susi_core::untrusted_content::wrap_tool_output("a2a:delegate-request", message),
            crate::susi_core::untrusted_content::ActionClass::Consequential,
        )
        .map_err(EaiError::governance)?;
        let url = Self::a2a_peer_url(peer)?;
        let body = crate::susi_core::a2a_wire::message_send_request(&message);
        let body_bytes = serde_json::to_vec(&body)
            .map_err(|e| EaiError::protocol(format!("a2a_delegate: encode: {e}")))?;
        let endpoint = format!("{url}/");
        let mut headers: Vec<(String, String)> = vec![
            ("content-type".into(), "application/json".into()),
            (
                "a2a-version".into(),
                crate::susi_core::a2a_wire::A2A_VERSION.to_string(),
            ),
        ];
        // The host token is for our own A2A surface (loopback) only; peers
        // authorize us by the member signature below, and a configured
        // third-party agent must never receive it.
        let cfg = crate::susi_config::SusiConfig::load_global().ok();
        let local_a2a = format!(
            "http://127.0.0.1:{}",
            cfg.as_ref()
                .map_or(susi_paths::ports::A2A_HTTP, |c| c.a2a_http_port())
        );
        let token = if url == local_a2a || url.starts_with("http://[::1]:") {
            cfg.map(|c| c.api_auth_token()).unwrap_or_default()
        } else {
            String::new()
        };
        if !token.is_empty() {
            headers.push(("authorization".into(), format!("Bearer {token}")));
        }
        // Member signature over `susi-peer-req-v2:{node}:{ts}:{nonce}:POST:/:{sha256(body)}`
        // — a bound member's receiver authorizes us without the bearer.
        headers.extend(crate::susi_core::mcp_client::signed_headers(
            "POST",
            &endpoint,
            &body_bytes,
        ));
        let refs: Vec<(&str, &str)> = headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        // 120s body timeout: fleet missions can take minutes; connect stays
        // the transport default (15s min of timeout).
        let call = susi_http_transport::http_call_with_body(
            "POST",
            &endpoint,
            &refs,
            Some(&body_bytes),
            120,
            0,
        )
        .map_err(|e| EaiError::network(format!("a2a_delegate to {url}: {e}")))?;
        if !(200..300).contains(&call.status) {
            return Err(EaiError::network(format!(
                "a2a_delegate to {url}: HTTP {}",
                call.status
            )));
        }
        let bytes = call
            .into_bytes(16 * 1024 * 1024)
            .map_err(|e| EaiError::network(format!("a2a_delegate read {url}: {e}")))?;
        let text = String::from_utf8(bytes)
            .map_err(|e| EaiError::network(format!("a2a_delegate read {url}: {e}")))?;
        let doc: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| EaiError::protocol(format!("a2a_delegate: bad JSON-RPC reply: {e}")))?;
        match crate::susi_core::a2a_wire::reply_summary(&doc) {
            Ok(reply) => crate::susi_core::untrusted_content::enforce_action(
                &crate::susi_core::untrusted_content::wrap_tool_output(
                    "a2a:delegate-reply",
                    &reply,
                ),
                crate::susi_core::untrusted_content::ActionClass::Consequential,
            )
            .map_err(EaiError::governance),
            Err(error) => {
                let safe = crate::susi_core::untrusted_content::wrap_tool_output(
                    "a2a:delegate-error",
                    &error,
                )
                .redacted_for_sink();
                Err(EaiError::network(format!("a2a_delegate {safe}")))
            }
        }
    }

    crate::tool_def!(agents_list_tool_attr {
        name: "agents_list",
        description: "List managed external executors and local setup readiness",
        input_schema: args::no_args(),
    });
    pub fn agents_list(_arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let agents = agents::external_list(workspace, "execution");
        serde_json::to_string(&agents).map_err(|e| EaiError::protocol(e.to_string()))
    }

    crate::tool_def!(agents_run_tool_attr {
        name: "agents_run",
        description: "Launch an external agent task; returns a durable task ID for status, logs and cancellation. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace).",
        input_schema: args::typed::<args::AgentsRun>(),
    });
    pub fn agents_run(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        // The CLI runs this in its working directory; the socket
        // takes an optional, MAC-policy-admitted `workspace`.
        let workspace =
            &crate::tools::forward::policy_workspace(arg, "workspace", workspace, true)?;
        let agent = arg
            .get("agent")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::protocol("agent is required"))?;
        let prompt = arg
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::protocol("prompt is required"))?;
        let audit = format!("{agent}: {prompt}");
        gawd_hooks::audit_action("agents_run", &audit, workspace)?;
        let run = agents::external_run(workspace, "execution", agent, prompt)
            .map_err(EaiError::process)?;
        serde_json::to_string(&run).map_err(|e| EaiError::protocol(e.to_string()))
    }

    crate::tool_def!(agents_tasks_tool_attr {
        name: "agents_tasks",
        description: "List persisted external agent tasks for this workspace. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace).",
        input_schema: args::typed::<args::WorkspaceView>(),
    });
    pub fn agents_tasks(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        // The CLI runs this in its working directory; the socket
        // takes an optional, MAC-policy-admitted `workspace`.
        let workspace =
            &crate::tools::forward::policy_workspace(arg, "workspace", workspace, false)?;
        let runs = agents::external_runs(workspace, "execution");
        serde_json::to_string(&runs).map_err(|e| EaiError::protocol(e.to_string()))
    }

    forwarded_tools! {
        agents_status(agents_status_tool_attr, args::typed::<args::ExternalTaskStatus>()),
        "Inspect an external task; refresh=true recovers cloud status after worker loss. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace).",
        |arg, workspace| external_agent_control(arg, workspace, "status");

        agents_cancel(agents_cancel_tool_attr, args::typed::<args::ExternalTaskId>()),
        "Request cancellation of a managed external agent task. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace).",
        |arg, workspace| external_agent_control(arg, workspace, "cancel");

        agents_logs(agents_logs_tool_attr, args::typed::<args::ExternalTaskLogs>()),
        "Read the last 64 KiB of external agent output; stderr=true selects errors. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace).",
        |arg, workspace| external_agent_control(arg, workspace, "logs");

        agents_send(agents_send_tool_attr, args::typed::<args::ExternalTaskSend>()),
        "Send a follow-up message to an existing cloud agent task. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace).",
        |arg, workspace| external_agent_control(arg, workspace, "send");
    }

    crate::tool_def!(frameworks_list_tool_attr {
        name: "frameworks_list",
        description: "List managed agent frameworks/engines and local setup readiness",
        input_schema: args::no_args(),
    });
    pub fn frameworks_list(_arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let engines = agents::external_list(workspace, "framework");
        serde_json::to_string(&engines).map_err(|e| EaiError::protocol(e.to_string()))
    }

    crate::tool_def!(frameworks_run_tool_attr {
        name: "frameworks_run",
        description: "Launch an agent-framework task; returns a durable task ID. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace).",
        input_schema: args::typed::<args::FrameworksRun>(),
    });
    pub fn frameworks_run(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        // The CLI runs this in its working directory; the socket
        // takes an optional, MAC-policy-admitted `workspace`.
        let workspace =
            &crate::tools::forward::policy_workspace(arg, "workspace", workspace, true)?;
        let engine = arg
            .get("engine")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::protocol("engine is required"))?;
        let prompt = arg
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::protocol("prompt is required"))?;
        let run = agents::external_run(workspace, "framework", engine, prompt)
            .map_err(EaiError::process)?;
        serde_json::to_string(&run).map_err(|e| EaiError::protocol(e.to_string()))
    }

    crate::tool_def!(frameworks_tasks_tool_attr {
        name: "frameworks_tasks",
        description: "List persisted agent-framework tasks for this workspace. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace).",
        input_schema: args::typed::<args::WorkspaceView>(),
    });
    pub fn frameworks_tasks(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        // The CLI runs this in its working directory; the socket
        // takes an optional, MAC-policy-admitted `workspace`.
        let workspace =
            &crate::tools::forward::policy_workspace(arg, "workspace", workspace, false)?;
        let runs = agents::external_runs(workspace, "framework");
        serde_json::to_string(&runs).map_err(|e| EaiError::protocol(e.to_string()))
    }

    forwarded_tools! {
        frameworks_status(frameworks_status_tool_attr, args::typed::<args::ExternalTaskStatus>()),
        "Inspect an agent-framework task. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace).",
        |arg, workspace| external_agent_control(arg, workspace, "status");

        frameworks_cancel(frameworks_cancel_tool_attr, args::typed::<args::ExternalTaskId>()),
        "Request cancellation of a managed agent-framework task. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace).",
        |arg, workspace| external_agent_control(arg, workspace, "cancel");

        frameworks_logs(frameworks_logs_tool_attr, args::typed::<args::ExternalTaskLogs>()),
        "Read the last 64 KiB of agent-framework output; stderr=true selects errors. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace).",
        |arg, workspace| external_agent_control(arg, workspace, "logs");
    }

    crate::tool_def!(tasks_list_tool_attr {
        name: "tasks_list",
        description: "List active and historical swarm tasks with liveness telemetry",
        input_schema: args::no_args(),
    });
    pub fn tasks_list(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let tasks = crate::susi_core::task_manager::SwarmTaskManager::global().list_tasks();
        serde_json::to_string_pretty(&tasks).map_err(|e| EaiError::protocol(e.to_string()))
    }

    forwarded_tools! {
        tasks_pause(tasks_pause_tool_attr, args::typed::<args::SwarmTaskId>()),
        "Pause a running task by task_id",
        |arg, _workspace| task_control(arg, SwarmTaskManager::pause_task, "paused");

        tasks_resume(tasks_resume_tool_attr, args::typed::<args::SwarmTaskId>()),
        "Resume a paused task by task_id",
        |arg, _workspace| task_control(arg, SwarmTaskManager::resume_task, "resumed");

        tasks_kill(tasks_kill_tool_attr, args::typed::<args::SwarmTaskId>()),
        "Kill a running or stalled task by task_id",
        |arg, _workspace| task_control(arg, SwarmTaskManager::kill_task, "killed");
    }

    crate::tool_def!(mcp_registry_tool_attr {
        name: "mcp_registry",
        description: "Interrogate global MCP registry and benchmark servers",
        input_schema: args::no_args(),
    });
    pub fn mcp_registry(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let entries = GmcpClient::autonomous_web_scout();
        Ok(format!(
            "Global MCP Substrate Roster (Count: {})\n\n{}",
            entries.len(),
            entries.join("\n")
        ))
    }

    crate::tool_def!(mcp_configure_tool_attr {
        name: "mcp_configure",
        description: "Configure external MCP server",
        input_schema: args::typed::<args::McpConfigure>(),
    });
    pub fn mcp_configure(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let name = arg
            .get("name")
            .and_then(|v| v.as_str())
            .or_else(|| arg.as_str().and_then(|s| s.split_whitespace().next()));
        let package = arg
            .get("package")
            .and_then(|v| v.as_str())
            .or_else(|| arg.as_str().and_then(|s| s.split_whitespace().nth(1)));

        if let Some(n) = name {
            let p = package.unwrap_or(n);
            let res = GmcpClient::auto_configure_server(n, p)?;
            Ok(format!("MCP Server '{n}' configuration status: {res}"))
        } else {
            Err(EaiError::protocol(
                "Usage: mcp_configure {name: <name>, package: <package>}",
            ))
        }
    }

    crate::tool_def!(leading_mcp_list_tool_attr {
        name: "leading_mcp_list",
        description: "List top MCP tool servers and enablement/readiness",
        input_schema: args::no_args(),
    });
    pub fn leading_mcp_list(_arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let rows = plane_tools::leading_mcp_list(workspace);
        serde_json::to_string(&rows).map_err(|e| EaiError::protocol(e.to_string()))
    }

    crate::tool_def!(leading_mcp_enable_tool_attr {
        name: "leading_mcp_enable",
        description: "Enable a leading MCP server into ~/.susi/mcp_config.json",
        input_schema: args::typed::<args::LeadingMcpServer>(),
    });
    pub fn leading_mcp_enable(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let server = arg
            .get("server")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::protocol("server is required"))?;
        let cfg = plane_tools::leading_mcp_enable(workspace, server).map_err(EaiError::process)?;
        serde_json::to_string(&cfg).map_err(|e| EaiError::protocol(e.to_string()))
    }

    crate::tool_def!(leading_mcp_disable_tool_attr {
        name: "leading_mcp_disable",
        description: "Disable a leading MCP server from ~/.susi/mcp_config.json",
        input_schema: args::typed::<args::LeadingMcpServer>(),
    });
    pub fn leading_mcp_disable(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let server = arg
            .get("server")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::protocol("server is required"))?;
        let removed = plane_tools::leading_mcp_disable(workspace, server)
            .map_err(EaiError::process)?
            .get("removed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Ok(serde_json::json!({ "server": server, "removed": removed }).to_string())
    }

    crate::tool_def!(agent_register_tool_attr {
        name: "agent_register",
        description: "Dynamically register a new agent profile",
        input_schema: args::typed::<args::AgentRegister>(),
    });
    pub fn agent_register(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let name = arg.get("name").and_then(|v| v.as_str());
        let desc = arg.get("description").and_then(|v| v.as_str());
        let cats = arg.get("categories").and_then(|v| v.as_str());

        if let (Some(n), Some(d), Some(c)) = (name, desc, cats) {
            let profile = crate::susi_core::AgentProfile {
                name: n.to_string(),
                description: d.to_string(),
                categories: c.split(',').map(|s| s.trim().to_string()).collect(),
                semantic_anchors: Vec::new(),
                base_rank: 0.8,
                is_core: false,
            };
            crate::susi_core::AgentMetaRegistry::global().register_agent(profile);
            Ok(format!("Successfully registered agent: {}", n))
        } else {
            let arg_s = arg.as_str().unwrap_or("");
            let parts: Vec<&str> = arg_s.splitn(3, ' ').collect();
            if parts.len() < 3 {
                return Err(EaiError::protocol(
                    "Usage: agent_register {name, description, categories}",
                ));
            }

            let profile = crate::susi_core::AgentProfile {
                name: parts[0].to_string(),
                description: parts[1].to_string(),
                categories: parts[2].split(',').map(|s| s.trim().to_string()).collect(),
                semantic_anchors: Vec::new(),
                base_rank: 0.8,
                is_core: false,
            };

            crate::susi_core::AgentMetaRegistry::global().register_agent(profile);
            Ok(format!("Successfully registered agent: {}", parts[0]))
        }
    }
}
