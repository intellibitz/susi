//! Exercise real Cargo compiler selection with a hostile private Cargo config.
#![allow(clippy::unwrap_used)] // A failed fixture setup must fail this regression.

#[cfg(windows)]
#[allow(dead_code)] // This harness exercises the stdlib builder's Cargo factory, not its CLI.
#[path = "../../toolchain/windows-stdlib/builder.rs"]
mod builder;
#[path = "../src/compiler_env.rs"]
mod compiler_env;

use std::{fs, process::Command};

#[test]
fn host_compiler_wrappers_doc_runner_and_flags_cannot_enter_native_gates() {
    let scratch = std::env::temp_dir().join(format!(
        "susi-compiler-binding-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let home = scratch.join("cargo-home");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir(scratch.join("src")).unwrap();
    let marker = scratch.join("poison-executed");
    let poison = scratch.join(format!("tripwire{}", std::env::consts::EXE_SUFFIX));
    let source = scratch.join("tripwire.rs");
    fs::write(
        &source,
        r#"fn main() {
        std::fs::write(std::env::var_os("TEST_WRAPPER_MARKER").unwrap(), b"executed").unwrap();
        std::process::exit(43);
    }"#,
    )
    .unwrap();
    let mut rustc = Command::new("rustc");
    compiler_env::apply_checked(&mut rustc).unwrap();
    let output = rustc.arg(&source).arg("-o").arg(&poison).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    fs::write(
        scratch.join("Cargo.toml"),
        "[package]\nname='compiler-binding-fixture'\nversion='0.1.0'\nedition='2021'\n",
    )
    .unwrap();
    fs::write(
        scratch.join("Cargo.lock"),
        "version = 4\n[[package]]\nname = \"compiler-binding-fixture\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(scratch.join("src/lib.rs"), "//! ```\n//! assert_eq!(2+2, 4);\n//! ```\n#[cfg(poison)] compile_error!(\"unreviewed flags\");\n").unwrap();
    let exe = poison.to_str().unwrap().replace('\\', "/");
    fs::write(home.join("config.toml"), format!("[build]\nrustc='{exe}'\nrustdoc='{exe}'\nrustc-wrapper='{exe}'\nrustc-workspace-wrapper='{exe}'\nrustflags=['--cfg','poison']\n")).unwrap();
    let run = |mut command: Command, args: &[&str]| {
        command
            .current_dir(&scratch)
            .env("CARGO_HOME", &home)
            .env("TEST_WRAPPER_MARKER", &marker)
            .env("CARGO_TARGET_DIR", scratch.join("target"))
            .env("CARGO_BUILD_TARGET_DIR", scratch.join("target"))
            .env("CARGO_BUILD_BUILD_DIR", scratch.join("target"))
            .args(args)
            .args(["--offline", "--locked"])
            .output()
            .unwrap()
    };
    // An unbound real Cargo invocation must genuinely execute the tripwire.
    let mut control = Command::new("cargo");
    for key in [
        "RUSTC",
        "RUSTDOC",
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
    ] {
        control.env_remove(key);
    }
    assert!(!run(control, &["tree"]).status.success());
    assert!(marker.exists());
    fs::remove_file(&marker).unwrap();
    let mut cargo = Command::new("cargo");
    compiler_env::apply(&mut cargo);
    let output = run(cargo, &["test", "--doc"]);
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    #[cfg(windows)]
    {
        let selected = rustc
            .get_envs()
            .find(|(key, _)| key == &"RUSTC")
            .unwrap()
            .1
            .unwrap();
        let installed = std::path::Path::new(selected)
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        for operation in ["tree", "build"] {
            let output = run(builder::sysroot_cargo(installed), &[operation]);
            assert!(output.status.success(), "{output:?}");
        }
    }
    assert!(!marker.exists());
    let impostor = scratch.join(format!("rustc{}", std::env::consts::EXE_SUFFIX));
    fs::copy(&poison, &impostor).unwrap();
    let mut rejected = Command::new(&impostor);
    assert!(compiler_env::apply_checked(&mut rejected).is_err());
    compiler_env::apply(&mut rejected);
    assert!(rejected
        .env("TEST_WRAPPER_MARKER", &marker)
        .arg("-Vv")
        .status()
        .is_err());
    assert!(!marker.exists());
    fs::remove_dir_all(scratch).unwrap();
}
