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

#[path = "support/args.rs"]
mod arguments;
use arguments::{number, required, take};

use std::io::Write;
use std::path::PathBuf;
use susi_harness::catalog::PriceCatalogue;
use susi_harness::gateway::{serve_admitted, GatewayState};

struct Options {
    port: u16,
    catalogue: PathBuf,
    ledger: PathBuf,
    upstream: String,
    provider: String,
    /// `--api-key-env`: an explicit override. `None` in direct mode means each
    /// model resolves its provider's own key env, which is what a gateway
    /// serving several providers needs.
    api_key_env: Option<String>,
    timeout: u64,
    max_output_tokens: Option<u32>,
    accounts: Option<PathBuf>,
}

fn main() {
    let options = match parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(message) => fail(&message),
    };
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => fail(&format!("runtime: {error}")),
    };
    if let Err(message) = runtime.block_on(run(options)) {
        fail(&message);
    }
}

async fn run(options: Options) -> Result<(), String> {
    if let Some(error) = susi_harness::providers::Registry::overlay_error() {
        return Err(error);
    }
    let catalogue = PriceCatalogue::load_or_builtin(&options.catalogue)
        .map_err(|error| format!("catalogue: {error}"))?;
    // A single explicit upstream needs one key: `--api-key-env` when given,
    // otherwise the historical default. In direct mode the gateway routes each
    // model through the registry, so the default is *no* override — pinning one
    // provider's key there made every other provider authenticate with it.
    let name = options
        .api_key_env
        .or_else(|| (options.upstream != "direct").then(|| "DEEPSEEK_API_KEY".to_string()));
    let api_key = name.map(susi_harness::secrets::Credential::Named);
    // The accounts are read, and the serving rule applied, before anything
    // listens: a malformed file used to fail only after the port was bound.
    let ingress =
        susi_harness::ingress::load_for_host(HOST, options.accounts.as_deref(), "accounts")?;
    let mut state = GatewayState::cached(
        catalogue,
        options.ledger,
        options.upstream,
        options.provider,
        options.timeout,
    );
    state.api_key = api_key;
    state.max_output_tokens = options.max_output_tokens;
    let listener = tokio::net::TcpListener::bind((HOST, options.port))
        .await
        .map_err(|error| format!("bind: {error}"))?;
    let address = listener
        .local_addr()
        .map_err(|error| format!("local_addr: {error}"))?;
    println!("harness-gateway listening on http://{address}");
    let _ = std::io::stdout().flush();
    serve_admitted(listener, state, ingress)
        .await
        .map_err(|error| format!("serve: {error}"))
}

/// The one address this binary serves on. Any other goes through
/// [`susi_harness::ingress::require_accounts_off_loopback`], as the other
/// entry points do.
const HOST: &str = "127.0.0.1";

/// Whether `name` can name a key variable: upper-case letters, digits and
/// `_`, the only names the key lookup reads. Any other name was reported
/// as "unset or empty" even when it was set.
fn key_env_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut port: u16 = 0;
    let mut catalogue = PathBuf::new();
    let mut ledger = PathBuf::new();
    let mut upstream = String::new();
    let mut provider = String::from("upstream");
    let mut api_key_env: Option<String> = None;
    let mut timeout: u64 = 120;
    let mut max_output_tokens: Option<u32> = None;
    let mut accounts: Option<PathBuf> = None;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--port" => port = number(&mut args, &flag)?,
            "--catalogue" => catalogue = PathBuf::from(take(&mut args, &flag)?),
            "--ledger" => ledger = PathBuf::from(take(&mut args, &flag)?),
            "--upstream" => upstream = take(&mut args, &flag)?,
            "--provider" => provider = take(&mut args, &flag)?,
            "--api-key-env" => {
                let name = take(&mut args, &flag)?;
                if !key_env_name(&name) {
                    return Err(format!(
                        "--api-key-env {name}: key variable names are upper-case letters, \
                         digits and _"
                    ));
                }
                api_key_env = Some(name);
            }
            "--accounts" => accounts = Some(PathBuf::from(take(&mut args, &flag)?)),
            "--timeout" => timeout = number(&mut args, &flag)?,
            "--max-output-tokens" => max_output_tokens = Some(number(&mut args, &flag)?),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    required!(catalogue.as_os_str() => "--catalogue is required");
    required!(ledger.as_os_str() => "--ledger is required");
    required!(upstream.trim() => "--upstream is required");
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
        assert_eq!(
            options.api_key_env, None,
            "direct mode pins no single key by default"
        );
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

    /// `--api-key-env my_proxy_key` was accepted, then reported as "unset or
    /// empty" while it was set: the key lookup reads upper-case names only.
    #[test]
    fn a_key_env_name_the_lookup_cannot_read_is_refused_by_name() {
        let with_key_env = |name: &'static str| {
            parse(args(&[
                "--catalogue",
                "prices.json",
                "--ledger",
                "ledger.jsonl",
                "--upstream",
                "direct",
                "--api-key-env",
                name,
            ]))
        };
        let error = with_key_env("my_proxy_key").err().unwrap();
        assert!(error.contains("upper-case"), "{error}");
        assert!(!error.contains("unset"), "{error}");
        assert_eq!(
            with_key_env("MY_PROXY_KEY").unwrap().api_key_env.as_deref(),
            Some("MY_PROXY_KEY")
        );
    }

    #[test]
    fn this_binary_serves_on_loopback_only() {
        assert!(susi_harness::ingress::is_loopback(HOST));
        assert!(susi_harness::ingress::require_accounts_off_loopback(HOST, None).is_ok());
    }

    #[test]
    fn refuses_unknown_or_incomplete_arguments() {
        assert!(parse(args(&["--nope"])).is_err());
        assert!(parse(args(&["--catalogue"])).is_err());
        assert!(parse(args(&["--catalogue", "p", "--ledger", "l"])).is_err());
        assert!(parse(args(&["--port", "not-a-number"])).is_err());
    }
}
