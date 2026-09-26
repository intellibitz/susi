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

/// Seal every data chunk of `body` as it is produced — never buffering the
/// stream — so a sealed long-lived SSE response keeps flowing.
fn seal_each_chunk(
    body: BoxBody,
    seal: impl Fn(&[u8]) -> Vec<u8> + Send + Sync + 'static,
) -> BoxBody {
    body.map_frame(move |frame| frame.map_data(|chunk| Bytes::from(seal(&chunk))))
        .boxed()
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
    // The client can open a chunk-sealed stream (`v1s`), so a sealed SSE
    // response need not be buffered whole before it is encrypted.
    let stream_seal = hdr("x-susi-enc-accept").is_some_and(|v| {
        v.split(',')
            .any(|t| t.trim() == crate::susi_core::mcp_client::SEALED_STREAM)
    });
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
    // Sealed request ⇒ sealed response. A client that accepts `v1s` gets
    // each body chunk sealed as it streams (long-lived SSE keeps flowing);
    // otherwise the (bounded) body is collected and sealed once as `v1`.
    // The client refuses unsealed responses to sealed requests, so a
    // relay cannot silently downgrade the return path either.
    let seal_key = match seal_key {
        Some(pk)
            if stream_seal && crate::susi_config::cluster_key::member_seal(&pk, b"").is_some() =>
        {
            let (mut rparts, rbody) = result.into_parts();
            rparts.headers.remove(hyper::header::CONTENT_LENGTH);
            let h = &mut rparts.headers;
            h.insert(
                "x-susi-enc",
                hyper::header::HeaderValue::from_static(
                    crate::susi_core::mcp_client::SEALED_STREAM,
                ),
            );
            h.insert(
                hyper::header::CONTENT_TYPE,
                hyper::header::HeaderValue::from_static("application/octet-stream"),
            );
            let sealed = seal_each_chunk(rbody, move |chunk| {
                crate::susi_core::mcp_client::seal_stream_record(&pk, chunk)
            });
            result = Response::from_parts(rparts, sealed);
            None
        }
        other => other,
    };
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

    /// A chunk reaches the client sealed while the stream is still open —
    /// the old path collected the whole body first, which never completes
    /// for a long-lived SSE stream.
    #[tokio::test]
    async fn sealed_stream_emits_chunks_before_the_body_ends() {
        /// A body that stays open until its sender drops.
        struct OpenBody(tokio::sync::mpsc::Receiver<Bytes>);
        impl hyper::body::Body for OpenBody {
            type Data = Bytes;
            type Error = Infallible;
            fn poll_frame(
                mut self: std::pin::Pin<&mut Self>,
                cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, Infallible>>>
            {
                self.0
                    .poll_recv(cx)
                    .map(|chunk| chunk.map(|b| Ok(hyper::body::Frame::data(b))))
            }
        }
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let mut sealed = seal_each_chunk(OpenBody(rx).boxed(), |chunk| [b"S:", chunk].concat());
        tx.send(Bytes::from_static(b"data: 1\n\n")).await.unwrap();
        let first = tokio::time::timeout(std::time::Duration::from_secs(5), sealed.frame())
            .await
            .expect("sealed chunk withheld until end of stream")
            .unwrap()
            .unwrap();
        assert_eq!(
            first.into_data().unwrap(),
            Bytes::from_static(b"S:data: 1\n\n")
        );
        drop(tx);
        assert!(sealed.frame().await.is_none());
    }

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
