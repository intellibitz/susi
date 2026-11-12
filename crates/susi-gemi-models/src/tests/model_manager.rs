use super::*;
use crate::susi_core::commit_log::ENV_LOCK;

// Configuration, telemetry and task workers may retain a resolved home.
// Run the real lock tests with a private home from process creation, then
// let the parent clean up only after the fixture process has exited.
fn private_download_fixture(name: &str) -> Option<PathBuf> {
    const CHILD: &str = "SUSI_MANAGED_DOWNLOAD_TEST_CHILD";
    let cwd = std::env::current_dir().unwrap();
    if std::env::var(CHILD).is_ok_and(|value| value == name)
        && fs::read_to_string(cwd.join(".download-fixture")).is_ok_and(|value| value == name)
    {
        assert_eq!(
            PathBuf::from(std::env::var_os("SUSI_HOME").unwrap())
                .canonicalize()
                .unwrap(),
            cwd.canonicalize().unwrap()
        );
        return Some(cwd);
    }
    let root = std::env::temp_dir().join(format!(
        "susi-download-fixture-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    fs::write(root.join(".download-fixture"), name).unwrap();
    let (_, module) = module_path!().split_once("::").unwrap();
    let output = susi_paths::test_env::isolate_home(
        &mut std::process::Command::new(std::env::current_exe().unwrap()),
        &root,
    )
    .env(CHILD, name)
    .env("SUSI_HOME", &root)
    .current_dir(&root)
    .args(["--exact", &format!("{module}::{name}"), "--test-threads=1"])
    .output()
    .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed; 0 failed"),
        "isolated fixture {} failed: {stdout}\n{}",
        root.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(root).unwrap();
    None
}

#[test]
fn managed_download_waits_through_forward_progress_but_times_out_a_stalled_publisher() {
    let _environment = crate::susi_core::commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    use std::time::Duration;

    let Some(root) = private_download_fixture(
        "managed_download_waits_through_forward_progress_but_times_out_a_stalled_publisher",
    ) else {
        return;
    };
    let lock_path = root.join("transaction.lock");
    let holder = fs::File::create(&lock_path).unwrap();
    holder.lock().unwrap();
    let waiter = fs::OpenOptions::new().write(true).open(&lock_path).unwrap();
    let staged = root.join("model");
    let part = root.join("model.part");
    let task = crate::susi_core::task_manager::SwarmTaskManager::global()
        .register_task("managed_download_wait", "model");
    let idle = Duration::from_millis(200);
    let started = std::time::Instant::now();
    std::thread::scope(|threads| {
        let owner = threads.spawn(move || {
            let data = fs::File::create(part).unwrap();
            for size in 1..=5 {
                std::thread::sleep(Duration::from_millis(60));
                data.set_len(size).unwrap();
            }
            drop(holder);
        });
        ModelManager::wait_for_download_transaction(&waiter, &staged, &task, idle).unwrap();
        owner.join().unwrap();
    });
    assert!(
        started.elapsed() > idle,
        "forward progress must extend the initial idle deadline"
    );
    // The same occupied lock now makes no progress on this prefix.
    let stalled = fs::OpenOptions::new().write(true).open(&lock_path).unwrap();
    let stalled_started = std::time::Instant::now();
    let error =
        ModelManager::wait_for_download_transaction(&stalled, &staged, &task, idle).unwrap_err();
    assert!(
        stalled_started.elapsed() >= idle,
        "a stalled worker must retain its full idle budget"
    );
    assert!(error.contains("Timed out waiting"));
    drop(waiter);
    ModelManager::wait_for_download_transaction(&stalled, &staged, &task, idle).unwrap();
    drop(stalled);
}

#[test]
fn managed_download_waits_for_publication_and_reuses_verified_artifact() {
    let _environment = crate::susi_core::commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let Some(root) = private_download_fixture(
        "managed_download_waits_for_publication_and_reuses_verified_artifact",
    ) else {
        return;
    };
    let models = root.join("models");
    let store = ModelManager::transactional_store(&models);
    let name = "shared.bin";
    let url = "https://models.invalid/shared.bin";
    let staged = store.staging_target(name);
    fs::create_dir_all(staged.parent().unwrap()).unwrap();
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(staged.with_extension("transaction.lock"))
        .unwrap();
    lock.lock().unwrap();
    let worker_models = models.clone();
    let (done, result) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let task = crate::susi_core::task_manager::SwarmTaskManager::global()
            .register_task("managed_download_test", url);
        done.send(ModelManager::execute_download_stream_in(
            &DownloadSource {
                requested: url,
                resolved: url,
                publisher_sha256: None,
            },
            &task,
            &worker_models,
            |_| Ok(()),
            std::thread::sleep,
        ))
        .unwrap();
    });
    assert!(result
        .recv_timeout(std::time::Duration::from_millis(100))
        .is_err());
    let payload = b"verified shared payload";
    fs::write(&staged, payload).unwrap();
    let checksum = crate::resumable_downloads::sha256_hex(payload);
    ModelManager::publish_download(&models, name, url, Some(&checksum), &|p| p.is_file()).unwrap();
    drop(lock);
    result
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap()
        .unwrap();
    worker.join().unwrap();
    assert_eq!(fs::read(models.join(name)).unwrap(), payload);
    assert!(!staged.exists());
    assert!(!ModelManager::managed_download_is_ready(
        &models,
        name,
        "https://other.invalid/shared.bin"
    ));
    fs::write(models.join(name), b"tampered shared payload").unwrap();
    assert!(!ModelManager::managed_download_is_ready(&models, name, url));
}

