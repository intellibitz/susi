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

//! Serve the cockpit panel over the ledger and the event feed.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use susi_harness::cockpit::{serve, Cockpit};

fn main() {
    let (ledger, events, port) = match parse(std::env::args().skip(1)) {
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
    if let Err(message) = runtime.block_on(run(ledger, events, port)) {
        fail(&message);
    }
}

async fn run(ledger: PathBuf, events: PathBuf, port: u16) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|error| format!("bind: {error}"))?;
    let address = listener
        .local_addr()
        .map_err(|error| format!("local_addr: {error}"))?;
    println!("susi cockpit listening on http://{address}");
    let _ = std::io::stdout().flush();
    serve(
        listener,
        Arc::new(Cockpit {
            ledger_path: ledger,
            events_path: events,
        }),
    )
    .await
    .map_err(|error| format!("serve: {error}"))
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<(PathBuf, PathBuf, u16), String> {
    let mut ledger = PathBuf::new();
    let mut events = PathBuf::new();
    let mut port: u16 = 9181;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--ledger" => ledger = PathBuf::from(take(&mut args, &argument)?),
            "--events" => events = PathBuf::from(take(&mut args, &argument)?),
            "--port" => {
                port = take(&mut args, &argument)?
                    .parse()
                    .map_err(|_| "port must be a number".to_string())?;
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if ledger.as_os_str().is_empty() {
        return Err("--ledger is required".into());
    }
    if events.as_os_str().is_empty() {
        events = ledger.with_file_name("events.jsonl");
    }
    Ok((ledger, events, port))
}

fn take(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next().ok_or_else(|| format!("{flag} needs a value"))
}

fn fail(message: &str) -> ! {
    eprintln!("harness-cockpit: {message}");
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args<'a>(list: &'a [&'a str]) -> impl Iterator<Item = String> + 'a {
        list.iter().map(|value| (*value).to_string())
    }

    #[test]
    fn parses_the_ledger_and_defaults_the_feed() {
        let (ledger, events, port) = parse(args(&["--ledger", "out/ledger.jsonl"])).unwrap();
        assert_eq!(ledger, PathBuf::from("out/ledger.jsonl"));
        assert_eq!(events, PathBuf::from("out/events.jsonl"));
        assert_eq!(port, 9181);
    }

    #[test]
    fn refuses_a_missing_ledger() {
        assert!(parse(args(&[])).is_err());
        assert!(parse(args(&["--port", "x"])).is_err());
    }
}
