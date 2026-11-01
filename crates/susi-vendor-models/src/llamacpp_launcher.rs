//! llama-server launch planner — reads a GGUF's header, derives `-ngl` /
//! `-c` / `-t` from the host's memory and cores, and produces the argv and
//! health-check URL used to start the server. The optional installed-runner
//! adapter owns its authenticated loopback child and never downloads a runner.

use crate::gguf_inspector;
use crate::quant_recommender::Host;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};
use susi_error::{EaiError, EaiResult};

type ChildAdopter = fn(&std::process::Child) -> bool;
static ADOPTER: OnceLock<ChildAdopter> = OnceLock::new();
static OWNED: Mutex<Option<Arc<OwnedRunner>>> = Mutex::new(None);
static FAILED: Mutex<Option<(std::path::PathBuf, Instant)>> = Mutex::new(None);

/// The host supplies its existing process-lifetime ownership mechanism.
pub fn set_child_adopter(adopter: ChildAdopter) {
    let _ = ADOPTER.set(adopter);
}

/// A lease keeps the server alive while an inference request uses it, even if
/// another request selects a different model. Credentials are never Debug-printed.
#[derive(Clone)]
pub struct OwnedEndpoint {
    pub api_base: String,
    pub api_key: String,
    pub model: String,
    _runner: Arc<OwnedRunner>,
}

struct OwnedRunner {
    child: Mutex<std::process::Child>,
    path: std::path::PathBuf,
    stamp: (u64, SystemTime, String),
    context: u64,
    api_base: String,
    api_key: String,
    _files: PrivateFiles,
}

struct PrivateFiles(std::path::PathBuf);

impl Drop for PrivateFiles {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.0.join("key"));
        let _ = std::fs::remove_file(self.0.join("entropy"));
        let _ = std::fs::remove_dir(&self.0);
    }
}

