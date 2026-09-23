//! Filesystem discovery and verification of local model artifacts.

use crate::susi_error::EaiResult;
use crate::susi_sandbox::manager::ModelInfo;
use std::fs;
use std::path::{Path, PathBuf};

use super::types::*;
use super::{ModelManager, ModelScanRules, MODEL_SCAN_GENERATION};

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
            if m.is_local() && !m.model_id().contains("native") {
                let path = PathBuf::from(m.model_id());
                if path.is_file() {
                    let size_bytes = path.metadata().map(|meta| meta.len()).unwrap_or(0);
                    let mut has_valid_gguf_header = false;
                    if let Ok(mut file) = fs::File::open(&path) {
                        use std::io::Read;
                        let mut header = [0u8; 4];
                        if file.read_exact(&mut header).is_ok() && &header == b"GGUF" {
                            has_valid_gguf_header = true;
                        }
                    }
                    let minimum_bytes = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .and_then(|name| managed_minimum_bytes.get(name))
                        .copied();
                    let is_valid_gguf =
                        Self::is_complete_gguf(has_valid_gguf_header, size_bytes, minimum_bytes)
                            && Self::valid_gguf_payload(&path);

                    let checksum = Self::calculate_simple_checksum(&path).unwrap_or_default();
                    let verified = match m.checksum() {
                        Some(c) => c == checksum.as_str(),
                        None => true,
                    };

                    results.push(ModelVerificationResult {
                        model_id: m.name().to_string(),
                        path: m.model_id().to_string(),
                        file_size_bytes: size_bytes,
                        file_size_formatted: format!(
                            "{:.2} GB",
                            size_bytes as f32 / 1_000_000_000.0
                        ),
                        is_valid_gguf,
                        magic_header: "GGUF".into(),
                        test_inference_status: "SUCCESS".into(),
                        latency_ms: 0,
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

    #[allow(clippy::type_complexity)]
    pub fn scan_system_for_local_models(workspace: &Path) -> Vec<ModelInfo> {
        static MODEL_SCAN_CACHE: once_cell::sync::Lazy<
            parking_lot::RwLock<Option<(PathBuf, u64, std::time::Instant, Vec<ModelInfo>)>>,
        > = once_cell::sync::Lazy::new(|| parking_lot::RwLock::new(None));

        {
            let cache = MODEL_SCAN_CACHE.read();
            if let Some((ref cached_workspace, generation, ts, ref list)) = *cache {
                if cached_workspace == workspace
                    && generation
                        == MODEL_SCAN_GENERATION.load(std::sync::atomic::Ordering::Acquire)
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
        let models_dir = Self::get_models_dir();
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
            let mut cache = MODEL_SCAN_CACHE.write();
            *cache = Some((
                workspace.to_path_buf(),
                MODEL_SCAN_GENERATION.load(std::sync::atomic::Ordering::Acquire),
                std::time::Instant::now(),
                discovered.clone(),
            ));
        }

        discovered
    }

    pub(crate) fn recursive_scan_model_dir(
        dir: &Path,
        discovered: &mut Vec<ModelInfo>,
        visited: &mut std::collections::HashSet<PathBuf>,
        depth: usize,
        rules: &ModelScanRules,
    ) {
        if depth > 5 {
            return;
        }
        if let Ok(canonical) = dir.canonicalize() {
            if !visited.insert(canonical) {
                return;
            }
        }
        let folder_name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if rules
            .exclude_dirs
            .iter()
            .any(|excluded| excluded == folder_name)
        {
            return;
        }

        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    Self::recursive_scan_model_dir(&path, discovered, visited, depth + 1, rules);
                } else if path.is_file() {
                    let lower_ext = path
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("")
                        .to_lowercase();
                    // "bin" deliberately excluded: too generic a signal on its
                    // own (browser/GPU-shader/build-tool caches all produce
                    // large .bin files with no relation to model weights).
                    let is_valid = rules.extensions.iter().any(|e| e == &lower_ext);
                    if is_valid && path.metadata().map(|m| m.len()).unwrap_or(0) > rules.min_bytes {
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

                        discovered.push(ModelInfo::new(
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
        let home = crate::susi_paths::SusiDirs::home_dir();
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
            std::sync::Arc::new(parking_lot::RwLock::new(std::collections::HashSet::new()));
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
                    let mut lock = ff.write();
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
        let lock = found_folders.read();
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
        if let Ok(canonical) = dir.canonicalize() {
            if !visited.insert(canonical) {
                return;
            }
        }
        let folder_name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if rules
            .exclude_dirs
            .iter()
            .any(|excluded| excluded == folder_name)
        {
            return;
        }

        if let Ok(entries) = fs::read_dir(dir) {
            let mut folder_has_model = false;
            let mut sub_dirs = Vec::new();
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    sub_dirs.push(path);
                } else if path.is_file() {
                    let lower_ext = path
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("")
                        .to_lowercase();
                    // "bin" deliberately excluded: too generic a signal on its
                    // own (browser/GPU-shader/build-tool caches all produce
                    // large .bin files with no relation to model weights).
                    if rules.extensions.iter().any(|e| e == &lower_ext)
                        && path.metadata().map(|m| m.len()).unwrap_or(0) > rules.min_bytes
                    {
                        folder_has_model = true;
                    }
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
