//! Wire, TLS, streaming, redirect and proxy tests for the std client.
//!
//! Every fixture is local: loopback listeners, throwaway certificates
//! minted per run, no provider keys, no network. Every accept, read and
//! join is bounded, so a regression fails the test instead of hanging it.

use super::*;
use std::io::{BufRead, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread::JoinHandle;
use susi_vendor_rcgen as rcgen;

const FIXTURE_BOUND: Duration = Duration::from_secs(10);

/// Accept one connection within [`FIXTURE_BOUND`], or `None`.
fn accept_bounded(listener: &TcpListener) -> Option<TcpStream> {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + FIXTURE_BOUND;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                stream.set_read_timeout(Some(FIXTURE_BOUND)).unwrap();
                stream.set_write_timeout(Some(FIXTURE_BOUND)).unwrap();
                return Some(stream);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => panic!("fixture accept: {e}"),
        }
    }
}

/// Read a request head and any `Content-Length` body.
fn read_request(stream: &mut impl Read) -> Option<String> {
    let mut request = Vec::new();
    let mut byte = [0u8; 1];
    while !request.ends_with(b"\r\n\r\n") {
        match stream.read(&mut byte) {
            Ok(1) => request.push(byte[0]),
            _ => return None,
        }
    }
    let head = String::from_utf8_lossy(&request).to_string();
    let length = head
        .to_ascii_lowercase()
        .lines()
        .find_map(|l| {
            l.strip_prefix("content-length:")
                .map(|v| v.trim().to_string())
        })
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body).ok()?;
    Some(format!("{head}{}", String::from_utf8_lossy(&body)))
}

/// Serve `replies` on consecutive connections; the handle yields every
/// request received. `{port}` in a reply is replaced by the listener port.
fn serve_sequence(replies: Vec<String>) -> (String, JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let worker = std::thread::spawn(move || {
        let mut seen = Vec::new();
        for reply in replies {
            let Some(mut stream) = accept_bounded(&listener) else {
                break;
            };
            let Some(request) = read_request(&mut stream) else {
                break;
            };
            seen.push(request);
            let _ = stream.write_all(reply.replace("{port}", &port.to_string()).as_bytes());
        }
        seen
    });
    (format!("http://127.0.0.1:{port}"), worker)
}

fn call(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
    redirects: u32,
) -> Result<HttpCall, String> {
    let request = Request::new(method, url, headers, body, 5, redirects)?;
    execute(request, None, || None)
}

#[test]
fn chunked_bodies_stream_each_chunk_before_the_response_ends() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/stream", listener.local_addr().unwrap());
    let (read_first, first_was_read) = mpsc::channel::<()>();
    let server = std::thread::spawn(move || {
        let mut stream = accept_bounded(&listener).expect("client connects");
        read_request(&mut stream).expect("request");
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\nb;ext=1\r\ndata: one\n\n\r\n")
            .unwrap();
        // The second event is only sent after the client consumed the
        // first, which proves the reader streams instead of buffering.
        first_was_read
            .recv_timeout(FIXTURE_BOUND)
            .expect("client streamed the first chunk");
        stream
            .write_all(b"B\r\ndata: two\n\n\r\n0\r\nX-Trailer: t\r\n\r\n")
            .unwrap();
    });
    let call = call("GET", &url, &[], None, 0).unwrap();
    assert_eq!(call.header("content-type"), Some("text/event-stream"));
    let mut lines = std::io::BufReader::new(call.into_reader()).lines();
    assert_eq!(lines.next().unwrap().unwrap(), "data: one");
    assert_eq!(lines.next().unwrap().unwrap(), "");
    read_first.send(()).unwrap();
    assert_eq!(lines.next().unwrap().unwrap(), "data: two");
    assert_eq!(lines.next().unwrap().unwrap(), "");
    assert!(lines.next().is_none());
    server.join().unwrap();
}

#[test]
fn a_stalled_stream_fails_with_a_timeout_inside_the_budget() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let (stop, stopped) = mpsc::channel::<()>();
    let server = std::thread::spawn(move || {
        let mut stream = accept_bounded(&listener).expect("client connects");
        read_request(&mut stream).expect("request");
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\npart\r\n")
            .unwrap();
        let _ = stopped.recv_timeout(FIXTURE_BOUND);
    });
    let started = Instant::now();
    let mut request = Request::new("GET", &url, &[], None, 1, 0).unwrap();
    request.deadline = started + Duration::from_millis(150);
    let call = execute(request, None, || None).unwrap();
    let error = call.into_bytes(1024).unwrap_err();
    let elapsed = started.elapsed();
    stop.send(()).unwrap();
    server.join().unwrap();
    assert!(error.to_string().contains("timeout"), "{error}");
    assert!(
        elapsed < Duration::from_millis(1000),
        "elapsed: {elapsed:?}"
    );
}

