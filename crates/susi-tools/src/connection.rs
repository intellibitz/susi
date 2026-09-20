//! SDK-backed outbound MCP connections. A caller-supplied ClientHandler owns
//! consent/UI for sampling, roots and elicitation; no capability is invented.
use crate::McpServerConfig;
use rmcp::{
    model::CallToolRequestParams,
    service::{RoleClient, RunningService},
    transport::{
        streamable_http_client::StreamableHttpClientTransportConfig, StreamableHttpClientTransport,
        TokioChildProcess,
    },
    ClientHandler, ServiceExt,
};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
    time::Duration,
};

/// Connect with any host handler, exposing all SDK methods (resources, prompts,
/// tools, subscriptions, tasks and automatic MRTR fulfillment).
pub async fn connect<H: ClientHandler>(
    config: &McpServerConfig,
    handler: H,
) -> Result<RunningService<RoleClient, H>, String> {
    if config.command.starts_with("http://") || config.command.starts_with("https://") {
        let mut url = reqwest::Url::parse(&config.command).map_err(|e| e.to_string())?;
        if url.path() == "/" {
            url.set_path("/mcp");
        }
        let mut transport = StreamableHttpClientTransportConfig::with_uri(url.to_string());
        if let Some(token) = config.extra.get("auth_token").and_then(Value::as_str) {
            transport = transport.auth_header(token);
        }
        if let Some(headers) = config.extra.get("headers").and_then(Value::as_object) {
            for (name, value) in headers {
                let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                    .map_err(|e| e.to_string())?;
                let value = value.as_str().ok_or("MCP header values must be strings")?;
                transport.custom_headers.insert(
                    name,
                    reqwest::header::HeaderValue::from_str(value).map_err(|e| e.to_string())?,
                );
            }
        }
        // Redirects must not forward configured credentials to another authority.
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| e.to_string())?;
        handler
            .serve(StreamableHttpClientTransport::with_client(
                client, transport,
            ))
            .await
            .map_err(|e| e.to_string())
    } else {
        let mut command = tokio::process::Command::new(&config.command);
        command.args(&config.args).kill_on_drop(true);
        if let Some(env) = &config.env {
            command.envs(env);
        }
        if let Some(cwd) = config.extra.get("cwd").and_then(Value::as_str) {
            command.current_dir(cwd);
        }
        let transport = TokioChildProcess::new(command).map_err(|e| e.to_string())?;
        handler.serve(transport).await.map_err(|e| e.to_string())
    }
}

struct LeaseGuard {
    connection: Connection,
    armed: bool,
}
impl Drop for LeaseGuard {
    fn drop(&mut self) {
        if self.armed {
            self.connection.cancellation_token().cancel();
        }
    }
}

type Connection = Arc<RunningService<RoleClient, ()>>;
type Slot = Arc<tokio::sync::Mutex<Option<Connection>>>;
type Pool = tokio::sync::Mutex<HashMap<String, Slot>>;
static RUNTIME: OnceLock<Result<tokio::runtime::Runtime, std::io::Error>> = OnceLock::new();
static POOL: OnceLock<Pool> = OnceLock::new();

pub(crate) fn call_blocking(config: McpServerConfig, name: String, arguments: Value) -> String {
    let runtime = match RUNTIME.get_or_init(tokio::runtime::Runtime::new) {
        Ok(rt) => rt,
        Err(e) => return format!("[FAIL] MCP runtime: {e}"),
    };
    let cfg = susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
    let lease = Duration::from_secs(cfg.execution_lease_secs());
    let handshake = Duration::from_secs(cfg.cloud_scout_timeout_secs());
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    runtime.spawn(async move {
        let result = tokio::time::timeout(lease, call(config, name, arguments, handshake))
            .await
            .map_err(|_| "MCP execution lease expired".to_string())
            .and_then(|r| r);
        let _ = send.send(result);
    });
    match receive.recv_timeout(lease + Duration::from_secs(1)) {
        Ok(Ok(value)) => value,
        Ok(Err(e)) => format!("[FAIL] MCP: {e}"),
        Err(e) => format!("[FAIL] MCP: {e}"),
    }
}

async fn call(
    config: McpServerConfig,
    name: String,
    arguments: Value,
    handshake: Duration,
) -> Result<String, String> {
    // serde_json's ordered object representation includes env/cwd/auth in the
    // pool identity, so distinct security contexts never share a connection.
    let key = serde_json::to_value(&config)
        .map_err(|e| e.to_string())?
        .to_string();
    let pool = POOL.get_or_init(Default::default);
    let slot = pool.lock().await.entry(key.clone()).or_default().clone();
    let connection = {
        // Only this server's cold start is serialized; unrelated remotes remain concurrent.
        let mut slot = slot.lock().await;
        if let Some(connection) = slot
            .as_ref()
            .filter(|c| !c.is_transport_closed() && !c.is_closed())
        {
            connection.clone()
        } else {
            let connection = tokio::time::timeout(handshake, connect(&config, ()))
                .await
                .map_err(|_| "MCP handshake timed out".to_string())??;
            let connection = Arc::new(connection);
            *slot = Some(connection.clone());
            connection
        }
    };
    let arguments = arguments
        .as_object()
        .cloned()
        .ok_or("Tool arguments must be an object")?;
    // Dropping this future on lease expiry closes the transport. For stdio the
    // SDK owns and terminates the child process group, avoiding PID-reuse races.
    let mut lease_guard = LeaseGuard {
        connection: connection.clone(),
        armed: true,
    };
    let result = connection
        .call_tool(CallToolRequestParams::new(name).with_arguments(arguments))
        .await;
    lease_guard.armed = false;
    match result {
        Ok(result) => {
            // Preserve images/audio/resource links and structured content. The
            // old adapter discarded everything except the first text block.
            let serialized = serde_json::to_string(&result).map_err(|e| e.to_string())?;
            if result.is_error == Some(true) {
                Err(serialized)
            } else {
                Ok(serialized)
            }
        }
        Err(error) => {
            if connection.is_transport_closed() {
                let mut cached = slot.lock().await;
                if cached.as_ref().is_some_and(|c| Arc::ptr_eq(c, &connection)) {
                    *cached = None;
                }
            }
            Err(error.to_string())
        }
    }
}
