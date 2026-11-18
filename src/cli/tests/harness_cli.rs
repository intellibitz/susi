//! `susi harness` parsing and help, which core keeps after its implementation
//! moved to Susi Commercial Apps (Mandate 84): the forwarder hands the suite only
//! words these definitions accepted.

use super::HarnessCommands;
use crate::cli::defs::{Cli, Commands};
use clap::Parser;
use std::path::PathBuf;

#[test]
fn api_parses_the_graph_bus() {
    let cli = Cli::try_parse_from([
        "susi",
        "harness",
        "api",
        "--ledger",
        "out/ledger.jsonl",
        "--graph",
        "out/bus.jsonl",
    ])
    .unwrap();
    let Some(Commands::Harness { action }) = cli.command else {
        panic!("expected the harness command");
    };
    match action {
        HarnessCommands::Api { graph, .. } => {
            assert_eq!(graph, Some(PathBuf::from("out/bus.jsonl")));
        }
        _ => panic!("expected harness api"),
    }
}

#[test]
fn cockpit_stays_a_hidden_alias_of_api() {
    let cli = Cli::try_parse_from(["susi", "harness", "cockpit", "--ledger", "l.jsonl"]).unwrap();
    let Some(Commands::Harness { action }) = cli.command else {
        panic!("expected the harness command");
    };
    assert!(matches!(action, HarnessCommands::Api { .. }));
    let mut command = <Cli as clap::CommandFactory>::command();
    let harness = command
        .find_subcommand_mut("harness")
        .expect("harness subcommand");
    let help = harness.render_long_help().to_string();
    assert!(help.contains("api"), "{help}");
    assert!(!help.contains("cockpit"), "{help}");
}

#[test]
fn models_parses_without_a_gateway() {
    let cli = Cli::try_parse_from(["susi", "harness", "models"]).unwrap();
    let Some(Commands::Harness { action }) = cli.command else {
        panic!("expected the harness command");
    };
    match action {
        HarnessCommands::Models {
            verify,
            discover,
            provider,
            available,
        } => {
            assert!(!verify);
            assert!(!discover);
            assert!(!available);
            assert!(provider.is_none());
        }
        _ => panic!("expected harness models"),
    }
}

#[test]
fn models_available_parses() {
    let cli = Cli::try_parse_from(["susi", "harness", "models", "--available"]).unwrap();
    let Some(Commands::Harness { action }) = cli.command else {
        panic!("expected the harness command");
    };
    match action {
        HarnessCommands::Models { available, .. } => {
            assert!(available);
        }
        _ => panic!("expected harness models --available"),
    }
}

#[test]
fn gateway_parses_with_defaults() {
    let cli = Cli::try_parse_from(["susi", "harness", "gateway"]).unwrap();
    let Some(Commands::Harness { action }) = cli.command else {
        panic!("expected the harness command");
    };
    match action {
        HarnessCommands::Gateway {
            port,
            host,
            upstream,
            ..
        } => {
            assert_eq!(port, 9180);
            assert_eq!(host, "127.0.0.1");
            assert_eq!(upstream, "direct");
        }
        _ => panic!("expected harness gateway"),
    }
}

#[test]
fn models_verify_parses_with_a_provider() {
    let cli = Cli::try_parse_from([
        "susi",
        "harness",
        "models",
        "--verify",
        "--provider",
        "gemini",
    ])
    .unwrap();
    let Some(Commands::Harness { action }) = cli.command else {
        panic!("expected the harness command");
    };
    match action {
        HarnessCommands::Models {
            verify, provider, ..
        } => {
            assert!(verify);
            assert_eq!(provider.as_deref(), Some("gemini"));
        }
        _ => panic!("expected harness models --verify"),
    }
}

#[test]
fn models_provider_requires_verify_or_discover() {
    assert!(Cli::try_parse_from(["susi", "harness", "models", "--provider", "gemini"]).is_err());
    assert!(Cli::try_parse_from([
        "susi",
        "harness",
        "models",
        "--verify",
        "--discover",
        "--provider",
        "gemini"
    ])
    .is_err());
    let cli = Cli::try_parse_from([
        "susi",
        "harness",
        "models",
        "--discover",
        "--provider",
        "deepinfra",
    ])
    .unwrap();
    let Some(Commands::Harness { action }) = cli.command else {
        panic!("expected the harness command");
    };
    match action {
        HarnessCommands::Models {
            discover,
            verify,
            provider,
            ..
        } => {
            assert!(discover && !verify);
            assert_eq!(provider.as_deref(), Some("deepinfra"));
        }
        _ => panic!("expected harness models --discover"),
    }
}

#[test]
fn ledger_help_gives_the_order_the_default_is_resolved_in() {
    use clap::CommandFactory;
    let cli = Cli::command();
    let harness = cli.find_subcommand("harness").expect("the harness command");
    for name in ["run", "budget", "gateway"] {
        let help = harness
            .find_subcommand(name)
            .and_then(|command| {
                command
                    .get_arguments()
                    .find(|argument| argument.get_id() == "ledger")
            })
            .and_then(|argument| argument.get_help())
            .map(ToString::to_string)
            .unwrap_or_else(|| panic!("harness {name} --ledger has help"));
        let setting = help.find("SUSI_HARNESS_LEDGER").expect("names the setting");
        let workspace = help.find("out/ledger.jsonl").expect("names the workspace");
        let home = help.find("data home").expect("names the data home");
        assert!(setting < workspace && workspace < home, "{name}: {help}");
    }
}