#[test]
fn truncated_bodies_are_errors_not_short_reads() {
    let (url, server) = serve_sequence(vec![
        "HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc".into()
    ]);
    let error = call("GET", &url, &[], None, 0)
        .unwrap()
        .into_bytes(64)
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof, "{error}");
    server.join().unwrap();
    let (url, server) = serve_sequence(vec![
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nab".into(),
    ]);
    let error = call("GET", &url, &[], None, 0)
        .unwrap()
        .into_bytes(64)
        .unwrap_err();
    assert!(error.to_string().contains("final chunk"), "{error}");
    server.join().unwrap();
}

#[test]
fn close_delimited_bodies_head_requests_and_interim_responses() {
    let (url, server) = serve_sequence(vec![
        "HTTP/1.1 100 Continue\r\n\r\nHTTP/1.0 200 OK\r\nX-Odd: caf\u{e9}\r\n\r\nuntil close"
            .into(),
        "HTTP/1.1 200 OK\r\nContent-Length: 99\r\n\r\n".into(),
    ]);
    let first = call("GET", &url, &[], None, 0).unwrap();
    assert_eq!(first.status, 200);
    // Non-ASCII values are dropped, as http::HeaderValue::to_str dropped them.
    assert_eq!(first.header("x-odd"), None);
    assert_eq!(first.into_utf8(64).unwrap(), "until close");
    let head = call("HEAD", &url, &[], None, 0).unwrap();
    assert_eq!(head.header("content-length"), Some("99"));
    assert_eq!(head.into_bytes(0).unwrap(), b"");
    server.join().unwrap();
}

#[test]
fn request_heads_carry_defaults_framing_and_reject_header_injection() {
    let (url, server) = serve_sequence(vec![
        "HTTP/1.1 204 No Content\r\n\r\n".into(),
        "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".into(),
    ]);
    let posted = call(
        "POST",
        &format!("{url}/v1/chat?x=1#frag"),
        &[("Authorization", "Bearer t"), ("Content-Length", "999")],
        Some(b"{}"),
        0,
    )
    .unwrap();
    assert_eq!(posted.status, 204);
    let got = call(
        "GET",
        &format!("{url}?q"),
        &[("User-Agent", "caller")],
        None,
        0,
    )
    .unwrap();
    assert_eq!(got.status, 200);
    let seen = server.join().unwrap();
    let post = &seen[0];
    assert!(post.starts_with("POST /v1/chat?x=1 HTTP/1.1\r\n"), "{post}");
    assert!(post.contains("\r\nHost: 127.0.0.1:"), "{post}");
    assert!(
        post.contains(&format!("\r\nUser-Agent: {USER_AGENT}\r\n")),
        "{post}"
    );
    assert!(
        post.contains("\r\nContent-Length: 2\r\n") && !post.contains("999"),
        "{post}"
    );
    assert!(post.contains("\r\nConnection: close\r\n") && post.ends_with("\r\n\r\n{}"));
    let get = &seen[1];
    assert!(
        get.starts_with("GET /?q HTTP/1.1\r\n") && !get.contains("Content-Length"),
        "{get}"
    );
    assert!(get.contains("\r\nUser-Agent: caller\r\n") && !get.contains(USER_AGENT));

    for (name, value) in [
        ("X-Bad", "a\r\nInjected: 1"),
        ("Bad Name", "v"),
        ("X-Nul", "a\0b"),
    ] {
        let error = call("GET", "http://127.0.0.1:9/", &[(name, value)], None, 0)
            .err()
            .expect("refused before any socket");
        assert!(
            error.contains("invalid") && error.contains("header"),
            "{error}"
        );
    }
    for url in [
        "ftp://x/",
        "http://",
        "http://a b/",
        "http://[::1/",
        "http://h:99999/",
    ] {
        assert!(call("GET", url, &[], None, 0).is_err(), "{url}");
    }
}

#[test]
fn oversized_response_heads_are_refused() {
    let big = format!(
        "HTTP/1.1 200 OK\r\nX-Big: {}\r\n\r\n",
        "a".repeat(70 * 1024)
    );
    let (url, server) = serve_sequence(vec![big]);
    let error = call("GET", &url, &[], None, 0).err().expect("refused");
    assert!(error.contains("64 KiB"), "{error}");
    server.join().unwrap();
}

