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

//! Run one native worker turn through the metering gateway.
//!
//! ```text
//! harness-run --gateway http://127.0.0.1:9185 \
//!   --model deepseek-flash --workspace ./scratch --state ./state \
//!   --mission pilot-2 --task exec-1 "Create hello.txt containing OK"
//! ```

#[path = "support/args.rs"]
mod arguments;
use arguments::{number, required, take};

use serde_json::json;
use std::path::PathBuf;
use std::time::Duration;
use susi_harness::executor::{run, ExecutorConfig, RunOutcome};

fn main() {
    let config = match parse(std::env::args().skip(1)) {
        Ok(config) => config,
        Err(message) => fail(&message),
    };
    match run(&config) {
        Ok(RunOutcome::Succeeded {
            final_text,
            reported_cost_usd,
        }) => {
            print(json!({
                "ok": true,
                "final": final_text,
                "reported_cost_usd": reported_cost_usd
            }));
        }
        Ok(RunOutcome::Failed { message }) => {
            print(json!({"ok": false, "error": message}));
            std::process::exit(1);
        }
        Ok(RunOutcome::TimedOut) => {
            print(json!({"ok": false, "error": "timed out"}));
            std::process::exit(1);
        }
        Err(error) => fail(&error.to_string()),
    }
}

fn print(value: serde_json::Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(&value).unwrap_or_default()
    );
}

