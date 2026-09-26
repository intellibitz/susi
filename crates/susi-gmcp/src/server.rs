//! MCP stdio and Streamable HTTP entry points backed by the official Rust SDK.
use crate::protocol::GmcpService;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{
    body::Incoming,
    header::{HeaderValue, CONTENT_TYPE},
    service::service_fn,
    Method, Request, Response, StatusCode,
};
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto::Builder,
};
use rmcp::{
    transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
    },
    ServiceExt,
};
use std::{
    convert::Infallible,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio_rustls::TlsAcceptor;
use tower_service::Service;

// Canonical dual-protocol (TLS-sniffing) transport shared with the GEMI
// REST and A2A servers.
#[rustfmt::skip]
#[path = "../../susi-server/src/dual_transport.rs"]
mod dual_transport;
use dual_transport::negotiate_transport;

type BoxBody = http_body_util::combinators::BoxBody<Bytes, Infallible>;
type HttpService = StreamableHttpService<GmcpService, LocalSessionManager>;

fn response(status: StatusCode, text: &'static str) -> Response<BoxBody> {
    let mut response = Response::new(Full::new(Bytes::from_static(text.as_bytes())).boxed());
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/plain"));
    // RFC 9110 §15.5.2: a 401 MUST carry a challenge; RFC 6585 §4: tell a
    // rate-limited client when the per-minute window reopens.
    if status == StatusCode::UNAUTHORIZED {
        headers.insert(
            hyper::header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Bearer realm=\"susi-gmcp\""),
        );
    } else if status == StatusCode::TOO_MANY_REQUESTS {
        headers.insert(hyper::header::RETRY_AFTER, HeaderValue::from_static("60"));
    }
    response
}

fn cors(response: &mut Response<BoxBody>, origin: &str) {
    if let Ok(value) = HeaderValue::from_str(origin) {
        response
            .headers_mut()
            .insert("access-control-allow-origin", value);
    }
    response.headers_mut().insert(
        "access-control-allow-methods",
        HeaderValue::from_static("GET, POST, DELETE, OPTIONS"),
    );
    response.headers_mut().insert("access-control-allow-headers", HeaderValue::from_static("Content-Type, Authorization, Accept, MCP-Protocol-Version, Mcp-Session-Id, Mcp-Method, Mcp-Name, Last-Event-ID"));
    response.headers_mut().insert(
        "access-control-expose-headers",
        HeaderValue::from_static("Mcp-Session-Id, MCP-Protocol-Version"),
    );
}

pub struct GmcpServer;
impl GmcpServer {
    pub fn run_stdio(workspace: &Path, version: &str) {
        eprintln!("[GMCP] v{version}: MCP over newline-delimited stdio");
        let runtime = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("[GMCP] Runtime failed: {e}");
                return;
            }
        };
        let service = GmcpService::new(workspace.to_path_buf());
        if let Err(e) = crate::catalog::install(&service) {
            eprintln!("[GMCP] Catalog failed: {e}");
            return;
        }
        runtime.block_on(async move {
            let limit = crate::susi_sandbox::manager::SusiConfig::load_global_arc()
                .unwrap_or_default()
                .max_rpc_body_bytes();
            let transport = (
                crate::stdio::BoundedLines::new(tokio::io::stdin(), limit),
                tokio::io::stdout(),
            );
            match service.serve(transport).await {
                Ok(running) => {
                    if let Err(e) = running.waiting().await {
                        eprintln!("[GMCP] Stdio failed: {e}");
                    }
                }
                Err(e) => eprintln!("[GMCP] Stdio negotiation failed: {e}"),
            }
        });
    }

    pub fn start_http_server(
        workspace: PathBuf,
        listeners: Vec<std::net::TcpListener>,
        tls: Option<TlsAcceptor>,
        require_tls_remote: bool,
    ) {
        let runtime = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("[GMCP] Runtime failed: {e}");
                return;
            }
        };
        runtime.block_on(async move {
            let cfg =
                crate::susi_sandbox::manager::SusiConfig::load_global_arc().unwrap_or_default();
            let permits = Arc::new(tokio::sync::Semaphore::new(cfg.max_concurrent_agents()));
            let application = GmcpService::new(workspace);
            if let Err(e) = crate::catalog::install(&application) {
                eprintln!("[GMCP] Catalog failed: {e}");
                return;
            }
            let service = http_service(application);
            eprintln!("[GMCP] Streamable HTTP available at /mcp (alias /messages)");

            // One accept loop per bound socket (loopback + external when the
            // bind address is a specific non-loopback IP).
            let mut accept_loops = Vec::new();
            for std_listener in listeners {
                if let Err(e) = std_listener.set_nonblocking(true) {
                    eprintln!("[GMCP] Listener failed: {e}");
                    continue;
                }
                let listener = match tokio::net::TcpListener::from_std(std_listener) {
                    Ok(l) => l,
                    Err(e) => {
                        eprintln!("[GMCP] Listener failed: {e}");
                        continue;
                    }
                };
                let permits = Arc::clone(&permits);
                let service = service.clone();
                let tls = tls.clone();
                accept_loops.push(tokio::spawn(async move {
                    loop {
                        let (stream, peer) = match listener.accept().await {
                            Ok(pair) => pair,
                            Err(e) => {
                                eprintln!("[GMCP] Accept failed: {e}");
                                continue;
                            }
                        };
                        let permit = match permits.clone().try_acquire_owned() {
                            Ok(p) => p,
                            Err(_) => {
                                drop(stream);
                                continue;
                            }
                        };
                        let service = service.clone();
                        let peer_ip = peer.ip();
                        let tls = tls.clone();
                        let remote = !peer_ip.is_loopback();
                        tokio::spawn(async move {
                            let _permit = permit;
                            let Some(io) = negotiate_transport(
                                stream,
                                remote,
                                tls.as_ref(),
                                require_tls_remote,
                                "[GMCP]",
                            )
                            .await
                            else {
                                return;
                            };
                            let svc = service_fn(move |req| {
                                handle_request(req, service.clone(), peer_ip)
                            });
                            if let Err(e) = Builder::new(TokioExecutor::new())
                                .serve_connection(TokioIo::new(io), svc)
                                .await
                            {
                                eprintln!("[GMCP] Connection failed: {e}");
                            }
                        });
                    }
                }));
            }
            for accept_loop in accept_loops {
                let _ = accept_loop.await;
            }
        });
    }
}

