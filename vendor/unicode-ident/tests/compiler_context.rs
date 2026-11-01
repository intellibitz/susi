//! Real compiler-context regression. No shell, network, generated character
//! tables, or fake classifier is used. Run with the installed Rust 1.99 toolchain.
use std::{
    env, fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct Fixture {
    root: PathBuf,
    parent: PathBuf,
    owner: String,
}
impl Fixture {
    fn new() -> Self {
        let parent = env::temp_dir().canonicalize().expect("temporary directory");
        let owner = format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = parent.join(format!("susi-unicode-context-{owner}"));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("owner"), &owner).unwrap();
        Self {
            root,
            parent,
            owner,
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // Failed fixtures retain their exact sources/logs for diagnosis. Only
        // this unique, marked direct child of the original temp root is removed.
        if !std::thread::panicking()
            && self.root.parent() == Some(self.parent.as_path())
            && self.root.canonicalize().ok().as_ref() == Some(&self.root)
            && fs::read_to_string(self.root.join("owner")).ok().as_deref() == Some(&self.owner)
            && fs::symlink_metadata(&self.root).is_ok_and(|m| !m.file_type().is_symlink())
        {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}
fn literal(path: &Path) -> String {
    let raw = path.to_str().expect("UTF-8 source path");
    let value = if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
        format!("//{}", rest.replace('\\', "/"))
    } else {
        raw.strip_prefix(r"\\?\").unwrap_or(raw).replace('\\', "/")
    };
    format!("\"{}\"", value.replace('"', "\\\""))
}

fn run(mut command: Command, log: &Path, timeout: Duration) -> String {
    let file = fs::File::create(log).unwrap();
    let mut child = command
        .stdin(Stdio::null())
        .stdout(file.try_clone().unwrap())
        .stderr(file)
        .spawn()
        .unwrap();
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            panic!("command timed out; retained log: {}", log.display());
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut output = String::new();
    fs::File::open(log)
        .unwrap()
        .take(64 * 1024)
        .read_to_string(&mut output)
        .unwrap();
    assert!(
        status.success(),
        "command failed ({status}); {}\n{output}",
        log.display()
    );
    output
}

#[test]
fn real_proc_macro_negatives_runtime_capability_and_session_cleanup() {
    let ident = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .canonicalize()
        .unwrap();
    let vendor = ident.parent().unwrap();
    let workspace = vendor.parent().unwrap();
    let adapter = vendor.join("idna-adapter");
    let parser = workspace.join("crates/susi-vendor-syn");
    assert!(adapter.join("Cargo.toml").is_file());
    assert!(parser.join("Cargo.toml").is_file());
    let fixture = Fixture::new();
    for directory in ["src", "probe/src", "runtime-tmp"] {
        fs::create_dir_all(fixture.root.join(directory)).unwrap();
    }
    let manifest = format!(
        r#"[package]
name = "susi_unicode_context_fixture"
version = "0.0.0"
edition = "2021"
[workspace]
members = ["probe"]
[dependencies]
probe = {{ path = "probe" }}
unicode-ident = {{ path = {ident} }}
susi-vendor-syn = {{ path = {parser} }}
[patch.crates-io]
unicode-ident = {{ path = {ident} }}
idna_adapter = {{ path = {adapter} }}
"#,
        ident = literal(&ident),
        parser = literal(&parser),
        adapter = literal(&adapter)
    );
    fs::write(fixture.root.join("Cargo.toml"), manifest).unwrap();
    fs::write(
        fixture.root.join("probe/Cargo.toml"),
        format!(
            r#"[package]
name = "probe"
version = "0.0.0"
edition = "2021"
[lib]
proc-macro = true
[dependencies]
unicode-ident = {{ path = {} }}
"#,
            literal(&ident)
        ),
    )
    .unwrap();
    fs::write(fixture.root.join("probe/src/lib.rs"), MACRO).unwrap();
    fs::write(fixture.root.join("src/main.rs"), CONSUMER).unwrap();
    let rustc = env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let mut version = Command::new(&rustc);
    version.current_dir(&ident).arg("-vV");
    let version = run(
        version,
        &fixture.root.join("compiler.log"),
        Duration::from_secs(20),
    );
    assert!(
        version.starts_with("rustc 1.99.0 "),
        "this acceptance test requires the pinned installed compiler: {version}"
    );
    let host = version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .unwrap();
    let mut sysroot = Command::new(&rustc);
    sysroot.current_dir(&ident).args(["--print", "sysroot"]);
    let sysroot = run(
        sysroot,
        &fixture.root.join("sysroot.log"),
        Duration::from_secs(20),
    );
    let compiler = PathBuf::from(sysroot.trim())
        .join("bin")
        .join(if cfg!(windows) { "rustc.exe" } else { "rustc" });
    assert!(compiler.is_file());
    let target = fixture.root.join("build");
    let mut cargo = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    cargo
        .current_dir(&fixture.root)
        .args(["build", "--offline", "--target", host, "--target-dir"])
        .arg(&target)
        .env("CARGO_TARGET_DIR", &target)
        .env("CARGO_BUILD_TARGET_DIR", &target)
        .env("CARGO_BUILD_BUILD_DIR", &target)
        .env("RUSTC", &compiler)
        .env("RUSTC_WRAPPER", "")
        .env("RUSTC_WORKSPACE_WRAPPER", "")
        .env("RUSTFLAGS", "")
        .env("CARGO_ENCODED_RUSTFLAGS", "")
        .env("RUSTUP_AUTO_INSTALL", "0");
    run(
        cargo,
        &fixture.root.join("build.log"),
        Duration::from_secs(180),
    );
    let executable = target.join(host).join("debug").join(if cfg!(windows) {
        "susi_unicode_context_fixture.exe"
    } else {
        "susi_unicode_context_fixture"
    });
    for missing in [false, true] {
        let mut child = Command::new(&executable);
        child
            .current_dir(&fixture.root)
            .env("TEMP", fixture.root.join("runtime-tmp"))
            .env("TMP", fixture.root.join("runtime-tmp"))
            .env("TMPDIR", fixture.root.join("runtime-tmp"));
        let label = if missing {
            child
                .arg("missing-capability")
                .env("RUSTC", fixture.root.join("missing-rustc"))
                .env("RUSTUP_HOME", fixture.root.join("missing-rustup"));
            "missing"
        } else {
            child.env("RUSTC", &compiler);
            "available"
        };
        let output = run(
            child,
            &fixture.root.join(format!("{label}.log")),
            Duration::from_secs(30),
        );
        assert!(
            output.contains(if missing {
                "CAPABILITY_ERROR_DISTINCT_FROM_INVALID_SYNTAX"
            } else {
                "TODHRI_ANALYSIS_OK"
            }),
            "{output}"
        );
        assert!(
            output.contains("EXPLICIT_SHUTDOWN_AND_OWNED_FILES_REAPED"),
            "{output}"
        );
        assert_eq!(
            fs::read_dir(fixture.root.join("runtime-tmp"))
                .unwrap()
                .count(),
            0,
            "helper owned files must be gone before test returns"
        );
    }
}

const MACRO: &str = r#"
extern crate proc_macro;
#[proc_macro]
pub fn validate(_:proc_macro::TokenStream)->proc_macro::TokenStream {
    assert_eq!(unicode_ident::try_is_xid_start('\u{feff}'),Ok(false));
    assert_eq!(unicode_ident::try_is_xid_continue('\u{feff}'),Ok(false));
    assert_eq!(unicode_ident::try_is_xid_start('\u{105c0}'),Ok(true));
    assert_eq!(unicode_ident::try_is_xid_continue('\u{105c0}'),Ok(true));
    assert_eq!(unicode_ident::try_is_xid_start('λ'),Ok(true));
    assert!(unicode_ident::take_classification_error().is_none());
    "fn negative_then_positive_macro_survived()->bool { true }".parse().unwrap()
}
"#;
const CONSUMER: &str = r#"
probe::validate!();
fn no_owned_helper_files() {
    let prefix=format!("susi-ident-broker-{}-",std::process::id());
    assert!(!std::fs::read_dir(std::env::temp_dir()).unwrap().any(|entry| entry.unwrap().file_name().to_string_lossy().starts_with(&prefix)),"parser session retained compiler-helper artifacts");
}
fn main(){
    assert!(negative_then_positive_macro_survived());
    assert_eq!(susi_vendor_syn::is_valid_rust_checked("fn normal() {}"),Ok(true));
    assert_eq!(susi_vendor_syn::is_valid_rust_checked("fn 😀() {}"),Ok(false));
    let missing = std::env::args().any(|a|a=="missing-capability");
    // U+10FFFF is permanently unassigned: newer OS Unicode versions must not
    // make an unavailable-compiler test accidentally rely on OS-known Todhri.
    let source = if missing { "fn \u{10ffff}() {}" } else { "fn \u{105c0}() {}" };
    let result=susi_vendor_syn::outline_checked(source);
    // Check automatic final-session teardown BEFORE explicitly requesting it.
    no_owned_helper_files();
    if missing {
        let error=result.unwrap_err(); assert!(error.contains("classification unavailable"),"{error}");
        assert_eq!(unicode_ident::try_is_xid_start('\u{10ffff}'),Err("identifier compiler service: Unavailable".into()));
        println!("CAPABILITY_ERROR_DISTINCT_FROM_INVALID_SYNTAX: {error}");
    }else{
        let items=result.unwrap().unwrap();assert_eq!(items[0].1,"\u{105c0}");
        assert_eq!(unicode_ident::try_is_xid_continue('\u{feff}'),Ok(false));
        // Force a real helper even when the OS already knows Todhri.
        assert_eq!(unicode_ident::try_is_xid_start('\u{10ffff}'),Ok(false));
        println!("TODHRI_ANALYSIS_OK; NEGATIVE_QUERY_DID_NOT_POISON_MACRO");
    }
    unicode_ident::shutdown_compiler().unwrap(); no_owned_helper_files();
    println!("EXPLICIT_SHUTDOWN_AND_OWNED_FILES_REAPED");
}
"#;
