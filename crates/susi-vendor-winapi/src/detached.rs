//! Original Windows detached-process boundary; no shell or inherited handles.
//!
//! `CreateProcessW` receives an explicit application, mutable UTF-16 command
//! line and environment, and FALSE for handle inheritance. The child must open
//! its own log. This avoids changing process-global handle or environment state.

use std::{
    ffi::{OsStr, OsString},
    io,
    os::windows::ffi::OsStrExt,
    path::Path,
};

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn wide(value: &OsStr) -> io::Result<Vec<u16>> {
    let value: Vec<_> = value.encode_wide().collect();
    if value.contains(&0) {
        return Err(invalid("embedded NUL in process input"));
    }
    Ok(value)
}

// The first argument has different Windows parsing rules; it is constructed
// separately from the validated executable path. Remaining arguments follow
// the documented CRT/Rust backslash-before-quote convention.
fn append_argument(command: &mut Vec<u16>, arg: &OsStr) -> io::Result<()> {
    let arg = wide(arg)?;
    command.push(b'"' as u16);
    let mut slashes = 0;
    for unit in arg {
        if unit == b'\\' as u16 {
            slashes += 1;
            continue;
        }
        command.extend(std::iter::repeat_n(
            b'\\' as u16,
            if unit == b'"' as u16 {
                slashes * 2 + 1
            } else {
                slashes
            },
        ));
        slashes = 0;
        command.push(unit);
    }
    command.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
    command.push(b'"' as u16);
    Ok(())
}

