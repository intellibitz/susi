//! Executable MCP lifecycle probes.
//!
//! A catalog entry is not evidence that a server is compatible.  This module
//! gives transports a small, synchronous boundary for exercising a real MCP
//! session and for reporting the negative-path checks that an admitted server
//! must pass.  HTTP, stdio, and in-process transports can implement the same
//! trait without making the profile depend on one transport implementation.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    time::{Duration, Instant},
};

/// MCP revisions understood by the shipped session client, in ascending
/// order so the last common entry is the newest one.
pub const SUPPORTED_PROTOCOL_VERSIONS: [&str; 4] =
    ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];
pub const CURRENT_PROTOCOL_VERSION: &str = "2025-11-25";

const INITIALIZE_ID: u64 = 1;
const TOOLS_LIST_START_ID: u64 = 2;
const CANCELLATION_ID: u64 = 3;
const MAX_DISCOVERY_PAGES: usize = 64;

/// Probe defaults.  The transport owns the actual socket/process deadline;
/// this value is carried on every request so it cannot silently be ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpProbeOptions {
    pub timeout: Duration,
    pub max_response_bytes: usize,
    pub max_discovery_pages: usize,
    pub client_name: String,
    pub client_version: String,
}

impl Default for McpProbeOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(5),
            max_response_bytes: 1024 * 1024,
            max_discovery_pages: MAX_DISCOVERY_PAGES,
            client_name: "susi-mcp-probe".to_string(),
            client_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

/// One JSON-RPC request handed to an MCP transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpProbeRequest {
    pub id: u64,
    pub method: String,
    pub params: Value,
    pub timeout: Duration,
    /// Headers are deliberately constructed from a protocol-only allowlist.
    /// A transport must not add host credentials to this request.
    pub headers: BTreeMap<String, String>,
}

impl McpProbeRequest {
    #[must_use]
    pub fn new(id: u64, method: &str, params: Value, timeout: Duration) -> Self {
        let headers = BTreeMap::from([
            (
                "accept".to_string(),
                "application/json, text/event-stream".to_string(),
            ),
            ("content-type".to_string(), "application/json".to_string()),
        ]);
        Self {
            id,
            method: method.to_string(),
            params,
            timeout,
            headers,
        }
    }

    #[must_use]
    pub fn with_protocol_version(mut self, version: &str) -> Self {
        self.headers
            .insert("mcp-protocol-version".to_string(), version.to_string());
        self
    }

    #[must_use]
    pub fn sensitive_headers(&self) -> Vec<String> {
        self.headers
            .keys()
            .filter(|name| {
                matches!(
                    name.to_ascii_lowercase().as_str(),
                    "authorization" | "proxy-authorization" | "cookie" | "set-cookie" | "x-api-key"
                )
            })
            .cloned()
            .collect()
    }
}

/// One JSON-RPC notification handed to an MCP transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpProbeNotification {
    pub method: String,
    pub params: Value,
    pub headers: BTreeMap<String, String>,
}

impl McpProbeNotification {
    #[must_use]
    pub fn new(method: &str, params: Value, protocol_version: Option<&str>) -> Self {
        let mut headers =
            BTreeMap::from([("content-type".to_string(), "application/json".to_string())]);
        if let Some(version) = protocol_version {
            headers.insert("mcp-protocol-version".to_string(), version.to_string());
        }
        Self {
            method: method.to_string(),
            params,
            headers,
        }
    }
}

/// Response returned by a transport after decoding one JSON-RPC result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpProbeResponse {
    pub payload: Value,
    pub bytes: usize,
}

impl McpProbeResponse {
    #[must_use]
    pub fn from_value(payload: Value) -> Self {
        let bytes = serde_json::to_vec(&payload).map_or(0, |encoded| encoded.len());
        Self { payload, bytes }
    }

    #[must_use]
    pub const fn new(payload: Value, bytes: usize) -> Self {
        Self { payload, bytes }
    }
}

/// Errors a transport or the protocol validator can expose to a probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpProbeError {
    Transport {
        method: String,
        detail: String,
    },
    Timeout {
        method: String,
    },
    Malformed {
        method: String,
        detail: String,
    },
    Protocol {
        method: String,
        detail: String,
    },
    ResponseTooLarge {
        method: String,
        bytes: usize,
        max: usize,
    },
    CredentialLeak {
        detail: String,
    },
}