#[test]
fn redirects_follow_the_previous_client_rules() {
    // Relative Location, method kept for GET, credentials dropped on redirect.
    let (url, server) = serve_sequence(vec![
        "HTTP/1.1 302 Found\r\nLocation: next/../final?x=1\r\nContent-Length: 0\r\n\r\n".into(),
        "HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\ndone".into(),
    ]);
    let done = call(
        "GET",
        &format!("{url}/dir/start"),
        &[
            ("Authorization", "Bearer secret"),
            ("Cookie", "c=1"),
            ("Proxy-Authorization", "proxy-secret"),
            ("X-Api-Key", "provider-secret"),
            ("X-Goog-API-Key", "google-secret"),
            ("Api-Key", "azure-secret"),
            ("X-Keep", "k"),
        ],
        None,
        3,
    )
    .unwrap();
    assert_eq!(done.into_utf8(16).unwrap(), "done");
    let seen = server.join().unwrap();
    for credential in [
        "Bearer secret",
        "c=1",
        "proxy-secret",
        "provider-secret",
        "google-secret",
        "azure-secret",
    ] {
        assert!(seen[0].contains(credential));
        assert!(!seen[1].contains(credential));
    }
    assert!(
        seen[1].starts_with("GET /dir/final?x=1 HTTP/1.1"),
        "{}",
        seen[1]
    );
    assert!(seen[1].contains("X-Keep"));

    // 303 turns a POST into a bodiless GET at an absolute Location.
    let (url, server) = serve_sequence(vec![
        "HTTP/1.1 303 See Other\r\nLocation: http://127.0.0.1:{port}/after\r\n\r\n".into(),
        "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".into(),
    ]);
    assert_eq!(
        call("POST", &url, &[], Some(b"payload"), 2).unwrap().status,
        200
    );
    let seen = server.join().unwrap();
    assert!(
        seen[1].starts_with("GET /after ") && !seen[1].contains("payload"),
        "{}",
        seen[1]
    );

    // 307 cannot resend a body.
    let (url, server) = serve_sequence(vec![
        "HTTP/1.1 307 Temporary Redirect\r\nLocation: /again\r\n\r\n".into(),
    ]);
    let error = call("POST", &url, &[], Some(b"x"), 2)
        .err()
        .expect("refused");
    assert!(error.contains("redirect failed"), "{error}");
    server.join().unwrap();

    // max_redirects = 0 returns the 3xx so callers can read its headers.
    let (url, server) = serve_sequence(vec![
        "HTTP/1.1 302 Found\r\nLocation: /x\r\nX-Linked-Etag: \"abc\"\r\n\r\n".into(),
    ]);
    let found = call("HEAD", &url, &[], None, 0).unwrap();
    assert_eq!(
        (found.status, found.header("x-linked-etag")),
        (302, Some("\"abc\""))
    );
    server.join().unwrap();

    // Past the cap is an error; a redirect without Location is an error.
    let (url, server) = serve_sequence(vec![
        "HTTP/1.1 301 Moved\r\nLocation: /a\r\n\r\n".into(),
        "HTTP/1.1 301 Moved\r\nLocation: /b\r\n\r\n".into(),
    ]);
    let error = call("GET", &url, &[], None, 1).err().expect("capped");
    assert!(error.contains("too many redirects"), "{error}");
    server.join().unwrap();
    let (url, server) = serve_sequence(vec!["HTTP/1.1 302 Found\r\n\r\n".into()]);
    let error = call("GET", &url, &[], None, 1).err().expect("no location");
    assert!(error.contains("Location"), "{error}");
    server.join().unwrap();
}

#[test]
fn location_resolution_follows_rfc_3986() {
    let base = wire::Target::parse("https://h.example:8443/a/b/c?q=1").unwrap();
    let join = |l: &str| {
        let t = base.join(l).unwrap();
        format!("{}|{}|{}", t.host, t.port, t.path_and_query)
    };
    assert_eq!(join("d"), "h.example|8443|/a/b/d");
    assert_eq!(join("../d?x"), "h.example|8443|/a/d?x");
    assert_eq!(join("/root"), "h.example|8443|/root");
    assert_eq!(join("?z=2"), "h.example|8443|/a/b/c?z=2");
    assert_eq!(join("//cdn.example/f"), "cdn.example|443|/f");
    assert_eq!(join("http://[::1]:81/v"), "::1|81|/v");
    let t = wire::Target::parse("http://user:pw@[::1]/").unwrap();
    assert_eq!(
        (t.host.as_str(), t.port, t.host_header()),
        ("::1", 80, "[::1]".to_string())
    );
}

