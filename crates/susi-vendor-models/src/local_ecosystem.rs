//! Inventory of the local AI ecosystem installed on this host: inference
//! engines, desktop runtimes and model stores, plus the GPU/NPU backends
//! they could use. Detection is read-only; only [`start`] launches anything,
//! and only on an explicit request.
//!
//! Third-party names, ports and health paths live here (vendor code); the
//! scan is generic over [`Probe`] so it is tested without touching the host.
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Everything the scan needs from the host.
pub trait Probe {
    fn which(&self, program: &str) -> Option<PathBuf>;
    fn home(&self) -> Option<PathBuf>;
    fn exists(&self, path: &Path) -> bool;
    /// True when `GET http://127.0.0.1:<port><path>` answers 2xx.
    fn http_ok(&self, port: u16, path: &str) -> bool;
    /// First line of `<program> --version`, when it prints one.
    fn version(&self, program: &Path) -> Option<String>;
    fn is_macos_arm(&self) -> bool;
}

struct Def {
    id: &'static str,
    name: &'static str,
    binaries: &'static [&'static str],
    /// Home-relative directories whose presence proves an install or model store.
    dirs: &'static [&'static str],
    port: u16,
    health: &'static str,
    /// Argv that starts the server without further input; `None` when it needs
    /// a model or other choice only the user can make.
    start: Option<&'static [&'static str]>,
    /// OpenAI-compatible `/v1` base once running (empty for stores/non-serving).
    openai_compat: bool,
}

const DEFS: &[Def] = &[
    Def {
        id: "ollama",
        name: "Ollama",
        binaries: &["ollama"],
        dirs: &[".ollama"],
        port: 11434,
        health: "/api/version",
        start: Some(&["ollama", "serve"]),
        openai_compat: true,
    },
    Def {
        id: "lm-studio",
        name: "LM Studio",
        binaries: &["lms"],
        dirs: &[".lmstudio", ".cache/lm-studio"],
        port: 1234,
        health: "/v1/models",
        start: Some(&["lms", "server", "start"]),
        openai_compat: true,
    },
    Def {
        id: "llama-cpp",
        name: "llama.cpp",
        binaries: &["llama-server", "llama-cli"],
        dirs: &[],
        port: 8080,
        health: "/health",
        start: None,
        openai_compat: true,
    },
    Def {
        id: "vllm",
        name: "vLLM",
        binaries: &["vllm"],
        dirs: &[],
        port: 8000,
        health: "/v1/models",
        start: None,
        openai_compat: true,
    },
    Def {
        id: "sglang",
        name: "SGLang",
        binaries: &["sglang"],
        dirs: &[],
        port: 30000,
        health: "/v1/models",
        start: None,
        openai_compat: true,
    },
    Def {
        id: "localai",
        name: "LocalAI",
        binaries: &["local-ai"],
        dirs: &[],
        port: 8080,
        health: "/readyz",
        start: Some(&["local-ai"]),
        openai_compat: true,
    },
    Def {
        id: "jan",
        name: "Jan",
        binaries: &["jan"],
        dirs: &[".local/share/Jan", "jan"],
        port: 1337,
        health: "/v1/models",
        start: None,
        openai_compat: true,
    },
    Def {
        id: "gpt4all",
        name: "GPT4All",
        binaries: &[],
        dirs: &[".local/share/nomic.ai/GPT4All"],
        port: 4891,
        health: "/v1/models",
        start: None,
        openai_compat: true,
    },
    Def {
        id: "koboldcpp",
        name: "KoboldCpp",
        binaries: &["koboldcpp"],
        dirs: &[],
        port: 5001,
        health: "/api/v1/model",
        start: None,
        openai_compat: false,
    },
    Def {
        id: "text-generation-webui",
        name: "text-generation-webui",
        binaries: &[],
        dirs: &["text-generation-webui"],
        port: 5000,
        health: "/v1/models",
        start: None,
        openai_compat: true,
    },
    Def {
        id: "huggingface-cache",
        name: "Hugging Face cache",
        binaries: &[],
        dirs: &[".cache/huggingface/hub"],
        port: 0,
        health: "",
        start: None,
        openai_compat: false,
    },
];

