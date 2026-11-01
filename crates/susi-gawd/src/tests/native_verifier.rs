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