// --- TLS -------------------------------------------------------------------

struct Pki {
    ca_pem: String,
    leaf_key_pem: String,
    good: String,
    expired: String,
    mismatched: String,
    foreign: String,
}

/// Distinct serial numbers per certificate (rcgen without a crypto
/// provider cannot pick random ones itself).
fn serial() -> rcgen::SerialNumber {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    ((nanos & 0x00ff_ffff_ffff_0000) | n).into()
}

/// A throwaway CA, a leaf for 127.0.0.1 it signs, the same leaf expired,
/// one for another name, and one signed by an unrelated CA.
fn pki() -> &'static Pki {
    static PKI: std::sync::OnceLock<Pki> = std::sync::OnceLock::new();
    PKI.get_or_init(|| {
        let (ca_key, other_key, leaf_key) = std::thread::scope(|scope| {
            let ca = scope.spawn(|| {
                rcgen::generate_simple_self_signed(Vec::<String>::new())
                    .unwrap()
                    .signing_key
            });
            let other = scope.spawn(|| {
                rcgen::generate_simple_self_signed(Vec::<String>::new())
                    .unwrap()
                    .signing_key
            });
            let leaf = rcgen::generate_simple_self_signed(Vec::<String>::new())
                .unwrap()
                .signing_key;
            (ca.join().unwrap(), other.join().unwrap(), leaf)
        });
        let ca_params = |name: &str| {
            let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
            params.serial_number = Some(serial());
            params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
            params.key_usages = vec![
                rcgen::KeyUsagePurpose::KeyCertSign,
                rcgen::KeyUsagePurpose::CrlSign,
            ];
            params
                .distinguished_name
                .push(rcgen::DnType::CommonName, name);
            params
        };
        let trusted = ca_params("susi http-transport test CA");
        let untrusted = ca_params("susi untrusted test CA");
        let ca_pem = trusted.self_signed(&ca_key).unwrap().pem();
        let issuer = rcgen::Issuer::from_params(&trusted, &ca_key);
        let other_issuer = rcgen::Issuer::from_params(&untrusted, &other_key);
        let leaf =
            |names: Vec<String>, expired: bool, by: &rcgen::Issuer<'_, &rcgen::RsaSigningKey>| {
                let mut params = rcgen::CertificateParams::new(names).unwrap();
                params.serial_number = Some(serial());
                if expired {
                    params.not_before = rcgen::date_time_ymd(1999, 1, 1);
                    params.not_after = rcgen::date_time_ymd(2000, 1, 1);
                }
                params.signed_by(&leaf_key, by).unwrap().pem()
            };
        let ip = vec!["127.0.0.1".to_string()];
        Pki {
            good: leaf(ip.clone(), false, &issuer),
            expired: leaf(ip.clone(), true, &issuer),
            mismatched: leaf(vec!["mismatch.invalid".into()], false, &issuer),
            foreign: leaf(ip, false, &other_issuer),
            leaf_key_pem: leaf_key.serialize_pem(),
            ca_pem,
        }
    })
}

/// Serve one HTTPS exchange with a chunked reply. `None` when the
/// handshake fails (the client refused the certificate).
fn https_once(cert_pem: &str, key_pem: &str) -> (u16, JoinHandle<Option<String>>) {
    let identity =
        native_tls::Identity::from_pkcs8(cert_pem.as_bytes(), key_pem.as_bytes()).unwrap();
    let acceptor = native_tls::TlsAcceptor::new(identity).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let worker = std::thread::spawn(move || {
        let stream = accept_bounded(&listener)?;
        let mut tls = acceptor.accept(stream).ok()?;
        let request = read_request(&mut tls)?;
        tls.write_all(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n6\r\nsecure\r\n0\r\n\r\n",
        )
        .ok()?;
        let _ = tls.shutdown();
        Some(request)
    });
    (port, worker)
}

fn trusting(ca_pem: &str) -> native_tls::TlsConnector {
    native_tls::TlsConnector::builder()
        .disable_built_in_roots(true)
        .add_root_certificate(native_tls::Certificate::from_pem(ca_pem.as_bytes()).unwrap())
        .build()
        .unwrap()
}