#[test]
fn vc_201_044_download_publish_called_by_production() {
    let _environment = crate::susi_core::commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let root =
        std::env::temp_dir().join(format!("susi-vc-201-044-production-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let models_dir = root.join("models");
    fs::create_dir_all(&models_dir).unwrap();

    let file_name = "production-model.bin";
    let payload = b"streamed model payload";
    let store = ModelManager::transactional_store(&models_dir);
    let staged = store.staging_target(file_name);
    fs::create_dir_all(staged.parent().unwrap()).unwrap();
    fs::write(&staged, payload).unwrap();
    let checksum = crate::resumable_downloads::sha256_hex(payload);

    let final_path = ModelManager::publish_download(
        &models_dir,
        file_name,
        "https://models.example.test/production-model.bin",
        Some(&checksum),
        &|path| path.metadata().is_ok_and(|metadata| metadata.len() > 0),
    )
    .unwrap();

    assert_eq!(final_path, models_dir.join(file_name));
    assert_eq!(fs::read(&final_path).unwrap(), payload);
    assert!(store.is_ready(file_name));
    assert!(!staged.exists());
    assert!(store.recover().is_empty());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn starter_download_is_small_even_with_large_gpu() {
    let _environment = crate::susi_core::commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let host = crate::zc_first_model::FirstRunHost {
        ram: 128_000_000_000,
        vram: 48_000_000_000,
        disk: 100_000_000_000,
        offline: false,
    };
    match ModelManager::starter_download_plan(&host, Path::new("models")) {
        crate::zc_first_model::FirstRunPlan::Fetch { model, plan } => {
            assert_eq!(model.hf_file, "qwen2.5-0.5b-instruct-q4_0.gguf");
            assert_eq!(model.size_bytes, 428_730_208);
            assert!(plan.sha256.is_some());
        }
        other => panic!("unexpected first-run plan {other:?}"),
    }
    let low_memory = crate::zc_first_model::FirstRunHost {
        ram: 2_000_000_000,
        vram: 0,
        ..host
    };
    assert!(matches!(
        ModelManager::starter_download_plan(&low_memory, Path::new("models")),
        crate::zc_first_model::FirstRunPlan::Fetch { .. }
    ));
}
#[test]
fn test_failed_model_recovers_after_success() {
    let _environment = crate::susi_core::commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let name = "test_failure_cooldown_model";
    ModelManager::record_inference_result(name, false);
    assert!(ModelManager::cooling_down(name));
    ModelManager::record_inference_result(name, true);
    assert!(!ModelManager::cooling_down(name));
}

#[test]
fn test_complete_gguf_requires_the_managed_artifact_size() {
    let _environment = crate::susi_core::commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(ModelManager::is_complete_gguf(true, 10_000, Some(10_000)));
    assert!(
        !ModelManager::is_complete_gguf(true, 9_999, Some(10_000)),
        "a truncated managed GGUF must not be considered usable solely from its header"
    );
    assert!(!ModelManager::is_complete_gguf(false, 10_000, Some(10_000)));
}

#[test]
fn test_header_only_gguf_is_not_complete() {
    let _environment = crate::susi_core::commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let path = std::env::temp_dir().join(format!("susi_header_only_{}.gguf", std::process::id()));
    fs::write(&path, b"GGUF").unwrap();
    assert!(!ModelManager::is_complete_model_file(&path, 0));
    fs::remove_file(path).unwrap();
}

/// A one-tensor GGUF (version 3, four f32 values), whole.
fn one_tensor_gguf() -> Vec<u8> {
    let mut bytes = b"GGUF".to_vec();
    bytes.extend(3u32.to_le_bytes());
    bytes.extend(1u64.to_le_bytes()); // tensors
    bytes.extend(1u64.to_le_bytes()); // metadata
    bytes.extend(23u64.to_le_bytes());
    bytes.extend(b"tokenizer.chat_template");
    bytes.extend(8u32.to_le_bytes()); // string
    bytes.extend(14u64.to_le_bytes());
    bytes.extend(b"{% if tools %}");
    bytes.extend(1u64.to_le_bytes()); // name length
    bytes.push(b'x');
    bytes.extend(1u32.to_le_bytes()); // dimensions
    bytes.extend(4u64.to_le_bytes()); // four f32 values
    bytes.extend(0u32.to_le_bytes()); // f32
    bytes.extend(0u64.to_le_bytes()); // tensor offset
    let aligned = bytes.len().div_ceil(32) * 32;
    bytes.resize(aligned + 16, 0); // alignment, then the tensor
    bytes
}

#[test]
fn the_positioned_reader_tracks_reads_and_seeks() {
    let _environment = crate::susi_core::commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    use std::io::{Read, Seek, SeekFrom};
    let data: Vec<u8> = (0..=255).collect();
    let mut reader = PositionedReader::new(std::io::Cursor::new(data)).unwrap();
    let mut two = [0u8; 2];
    reader.read_exact(&mut two).unwrap();
    assert_eq!((two, reader.stream_position().unwrap()), ([0, 1], 2));
    assert_eq!(reader.seek(SeekFrom::End(0)).unwrap(), 256);
    assert_eq!(reader.seek(SeekFrom::Start(10)).unwrap(), 10);
    reader.read_exact(&mut two).unwrap();
    assert_eq!((two, reader.stream_position().unwrap()), ([10, 11], 12));
    assert_eq!(reader.seek(SeekFrom::Current(-2)).unwrap(), 10);
    reader.read_exact(&mut two).unwrap();
    assert_eq!(two, [10, 11]);
}

/// A file whose bytes are not a whole GGUF gets a verdict; a read an
/// I/O error ended (here: the path is a directory) gets none, so it is
/// never remembered.
#[test]
fn a_gguf_read_error_is_told_apart_from_a_bad_file() {
    let _environment = crate::susi_core::commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!(
        "susi_gguf_read_error_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    assert_eq!(ModelManager::judge_gguf_payload(&dir), None);
    assert_eq!(ModelManager::judge_gguf_template_supports_tools(&dir), None);
    let truncated = dir.join("truncated.gguf");
    let whole = one_tensor_gguf();
    fs::write(&truncated, &whole[..30]).unwrap();
    assert_eq!(ModelManager::judge_gguf_payload(&truncated), Some(false));
    let garbage = dir.join("garbage.gguf");
    fs::write(&garbage, vec![7u8; whole.len()]).unwrap();
    assert_eq!(ModelManager::judge_gguf_payload(&garbage), Some(false));
    assert_eq!(
        ModelManager::judge_gguf_template_supports_tools(&garbage),
        Some(false)
    );
    let _ = fs::remove_dir_all(dir);
}

/// Regression for the auto step-up/step-down feature: a Simple prompt
/// must make a *smaller* model score higher, not lower.
#[test]
fn test_size_preference_score_simple_favors_smaller_models() {
    let _environment = crate::susi_core::commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    use crate::intent::TaskComplexity;
    let small_score =
        ModelManager::size_preference_score(1.5, 1.5, 40.0, Some(TaskComplexity::Simple));
    let big_score =
        ModelManager::size_preference_score(40.0, 1.5, 40.0, Some(TaskComplexity::Simple));
    assert!(
        small_score > big_score,
        "smaller model must score higher for Simple: small={small_score}, big={big_score}"
    );
}

#[test]
fn test_size_preference_score_complex_favors_bigger_models() {
    let _environment = crate::susi_core::commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    use crate::intent::TaskComplexity;
    let small_score =
        ModelManager::size_preference_score(1.5, 1.5, 40.0, Some(TaskComplexity::Complex));
    let big_score =
        ModelManager::size_preference_score(40.0, 1.5, 40.0, Some(TaskComplexity::Complex));
    assert!(
        big_score > small_score,
        "bigger model must score higher for Complex: small={small_score}, big={big_score}"
    );
}

#[test]
fn test_size_preference_score_none_matches_complex_default() {
    let _environment = crate::susi_core::commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    use crate::intent::TaskComplexity;
    assert_eq!(
        ModelManager::size_preference_score(14.0, 1.5, 40.0, None),
        ModelManager::size_preference_score(14.0, 1.5, 40.0, Some(TaskComplexity::Complex)),
        "None must preserve today's default (bigger-wins) behavior for existing callers"
    );
}

/// The actual point of the percentile redesign: with a genuine middle
/// tier present, Moderate must prefer it over BOTH extremes - not just
/// be "less extreme in one direction" the way a binary sign-flip
/// could only ever produce.
#[test]
fn test_size_preference_score_moderate_favors_middle_sized_model_over_both_extremes() {
    let _environment = crate::susi_core::commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    use crate::intent::TaskComplexity;
    let small = ModelManager::size_preference_score(1.5, 1.5, 45.0, Some(TaskComplexity::Moderate));
    let middle =
        ModelManager::size_preference_score(14.0, 1.5, 45.0, Some(TaskComplexity::Moderate));
    let big = ModelManager::size_preference_score(45.0, 1.5, 45.0, Some(TaskComplexity::Moderate));
    assert!(
        middle > small && middle > big,
        "middle-sized model must beat both extremes for Moderate: small={small}, middle={middle}, big={big}"
    );
}

#[test]
fn test_size_preference_score_falls_back_to_magnitude_when_no_size_range() {
    let _environment = crate::susi_core::commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    use crate::intent::TaskComplexity;
    // min == max: nothing to target between (e.g. only one distinct
    // size among candidates) - must not divide by zero or panic, and
    // must fall back to preferring the bigger raw size.
    let small = ModelManager::size_preference_score(5.0, 5.0, 5.0, Some(TaskComplexity::Simple));
    let same = ModelManager::size_preference_score(5.0, 5.0, 5.0, Some(TaskComplexity::Complex));
    assert!(small.is_finite() && same.is_finite());
    assert_eq!(
        small, same,
        "degenerate range must ignore complexity entirely"
    );
}

/// Regression for the escalation mechanism: a retry's floor must win
/// even when the heuristic guess on the (now longer, correction-
/// annotated) retry prompt is still low.
#[test]
fn test_resolve_complexity_floor_floor_wins_over_lower_heuristic() {
    let _environment = crate::susi_core::commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    use crate::intent::TaskComplexity;
    assert_eq!(
        ModelManager::resolve_complexity_floor(
            TaskComplexity::Trivial,
            Some(TaskComplexity::Complex)
        ),
        TaskComplexity::Complex
    );
}

#[test]
fn test_resolve_complexity_floor_heuristic_wins_when_already_above_floor() {
    let _environment = crate::susi_core::commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    use crate::intent::TaskComplexity;
    assert_eq!(
        ModelManager::resolve_complexity_floor(
            TaskComplexity::VeryComplex,
            Some(TaskComplexity::Moderate)
        ),
        TaskComplexity::VeryComplex
    );
}

#[test]
fn test_resolve_complexity_floor_none_means_pure_heuristic() {
    let _environment = crate::susi_core::commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    use crate::intent::TaskComplexity;
    assert_eq!(
        ModelManager::resolve_complexity_floor(TaskComplexity::Simple, None),
        TaskComplexity::Simple
    );
}

#[test]
fn test_universal_format_recognition() {
    let _environment = crate::susi_core::commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp_dir = std::env::temp_dir().join(format!("susi_model_test_v2-{}", std::process::id()));
    let _ = fs::create_dir_all(&tmp_dir);
    let sf_path = tmp_dir.join("test.safetensors");
    let _ = fs::write(&sf_path, vec![0u8; 2_000_000]);
    let mut discovered = Vec::new();
    let mut visited = std::collections::HashSet::new();
    let cfg = crate::susi_sandbox::manager::SusiConfig::default();
    let rules = ModelScanRules {
        exclude_dirs: cfg.model_scan_exclude_dirs(),
        extensions: cfg.model_file_extensions(),
        min_bytes: cfg.model_file_min_bytes(),
    };
    ModelManager::recursive_scan_model_dir(&tmp_dir, &mut discovered, &mut visited, 0, &rules);
    assert!(discovered
        .iter()
        .any(|m| m.model_id().contains("test.safetensors")));
    let _ = fs::remove_dir_all(&tmp_dir);
}

#[test]
fn test_downloads_expert_audit() {
    let _environment = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let mut environment = susi_paths::test_env::EnvGuard::isolated();
    environment.set("SUSI_USE_DOWNLOADS_DIR", "1");
    let tmp_dir =
        std::env::temp_dir().join(format!("susi_downloads_audit_test-{}", std::process::id()));
    let _ = fs::create_dir_all(&tmp_dir);
    let audit = ModelManager::run_downloads_expert_audit(&tmp_dir);
    assert!(audit.contains("SUSI MODEL DOWNLOADING EXPERT AUDIT"));
    assert!(audit.contains("HARDWARE STATS"));
    assert!(audit.contains("STORAGE REPOSITORY STATS"));
    assert!(audit.contains("NETWORK STATS"));
    assert!(audit.contains("BACKGROUND DOWNLOAD CONTROLLER STATS"));
    let _ = fs::remove_dir_all(&tmp_dir);
}

#[test]
fn foreground_install_rejects_non_https_sources() {
    let _environment = crate::susi_core::commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let error =
        ModelManager::install_model_foreground("http://example.test/model.gguf").unwrap_err();
    assert!(error.contains("HTTPS"));
    assert!(ModelManager::install_model_foreground("not a URL").is_err());
}

#[test]
fn supports_tool_calling_is_false_without_a_parseable_template() {
    let _environment = crate::susi_core::commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(!ModelManager::supports_tool_calling(Path::new(
        "/nonexistent/model.gguf"
    )));
}

fn fixture_download(
    url: &str,
    task: &TaskHandle,
    models: &Path,
    pin: Option<&str>,
) -> Result<(), String> {
    fixture_download_with_sleep(url, task, models, pin, std::thread::sleep)
}

fn fixture_download_with_sleep(
    url: &str,
    task: &TaskHandle,
    models: &Path,
    pin: Option<&str>,
    sleep: impl Fn(std::time::Duration),
) -> Result<(), String> {
    ModelManager::execute_download_stream_in(
        &DownloadSource {
            requested: url,
            resolved: url,
            publisher_sha256: pin,
        },
        task,
        models,
        |path| {
            if let Some(expected) = pin {
                let actual = ModelManager::calculate_simple_checksum(path)
                    .map_err(|error| error.to_string())?;
                if actual != expected {
                    return Err("Existing model differs from the admitted publisher digest".into());
                }
            }
            Ok(())
        },
        sleep,
    )
}

#[test]
fn admitted_download_uses_one_get_and_preserves_a_conflicting_final_file() {
    let _environment = crate::susi_core::commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    use crate::download::tests::{server, workspace};
    let root = workspace("admitted_weight");
    let models = root.join("models");
    let response = "HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\nabcdefghij";
    let (url, transport) = server(vec![("get /weights.bin ", response)]);
    let url = url.replace("model.gguf", "weights.bin");
    let digest = crate::resumable_downloads::sha256_hex(b"abcdefghij");
    let task = crate::susi_core::task_manager::SwarmTaskManager::global()
        .register_task("admitted_weight", &url);
    fixture_download(&url, &task, &models, Some(&digest)).unwrap();
    transport.join().unwrap();
    let final_path = models.join("weights.bin");
    assert_eq!(fs::read(&final_path).unwrap(), b"abcdefghij");
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(models.join("weights.bin.manifest.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["sha256"].as_str(), Some(digest.as_str()));
    // The server is gone. A later caller can use valid published bytes
    // locally, and must preserve conflicting bytes rather than replace them.
    let task = crate::susi_core::task_manager::SwarmTaskManager::global()
        .register_task("adopt_admitted_weight", &url);
    fixture_download(&url, &task, &models, Some(&digest)).unwrap();
    fs::write(&final_path, b"user-owned").unwrap();
    let task = crate::susi_core::task_manager::SwarmTaskManager::global()
        .register_task("reject_conflicting_weight", &url);
    let error = fixture_download(&url, &task, &models, Some(&digest)).unwrap_err();
    assert!(error.contains("admitted publisher digest"), "{error}");
    assert_eq!(fs::read(&final_path).unwrap(), b"user-owned");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn failed_staging_manifest_does_not_record_a_license_or_complete_the_task() {
    let _environment = crate::susi_core::commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    use crate::download::tests::{server, workspace};
    use crate::susi_core::task_manager::{SwarmTaskManager, TaskStatus};
    let root = workspace("failed_staging_manifest");
    let models = root.join("models");
    fs::create_dir_all(models.join("weights.bin.manifest.json")).unwrap();
    let (url, transport) = server(vec![(
        "get /weights.bin ",
        "HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\nabcdefghij",
    )]);
    let url = url.replace("model.gguf", "weights.bin");
    let digest = crate::resumable_downloads::sha256_hex(b"abcdefghij");
    let task = SwarmTaskManager::global().register_task("failed_staging_manifest", &url);
    let mut recorded = false;
    let result = ModelManager::execute_download_stream_in(
        &DownloadSource {
            requested: &url,
            resolved: &url,
            publisher_sha256: Some(&digest),
        },
        &task,
        &models,
        |_| {
            recorded = true;
            Ok(())
        },
        std::thread::sleep,
    );
    transport.join().unwrap();
    assert!(result.is_err());
    assert!(!recorded);
    let progress: ModelDownloadProgress =
        serde_json::from_slice(&fs::read(ModelManager::progress_path(&url)).unwrap()).unwrap();
    assert_eq!(progress.status, "FAILED");
    assert_eq!(
        task.status.load(std::sync::atomic::Ordering::Acquire),
        TaskStatus::Failed as u8
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn receipt_storage_failure_cannot_report_completion_even_when_reusing_published_bytes() {
    let _environment = crate::susi_core::commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    use crate::download::tests::{server, workspace};
    use crate::susi_core::task_manager::{SwarmTaskManager, TaskStatus};
    use std::sync::atomic::Ordering;
    let root = workspace("receipt_write_failure");
    let models = root.join("models");
    let sidecar = models.join("weights.license.json");
    fs::create_dir_all(&sidecar).unwrap(); // An actual receipt write cannot replace a directory.
    let response = "HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\nabcdefghij";
    let (url, transport) = server(vec![("get /weights.bin ", response)]);
    let requested = url.replace("/model.gguf", "/resolve/main/weights.bin");
    let resolved = url.replace("model.gguf", "weights.bin");
    let digest = crate::resumable_downloads::sha256_hex(b"abcdefghij");
    let progress = || -> ModelDownloadProgress {
        serde_json::from_slice(&fs::read(ModelManager::progress_path(&requested)).unwrap()).unwrap()
    };
    // The first call transfers the bytes. The second adopts them with no
    // server available; both must fail if the receipt cannot be stored.
    for should_pass in [false, false, true] {
        if should_pass {
            fs::remove_dir(&sidecar).unwrap();
        }
        let task = SwarmTaskManager::global().register_task("receipt_storage", &requested);
        let result = ModelManager::execute_download_stream_in(
            &DownloadSource {
                requested: &requested,
                resolved: &resolved,
                publisher_sha256: Some(&digest),
            },
            &task,
            &models,
            |path| {
                assert_eq!(progress().status, "RUNNING");
                assert_eq!(
                    task.status.load(Ordering::Acquire),
                    TaskStatus::Running as u8
                );
                assert_eq!(
                    ModelManager::calculate_simple_checksum(path).unwrap(),
                    digest
                );
                crate::susi_config::atomic_write_json_pretty(&sidecar, &digest)
                    .map_err(|error| error.to_string())
            },
            std::thread::sleep,
        );
        assert_eq!(result.is_ok(), should_pass);
        let (expected_progress, expected_task) = if should_pass {
            ("COMPLETED", TaskStatus::Completed)
        } else {
            ("FAILED", TaskStatus::Failed)
        };
        assert_eq!(progress().status, expected_progress);
        assert_eq!(task.status.load(Ordering::Acquire), expected_task as u8);
        assert_eq!(fs::read(models.join("weights.bin")).unwrap(), b"abcdefghij");
    }
    transport.join().unwrap();
    assert_eq!(
        serde_json::from_slice::<String>(&fs::read(sidecar).unwrap()).unwrap(),
        digest
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn admitted_download_retries_corrupt_bytes_and_unpinned_download_still_checks_head() {
    let _environment = crate::susi_core::commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    use crate::download::tests::{server, workspace};
    let good = "HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\nabcdefghij";
    let bad = "HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\nabcdefghik";
    let interrupted = "HTTP/1.1 200 OK\r\nContent-Length: 10\r\nETag: \"fixture-v1\"\r\nConnection: close\r\n\r\nabcde";
    let rest = "HTTP/1.1 206 Partial Content\r\nContent-Length: 5\r\nContent-Range: bytes 5-9/10\r\nETag: \"fixture-v1\"\r\nConnection: close\r\n\r\nfghij";
    let head = "HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\n";
    let digest = crate::resumable_downloads::sha256_hex(b"abcdefghij");
    for (name, pin, responses, expected_ticks) in [
        (
            "retry_weight",
            Some(digest.as_str()),
            vec![("get /weights.bin ", bad), ("get /weights.bin ", good)],
            20,
        ),
        (
            "resume_weight",
            Some(digest.as_str()),
            vec![
                ("get /weights.bin ", interrupted),
                ("range: bytes=5-", rest),
            ],
            10,
        ),
        (
            "unpinned_weight",
            None,
            vec![("head /weights.bin ", head), ("get /weights.bin ", good)],
            0,
        ),
    ] {
        let root = workspace(name);
        let models = root.join("models");
        let (url, transport) = server(responses);
        let url = url.replace("model.gguf", "weights.bin");
        let task =
            crate::susi_core::task_manager::SwarmTaskManager::global().register_task(name, &url);
        let ticks = std::cell::RefCell::new(Vec::new());
        fixture_download_with_sleep(&url, &task, &models, pin, |duration| {
            ticks.borrow_mut().push(duration);
        })
        .unwrap();
        assert_eq!(
            *ticks.borrow(),
            vec![std::time::Duration::from_millis(100); expected_ticks]
        );
        transport.join().unwrap();
        assert_eq!(fs::read(models.join("weights.bin")).unwrap(), b"abcdefghij");
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(models.join("weights.bin.manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["sha256"].as_str(), pin);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn cancellation_during_retry_backoff_stops_without_publishing() {
    let _environment = crate::susi_core::commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    use crate::download::tests::{server, workspace};
    use std::sync::atomic::Ordering;
    let root = workspace("cancel_retry_backoff");
    let models = root.join("models");
    let bad = "HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\nabcdefghik";
    let digest = crate::resumable_downloads::sha256_hex(b"abcdefghij");
    let (url, transport) = server(vec![("get /weights.bin ", bad)]);
    let url = url.replace("model.gguf", "weights.bin");
    let task = crate::susi_core::task_manager::SwarmTaskManager::global()
        .register_task("cancel_retry_backoff", &url);
    let ticks = std::cell::RefCell::new(Vec::new());
    let recorded = std::cell::Cell::new(false);
    let result = ModelManager::execute_download_stream_in(
        &DownloadSource {
            requested: &url,
            resolved: &url,
            publisher_sha256: Some(&digest),
        },
        &task,
        &models,
        |_| {
            recorded.set(true);
            Ok(())
        },
        |duration| {
            ticks.borrow_mut().push(duration);
            task.cancel_flag.store(true, Ordering::Release);
        },
    );
    transport.join().unwrap();
    assert_eq!(result.unwrap_err(), "Download cancelled");
    assert_eq!(*ticks.borrow(), vec![std::time::Duration::from_millis(100)]);
    assert!(!models.join("weights.bin").exists());
    assert!(!models.join("weights.bin.manifest.json").exists());
    let progress: ModelDownloadProgress =
        serde_json::from_slice(&fs::read(ModelManager::progress_path(&url)).unwrap()).unwrap();
    assert_eq!(progress.status, "STOPPED");
    assert!(
        !recorded.get(),
        "a cancelled transfer cannot record a receipt"
    );
    assert_ne!(
        task.status.load(Ordering::Acquire),
        crate::susi_core::task_manager::TaskStatus::Completed as u8
    );
    fs::remove_dir_all(root).unwrap();
}
