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

//! Native Susi launcher, installation owner and public command contract.
#![allow(missing_docs)]
#[cfg(windows)]
extern crate susi_windows_runtime;

mod cli;
mod dev_instance;
use clap::{CommandFactory, FromArgMatches};
use cli::defs::Cli;
use std::env;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;

fn bare_terminal_invocation(has_command: bool, has_intent: bool, stdin_is_terminal: bool) -> bool {
    !has_command && !has_intent && stdin_is_terminal
}

/// Usage for bare `susi`, plus where the conversation now lives.
fn print_bare_usage(tree: &mut clap::Command) -> std::process::ExitCode {
    let mut stdout = io::stdout();
    let printed = write!(stdout, "{}", tree.render_help())
        .and_then(|()| {
            writeln!(
                stdout,
                "\nGive Susi one goal: susi \"<what you want done>\" (or pipe it on stdin).\n\
                 The interactive conversation lives in the Susi desktop app."
            )
        })
        .and_then(|()| stdout.flush());
    if printed.is_err() {
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}

/// Parse argv against `tree`. The clap tree is large; `main` builds it once
/// and the same tree then serves the intent guard, the near-miss check and
/// bare usage (D-SP-1), instead of rebuilding it for each. Errors, `--help`
/// and `--version` exit exactly as `Cli::parse` would.
fn parse_cli(tree: &mut clap::Command) -> Cli {
    let mut matches = tree.get_matches_mut();
    Cli::from_arg_matches_mut(&mut matches).unwrap_or_else(|error| error.format(tree).exit())
}

fn get_home_dir() -> PathBuf {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The CLI's clap command tree is large, and a debug build spends enough stack
/// constructing it to exhaust the default Windows main-thread reserve before
/// the first line of output — the symptom is an immediate
/// `thread 'main' has overflowed its stack`, not a clap error. The surface's
/// size must never decide whether the binary starts, so the CLI runs on a
/// thread with a generous stack. The value is virtual address space, not
/// committed memory, so it costs nothing on a platform that never needs it.
const CLI_STACK_BYTES: usize = 16 * 1024 * 1024;

/// Refuse harness-owned subcommands when a worker shell invokes the CLI: the
/// tool spawns it with `SUSI_WORKER_GUARD=1`. The args are matched as words, so
/// passing `workflow` and `finish` separately is caught too.
fn guarded_worker_invocation(args: &[String]) -> bool {
    if std::env::var_os("SUSI_WORKER_GUARD").is_none() {
        return false;
    }
    let words: Vec<String> = args
        .iter()
        .filter(|arg| !arg.starts_with('-'))
        .map(|arg| arg.to_ascii_lowercase())
        .collect();
    words.windows(2).any(|pair| {
        matches!(pair[0].as_str(), "workflow" | "tasks")
            && matches!(
                pair[1].as_str(),
                "finish" | "sync" | "start" | "claim" | "close" | "release" | "renew" | "abandon"
            )
    })
}

fn main() -> std::process::ExitCode {
    if let Some(status) = susi_config::vault_exec::dispatch_gpg_program_multicall() {
        return status;
    }
    if let Some(status) = cli::install_cli::installed_channel::dispatch() {
        return status;
    }
    let Ok(args) = env::args_os()
        .skip(1)
        .map(|arg| arg.into_string())
        .collect::<Result<Vec<_>, _>>()
    else {
        eprintln!("command arguments must be valid UTF-8");
        return std::process::ExitCode::from(2);
    };
    if guarded_worker_invocation(&args) {
        eprintln!(
            "refused: the harness owns susi workflow/tasks commands; leave the changes in the tree"
        );
        return std::process::ExitCode::from(2);
    }
    drop(args);
    // Rendering one global info flag never needs to build the nested command parsers.
    // Clap still owns argument handling, output, executable naming and the exit code.
    let mut args = env::args_os().skip(1);
    let flag = args.next();
    if args.next().is_none() {
        let command = match flag.as_deref().and_then(|arg| arg.to_str()) {
            Some("--help" | "-h") => Some(cli::defs::RootInfo::command()),
            Some("--version" | "-V") => Some(clap::Command::new("susi")),
            _ => None,
        };
        if let Some(command) = command {
            susi_paths::stdio::install_broken_pipe_guard();
            command.version(env!("CARGO_PKG_VERSION")).get_matches();
        }
    }
    match std::thread::Builder::new()
        .name("susi-cli".to_string())
        .stack_size(CLI_STACK_BYTES)
        .spawn(run)
    {
        Ok(handle) => handle.join().unwrap_or(std::process::ExitCode::FAILURE),
        // A host that refuses the thread still gets a working CLI for any
        // command that fits the default stack.
        Err(_) => run(),
    }
}

fn run() -> std::process::ExitCode {
    susi_paths::stdio::install_broken_pipe_guard();
    let words: Vec<_> = env::args_os().skip(1).collect();
    if bare_terminal_invocation(false, !words.is_empty(), io::stdin().is_terminal()) {
        return print_bare_usage(&mut cli::defs::RootInfo::command());
    }
    // Concrete feature parsers live beside their implementations. Keep only
    // installation and native delivery parsing in this bootstrap executable.
    let first = words.first().and_then(|word| word.to_str());
    let native_help = first == Some("help")
        && words
            .get(1)
            .and_then(|word| word.to_str())
            .is_some_and(|word| matches!(word, "install" | "install-release" | "local"));
    if native_help || matches!(first, Some("install" | "install-release" | "local")) {
        let mut command_tree = Cli::command().version(env!("CARGO_PKG_VERSION"));
        let cli = parse_cli(&mut command_tree);
        if let Some(status) = cli::install_cli::installed_channel::management(cli.command.as_ref())
        {
            return status;
        }
        if let Some(cli::defs::Commands::Local { args }) = &cli.command {
            return cli::local_delivery_cli::execute(args);
        }
        if matches!(cli.command, Some(cli::defs::Commands::Install)) {
            return cli::install_cli::execute();
        }
    } else if first == Some("help") && words.len() == 1 {
        let mut tree = cli::defs::RootInfo::command();
        return match tree.print_long_help() {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("Unable to print help: {error}");
                std::process::ExitCode::FAILURE
            }
        };
    }
    let prepare_instance = || dev_instance::isolate_if_dev_build(&get_home_dir());
    cli::runtime_forward::execute(&words, prepare_instance)
}
