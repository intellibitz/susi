//! Filesystem discovery and verification of local model artifacts.

use crate::susi_error::EaiResult;
use crate::susi_sandbox::manager::DynamicModelInfo;
use std::fs;
use std::path::{Path, PathBuf};

use super::types::*;
use super::{ModelManager, ModelScanRules, MODEL_SCAN_GENERATION};

// Keep traversal policies separate (bounded inventory vs deep home scan), but
// share cycle/exclusion admission and the model-file predicate.
fn visit_model_directory(
    dir: &Path,
    visited: &mut std::collections::HashSet<PathBuf>,
    rules: &ModelScanRules,
) -> bool {
    if dir.canonicalize().is_ok_and(|path| !visited.insert(path)) {
        return false;
    }
    let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
    !rules.exclude_dirs.iter().any(|excluded| excluded == name)
}

fn model_extension(path: &Path, rules: &ModelScanRules) -> Option<String> {
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    // Membership, not suffix alone, keeps generic binary caches out.
    (rules.extensions.iter().any(|e| e == &extension)
        && path.metadata().map(|m| m.len()).unwrap_or(0) > rules.min_bytes)
        .then_some(extension)
}

impl ModelManager {
    pub fn verify_local_models(workspace: &Path) -> Vec<ModelVerificationResult> {
        let models = Self::list_models(workspace);
        let managed_minimum_bytes: std::collections::HashMap<String, u64> =
            crate::hf_discovery::resolve_model_ladder(
                &crate::susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default(),
            )
            .into_iter()
            .map(|step| (step.hf_file, step.min_bytes))
            .collect();
        let mut results = Vec::new();
        for m in models {
            if m.is_local() && !Self::is_synthetic_model_id(m.model_id()) {
                let path = PathBuf::from(m.model_id());
                if path.is_file() {
                    let size_bytes = path.metadata().map(|meta| meta.len()).unwrap_or(0);
                    let mut magic_header = String::new();
                    if let Ok(mut file) = fs::File::open(&path) {
                        use std::io::Read;
                        let mut header = [0u8; 4];
                        if file.read_exact(&mut header).is_ok() {
                            magic_header = String::from_utf8_lossy(&header).into_owned();
                        }
                    }
                    let has_valid_gguf_header = magic_header == "GGUF";
                    let minimum_bytes = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .and_then(|name| managed_minimum_bytes.get(name))
                        .copied();
                    let is_valid_gguf =
                        Self::is_complete_gguf(has_valid_gguf_header, size_bytes, minimum_bytes)
                            && Self::valid_gguf_payload(&path);

                    // No reference checksum means unverified, not verified.
                    let reference = m.checksum();
                    let verified = reference.is_some_and(|c| {
                        Self::calculate_simple_checksum(&path)
                            .is_ok_and(|actual| c == actual.as_str())
                    });

                    results.push(ModelVerificationResult {
                        model_id: m.name().to_string(),
                        path: m.model_id().to_string(),
                        file_size_bytes: size_bytes,
                        file_size_formatted: format!(
                            "{:.2} GB",
                            size_bytes as f32 / 1_000_000_000.0
                        ),
                        is_valid_gguf,
                        magic_header,
                        test_inference_status: "NOT_RUN".into(),
                        latency_ms: 0,
                        checksum_known: reference.is_some(),
                        checksum_verified: verified,
                    });
                }
            }
        }
        results
    }