impl fmt::Display for McpProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport { method, detail } => write!(f, "{method} transport error: {detail}"),
            Self::Timeout { method } => write!(f, "{method} exceeded its deadline"),
            Self::Malformed { method, detail } => {
                write!(f, "{method} malformed response: {detail}")
            }
            Self::Protocol { method, detail } => write!(f, "{method} protocol error: {detail}"),
            Self::ResponseTooLarge { method, bytes, max } => {
                write!(
                    f,
                    "{method} response is {bytes} bytes, over {max}-byte limit"
                )
            }
            Self::CredentialLeak { detail } => write!(f, "credential isolation failed: {detail}"),
        }
    }
}

impl std::error::Error for McpProbeError {}

/// Transport boundary used by [`McpProbe`].
pub trait McpProbeTransport {
    /// Send one request and decode the JSON-RPC result payload.  The
    /// implementation must honor `request.timeout` and preserve the
    /// protocol-only request headers.
    fn request(&mut self, request: &McpProbeRequest) -> Result<McpProbeResponse, McpProbeError>;

    /// Send a JSON-RPC notification and wait for its transport acknowledgement
    /// where the transport has one.
    fn notify(&mut self, notification: &McpProbeNotification) -> Result<(), McpProbeError>;

    /// Exercise cancellation of an in-flight request.  The default sends the
    /// standard MCP cancellation notification, which is the wire behavior an
    /// HTTP or stdio transport must preserve.
    fn probe_cancellation(
        &mut self,
        request_id: u64,
        timeout: Duration,
        protocol_version: &str,
    ) -> Result<(), McpProbeError> {
        let notification = McpProbeNotification::new(
            "notifications/cancelled",
            json!({"requestId": request_id, "reason": "susi compatibility probe"}),
            Some(protocol_version),
        );
        let _ = timeout;
        self.notify(&notification)
    }

    /// Exercise the transport deadline with a request that the implementation
    /// can hold open in its own test harness.  `Ok(elapsed)` means the request
    /// was completed or cancelled within the requested deadline; a timeout
    /// error is also an acceptable, fail-closed result.
    fn probe_timeout(
        &mut self,
        timeout: Duration,
        protocol_version: &str,
    ) -> Result<Duration, McpProbeError> {
        let request = McpProbeRequest::new(u64::MAX, "ping", json!({}), timeout)
            .with_protocol_version(protocol_version);
        let started = Instant::now();
        match self.request(&request) {
            Ok(_) => Ok(started.elapsed()),
            Err(McpProbeError::Timeout { .. }) => Ok(timeout),
            Err(error) => Err(error),
        }
    }

    /// Feed a malformed JSON-RPC result through the same decoder used by the
    /// transport.  Success means the malformed frame was rejected.  This is
    /// required because only the transport knows how to exercise its own
    /// framing decoder (SSE, stdio, or another MCP wire format).
    fn probe_malformed_response(
        &mut self,
        timeout: Duration,
        protocol_version: &str,
    ) -> Result<(), McpProbeError>;

    /// Verify that the transport has not attached host credentials to probe
    /// requests or returned them as part of server metadata.  It is required
    /// because the transport is the only layer that can observe final wire
    /// headers and decoded server metadata.
    fn probe_credential_isolation(&self) -> Result<(), McpProbeError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpProbeCheck {
    pub passed: bool,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpProbeReport {
    pub protocol_version: Option<String>,
    pub server_info: Option<Value>,
    pub capabilities: Option<Value>,
    pub supports_list_changed: Option<bool>,
    pub tools: Vec<String>,
    pub checks: BTreeMap<String, McpProbeCheck>,
}

impl McpProbeReport {
    #[must_use]
    pub fn passed(&self) -> bool {
        !self.checks.is_empty() && self.checks.values().all(|check| check.passed)
    }

    fn record(&mut self, name: &str, result: Result<(), McpProbeError>) {
        let check = match result {
            Ok(()) => McpProbeCheck {
                passed: true,
                detail: "passed".to_string(),
            },
            Err(error) => McpProbeCheck {
                passed: false,
                detail: error.to_string(),
            },
        };
        self.checks.insert(name.to_string(), check);
    }
}

/// Run the executable MCP lifecycle checks against an admitted server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpProbe {
    options: McpProbeOptions,
}

impl McpProbe {
    #[must_use]
    pub const fn new(options: McpProbeOptions) -> Self {
        Self { options }
    }

