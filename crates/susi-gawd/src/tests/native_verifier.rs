//! Native test-only verification executable: real file/content checks, no shell.
use std::sync::OnceLock;

pub fn program() -> &'static str {
    static PROGRAM: OnceLock<String> = OnceLock::new();
    PROGRAM.get_or_init(|| {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("susi-native-verifier-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let source = dir.join("main.rs");
        let output = dir.join(if cfg!(windows) { "verify.exe" } else { "verify" });
        std::fs::write(&source, r#"
use std::{env,fs,path::Path,process::ExitCode};
fn main()->ExitCode {
 let a:Vec<String>=env::args().skip(1).collect();
 let ok=match a.first().map(String::as_str) {
  Some("exists") if a.len()==2 => Path::new(&a[1]).is_file(),
  Some("contains") if a.len()>=3 => fs::read_to_string(&a[1]).is_ok_and(|s| a[2..].iter().all(|needle|s.contains(needle))),
  Some("check-write") if a.len()==4 => fs::read_to_string(&a[1]).is_ok_and(|s|s.trim()==a[2]) && fs::write(&a[3],b"verified\n").is_ok(),
  Some("suite") if a.len()==1 => {
   let cases=fs::read_to_string("cases.txt").unwrap();let solved=fs::read_to_string("solved.txt").unwrap_or_default();
   let count=cases.lines().filter(|line|solved.contains(line)).count();
   fs::write("metrics.txt",format!("cases_passed={count}\n")).is_ok()
  },
  _=>false,
 }; if ok {ExitCode::SUCCESS}else{ExitCode::FAILURE}
}
"#).unwrap();
        let status=std::process::Command::new("rustc").args(["--edition=2021","--crate-name","susi_test_verifier"])
            .arg(&source).arg("-o").arg(&output).status().unwrap();
        assert!(status.success(), "native verifier must compile");
        output.to_str().unwrap().to_owned()
    })
}

pub fn argv<'a>(args: &[&'a str]) -> Vec<&'a str> {
    std::iter::once(program())
        .chain(args.iter().copied())
        .collect()
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod inline_tests {
    use super::*;

    #[test]
    fn program_compiles_and_caches_existing_executable_path() {
        let p1 = program();
        let p2 = program();
        assert_eq!(p1, p2);
        assert!(std::path::Path::new(p1).is_file());
    }

    #[test]
    fn argv_prepends_program_before_caller_arguments() {
        let full = argv(&["exists", "foo.txt"]);
        assert_eq!(full.len(), 3);
        assert_eq!(full[0], program());
        assert_eq!(full[1], "exists");
        assert_eq!(full[2], "foo.txt");
    }

    #[test]
    fn compiled_verifier_exists_command_distinguishes_present_and_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sample.txt");
        std::fs::write(&file, "hello").unwrap();
        let ok = std::process::Command::new(program())
            .args(["exists", file.to_str().unwrap()])
            .status()
            .unwrap();
        assert!(ok.success());

        let missing = dir.path().join("missing.txt");
        let fail = std::process::Command::new(program())
            .args(["exists", missing.to_str().unwrap()])
            .status()
            .unwrap();
        assert!(!fail.success());
    }

    #[test]
    fn compiled_verifier_contains_command_checks_all_needles() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("doc.txt");
        std::fs::write(&file, "alpha beta gamma").unwrap();
        let ok = std::process::Command::new(program())
            .args(["contains", file.to_str().unwrap(), "alpha", "gamma"])
            .status()
            .unwrap();
        assert!(ok.success());

        let fail = std::process::Command::new(program())
            .args(["contains", file.to_str().unwrap(), "alpha", "delta"])
            .status()
            .unwrap();
        assert!(!fail.success());
    }

    #[test]
    fn compiled_verifier_check_write_and_suite_commands_produce_expected_files() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("input.txt");
        let receipt = dir.path().join("receipt.txt");
        std::fs::write(&src, "  expected-token \n").unwrap();

        let status = std::process::Command::new(program())
            .args([
                "check-write",
                src.to_str().unwrap(),
                "expected-token",
                receipt.to_str().unwrap(),
            ])
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(std::fs::read_to_string(&receipt).unwrap(), "verified\n");

        std::fs::write(dir.path().join("cases.txt"), "case-a\ncase-b\ncase-c\n").unwrap();
        std::fs::write(dir.path().join("solved.txt"), "case-a\ncase-c\n").unwrap();
        let suite_ok = std::process::Command::new(program())
            .current_dir(dir.path())
            .arg("suite")
            .status()
            .unwrap();
        assert!(suite_ok.success());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("metrics.txt")).unwrap(),
            "cases_passed=2\n"
        );
    }
}