/// Starts an executable without sharing any inheritable caller handle.
///
/// Both paths must be absolute. `environment` is the complete child environment,
/// not an overlay. The process is detached from the console and receives no
/// standard handles; it must open its own files. Success means creation, not
/// completion. Returned process/thread handles are closed before returning.
#[allow(unsafe_code)]
pub fn spawn(
    program: &Path,
    args: &[OsString],
    cwd: &Path,
    environment: &[(OsString, OsString)],
) -> io::Result<()> {
    use winapi::um::{
        handleapi::CloseHandle,
        processthreadsapi::{CreateProcessW, PROCESS_INFORMATION, STARTUPINFOW},
        winbase::{
            CREATE_NEW_PROCESS_GROUP, CREATE_UNICODE_ENVIRONMENT, DETACHED_PROCESS,
            STARTF_USESHOWWINDOW,
        },
    };
    if !program.is_absolute() || !cwd.is_absolute() {
        return Err(invalid("detached process paths must be absolute"));
    }
    let mut application = wide(program.as_os_str())?;
    if application.contains(&(b'"' as u16)) {
        return Err(invalid("quote in executable path"));
    }
    let mut command = vec![b'"' as u16];
    command.extend(&application);
    command.push(b'"' as u16);
    for arg in args {
        command.push(b' ' as u16);
        append_argument(&mut command, arg)?;
    }
    command.push(0);
    if command.len() > 32767 {
        return Err(invalid("Windows command line exceeds 32767 UTF-16 units"));
    }
    application.push(0);
    let mut directory = wide(cwd.as_os_str())?;
    directory.push(0);
    let mut entries = Vec::with_capacity(environment.len());
    for (key, value) in environment {
        let key = wide(key)?;
        // Hidden shell entries start with '=' (=C:, =ExitCode): legal keys, so
        // only an '=' after the first unit would corrupt the block.
        if key.is_empty() || key[1..].contains(&(b'=' as u16)) {
            return Err(invalid("invalid environment key"));
        }
        let mut entry = key.clone();
        entry.push(b'=' as u16);
        entry.extend(wide(value)?);
        entry.push(0);
        entries.push((key, entry));
    }
    // Windows requires a case-insensitive Unicode sorted environment block.
    // SAFETY: all buffers are live, lengths fit i32 (bounded below), and the
    // ordinal OS comparison reads exactly those UTF-16 units, without mutation.
    if entries
        .iter()
        .any(|(key, entry)| key.len() > 32767 || entry.len() > 32767)
    {
        return Err(invalid("environment entry exceeds Windows bounds"));
    }
    let compare = |a: &[u16], b: &[u16]| unsafe {
        winapi::um::stringapiset::CompareStringOrdinal(
            a.as_ptr(),
            a.len() as i32,
            b.as_ptr(),
            b.len() as i32,
            1,
        )
    };
    let mut comparison_failed = false;
    entries.sort_by(|a, b| {
        let order = compare(&a.0, &b.0);
        comparison_failed |= order == 0;
        order.cmp(&2)
    });
    if comparison_failed {
        return Err(io::Error::other("Windows environment comparison failed"));
    }
    for pair in entries.windows(2) {
        match compare(&pair[0].0, &pair[1].0) {
            0 => return Err(io::Error::other("Windows environment comparison failed")),
            2 => return Err(invalid("duplicate case-insensitive environment key")),
            _ => {}
        }
    }
    let mut block: Vec<u16> = entries.into_iter().flat_map(|(_, entry)| entry).collect();
    block.push(0);
    if block.len() == 1 {
        block.push(0);
    }
    // SAFETY: STARTUPINFOW and PROCESS_INFORMATION are POD Windows ABI structs;
    // zero initializes reserved fields/pointers. cb describes the exact layout.
    // All input buffers stay allocated through the synchronous API call. Only
    // command is mutable as required by CreateProcessW. FALSE prevents handle
    // inheritance; null security attributes create non-inheritable new handles.
    // A successful call owns exactly the two output handles closed below; failed
    // calls transfer no handle ownership. No foreign pointers escape this call.
    unsafe {
        let mut startup: STARTUPINFOW = std::mem::zeroed();
        startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        startup.dwFlags = STARTF_USESHOWWINDOW;
        startup.wShowWindow = 0; // SW_HIDE; never request an interactive window.
        let mut process: PROCESS_INFORMATION = std::mem::zeroed();
        if CreateProcessW(
            application.as_ptr(),
            command.as_mut_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
            DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_UNICODE_ENVIRONMENT,
            block.as_mut_ptr().cast(),
            directory.as_ptr(),
            &mut startup,
            &mut process,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        CloseHandle(process.hThread);
        CloseHandle(process.hProcess);
    }
    Ok(())
}

#[cfg(test)]
// Test assertions and fixture setup must fail loudly; production retains denied panic-path lints.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::{
        fs,
        process::Command,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    fn wait(path: &Path) {
        let start = Instant::now();
        while !path.exists() {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "missing {}",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn environment(root: &Path, mode: &str) -> Vec<(OsString, OsString)> {
        let mut values: Vec<_> = std::env::vars_os()
            .filter(|(key, _)| !key.to_string_lossy().starts_with("SUSI_DETACHED_TEST_"))
            .collect();
        values.push((
            "SUSI_DETACHED_TEST_ROOT".into(),
            root.as_os_str().to_owned(),
        ));
        values.push(("SUSI_DETACHED_TEST_MODE".into(), mode.into()));
        values
    }
    fn arguments() -> Vec<OsString> {
        let mut result: Vec<OsString> = ["--exact", "detached::tests::child", "--nocapture"]
            .into_iter()
            .map(Into::into)
            .collect();
        for value in [
            "",
            "space and Ελληνικά 🦀",
            "quote\"inside",
            "backslash\\\"quote",
            "trailing space\\",
            "\\\\",
        ] {
            result.push("--skip".into());
            result.push(value.into());
        }
        result
    }
    fn encode(arguments: impl Iterator<Item = OsString>) -> Vec<u8> {
        let mut bytes = Vec::new();
        for argument in arguments {
            let wide: Vec<_> = argument.encode_wide().collect();
            bytes.extend((wide.len() as u32).to_le_bytes());
            for unit in wide {
                bytes.extend(unit.to_le_bytes());
            }
        }
        bytes
    }
    #[test]
    fn child() {
        let Some(root) = std::env::var_os("SUSI_DETACHED_TEST_ROOT") else {
            return;
        };
        let root = std::path::PathBuf::from(root);
        let mode = std::env::var("SUSI_DETACHED_TEST_MODE").unwrap();
        if mode == "wrapper" {
            spawn(
                &std::env::current_exe().unwrap(),
                &arguments(),
                &root,
                &environment(&root, "sleeper"),
            )
            .unwrap();
            wait(&root.join("started"));
            println!("wrapper completed while detached child remains alive");
        } else {
            fs::write(root.join("arguments"), encode(std::env::args_os().skip(1))).unwrap();
            fs::write(root.join("started"), b"ready").unwrap();
            if mode == "sleeper" {
                let start = Instant::now();
                while !root.join("release").exists() && start.elapsed() < Duration::from_secs(5) {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            fs::write(root.join("finished"), b"done").unwrap();
        }
    }
    #[test]
    fn detached_unicode_arguments_and_caller_pipe_eof_are_preserved() {
        let root = std::env::temp_dir().join(format!(
            "susi-native-detached-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let started = Instant::now();
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "detached::tests::child", "--nocapture"])
            .envs(environment(&root, "wrapper"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let elapsed = started.elapsed();
        // Always release the owned sleeper before assertions, including failure.
        fs::write(root.join("release"), b"release").unwrap();
        wait(&root.join("finished"));
        assert!(
            elapsed < Duration::from_secs(4),
            "caller pipe stayed inherited for {elapsed:?}"
        );
        assert_eq!(
            fs::read(root.join("arguments")).unwrap(),
            encode(arguments().into_iter())
        );
    }
    #[test]
    fn invalid_inputs_and_missing_executable_are_errors() {
        let cwd = std::env::temp_dir();
        let exe = std::env::current_exe().unwrap();
        assert!(spawn(Path::new("relative.exe"), &[], &cwd, &[]).is_err());
        assert!(spawn(&exe, &["NUL\0value".into()], &cwd, &[]).is_err());
        assert!(spawn(
            &exe,
            &[],
            &cwd,
            &[("PATH".into(), "a".into()), ("path".into(), "b".into())]
        )
        .is_err());
        assert!(spawn(&exe, &[], &cwd, &[("A=B".into(), "c".into())]).is_err());
        assert!(spawn(&cwd.join("susi-missing-detached-test.exe"), &[], &cwd, &[]).is_err());
    }

    #[test]
    fn hidden_shell_environment_entries_are_accepted() {
        // cmd.exe hands every child =ExitCode and =<drive>: entries.
        let exe = std::env::current_exe().unwrap();
        let hidden = [
            ("=ExitCode".into(), "00000000".into()),
            ("=C:".into(), "C:\\".into()),
            ("SystemRoot".into(), std::env::var_os("SystemRoot").unwrap()),
        ];
        spawn(
            &exe,
            &["--exact".into(), "detached::tests::missing_test".into()],
            &std::env::temp_dir(),
            &hidden,
        )
        .unwrap();
    }
}
