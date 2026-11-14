//! A scoped JSON mission may already have spent money when the peer drops
//! its response. Count the inference POSTs on the actual provider connection.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

fn isolated(program: &str, home: &Path) -> Command {
    let mut cmd = Command::new(program);
    cmd.env_clear().current_dir(home).stdin(Stdio::null());
    for name in [
        "PATH",
        "SystemRoot",
        "WINDIR",
        "PATHEXT",
        "ComSpec",
        "TEMP",
        "TMP",
    ] {
        if let Some(value) = std::env::var_os(name) {
            cmd.env(name, value);
        }
    }
    for name in [
        "HOME",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "XDG_CONFIG_HOME",
        "SUSI_HOME",
    ] {
        cmd.env(name, home);
    }
    cmd.env("GIT_CONFIG_NOSYSTEM", "1")
        .env("SUSI_NO_REGISTRY_ENV", "1")
        .env("SUSI_AGENT", "mission-test")
        .env("GIT_TERMINAL_PROMPT", "0");
    cmd
}

struct Gateway(Child);
impl Drop for Gateway {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn default_mission_does_not_repeat_a_consumed_provider_request_after_disconnect() {
    let tmp = susi_paths::tempfile::tempdir().unwrap();
    let home = tmp.path();
    let workspace = home.join("work");
    let board = home.join("board.git");
    let ledger = home.join("ledger.jsonl");
    let hashes = home.join("prices.json");
    let overlay = home.join("providers.json");
    let git = |dir: &Path, args: &[&str]| {
        let result = isolated("git", home)
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    };
    git(home, &["init", "--bare", "board.git"]);
    git(home, &["init", "-b", "main", "work"]);
    git(&workspace, &["config", "user.email", "offline@localhost"]);
    git(&workspace, &["config", "user.name", "offline"]);
    git(&workspace, &["commit", "--allow-empty", "-qm", "fixture"]);
    let provider = TcpListener::bind("127.0.0.1:0").unwrap();
    provider.set_nonblocking(true).unwrap();
    let upstream = provider.local_addr().unwrap();
    std::fs::write(&overlay,json!({"providers":[{
        "id":"fixture-vendor","wire":"openai","endpoint":format!("http://{upstream}/v1/chat/completions"),"key_env":"FIXTURE_API_KEY",
        "models":[{"id":"fixture-worker","backend":"fixture-backend","tools":true,"tier":"worker"}]
    }]}).to_string()).unwrap();
    std::fs::write(&hashes,json!({"schema":"susi/harness/prices/v1","models":{"fixture-worker":{"input_hit":1000,"input_miss":1000,"output":1000}}}).to_string()).unwrap();
    let address = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let gateway = Gateway(
        isolated(env!("CARGO_BIN_EXE_harness-gateway"), home)
            .env("SUSI_HARNESS_PROVIDERS", &overlay)
            .env("SUSI_HARNESS_PREFER", "fixture-vendor")
            .env("FIXTURE_API_KEY", "synthetic-testing-only")
            .env("SUSI_HARNESS_BUDGET_FIXTURE_VENDOR_USD", "2")
            .args([
                "--port",
                &address.port().to_string(),
                "--upstream",
                "direct",
                "--timeout",
                "5",
                "--catalogue",
            ])
            .arg(&hashes)
            .arg("--ledger")
            .arg(&ledger)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while TcpStream::connect(address).is_err() {
        assert!(Instant::now() < deadline, "test gateway did not start");
        std::thread::sleep(Duration::from_millis(20));
    }
    let file = home.join("mission.json");
    std::fs::write(&file,json!({"id":"no-replay","tasks":[{"id":"task","workspace":workspace,"prompt":"Return OK"}],
        "candidates":["byok:auto"],"max_parallel":1,"timeout_secs":12,"gateway_url":format!("http://{address}"),"state_dir":home.join("state"),"ledger_path":ledger}).to_string()).unwrap();
    let running = AtomicBool::new(true);
    let posts = AtomicUsize::new(0);
    let probes = AtomicUsize::new(0);
    let result = std::thread::scope(|scope| {
        scope.spawn(|| {
            while running.load(Ordering::Acquire) {
                let (mut stream,_) = match provider.accept() {
                    Ok(pair)=>pair,
                    Err(e) if e.kind()==std::io::ErrorKind::WouldBlock=> {std::thread::sleep(Duration::from_millis(5));continue}
                    Err(e)=>panic!("fixture accept: {e}"),
                };
                stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                let mut reader=BufReader::new(&stream);
                let mut first=String::new();
                reader.read_line(&mut first).unwrap();
                let mut length=0;
                loop {
                    let mut line=String::new();
                    if reader.read_line(&mut line).unwrap()==0 || line.trim().is_empty(){break;}
                    if let Some((name,value))=line.split_once(':') {
                        if name.eq_ignore_ascii_case("content-length") {length=value.trim().parse::<usize>().unwrap();}
                    }
                }
                if first.starts_with("POST /v1/chat/completions ") {
                    let mut body=vec![0;length];
                    reader.read_exact(&mut body).unwrap();
                    let request:Value=serde_json::from_slice(&body).unwrap();
                    assert_eq!(request["model"],"fixture-backend");
                    posts.fetch_add(1,Ordering::Relaxed);
                    // The provider has consumed the billable request. Close before returning any status.
                } else {
                    assert!(first.starts_with("GET /v1/models "));
                    probes.fetch_add(1,Ordering::Relaxed);
                    let body=r#"{"data":[{"id":"fixture-backend"}]}"#;
                    write!(stream,"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
                }
            }
        });
        let output = isolated(env!("CARGO_BIN_EXE_harness-mission"), home)
            .env("SUSI_BOARD_REMOTE", &board)
            .arg(&file)
            .output();
        running.store(false, Ordering::Release);
        output.unwrap()
    });
    drop(gateway);
    assert_eq!(
        result.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report: Value = serde_json::from_slice(&result.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&result.stderr)));
    assert_eq!(report["failed"], 1);
    assert_eq!(report["tasks"][0]["attempts"].as_array().unwrap().len(), 1);
    assert!(probes.load(Ordering::Relaxed) > 0);
    assert_eq!(posts.load(Ordering::Relaxed), 1);
}