fn https_get(port: u16, connector: Option<&native_tls::TlsConnector>) -> Result<String, String> {
    let request = Request::new(
        "GET",
        &format!("https://127.0.0.1:{port}/secure"),
        &[],
        None,
        5,
        0,
    )?;
    execute(request, connector, || None)?.into_utf8(64)
}

#[test]
fn tls_verifies_the_chain_the_name_and_the_validity_period() {
    let pki = pki();
    let connector = trusting(&pki.ca_pem);

    let (port, server) = https_once(&pki.good, &pki.leaf_key_pem);
    assert_eq!(https_get(port, Some(&connector)).unwrap(), "secure");
    let request = server
        .join()
        .unwrap()
        .expect("server completed the exchange");
    assert!(request.starts_with("GET /secure HTTP/1.1\r\n"), "{request}");

    for (label, cert) in [
        ("untrusted CA", &pki.foreign),
        ("hostname mismatch", &pki.mismatched),
        ("expired certificate", &pki.expired),
    ] {
        let (port, server) = https_once(cert, &pki.leaf_key_pem);
        let error = https_get(port, Some(&connector)).expect_err(label);
        assert!(error.starts_with("tls: handshake"), "{label}: {error}");
        assert!(
            server.join().unwrap().is_none(),
            "{label}: no request may be sent"
        );
    }
}

#[test]
fn the_production_connector_verifies_against_the_platform_trust_store() {
    // The throwaway CA is in no platform store, so the connector every
    // caller gets must refuse a certificate it issued.
    let pki = pki();
    let (port, server) = https_once(&pki.good, &pki.leaf_key_pem);
    let error = https_get(port, None).expect_err("platform trust must refuse a private CA");
    assert!(error.starts_with("tls: handshake"), "{error}");
    assert!(server.join().unwrap().is_none());
}

/// A CONNECT proxy that tunnels one connection to `upstream`, or answers
/// `refuse` instead.
fn connect_proxy(upstream: u16, refuse: Option<&'static str>) -> (Proxy, JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy = Proxy::parse(
        &format!("http://agent:p%40ss@{}", listener.local_addr().unwrap()),
        None,
    )
    .unwrap();
    let worker = std::thread::spawn(move || {
        let mut client = accept_bounded(&listener).expect("client reaches the proxy");
        let request = read_request(&mut client).expect("CONNECT request");
        if let Some(reply) = refuse {
            client.write_all(reply.as_bytes()).unwrap();
            return request;
        }
        let server = TcpStream::connect(("127.0.0.1", upstream)).unwrap();
        server.set_read_timeout(Some(FIXTURE_BOUND)).unwrap();
        client
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .unwrap();
        let (mut c_read, mut s_write) = (client.try_clone().unwrap(), server.try_clone().unwrap());
        let up = std::thread::spawn(move || {
            let _ = std::io::copy(&mut c_read, &mut s_write);
            let _ = s_write.shutdown(std::net::Shutdown::Write);
        });
        let (mut s_read, mut c_write) = (server, client);
        let _ = std::io::copy(&mut s_read, &mut c_write);
        let _ = c_write.shutdown(std::net::Shutdown::Write);
        up.join().unwrap();
        request
    });
    (proxy, worker)
}

#[test]
fn https_through_a_connect_proxy_keeps_end_to_end_verification() {
    let pki = pki();
    let connector = trusting(&pki.ca_pem);
    let (port, server) = https_once(&pki.good, &pki.leaf_key_pem);
    let (proxy, proxied) = connect_proxy(port, None);
    let target = wire::Target::parse(&format!("https://127.0.0.1:{port}/")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut stream = connect::open(&target, Some(&proxy), Some(&connector), deadline).unwrap();
    stream
        .write_all(b"GET /tunnel HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut reader = std::io::BufReader::new(stream);
    assert_eq!(wire::read_head(&mut reader).unwrap().status, 200);
    drop(reader);
    let connect = proxied.join().unwrap();
    assert!(
        connect.starts_with(&format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n")),
        "{connect}"
    );
    // agent:p@ss
    assert!(
        connect.contains("Proxy-Authorization: Basic YWdlbnQ6cEBzcw=="),
        "{connect}"
    );
    assert!(server.join().unwrap().unwrap().starts_with("GET /tunnel"));

    let (proxy, proxied) = connect_proxy(
        9,
        Some("HTTP/1.1 407 Proxy Authentication Required\r\n\r\n"),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    let error = connect::open(&target, Some(&proxy), Some(&connector), deadline)
        .err()
        .expect("refused tunnel");
    assert!(error.contains("proxy server responded 407"), "{error}");
    proxied.join().unwrap();
}
