#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;
use susi_unsafe_ratchet::measurement::{scan, write_json_atomic};

fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Result<BTreeMap<String, String>, String> {
    let mut args = args.into_iter();
    let mut values = BTreeMap::new();
    while let Some(key) = args.next() {
        if !matches!(
            key.as_str(),
            "--root" | "--target" | "--features" | "--output"
        ) {
            return Err(format!("unknown argument {key}"));
        }
        let value = args.next().ok_or_else(|| format!("{key} needs a value"))?;
        if values.insert(key, value).is_some() {
            return Err("duplicate argument".into());
        }
    }
    Ok(values)
}

fn run() -> Result<(), String> {
    let values = parse_args(std::env::args().skip(1))?;
    let get = |key: &str| {
        values
            .get(key)
            .map(String::as_str)
            .ok_or_else(|| format!("{key} is required"))
    };
    let measurement = scan(
        Path::new(get("--root")?),
        get("--target")?,
        get("--features")?,
    )?;
    write_json_atomic(Path::new(get("--output")?), &measurement)?;
    println!(
        "complete crate-forbid measurement: {} packages, {} files",
        measurement.packages.len(),
        measurement
            .packages
            .iter()
            .map(|p| p.files.len())
            .sum::<usize>()
    );
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("unsafe_measure: {error}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    #[test]
    fn parse_args_accepts_all_four_supported_flags() {
        let parsed = parse_args(
            [
                "--root",
                ".",
                "--target",
                "x86_64-pc-windows-msvc",
                "--features",
                "default",
                "--output",
                "out.json",
            ]
            .into_iter()
            .map(String::from),
        )
        .expect("valid flags");
        assert_eq!(parsed.get("--root").map(String::as_str), Some("."));
        assert_eq!(parsed.get("--output").map(String::as_str), Some("out.json"));
    }

    #[test]
    fn parse_args_rejects_unknown_flag_name() {
        let err = parse_args(["--bogus", "val"].into_iter().map(String::from))
            .expect_err("unknown flag must fail");
        assert!(err.contains("unknown argument --bogus"));
    }

    #[test]
    fn parse_args_rejects_flag_missing_value() {
        let err = parse_args(["--root"].into_iter().map(String::from))
            .expect_err("missing value must fail");
        assert!(err.contains("--root needs a value"));
    }

    #[test]
    fn parse_args_rejects_duplicate_flag() {
        let err = parse_args(["--root", "a", "--root", "b"].into_iter().map(String::from))
            .expect_err("duplicate flag must fail");
        assert_eq!(err, "duplicate argument");
    }
}