/// Env override for non-default ports: `SUSI_ENGINE_PORTS=jan=1400,ollama=11435`.
pub const PORTS_ENV: &str = "SUSI_ENGINE_PORTS";

/// Parse `id=port` pairs; malformed pairs, unknown ids and port 0 are dropped.
pub fn parse_port_overrides(spec: &str) -> Vec<(String, u16)> {
    spec.split(',')
        .filter_map(|pair| {
            let (id, port) = pair.split_once('=')?;
            let id = id.trim().to_ascii_lowercase();
            let port: u16 = port.trim().parse().ok().filter(|p| *p != 0)?;
            DEFS.iter()
                .any(|d| d.id == id && d.port != 0)
                .then_some((id, port))
        })
        .collect()
}

fn env_overrides() -> Vec<(String, u16)> {
    susi_config::env_or_cloud_env(PORTS_ENV)
        .map(|v| parse_port_overrides(&v))
        .unwrap_or_default()
}

fn port_of(d: &Def, overrides: &[(String, u16)]) -> u16 {
    overrides
        .iter()
        .find(|(id, _)| id == d.id)
        .map_or(d.port, |(_, p)| *p)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Detected {
    pub id: String,
    pub name: String,
    pub installed: bool,
    pub running: bool,
    pub binary: Option<PathBuf>,
    pub version: Option<String>,
    /// Existing install/model directories found under the home directory.
    pub dirs: Vec<PathBuf>,
    /// OpenAI-compatible base URL when the engine is running and speaks it.
    pub endpoint: Option<String>,
    /// `susi ecosystem start <id>` can launch it without further input.
    pub startable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Accelerator {
    pub backend: &'static str,
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Ecosystem {
    pub engines: Vec<Detected>,
    pub accelerators: Vec<Accelerator>,
}

pub fn scan(p: &dyn Probe) -> Ecosystem {
    scan_with(p, &env_overrides())
}

fn scan_with(p: &dyn Probe, overrides: &[(String, u16)]) -> Ecosystem {
    let home = p.home();
    let engines = DEFS
        .iter()
        .map(|d| {
            let binary = d.binaries.iter().find_map(|b| p.which(b));
            let dirs: Vec<PathBuf> = home
                .as_ref()
                .map(|h| {
                    d.dirs
                        .iter()
                        .map(|rel| h.join(rel))
                        .filter(|path| p.exists(path))
                        .collect()
                })
                .unwrap_or_default();
            let port = port_of(d, overrides);
            let running = port != 0 && p.http_ok(port, d.health);
            let installed = binary.is_some() || !dirs.is_empty() || running;
            Detected {
                id: d.id.into(),
                name: d.name.into(),
                installed,
                running,
                version: binary.as_deref().and_then(|b| p.version(b)),
                endpoint: (running && d.openai_compat)
                    .then(|| format!("http://127.0.0.1:{port}/v1")),
                startable: d.start.is_some() && binary.is_some() && !running,
                binary,
                dirs,
            }
        })
        .collect();
    Ecosystem {
        engines,
        accelerators: accelerators(p),
    }
}

/// `(name, OpenAI-compatible base URL)` for every serving engine in the table,
/// at its default (or `SUSI_ENGINE_PORTS`-overridden) port, for the HTTP provider probe to try. Bases shared by
/// two engines (8080) appear once, under the first name.
pub fn default_openai_endpoints() -> Vec<(String, String)> {
    openai_endpoints_with(&env_overrides())
}

fn openai_endpoints_with(overrides: &[(String, u16)]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for d in DEFS.iter().filter(|d| d.openai_compat && d.port != 0) {
        let base = format!("http://localhost:{}/v1", port_of(d, overrides));
        if !out.iter().any(|(_, b)| *b == base) {
            out.push((d.name.to_string(), base));
        }
    }
    out
}

/// GPU/NPU compute backends evidenced on this host (driver tools or device nodes).
pub fn accelerators(p: &dyn Probe) -> Vec<Accelerator> {
    let mut out = Vec::new();
    let mut add =
        |backend: &'static str, evidence: String| out.push(Accelerator { backend, evidence });
    if let Some(path) = p.which("nvidia-smi") {
        add("cuda", path.display().to_string());
    }
    if let Some(path) = p.which("rocm-smi") {
        add("rocm", path.display().to_string());
    } else if p.exists(Path::new("/dev/kfd")) {
        add("rocm", "/dev/kfd".into());
    }
    if p.is_macos_arm() {
        add("metal", "Apple silicon".into());
    }
    if let Some(path) = p.which("vulkaninfo") {
        add("vulkan", path.display().to_string());
    }
    if let Some(path) = p.which("sycl-ls").or_else(|| p.which("xpu-smi")) {
        add("oneapi", path.display().to_string());
    }
    for n in crate::npu_detection::npus(p) {
        add(n.backend, n.evidence);
    }
    out
}

/// Launch a startable engine detached. Refuses anything not in the table or
/// not installed; the child is reaped in the background.
pub fn start(id: &str, p: &dyn Probe) -> Result<String, String> {
    let def = DEFS
        .iter()
        .find(|d| d.id == id)
        .ok_or_else(|| format!("unknown engine {id}"))?;
    let argv = def
        .start
        .ok_or_else(|| format!("{} needs a model or options; start it yourself", def.name))?;
    let program = p
        .which(argv[0])
        .ok_or_else(|| format!("{} is not installed ({} not on PATH)", def.name, argv[0]))?;
    if p.http_ok(def.port, def.health) {
        return Ok(format!(
            "{} is already running on port {}",
            def.name, def.port
        ));
    }
    let mut child = std::process::Command::new(program)
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to start {}: {e}", def.name))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(format!("started {} ({})", def.name, argv.join(" ")))
}

/// The real host.
pub struct HostProbe;

impl Probe for HostProbe {
    fn which(&self, program: &str) -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            // A relative PATH entry could be a repo-supplied binary.
            .filter(|d| d.is_absolute())
            .map(|d| d.join(program))
            .find(|c| {
                c.is_file() && {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        c.metadata()
                            .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
                    }
                    #[cfg(not(unix))]
                    true
                }
            })
    }
    fn home(&self) -> Option<PathBuf> {
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
    }
    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
    fn http_ok(&self, port: u16, path: &str) -> bool {
        susi_http_transport::http_call("GET", &format!("http://127.0.0.1:{port}{path}"), &[], 1, 0)
            .is_ok_and(|c| (200..300).contains(&c.status))
    }
    fn version(&self, program: &Path) -> Option<String> {
        let out = susi_core::bounded_cmd::output_within(
            std::process::Command::new(program).arg("--version"),
            std::time::Duration::from_secs(3),
        )
        .ok()?;
        let first_line = |bytes: &[u8]| {
            String::from_utf8_lossy(bytes)
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .map(str::to_string)
        };
        first_line(&out.stdout).or_else(|| first_line(&out.stderr))
    }
    fn is_macos_arm(&self) -> bool {
        cfg!(all(target_os = "macos", target_arch = "aarch64"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    #[derive(Default)]
    struct Fake {
        bins: HashMap<&'static str, PathBuf>,
        paths: HashSet<PathBuf>,
        up: HashSet<(u16, &'static str)>,
        mac: bool,
    }
    impl Probe for Fake {
        fn which(&self, p: &str) -> Option<PathBuf> {
            self.bins.get(p).cloned()
        }
        fn home(&self) -> Option<PathBuf> {
            Some("/home/u".into())
        }
        fn exists(&self, p: &Path) -> bool {
            self.paths.contains(p)
        }
        fn http_ok(&self, port: u16, path: &str) -> bool {
            self.up.iter().any(|(p, h)| *p == port && *h == path)
        }
        fn version(&self, _: &Path) -> Option<String> {
            Some("v1".into())
        }
        fn is_macos_arm(&self) -> bool {
            self.mac
        }
    }

    fn engine<'a>(e: &'a Ecosystem, id: &str) -> &'a Detected {
        e.engines.iter().find(|d| d.id == id).unwrap()
    }

    #[test]
    fn empty_host_reports_nothing_installed() {
        let e = scan(&Fake::default());
        assert!(e
            .engines
            .iter()
            .all(|d| !d.installed && !d.running && !d.startable));
        assert!(e.accelerators.is_empty());
    }

    #[test]
    fn installed_but_stopped_engine_is_startable() {
        let mut f = Fake::default();
        f.bins.insert("ollama", "/usr/bin/ollama".into());
        let e = scan(&f);
        let o = engine(&e, "ollama");
        assert!(o.installed && !o.running && o.startable);
        assert_eq!(o.version.as_deref(), Some("v1"));
        assert!(o.endpoint.is_none());
    }

    #[test]
    fn running_engine_exposes_openai_endpoint_and_is_not_startable() {
        let mut f = Fake::default();
        f.bins.insert("ollama", "/usr/bin/ollama".into());
        f.up.insert((11434, "/api/version"));
        let o = scan(&f)
            .engines
            .into_iter()
            .find(|d| d.id == "ollama")
            .unwrap();
        assert!(o.running && !o.startable);
        assert_eq!(o.endpoint.as_deref(), Some("http://127.0.0.1:11434/v1"));
    }

    #[test]
    fn shared_port_is_disambiguated_by_health_path() {
        let mut f = Fake::default();
        f.up.insert((8080, "/health"));
        let e = scan(&f);
        assert!(engine(&e, "llama-cpp").running);
        assert!(!engine(&e, "localai").running);
    }

    #[test]
    fn directory_alone_proves_install_and_model_store() {
        let mut f = Fake::default();
        f.paths.insert("/home/u/.cache/huggingface/hub".into());
        let e = scan(&f);
        let h = engine(&e, "huggingface-cache");
        assert!(h.installed && !h.running && h.dirs.len() == 1 && h.endpoint.is_none());
    }

    #[test]
    fn engines_needing_a_model_are_never_startable() {
        let mut f = Fake::default();
        f.bins
            .insert("llama-server", "/usr/bin/llama-server".into());
        assert!(!engine(&scan(&f), "llama-cpp").startable);
        assert!(start("llama-cpp", &f)
            .unwrap_err()
            .contains("needs a model"));
        assert!(start("nope", &f).is_err());
    }

    #[test]
    fn default_endpoints_are_unique_and_include_newer_engines() {
        let eps = default_openai_endpoints();
        let bases: HashSet<_> = eps.iter().map(|(_, b)| b.clone()).collect();
        assert_eq!(bases.len(), eps.len());
        assert!(eps.contains(&("Jan".to_string(), "http://localhost:1337/v1".to_string())));
        assert!(eps.contains(&(
            "Ollama".to_string(),
            "http://localhost:11434/v1".to_string()
        )));
    }

    #[test]
    fn port_overrides_parse_defensively() {
        assert_eq!(
            parse_port_overrides(
                "jan=1400, Ollama=11435,bogus=1,jan=x,vllm=0,huggingface-cache=9,noequals"
            ),
            vec![("jan".to_string(), 1400), ("ollama".to_string(), 11435)]
        );
    }

    #[test]
    fn overridden_port_drives_detection_and_endpoints() {
        let mut f = Fake::default();
        f.up.insert((1400, "/v1/models"));
        let o = [("jan".to_string(), 1400_u16)];
        let e = scan_with(&f, &o);
        let jan = e.engines.iter().find(|d| d.id == "jan").unwrap();
        assert!(jan.running);
        assert_eq!(jan.endpoint.as_deref(), Some("http://127.0.0.1:1400/v1"));
        assert!(openai_endpoints_with(&o)
            .contains(&("Jan".to_string(), "http://localhost:1400/v1".to_string())));
        // Default port no longer counts once overridden.
        f.up.clear();
        f.up.insert((1337, "/v1/models"));
        assert!(
            !scan_with(&f, &o)
                .engines
                .iter()
                .find(|d| d.id == "jan")
                .unwrap()
                .running
        );
    }

    #[test]
    fn accelerators_are_reported_from_evidence() {
        let mut f = Fake::default();
        f.bins.insert("nvidia-smi", "/usr/bin/nvidia-smi".into());
        f.paths.insert("/dev/kfd".into());
        f.mac = true;
        let backends: Vec<_> = accelerators(&f).iter().map(|a| a.backend).collect();
        assert_eq!(backends, ["cuda", "rocm", "metal"]);
    }
}
