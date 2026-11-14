use super::*;

#[cfg(windows)]
#[test]
fn registry_environment_isolation_covers_keys_settings_homes_and_cached_values() {
    const CHILD: &str = "SUSI_REGISTRY_ISOLATION_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let scratch = std::env::temp_dir().join(format!(
            "susi-registry-isolation-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&scratch).unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child.env_clear();
        for key in ["SystemRoot", "WINDIR"] {
            if let Some(value) = std::env::var_os(key) {
                child.env(key, value);
            }
        }
        let result = child
            .args([
                "--exact",
                "secrets::tests::registry_environment_isolation_covers_keys_settings_homes_and_cached_values",
                "--test-threads=1",
            ])
            .env(CHILD, "1")
            .env("SUSI_NO_REGISTRY_ENV", "1")
            .env("HOME", &scratch)
            .env("USERPROFILE", &scratch)
            .env("SUSI_HOME", &scratch)
            .current_dir(&scratch)
            .output()
            .unwrap();
        std::fs::remove_dir(&scratch).unwrap();
        let stdout = String::from_utf8_lossy(&result.stdout);
        assert!(
            result.status.success() && stdout.contains("1 passed; 0 failed"),
            "{stdout}\n{}",
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }

    const KEY: &str = "SUSI_TEST_REGISTRY_KEY";
    const SETTING: &str = "SUSI_TEST_REGISTRY_SETTING";
    let fixture_home = std::env::var("SUSI_HOME").unwrap();
    TEST_WINDOWS_SCOPES
        .set(BTreeMap::from([
            (
                "User".into(),
                BTreeMap::from([
                    (KEY.into(), "synthetic-key".into()),
                    (SETTING.into(), "synthetic-user-setting".into()),
                    ("SUSI_HOME".into(), "synthetic-registry-home".into()),
                ]),
            ),
            (
                "Machine".into(),
                BTreeMap::from([(SETTING.into(), "synthetic-machine-setting".into())]),
            ),
        ]))
        .unwrap();
    test_dotenv::with("SUSI_NO_REGISTRY_ENV=1\n", || {
        test_dotenv::with_home("", || {
            // The launcher sets isolation before the first lookup. No registry
            // snapshot, including our synthetic one, is consulted.
            assert!(lookup(KEY).is_none());
            assert!(setting(SETTING).is_none());
            std::env::remove_var("SUSI_HOME");
            assert_eq!(
                data_home(),
                std::path::Path::new(&fixture_home).join(".susi")
            );
            assert_eq!(
                TEST_WINDOWS_READS.load(std::sync::atomic::Ordering::SeqCst),
                0
            );

            // Normal behavior remains available. A .env switch cannot change
            // this policy; only the explicitly supplied process environment can.
            std::env::remove_var("SUSI_NO_REGISTRY_ENV");
            assert_eq!(lookup(KEY).unwrap().expose(), "synthetic-key");
            assert_eq!(setting(SETTING).as_deref(), Some("synthetic-user-setting"));
            assert_eq!(
                registry_value(SETTING, "Machine").as_deref(),
                Some("synthetic-machine-setting")
            );
            assert_eq!(
                data_home(),
                std::path::PathBuf::from("synthetic-registry-home")
            );
            let reads = TEST_WINDOWS_READS.load(std::sync::atomic::Ordering::SeqCst);
            assert!(reads > 0);

            // Presence is fail-closed, even an empty or unexpected value. The
            // old key cache and initialized registry snapshot remain bypassed.
            for value in ["", "0", "1"] {
                std::env::set_var("SUSI_NO_REGISTRY_ENV", value);
                assert!(lookup(KEY).is_none());
                assert!(setting(SETTING).is_none());
                assert_eq!(
                    data_home(),
                    std::path::Path::new(&fixture_home).join(".susi")
                );
                assert_eq!(
                    TEST_WINDOWS_READS.load(std::sync::atomic::Ordering::SeqCst),
                    reads
                );
            }
            std::env::set_var(KEY, "synthetic-process-key");
            assert_eq!(
                first_fitting(KEY, None).as_deref(),
                Some("synthetic-process-key")
            );
            test_dotenv::with_home(
                "SUSI_TEST_REGISTRY_SETTING=synthetic-file-setting\n",
                || {
                    assert_eq!(setting(SETTING).as_deref(), Some("synthetic-file-setting"));
                },
            );
        })
    });
}

