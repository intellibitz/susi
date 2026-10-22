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

//! Run the metering gateway.
//!
//! Workers point their OpenAI-compatible base URL at this process. Every
//! non-streaming completion is forwarded, priced from the catalogue, and
//! written to the ledger before the answer is returned.
//!
//! ```text
//! harness-gateway --catalogue prices.json --ledger ledger.jsonl \
//!   --upstream https://api.deepseek.com/chat/completions --provider deepseek
//! ```
//!
//! With `--accounts accounts.json` the same process also authenticates callers:
//! each bearer token names an account with a money cap and a rate limit, and
//! `/v1/models` lists the registry. Without it the gateway stays loopback-open.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use susi_harness::catalog::PriceCatalogue;
use susi_harness::gateway::{serve, GatewayState};

struct Options {
    port: u16,
    catalogue: PathBuf,
    ledger: PathBuf,
    upstream: String,
    provider: String,
    api_key_env: String,
    timeout: u64,
    max_output_tokens: Option<u32>,
    accounts: Option<PathBuf>,
}

fn main() {
    let options = match parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(message) => fail(&message),
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => fail(&format!("runtime: {error}")),
    };
    if let Err(message) = runtime.block_on(run(options)) {
        fail(&message);
    }
}

async fn run(options: Options) -> Result<(), String> {
    let catalogue =
        PriceCatalogue::load(&options.catalogue).map_err(|error| format!("catalogue: {error}"))?;
    let api_key = if options.upstream == "direct" {
        std::env::var(&options.api_key_env).unwrap_or_default()
    } else {
        std::env::var(&options.api_key_env)
            .ok()
            .filter(|key| !key.trim().is_empty())
            .ok_or_else(|| format!("{} is unset or empty", options.api_key_env))?
    };
    let cache_dir = options.ledger.parent().map(std::path::Path::to_path_buf);
    let state = Arc::new(GatewayState {
        catalogue,
        ledger_path: options.ledger,
        upstream_chat_url: options.upstream,
        provider: options.provider,
        api_key,
        timeout_secs: options.timeout,
        max_output_tokens: options.max_output_tokens,
        cache: Some(std::sync::Arc::new(
            susi_harness::cache::ResponseCache::new(cache_dir),
        )),
    });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", options.port))
        .await
        .map_err(|error| format!("bind: {error}"))?;
    let address = listener
        .local_addr()
        .map_err(|error| format!("local_addr: {error}"))?;
    println!("harness-gateway listening on http://{address}");
    let _ = std::io::stdout().flush();
    match options.accounts {
        Some(path) => {
            let text = std::fs::read_to_string(&path)
                .map_err(|error| format!("accounts {}: {error}", path.display()))?;
            let ingress =
                susi_harness::ingress::Ingress::parse(&text).map_err(|error| error.to_string())?;
            susi_harness::ingress::serve(listener, state, Arc::new(ingress))
                .await
                .map_err(|error| format!("serve: {error}"))
        }
        None => serve(listener, state)
            .await
            .map_err(|error| format!("serve: {error}")),
    }
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut port: u16 = 0;
    let mut catalogue = PathBuf::new();
    let mut ledger = PathBuf::new();
    let mut upstream = String::new();
    let mut provider = String::from("upstream");
    let mut api_key_env = String::from("DEEPSEEK_API_KEY");
    let mut timeout: u64 = 120;
    let mut max_output_tokens: Option<u32> = None;
    let mut accounts: Option<PathBuf> = None;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--port" => {
                port = take(&mut args, &flag)?
                    .parse()
                    .map_err(|_| "port must be a number".to_string())?;
            }
            "--catalogue" => catalogue = PathBuf::from(take(&mut args, &flag)?),
            "--ledger" => ledger = PathBuf::from(take(&mut args, &flag)?),
            "--upstream" => upstream = take(&mut args, &flag)?,
            "--provider" => provider = take(&mut args, &flag)?,
            "--api-key-env" => api_key_env = take(&mut args, &flag)?,
            "--accounts" => accounts = Some(PathBuf::from(take(&mut args, &flag)?)),
            "--timeout" => {
                timeout = take(&mut args, &flag)?
                    .parse()
                    .map_err(|_| "timeout must be a number of seconds".to_string())?;
            }
            "--max-output-tokens" => {
                max_output_tokens = Some(
                    take(&mut args, &flag)?
                        .parse()
                        .map_err(|_| "max-output-tokens must be a number".to_string())?,
                );
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if catalogue.as_os_str().is_empty() {
        return Err("--catalogue is required".into());
    }
    if ledger.as_os_str().is_empty() {
        return Err("--ledger is required".into());
    }
    if upstream.trim().is_empty() {
        return Err("--upstream is required".into());
    }
    Ok(Options {
        port,
        catalogue,
        ledger,
        upstream,
        provider,
        api_key_env,
        timeout,
        max_output_tokens,
        accounts,
    })
}

fn take(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next().ok_or_else(|| format!("{flag} needs a value"))
}

fn fail(message: &str) -> ! {
    eprintln!("harness-gateway: {message}");
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args<'a>(list: &'a [&'a str]) -> impl Iterator<Item = String> + 'a {
        list.iter().map(|value| (*value).to_string())
    }

    #[test]
    fn parses_direct_upstream_mode() {
        let options = parse(args(&[
            "--port",
            "9188",
            "--catalogue",
            "prices.json",
            "--ledger",
            "ledger.jsonl",
            "--upstream",
            "direct",
        ]))
        .unwrap();
        assert_eq!(options.upstream, "direct");
    }

    #[test]
    fn parses_required_flags() {
        let options = parse(args(&[
            "--port",
            "9180",
            "--catalogue",
            "prices.json",
            "--ledger",
            "ledger.jsonl",
            "--upstream",
            "http://127.0.0.1:1/chat",
            "--provider",
            "deepseek",
        ]))
        .unwrap();
        assert_eq!(options.port, 9180);
        assert_eq!(options.catalogue, PathBuf::from("prices.json"));
        assert_eq!(options.provider, "deepseek");
        assert_eq!(options.api_key_env, "DEEPSEEK_API_KEY");
        assert_eq!(options.timeout, 120);
        assert!(options.max_output_tokens.is_none());
    }

    #[test]
    fn parses_an_accounts_file() {
        let options = parse(args(&[
            "--catalogue",
            "prices.json",
            "--ledger",
            "ledger.jsonl",
            "--upstream",
            "direct",
            "--accounts",
            "accounts.json",
        ]))
        .unwrap();
        assert_eq!(options.accounts, Some(PathBuf::from("accounts.json")));
    }

    #[test]
    fn refuses_unknown_or_incomplete_arguments() {
        assert!(parse(args(&["--nope"])).is_err());
        assert!(parse(args(&["--catalogue"])).is_err());
        assert!(parse(args(&["--catalogue", "p", "--ledger", "l"])).is_err());
        assert!(parse(args(&["--port", "not-a-number"])).is_err());
    }
}