fn fail(message: &str) -> ! {
    eprintln!("harness-run: {message}");
    std::process::exit(1);
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<ExecutorConfig, String> {
    let mut gateway = String::new();
    let mut model = String::new();
    let mut workspace = PathBuf::new();
    let mut state = PathBuf::new();
    let mut mission = String::new();
    let mut task = String::new();
    let mut attempt: u32 = 1;
    let mut timeout: u64 = 600;
    let mut prompt_words: Vec<String> = Vec::new();

    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--gateway" => gateway = take(&mut args, &argument)?,
            "--model" => model = take(&mut args, &argument)?,
            "--workspace" => workspace = PathBuf::from(take(&mut args, &argument)?),
            "--state" => state = PathBuf::from(take(&mut args, &argument)?),
            "--mission" => mission = take(&mut args, &argument)?,
            "--task" => task = take(&mut args, &argument)?,
            "--attempt" => attempt = number(&mut args, &argument)?,
            "--timeout" => timeout = number(&mut args, &argument)?,
            other if other.starts_with("--") => {
                return Err(format!("unknown argument {other}"));
            }
            other => prompt_words.push(other.to_string()),
        }
    }

    required!(gateway.trim() => "--gateway is required");
    required!(model.trim() => "--model is required");
    required!(workspace.as_os_str() => "--workspace is required");
    required!(state.as_os_str() => "--state is required");
    required!(mission.trim(), task.trim() => "--mission and --task are required");
    required!(prompt_words => "a prompt is required");

    Ok(ExecutorConfig {
        gateway_url: gateway,
        model,
        workspace,
        state_dir: state,
        mission,
        task,
        attempt,
        timeout: Duration::from_secs(timeout),
        prompt: prompt_words.join(" "),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args<'a>(list: &'a [&'a str]) -> impl Iterator<Item = String> + 'a {
        list.iter().map(|value| (*value).to_string())
    }

    #[test]
    fn parses_the_required_flags_and_joins_the_prompt() {
        let config = parse(args(&[
            "--gateway",
            "http://127.0.0.1:9185",
            "--model",
            "deepseek-flash",
            "--workspace",
            "scratch",
            "--state",
            "state",
            "--mission",
            "pilot-2",
            "--task",
            "exec-1",
            "--attempt",
            "2",
            "create",
            "hello.txt",
        ]))
        .unwrap();
        assert_eq!(config.prompt, "create hello.txt");
        assert_eq!(config.attempt, 2);
        assert_eq!(config.timeout, Duration::from_secs(600));
    }

    #[test]
    fn refuses_missing_required_flags() {
        assert!(parse(args(&["--gateway", "g"])).is_err());
        assert!(parse(args(&["--gateway", "g", "--model", "m"])).is_err());
        assert!(parse(args(&["--attempt", "nope"])).is_err());
        assert!(parse(args(&["--nope"])).is_err());
    }

    #[test]
    fn defaults_attempt_and_timeout_overrides_timeout_and_validates_missing_fields() {
        let config = parse(args(&[
            "--gateway",
            "http://127.0.0.1:9185",
            "--model",
            "deepseek-flash",
            "--workspace",
            "scratch",
            "--state",
            "state",
            "--mission",
            "pilot-2",
            "--task",
            "exec-1",
            "--timeout",
            "120",
            "verify",
            "defaults",
        ]))
        .unwrap();
        assert_eq!(config.attempt, 1);
        assert_eq!(config.timeout, Duration::from_secs(120));
        assert_eq!(config.prompt, "verify defaults");

        assert_eq!(
            parse(args(&[
                "--gateway",
                "http://127.0.0.1:9185",
                "--model",
                "deepseek-flash",
                "--state",
                "state",
                "--mission",
                "pilot-2",
                "--task",
                "exec-1",
                "prompt",
            ]))
            .err()
            .unwrap(),
            "--workspace is required"
        );
        assert_eq!(
            parse(args(&[
                "--gateway",
                "http://127.0.0.1:9185",
                "--model",
                "deepseek-flash",
                "--workspace",
                "scratch",
                "--mission",
                "pilot-2",
                "--task",
                "exec-1",
                "prompt",
            ]))
            .err()
            .unwrap(),
            "--state is required"
        );
        assert_eq!(
            parse(args(&[
                "--gateway",
                "http://127.0.0.1:9185",
                "--model",
                "deepseek-flash",
                "--workspace",
                "scratch",
                "--state",
                "state",
                "--task",
                "exec-1",
                "prompt",
            ]))
            .err()
            .unwrap(),
            "--mission and --task are required"
        );
        assert_eq!(
            parse(args(&[
                "--gateway",
                "http://127.0.0.1:9185",
                "--model",
                "deepseek-flash",
                "--workspace",
                "scratch",
                "--state",
                "state",
                "--mission",
                "pilot-2",
                "--task",
                "exec-1",
            ]))
            .err()
            .unwrap(),
            "a prompt is required"
        );
    }

    #[test]
    fn validates_invalid_timeout_missing_flag_values_and_whitespace_only_gateway_or_model() {
        assert_eq!(
            parse(args(&["--timeout", "nope"])).err().unwrap(),
            "timeout must be a number of seconds"
        );
        assert_eq!(
            parse(args(&["--workspace"])).err().unwrap(),
            "--workspace needs a value"
        );
        assert_eq!(
            parse(args(&[
                "--gateway",
                "  ",
                "--model",
                "deepseek-flash",
                "--workspace",
                "scratch",
                "--state",
                "state",
                "--mission",
                "pilot-2",
                "--task",
                "exec-1",
                "prompt",
            ]))
            .err()
            .unwrap(),
            "--gateway is required"
        );
        assert_eq!(
            parse(args(&[
                "--gateway",
                "http://127.0.0.1:9185",
                "--model",
                "  ",
                "--workspace",
                "scratch",
                "--state",
                "state",
                "--mission",
                "pilot-2",
                "--task",
                "exec-1",
                "prompt",
            ]))
            .err()
            .unwrap(),
            "--model is required"
        );
    }

    #[test]
    fn rejects_whitespace_task_and_missing_values_for_remaining_flags() {
        assert_eq!(
            parse(args(&[
                "--gateway",
                "http://127.0.0.1:9185",
                "--model",
                "deepseek-flash",
                "--workspace",
                "scratch",
                "--state",
                "state",
                "--mission",
                "pilot-2",
                "--task",
                "   ",
                "prompt",
            ]))
            .err()
            .unwrap(),
            "--mission and --task are required"
        );
        for flag in [
            "--gateway",
            "--model",
            "--state",
            "--mission",
            "--task",
            "--attempt",
            "--timeout",
        ] {
            assert_eq!(
                parse(args(&[flag])).err().unwrap(),
                format!("{flag} needs a value")
            );
        }
    }
}
