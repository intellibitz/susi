#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

//! Launch the LiteLLM proxy that sits behind the metered gateway.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use susi_harness::litellm::{argv, environment_names, readiness_url, DEFAULT_CONFIG};

struct Options {
    config: PathBuf,
    host: String,
    port: u16,
    litellm: PathBuf,
    state: PathBuf,
    ready_timeout: u64,
}

fn main() {
    let options = match parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(message) => fail(&message),
    };
    if let Err(message) = run(options) {
        fail(&message);
    }
}

fn run(options: Options) -> Result<(), String> {
    let text = std::fs::read_to_string(&options.config)
        .map_err(|error| format!("read {}: {error}", options.config.display()))?;
    for name in environment_names(&text) {
        let unset = std::env::var(&name)
            .map(|value| value.trim().is_empty())
            .unwrap_or(true);
        if unset {
            eprintln!("harness-litellm: {name} is unset; models that use it fail until it is set");
        }
    }
    std::fs::create_dir_all(&options.state)
        .map_err(|error| format!("create {}: {error}", options.state.display()))?;
    let log_path = options.state.join("litellm.log");
    let log = std::fs::File::create(&log_path)
        .map_err(|error| format!("create {}: {error}", log_path.display()))?;
    let stderr = log
        .try_clone()
        .map_err(|error| format!("clone {}: {error}", log_path.display()))?;
    let mut child = Command::new(&options.litellm)
        .args(argv(&options.config, &options.host, options.port))
        // LiteLLM prints model names as UTF-8; without this, a redirected
        // Windows console defaults to cp1252 and startup dies on the first
        // non-ASCII character.
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUTF8", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr))
        .spawn()
        .map_err(|error| format!("spawn {}: {error}", options.litellm.display()))?;
    wait_ready(&mut child, &options)?;
    println!(
        "harness-litellm listening on http://{}:{}",
        options.host, options.port
    );
    let status = child
        .wait()
        .map_err(|error| format!("wait for litellm: {error}"))?;
    if !status.success() {
        return Err(format!(
            "litellm exited with {status}; see {}",
            log_path.display()
        ));
    }
    Ok(())
}

fn wait_ready(child: &mut Child, options: &Options) -> Result<(), String> {
    let url = readiness_url(&options.host, options.port);
    let deadline = Instant::now() + Duration::from_secs(options.ready_timeout);
    let mut last = String::from("no attempt yet");
    while Instant::now() < deadline {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("poll litellm: {error}"))?
        {
            return Err(format!(
                "litellm exited before it was ready ({status}); see {}",
                options.state.join("litellm.log").display()
            ));
        }
        match susi_http_transport::http_call("GET", &url, &[], 2, 0) {
            Ok(response) if (200..300).contains(&response.status) => return Ok(()),
            Ok(response) => last = format!("status {}", response.status),
            Err(error) => last = error,
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err(format!(
        "litellm was not ready after {} s ({last})",
        options.ready_timeout
    ))
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut config = PathBuf::from(DEFAULT_CONFIG);
    let mut host = String::from("127.0.0.1");
    let mut port: u16 = 4000;
    let mut litellm = PathBuf::from("litellm");
    let mut state = PathBuf::from("harness-litellm");
    let mut ready_timeout: u64 = 120;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--config" => config = PathBuf::from(take(&mut args, &argument)?),
            "--host" => host = take(&mut args, &argument)?,
            "--port" => {
                port = take(&mut args, &argument)?
                    .parse()
                    .map_err(|_| "port must be a number".to_string())?;
            }
            "--litellm" => litellm = PathBuf::from(take(&mut args, &argument)?),
            "--state" => state = PathBuf::from(take(&mut args, &argument)?),
            "--ready-timeout" => {
                ready_timeout = take(&mut args, &argument)?
                    .parse()
                    .map_err(|_| "ready-timeout must be a number of seconds".to_string())?;
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if !config.is_file() {
        return Err(format!("config {} does not exist", config.display()));
    }
    Ok(Options {
        config,
        host,
        port,
        litellm,
        state,
        ready_timeout,
    })
}

fn take(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next().ok_or_else(|| format!("{flag} needs a value"))
}

fn fail(message: &str) -> ! {
    eprintln!("harness-litellm: {message}");
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn args<'a>(list: &'a [&'a str]) -> impl Iterator<Item = String> + 'a {
        list.iter().map(|value| (*value).to_string())
    }

    fn config_file() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.yaml"), "model_list: []").unwrap();
        dir
    }

    #[test]
    fn the_defaults_are_the_repository_config_and_the_local_port() {
        let dir = config_file();
        let path = dir.path().join("config.yaml");
        let options = parse(args(&["--config", path.to_str().unwrap()])).unwrap();
        assert_eq!(options.port, 4000);
        assert_eq!(options.host, "127.0.0.1");
        assert_eq!(options.litellm, Path::new("litellm"));
        assert_eq!(options.ready_timeout, 120);
    }

    #[test]
    fn a_missing_config_is_refused_before_anything_starts() {
        assert!(parse(args(&["--config", "nope.yaml"])).is_err());
        assert!(parse(args(&["--port"])).is_err());
        assert!(parse(args(&["--nope"])).is_err());
    }
}
