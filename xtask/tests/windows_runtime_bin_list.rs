//! Every xtask binary that links `susi_windows_runtime` must be named in
//! `xtask/build.rs`. The runtime defines `atexit` for the OS-UCRT link that
//! `susi_build_os_ucrt::configure_for` sets up; a binary left out of that list
//! links the default `msvcrt.lib` instead and fails on Windows MSVC with
//! LNK2005 (`atexit already defined`) / LNK1169. `release-qualify` shipped in
//! that state, which made Windows release qualification impossible.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use std::{fs, path::Path};

fn configured_binaries(build_rs: &str) -> Vec<String> {
    let start = build_rs
        .find("configure_for(&[")
        .expect("xtask/build.rs calls susi_build_os_ucrt::configure_for(&[...])");
    let rest = &build_rs[start..];
    let end = rest.find("])").expect("configure_for list is closed");
    rest[..end]
        .split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect()
}

#[test]
fn every_binary_linking_the_windows_runtime_uses_the_os_ucrt_link() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let configured = configured_binaries(&fs::read_to_string(root.join("build.rs")).unwrap());
    let mut sources = vec![("xtask".to_owned(), root.join("src/main.rs"))];
    for entry in fs::read_dir(root.join("src/bin")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|ext| ext == "rs") {
            let name = path.file_stem().unwrap().to_str().unwrap().to_owned();
            sources.push((name, path));
        }
    }
    let mut checked = 0;
    let mut missing = Vec::new();
    for (name, path) in sources {
        let text = fs::read_to_string(&path).unwrap();
        if text.contains("extern crate susi_windows_runtime;") {
            checked += 1;
            if !configured.contains(&name) {
                missing.push(name);
            }
        }
    }
    assert!(
        checked > 0,
        "no binary links susi_windows_runtime; the scan is broken"
    );
    assert!(
        missing.is_empty(),
        "binaries link susi_windows_runtime but are missing from xtask/build.rs configure_for: {missing:?}"
    );
}

#[test]
fn configured_list_parses_names_in_order() {
    let text = "fn main() { x::configure_for(&[\n \"a\",\n \"b-c\",\n]) }";
    assert_eq!(configured_binaries(text), ["a", "b-c"]);
}
