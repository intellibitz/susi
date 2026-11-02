//! Complete native measurement admission and explicitly reviewed migration.
#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;
use susi_unsafe_ratchet::{inventory, measurement};

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
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
