use crate::ModelManager;
use std::fs;
use std::path::Path;

#[test]
fn fractional_gib_preserves_reserve_and_admits_the_audited_starter() {
    let available = (4.95_f64 * 1_073_741_824.0) as u64;
    let host = crate::zc_first_model::FirstRunHost {
        ram: ModelManager::inference_headroom_bytes(available, 4.0),
        disk: 2_000_000_000,
        offline: false,
        ..Default::default()
    };
    assert_eq!(host.ram, available - 4 * 1_073_741_824);
    assert!(matches!(
        ModelManager::starter_download_plan(&host, Path::new("unused-models")),
        crate::zc_first_model::FirstRunPlan::Fetch { .. }
    ));
    for available in [0, 4 * 1_073_741_824, 4 * 1_073_741_824 + 999_999_999] {
        let too_small = crate::zc_first_model::FirstRunHost {
            ram: ModelManager::inference_headroom_bytes(available, 4.0),
            ..host
        };
        assert!(matches!(
            ModelManager::starter_download_plan(&too_small, Path::new("unused-models")),
            crate::zc_first_model::FirstRunPlan::TooSmall
        ));
    }
}

#[cfg(not(feature = "local-engine"))]
#[test]
fn a_build_without_the_engine_refuses_the_starter_before_downloading() {
    let err = ModelManager::install_starter_model_on_request(Path::new("."))
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(err.contains("no local model engine"), "{err}");
    assert!(err.contains("nothing was downloaded"), "{err}");
}

#[test]
fn unverified_existing_tokenizer_is_not_overwritten() {
    let dir = std::env::temp_dir().join(format!(
        "susi-tokenizer-policy-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("unknown.tokenizer.json");
    fs::write(&path, b"user-owned invalid tokenizer bytes").unwrap();
    let step = crate::hardware::ModelLadderStep {
        step: 1,
        label: "fixture".into(),
        hf_repo: "org/model".into(),
        hf_file: "unknown.gguf".into(),
        tokenizer_repo: "org/model".into(),
        min_bytes: 0,
        expected_bytes: 0,
    };
    let result = ModelManager::ensure_ladder_tokenizer(
        &step,
        &dir,
        &crate::susi_sandbox::manager::SusiConfig::default(),
    );
    assert!(result.is_err());
    assert_eq!(
        fs::read(&path).unwrap(),
        b"user-owned invalid tokenizer bytes"
    );
    assert!(!path.with_extension("json.part").exists());
    fs::remove_file(&path).unwrap();
    fs::remove_dir(&dir).unwrap();
}

#[test]
fn offline_host_starter_download_plan_returns_offline_no_model_and_network_status_mocks() {
    let (ok, msg, latency_ms) = ModelManager::check_network_status();
    assert!(ok);
    assert!(msg.contains("mocked"));
    assert_eq!(latency_ms, 2);

    let offline_host = crate::zc_first_model::FirstRunHost {
        ram: 8 * 1_073_741_824,
        disk: 10_000_000_000,
        offline: true,
        ..Default::default()
    };
    assert!(matches!(
        ModelManager::starter_download_plan(&offline_host, Path::new("unused-offline-models")),
        crate::zc_first_model::FirstRunPlan::OfflineNoModel
    ));
}

#[test]
fn missing_tokenizer_for_unaudited_repo_fails_license_gate_before_download() {
    let dir = std::env::temp_dir().join(format!(
        "susi-tokenizer-unaudited-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let step = crate::hardware::ModelLadderStep {
        step: 1,
        label: "unaudited".into(),
        hf_repo: "unaudited-org/unaudited-model".into(),
        hf_file: "unaudited.gguf".into(),
        tokenizer_repo: String::new(),
        min_bytes: 0,
        expected_bytes: 0,
    };
    let result = ModelManager::ensure_ladder_tokenizer(
        &step,
        &dir,
        &crate::susi_sandbox::manager::SusiConfig::default(),
    );
    assert!(result.is_err());
    assert!(!dir.join("unaudited.tokenizer.json").exists());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn verify_model_ladder_and_expert_audit_report_steps_without_starting_downloads() {
    use std::io::{BufRead, Write};
    let _env_lock = crate::susi_core::commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!(
        "susi-ladder-verify-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    let mut env = susi_paths::test_env::EnvGuard::isolated();
    env.set("HOME", &dir);
    env.set("SUSI_HOME", &dir);
    env.remove("HF_TOKEN");

    // The one-shot server that answers the network probe. Its accept is
    // bounded: a probe that never arrives must fail the test, never hang it.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let worker = std::thread::spawn(move || -> Result<String, String> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let (mut stream, _) = loop {
            match listener.accept() {
                Ok(conn) => break conn,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if std::time::Instant::now() >= deadline {
                        return Err(
                            "the network probe never reached the test's hf_base_url within 20 s"
                                .to_string(),
                        );
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(e) => return Err(format!("accept failed: {e}")),
            }
        };
        stream.set_nonblocking(false).map_err(|e| e.to_string())?;
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .map_err(|e| e.to_string())?;
        let mut request = String::new();
        std::io::BufReader::new(stream.try_clone().map_err(|e| e.to_string())?)
            .read_line(&mut request)
            .map_err(|e| e.to_string())?;
        let body = r#"{"id":"org/starter","cardData":{"license":"apache-2.0"},"siblings":[{"rfilename":"starter-Q4_0.gguf","size":1024}]}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .map_err(|e| e.to_string())?;
        Ok(request)
    });

    let mut cfg = crate::susi_sandbox::manager::SusiConfig::default();
    cfg.settings
        .insert("hf_base_url".into(), serde_json::json!(base));
    cfg.settings.insert(
        "model_ladder".into(),
        serde_json::json!([{
            "step": 1,
            "label": "starter-tier",
            "hf_repo": "org/starter",
            "hf_file": "starter-Q4_0.gguf",
            "tokenizer_repo": "org/starter",
            "min_ram_gb": 0.0,
            "min_bytes": 1024,
            "expected_bytes": 1024
        }]),
    );
    cfg.settings.insert(
        "model_scoring_heuristics".into(),
        serde_json::json!({
            "native_candle_bonus": 15.0,
            "size_gb_multipliers": {},
            "system_ram_buffer_gb": 0.0
        }),
    );
    cfg.save(&susi_paths::SusiDirs::config_dir()).unwrap();
    let reloaded = crate::susi_sandbox::manager::SusiConfig::load_global().unwrap();
    assert_eq!(
        reloaded.model_ladder().len(),
        1,
        "load_global must read back the ladder just saved to {}",
        dir.display()
    );

    let report = ModelManager::verify_model_ladder(&dir).unwrap();
    let request = worker.join().unwrap().unwrap_or_else(|e| {
        panic!(
            "{e}; verify_model_ladder reported {} ladder step(s) {:?}",
            report.total_steps,
            report
                .steps
                .iter()
                .map(|s| format!("{}:{}", s.model_label, s.status))
                .collect::<Vec<_>>()
        )
    });
    assert!(
        request.starts_with("GET /api/models"),
        "the probe asked the test server for {request:?}"
    );
    assert!(report.total_steps > 0);
    assert_eq!(report.steps.len(), report.total_steps);
    assert!(report.network_status.contains("Connected: true"));

    let audit = ModelManager::run_downloads_expert_audit(&dir);
    assert!(audit.contains("=== SUSI MODEL DOWNLOADING EXPERT AUDIT ==="));
    assert!(audit.contains("[HARDWARE STATS]:"));
    assert!(audit.contains("[NETWORK STATS]:"));
    let _ = fs::remove_dir_all(&dir);
}
