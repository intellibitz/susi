//! Complete native measurement admission and explicitly reviewed migration.
#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;
use susi_unsafe_ratchet::{inventory, measurement};

fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Result<BTreeMap<String, String>, String> {
    let mut args = args.into_iter();
    let mut values = BTreeMap::new();
    while let Some(key) = args.next() {
        if !matches!(
            key.as_str(),
            "--measurement"
                | "--baseline"
                | "--receipt"
                | "--write-baseline"
                | "--base-measurement"
                | "--review"
        ) {
            return Err(format!("unknown argument {key}; complete native --measurement required; raw geiger exports are inadmissible"));
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
            .map(Path::new)
            .ok_or_else(|| format!("{key} is required"))
    };
    if let Some(output) = values.get("--write-baseline") {
        if values.contains_key("--receipt") {
            return Err("migration does not issue a clean gate receipt".into());
        }
        let baseline = inventory::migrate(
            get("--baseline")?,
            get("--base-measurement")?,
            get("--measurement")?,
            get("--review")?,
        )?;
        measurement::write_json_atomic(Path::new(output), &baseline)?;
        println!(
            "reviewed complete baseline written: {} full package identities",
            baseline.allowances.len()
        );
    } else {
        if values.contains_key("--base-measurement") || values.contains_key("--review") {
            return Err("migration arguments require --write-baseline".into());
        }
        let measured = measurement::read_measurement(get("--measurement")?)?;
        let baseline = inventory::read_baseline(get("--baseline")?)?;
        let receipt = inventory::receipt(
            &measured,
            get("--measurement")?,
            &baseline,
            get("--baseline")?,
        )?;
        measurement::write_json_atomic(get("--receipt")?, &receipt)?;
        println!(
            "unsafe-exposure ratchet clean: {} completely measured package identities",
            measured.packages.len()
        );
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("unsafe_ratchet: {error}");
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
    fn parse_args_accepts_gate_and_migration_flags() {
        let parsed = parse_args(
            [
                "--measurement",
                "m.json",
                "--baseline",
                "b.json",
                "--receipt",
                "r.json",
            ]
            .into_iter()
            .map(String::from),
        )
        .expect("valid flags");
        assert_eq!(
            parsed.get("--measurement").map(String::as_str),
            Some("m.json")
        );
        assert_eq!(parsed.get("--receipt").map(String::as_str), Some("r.json"));
    }

    #[test]
    fn parse_args_rejects_unknown_geiger_flag_with_admissibility_message() {
        let err = parse_args(["--geiger", "raw.json"].into_iter().map(String::from))
            .expect_err("unknown flag must fail");
        assert!(err.contains("raw geiger exports are inadmissible"));
    }

    #[test]
    fn parse_args_rejects_missing_value_for_flag() {
        let err = parse_args(["--baseline"].into_iter().map(String::from))
            .expect_err("missing value must fail");
        assert!(err.contains("--baseline needs a value"));
    }

    #[test]
    fn parse_args_rejects_duplicate_flag_specification() {
        let err = parse_args(
            ["--receipt", "a.json", "--receipt", "b.json"]
                .into_iter()
                .map(String::from),
        )
        .expect_err("duplicate flag must fail");
        assert_eq!(err, "duplicate argument");
    }
}
