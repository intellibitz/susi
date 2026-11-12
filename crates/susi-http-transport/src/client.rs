//! Shared outbound HTTP client for every SUSI plane that talks to a remote
//! JSON endpoint (MCP, peer A2A, live search, eval/benchmark, HF download,
//! cloud agent APIs, webhooks, BYOK inference providers).
//!
//! Vendor SDKs and provider-specific wire shapes do **not** live here:
//! OpenAI / Anthropic / Gemini bodies stay in `susi-adapters-llm`.
//!
//! HTTP/1.1 on std sockets (`wire`, `connect`): one total deadline per
//! call covers DNS, connect, TLS, the CONNECT tunnel, every redirect hop,
//! the response head and the body the caller reads afterwards. TLS is the
//! platform stack with its trust store; verification is never disabled.
//! Connections are not pooled (`Connection: close`).

use std::io::{BufReader, Read};
use std::time::{Duration, Instant};

use crate::connect::{self, Proxy};
use crate::wire::{self, BodyReader, Target};
use tokio_rustls::native_tls;

/// `User-Agent` sent when the caller sets none.
pub(crate) const USER_AGENT: &str = concat!("susi-http-transport/", env!("CARGO_PKG_VERSION"));

/// One completed request: status, headers, and a `Read` body. HTTP error
/// statuses are returned, not turned into `Err` — resume downloads need
/// 416 / 206.
pub struct HttpCall {
    /// HTTP status code.
    pub status: u16,
    headers: Vec<(String, String)>,
    body: Box<dyn Read + Send>,
}

impl HttpCall {
    /// First header value matching `name` (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// All response headers as `(name, value)` pairs, names lowercase.
    pub fn headers(&self) -> &[(String, String)] {
        &self.headers
    }

    /// Consume the body as a `Read`. Reads stay bounded by the call's
    /// deadline and fail with a `timeout` error once it passes.
    pub fn into_reader(self) -> impl Read {
        self.body
    }

    /// Consume the body into a bounded byte buffer, rejecting payloads over
    /// `max`. One extra byte is read to detect overflow.
    pub fn into_bytes(self, max: u64) -> Result<Vec<u8>, std::io::Error> {
        let mut bytes = Vec::new();
        self.into_reader()
            .take(max.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > max {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "response body exceeds configured limit",
            ));
        }
        Ok(bytes)
    }

    /// Consume the body as UTF-8 text, failing when the cap is exceeded.
    pub fn into_utf8(self, max: u64) -> Result<String, String> {
        let bytes = self.into_bytes(max).map_err(|e| e.to_string())?;
        String::from_utf8(bytes).map_err(|e| e.to_string())
    }
}

/// Whether `url` names this machine (`localhost` or a loopback literal).
#[cfg(test)]
fn loopback_url(url: &str) -> bool {
    Target::parse(url).is_ok_and(|target| loopback_target(&target))
}

fn loopback_target(target: &Target) -> bool {
    target.host.eq_ignore_ascii_case("localhost")
        || target
            .host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// GET or HEAD with a total deadline and redirect cap. `timeout_secs` (at
/// least one second) covers DNS through reading the body, across redirects.
/// The deadline continues while the caller consumes `into_reader()`.
/// `max_redirects = 0`
/// is required to observe Hugging Face `X-Linked-ETag` on the 302.
pub fn http_call(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    timeout_secs: u64,
    max_redirects: u32,
) -> Result<HttpCall, String> {
    http_call_with_body(method, url, headers, None, timeout_secs, max_redirects)
}

/// GET/HEAD/DELETE/POST/PUT/PATCH. `body` is sent for methods that carry
/// one; GET/HEAD ignore it. HTTP error statuses are returned, not `Err`.
/// `timeout_secs` is the total request-and-body budget described by [`http_call`].
#[allow(clippy::too_many_arguments)] // six independent request facts every caller already has; a params struct would only move the count to all ten call sites
pub fn http_call_with_body(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
    timeout_secs: u64,
    max_redirects: u32,
) -> Result<HttpCall, String> {
    let request = Request::new(method, url, headers, body, timeout_secs, max_redirects)?;
    execute(request, None, Proxy::from_env)
}

/// POST a body and read a bounded UTF-8 response.
pub fn http_post_utf8(
    url: &str,
    headers: &[(String, String)],
    body: &[u8],
    timeout_secs: u64,
    max_bytes: u64,
) -> Result<(u16, String), String> {
    let refs: Vec<(&str, &str)> = headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let call = http_call_with_body("POST", url, &refs, Some(body), timeout_secs, 0)?;
    let status = call.status;
    let text = call.into_utf8(max_bytes)?;
    Ok((status, text))
}

/// One logical request, before redirects.
struct Request {
    method: &'static str,
    target: Target,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
    deadline: Instant,
    max_redirects: u32,
}

impl Request {
    #[allow(clippy::too_many_arguments)] // the six facts of the public call, passed through unchanged
    fn new(
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
        timeout_secs: u64,
        max_redirects: u32,
    ) -> Result<Self, String> {
        let method: &'static str = match method {
            "GET" => "GET",
            "HEAD" => "HEAD",
            "DELETE" => "DELETE",
            "POST" => "POST",
            "PUT" => "PUT",
            "PATCH" => "PATCH",
            other => return Err(format!("unsupported HTTP method {other}")),
        };
        let body = match method {
            "GET" | "HEAD" => None,
            "DELETE" => body.map(<[u8]>::to_vec),
            _ => Some(body.unwrap_or_default().to_vec()),
        };
        Ok(Self {
            method,
            target: Target::parse(url)?,
            headers: headers
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
            body,
            deadline: Instant::now() + Duration::from_secs(timeout_secs.max(1)),
            max_redirects,
        })
    }
}