impl Drop for OwnedRunner {
    fn drop(&mut self) {
        let child = self.child.get_mut().unwrap_or_else(|e| e.into_inner());
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl OwnedRunner {
    fn running(&self) -> bool {
        self.child
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .try_wait()
            .is_ok_and(|status| status.is_none())
    }

    fn endpoint(self: &Arc<Self>) -> OwnedEndpoint {
        OwnedEndpoint {
            api_base: self.api_base.clone(),
            api_key: self.api_key.clone(),
            model: "susi-owned-local".into(),
            _runner: Arc::clone(self),
        }
    }
}

pub fn shutdown_owned() {
    let previous = OWNED.lock().unwrap_or_else(|e| e.into_inner()).take();
    drop(previous);
}

pub fn owned_resident(path: &Path) -> bool {
    owned_endpoint(path).is_some()
}

pub fn owned_endpoint(path: &Path) -> Option<OwnedEndpoint> {
    let owned = OWNED.lock().unwrap_or_else(|e| e.into_inner());
    let runner = owned.as_ref()?;
    let meta = path.metadata().ok()?;
    (runner.path == path
        && runner.stamp
            == (
                meta.len(),
                meta.modified().ok()?,
                susi_config::file_node(&meta),
            )
        && runner.running())
    .then(|| runner.endpoint())
}

/// Use the existing OpenAI wire format/parser, with one bounded local request
/// and the same greedy decoding/output budget as native inference.
pub fn generate_owned(path: &Path, prompt: &str, max_tokens: u32) -> EaiResult<Option<String>> {
    let Some(endpoint) = owned_endpoint(path) else {
        return Ok(None);
    };
    let mut body = susi_core::inference_wire::openai_chat_body(&endpoint.model, prompt, max_tokens);
    body["temperature"] = serde_json::json!(0);
    body["messages"] = serde_json::json!([
        {"role": "system", "content": "You are a helpful assistant."},
        {"role": "user", "content": prompt}
    ]);
    let payload = serde_json::to_vec(&body).map_err(|e| EaiError::inference(e.to_string()))?;
    let authorization = format!("Bearer {}", endpoint.api_key);
    let result = (|| {
        let response = susi_http_transport::http_call_with_body(
            "POST",
            &format!("{}/chat/completions", endpoint.api_base),
            &[
                ("Authorization", &authorization),
                ("Content-Type", "application/json"),
            ],
            Some(&payload),
            20,
            0,
        )
        .map_err(EaiError::inference)?;
        if !(200..300).contains(&response.status) {
            return Err(EaiError::inference(format!(
                "owned llama.cpp returned HTTP {}",
                response.status
            )));
        }
        let bytes = response
            .into_bytes(16 * 1024 * 1024)
            .map_err(|e| EaiError::inference(e.to_string()))?;
        let json = serde_json::from_slice(&bytes)
            .map_err(|e| EaiError::inference(format!("owned llama.cpp JSON: {e}")))?;
        let answer =
            susi_core::inference_wire::openai_chat_text(&json).map_err(EaiError::inference)?;
        if answer.trim().is_empty() {
            return Err(EaiError::inference("owned llama.cpp returned no answer"));
        }
        Ok(Some(answer))
    })();
    if result.is_err() {
        *FAILED.lock().unwrap_or_else(|e| e.into_inner()) = Some((path.to_owned(), Instant::now()));
        shutdown_owned();
    }
    result
}

/// Reuse an already installed CPU runner for small GGUFs. The measured startup
/// path avoids GPU shader compilation; larger models retain the native selector.
/// Missing executables or absent host ownership support simply leave native
/// inference available. No model or runner is downloaded by this adapter.
pub fn ensure_owned(path: &Path, requested_context: u64) -> EaiResult<Option<OwnedEndpoint>> {
    use crate::local_ecosystem::{HostProbe, Probe};
    use std::io::Read;
    let Some(adopter) = ADOPTER.get().copied() else {
        return Ok(None);
    };
    let metadata = path.metadata().map_err(|e| EaiError::io(e.to_string()))?;
    if metadata.len() > 1024 * 1024 * 1024
        || path.extension().and_then(|e| e.to_str()) != Some("gguf")
    {
        return Ok(None);
    }
    let stamp = (
        metadata.len(),
        metadata
            .modified()
            .map_err(|e| EaiError::io(e.to_string()))?,
        susi_config::file_node(&metadata),
    );
    let mut owned = OWNED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(runner) = owned.as_ref() {
        if runner.path == path
            && runner.stamp == stamp
            && runner.context == requested_context
            && runner.running()
        {
            return Ok(Some(runner.endpoint()));
        }
    }
    let Some(executable) = HostProbe.which("llama-server") else {
        return Ok(None);
    };
    {
        let mut failed = FAILED.lock().unwrap_or_else(|e| e.into_inner());
        if failed.as_ref().is_some_and(|(previous, when)| {
            previous == path && when.elapsed() < Duration::from_secs(60)
        }) {
            return Ok(None);
        }
        *failed = Some((path.to_owned(), Instant::now()));
    }
    // The planner needs metadata, not a second copy of all the weights.
    let mut header = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(16 * 1024 * 1024).read_to_end(&mut header))
        .map_err(|e| EaiError::io(e.to_string()))?;
    let info = gguf_inspector::inspect(&header)?;
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .map_err(|e| EaiError::io(e.to_string()))?;
    let port = listener
        .local_addr()
        .map_err(|e| EaiError::io(e.to_string()))?
        .port();
    let launch = plan_from(
        &info,
        metadata.len(),
        path,
        &Spec {
            host: Host {
                vram: 0,
                ram: 0,
                disk: 0,
            },
            cores: std::thread::available_parallelism().map_or(2, |n| n.get().min(9) as u32),
            port,
            requested_ctx: requested_context,
        },
    );
    let dir = susi_paths::SusiDirs::data_dir()
        .join("runtime")
        .join(format!(
            "llama-{}-{}",
            std::process::id(),
            susi_config::cluster_key::random_nonce_hex()
        ));
    susi_config::create_private_dir(&dir).map_err(|e| EaiError::io(e.to_string()))?;
    let files = PrivateFiles(dir.clone());
    let secret_path = dir.join("entropy");
    let key = hex::encode(
        susi_config::load_or_create_secret(&secret_path)
            .map_err(|e| EaiError::io(e.to_string()))?,
    );
    let key_path = dir.join("key");
    if !susi_config::install_private_file(&key_path, key.as_bytes())
        .map_err(|e| EaiError::io(e.to_string()))?
    {
        return Err(EaiError::authentication("owned runner key already exists"));
    }
    let _ = std::fs::remove_file(secret_path);
    let mut command = std::process::Command::new(executable);
    command
        .args(&launch.argv[1..])
        .args([
            "--host",
            "127.0.0.1",
            "--device",
            "none",
            "--parallel",
            "1",
            "--alias",
            "susi-owned-local",
            "--no-webui",
            "--api-key-file",
        ])
        .arg(&key_path)
        .env_clear()
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    for name in [
        "PATH",
        "SystemRoot",
        "WINDIR",
        "TEMP",
        "TMP",
        "COMSPEC",
        "PATHEXT",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    for name in ["HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA"] {
        command.env(name, &dir);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    drop(listener);
    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return Err(EaiError::inference(format!(
                "installed llama-server could not start: {error}"
            )));
        }
    };
    let adopted = adopter(&child);
    let runner = Arc::new(OwnedRunner {
        child: Mutex::new(child),
        path: path.to_owned(),
        stamp,
        context: requested_context,
        api_base: format!("http://127.0.0.1:{port}/v1"),
        api_key: key,
        _files: files,
    });
    if !adopted {
        return Err(EaiError::inference(
            "host could not own llama-server process",
        ));
    }
    wait_until_ready(&runner, &launch.health_url, Duration::from_secs(10))?;
    *FAILED.lock().unwrap_or_else(|e| e.into_inner()) = None;
    let endpoint = runner.endpoint();
    *owned = Some(runner);
    Ok(Some(endpoint))
}

fn wait_until_ready(runner: &OwnedRunner, url: &str, budget: Duration) -> EaiResult<()> {
    let started = Instant::now();
    let authorization = format!("Bearer {}", runner.api_key);
    while started.elapsed() < budget {
        if !runner.running() {
            return Err(EaiError::inference(
                "installed llama-server exited before becoming ready",
            ));
        }
        if let Ok(response) =
            susi_http_transport::http_call("GET", url, &[("Authorization", &authorization)], 1, 0)
        {
            if response.status == 200 {
                let body = response.into_utf8(4096).map_err(EaiError::inference)?;
                if serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .is_some_and(|v| v["status"] == "ok")
                    && runner.running()
                {
                    return Ok(());
                }
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(EaiError::inference(
        "installed llama-server exceeded its startup deadline",
    ))
}

/// Fraction of free VRAM the planner may spend on weights (rest is runtime,
/// KV growth, compositor).
const VRAM_BUDGET: f64 = 0.85;

/// What a planned llama-server launch looks like.
#[derive(Debug, Clone, PartialEq)]
pub struct Launch {
    pub argv: Vec<String>,
    /// `http://127.0.0.1:<port>/health` — poll until 200.
    pub health_url: String,
    /// Layers sent to the GPU (`-ngl`); `total` when fully offloaded.
    pub gpu_layers: u64,
    pub total_layers: u64,
    /// `-c` value actually chosen.
    pub context: u64,
    /// `-t` value actually chosen.
    pub threads: u32,
    /// `true` when every layer fits in VRAM.
    pub full_offload: bool,
    pub why: String,
}

/// Plan a llama-server launch for `gguf` on `host` listening on `port`.
///
/// - `-ngl` = layers whose per-layer weight share fits the VRAM budget
///   (all of them when the whole model fits, `0` when the host has no GPU
///   budget at all).
/// - `-c` = `min(requested_ctx, model ctx)` clamped `[512, 8192]`.
/// - `-t` = host cores minus one, clamped `[1, 64]`.
pub fn plan(gguf: &Path, spec: &Spec) -> EaiResult<Launch> {
    let bytes = std::fs::read(gguf)
        .map_err(|e| EaiError::io(format!("cannot read {}: {e}", gguf.display())))?;
    let info = gguf_inspector::inspect(&bytes)
        .map_err(|e| EaiError::config(format!("{}: {e}", gguf.display())))?;
    let size = bytes.len() as u64;
    Ok(plan_from(&info, size, gguf, spec))
}

/// Host-side launch parameters for `plan` / `plan_from`.
#[derive(Debug, Clone, Copy)]
pub struct Spec {
    pub host: Host,
    pub cores: u32,
    pub port: u16,
    /// Context the caller asked for; clamped into `[512, min(model_ctx, 8192)]`.
    pub requested_ctx: u64,
}

/// Planning maths split out so tests can feed synthetic headers/hosts without
/// touching the filesystem.
pub fn plan_from(
    info: &gguf_inspector::GgufInfo,
    file_bytes: u64,
    gguf: &Path,
    spec: &Spec,
) -> Launch {
    let layers = info.layer_count.unwrap_or(1).max(1);
    let bytes_per_layer = (file_bytes / layers).max(1);
    let vram_cap = (spec.host.vram as f64 * VRAM_BUDGET) as u64;
    let gpu_layers = if spec.host.vram == 0 {
        0
    } else {
        (vram_cap / bytes_per_layer).min(layers)
    };
    let full_offload = gpu_layers >= layers;
    let ngl = if full_offload { layers } else { gpu_layers };

    let model_ctx = info.context_length.unwrap_or(4096);
    let context = spec.requested_ctx.min(model_ctx).clamp(512, 8192);
    let threads = spec.cores.saturating_sub(1).clamp(1, 64);

    let mut argv = vec![
        "llama-server".to_string(),
        "-m".into(),
        gguf.display().to_string(),
        "--port".into(),
        spec.port.to_string(),
        "-c".into(),
        context.to_string(),
        "-t".into(),
        threads.to_string(),
        "-ngl".into(),
        ngl.to_string(),
    ];
    if !full_offload && gpu_layers > 0 {
        argv.push("--no-mmap".into()); // avoid pinning CPU pages we will not use
    }
    let why = if full_offload {
        format!(
            "model fits VRAM budget ({:.1} GiB file vs {:.1} GiB usable) — all {layers} layers on GPU",
            gib(file_bytes),
            gib(vram_cap)
        )
    } else if gpu_layers == 0 {
        format!("no VRAM budget — {layers} layers on CPU, {threads} threads")
    } else {
        format!(
            "VRAM holds {gpu_layers}/{layers} layers ({:.1} GiB usable) — rest on CPU",
            gib(vram_cap)
        )
    };
    Launch {
        argv,
        health_url: format!("http://127.0.0.1:{}/health", spec.port),
        gpu_layers: ngl,
        total_layers: layers,
        context,
        threads,
        full_offload,
        why,
    }
}

fn gib(b: u64) -> f64 {
    b as f64 / (1 << 30) as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gguf_inspector::GgufInfo;

    #[test]
    fn owned_runner_fixture() {
        use std::io::{BufRead, Read, Write};
        let Ok(port) = std::env::var("SUSI_LLAMA_FIXTURE_PORT") else {
            return;
        };
        let listener = std::net::TcpListener::bind(format!("127.0.0.1:{port}")).unwrap();
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut authorized = false;
            let mut generation = false;
            let mut content_length = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                authorized |= line
                    .trim()
                    .eq_ignore_ascii_case("Authorization: Bearer fixture-secret");
                generation |= line.starts_with("POST /v1/chat/completions ");
                if let Some((name, value)) = line.split_once(':') {
                    if name.eq_ignore_ascii_case("Content-Length") {
                        content_length = value.trim().parse::<usize>().unwrap();
                    }
                }
            }
            let mut payload = vec![0; content_length];
            reader.read_exact(&mut payload).unwrap();
            if generation {
                let body: serde_json::Value = serde_json::from_slice(&payload).unwrap();
                authorized &= body["max_tokens"] == 7 && body["temperature"] == 0;
            }
            let body = if authorized && generation {
                "{\"choices\":[{\"message\":{\"content\":\"owned-fixture-answer\"}}]}"
            } else if authorized {
                "{\"status\":\"ok\"}"
            } else {
                "{}"
            };
            let status = if authorized {
                "200 OK"
            } else {
                "401 Unauthorized"
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    }

    #[test]
    fn owned_runner_health_requires_auth_and_last_lease_reaps_process() {
        let dir = tempfile::tempdir().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "llamacpp_launcher::tests::owned_runner_fixture",
                "--nocapture",
            ])
            .env("SUSI_LLAMA_FIXTURE_PORT", address.port().to_string())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        drop(listener);
        let child = command.spawn().unwrap();
        let model = dir.path().join("fixture.gguf");
        std::fs::write(&model, b"x").unwrap();
        let metadata = model.metadata().unwrap();
        let mut runner = OwnedRunner {
            child: Mutex::new(child),
            path: model.clone(),
            stamp: (
                1,
                metadata.modified().unwrap(),
                susi_config::file_node(&metadata),
            ),
            context: 512,
            api_base: format!("http://{address}/v1"),
            api_key: "fixture-secret".into(),
            _files: PrivateFiles(dir.path().to_owned()),
        };
        let url = format!("http://{address}/health");
        wait_until_ready(&runner, &url, Duration::from_secs(3)).unwrap();
        runner.api_key = "wrong-key".into();
        assert!(wait_until_ready(&runner, &url, Duration::from_millis(250)).is_err());
        runner.api_key = "fixture-secret".into();
        wait_until_ready(&runner, &url, Duration::from_secs(3)).unwrap();
        let runner = Arc::new(runner);
        let lease = runner.endpoint();
        *OWNED.lock().unwrap() = Some(Arc::clone(&runner));
        assert_eq!(
            generate_owned(&model, "fixture question", 7)
                .unwrap()
                .as_deref(),
            Some("owned-fixture-answer")
        );
        drop(runner);
        shutdown_owned();
        assert!(lease._runner.running());
        drop(lease);
        assert!(
            std::net::TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_err()
        );
    }

    const GIB: u64 = 1 << 30;

    fn info(layers: u64, ctx: u64) -> GgufInfo {
        GgufInfo {
            layer_count: Some(layers),
            context_length: Some(ctx),
            ..GgufInfo::default()
        }
    }

    fn host(vram_gb: u64, ram_gb: u64) -> Host {
        Host {
            vram: vram_gb * GIB,
            ram: ram_gb * GIB,
            disk: 0,
        }
    }

    fn spec(vram_gb: u64, ram_gb: u64, cores: u32, port: u16, ctx: u64) -> Spec {
        Spec {
            host: host(vram_gb, ram_gb),
            cores,
            port,
            requested_ctx: ctx,
        }
    }

    #[test]
    fn llamacpp_launcher_full_offload_when_model_fits() {
        // 4 GiB file, 32 layers, 24 GiB card → every layer offloads.
        let l = plan_from(
            &info(32, 4096),
            4 * GIB,
            Path::new("m.gguf"),
            &spec(24, 64, 8, 8080, 4096),
        );
        assert!(l.full_offload);
        assert_eq!(l.gpu_layers, 32);
        assert!(l.argv.join(" ").contains("-ngl 32"));
    }

    #[test]
    fn llamacpp_launcher_partial_offload_splits_layers() {
        // 8 GiB file / 32 layers = 256 MiB per layer; 4 GiB card → ~13 layers
        // (4 GiB × 0.85 = 3.4 GiB / 256 MiB = 13).
        let l = plan_from(
            &info(32, 4096),
            8 * GIB,
            Path::new("m.gguf"),
            &spec(4, 64, 8, 8080, 4096),
        );
        assert!(!l.full_offload);
        assert_eq!(l.gpu_layers, 13);
        assert!(l.argv.join(" ").contains("-ngl 13"));
        assert!(l.argv.join(" ").contains("--no-mmap"));
    }

    #[test]
    fn llamacpp_launcher_cpu_only_when_no_vram() {
        let l = plan_from(
            &info(32, 4096),
            4 * GIB,
            Path::new("m.gguf"),
            &spec(0, 64, 8, 8080, 4096),
        );
        assert_eq!(l.gpu_layers, 0);
        assert!(l.argv.join(" ").contains("-ngl 0"));
    }

    #[test]
    fn llamacpp_launcher_context_clamped_to_model_and_cap() {
        let l = plan_from(
            &info(32, 2048),
            GIB,
            Path::new("m.gguf"),
            &spec(24, 64, 8, 8080, 8192),
        );
        assert_eq!(l.context, 2048); // model ctx wins
        let l = plan_from(
            &info(32, 128_000),
            GIB,
            Path::new("m.gguf"),
            &spec(24, 64, 8, 8080, 16_000),
        );
        assert_eq!(l.context, 8192); // planner cap wins
        let l = plan_from(
            &info(32, 4096),
            GIB,
            Path::new("m.gguf"),
            &spec(24, 64, 8, 8080, 128),
        );
        assert_eq!(l.context, 512); // floor
    }

    #[test]
    fn llamacpp_launcher_threads_leave_one_core() {
        let l = plan_from(
            &info(32, 4096),
            GIB,
            Path::new("m.gguf"),
            &spec(0, 64, 8, 8080, 4096),
        );
        assert_eq!(l.threads, 7);
        let l = plan_from(
            &info(32, 4096),
            GIB,
            Path::new("m.gguf"),
            &spec(0, 64, 1, 8080, 4096),
        );
        assert_eq!(l.threads, 1);
    }

    #[test]
    fn llamacpp_launcher_health_url_uses_port() {
        let l = plan_from(
            &info(32, 4096),
            GIB,
            Path::new("m.gguf"),
            &spec(0, 64, 8, 9191, 4096),
        );
        assert_eq!(l.health_url, "http://127.0.0.1:9191/health");
        assert!(l.argv.join(" ").contains("--port 9191"));
    }
}