    pub(crate) fn calculate_simple_checksum(path: &Path) -> EaiResult<String> {
        use sha2::{Digest, Sha256};
        use std::io::Read;
        let mut file = fs::File::open(path)?;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 8192];
        loop {
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
        }
        Ok(hex::encode(hasher.finalize()))
    }

    #[allow(clippy::type_complexity)] // one-off function-local cache tuple; a named type would add no clarity
    pub fn scan_system_for_local_models(workspace: &Path) -> Vec<DynamicModelInfo> {
        static MODEL_SCAN_CACHE: std::sync::LazyLock<
            std::sync::RwLock<
                Option<(
                    PathBuf,
                    u64,
                    Option<std::time::SystemTime>,
                    std::time::Instant,
                    Vec<DynamicModelInfo>,
                )>,
            >,
        > = std::sync::LazyLock::new(|| std::sync::RwLock::new(None));

        // Another process (the daemon, a second shell) can publish a model
        // into the shared directory; that bumps no in-process generation,
        // but the rename updates the directory's modification time.
        let models_dir = Self::get_models_dir();
        let models_stamp = fs::metadata(&models_dir)
            .and_then(|meta| meta.modified())
            .ok();
        {
            let cache = MODEL_SCAN_CACHE.read().unwrap_or_else(|e| e.into_inner());
            if let Some((ref cached_workspace, generation, stamp, ts, ref list)) = *cache {
                if cached_workspace == workspace
                    && generation
                        == MODEL_SCAN_GENERATION.load(std::sync::atomic::Ordering::Acquire)
                    && stamp == models_stamp
                    && ts.elapsed().as_secs() < 60
                {
                    return list.clone();
                }
            }
        }

        let mut discovered = Vec::new();
        let mut visited = std::collections::HashSet::new();

        // Loaded once per scan (cached 60s below) rather than per recursive
        // call — a deep filesystem walk can hit this hundreds of times, and
        // each SusiConfig::load_global() is a file read + JSON parse.
        let cfg = crate::susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
        let rules = ModelScanRules {
            exclude_dirs: cfg.model_scan_exclude_dirs(),
            extensions: cfg.model_file_extensions(),
            min_bytes: cfg.model_file_min_bytes(),
        };

        if workspace.is_dir() {
            Self::recursive_scan_model_dir(workspace, &mut discovered, &mut visited, 0, &rules);
        }
        if models_dir.is_dir() && models_dir != workspace {
            Self::recursive_scan_model_dir(&models_dir, &mut discovered, &mut visited, 0, &rules);
        }

        if !cfg!(test) {
            for path_str in cfg.local_scan_paths() {
                let p = PathBuf::from(path_str);
                if p.is_dir() {
                    Self::recursive_scan_model_dir(&p, &mut discovered, &mut visited, 0, &rules);
                }
            }
        }
        discovered.sort_by(|a, b| a.model_id().cmp(b.model_id()));
        discovered.dedup_by(|a, b| a.model_id() == b.model_id());

        {
            let mut cache = MODEL_SCAN_CACHE.write().unwrap_or_else(|e| e.into_inner());
            *cache = Some((
                workspace.to_path_buf(),
                MODEL_SCAN_GENERATION.load(std::sync::atomic::Ordering::Acquire),
                models_stamp,
                std::time::Instant::now(),
                discovered.clone(),
            ));
        }

        discovered
    }

    pub(crate) fn recursive_scan_model_dir(
        dir: &Path,
        discovered: &mut Vec<DynamicModelInfo>,
        visited: &mut std::collections::HashSet<PathBuf>,
        depth: usize,
        rules: &ModelScanRules,
    ) {
        if depth > 5 {
            return;
        }
        if !visit_model_directory(dir, visited, rules) {
            return;
        }

        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    Self::recursive_scan_model_dir(&path, discovered, visited, depth + 1, rules);
                } else if path.is_file() {
                    if let Some(lower_ext) = model_extension(&path, rules) {
                        let file_name =
                            path.file_name().and_then(|n| n.to_str()).unwrap_or("model");
                        let checksum = None;
                        let prov_file = path.with_extension("provenance.json");
                        let provenance = if prov_file.exists() {
                            fs::read_to_string(&prov_file)
                                .ok()
                                .and_then(|s| serde_json::from_str(&s).ok())
                        } else {
                            None
                        };

                        discovered.push(DynamicModelInfo::new(
                            file_name.to_string(),
                            format!("Local {} Substrate", lower_ext.to_uppercase()),
                            path.to_string_lossy().to_string(),
                            format!("Universal Weights ({})", lower_ext.to_uppercase()),
                            true,
                            "Specialist".to_string(),
                            None,
                            "LocalGGUF".to_string(),
                            checksum,
                            provenance,
                        ));
                    }
                }
            }
        }
    }

    pub fn deep_scan_home_and_register(global_dir: &Path) -> EaiResult<String> {
        let home = susi_paths::SusiDirs::home_dir();
        if !home.is_dir() {
            return Err(crate::susi_error::EaiError::filesystem(
                "User home directory not detected",
            ));
        }

        let mut cfg = crate::susi_sandbox::manager::SusiConfig::load(global_dir)
            .map_err(|e| crate::susi_error::EaiError::config(e.to_string()))?;
        let home_scan_root_exclude_dirs = cfg.home_scan_root_exclude_dirs();
        let rules = std::sync::Arc::new(ModelScanRules {
            exclude_dirs: cfg.model_discovery_exclude_dirs(),
            extensions: cfg.model_file_extensions(),
            min_bytes: cfg.model_file_min_bytes(),
        });

        let mut sub_paths = Vec::new();
        if let Ok(entries) = fs::read_dir(&home) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if path.is_dir()
                    && !name.starts_with('.')
                    && !home_scan_root_exclude_dirs
                        .iter()
                        .any(|excluded| excluded == name)
                {
                    sub_paths.push(path);
                }
            }
        }
        let hidden_folders = [".android", ".cache", ".local", "Downloads"];
        for h in hidden_folders {
            let p = home.join(h);
            if p.is_dir() {
                sub_paths.push(p);
            }
        }

        sub_paths.sort();
        sub_paths.dedup();

        let found_folders =
            std::sync::Arc::new(std::sync::RwLock::new(std::collections::HashSet::new()));
        let mut handles = Vec::new();

        for sub_path in sub_paths {
            let ff = std::sync::Arc::clone(&found_folders);
            let rules = std::sync::Arc::clone(&rules);
            handles.push(std::thread::spawn(move || {
                let mut local_discovered = Vec::new();
                let mut local_visited = std::collections::HashSet::new();
                Self::recursive_scan_model_dir_for_paths(
                    &sub_path,
                    &mut local_discovered,
                    &mut local_visited,
                    &rules,
                );
                if !local_discovered.is_empty() {
                    let mut lock = ff.write().unwrap_or_else(|e| e.into_inner());
                    for p in local_discovered {
                        lock.insert(p);
                    }
                }
            }));
        }

        for h in handles {
            let _ = h.join();
        }

        let mut new_paths_added = 0;

        let mut paths = cfg.local_scan_paths();
        let lock = found_folders.read().unwrap_or_else(|e| e.into_inner());
        for folder in lock.iter() {
            if !paths.contains(folder) {
                paths.push(folder.clone());
                new_paths_added += 1;
            }
        }
        cfg.settings
            .insert("local_scan_paths".to_string(), serde_json::json!(paths));

        cfg.save(global_dir)
            .map_err(|e| crate::susi_error::EaiError::config(e.to_string()))?;

        Ok(format!("Deep scan complete. Discovered and registered {} new local model directories to substrate configuration.", new_paths_added))
    }

    fn recursive_scan_model_dir_for_paths(
        dir: &Path,
        discovered_folders: &mut Vec<String>,
        visited: &mut std::collections::HashSet<PathBuf>,
        rules: &ModelScanRules,
    ) {
        if !visit_model_directory(dir, visited, rules) {
            return;
        }

        if let Ok(entries) = fs::read_dir(dir) {
            let mut folder_has_model = false;
            let mut sub_dirs = Vec::new();
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    sub_dirs.push(path);
                } else if path.is_file() && model_extension(&path, rules).is_some() {
                    folder_has_model = true;
                }
            }

            if folder_has_model {
                discovered_folders.push(dir.to_string_lossy().to_string());
            }

            for sd in sub_dirs {
                Self::recursive_scan_model_dir_for_paths(&sd, discovered_folders, visited, rules);
            }
        }
    }
}