#[test]
fn debug_and_display_never_show_the_key() {
    let key = ApiKey("sk-very-secret-value".to_string());
    assert_eq!(format!("{key:?}"), "ApiKey(<redacted>)");
    assert_eq!(format!("{key}"), "<redacted>");
    assert!(!format!("{key:?} {key}").contains("sk-very"));
    assert_eq!(key.expose(), "sk-very-secret-value");
}

#[test]
fn names_that_are_not_env_names_are_refused() {
    assert!(!valid_name(""));
    assert!(!valid_name("lower_case"));
    assert!(!valid_name("KEY;rm -rf"));
    assert!(!valid_name("KEY VALUE"));
    assert!(valid_name("DEEPSEEK_API_KEY"));
    assert!(lookup("bad name").is_none());
}

#[test]
fn the_registry_names_the_anthropic_key_form_only() {
    let registry = crate::providers::Registry::builtin();
    assert_eq!(
        registry.key_prefix_for("ANTHROPIC_API_KEY"),
        Some(("anthropic", "sk-ant-"))
    );
    assert_eq!(registry.key_prefix_for("DEEPSEEK_API_KEY"), None);
    assert_eq!(
        registry.key_prefix_for("SUSI_HARNESS_BUDGET_ANTHROPIC_USD"),
        None
    );
}

#[test]
fn a_value_of_the_wrong_form_is_passed_over() {
    // The process scope is the one a test can set; a wrong form there is
    // skipped. A name no other scope sets then has no key at all.
    let name = "SUSI_TEST_SHAPED_KEY_0001";
    std::env::set_var(name, "not-the-right-form");
    assert_eq!(first_fitting(name, Some(("example", "sk-ex-"))), None);
    assert_eq!(
        first_fitting(name, None).as_deref(),
        Some("not-the-right-form"),
        "a provider with no published form takes any value"
    );
    std::env::set_var(name, "sk-ex-123");
    assert_eq!(
        first_fitting(name, Some(("example", "sk-ex-"))).as_deref(),
        Some("sk-ex-123")
    );
    std::env::remove_var(name);
}

#[test]
fn scope_names_are_normalized_without_blank_or_invalid_values() {
    let scope = normalize_scope([
        ("Deepseek_Api_Key".into(), " sk-1 ".into()),
        ("EMPTY".into(), "".into()),
        ("bad name".into(), "x".into()),
        ("PATH".into(), "C:/x".into()),
        ("EXPANDED".into(), "%USERPROFILE%/data".into()),
    ]);
    assert_eq!(
        scope.get("DEEPSEEK_API_KEY").map(String::as_str),
        Some("sk-1")
    );
    assert!(!scope.contains_key("EMPTY"));
    assert!(!scope.contains_key("BAD NAME"));
    assert_eq!(scope.get("PATH").map(String::as_str), Some("C:/x"));
    assert_eq!(
        scope.get("EXPANDED").map(String::as_str),
        Some("%USERPROFILE%/data")
    );
}

#[cfg(windows)]
#[test]
fn persistent_environment_values_are_read_using_native_registry_api() {
    use windows_registry::CURRENT_USER;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = format!(
        "Software\\Susi-Registry-Test-{}-{stamp}",
        std::process::id()
    );
    assert!(CURRENT_USER.open(&path).is_err());
    struct OwnedKey(String);
    impl Drop for OwnedKey {
        fn drop(&mut self) {
            assert!(
                self.0.starts_with("Software\\Susi-Registry-Test-")
                    && self.0.matches('\\').count() == 1
            );
            let _ = windows_registry::CURRENT_USER.remove_tree(&self.0);
        }
    }
    let cleanup = OwnedKey(path.clone());
    let key = CURRENT_USER.create(&path).unwrap();
    key.set_string("Susi_Test_Value", " native-test ").unwrap();
    key.set_string("EMPTY", " ").unwrap();
    key.set_string("bad name", "ignored").unwrap();
    let values = read_registry_scope(CURRENT_USER, &path);
    assert_eq!(
        values.get("SUSI_TEST_VALUE").map(String::as_str),
        Some("native-test")
    );
    assert!(!values.contains_key("EMPTY") && !values.contains_key("BAD NAME"));
    drop(key);
    drop(cleanup);
    assert!(CURRENT_USER.open(&path).is_err());
}

