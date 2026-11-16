//! Offline proof through the shipped ACP command, genuine GAWD/GEMI and a
//! separately compiled Rust MCP child. No executor or authorization is mocked.
//! Run with `cargo test --offline --locked -p susi --test acp_mcp_bridge -- --ignored --nocapture`.
//! Fixtures and failure evidence stay below SUSI_ACP_BRIDGE_TEST_ROOT (or target).
// Integration assertions deliberately fail loudly and preserve their fixture.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const TOOL_DESCRIPTION: &str = "ACP bridge fixture session probe";
const WAIT: Duration = Duration::from_secs(120);

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        #[cfg(unix)]
        let _ = Command::new("kill")
            .env_clear()
            .args(["-KILL", "--", &format!("-{}", self.0.id())])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        #[cfg(windows)]
        let _ = Command::new("taskkill")
            .env_clear()
            .args(["/PID", &self.0.id().to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
    home: PathBuf,
    child: PathBuf,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // Failure cleanup runs only after the ACP process guard is dropped.
        // Verify the exact executable before touching each recorded PID,
        // including helpers in a separately owned MCP process group/job.
        // Successful cases assert reaping before this last-resort cleanup.
        #[cfg(any(target_os = "linux", windows))]
        if let Ok(entries) = fs::read_dir(&self.root) {
            for path in entries.flatten().map(|entry| entry.path()) {
                if path.extension().is_none_or(|ext| ext != "jsonl") {
                    continue;
                }
                let Ok(body) = fs::read_to_string(path) else {
                    continue;
                };
                for row in body
                    .lines()
                    .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                {
                    if row["event"] != "started" {
                        continue;
                    }
                    let Some(pid) = row["data"]["pid"].as_u64() else {
                        continue;
                    };
                    let Ok(pid) = u32::try_from(pid) else {
                        continue;
                    };
                    if susi_config::process_probe::exe_path(pid)
                        .is_some_and(|exe| same_executable(&exe, &self.child))
                    {
                        #[cfg(target_os = "linux")]
                        let _ = Command::new("kill")
                            .env_clear()
                            .args(["-KILL", &pid.to_string()])
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .status();
                        #[cfg(windows)]
                        let _ = self
                            .command("taskkill")
                            .args(["/PID", &pid.to_string(), "/F"])
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .status();
                    }
                }
            }
        }
    }
}
impl Fixture {
    fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut cmd = Command::new(program);
        cmd.env_clear().current_dir(&self.repo).stdin(Stdio::null());
        for key in [
            "PATH",
            "CARGO_HOME",
            "RUSTUP_HOME",
            "SystemRoot",
            "WINDIR",
            "PATHEXT",
            "ComSpec",
        ] {
            if let Some(value) = std::env::var_os(key) {
                cmd.env(key, value);
            }
        }
        susi_paths::test_env::isolate_home(&mut cmd, &self.home);
        cmd.env("SUSI_HOME", self.home.join(".susi"))
            .env("SUSI_AGENT", "acp-bridge-fixture")
            .env("SUSI_BIN", env!("CARGO_BIN_EXE_susi"))
            .env("SUSI_BOARD_REMOTE", self.root.join("board.git"))
            .env("SUSI_PORT_OFFSET", "17000")
            .env("SUSI_ACP_FIXTURE_API_KEY", "synthetic-loopback-only")
            .env("CARGO_NET_OFFLINE", "true")
            .env("RUSTUP_AUTO_INSTALL", "0")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.home.join("gitconfig"))
            .env("GIT_TERMINAL_PROMPT", "0");
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        cmd
    }

    fn git_at(&self, dir: &Path, args: &[&str]) -> String {
        let out = self
            .command("git")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }
    fn assert_local_remotes(&self) {
        assert!(self.root.join(".susi-agent-root").is_file());
        for (repo, remote) in [
            (&self.repo, "origin"),
            (&self.repo, "board"),
            (&self.root.join("susi-commercial-apps"), "origin"),
        ] {
            let url = self.git_at(repo, &["remote", "get-url", remote]);
            let path = Path::new(&url);
            assert!(
                path.is_absolute() && path.starts_with(&self.root) && path.is_dir(),
                "nonlocal fixture remote: {url}"
            );
        }
    }
    fn susi(&self, args: &[&str]) -> Command {
        self.assert_local_remotes();
        let mut cmd = self.command(env!("CARGO_BIN_EXE_susi"));
        cmd.args(args);
        cmd
    }
    fn permission(&self, verb: &str, subject: &str, action: &str) {
        let out = self
            .susi(&[
                "privacy",
                verb,
                "--subject",
                subject,
                "--action",
                action,
                "--resource",
                "*",
            ])
            .output()
            .unwrap();
        // Grant stdout contains signed tokens, so never include it in diagnostics.
        assert!(
            out.status.success(),
            "{verb} {subject} {action}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    fn new(label: &str, endpoint: &str) -> Self {
        assert!(endpoint.starts_with("http://127.0.0.1:"));
        let base = std::env::var_os("SUSI_ACP_BRIDGE_TEST_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                Path::new(env!("CARGO_MANIFEST_DIR")).join("target/acp-bridge-fixtures")
            });
        fs::create_dir_all(&base).unwrap();
        let serial = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = base
            .canonicalize()
            .unwrap()
            .join(format!("{label}-{}-{serial}", std::process::id()));
        let fx = Self {
            repo: root.join("repo"),
            home: root.join("home"),
            child: root.join(format!("mcp-child{}", std::env::consts::EXE_SUFFIX)),
            root,
        };
        fs::create_dir_all(&fx.repo).unwrap();
        fs::create_dir_all(fx.home.join(".susi")).unwrap();
        fs::write(
            fx.root.join(".susi-agent-root"),
            "agent = \"acp-bridge-fixture\"\n",
        )
        .unwrap();
        fs::write(
            fx.repo.join("README.md"),
            "Offline ACP session fixture. Use the supplied probe tool; do not edit files.\n",
        )
        .unwrap();
        fs::write(fx.repo.join(".gitignore"), ".susi/\ntarget/\n").unwrap();
        for remote in ["origin", "board"] {
            fx.git_at(
                &fx.repo,
                &[
                    "init",
                    "--bare",
                    fx.root.join(format!("{remote}.git")).to_str().unwrap(),
                ],
            );
        }
        fx.git_at(&fx.repo, &["init", "-b", "main"]);
        for (key, value) in [
            ("user.name", "ACP bridge fixture"),
            ("user.email", "offline@localhost"),
            ("commit.gpgsign", "false"),
        ] {
            fx.git_at(&fx.repo, &["config", key, value]);
        }
        for remote in ["origin", "board"] {
            fx.git_at(
                &fx.repo,
                &[
                    "remote",
                    "add",
                    remote,
                    fx.root.join(format!("{remote}.git")).to_str().unwrap(),
                ],
            );
        }
        fx.git_at(&fx.repo, &["add", "."]);
        fx.git_at(&fx.repo, &["commit", "-qm", "offline fixture"]);
        fx.git_at(&fx.repo, &["push", "origin", "main"]);
        let board = fx.root.join("susi-commercial-apps");
        fs::create_dir_all(&board).unwrap();
        fx.git_at(&board, &["init", "-b", "main"]);
        fx.git_at(
            &board,
            &[
                "remote",
                "add",
                "origin",
                fx.root.join("board.git").to_str().unwrap(),
            ],
        );
        fx.assert_local_remotes();
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/acp_mcp_bridge/mcp_child.rs");
        let out = fx
            .command("rustc")
            .args(["--edition=2021", "--crate-name", "acp_mcp_fixture"])
            .arg(source)
            .arg("-o")
            .arg(&fx.child)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "MCP fixture compiler: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        // Override every bundled URL, so a discovered credential cannot select
        // a real endpoint. The process environment also contains no real key.
        let mut config: Value =
            serde_json::from_str(include_str!("../config/config.default.json")).unwrap();
        let endpoints = config["inference_endpoints"]["endpoints"]
            .as_array_mut()
            .unwrap();
        for item in endpoints.iter_mut() {
            item["api_base"] = json!("");
        }
        endpoints.push(json!({"name":"ACP fixture", "api_base":endpoint, "protocol_type":"chat", "model":"acp-fixture-model", "api_key_env":"SUSI_ACP_FIXTURE_API_KEY"}));
        fs::write(
            fx.home.join(".susi/config.json"),
            serde_json::to_vec_pretty(&config).unwrap(),
        )
        .unwrap();
        for subject in ["susi", "acp-client"] {
            for action in [
                "filesystem.read",
                "filesystem.write",
                "network.egress",
                "process.exec",
            ] {
                fx.permission("grant", subject, action);
            }
        }
        eprintln!("ACP bridge evidence: {}", fx.root.display());
        fx
    }
    fn descriptor(&self, tag: &str, literal: &str, mode: &str) -> Value {
        json!({"name":"fixture", "command":self.child, "args":[self.root.join(format!("{tag}.jsonl")), mode], "env":[{"name":"SUSI_ACP_FIXTURE_LITERAL", "value":literal}]})
    }
    fn set_config(&self, key: &str, value: Value) {
        let path = self.home.join(".susi/config.json");
        let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        config[key] = value;
        fs::write(path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
    }
    fn trace(&self, tag: &str) -> Vec<Value> {
        fs::read_to_string(self.root.join(format!("{tag}.jsonl")))
            .unwrap_or_default()
            .split_inclusive('\n')
            .filter(|line| line.ends_with('\n'))
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    fn wait_trace(&self, tag: &str, event: &str) -> Vec<Value> {
        let deadline = Instant::now() + WAIT;
        loop {
            let rows = self.trace(tag);
            if rows.iter().any(|v| v["event"] == event) {
                return rows;
            }
            assert!(
                Instant::now() < deadline,
                "missing {event} in {tag}; evidence {}",
                self.root.display()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    fn assert_no_persisted_mcp(&self, literals: &[&str]) {
        // Session configuration must not be serialized into global config or
        // MCP registrations. Ordinary interaction/tool evidence is permitted.
        for path in [
            self.home.join(".susi/config.json"),
            self.home.join(".susi/mcp.json"),
            self.home.join(".susi/mcp_config.json"),
            self.repo.join(".mcp.json"),
        ] {
            if let Ok(body) = fs::read_to_string(&path) {
                assert!(
                    !body.contains("fixture_probe"),
                    "persistent tool registry at {}",
                    path.display()
                );
                let value: Value = serde_json::from_str(&body).unwrap();
                for literal in literals {
                    assert!(
                        !contains_literal(&value, literal),
                        "session env persisted at {}",
                        path.display()
                    );
                }
            }
        }
    }
}

struct Acp {
    process: Process,
    input: Option<ChildStdin>,
    output: mpsc::Receiver<Result<Value, String>>,
    seen: Vec<Value>,
}
impl Acp {
    fn start(fx: &Fixture, tag: &str) -> Self {
        Self::start_with_version(fx, tag, json!(1))
    }
    fn start_with_version(fx: &Fixture, tag: &str, version: Value) -> Self {
        let stderr = fs::File::create(fx.root.join(format!("{tag}.stderr"))).unwrap();
        let mut child = fx
            .susi(&["acp", "serve"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .spawn()
            .unwrap();
        let input = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let transcript = fx.root.join(format!("{tag}.stdout.jsonl"));
        let (tx, output) = mpsc::channel();
        std::thread::spawn(move || {
            let mut trace = fs::File::create(transcript).unwrap();
            for line in BufReader::new(stdout).lines() {
                let parsed = line
                    .map_err(|e| e.to_string())
                    .and_then(|line| {
                        writeln!(trace, "{line}").unwrap();
                        trace.flush().unwrap();
                        serde_json::from_str::<Value>(&line)
                            .map_err(|e| format!("non-JSON ACP stdout: {line:?}: {e}"))
                    })
                    .and_then(|v| {
                        if v["jsonrpc"] == "2.0" {
                            Ok(v)
                        } else {
                            Err(format!("non-JSON-RPC stdout: {v}"))
                        }
                    });
                if tx.send(parsed).is_err() {
                    break;
                }
            }
        });
        let mut acp = Self {
            process: Process(child),
            input,
            output,
            seen: vec![],
        };
        let official = version.is_number();
        let params = if official {
            json!({"protocolVersion":version,"clientCapabilities":{}})
        } else {
            json!({"protocol_version":version})
        };
        let init = acp.request(1, "initialize", params);
        assert_eq!(
            init["result"][if official {
                "protocolVersion"
            } else {
                "protocol_version"
            }],
            version,
            "{init}"
        );
        acp
    }
    fn send(&mut self, value: Value) {
        writeln!(self.input.as_mut().unwrap(), "{value}").unwrap();
        self.input.as_mut().unwrap().flush().unwrap();
    }
    fn response(&mut self, id: u64) -> Value {
        self.response_within(id, WAIT)
    }
    fn response_within(&mut self, id: u64, within: Duration) -> Value {
        let deadline = Instant::now() + within;
        loop {
            let v = self
                .output
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("ACP response deadline / premature EOF")
                .expect("stdout must be pure NDJSON");
            self.seen.push(v.clone());
            if v["id"] == id {
                return v;
            }
        }
    }
    fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
        self.response(id)
    }
    fn session(&mut self, fx: &Fixture, id: u64, servers: Vec<Value>) -> String {
        let reply = self.request(
            id,
            "session/new",
            json!({"cwd":fx.repo,"mcpServers":servers}),
        );
        reply["result"]["sessionId"]
            .as_str()
            .unwrap_or_else(|| panic!("session/new failed: {reply}"))
            .to_owned()
    }
    fn prompt(&mut self, id: u64, sid: &str) -> Value {
        self.request(id, "session/prompt", json!({"sessionId":sid,"prompt":[{"type":"text","text":"Call the ACP bridge fixture session probe tool with marker actual-call. Report its literal and cwd exactly. Do not edit files or run other tools."}]}))
    }
    fn eof(&mut self) {
        self.input.take();
        let deadline = Instant::now() + Duration::from_secs(15);
        while self.process.0.try_wait().unwrap().is_none() {
            assert!(
                Instant::now() < deadline,
                "ACP did not exit after stdin EOF"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        loop {
            match self.output.recv_timeout(Duration::from_secs(5)) {
                Ok(item) => self
                    .seen
                    .push(item.expect("stdout must remain pure NDJSON through shutdown")),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    panic!("ACP stdout reader did not reach EOF")
                }
            }
        }
    }
}

struct Provider {
    url: String,
    stop: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<Value>>>,
    pause_tools: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Provider {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(vec![]));
        let pause_tools = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let recorded = requests.clone();
        let paused = pause_tools.clone();
        let worker = std::thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => Self::respond(stream, &recorded, &paused, &stopped),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5))
                    }
                    Err(e) => panic!("fake provider accept: {e}"),
                }
            }
        });
        Self {
            url,
            stop,
            requests,
            pause_tools,
            worker: Some(worker),
        }
    }
    fn respond(
        mut stream: TcpStream,
        recorded: &Mutex<Vec<Value>>,
        pause_tools: &AtomicBool,
        stop: &AtomicBool,
    ) {
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut first = String::new();
        if reader.read_line(&mut first).unwrap() == 0 {
            return;
        }
        let mut length = 0;
        let mut header_bytes = first.len();
        loop {
            let mut line = String::new();
            assert!(
                reader.read_line(&mut line).unwrap() > 0,
                "EOF inside HTTP headers"
            );
            header_bytes += line.len();
            assert!(header_bytes <= 32768);
            if line == "\r\n" {
                break;
            }
            if let Some((key, value)) = line.split_once(':') {
                if key.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse().unwrap();
                }
            }
        }
        assert!(length <= 1024 * 1024);
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        let reply = if first.starts_with("GET /v1/models ") {
            json!({"object":"list","data":[{"id":"acp-fixture-model","object":"model","owned_by":"offline-fixture"}]})
        } else if first.starts_with("POST /v1/completions ") {
            // Legacy local proxy/discovery callers may use text completions.
            // This reply does not claim tool execution; the acceptance checks
            // still require an actual chat schema, MCP call and tool result.
            let request:Value=serde_json::from_slice(&body).unwrap();
            assert!(request["prompt"].is_string(),"malformed completion request: {request}");
            recorded.lock().unwrap().push(request);
            json!({"id":"offline-text-fixture","object":"text_completion","model":"acp-fixture-model","choices":[{"index":0,"text":"Offline fixture ready. Use the supplied session probe through chat tools.","finish_reason":"stop"}],"usage":{"prompt_tokens":16,"completion_tokens":8}})
        } else {
            assert!(first.starts_with("POST /v1/chat/completions "), "unexpected local provider request: {first}");
            let request: Value = serde_json::from_slice(&body).unwrap(); recorded.lock().unwrap().push(request.clone());
            let tools = request["tools"].as_array();
            let probe = tools.and_then(|v| v.iter().find(|t| is_probe(t)));
            if probe.is_some() { while pause_tools.load(Ordering::SeqCst) && !stop.load(Ordering::SeqCst) { std::thread::sleep(Duration::from_millis(5)); } }
            let result = request["messages"].as_array().and_then(|m| m.iter().rev().take_while(|m|m["role"]!="user").find(|m| m["role"] == "tool"));
            let message = if let Some(result) = result { json!({"role":"assistant","content":format!("ACP_BRIDGE_RESULT {}", result["content"])}) }
            else if let Some(probe) = probe {
                assert_eq!(probe["function"]["parameters"]["properties"]["marker"]["type"], "string");
                assert_eq!(probe["function"]["parameters"]["required"], json!(["marker"]));
                json!({"role":"assistant","content":null,"tool_calls":[{"id":"fixture-call","type":"function","function":{"name":probe["function"]["name"],"arguments":"{\"marker\":\"actual-call\"}"}}]})
            } else { json!({"role":"assistant","content":"Offline fixture ready. Use the supplied session probe to answer."}) };
            json!({"id":"offline-fixture","object":"chat.completion","model":"acp-fixture-model","choices":[{"index":0,"finish_reason":if message["tool_calls"].is_array(){"tool_calls"}else{"stop"},"message":message}],"usage":{"prompt_tokens":16,"completion_tokens":8}})
        }.to_string();
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",reply.len()).unwrap();
        stream.flush().unwrap();
    }
    fn finish(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("loopback provider assertions");
        }
    }
}
impl Drop for Provider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[test]
#[ignore = "real shipped-binary GAWD/GEMI/MCP integration; isolated offline processes"]
fn supplied_mcp_tools_are_called_and_same_cwd_sessions_are_isolated() {
    let mut provider = Provider::start();
    let fx = Fixture::new("isolated-tools", &provider.url);
    let mut acp = Acp::start(&fx, "acp");
    let literals = [
        "alpha:${HOME}:$UNCHANGED\\literal\nsecond line",
        "beta:${HOME}:$UNCHANGED\\literal",
    ];
    let first = acp.session(
        &fx,
        2,
        vec![fx.descriptor("alpha", literals[0], "normal-descendant")],
    );
    let second = acp.session(&fx, 3, vec![fx.descriptor("beta", literals[1], "normal")]);
    assert_ne!(first, second);
    let mut receipt_owners = HashMap::<String, String>::new();
    let mut used_receipts = HashSet::new();
    for (id, sid, tag, literal) in [
        (4, &first, "alpha", literals[0]),
        (5, &second, "beta", literals[1]),
        (6, &first, "alpha", literals[0]),
    ] {
        let called_before = fx
            .trace(tag)
            .iter()
            .filter(|row| row["event"] == "called")
            .count();
        let update_start = acp.seen.len();
        let reply = acp.prompt(id, sid);
        assert!(reply.get("error").is_none(), "prompt failed: {reply}");
        assert_eq!(reply["result"]["stopReason"], "end_turn", "{reply}");
        let rows = fx.trace(tag);
        assert!(
            rows.iter().filter(|row| row["event"] == "called").count() > called_before,
            "turn {id} reused an old tool result without a fresh MCP call"
        );
        for row in rows
            .iter()
            .filter(|v| v["event"] == "started" || v["event"] == "called")
        {
            assert_eq!(row["data"]["literal"], literal);
            assert_eq!(row["data"]["cwd"], fx.repo.to_str().unwrap());
        }
        assert!(rows.iter().any(|v| v["event"] == "request"
            && v["data"]["method"] == "tools/call"
            && v["data"]["params"]["arguments"]["marker"] == "actual-call"));
        let expected = &rows.iter().find(|row| row["event"] == "started").unwrap()["data"];
        let graph: Vec<Value> = fs::read_to_string(fx.home.join(".susi/context_graph.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        for receipt in
            assert_session_tool_evidence(&acp.seen[update_start..], sid, expected, &graph)
        {
            assert!(
                used_receipts.insert(receipt.clone()),
                "turn {id} cited an earlier turn's receipt"
            );
            let mission = receipt.rsplit_once(':').unwrap().0.to_owned();
            if let Some(owner) = receipt_owners.insert(mission, sid.clone()) {
                assert_eq!(
                    owner, *sid,
                    "same-cwd sessions shared a mission evidence owner"
                );
            }
        }
    }
    // A third session in the same process has no supplied tools. Its model
    // request must not inherit either live session's private catalog.
    let empty = acp.session(&fx, 7, vec![]);
    let before = provider.requests.lock().unwrap().len();
    let reply = acp.prompt(8, &empty);
    assert!(
        reply.get("error").is_none(),
        "empty-session prompt: {reply}"
    );
    {
        let requests = provider.requests.lock().unwrap();
        assert!(
            requests.len() > before,
            "empty session never reached the local provider"
        );
        assert!(
            requests[before..].iter().all(|r| !has_probe(r)),
            "session MCP catalog leaked to an empty session"
        );
    }
    acp.eof();
    provider.finish();
    assert_child_gone(&fx, "alpha");
    assert_descendant_gone(&fx, "alpha");
    assert_child_gone(&fx, "beta");
    let requests = provider.requests.lock().unwrap();
    assert!(
        requests.iter().any(|r| r["tools"]
            .as_array()
            .is_some_and(|ts| ts.iter().any(is_probe))),
        "real model never received session schema"
    );
    fx.assert_no_persisted_mcp(&literals);
}

fn assert_session_tool_evidence(
    frames: &[Value],
    session: &str,
    expected: &Value,
    graph: &[Value],
) -> Vec<String> {
    // This vertical slice uses GEMI's existing result/evidence rendering.
    // Typed ACP tool updates and model continuation with role=tool messages
    // remain separate work; this is not a full ACP conformance assertion.
    let mut text = String::new();
    for frame in frames
        .iter()
        .filter(|frame| frame["method"] == "session/update")
    {
        assert_eq!(
            frame["params"]["sessionId"], session,
            "tool result reached another ACP session"
        );
        let update = &frame["params"]["update"];
        if update["sessionUpdate"] == "agent_message_chunk" {
            text.push_str(update["content"]["text"].as_str().unwrap());
        }
    }
    let mut receipts = Vec::new();
    for block in text.split("\n\n") {
        let Some((header, body)) = block.split_once("\nTool reported:\n") else {
            continue;
        };
        let fields: Vec<_> = header.split(" | ").collect();
        assert_eq!(fields.len(), 6, "malformed host evidence header: {header}");
        let tool = fields[0].strip_prefix("Source: ").unwrap();
        let receipt = fields[1].strip_prefix("receipt ").unwrap();
        let mission = receipt.rsplit_once(':').unwrap().0;
        let arguments: Value =
            serde_json::from_str(fields[3].strip_prefix("arguments ").unwrap()).unwrap();
        assert_eq!(arguments, json!({"marker":"actual-call"}));
        assert_eq!(fields[4], "selector entire response");
        let raw = body
            .lines()
            .map(|line| {
                line.strip_prefix("> ")
                    .expect("quoted host-captured output")
            })
            .collect::<Vec<_>>()
            .join("\n");
        let result: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(result["isError"], false, "MCP returned an error");
        let content = result["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");
        // Parse both JSON layers; substring matching would miss changed
        // backslashes/newlines or allow another session's literal to pass.
        let actual: Value = serde_json::from_str(content[0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(
            &actual, expected,
            "ACP session received another child's cwd/env/PID or changed its exact result"
        );
        assert_eq!(
            fields[5].strip_prefix("output_hash ").unwrap(),
            format!("{:x}", Sha256::digest(raw.as_bytes())),
            "evidence hash does not bind the displayed tool result"
        );
        let call = graph
            .iter()
            .map(|row| &row["node_added"])
            .find(|node| node["kind"] == "tool_call" && node["properties"]["receipt_id"] == receipt)
            .expect("receipt has a persisted tool-call node");
        assert_eq!(call["properties"]["tool"], tool);
        let owner = graph
            .iter()
            .map(|row| &row["node_added"])
            .find(|node| node["kind"] == "mission" && node["properties"]["mission_id"] == mission)
            .expect("receipt belongs to a recorded mission");
        assert!(
            graph.iter().any(|row| row["edge_added"]["kind"] == "called"
                && row["edge_added"]["source"] == owner["id"]
                && row["edge_added"]["target"] == call["id"]),
            "tool receipt is not linked to its owning mission evidence"
        );
        receipts.push(receipt.to_owned());
    }
    assert!(
        !receipts.is_empty(),
        "ACP session update contained no exact MCP result with owning evidence"
    );
    receipts
}

#[test]
#[ignore = "real shipped-binary empty MCP session prompt and pure NDJSON stdout"]
fn empty_mcp_session_prompt_uses_the_real_executor_with_pure_stdout() {
    let mut provider = Provider::start();
    let fx = Fixture::new("empty-mcp", &provider.url);
    let mut acp = Acp::start(&fx, "acp");
    let sid = acp.session(&fx, 2, vec![]);
    let reply = acp.prompt(3, &sid);
    assert_eq!(reply["result"]["stopReason"], "end_turn", "{reply}");
    assert!(
        acp.seen.iter().any(|row| row["method"] == "session/update"),
        "empty session emitted no result update"
    );
    acp.eof();
    provider.finish();
    let requests = provider.requests.lock().unwrap();
    assert!(
        requests
            .iter()
            .any(|request| request["messages"].is_array()),
        "empty session never reached the real configured chat provider"
    );
    assert!(
        requests.iter().all(|request| !has_probe(request)),
        "empty session received a supplied MCP schema"
    );
}

#[test]
#[ignore = "real shipped-binary legacy ACP prompt preserves pure NDJSON and lifecycle"]
fn legacy_empty_mcp_prompt_preserves_real_execution_and_pure_stdout() {
    let mut provider = Provider::start();
    let fx = Fixture::new("legacy-empty-mcp", &provider.url);
    let mut acp = Acp::start_with_version(&fx, "acp", json!("2026-11-11"));
    let created = acp.request(2, "session/new", json!({"workspace":fx.repo}));
    let sid = created["result"]["session_id"]
        .as_str()
        .unwrap_or_else(|| panic!("legacy session/new failed: {created}"));
    let reply=acp.request(3,"session/prompt",json!({"session_id":sid,"prompt":"Report that the offline fixture is ready. Do not edit files or invoke tools."}));
    assert_eq!(reply["result"]["stop_reason"], "end_turn", "{reply}");
    assert!(
        reply["result"]["output"]
            .as_array()
            .is_some_and(|blocks| blocks.iter().any(|block| block["text"]
                .as_str()
                .is_some_and(|text| !text.trim().is_empty()))),
        "legacy prompt returned no text: {reply}"
    );
    let listed = acp.request(4, "session/list", json!({}));
    assert!(
        listed["result"]["sessions"]
            .as_array()
            .is_some_and(|sessions| sessions
                .iter()
                .any(|session| session["session_id"] == sid && session["turns_completed"] == 1)),
        "legacy completed turn missing from session/list: {listed}"
    );
    let closed = acp.request(5, "session/close", json!({"session_id":sid}));
    assert_eq!(closed["result"]["closed"], true, "{closed}");
    acp.eof();
    provider.finish();
    let requests = provider.requests.lock().unwrap();
    assert!(
        requests
            .iter()
            .any(|request| request["messages"].is_array()),
        "legacy prompt never reached the real chat provider"
    );
    assert!(
        requests.iter().all(|request| !has_probe(request)),
        "legacy empty session received a private MCP tool"
    );
}

#[test]
#[ignore = "real shipped-binary MCP ownership cleanup during asynchronous initialization"]
fn eof_cleans_up_a_pending_mcp_initialization() {
    let mut provider = Provider::start();
    let fx = Fixture::new("pending-init", &provider.url);
    let mut acp = Acp::start(&fx, "acp");
    acp.send(json!({"jsonrpc":"2.0","id":2,"method":"session/new","params":{"cwd":fx.repo,"mcpServers":[fx.descriptor("pending","literal-pending","pending-init-descendant")]}}));
    fx.wait_trace("pending", "blocked");
    fx.wait_trace("pending.helper", "started");
    acp.eof();
    provider.finish();
    assert_child_gone(&fx, "pending");
    assert_descendant_gone(&fx, "pending");
    fx.assert_no_persisted_mcp(&["literal-pending"]);
}

#[test]
#[ignore = "real shipped-binary setup deadline closes MCP descendants holding pipes"]
fn initialization_deadline_stops_an_mcp_child_and_its_descendant() {
    let mut provider = Provider::start();
    let fx = Fixture::new("init-deadline", &provider.url);
    fx.set_config("cloud_scout_timeout_secs", json!(1));
    let mut acp = Acp::start(&fx, "acp");
    acp.send(json!({"jsonrpc":"2.0","id":2,"method":"session/new","params":{"cwd":fx.repo,"mcpServers":[fx.descriptor("timeout","literal-timeout","pending-init-descendant")]}}));
    fx.wait_trace("timeout", "blocked");
    let reply = acp.response_within(2, Duration::from_secs(8));
    assert!(
        reply["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("lease expired")),
        "setup did not report its deadline: {reply}"
    );
    assert_child_gone(&fx, "timeout");
    assert_descendant_gone(&fx, "timeout");
    // A failed initializer must not destroy the ACP connection or reserve a
    // phantom session: a fresh empty session is still admitted.
    acp.session(&fx, 3, vec![]);
    acp.eof();
    provider.finish();
}

#[test]
#[ignore = "real shipped-binary MCP leader exit with a descendant holding pipes"]
fn exited_mcp_parent_does_not_leave_a_descendant_holding_its_pipes() {
    let mut provider = Provider::start();
    let fx = Fixture::new("parent-exit", &provider.url);
    fx.set_config("cloud_scout_timeout_secs", json!(20));
    let mut acp = Acp::start(&fx, "acp");
    acp.send(json!({"jsonrpc":"2.0","id":2,"method":"session/new","params":{"cwd":fx.repo,"mcpServers":[fx.descriptor("exited","literal-exited","parent-exit-descendant")]}}));
    fx.wait_trace("exited.helper", "started");
    // This bound is deliberately shorter than the configured setup lease:
    // observing the leader's exit must itself close the held pipe tree.
    let reply = acp.response_within(2, Duration::from_secs(5));
    assert!(
        reply.get("error").is_some(),
        "exited MCP leader was admitted: {reply}"
    );
    assert_child_gone(&fx, "exited");
    assert_descendant_gone(&fx, "exited");
    acp.eof();
    provider.finish();
}

#[test]
#[ignore = "real shipped-binary MAC revocation after MCP initialization"]
fn permission_revocation_prevents_an_initialized_mcp_tool_call() {
    let mut provider = Provider::start();
    let fx = Fixture::new("revoke", &provider.url);
    let mut acp = Acp::start(&fx, "acp");
    let sid = acp.session(
        &fx,
        2,
        vec![fx.descriptor("revoked", "literal-revoked", "normal")],
    );
    fx.wait_trace("revoked", "started");
    provider.pause_tools.store(true, Ordering::SeqCst);
    acp.send(json!({"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":sid,"prompt":[{"type":"text","text":"Call the ACP bridge fixture session probe tool with marker actual-call."}]}}));
    let deadline = Instant::now() + WAIT;
    while !provider.requests.lock().unwrap().iter().any(has_probe) {
        assert!(
            Instant::now() < deadline,
            "revocation test never reached a model with the session tool"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    // Revoke after the live model has received the schema but before its tool
    // request reaches the executor, exercising authorization at fulfillment.
    fx.permission("revoke", "acp-client", "process.exec");
    provider.pause_tools.store(false, Ordering::SeqCst);
    let reply = acp.response(3);
    assert!(
        reply.get("error").is_some() || reply["result"]["stopReason"] == "end_turn",
        "unexpected revoked result: {reply}"
    );
    assert!(
        !fx.trace("revoked").iter().any(|v| v["event"] == "called"),
        "revoked session executed its MCP tool"
    );
    assert!(
        provider.requests.lock().unwrap().iter().any(has_probe),
        "revocation test never reached a model with the session tool"
    );
    acp.eof();
    provider.finish();
    assert_child_gone(&fx, "revoked");
}

#[test]
#[ignore = "real shipped-binary cancellation of a blocked MCP tool call"]
fn cancellation_interrupts_a_pending_mcp_tool_call() {
    let mut provider = Provider::start();
    let fx = Fixture::new("cancel", &provider.url);
    let mut acp = Acp::start(&fx, "acp");
    let sid = acp.session(
        &fx,
        2,
        vec![fx.descriptor("cancel", "literal-cancel", "pending-call-descendant")],
    );
    acp.send(json!({"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":sid,"prompt":[{"type":"text","text":"Call the ACP bridge fixture session probe tool with marker actual-call."}]}}));
    fx.wait_trace("cancel", "blocked");
    fx.wait_trace("cancel.helper", "started");
    acp.send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":sid}}));
    let reply = acp.response(3);
    assert_eq!(reply["result"]["stopReason"], "cancelled", "{reply}");
    assert_child_gone(&fx, "cancel");
    assert_descendant_gone(&fx, "cancel");
    acp.eof();
    provider.finish();
}

fn has_probe(request: &Value) -> bool {
    request["tools"]
        .as_array()
        .is_some_and(|ts| ts.iter().any(is_probe))
}

fn is_probe(tool: &Value) -> bool {
    // The bridge may prefix server/tool provenance to the original description.
    tool["function"]["description"]
        .as_str()
        .is_some_and(|description| description.ends_with(TOOL_DESCRIPTION))
}

fn contains_literal(value: &Value, literal: &str) -> bool {
    match value {
        Value::String(text) => {
            text == literal
                || serde_json::from_str::<Value>(text)
                    .ok()
                    .is_some_and(|v| contains_literal(&v, literal))
        }
        Value::Array(items) => items.iter().any(|v| contains_literal(v, literal)),
        Value::Object(fields) => fields.values().any(|v| contains_literal(v, literal)),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

fn assert_child_gone(fx: &Fixture, tag: &str) {
    assert_recorded_process_stopped(fx, tag, false);
}

fn assert_descendant_gone(fx: &Fixture, tag: &str) {
    let helper = format!("{tag}.helper");
    let heartbeat = fx.root.join(format!("{helper}.heartbeat"));
    assert!(
        fs::metadata(&heartbeat).is_ok_and(|m| m.len() > 0),
        "descendant never produced a heartbeat"
    );
    assert_recorded_process_stopped(fx, &helper, true);
    let stopped = fs::metadata(&heartbeat).unwrap().len();
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        fs::metadata(heartbeat).unwrap().len(),
        stopped,
        "MCP descendant continued running after cleanup"
    );
}

fn same_executable(actual: &Path, expected: &Path) -> bool {
    let (Ok(actual), Ok(expected)) = (actual.canonicalize(), expected.canonicalize()) else {
        return false;
    };
    #[cfg(windows)]
    {
        actual
            .as_os_str()
            .eq_ignore_ascii_case(expected.as_os_str())
    }
    #[cfg(not(windows))]
    {
        actual == expected
    }
}

fn assert_recorded_process_stopped(fx: &Fixture, tag: &str, descendant: bool) {
    let rows = fx.trace(tag);
    let pid = u32::try_from(
        rows.iter().find(|r| r["event"] == "started").unwrap()["data"]["pid"]
            .as_u64()
            .unwrap(),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let live = susi_config::process_probe::pid_alive(pid)
            .expect("native process liveness is available");
        // The host owns/reaps the direct child. An orphaned helper may remain
        // a dead zombie until the OS/container's init reaps it; no code or
        // handles survive that state. Do not call a running helper cleaned up.
        #[cfg(target_os = "linux")]
        let dead_orphan = descendant
            && fs::read_to_string(format!("/proc/{pid}/stat"))
                .ok()
                .and_then(|stat| {
                    stat.rsplit_once(") ")
                        .map(|(_, tail)| tail.starts_with("Z ") || tail.starts_with("X "))
                })
                .unwrap_or(false);
        #[cfg(not(target_os = "linux"))]
        let dead_orphan = {
            let _ = descendant;
            false
        };
        if !live || dead_orphan {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "owned MCP process {tag}/{pid} remained alive after cleanup"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
