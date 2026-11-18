//! Opt-in, offline execution proof. Build the genuine coordinator first:
//! `cargo build --offline --locked -p xtask --bin local-workflow`, then run
//! `cargo test --offline --locked -p susi --test harness_mcp_execution_parity -- --ignored --nocapture`.
//! Core forwards `susi harness` to Susi Commercial Apps, so `susi-commercial` (with
//! `core-harness`) must be on PATH or named by SUSI_COMMERCIAL_BIN.
//! Requires the reviewed compiler and authenticated rust-src used by `susi local`.
//! `SUSI_HARNESS_MCP_PROVIDER` selects an absolute standard stdio provider
//! executable exposing the worker filesystem tool contracts.
//! Every process has a cleared environment, private home and local Git remotes.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const INITIAL: &str = "pub fn answer() -> u32 {\n    41\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn accepted_change() {\n        assert_eq!(super::answer(), 42);\n    }\n}\n";

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
        let _ = susi_paths::console::background_command("taskkill")
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
    workflow: PathBuf,
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
            // `susi harness` runs in Susi Commercial Apps (Mandate 84): core
            // finds `susi-commercial` here or on PATH.
            "SUSI_COMMERCIAL_BIN",
        ] {
            if let Some(value) = std::env::var_os(key) {
                cmd.env(key, value);
            }
        }
        susi_paths::test_env::isolate_home(&mut cmd, &self.home);
        cmd.env("SUSI_HOME", self.home.join(".susi"))
            .env("SUSI_AGENT", "harness-parity")
            .env("SUSI_BIN", env!("CARGO_BIN_EXE_susi"))
            .env("SUSI_PARITY_WORKFLOW_BIN", &self.workflow)
            .env("SUSI_BOARD_REMOTE", self.root.join("board.git"))
            .env("SUSI_HARNESS_PROVIDERS", self.root.join("providers.json"))
            .env("SUSI_HARNESS_PREFER", "fixture-vendor")
            .env("FIXTURE_API_KEY", "synthetic-local-fixture-only")
            .env("SUSI_HARNESS_BUDGET_FIXTURE_VENDOR_USD", "25")
            .env("CARGO_NET_OFFLINE", "true")
            .env("RUSTUP_AUTO_INSTALL", "0")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.home.join("gitconfig"))
            .env("GIT_ALLOW_PROTOCOL", "file")
            .env("GIT_TERMINAL_PROMPT", "0");
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        cmd
    }

    fn git(&self, args: &[&str]) -> String {
        let output = self.command("git").args(args).output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn assert_local_remotes(&self) {
        for remote in ["origin", "board"] {
            let url = self.git(&["remote", "get-url", remote]);
            let path = Path::new(&url);
            assert!(
                path.is_absolute() && path.starts_with(&self.root) && path.is_dir(),
                "nonlocal {remote}: {url}"
            );
        }
    }

    fn susi(&self, args: &[&str]) -> Command {
        self.assert_local_remotes();
        let mut cmd = self.command(env!("CARGO_BIN_EXE_susi"));
        cmd.args(args);
        cmd
    }

    fn setup(root: PathBuf, workflow: PathBuf) -> Self {
        let fixture = Self {
            repo: root.join("repo"),
            home: root.join("home"),
            root,
            workflow,
        };
        fs::create_dir_all(fixture.repo.join("src")).unwrap();
        fs::create_dir_all(&fixture.home).unwrap();
        fs::create_dir_all(fixture.repo.join("xtask/src/bin")).unwrap();
        fs::write(fixture.repo.join("Cargo.toml"), "[workspace]\nmembers = [\"xtask\", \"crates/susi-vendor-sha2\", \"crates/susi-vendor-serde-json\"]\nresolver = \"2\"\n\n[workspace.lints.rust]\nunsafe_code = \"forbid\"\n\n[package]\nname = \"harness-parity-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\nlicense = \"Apache-2.0\"\n").unwrap();
        fs::write(fixture.repo.join("src/lib.rs"), INITIAL).unwrap();
        fs::write(fixture.repo.join(".gitignore"), "/target/\n").unwrap();
        fs::write(
            fixture.repo.join("deny.toml"),
            "[licenses]\nallow = [\"Apache-2.0\", \"MIT\", \"Unicode-3.0\", \"BSD-3-Clause\"]\n",
        )
        .unwrap();
        fs::write(
            fixture.repo.join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"1.99.0\"\n",
        )
        .unwrap();
        fs::write(fixture.repo.join("xtask/Cargo.toml"), "[package]\nname = \"xtask\"\nversion = \"0.1.0\"\nedition = \"2021\"\nlicense = \"Apache-2.0\"\n").unwrap();
        // This forwards to the genuine coordinator. It supplies no gate verdicts.
        fs::write(fixture.repo.join("xtask/src/bin/local-workflow.rs"), "fn main() {\n    let program = std::env::var_os(\"SUSI_PARITY_WORKFLOW_BIN\").expect(\"genuine coordinator\");\n    assert!(std::path::Path::new(&program).is_absolute());\n    let status = std::process::Command::new(program)\n        .args(std::env::args_os().skip(1))\n        .status()\n        .expect(\"run genuine coordinator\");\n    std::process::exit(status.code().unwrap_or(1));\n}\n").unwrap();
        let source = Path::new(env!("CARGO_MANIFEST_DIR"));
        for name in [
            "builder.rs",
            "native_target.rs",
            "platform_unicode.rs",
            "consumer.rs",
            "Cargo.toml",
            "Cargo.lock",
        ] {
            copy(
                source,
                &fixture.repo,
                &format!("toolchain/windows-stdlib/{name}"),
            );
        }
        for facade in ["susi-vendor-sha2", "susi-vendor-serde-json"] {
            for file in ["Cargo.toml", "src/lib.rs"] {
                copy(source, &fixture.repo, &format!("crates/{facade}/{file}"));
            }
        }
        for remote in ["origin", "board"] {
            let output = fixture
                .command("git")
                .args(["init", "--bare"])
                .arg(fixture.root.join(format!("{remote}.git")))
                .output()
                .unwrap();
            assert!(output.status.success());
        }
        fixture.git(&["init", "-b", "main"]);
        fixture.git(&["config", "user.name", "Harness parity fixture"]);
        fixture.git(&["config", "user.email", "offline@localhost"]);
        fixture.git(&["config", "commit.gpgsign", "false"]);
        for remote in ["origin", "board"] {
            fixture.git(&[
                "remote",
                "add",
                remote,
                fixture.root.join(format!("{remote}.git")).to_str().unwrap(),
            ]);
        }
        let locked = fixture
            .command("cargo")
            .args(["generate-lockfile", "--offline"])
            .output()
            .unwrap();
        assert!(
            locked.status.success(),
            "{}",
            String::from_utf8_lossy(&locked.stderr)
        );
        fixture.git(&["add", "."]);
        fixture.git(&["commit", "-qm", "offline fixture baseline"]);
        fixture.git(&["push", "origin", "main"]);
        fixture.assert_local_remotes();
        fixture
    }

    fn run_paths(&self, gateway: &str) -> Value {
        json!({"repo":self.repo,"plan":self.root.join("plan.json"),"state":self.root.join("state.json"),"gateway":gateway,
            "model":"byok:auto","workers":"auto","timeout":300,"parallel":1,"prefix":"parity","attempts":1,
            "catalogue":self.root.join("prices.json"),"ledger":self.root.join("ledger.jsonl"),"controls":self.root.join("controls.jsonl"),
            "adapter_state":self.root.join("adapter"),"rsi_log":self.root.join("rsi.jsonl"),"budget_usd":"5","daily_usd":"25"})
    }

    fn preflight(&self) {
        let output = self.susi(&["local", "workflow", "list"]).output().unwrap();
        fs::write(self.root.join("bootstrap.stdout"), &output.stdout).unwrap();
        fs::write(self.root.join("bootstrap.stderr"), &output.stderr).unwrap();
        assert!(
            output.status.success(),
            "real native bootstrap refused; fixture retained at {}:\n{}",
            self.root.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        let baseline = self
            .command("cargo")
            .args([
                "test",
                "--offline",
                "--locked",
                "-p",
                "harness-parity-fixture",
            ])
            .output()
            .unwrap();
        fs::write(self.root.join("baseline.stdout"), &baseline.stdout).unwrap();
        fs::write(self.root.join("baseline.stderr"), &baseline.stderr).unwrap();
        assert!(
            !baseline.status.success(),
            "unfixed baseline unexpectedly passed"
        );
        let failure = String::from_utf8_lossy(&baseline.stdout);
        for expected in [
            "tests::accepted_change ... FAILED",
            "left: 41",
            "right: 42",
            "test result: FAILED. 0 passed; 1 failed",
        ] {
            assert!(
                failure.contains(expected),
                "baseline must fail its acceptance assertion, not setup: {failure}\n{}",
                String::from_utf8_lossy(&baseline.stderr)
            );
        }
    }

    fn configure(&self, upstream: &str) {
        assert!(upstream.starts_with("http://127.0.0.1:"));
        let provider = PathBuf::from(
            std::env::var_os("SUSI_HARNESS_MCP_PROVIDER")
                .expect("set SUSI_HARNESS_MCP_PROVIDER to the standard stdio provider executable"),
        );
        assert!(provider.is_absolute() && provider.is_file());
        let config = self.home.join(".susi");
        fs::create_dir_all(&config).unwrap();
        fs::write(
            config.join("mcp_config.json"),
            serde_json::to_vec(&json!({
                "mcp_servers":{"fixture-provider":{"command":provider,"args":["--stdio"]}}
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(self.root.join("providers.json"), json!({"providers":[{"id":"fixture-vendor","wire":"openai","endpoint":upstream,"key_env":"FIXTURE_API_KEY","models":[{"id":"fixture-model","backend":"fixture-backend","tools":true,"tier":"worker"}]}]}).to_string()).unwrap();
        fs::write(self.root.join("prices.json"), json!({"schema":"susi/harness/prices/v1","models":{"fixture-model":{"input_hit":1000000,"input_miss":1000000,"output":1000000},"byok:auto":{"input_hit":1000000,"input_miss":1000000,"output":1000000}}}).to_string()).unwrap();
        fs::write(self.root.join("plan.json"), json!({"mission":"parity","tasks":[{"key":"fix","title":"Correct answer without deleting its test","accept":"cargo test --offline --locked -p harness-parity-fixture","scopes":["src"],"deps":[],"prompt":"Change answer() from 41 to 42 in src/lib.rs, preserving every test."}]}).to_string()).unwrap();
        for action in [
            "filesystem.read",
            "filesystem.write",
            "network.egress",
            "process.exec",
        ] {
            let output = self
                .susi(&["privacy", "grant", "--action", action, "--resource", "*"])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "grant {action}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    fn gateway(&self) -> (Process, String) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let child = self
            .susi(&[
                "harness",
                "gateway",
                "--host",
                "127.0.0.1",
                "--port",
                &address.port().to_string(),
                "--upstream",
                "direct",
                "--timeout",
                "10",
                "--catalogue",
            ])
            .arg(self.root.join("prices.json"))
            .arg("--ledger")
            .arg(self.root.join("ledger.jsonl"))
            .stdout(Stdio::null())
            .stderr(fs::File::create(self.root.join("gateway.stderr")).unwrap())
            .spawn()
            .unwrap();
        let mut child = Process(child);
        let deadline = Instant::now() + Duration::from_secs(30);
        while TcpStream::connect(address).is_err() {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "gateway stopped: {}",
                fs::read_to_string(self.root.join("gateway.stderr")).unwrap()
            );
            assert!(
                Instant::now() < deadline,
                "gateway startup exceeded deadline"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        (child, format!("http://{address}"))
    }

    fn direct(&self, gateway: &str, name: &str) -> Value {
        let options = self.run_paths(gateway);
        let mut command = self.susi(&["harness", "run"]);
        command.arg(self.root.join("plan.json"));
        for (key, value) in options.as_object().unwrap() {
            if key == "plan" {
                continue;
            }
            command.arg(format!("--{}", key.replace('_', "-"))).arg(
                value
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| value.to_string()),
            );
        }
        let stdout = self.root.join(format!("{name}.stdout"));
        let stderr = self.root.join(format!("{name}.stderr"));
        let mut child = Process(
            command
                .stdout(fs::File::create(&stdout).unwrap())
                .stderr(fs::File::create(&stderr).unwrap())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(600);
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "direct execution exceeded deadline; see {}",
                stderr.display()
            );
            std::thread::sleep(Duration::from_millis(25));
        };
        let text = fs::read_to_string(&stdout).unwrap();
        assert!(
            status.success(),
            "direct run failed at {}:\n{text}\n{}",
            self.root.display(),
            fs::read_to_string(&stderr).unwrap()
        );
        serde_json::Deserializer::from_str(&text)
            .into_iter::<Value>()
            .next()
            .unwrap()
            .unwrap()
    }

    fn ledger(&self) -> Value {
        // The ledger's rows as written; none may be torn. (The harness and its
        // ledger reader live in Susi Commercial Apps since Mandate 84.)
        let text = fs::read_to_string(self.root.join("ledger.jsonl")).unwrap_or_default();
        let mut value = Value::Array(
            text.lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .collect(),
        );
        for row in value.as_array_mut().unwrap() {
            let row = row.as_object_mut().unwrap();
            row.remove("started_unix_ms");
            row.remove("ended_unix_ms");
        }
        value
    }

    fn accepted(&self, before: &str) {
        assert_eq!(
            self.git(&["rev-parse", "main"]),
            before,
            "integration must preserve primary main"
        );
        let accepted = self.git(&["show", "refs/susi/integration/current:src/lib.rs"]);
        assert_eq!(accepted, INITIAL.replace("    41", "    42").trim_end());
        let state: Value =
            serde_json::from_slice(&fs::read(self.root.join("state.json")).unwrap()).unwrap();
        assert_eq!(state["records"]["fix"]["status"], "done", "{state}");
        let task = state["records"]["fix"]["task"].as_str().unwrap();
        assert!(
            self.root
                .join("adapter/fix")
                .join(task)
                .join("attempt-1/result.json")
                .is_file(),
            "real worker result is missing"
        );
        assert!(
            !self
                .git(&[
                    "for-each-ref",
                    "--format=%(refname)",
                    "refs/susi/integration/receipts"
                ])
                .is_empty(),
            "native integration receipt missing"
        );
    }
}

fn copy(source: &Path, destination: &Path, relative: &str) {
    let target = destination.join(relative);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::copy(source.join(relative), target).unwrap();
}

struct Provider {
    url: String,
    calls: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Provider {
    fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!(
            "http://{}/v1/chat/completions",
            listener.local_addr().unwrap()
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (count, ended) = (Arc::clone(&calls), Arc::clone(&stop));
        let worker = std::thread::spawn(move || {
            while !ended.load(Ordering::SeqCst) {
                let Ok((mut socket, peer)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                };
                assert!(peer.ip().is_loopback());
                socket
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let mut input = BufReader::new(socket.try_clone().unwrap());
                let mut first = String::new();
                assert!(
                    input.read_line(&mut first).unwrap() > 0,
                    "provider request EOF"
                );
                let models = first.starts_with("GET /v1/models ");
                assert!(
                    models || first.starts_with("POST /v1/chat/completions "),
                    "unexpected provider request {first}"
                );
                let mut length = 0;
                let mut header_bytes = first.len();
                loop {
                    let mut line = String::new();
                    let read = input.read_line(&mut line).unwrap();
                    assert!(read > 0, "provider header EOF");
                    header_bytes += read;
                    assert!(header_bytes < 32 * 1024, "provider headers too large");
                    if line == "\r\n" || line == "\n" {
                        break;
                    }
                    if let Some((key, value)) = line.split_once(':') {
                        if key.eq_ignore_ascii_case("content-length") {
                            length = value.trim().parse::<usize>().unwrap();
                        }
                    }
                }
                if models {
                    assert_eq!(length, 0);
                    let body = json!({"object":"list","data":[{"id":"fixture-backend","object":"model","owned_by":"fixture-vendor"}]}).to_string();
                    write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
                    continue;
                }
                assert!(length > 0 && length < 1024 * 1024);
                let mut body = vec![0; length];
                input.read_exact(&mut body).unwrap();
                let request: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(request["model"], "fixture-backend");
                let inference = request
                    .get("tools")
                    .and_then(Value::as_array)
                    .is_some_and(|tools| !tools.is_empty());
                let turn = if inference {
                    count.fetch_add(1, Ordering::SeqCst)
                } else {
                    usize::MAX
                };
                let message = if !inference {
                    json!({"role":"assistant","content":"READY"})
                } else if turn == 0 {
                    json!({"role":"assistant","tool_calls":[{"id":"edit","type":"function","function":{"name":"replace_file_content","arguments":json!({"path":"src/lib.rs","target":"    41","replacement":"    42"}).to_string()}}]})
                } else {
                    assert_eq!(turn, 1, "a consumed task was replayed");
                    assert!(request["messages"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|m| m["role"] == "tool"));
                    json!({"role":"assistant","content":"Changed answer to 42 without removing its test."})
                };
                let body = json!({"id":format!("fixture-{turn}"),"object":"chat.completion","choices":[{"index":0,"message":message,"finish_reason":if turn==0 {"tool_calls"} else {"stop"}}],"usage":{"prompt_tokens":10,"completion_tokens":5}}).to_string();
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
            }
        });
        Self {
            url,
            calls,
            stop,
            worker: Some(worker),
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

struct Session {
    _process: Process,
    stdin: std::process::ChildStdin,
    replies: mpsc::Receiver<Value>,
    next: u64,
}
impl Session {
    fn spawn(fixture: &Fixture) -> Self {
        let mut child = fixture
            .susi(&["mcp", "serve"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(fs::File::create(fixture.root.join("mcp.stderr")).unwrap())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, replies) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else {
                    break;
                };
                if let Ok(value) = serde_json::from_str::<Value>(&line) {
                    if tx.send(value).is_err() {
                        break;
                    }
                }
            }
        });
        let mut s = Self {
            _process: Process(child),
            stdin,
            replies,
            next: 0,
        };
        let init = s.request("initialize", json!({"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"harness-parity","version":"1"}}));
        assert_eq!(init["protocolVersion"], "2025-11-25");
        writeln!(
            s.stdin,
            "{}",
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .unwrap();
        s.stdin.flush().unwrap();
        s
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let reply = self.response(method, params, Instant::now() + Duration::from_secs(90));
        assert!(reply.get("error").is_none(), "JSON-RPC error: {reply}");
        reply["result"].clone()
    }

    fn response(&mut self, method: &str, params: Value, deadline: Instant) -> Value {
        self.next += 1;
        writeln!(
            self.stdin,
            "{}",
            json!({"jsonrpc":"2.0","id":self.next,"method":method,"params":params})
        )
        .unwrap();
        self.stdin.flush().unwrap();
        loop {
            assert!(Instant::now() < deadline, "bounded MCP response");
            let reply = self
                .replies
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("bounded MCP response");
            if reply.get("id") == Some(&json!(self.next)) {
                return reply;
            }
        }
    }

    fn tool_names(&mut self) -> Vec<String> {
        let deadline = Instant::now() + Duration::from_secs(90);
        let mut names = Vec::new();
        let mut cursor = None;
        for _ in 0..50 {
            assert!(Instant::now() < deadline, "bounded tools/list catalog");
            let reply = self.response(
                "tools/list",
                cursor
                    .as_ref()
                    .map_or_else(|| json!({}), |cursor: &Value| json!({"cursor":cursor})),
                deadline,
            );
            // Discovery can change the revision-bound catalog between pages.
            // Only this read-only rejection permits a fresh listing.
            if cursor.is_some()
                && reply["error"]["code"] == -32602
                && reply["error"]["message"] == "Catalog changed; restart listing without a cursor"
            {
                names.clear();
                cursor = None;
                continue;
            }
            assert!(reply.get("error").is_none(), "JSON-RPC error: {reply}");
            let page = &reply["result"];
            names.extend(
                page["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|tool| tool["name"].as_str().unwrap().to_string()),
            );
            cursor = page.get("nextCursor").cloned();
            match cursor.as_ref() {
                None => return names,
                Some(Value::String(_)) => {}
                Some(_) => panic!("invalid tools/list cursor: {page}"),
            }
        }
        panic!("tools/list must finish within 50 pages including catalog restarts")
    }

    fn call(&mut self, name: &str, arguments: Value) -> Value {
        let answer = self.request("tools/call", json!({"name":name,"arguments":arguments}));
        assert_eq!(answer["isError"], false, "{name}: {answer}");
        serde_json::from_str(answer["content"][0]["text"].as_str().unwrap()).unwrap()
    }

    fn run(&mut self, fixture: &Fixture, gateway: &str) -> Value {
        let started = self.call("harness_run", fixture.run_paths(gateway));
        assert_eq!(started["started"], true);
        assert_eq!(started["state"], json!(fixture.root.join("state.json")));
        let deadline = Instant::now() + Duration::from_secs(600);
        loop {
            let runs = self.call("harness_runs", json!({}));
            let run = runs["result"]
                .as_array()
                .unwrap()
                .iter()
                .find(|run| run["mission"] == "parity")
                .unwrap();
            if run["running"] == false {
                fs::write(
                    fixture.root.join("mcp-outcome.json"),
                    serde_json::to_vec_pretty(run).unwrap(),
                )
                .unwrap();
                assert!(
                    run["outcome"]["result"].get("error").is_none(),
                    "MCP execution failed: {run}"
                );
                assert_eq!(run["outcome"]["result"]["failure"], Value::Null, "{run}");
                return run["outcome"]["result"]["report"].clone();
            }
            assert!(
                Instant::now() < deadline,
                "MCP mission exceeded deadline: {run}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

#[test]
#[ignore = "real offline native workflow/sysroot qualification and child-process MCP parity"]
fn accepted_task_matches_direct_and_mcp_and_is_not_replayed() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("harness-parity-{}-{stamp}", std::process::id()));
    let workflow = std::env::var_os("SUSI_PARITY_WORKFLOW_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join(format!("local-workflow{}", std::env::consts::EXE_SUFFIX))
        });
    assert!(
        workflow.is_absolute() && workflow.is_file(),
        "build the genuine local-workflow executable first: {}",
        workflow.display()
    );
    eprintln!("harness parity evidence: {}", root.display());
    let direct = Fixture::setup(root.join("direct"), workflow.clone());
    direct.preflight();
    // The preflight above deliberately refuses missing/unreviewed source. Never
    // replace native qualification with a test-produced success response.
    let mcp = Fixture::setup(root.join("mcp"), workflow);
    mcp.preflight();
    let direct_provider = Provider::spawn();
    direct.configure(&direct_provider.url);
    let (_direct_gateway, direct_url) = direct.gateway();
    let direct_before = direct.git(&["rev-parse", "main"]);
    let direct_report = direct.direct(&direct_url, "first");
    assert_eq!(direct_report["done"], json!(["fix"]));
    direct.accepted(&direct_before);
    let direct_ledger = direct.ledger();
    assert_eq!(direct_provider.calls.load(Ordering::SeqCst), 2);
    let direct_replay = direct.direct(&direct_url, "replay");
    assert_eq!(direct_replay["skipped"], json!(["fix"]));
    for key in ["done", "failed", "unstarted"] {
        assert_eq!(direct_replay[key], json!([]));
    }
    assert_eq!(direct_provider.calls.load(Ordering::SeqCst), 2);
    assert_eq!(direct.ledger(), direct_ledger);

    let mcp_provider = Provider::spawn();
    mcp.configure(&mcp_provider.url);
    let (_mcp_gateway, mcp_url) = mcp.gateway();
    let mcp_before = mcp.git(&["rev-parse", "main"]);
    let mut session = Session::spawn(&mcp);
    let names = session.tool_names();
    for name in ["harness_run", "harness_runs", "harness_status"] {
        assert!(names.iter().any(|n| n == name));
    }
    assert!(
        !names
            .iter()
            .any(|name| name == "harness_command" || name == "harness_cancel_run"),
        "record new control exposure explicitly"
    );
    let mut override_options = mcp.run_paths(&mcp_url);
    override_options["susi"] = json!("/untrusted/override");
    let refused = session.request(
        "tools/call",
        json!({"name":"harness_run","arguments":override_options}),
    );
    assert_eq!(
        refused["isError"], true,
        "socket executable override must remain refused: {refused}"
    );
    assert!(
        refused.to_string().contains("susi cannot be chosen"),
        "{refused}"
    );
    fs::write(mcp.root.join("unsupported-and-error.json"), serde_json::to_vec_pretty(&json!({"unsupported_controls":["harness_command","harness_cancel_run"],"executable_override":refused,"cancellation_claim":"Not proven: no dedicated MCP cancel tool; ordinary run scope is checked before and after execution."})).unwrap()).unwrap();
    let mcp_report = session.run(&mcp, &mcp_url);
    assert_eq!(mcp_report, direct_report);
    mcp.accepted(&mcp_before);
    let mcp_ledger = mcp.ledger();
    assert_eq!(mcp_ledger, direct_ledger);
    assert_eq!(mcp_provider.calls.load(Ordering::SeqCst), 2);
    let mcp_replay = session.run(&mcp, &mcp_url);
    assert_eq!(mcp_replay, direct_replay);
    assert_eq!(mcp_provider.calls.load(Ordering::SeqCst), 2);
    assert_eq!(mcp.ledger(), mcp_ledger);
    let status = session.call("harness_status", json!({"ledger":mcp.root.join("ledger.jsonl"),"state":mcp.root.join("state.json"),"plan":mcp.root.join("plan.json")}));
    let mission = status["missions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|mission| mission["mission"] == "parity")
        .unwrap();
    assert_eq!(mission["state"], "done");
    assert_eq!(mission["done"], 1);
    assert_eq!(mission["failed"], 0);
    assert_eq!(mission["calls"], 2);
    assert_eq!(mission["cost_micro_usd"], 30);
    fs::write(root.join("evidence.json"), serde_json::to_vec_pretty(&json!({"direct_report":direct_report,"mcp_report":mcp_report,"direct_replay":direct_replay,"mcp_replay":mcp_replay,"ledger":mcp_ledger,"status":status,"direct_inference_posts":2,"mcp_inference_posts":2,"replay_inference_posts":0,"external_endpoints_configured":false,"bootstrap":"real tracked sysroot builder and genuine native coordinator; fixture trampoline only forwards args and status","default_state_paths":"Explicit paths used; current CLI/socket default-path mismatch is not normalized or claimed fixed."})).unwrap()).unwrap();
}