#[test]
fn key_shaped_text_is_redacted_and_ordinary_text_is_not() {
    let body = r#"{"error":{"message":"Incorrect API key provided: sk-litel**************************2026. See https://platform.openai.com/account/api-keys."}}"#;
    let clean = redact(body);
    assert!(!clean.contains("sk-litel"), "{clean}");
    assert!(
        clean.contains("Incorrect API key provided: <redacted>"),
        "{clean}"
    );
    assert!(clean.contains("api-keys"), "ordinary words stay: {clean}");
    assert_eq!(
        redact("key AIzaSyA1234567890abcdef ok"),
        "key <redacted> ok"
    );
    assert_eq!(
        redact("Bearer sk-ant-api03-abcdefghijklmnop end"),
        "Bearer <redacted> end"
    );
    // Short runs are words, not keys.
    assert_eq!(redact("risk-free task-ok sk-1"), "risk-free task-ok sk-1");
    assert_eq!(redact("context length exceeded"), "context length exceeded");
    // A prefix inside a word is not a key: command output a worker reads
    // keeps its file names.
    assert_eq!(
        redact("src/task-runner-tests.rs risk-assessment-module"),
        "src/task-runner-tests.rs risk-assessment-module"
    );
    assert_eq!(
        redact("OPENAI_API_KEY=sk-proj-abcdefghij\n'AIzaSyA1234567890abcdef'"),
        "OPENAI_API_KEY=<redacted>\n'<redacted>'"
    );
}

#[test]
fn a_key_is_masked_after_an_escape_a_digit_or_punctuation_too() {
    // Each of these passed through unmasked while a key had to start a
    // word: the character before it is a letter, a digit or `-`.
    for (text, masked) in [
        (
            "Authorization=Bearer%20sk-proj-abcdefghijklmnop",
            "Authorization=Bearer%20<redacted>",
        ),
        (
            "url?q=key%3DAIzaSyAbcdefghijklmnop",
            "url?q=key%3D<redacted>",
        ),
        (
            r#"{"out":"exit code: 0\nsk-proj-abcdefghijklmnop"}"#,
            r#"{"out":"exit code: 0\n<redacted>"}"#,
        ),
        (
            r#"{"k":"\u003dsk-proj-abcdefghijklmnop"}"#,
            r#"{"k":"\u003d<redacted>"}"#,
        ),
        ("**sk-proj-abcdefghijklmnop**", "**<redacted>"),
        ("-sk-proj-Abc123def456ghi\n+x", "-<redacted>\n+x"),
        ("v2sk-proj-abcdefghijklmnop", "v2<redacted>"),
        ("name_AIzaSyAbcdefghijklmnop", "name_<redacted>"),
    ] {
        assert_eq!(redact(text), masked, "{text}");
    }
    // An escape's letter stands for a separator, so after one even a run
    // short enough to pass for the rest of a word is masked.
    let backslash = '\x5c';
    for escape in [
        format!("{backslash}n"),
        format!("{backslash}t"),
        format!("{backslash}u003d"),
        format!("{backslash}x3d"),
        "%3D".to_string(),
        "%3d".to_string(),
    ] {
        assert_eq!(
            redact(&format!("line{escape}sk-proj-abcdefghij")),
            format!("line{escape}<redacted>"),
            "{escape}"
        );
    }
    // Glued to a word, a run that is plainly a key is masked all the same.
    assert_eq!(
        redact("tokensk-0123456789abcdef0123456789abcdef"),
        "token<redacted>"
    );
    assert_eq!(redact("xsk-abcd****1234"), "x<redacted>");
    assert_eq!(redact("keyAIzaSyA1b2C3d4E5f6"), "key<redacted>");
}

#[test]
fn words_that_contain_a_key_prefix_are_left_alone() {
    for text in [
        "src/task-runner-tests.rs risk-assessment-module",
        "pip install Flask-SQLAlchemy Flask-SocketIO",
        "task-runner-v2-tests disk-usage-report.json",
        "mask-sensitive-data whisk-it-together",
    ] {
        assert_eq!(redact(text), text);
    }
}

#[test]
fn dotenv_files_are_read_as_tools_write_them() {
    let parsed = parse_dotenv(concat!(
        "# keys\n",
        "OPENAI_API_KEY=sk-abc\n",
        "export GEMINI_API_KEY=\"g-1\"\n",
        "EMPTY=\n",
        "lower=x\n",
        "  ANTHROPIC_API_KEY = 'sk-ant-1'  \n",
        "not a line\n",
    ));
    assert_eq!(
        parsed.get("OPENAI_API_KEY").map(String::as_str),
        Some("sk-abc")
    );
    assert_eq!(
        parsed.get("GEMINI_API_KEY").map(String::as_str),
        Some("g-1")
    );
    assert_eq!(
        parsed.get("ANTHROPIC_API_KEY").map(String::as_str),
        Some("sk-ant-1")
    );
    assert!(!parsed.contains_key("EMPTY"));
    assert!(!parsed.contains_key("lower"));
    assert_eq!(parsed.len(), 3);
}

