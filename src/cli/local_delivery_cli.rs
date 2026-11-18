//! Native local delivery: core's development and release tools (the former
//! `xtask`) moved to Susi Commercial Apps on 2026-11-19 (Mandate 84). `susi
//! local` names the launcher that serves them instead of carrying them.

pub(crate) fn execute(args: &[String]) -> std::process::ExitCode {
    eprintln!(
        "susi local is served by Susi Commercial Apps' development tools: run `susi-dev {}` from this core checkout (build it in a Susi Commercial Apps checkout with `cargo build --manifest-path tools/core-xtask/Cargo.toml --bin susi-dev`)",
        args.join(" ")
    );
    std::process::ExitCode::FAILURE
}
