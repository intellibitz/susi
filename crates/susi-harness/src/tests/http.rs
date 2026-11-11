use super::*;

#[test]
fn http_dates_are_imf_fixdate() {
    assert_eq!(http_date(UNIX_EPOCH), "Thu, 01 Jan 1970 00:00:00 GMT");
    let at = UNIX_EPOCH + Duration::from_secs(1_794_355_200);
    assert_eq!(http_date(at), "Wed, 11 Nov 2026 00:00:00 GMT");
    let leap = UNIX_EPOCH + Duration::from_secs(951_782_400 + 3_661);
    assert_eq!(http_date(leap), "Tue, 29 Feb 2000 01:01:01 GMT");
}

#[test]
fn header_values_read_back_as_text_only_when_visible_ascii() {
    assert!(HeaderValue::from_str("a\u{7f}").is_err());
    let accented = HeaderValue::from_str("caf\u{e9}").unwrap();
    assert!(accented.to_str().is_err());
    assert_eq!(
        HeaderValue::from_static("a b\tc").to_str().unwrap(),
        "a b\tc"
    );
}

#[test]
fn inserting_a_header_replaces_every_value_in_the_first_ones_place() {
    let mut headers = HeaderMap::new();
    headers.append("A", HeaderValue::from_static("1"));
    headers.append("b", HeaderValue::from_static("2"));
    headers.append("a", HeaderValue::from_static("3"));
    headers.insert("a", HeaderValue::from_static("4"));
    let all: Vec<(&str, &[u8])> = headers.iter().map(|(n, v)| (n, v.as_bytes())).collect();
    assert_eq!(all, vec![("a", &b"4"[..]), ("b", &b"2"[..])]);
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

fn read_answer(stream: &mut TcpStream) -> String {
    let mut text = Vec::new();
    let mut buffer = [0u8; 1024];
    while let Ok(read) = stream.read(&mut buffer) {
        if read == 0 {
            break;
        }
        text.extend_from_slice(&buffer[..read]);
        let head_end = text.windows(4).position(|w| w == b"\r\n\r\n");
        if let Some(end) = head_end {
            let head = String::from_utf8_lossy(&text[..end]).to_ascii_lowercase();
            let length: usize = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .and_then(|value| value.trim().parse().ok())
                .unwrap_or(0);
            if text.len() >= end + 4 + length {
                break;
            }
        }
    }
    String::from_utf8_lossy(&text).into_owned()
}

/// On shutdown the server stops accepting, closes a connection that waits
/// between requests at once, answers the request in flight (closing its
/// connection after) and returns once that connection is done.
#[test]
fn shutdown_finishes_the_request_in_flight_and_closes_idle_connections() {
    let runtime = runtime();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let router = Router::new().route("/fast", get(|_| async { "ok" })).route(
        "/slow",
        get(|_| async {
            tokio::time::sleep(Duration::from_millis(400)).await;
            "done"
        }),
    );
    let (address, server) = runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(serve(listener, router, Peer::Withhold, async {
            let _ = stopped.await;
        }));
        (address, server)
    });
    let timeout = Some(Duration::from_secs(5));
    let mut idle = TcpStream::connect(address).unwrap();
    idle.set_read_timeout(timeout).unwrap();
    idle.write_all(b"GET /fast HTTP/1.1\r\nhost: x\r\n\r\n")
        .unwrap();
    assert!(read_answer(&mut idle).ends_with("ok"));
    let mut busy = TcpStream::connect(address).unwrap();
    busy.set_read_timeout(timeout).unwrap();
    busy.write_all(b"GET /slow HTTP/1.1\r\nhost: x\r\n\r\n")
        .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    stop.send(()).unwrap();

    let mut rest = [0u8; 1];
    assert_eq!(
        idle.read(&mut rest).unwrap(),
        0,
        "the idle connection is closed"
    );
    let answer = read_answer(&mut busy);
    assert!(answer.contains("\r\nconnection: close\r\n"), "{answer}");
    assert!(answer.ends_with("done"), "{answer}");
    assert_eq!(
        busy.read(&mut rest).unwrap(),
        0,
        "then its connection closes"
    );
    runtime
        .block_on(async { tokio::time::timeout(Duration::from_secs(5), server).await })
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        TcpStream::connect_timeout(&address, Duration::from_secs(1)).is_err(),
        "nothing listens any more"
    );
}

/// A client that hangs up while its handler runs has the handler dropped,
/// as hyper dropped it; what it spawned detached would go on.
#[test]
fn a_handler_whose_client_hangs_up_is_dropped() {
    struct Dropped(Arc<AtomicBool>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let runtime = runtime();
    let dropped = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&dropped);
    let router = Router::new().route(
        "/wait",
        post(move |_| {
            let guard = Dropped(Arc::clone(&seen));
            async move {
                tokio::time::sleep(Duration::from_secs(30)).await;
                drop(guard);
                "late"
            }
        }),
    );
    let address = runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(serve(
            listener,
            router,
            Peer::Withhold,
            std::future::pending(),
        ));
        address
    });
    let mut client = TcpStream::connect(address).unwrap();
    client
        .write_all(b"POST /wait HTTP/1.1\r\nhost: x\r\ncontent-length: 2\r\n\r\nhi")
        .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        !dropped.load(Ordering::SeqCst),
        "running while the client waits"
    );
    drop(client);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !dropped.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        dropped.load(Ordering::SeqCst),
        "dropped once the client went"
    );
}