/// Run `request`, following redirects the way the previous client did:
/// any 3xx but 304 is a redirect; 301/302/303 turn every method but HEAD
/// into a bodiless GET; 307/308 keep the method and refuse to resend a
/// body (POST/PUT/PATCH) or a DELETE; `Authorization` and `Cookie` are
/// never carried to the next hop. With `max_redirects = 0` the 3xx is
/// returned; past a non-zero cap the call fails.
///
/// Loopback hops never use a proxy, so local inference and host-control
/// traffic stay on this machine even with HTTP(S)/ALL_PROXY configured.
fn execute(
    mut request: Request,
    connector: Option<&native_tls::TlsConnector>,
    proxy_from_env: fn() -> Option<Proxy>,
) -> Result<HttpCall, String> {
    let mut redirects = 0u32;
    loop {
        connect::remaining(request.deadline).map_err(|e| e.to_string())?;
        let proxy = if loopback_target(&request.target) {
            None
        } else {
            proxy_from_env()
        };
        let call = send_once(&request, proxy.as_ref(), connector)?;
        let redirect = (300..400).contains(&call.status) && call.status != 304;
        if !redirect || request.max_redirects == 0 {
            return Ok(call);
        }
        if redirects >= request.max_redirects {
            return Err(format!(
                "too many redirects (more than {})",
                request.max_redirects
            ));
        }
        let location = call
            .header("location")
            .ok_or_else(|| format!("redirect {} without a Location header", call.status))?
            .to_string();
        let next = request.target.join(&location)?;
        let method = match call.status {
            307 | 308 => {
                if matches!(request.method, "POST" | "PUT" | "PATCH" | "DELETE") {
                    return Err(format!(
                        "redirect failed: {} cannot be resent after {}",
                        request.method, call.status
                    ));
                }
                request.method
            }
            _ if request.method == "HEAD" => "HEAD",
            _ => "GET",
        };
        drop(call);
        request.headers.retain(|(name, _)| {
            !name.eq_ignore_ascii_case("authorization") && !name.eq_ignore_ascii_case("cookie")
        });
        if !matches!(method, "POST" | "PUT" | "PATCH") {
            request.body = None;
        }
        request.method = method;
        request.target = next;
        redirects += 1;
    }
}

/// One request/response exchange on a fresh connection.
fn send_once(
    request: &Request,
    proxy: Option<&Proxy>,
    connector: Option<&native_tls::TlsConnector>,
) -> Result<HttpCall, String> {
    let head = wire::request_head(
        request.method,
        &request.target,
        &request.headers,
        request.body.as_ref().map(Vec::len),
    )?;
    let mut stream = connect::open(&request.target, proxy, connector, request.deadline)?;
    let send = |stream: &mut connect::Conn| -> std::io::Result<()> {
        use std::io::Write;
        stream.write_all(&head)?;
        if let Some(body) = &request.body {
            stream.write_all(body)?;
        }
        stream.flush()
    };
    send(&mut stream).map_err(|e| format!("sending the request: {e}"))?;
    let mut reader = BufReader::new(stream);
    let head = wire::read_head(&mut reader).map_err(|e| format!("reading the response: {e}"))?;
    let framing =
        wire::framing(request.method, &head).map_err(|e| format!("reading the response: {e}"))?;
    Ok(HttpCall {
        status: head.status,
        body: Box::new(BodyReader::new(reader, framing)),
        headers: head.headers,
    })
}