/// `text` as Windows PowerShell 5 writes it with `>` or `Out-File`:
/// UTF-16LE with a byte-order mark and CRLF line ends.
fn utf16le_with_bom(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xFE];
    for unit in text.replace('\n', "\r\n").encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

#[test]
fn a_dotenv_file_in_any_encoding_windows_writes_is_read() {
    let line = "OPENAI_API_KEY=sk-test-encoding\n";
    let mut utf16be = vec![0xFE, 0xFF];
    for unit in line.encode_utf16() {
        utf16be.extend_from_slice(&unit.to_be_bytes());
    }
    let utf8_bom = [b"\xEF\xBB\xBF".as_slice(), line.as_bytes()].concat();
    let crlf = line.replace('\n', "\r\n").into_bytes();
    for (form, bytes) in [
        ("UTF-16LE with a BOM", utf16le_with_bom(line)),
        ("UTF-16BE with a BOM", utf16be),
        ("UTF-8 with a BOM", utf8_bom),
        ("UTF-8 with CRLF", crlf),
        ("UTF-8", line.as_bytes().to_vec()),
    ] {
        let (text, problem) = decode_dotenv(&bytes);
        assert_eq!(problem, None, "{form}");
        assert_eq!(
            parse_dotenv(&text)
                .get("OPENAI_API_KEY")
                .map(String::as_str),
            Some("sk-test-encoding"),
            "{form}"
        );
    }
    // Through the file, as the harness reads it.
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let path = dir.path().join(".env");
    std::fs::write(&path, utf16le_with_bom(line)).unwrap();
    assert_eq!(
        read_dotenv(&path).get("OPENAI_API_KEY").map(String::as_str),
        Some("sk-test-encoding")
    );
}

#[test]
fn a_dotenv_file_that_does_not_decode_says_so_and_keeps_the_lines_that_do() {
    // A Windows-1252 comment: the key line still counts.
    let (text, problem) = decode_dotenv(b"# caf\xE9\nOPENAI_API_KEY=sk-test-ansi\n");
    assert_eq!(problem, Some("is not UTF-8 text"));
    assert_eq!(
        parse_dotenv(&text)
            .get("OPENAI_API_KEY")
            .map(String::as_str),
        Some("sk-test-ansi")
    );
    let (_, problem) = decode_dotenv(b"K\0=\0v\0");
    assert!(problem.is_some_and(|p| p.contains("NUL")), "{problem:?}");
    let (_, problem) = decode_dotenv(&[0xFF, 0xFE, 0x41]);
    assert_eq!(problem, Some("is not valid UTF-16 text"));
}

#[test]
fn a_dotenv_directory_is_passed_over() {
    // Python projects keep a virtual environment in a `.env` folder.
    let dir = susi_paths::tempfile::tempdir().unwrap();
    assert!(read_dotenv(dir.path()).is_empty());
    assert!(read_dotenv(&dir.path().join("missing")).is_empty());
}

#[test]
fn an_inline_comment_after_a_dotenv_value_is_not_part_of_the_key() {
    let parsed = parse_dotenv(concat!(
        "OPENAI_API_KEY=sk-abc  # personal\n",
        "ANTHROPIC_API_KEY=\"sk-ant-1\" # work\n",
        "GEMINI_API_KEY='g-1'#c\n",
        "DEEPSEEK_API_KEY=sk-hash#in-the-value\n",
        "MOONSHOT_API_KEY=\t# nothing but a comment\n",
        "export\tKIMI_API_KEY=\"k # quoted\"\n",
        "SUSI_HARNESS_PROVIDERS=\"C:\\cfg\\providers.json\"\n",
    ));
    let get = |name: &str| parsed.get(name).map(String::as_str);
    assert_eq!(get("OPENAI_API_KEY"), Some("sk-abc"));
    assert_eq!(get("ANTHROPIC_API_KEY"), Some("sk-ant-1"));
    assert_eq!(get("GEMINI_API_KEY"), Some("g-1"));
    assert_eq!(
        get("DEEPSEEK_API_KEY"),
        Some("sk-hash#in-the-value"),
        "a # with no space before it is part of the value"
    );
    assert_eq!(get("MOONSHOT_API_KEY"), None);
    assert_eq!(get("KIMI_API_KEY"), Some("k # quoted"));
    assert_eq!(
        get("SUSI_HARNESS_PROVIDERS"),
        Some("C:\\cfg\\providers.json"),
        "no escapes are decoded"
    );
}

