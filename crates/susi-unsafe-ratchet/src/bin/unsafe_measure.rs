#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;
use susi_unsafe_ratchet::measurement::{scan, write_json_atomic};

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
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