pub fn http_service(service: GmcpService) -> HttpService {
    let cfg = crate::susi_sandbox::manager::SusiConfig::load_global_arc().unwrap_or_default();
    let mut transport = StreamableHttpServerConfig::default();
    transport.max_request_body_bytes = cfg.max_rpc_body_bytes();
    if let Some(hosts) = cfg.get::<Vec<String>>("mcp_allowed_hosts") {
        transport = transport.with_allowed_hosts(hosts);
    }
    let origin = cfg.allow_origin();
    // Wildcard CORS is not an Origin-validation policy. With no explicit origin
    // configured, reject browser origins; non-browser clients still pass.
    transport = if origin == "*" {
        transport.enforce_origin_validation()
    } else {
        transport.with_allowed_origins([origin])
    };
    StreamableHttpService::new(
        move || Ok(service.session()),
        Arc::new(LocalSessionManager::default()),
        transport,
    )
}

async fn handle_request(
    req: Request<Incoming>,
    mut service: HttpService,
    peer: std::net::IpAddr,
) -> Result<Response<BoxBody>, Infallible> {
    let cfg = crate::susi_sandbox::manager::SusiConfig::load_global_arc().unwrap_or_default();
    let hdr = |name: &str| {
        req.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    // Owned header values — the body buffer below moves `req`, so the
    // SignedRequest borrows these, not the request.
    let (node_h, ts_h, nonce_h, sig_h, enc_h, enc_nonce_h, node_pub_h) = (
        hdr("x-susi-node"),
        hdr("x-susi-req-ts"),
        hdr("x-susi-req-nonce"),
        hdr("x-susi-req-sig"),
        hdr("x-susi-enc"),
        hdr("x-susi-enc-nonce"),
        hdr("x-susi-node-pub"),
    );
    let signed = crate::susi_core::net_guard::SignedRequest {
        node: node_h.as_deref(),
        ts_secs: ts_h.as_deref().and_then(|s| s.parse().ok()),
        nonce: nonce_h.as_deref(),
        sig: sig_h.as_deref(),
    };
    // Buffer the body once up front: request signatures bind to the
    // exact body bytes (v2 canonical), and `service.call` accepts any
    // Body so the buffered bytes rebuild into the same request shape
    // downstream — JSON-RPC bodies are small and bounded anyway.
    let (mut parts, body) = req.into_parts();
    // Same ceiling the MCP transport enforces, plus the 16-byte AEAD tag a
    // sealed body carries.
    let body_limit = cfg.max_rpc_body_bytes().saturating_add(16);
    let body_bytes = match http_body_util::BodyExt::collect(http_body_util::Limited::new(
        body, body_limit,
    ))
    .await
    {
        Ok(c) => c.to_bytes(),
        Err(_) => {
            return Ok(response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "request body exceeds limit",
            ));
        }
    };
    // Pairwise-sealed body (bound-member channel): open before auth so
    // the v2 signature verifies against the plaintext hash the sender
    // signed. Decryption uses the *roster-bound* pubkey when one exists;
    // the self-asserted `x-susi-node-pub` covers the asymmetric window —
    // AEAD tag failure or a swapped pubkey fails closed either way.
    // When the request was sealed, the response is sealed back to the
    // same key — the session id and tool results travel encrypted too.
    let mut seal_key: Option<String> = None;
    let effective = if enc_h.is_some() || enc_nonce_h.is_some() {
        let open_key = node_h
            .as_deref()
            .and_then(crate::susi_config::cluster_key::bound_pubkey_for_node)
            .or(node_pub_h);
        match (
            enc_h.as_deref(),
            enc_nonce_h.as_deref(),
            open_key.as_deref(),
        ) {
            (Some("v1"), Some(nonce), Some(pk)) => {
                match crate::susi_config::cluster_key::member_open(pk, nonce, &body_bytes) {
                    Some(pt) => {
                        seal_key = Some(pk.to_string());
                        // rmcp requires application/json — the sealed wire
                        // body travelled as octet-stream.
                        parts.headers.insert(
                            hyper::header::CONTENT_TYPE,
                            hyper::header::HeaderValue::from_static("application/json"),
                        );
                        pt
                    }
                    None => return Ok(response(StatusCode::UNAUTHORIZED, "Unauthorized")),
                }
            }
            _ => return Ok(response(StatusCode::UNAUTHORIZED, "Unauthorized")),
        }
    } else {
        body_bytes.to_vec()
    };
    let req = Request::from_parts(
        parts,
        http_body_util::Full::new(hyper::body::Bytes::from(effective.clone())),
    );
    let mut result = if req.method() == Method::OPTIONS {
        response(StatusCode::NO_CONTENT, "")
    } else if !crate::susi_core::net_guard::NetGuard::is_authorized(
        req.headers()
            .get(hyper::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok()),
        peer,
        &signed,
        req.method().as_str(),
        req.uri().path(),
        Some(&effective),
    ) {
        response(StatusCode::UNAUTHORIZED, "Unauthorized")
    } else if !crate::susi_core::net_guard::RateLimiter::global()
        .check(peer, cfg.rate_limit_per_minute())
    {
        response(StatusCode::TOO_MANY_REQUESTS, "Rate limit exceeded")
    } else if matches!(req.uri().path(), "/mcp" | "/messages") {
        service.call(req).await?
    } else {
        response(
            StatusCode::NOT_FOUND,
            "Use the MCP Streamable HTTP endpoint at /mcp",
        )
    };
    // Sealed request ⇒ sealed response: collect the (bounded) service
    // body, encrypt to the requester's key, and mark it `x-susi-enc: v1`.
    // The client refuses unsealed responses to sealed requests, so a
    // relay cannot silently downgrade the return path either.
    if let Some(pk) = seal_key {
        let (mut rparts, rbody) = result.into_parts();
        // The rebuilt body's length differs from whatever the service
        // set — stale lengths truncate or hang the reader.
        rparts.headers.remove(hyper::header::CONTENT_LENGTH);
        let rbytes = match http_body_util::BodyExt::collect(http_body_util::Limited::new(
            rbody,
            16 * 1024 * 1024,
        ))
        .await
        {
            Ok(c) => c.to_bytes(),
            Err(_) => {
                return Ok(response(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "response body exceeds limit",
                ));
            }
        };
        result = match crate::susi_config::cluster_key::member_seal(&pk, &rbytes) {
            Some((nonce, ct)) => {
                let mut sealed = Response::from_parts(
                    rparts,
                    http_body_util::Full::new(Bytes::from(ct)).boxed(),
                );
                let h = sealed.headers_mut();
                h.insert("x-susi-enc", hyper::header::HeaderValue::from_static("v1"));
                if let Ok(v) = hyper::header::HeaderValue::from_str(&nonce) {
                    h.insert("x-susi-enc-nonce", v);
                }
                h.insert(
                    hyper::header::CONTENT_TYPE,
                    hyper::header::HeaderValue::from_static("application/octet-stream"),
                );
                sealed
            }
            // Our node.key is unreadable — return the plaintext body
            // rather than dropping the call; the client fails closed
            // on unsealed replies, so the downgrade is visible, never
            // silent.
            None => Response::from_parts(rparts, http_body_util::Full::new(rbytes).boxed()),
        };
    }
    cors(&mut result, &cfg.allow_origin());
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejections_carry_rfc_challenge_headers() {
        let unauthorized = response(StatusCode::UNAUTHORIZED, "Unauthorized");
        assert_eq!(
            unauthorized.headers()[hyper::header::WWW_AUTHENTICATE],
            "Bearer realm=\"susi-gmcp\""
        );
        let limited = response(StatusCode::TOO_MANY_REQUESTS, "Rate limit exceeded");
        assert_eq!(limited.headers()[hyper::header::RETRY_AFTER], "60");
        let missing = response(StatusCode::NOT_FOUND, "nope");
        assert!(!missing
            .headers()
            .contains_key(hyper::header::WWW_AUTHENTICATE));
    }
}
