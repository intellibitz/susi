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

//! Serve the cockpit panel over the ledger, the event feed and the plan.

use std::path::PathBuf;
use std::sync::Arc;
use susi_harness::budget::Budget;
use susi_harness::cockpit::{serve, Cockpit};
use susi_harness::money::MicroUsd;

struct Options {
    ledger: PathBuf,
    events: PathBuf,
    state: Option<PathBuf>,
    plan: Option<PathBuf>,
    controls: Option<PathBuf>,
    catalogue: Option<PathBuf>,
    graph: Option<PathBuf>,
    budget: Option<Budget>,
    mission: Option<String>,
    port: u16,
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
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", options.port))
        .await
        .map_err(|error| format!("bind: {error}"))?;
    let address = listener
        .local_addr()
        .map_err(|error| format!("local_addr: {error}"))?;
    println!("susi cockpit listening on http://{address}");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    serve(
        listener,
        Arc::new(Cockpit {
            ledger_path: options.ledger,
            events_path: options.events,
            state_path: options.state,
            plan_path: options.plan,
            controls_path: options.controls,
            catalogue_path: options.catalogue,
            graph_path: options.graph,
            budget: options.budget,
            mission: options.mission,
        }),
    )
    .await
    .map_err(|error| format!("serve: {error}"))
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut ledger = PathBuf::new();
    let mut events = PathBuf::new();
    let mut state = None;
    let mut plan = None;
    let mut controls = None;
    let mut catalogue = None;
    let mut graph = None;
    let mut budget = None;
    let mut mission = None;
    let mut port: u16 = 9181;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--ledger" => ledger = PathBuf::from(take(&mut args, &argument)?),
            "--events" => events = PathBuf::from(take(&mut args, &argument)?),
            "--state" => state = Some(PathBuf::from(take(&mut args, &argument)?)),
            "--plan" => plan = Some(PathBuf::from(take(&mut args, &argument)?)),
            "--controls" => controls = Some(PathBuf::from(take(&mut args, &argument)?)),
            "--catalogue" => catalogue = Some(PathBuf::from(take(&mut args, &argument)?)),
            "--graph" => graph = Some(PathBuf::from(take(&mut args, &argument)?)),
            "--budget-usd" => {
                let amount = take(&mut args, &argument)?;
                let micros = MicroUsd::parse_usd(&amount)
                    .ok_or_else(|| format!("--budget-usd {amount} is not a dollar amount"))?;
                budget = Some(Budget::new(micros));
            }
            "--mission" => mission = Some(take(&mut args, &argument)?),
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
    Ok(Options {
        ledger,
        events,
        state,
        plan,
        controls,
        catalogue,
        graph,
        budget,
        mission,
        port,
    })
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
        let options = parse(args(&["--ledger", "out/ledger.jsonl"])).unwrap();
        assert_eq!(options.ledger, PathBuf::from("out/ledger.jsonl"));
        assert_eq!(options.events, PathBuf::from("out/events.jsonl"));
        assert_eq!(options.port, 9181);
    }

    #[test]
    fn parses_the_plan_controls_and_budget() {
        let options = parse(args(&[
            "--ledger",
            "l.jsonl",
            "--plan",
            "plan.json",
            "--controls",
            "control.jsonl",
            "--budget-usd",
            "0.25",
            "--port",
            "9182",
        ]))
        .unwrap();
        assert_eq!(options.plan, Some(PathBuf::from("plan.json")));
        assert_eq!(options.controls, Some(PathBuf::from("control.jsonl")));
        assert_eq!(
            options.budget.map(|budget| budget.limit.micros()),
            Some(250_000)
        );
        assert_eq!(options.port, 9182);
    }

    #[test]
    fn parses_the_graph_bus() {
        let options = parse(args(&["--ledger", "l.jsonl", "--graph", "bus.jsonl"])).unwrap();
        assert_eq!(options.graph, Some(PathBuf::from("bus.jsonl")));
    }

    #[test]
    fn refuses_a_missing_ledger() {
        assert!(parse(args(&[])).is_err());
        assert!(parse(args(&["--port", "x"])).is_err());
        assert!(parse(args(&["--ledger", "x", "--budget-usd", "nope"])).is_err());
    }
}