#[cfg(test)]
#[path = "client_wire_tests.rs"]
mod wire_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;

    /// Serve exactly one canned HTTP response, return its base URL.
    ///
    /// The whole request (head and any `Content-Length` body) is read before
    /// replying. A client may send the body in a later segment; closing a
    /// Windows socket with those bytes unread resets the connection, and the
    /// reset discards the reply before the client reads it (os error 10054).
    fn serve_once(response: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let mut request = Vec::new();
                let mut buf = [0u8; 4096];
                let head_end = loop {
                    if let Some(at) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        break Some(at + 4);
                    }
                    match std::io::Read::read(&mut stream, &mut buf) {
                        Ok(0) | Err(_) => break None,
                        Ok(count) => request.extend_from_slice(&buf[..count]),
                    }
                };
                if let Some(head_end) = head_end {
                    let head = String::from_utf8_lossy(&request[..head_end]).to_ascii_lowercase();
                    let length = head
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    while request.len() < head_end + length {
                        match std::io::Read::read(&mut stream, &mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(count) => request.extend_from_slice(&buf[..count]),
                        }
                    }
                }
                let _ = stream.write_all(response.as_bytes());
            }
        });
        format!("http://127.0.0.1:{port}/")
    }

    #[test]
    fn loopback_hosts_are_parsed_without_matching_remote_lookalikes() {
        for url in [
            "http://127.0.0.1:8080/v1",
            "http://localhost/",
            "http://[::1]/",
        ] {
            assert!(loopback_url(url), "{url}");
        }
        for url in [
            "http://127.0.0.1.example/",
            "https://cloud.example/",
            "http://localhost@cloud.example/",
        ] {
            assert!(!loopback_url(url), "{url}");
        }
    }

    #[test]
    fn loopback_requests_bypass_proxy_while_cloud_requests_keep_it() {
        // A subprocess keeps proxy variables and the client pool isolated from
        // concurrent tests. The proxy receives only the final remote request.
        if let Ok(origin) = std::env::var("SUSI_HTTP_TEST_ORIGIN") {
            let health = std::env::var("SUSI_HTTP_TEST_HEALTH").unwrap();
            assert_eq!(
                http_call("GET", &health, &[], 2, 0)
                    .unwrap()
                    .into_utf8(16)
                    .unwrap(),
                "ok"
            );
            let local = http_call_with_body(
                "POST",
                &origin,
                &[("Authorization", "Bearer private-test-token")],
                Some(b"private-goal"),
                2,
                0,
            )
            .unwrap();
            assert_eq!(local.into_utf8(16).unwrap(), "answer");
            let cloud = http_call("GET", "http://cloud.invalid/example", &[], 2, 0).unwrap();
            assert_eq!(cloud.into_utf8(16).unwrap(), "proxy");
            return;
        }
        let origin = serve_once("HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nanswer");
        let health = serve_once("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
        let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
        proxy.set_nonblocking(true).unwrap();
        let proxy_url = format!("http://{}", proxy.local_addr().unwrap());
        let proxy_worker = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            loop {
                match proxy.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        // Read whole header blocks: closing a Windows socket with
                        // unread request bytes resets it before the client reads.
                        let read_head = |stream: &mut std::net::TcpStream, into: &mut String| {
                            let mut bytes = [0; 4096];
                            let start = into.len();
                            while !into[start..].contains("\r\n\r\n") {
                                let count = stream.read(&mut bytes).unwrap();
                                assert!(count > 0, "proxy request ended early");
                                into.push_str(&String::from_utf8_lossy(&bytes[..count]));
                            }
                        };
                        let mut request = String::new();
                        read_head(&mut stream, &mut request);
                        if request.starts_with("CONNECT ") {
                            stream
                                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                                .unwrap();
                            read_head(&mut stream, &mut request);
                        }
                        stream
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nproxy")
                            .unwrap();
                        stream.shutdown(std::net::Shutdown::Write).unwrap();
                        return request;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "expected cloud request through proxy"
                        );
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("proxy accept: {error}"),
                }
            }
        });
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "client::tests::loopback_requests_bypass_proxy_while_cloud_requests_keep_it",
                "--nocapture",
            ])
            .env("SUSI_HTTP_TEST_ORIGIN", origin)
            .env("SUSI_HTTP_TEST_HEALTH", health)
            .env("NO_PROXY", "")
            .env("no_proxy", "")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        for name in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            command.env(name, &proxy_url);
        }
        let mut child = command.spawn().unwrap();
        let started = std::time::Instant::now();
        while child.try_wait().unwrap().is_none() {
            if started.elapsed() > Duration::from_secs(8) {
                let _ = child.kill();
                let _ = child.wait();
                panic!("isolated proxy check exceeded its deadline");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let request = proxy_worker.join().unwrap();
        assert!(
            request.contains("cloud.invalid") && request.contains("/example"),
            "{request}"
        );
        assert!(!request.contains("private-goal") && !request.contains("private-test-token"));
    }

    #[test]
    fn http_call_reports_status_and_headers() {
        let url =
            serve_once("HTTP/1.1 201 Created\r\nContent-Length: 2\r\nX-Susi-Marker: yes\r\n\r\nok");
        let call = http_call("GET", &url, &[], 5, 0).unwrap();
        assert_eq!(call.status, 201);
        assert_eq!(call.header("x-susi-marker"), Some("yes"));
        assert_eq!(call.header("X-SUSI-MARKER"), Some("yes"));
        assert_eq!(call.into_utf8(16).unwrap(), "ok");
    }

    #[test]
    fn non_2xx_status_is_returned_not_raised() {
        let url = serve_once("HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
        let call = http_call("GET", &url, &[], 5, 0).unwrap();
        assert_eq!(call.status, 404);
    }

    #[test]
    fn into_bytes_rejects_bodies_over_the_cap() {
        let url = serve_once("HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\nhello world");
        let call = http_call("GET", &url, &[], 5, 0).unwrap();
        assert!(call.into_bytes(5).is_err());
        let url = serve_once("HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\nhello world");
        let call = http_call("GET", &url, &[], 5, 0).unwrap();
        assert_eq!(call.into_bytes(64).unwrap(), b"hello world");
    }

    /// Keep the fixture bounded even when a missing client deadline regresses.
    fn delayed_response(
        header_delay: Duration,
        chunk_delay: Duration,
        length: usize,
    ) -> (
        String,
        std::sync::mpsc::Sender<()>,
        std::thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let (stop, stopped) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if std::time::Instant::now() >= deadline || stopped.try_recv().is_ok() {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("loopback accept: {error}"),
                }
            };
            // A socket accepted from a non-blocking listener is itself
            // non-blocking on Windows. Left that way, a read racing the
            // client's request fails with WouldBlock, the fixture drops the
            // connection and the client sees os error 10053 instead of the
            // delayed response the test is about.
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            // Read the whole request head before replying: closing a Windows
            // socket with unread request bytes resets it before the client
            // reads the response.
            let mut request = Vec::new();
            let mut bytes = [0; 4096];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                match stream.read(&mut bytes) {
                    Ok(count) if count > 0 => request.extend_from_slice(&bytes[..count]),
                    _ => return,
                }
            }
            if stopped.recv_timeout(header_delay).is_ok() {
                return;
            }
            if write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n"
            )
            .is_err()
            {
                return;
            }
            for _ in 0..length {
                if stopped.recv_timeout(chunk_delay).is_ok() || stream.write_all(b"x").is_err() {
                    return;
                }
            }
        });
        (url, stop, worker)
    }

    #[test]
    fn http_deadline_bounds_stalled_response_headers() {
        let (url, stop, worker) = delayed_response(Duration::from_secs(4), Duration::ZERO, 1);
        let started = std::time::Instant::now();
        let mut request = Request::new("GET", &url, &[], None, 1, 0).unwrap();
        request.deadline = started + Duration::from_millis(150);
        let result = execute(request, None, Proxy::from_env);
        let elapsed = started.elapsed();
        let _ = stop.send(());
        worker.join().unwrap();
        assert!(
            result.is_err(),
            "stalled headers must exhaust the caller budget"
        );
        assert!(
            elapsed < Duration::from_millis(1000),
            "elapsed: {elapsed:?}"
        );
    }

    #[test]
    fn http_deadline_includes_headers_and_trickled_body() {
        let (url, stop, worker) =
            delayed_response(Duration::from_millis(80), Duration::from_millis(20), 50);
        let started = std::time::Instant::now();
        let mut request = Request::new("GET", &url, &[], None, 1, 0).unwrap();
        request.deadline = started + Duration::from_millis(200);
        let call = execute(request, None, Proxy::from_env).unwrap();
        let result = call.into_bytes(50);
        let elapsed = started.elapsed();
        let _ = stop.send(());
        worker.join().unwrap();
        assert!(
            result.is_err(),
            "trickling must not extend the total deadline"
        );
        // A fresh body budget after the 80ms headers would take ~1080ms.
        // Allow scheduling slack while rejecting a per-read deadline reset.
        assert!(
            elapsed >= Duration::from_millis(180),
            "elapsed: {elapsed:?}"
        );
        assert!(elapsed < Duration::from_millis(800), "elapsed: {elapsed:?}");
    }

    #[test]
    fn http_deadline_preserves_longer_caller_stream_budget() {
        let (url, stop, worker) =
            delayed_response(Duration::from_millis(150), Duration::from_millis(160), 6);
        let call = http_call("GET", &url, &[], 4, 0).unwrap();
        let mut bytes = Vec::new();
        let result = call.into_reader().read_to_end(&mut bytes);
        let _ = stop.send(());
        worker.join().unwrap();
        result.unwrap();
        assert_eq!(bytes, b"xxxxxx");
    }
}