#[cfg(test)]
// Test fixture setup must fail loudly; production retains denied panic-path lints.
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn model_published_by_another_process_is_seen_before_the_cache_expires() {
        let workspace =
            std::env::temp_dir().join(format!("susi-scan-fresh-workspace-{}", std::process::id()));
        fs::create_dir_all(&workspace).unwrap();
        let model =
            ModelManager::get_models_dir().join(format!("scan-fresh-{}.gguf", std::process::id()));
        let _ = fs::remove_file(&model);
        let listed = |path: &Path| {
            ModelManager::scan_system_for_local_models(&workspace)
                .iter()
                .any(|m| Path::new(m.model_id()) == path)
        };
        assert!(!listed(&model));
        // A daemon's download lands without bumping this process's
        // generation; only the shared directory changes.
        fs::write(&model, vec![0u8; 1_000_001]).unwrap();
        let seen = listed(&model);
        let _ = fs::remove_file(&model);
        assert!(seen, "a model published by another process stayed hidden");
    }

    #[test]
    fn calculate_simple_checksum_returns_expected_sha256_and_errors_on_missing_path() {
        let root = std::env::temp_dir().join(format!(
            "susi-scan-checksum-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let file_path = root.join("fixture.gguf");
        fs::write(&file_path, b"abc").unwrap();

        let digest = ModelManager::calculate_simple_checksum(&file_path).unwrap();
        assert_eq!(
            digest,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );

        let missing = root.join("does-not-exist.gguf");
        assert!(ModelManager::calculate_simple_checksum(&missing).is_err());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn recursive_scan_model_dir_enforces_depth_exclusions_extensions_min_bytes_and_provenance_sidecar(
    ) {
        let root = std::env::temp_dir().join(format!(
            "susi-scan-rules-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();

        let rules = ModelScanRules {
            exclude_dirs: vec!["node_modules".to_string(), "target".to_string()],
            extensions: vec!["gguf".to_string(), "safetensors".to_string()],
            min_bytes: 1_000,
        };

        // 1. Valid .gguf (> min_bytes) with companion .provenance.json sidecar
        let valid_gguf = root.join("valid.gguf");
        fs::write(&valid_gguf, vec![0x47u8; 1_001]).unwrap();
        let prov = ModelProvenance {
            source_url: "https://huggingface.co/Qwen/Qwen3-0.6B-GGUF".to_string(),
            timestamp: 1_730_000_000,
            original_checksum: Some("sha256:cafe1234".to_string()),
        };
        fs::write(
            root.join("valid.provenance.json"),
            serde_json::to_string(&prov).unwrap(),
        )
        .unwrap();

        // 2. Valid .safetensors (> min_bytes) without sidecar
        fs::write(root.join("weights.safetensors"), vec![1u8; 1_500]).unwrap();

        // 3. Excluded by min_bytes (== min_bytes, not strictly greater)
        fs::write(root.join("too_small.gguf"), vec![2u8; 1_000]).unwrap();

        // 4. Excluded by extension (.bin is not in rules.extensions)
        fs::write(root.join("shader_cache.bin"), vec![3u8; 5_000]).unwrap();

        // 5. Excluded directories (node_modules, target)
        let nm_dir = root.join("node_modules");
        fs::create_dir_all(&nm_dir).unwrap();
        fs::write(nm_dir.join("ignored_nm.gguf"), vec![4u8; 2_000]).unwrap();

        let target_dir = root.join("target");
        fs::create_dir_all(&target_dir).unwrap();
        fs::write(target_dir.join("ignored_target.gguf"), vec![5u8; 2_000]).unwrap();

        // 6. Depth boundary: depth 5 is discovered, depth 6 is ignored
        let depth5_dir = root.join("d1").join("d2").join("d3").join("d4").join("d5");
        let depth6_dir = depth5_dir.join("d6");
        fs::create_dir_all(&depth6_dir).unwrap();
        fs::write(depth5_dir.join("depth5.gguf"), vec![6u8; 2_000]).unwrap();
        fs::write(depth6_dir.join("depth6.gguf"), vec![7u8; 2_000]).unwrap();

        let mut discovered = Vec::new();
        let mut visited = std::collections::HashSet::new();
        ModelManager::recursive_scan_model_dir(&root, &mut discovered, &mut visited, 0, &rules);

        let mut names: Vec<String> = discovered.iter().map(|m| m.name().to_string()).collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "depth5.gguf".to_string(),
                "valid.gguf".to_string(),
                "weights.safetensors".to_string()
            ]
        );

        let valid_entry = discovered
            .iter()
            .find(|m| m.name() == "valid.gguf")
            .unwrap();
        let loaded_prov: ModelProvenance =
            serde_json::from_value(valid_entry.provenance().unwrap().clone()).unwrap();
        assert_eq!(loaded_prov.source_url, prov.source_url);
        assert_eq!(loaded_prov.timestamp, prov.timestamp);
        assert_eq!(loaded_prov.original_checksum, prov.original_checksum);

        let safetensors_entry = discovered
            .iter()
            .find(|m| m.name() == "weights.safetensors")
            .unwrap();
        assert!(safetensors_entry.provenance().is_none());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn recursive_scan_model_dir_for_paths_discovers_qualifying_folders_and_ignores_bin_or_small_files(
    ) {
        let root = std::env::temp_dir().join(format!(
            "susi-scan-paths-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();

        let rules = ModelScanRules {
            exclude_dirs: vec!["node_modules".to_string()],
            extensions: vec!["gguf".to_string(), "safetensors".to_string()],
            min_bytes: 500,
        };

        let gguf_dir = root.join("gguf_hub");
        fs::create_dir_all(&gguf_dir).unwrap();
        fs::write(gguf_dir.join("model.gguf"), vec![1u8; 600]).unwrap();

        let safe_dir = root.join("nested").join("safetensors_hub");
        fs::create_dir_all(&safe_dir).unwrap();
        fs::write(safe_dir.join("weights.safetensors"), vec![2u8; 700]).unwrap();

        let ignored_dir = root.join("only_bin_and_small");
        fs::create_dir_all(&ignored_dir).unwrap();
        fs::write(ignored_dir.join("cache.bin"), vec![3u8; 5_000]).unwrap();
        fs::write(ignored_dir.join("tiny.gguf"), vec![4u8; 500]).unwrap();

        let excluded_dir = root.join("node_modules");
        fs::create_dir_all(&excluded_dir).unwrap();
        fs::write(excluded_dir.join("skipped.gguf"), vec![5u8; 1_000]).unwrap();

        let mut discovered_folders = Vec::new();
        let mut visited = std::collections::HashSet::new();
        ModelManager::recursive_scan_model_dir_for_paths(
            &root,
            &mut discovered_folders,
            &mut visited,
            &rules,
        );

        discovered_folders.sort();
        let mut expected = vec![
            gguf_dir.to_string_lossy().to_string(),
            safe_dir.to_string_lossy().to_string(),
        ];
        expected.sort();
        assert_eq!(discovered_folders, expected);

        // Re-scanning with the same `visited` set must not duplicate entries.
        ModelManager::recursive_scan_model_dir_for_paths(
            &root,
            &mut discovered_folders,
            &mut visited,
            &rules,
        );
        assert_eq!(discovered_folders.len(), 2);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn recursive_scan_model_dir_skips_already_visited_canonical_directories_and_non_matching_files()
    {
        let root = std::env::temp_dir().join(format!(
            "susi-scan-visited-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("candidate.gguf"), vec![0x47u8; 2_000]).unwrap();

        let rules = ModelScanRules {
            exclude_dirs: Vec::new(),
            extensions: vec!["gguf".to_string()],
            min_bytes: 500,
        };
        let mut discovered = Vec::new();
        let mut visited = std::collections::HashSet::new();
        visited.insert(root.canonicalize().unwrap());

        ModelManager::recursive_scan_model_dir(&root, &mut discovered, &mut visited, 0, &rules);
        assert!(
            discovered.is_empty(),
            "already-visited canonical directory must not be rescanned"
        );
        let _ = fs::remove_dir_all(&root);
    }
}