#[test]
fn susi_home_in_a_directorys_dotenv_never_moves_the_data_home() {
    // The data home's `.env` is found through SUSI_HOME; it cannot say
    // where it is itself.
    assert_eq!(scope_value(HOME_ENV, DATA_HOME_DOTENV), None);
    let set_elsewhere = process_value(HOME_ENV)
        .or_else(|| registry_value(HOME_ENV, "User"))
        .or_else(|| registry_value(HOME_ENV, "Machine"));
    if set_elsewhere.is_some() {
        eprintln!("SUSI_HOME is set in the environment, which comes first; skipping");
        return;
    }
    // A repository's `.env` that chose the data home chose the `.env`
    // read there too, and with it a registry overlay.
    let home = test_dotenv::with("SUSI_HOME=/srv/susi-test-home\n", data_home);
    assert_ne!(home, std::path::PathBuf::from("/srv/susi-test-home"));
    assert_eq!(home, data_home());
    let reported = test_dotenv::with("SUSI_HOME=/srv/susi-test-home\n", || setting(HOME_ENV));
    assert_eq!(reported, None, "`env` reports the home in use");
}

#[test]
fn a_setting_that_decides_where_keys_go_is_never_read_from_a_directorys_dotenv() {
    let overlay = crate::providers::OVERLAY_ENV;
    assert!(USER_ONLY.contains(&overlay) && USER_ONLY.contains(&HOME_ENV));
    if setting(overlay).is_some() {
        eprintln!("{overlay} is set where this machine reads settings; skipping");
        return;
    }
    let line = format!("{overlay}=/srv/overlay-0002.json\n");
    assert_eq!(test_dotenv::with(&line, || setting(overlay)), None);
    assert_eq!(test_dotenv::with(&line, || setting_path(overlay)), None);
    // The user's own `.env`, in the data home, still sets it.
    assert_eq!(
        test_dotenv::with_home(&line, || setting(overlay)).as_deref(),
        Some("/srv/overlay-0002.json")
    );
}

#[test]
fn a_relative_path_in_the_data_homes_dotenv_is_taken_from_the_data_home() {
    let name = "SUSI_TEST_PATH_SETTING_0001";
    let path = test_dotenv::with_home(&format!("{name}=providers.json\n"), || setting_path(name));
    assert_eq!(path, Some(data_home().join("providers.json")));
    let absolute = std::env::temp_dir().join("providers-0001.json");
    let path = test_dotenv::with_home(&format!("{name}='{}'\n", absolute.display()), || {
        setting_path(name)
    });
    assert_eq!(path, Some(absolute));
    // Set in a directory's `.env`, an ordinary path setting is that
    // directory's and stays relative to it.
    let path = test_dotenv::with(&format!("{name}=providers.json\n"), || setting_path(name));
    assert_eq!(path, Some(std::path::PathBuf::from("providers.json")));
}

#[test]
fn a_setting_is_read_from_dotenv_and_never_masked_as_a_key() {
    let name = "SUSI_TEST_SETTING_ONLY_IN_DOTENV_0001";
    let value = test_dotenv::with(&format!("{name}=/srv/overlay-0001.json\n"), || {
        setting(name)
    });
    assert_eq!(value.as_deref(), Some("/srv/overlay-0001.json"));
    assert_eq!(setting(name), None, "the stand-in is gone");
    assert_eq!(
        redact("ledger at /srv/overlay-0001.json"),
        "ledger at /srv/overlay-0001.json"
    );
}

#[test]
fn the_data_home_is_a_susi_directory() {
    let home = data_home();
    assert!(
        home.ends_with(".susi") || std::env::var_os("SUSI_HOME").is_some(),
        "{}",
        home.display()
    );
}

#[test]
fn process_environment_is_read_and_blank_values_are_not_keys() {
    // A variable no scope sets: lookup finds nothing, and is_set agrees.
    assert!(!is_set("SUSI_TEST_SECRET_NEVER_SET_0001"));
    assert!(process_value("SUSI_TEST_SECRET_NEVER_SET_0001").is_none());
}
