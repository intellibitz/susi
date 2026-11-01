//! Shared outbound HTTP client for every SUSI plane that talks to a remote
//! JSON endpoint (MCP, peer A2A, live search, eval/benchmark, HF download,
//! cloud agent APIs, webhooks).
//!
//! Vendor SDKs and provider-specific wire shapes do **not** live here:
//! OpenAI / Anthropic / Gemini bodies stay in `susi-adapters-llm`; Candle
//! stays in `susi-vendor-candle`. This module privately pools timeout- and
//! redirect-profiled `ureq` agents so callers never depend on the vendor type.

use std::collections::HashMap;
use std::io::Read;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// One completed request: status, headers, and a `Read` body. Callers
/// never name `ureq`. HTTP error statuses are returned, not turned into
/// `Err` — resume downloads need 416 / 206.
pub struct HttpCall {
    /// HTTP status code.
    pub status: u16,
    headers: Vec<(String, String)>,
    body: ureq::Body,
}

impl HttpCall {
    /// First header value matching `name` (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// All response headers as `(name, value)` pairs.
    pub fn headers(&self) -> &[(String, String)] {
        &self.headers
    }

    /// Consume the body as a `Read`.
    pub fn into_reader(self) -> impl Read {
        self.body.into_reader()
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

fn into_call(resp: ureq::http::Response<ureq::Body>) -> HttpCall {
    let status = resp.status().as_u16();
    let headers = resp
        .headers()
        .iter()
        .filter_map(|(k, v)| Some((k.as_str().to_string(), v.to_str().ok()?.to_string())))
        .collect();
    HttpCall {
        status,
        headers,
        body: resp.into_body(),
    }
}

const MAX_AGENT_PROFILES: usize = 16;
type AgentCache = Mutex<HashMap<(u64, u32, bool), ureq::Agent>>;
static HTTP_AGENTS: OnceLock<AgentCache> = OnceLock::new();

fn loopback_url(url: &str) -> bool {
    let Ok(uri) = url.parse::<ureq::http::Uri>() else {
        return false;
    };
    let Some(host) = uri.host() else {
        return false;
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

fn agent_for(timeout_secs: u64, max_redirects: u32, direct: bool) -> ureq::Agent {
    let timeout_secs = timeout_secs.max(1);
    let key = (timeout_secs, max_redirects, direct);
    let mut agents = HTTP_AGENTS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(agent) = agents.get(&key) {
        return agent.clone();
    }
    if agents.len() >= MAX_AGENT_PROFILES {
        agents.clear();
    }
    let timeout = Duration::from_secs(timeout_secs);
    let mut config = ureq::Agent::config_builder()
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::NativeTls)
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        // One caller-selected budget covers DNS, headers, redirects and body
        // reads. Phase-only limits leave response headers unbounded.
        .timeout_global(Some(timeout))
        .timeout_connect(Some(Duration::from_secs(15).min(timeout)))
        .timeout_recv_body(Some(timeout))
        .timeout_send_body(Some(timeout))
        .max_redirects(max_redirects);
    // Local inference and host-control traffic must stay on this machine,
    // even when the parent has an HTTP(S)/ALL_PROXY configured for cloud use.
    if direct {
        config = config.proxy(None);
    }
    let agent = ureq::Agent::new_with_config(config.build());
    agents.insert(key, agent.clone());
    agent
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
    let agent = agent_for(timeout_secs, max_redirects, loopback_url(url));
    match method {
        "HEAD" | "GET" => {
            let mut req = match method {
                "HEAD" => agent.head(url),
                _ => agent.get(url),
            };
            for (name, value) in headers {
                req = req.header(*name, *value);
            }
            let resp = req
                .config()
                .http_status_as_error(false)
                .build()
                .call()
                .map_err(|e| e.to_string())?;
            Ok(into_call(resp))
        }
        "DELETE" => match body {
            None => {
                let mut req = agent.delete(url);
                for (name, value) in headers {
                    req = req.header(*name, *value);
                }
                let resp = req
                    .config()
                    .http_status_as_error(false)
                    .build()
                    .call()
                    .map_err(|e| e.to_string())?;
                Ok(into_call(resp))
            }
            // DELETE may carry a JSON body (e.g. Ollama /api/delete); ureq's
            // typed builder is bodyless so the request is built raw.
            Some(payload) => {
                let mut builder = ureq::http::Request::builder().method("DELETE").uri(url);
                for (name, value) in headers {
                    builder = builder.header(*name, *value);
                }
                let request = builder.body(payload.to_vec()).map_err(|e| e.to_string())?;
                let resp = agent.run(request).map_err(|e| e.to_string())?;
                Ok(into_call(resp))
            }
        },
        "POST" | "PUT" | "PATCH" => {
            let mut req = match method {
                "PUT" => agent.put(url),
                "PATCH" => agent.patch(url),
                _ => agent.post(url),
            };
            for (name, value) in headers {
                req = req.header(*name, *value);
            }
            let payload = body.unwrap_or(&[]);
            let resp = req
                .config()
                .http_status_as_error(false)
                .build()
                .send(payload)
                .map_err(|e| e.to_string())?;
            Ok(into_call(resp))
        }
        other => Err(format!("unsupported HTTP method {other}")),
    }
}

/// POST a body and read a bounded UTF-8 response. Callers never name `ureq`.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;

    /// Serve exactly one canned HTTP response, return its base URL.
    fn serve_once(response: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = std::io::Read::read(&mut stream, &mut buf);
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
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = [0; 4096];
            if stream.read(&mut request).is_err() || stopped.recv_timeout(header_delay).is_ok() {
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
        let result = http_call("GET", &url, &[], 1, 0);
        let elapsed = started.elapsed();
        let _ = stop.send(());
        worker.join().unwrap();
        assert!(
            result.is_err(),
            "stalled headers must exhaust the caller budget"
        );
        assert!(
            elapsed < Duration::from_millis(2500),
            "elapsed: {elapsed:?}"
        );
    }

    #[test]
    fn http_deadline_includes_headers_and_trickled_body() {
        let (url, stop, worker) =
            delayed_response(Duration::from_millis(1200), Duration::from_millis(100), 100);
        let started = std::time::Instant::now();
        let call = http_call("GET", &url, &[], 2, 0).unwrap();
        let result = call.into_bytes(100);
        let elapsed = started.elapsed();
        let _ = stop.send(());
        worker.join().unwrap();
        assert!(
            result.is_err(),
            "trickling must not extend the total deadline"
        );
        assert!(
            elapsed < Duration::from_millis(2800),
            "elapsed: {elapsed:?}"
        );
    }

    #[test]
    fn http_deadline_preserves_longer_caller_stream_budget() {
        let (url, stop, worker) =
            delayed_response(Duration::from_millis(200), Duration::from_millis(200), 6);
        let call = http_call("GET", &url, &[], 4, 0).unwrap();
        let mut bytes = Vec::new();
        let result = call.into_reader().read_to_end(&mut bytes);
        let _ = stop.send(());
        worker.join().unwrap();
        result.unwrap();
        assert_eq!(bytes, b"xxxxxx");
    }
}