    pub fn run<T: McpProbeTransport>(&self, transport: &mut T) -> McpProbeReport {
        let mut report = McpProbeReport {
            protocol_version: None,
            server_info: None,
            capabilities: None,
            supports_list_changed: None,
            tools: Vec::new(),
            checks: BTreeMap::new(),
        };

        if self.options.timeout.is_zero()
            || self.options.max_response_bytes == 0
            || self.options.max_discovery_pages == 0
        {
            report.record(
                "configuration",
                Err(McpProbeError::Protocol {
                    method: "probe".to_string(),
                    detail: "timeout, response limit, and page limit must be positive".to_string(),
                }),
            );
            return report;
        }

        let initialize = McpProbeRequest::new(
            INITIALIZE_ID,
            "initialize",
            json!({
                "protocolVersion": CURRENT_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {
                    "name": self.options.client_name,
                    "version": self.options.client_version,
                }
            }),
            self.options.timeout,
        );
        if let Err(error) = validate_request_headers(&initialize) {
            report.record("credential_isolation", Err(error));
            return report;
        }
        let initialize_response = match transport.request(&initialize) {
            Ok(response) => response,
            Err(error) => {
                report.record("initialize", Err(error));
                return report;
            }
        };
        let initialize_payload = match response_payload(
            initialize_response,
            "initialize",
            self.options.max_response_bytes,
        ) {
            Ok(payload) => payload,
            Err(error) => {
                report.record("initialize", Err(error));
                return report;
            }
        };
        let (version, server_info, capabilities) = match validate_initialize(&initialize_payload) {
            Ok(parts) => parts,
            Err(error) => {
                report.record("initialize", Err(error));
                return report;
            }
        };
        report.protocol_version = Some(version.clone());
        report.server_info = Some(server_info);
        report.supports_list_changed = capabilities
            .get("tools")
            .and_then(|tools| tools.get("listChanged"))
            .and_then(Value::as_bool);
        report.capabilities = Some(capabilities);
        report.record("initialize", Ok(()));

        let initialized = McpProbeNotification::new(
            "notifications/initialized",
            Value::Object(serde_json::Map::new()),
            Some(&version),
        );
        report.record("initialized", transport.notify(&initialized));
        if !report.checks["initialized"].passed {
            return report;
        }

        let mut cursor = None;
        let mut seen_cursors = BTreeSet::new();
        let mut tools = Vec::new();
        for page in 0..self.options.max_discovery_pages {
            let params = cursor
                .as_ref()
                .map_or_else(|| json!({}), |value| json!({"cursor": value}));
            let request = McpProbeRequest::new(
                TOOLS_LIST_START_ID + page as u64,
                "tools/list",
                params,
                self.options.timeout,
            )
            .with_protocol_version(&version);
            if let Err(error) = validate_request_headers(&request) {
                report.record("credential_isolation", Err(error));
                return report;
            }
            let response = match transport.request(&request) {
                Ok(response) => response,
                Err(error) => {
                    report.record("discovery", Err(error));
                    return report;
                }
            };
            let payload =
                match response_payload(response, "tools/list", self.options.max_response_bytes) {
                    Ok(payload) => payload,
                    Err(error) => {
                        report.record("discovery", Err(error));
                        return report;
                    }
                };
            let (page_tools, next_cursor) = match validate_tools_page(&payload) {
                Ok(page) => page,
                Err(error) => {
                    report.record("discovery", Err(error));
                    return report;
                }
            };
            tools.extend(page_tools);
            match next_cursor {
                Some(next) if seen_cursors.insert(next.clone()) => cursor = Some(next),
                Some(_) => {
                    report.record(
                        "discovery",
                        Err(McpProbeError::Malformed {
                            method: "tools/list".to_string(),
                            detail: "server repeated a pagination cursor".to_string(),
                        }),
                    );
                    return report;
                }
                None => {
                    report.tools = tools;
                    report.record("discovery", Ok(()));
                    break;
                }
            }
            if page + 1 == self.options.max_discovery_pages {
                report.record(
                    "discovery",
                    Err(McpProbeError::Protocol {
                        method: "tools/list".to_string(),
                        detail: "server exceeded the discovery page limit".to_string(),
                    }),
                );
                return report;
            }
        }

        report.record(
            "cancellation",
            transport.probe_cancellation(CANCELLATION_ID, self.options.timeout, &version),
        );
        report.record(
            "timeout",
            transport
                .probe_timeout(self.options.timeout, &version)
                .and_then(|elapsed| {
                    if elapsed <= self.options.timeout {
                        Ok(())
                    } else {
                        Err(McpProbeError::Timeout {
                            method: "probe-timeout".to_string(),
                        })
                    }
                }),
        );
        report.record(
            "malformed_response",
            transport.probe_malformed_response(self.options.timeout, &version),
        );
        report.record(
            "credential_isolation",
            transport.probe_credential_isolation(),
        );
        report
    }
}

fn validate_request_headers(request: &McpProbeRequest) -> Result<(), McpProbeError> {
    let sensitive = request.sensitive_headers();
    if sensitive.is_empty() {
        Ok(())
    } else {
        Err(McpProbeError::CredentialLeak {
            detail: format!("probe request contains {} header(s)", sensitive.len()),
        })
    }
}

fn response_payload(
    response: McpProbeResponse,
    method: &str,
    max_response_bytes: usize,
) -> Result<Value, McpProbeError> {
    if response.bytes > max_response_bytes {
        return Err(McpProbeError::ResponseTooLarge {
            method: method.to_string(),
            bytes: response.bytes,
            max: max_response_bytes,
        });
    }
    Ok(response.payload)
}

fn validate_initialize(value: &Value) -> Result<(String, Value, Value), McpProbeError> {
    let object = value.as_object().ok_or_else(|| McpProbeError::Malformed {
        method: "initialize".to_string(),
        detail: "result is not an object".to_string(),
    })?;
    let version = object
        .get("protocolVersion")
        .and_then(Value::as_str)
        .ok_or_else(|| McpProbeError::Malformed {
            method: "initialize".to_string(),
            detail: "protocolVersion is missing or not a string".to_string(),
        })?;
    if !SUPPORTED_PROTOCOL_VERSIONS.contains(&version) {
        return Err(McpProbeError::Protocol {
            method: "initialize".to_string(),
            detail: format!("unsupported protocolVersion {version}"),
        });
    }
    let server_info = object
        .get("serverInfo")
        .filter(|value| value.is_object())
        .cloned()
        .ok_or_else(|| McpProbeError::Malformed {
            method: "initialize".to_string(),
            detail: "serverInfo is missing or not an object".to_string(),
        })?;
    let server_info_object = server_info
        .as_object()
        .ok_or_else(|| McpProbeError::Malformed {
            method: "initialize".to_string(),
            detail: "serverInfo is not an object".to_string(),
        })?;
    for field in ["name", "version"] {
        if server_info_object
            .get(field)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .is_none()
        {
            return Err(McpProbeError::Malformed {
                method: "initialize".to_string(),
                detail: format!("serverInfo.{field} is missing or empty"),
            });
        }
    }
    let capabilities = object
        .get("capabilities")
        .filter(|value| value.is_object())
        .cloned()
        .ok_or_else(|| McpProbeError::Malformed {
            method: "initialize".to_string(),
            detail: "capabilities is missing or not an object".to_string(),
        })?;
    Ok((version.to_string(), server_info, capabilities))
}

fn validate_tools_page(value: &Value) -> Result<(Vec<String>, Option<String>), McpProbeError> {
    let object = value.as_object().ok_or_else(|| McpProbeError::Malformed {
        method: "tools/list".to_string(),
        detail: "result is not an object".to_string(),
    })?;
    let tools = object
        .get("tools")
        .and_then(Value::as_array)
        .ok_or_else(|| McpProbeError::Malformed {
            method: "tools/list".to_string(),
            detail: "tools is missing or not an array".to_string(),
        })?;
    let mut names = Vec::with_capacity(tools.len());
    let mut seen = BTreeSet::new();
    for tool in tools {
        let name = tool
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| McpProbeError::Malformed {
                method: "tools/list".to_string(),
                detail: "a tool has no non-empty name".to_string(),
            })?;
        if !seen.insert(name.to_string()) {
            return Err(McpProbeError::Protocol {
                method: "tools/list".to_string(),
                detail: format!("duplicate tool name {name}"),
            });
        }
        names.push(name.to_string());
    }
    let next_cursor = object
        .get("nextCursor")
        .map(|cursor| {
            cursor
                .as_str()
                .filter(|cursor| !cursor.is_empty())
                .map(str::to_string)
                .ok_or_else(|| McpProbeError::Malformed {
                    method: "tools/list".to_string(),
                    detail: "nextCursor is not a non-empty string".to_string(),
                })
        })
        .transpose()?;
    Ok((names, next_cursor))
}
