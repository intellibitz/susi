#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(all(windows, target_arch = "x86_64"))]
#[allow(dead_code)]
#[path = "../bootstrap/os_ucrt.rs"]
mod os_ucrt;
#[path = "../src/pe_imports.rs"]
mod pe_imports;
use std::{fs, path::Path, process::Command};
fn compile(root: &Path, name: &str, library: &Path, abort: bool) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/ucrt")
        .join(format!("{name}.rs"));
    let mut command = Command::new("rustc");
    command
        .arg("--edition=2021")
        .arg(source)
        .arg("-o")
        .arg(root.join(format!("{name}.exe")))
        .args(["-C", "link-arg=/NODEFAULTLIB:vcruntime.lib", "-C"])
        .arg(format!("link-arg={}", library.display()));
    if abort {
        command.args(["-C", "panic=abort", "-C", "opt-level=3", "-C", "lto=fat"]);
    }
    let out = command.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
#[test]
fn native_os_boundary_preserves_unwind_abort_and_missing_capability_failure() {
    let root = std::env::temp_dir().join(format!(
        "susi-ucrt-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    let library = os_ucrt::generate(&root).unwrap();
    for (name, abort) in [
        ("unwind", false),
        ("abort", true),
        ("missing", false),
        ("driver", false),
    ] {
        compile(&root, name, &library, abort);
    }
    let unwind = Command::new(root.join("unwind.exe")).output().unwrap();
    assert!(
        unwind.status.success(),
        "{:?}:{}",
        unwind.status,
        String::from_utf8_lossy(&unwind.stderr)
    );
    let driver = Command::new(root.join("driver.exe"))
        .arg(&root)
        .output()
        .unwrap();
    assert!(
        driver.status.success(),
        "{:?}:{}",
        driver.status,
        String::from_utf8_lossy(&driver.stderr)
    );
    for name in ["unwind", "abort"] {
        let imports =
            pe_imports::inspect(&fs::read(root.join(format!("{name}.exe"))).unwrap()).unwrap();
        assert!(imports.iter().any(|name| name == "import:ucrtbase.dll"));
    }
    fs::remove_dir_all(root).unwrap();
}
