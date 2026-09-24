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
            // `{workspace}` in a cwd template means the caller's working
            // directory (client.rs uses the same convention); passing it
            // through verbatim made the child create a literal `{workspace}`
            // directory.
            if cwd.contains("{workspace}") {
                let ws = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                command.current_dir(cwd.replace("{workspace}", &ws.to_string_lossy()));
            } else {
                command.current_dir(cwd);
            }
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

/// Per-server connect-failure memory. A spawn/handshake failure is cooled
/// for `CONNECT_COOLDOWN` so every subsequent caller doesn't re-pay the
/// spawn cost of a remote that cannot start (e.g. a broken npx package
/// fails the handshake deterministically until the package is fixed).
static CONNECT_FAILURES: OnceLock<tokio::sync::Mutex<HashMap<String, std::time::Instant>>> =
    OnceLock::new();
const CONNECT_COOLDOWN: Duration = Duration::from_secs(300);

pub(crate) fn call_blocking_result(
    config: McpServerConfig,
    name: String,
    arguments: Value,
) -> Result<String, String> {
    let runtime = match RUNTIME.get_or_init(tokio::runtime::Runtime::new) {
        Ok(rt) => rt,
        Err(e) => return Err(format!("MCP runtime: {e}")),
    };
    let cfg = crate::susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
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
        Ok(result) => result,
        Err(error) => Err(error.to_string()),
    }
}

/// Probe a configured MCP server for its live tool catalog (name + description).
pub(crate) fn list_tools_blocking(
    config: McpServerConfig,
) -> Result<Vec<(String, String)>, String> {
    let runtime = match RUNTIME.get_or_init(tokio::runtime::Runtime::new) {
        Ok(rt) => rt,
        Err(e) => return Err(format!("MCP runtime: {e}")),
    };
    let cfg = crate::susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
    let lease = Duration::from_secs(cfg.execution_lease_secs().min(30));
    let handshake = Duration::from_secs(cfg.cloud_scout_timeout_secs().min(10));
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    runtime.spawn(async move {
        let result = tokio::time::timeout(lease, list_tools(config, handshake))
            .await
            .map_err(|_| "MCP tool discovery lease expired".to_string())
            .and_then(|r| r);
        let _ = send.send(result);
    });
    match receive.recv_timeout(lease + Duration::from_secs(1)) {
        Ok(Ok(tools)) => Ok(tools),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(e.to_string()),
    }
}

async fn acquire_pooled_connection(
    config: &McpServerConfig,
    handshake: Duration,
) -> Result<(Connection, Slot), String> {
    // serde_json's ordered object representation includes env/cwd/auth in the
    // pool identity, so distinct security contexts never share a connection.
    let key = serde_json::to_value(config)
        .map_err(|e| e.to_string())?
        .to_string();
    let failures = CONNECT_FAILURES.get_or_init(Default::default);
    if let Some(at) = failures.lock().await.get(&key) {
        if at.elapsed() < CONNECT_COOLDOWN {
            return Err("MCP server connect cooled after a recent failure".to_string());
        }
    }
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
            let connection = match tokio::time::timeout(handshake, connect(config, ())).await {
                Ok(Ok(connection)) => connection,
                Ok(Err(error)) => {
                    let mut failures = failures.lock().await;
                    if failures.len() > 64 {
                        failures.clear();
                    }
                    failures.insert(key.clone(), std::time::Instant::now());
                    return Err(error);
                }
                Err(_) => {
                    let mut failures = failures.lock().await;
                    if failures.len() > 64 {
                        failures.clear();
                    }
                    failures.insert(key.clone(), std::time::Instant::now());
                    return Err("MCP handshake timed out".to_string());
                }
            };
            failures.lock().await.remove(&key);
            let connection = Arc::new(connection);
            *slot = Some(connection.clone());
            connection
        }
    };
    Ok((connection, slot))
}

async fn list_tools(
    config: McpServerConfig,
    handshake: Duration,
) -> Result<Vec<(String, String)>, String> {
    let (connection, slot) = acquire_pooled_connection(&config, handshake).await?;
    let mut lease_guard = LeaseGuard {
        connection: connection.clone(),
        armed: true,
    };
    let result = connection.list_all_tools().await;
    lease_guard.armed = false;
    match result {
        Ok(tools) => Ok(tools
            .into_iter()
            .map(|t| {
                let desc = t
                    .description
                    .map(|d| d.into_owned())
                    .unwrap_or_else(|| format!("MCP tool '{}'", t.name));
                (t.name.into_owned(), desc)
            })
            .collect()),
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

async fn call(
    config: McpServerConfig,
    name: String,
    arguments: Value,
    handshake: Duration,
) -> Result<String, String> {
    let (connection, slot) = acquire_pooled_connection(&config, handshake).await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_failure_cooldown_short_circuits_respawn() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async {
            let config = McpServerConfig {
                command: "/nonexistent-susi-test-binary-xyz".to_string(),
                args: vec![],
                env: None,
                extra: HashMap::new(),
            };
            // A connect failure is remembered for CONNECT_COOLDOWN.
            let first = acquire_pooled_connection(&config, Duration::from_secs(2)).await;
            assert!(first.is_err());
            let key = serde_json::to_value(&config).expect("json").to_string();
            assert!(CONNECT_FAILURES
                .get_or_init(Default::default)
                .lock()
                .await
                .contains_key(&key));
            // The cooled call returns immediately without another spawn attempt.
            let start = std::time::Instant::now();
            let second = acquire_pooled_connection(&config, Duration::from_secs(2)).await;
            assert!(second
                .unwrap_err()
                .contains("connect cooled after a recent failure"));
            assert!(start.elapsed() < Duration::from_secs(1));
        });
    }
}
