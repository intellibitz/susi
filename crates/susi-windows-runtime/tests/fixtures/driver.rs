//! Standalone native qualification driver. Arguments: runtime.obj, OS-UCRT
//! import library, OS-stack import library, lifecycle.rs, private output dir.
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

fn run(mut command: Command, directory: &Path, label: &str, seconds: u64) -> Output {
    let stdout = directory.join(format!("{label}.stdout"));
    let stderr = directory.join(format!("{label}.stderr"));
    command
        .stdin(Stdio::null())
        .stdout(fs::File::create(&stdout).unwrap())
        .stderr(fs::File::create(&stderr).unwrap());
    let mut child = command.spawn().expect("spawn qualified native process");
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if start.elapsed() > Duration::from_secs(seconds) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("native process timed out: {label}");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    Output {
        status,
        stdout: fs::read(stdout).unwrap(),
        stderr: fs::read(stderr).unwrap(),
    }
}

fn main() {
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    assert_eq!(
        args.len(),
        5,
        "runtime.obj OS-UCRT.lib OS-stack.lib lifecycle.rs output-directory"
    );
    let root = &args[4];
    fs::create_dir_all(root).unwrap();
    for abort in [false, true] {
        let profile = if abort { "abort" } else { "unwind" };
        let executable = root.join(format!("lifecycle-{profile}.exe"));
        let map = root.join(format!("lifecycle-{profile}.map"));
        let mut compile = Command::new("rustc");
        compile
            .arg("--edition=2021")
            .arg(&args[3])
            .arg("-o")
            .arg(&executable);
        let rlib = args[0]
            .extension()
            .is_some_and(|extension| extension == "rlib");
        if rlib {
            compile
                .arg("--extern")
                .arg(format!("susi_windows_runtime={}", args[0].display()))
                .args(["--cfg", "susi_runtime_rlib"]);
        }
        for arg in [
            "/ENTRY:susi_pe_entry".into(),
            "/MERGE:.CRT=.rdata".into(),
            "/INCLUDE:_tls_used".into(),
            "/NODEFAULTLIB:msvcrt.lib".into(),
            "/NODEFAULTLIB:vcruntime.lib".into(),
            "ucrt.lib".into(),
            args[1].display().to_string(),
            args[2].display().to_string(),
            format!("/MAP:{}", map.display()),
        ] {
            compile.arg("-C").arg(format!("link-arg={arg}"));
        }
        if !rlib {
            compile
                .arg("-C")
                .arg(format!("link-arg={}", args[0].display()));
        }
        if abort {
            compile.args(["-C", "panic=abort", "-C", "opt-level=3", "-C", "lto=fat"]);
        }
        let output = run(compile, root, &format!("compile-{profile}"), 120);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let map = fs::read_to_string(map).unwrap();
        for forbidden in [
            "msvcrt:",
            "vcruntime:",
            "libcmt:",
            "exe_main.obj",
            "__scrt_",
        ] {
            assert!(
                !map.contains(forbidden),
                "foreign CRT object in link map: {forbidden}"
            );
        }
        assert!(map.contains("susi_pe_entry"));
        let runtime_name = args[0].file_name().unwrap().to_str().unwrap();
        for line in map.lines() {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.len() >= 4
                && fields[2].len() == 16
                && fields[2].bytes().all(|b| b.is_ascii_hexdigit())
            {
                let origin = fields[fields.len() - 1];
                assert!(
                    origin.ends_with(".dll")
                        || origin.ends_with(".rcgu.o")
                        || origin == runtime_name
                        || matches!(origin, "<absolute>" | "<linker-defined>"),
                    "unclassified implementation origin in link map: {origin}"
                );
            }
        }
        for mode in [
            "normal",
            "exit",
            "rtti0",
            "rtti1",
            "init-failure",
            if abort { "abort" } else { "uncaught" },
        ] {
            let label = format!("{profile}-{mode}");
            let log = root.join(format!("{label}.lifecycle"));
            assert!(
                !log.exists(),
                "qualification output directory must be fresh"
            );
            let mut command = Command::new(&executable);
            command
                .arg("unicode-λ space")
                .arg("")
                .arg(mode)
                .env("SUSI_PROBE_LOG", &log)
                .env_remove("SUSI_PROBE_FAIL_INIT");
            if mode == "init-failure" {
                command.env("SUSI_PROBE_FAIL_INIT", "1");
            }
            let output = run(command, root, &label, 20);
            let expected = match mode {
                "normal" => 37,
                "exit" => 29,
                "uncaught" => 101,
                "abort" => -1073740791,
                _ => 255,
            };
            assert_eq!(
                output.status.code(),
                Some(expected),
                "{label}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let lifecycle = fs::read_to_string(&log).unwrap_or_default();
            match mode {
                "normal" | "uncaught" => assert_eq!(
                    lifecycle, "tls\nmain-drop\nuser-exit\npreterm\nterm\ntls\n",
                    "{label}"
                ),
                "exit" | "rtti0" | "rtti1" => assert_eq!(lifecycle, "tls\ntls\n", "{label}"),
                "init-failure" => assert!(lifecycle.is_empty() && output.stdout.is_empty()),
                _ => assert_eq!(lifecycle, "tls\n", "{label}"),
            }
            println!("PASS {label}: exit {expected}");
        }
    }
}
