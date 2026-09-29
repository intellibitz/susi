//! Resumable artifact transfer. A model becomes visible only after validation.
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[derive(Default, Serialize, Deserialize)]
struct Checkpoint {
    url: String,
    validator: Option<String>,
}

pub(crate) fn artifact_name(url: &str) -> Result<String, String> {
    let parsed = url::Url::parse(url).map_err(|e| e.to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("Expected HTTP(S) artifact URL".into());
    }
    let name = parsed
        .path_segments()
        .and_then(|mut p| p.next_back())
        .unwrap_or("");
    if name.is_empty() || matches!(name, "." | "..") {
        return Err("Missing artifact filename".into());
    }
    Ok(name.to_string())
}

/// The SHA-256 a Hugging Face `X-Linked-ETag` carries (quoted 64-hex).
fn linked_etag_digest(value: &str) -> Option<String> {
    let tag = value.trim().trim_matches('"').to_ascii_lowercase();
    (tag.len() == 64 && tag.bytes().all(|b| b.is_ascii_hexdigit())).then_some(tag)
}

/// The SHA-256 the host publishes for `url`, if any. Hugging Face sends it
/// as `X-Linked-ETag` on the resolve redirect, which a redirect-following
/// client never sees, so this asks without following. `None` means the
/// host publishes no digest (or could not be asked): the download is then
/// validated structurally only, and no provenance checksum is recorded.
pub(crate) fn published_sha256(url: &str, token: Option<&str>) -> Option<String> {
    if !crate::susi_core::mac_policy::egress_permitted(url) {
        return None;
    }
    let mut headers: Vec<(&str, String)> = Vec::new();
    if let Some(token) = token {
        headers.push(("Authorization", format!("Bearer {token}")));
    }
    let header_refs: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let call = susi_http_transport::http_call("HEAD", url, &header_refs, 30, 0).ok()?;
    linked_etag_digest(call.header("x-linked-etag")?)
}

/// Move a validated `part` into place. With a published digest, the bytes
/// must hash to it first (a mismatch discards the part so the next attempt
/// starts clean), and the digest is recorded in the model's provenance so
/// `verify_model_integrity` detects later corruption on load.
fn publish(
    part: &Path,
    destination: &Path,
    url: &str,
    expected_sha256: Option<&str>,
) -> Result<(), String> {
    // Every model/tokenizer download funnels through here, including the
    // daemon's zero-config auto-prime, which never passes authorize_tool.
    if !crate::susi_core::mac_policy::egress_permitted(url) {
        return Err(format!(
            "[PRIVACY] network egress blocked by the privacy posture: {url} (run `susi privacy consent --egress`)"
        ));
    }
    if let Some(expected) = expected_sha256 {
        let actual = super::lifecycle::ModelManager::calculate_simple_checksum(part)
            .map_err(|e| e.to_string())?;
        if actual != expected {
            OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(part)
                .map_err(|e| e.to_string())?;
            return Err(format!(
                "Checksum mismatch: published sha256 {expected}, downloaded {actual}"
            ));
        }
    }
    fs::rename(part, destination).map_err(|e| e.to_string())?;
    if let Some(expected) = expected_sha256 {
        let provenance = super::lifecycle::ModelProvenance {
            source_url: url.to_string(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            original_checksum: Some(expected.to_string()),
        };
        crate::susi_config::atomic_write_json_pretty(
            &destination.with_extension("provenance.json"),
            &provenance,
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn range_bounds(value: &str) -> Option<(u64, u64, u64)> {
    let (span, total) = value.strip_prefix("bytes ")?.split_once('/')?;
    let (start, end) = span.split_once('-')?;
    let (start, end, total) = (start.parse().ok()?, end.parse().ok()?, total.parse().ok()?);
    (start <= end && end < total).then_some((start, end, total))
}

#[allow(clippy::too_many_arguments)] // flat parameter list mirrors the call sites; a builder would only wrap them
pub(crate) fn transfer(
    url: &str,
    destination: &Path,
    timeout_secs: u64,
    token: Option<&str>,
    cancelled: &dyn Fn() -> bool,
    progress: &dyn Fn(u64, u64),
    validate: &dyn Fn(&Path) -> bool,
    expected_sha256: Option<&str>,
) -> Result<(), String> {
    let lock_path = PathBuf::from(format!("{}.lock", destination.display()));
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path)
        .map_err(|e| e.to_string())?;
    // A sibling thread's fork briefly duplicates this descriptor into the
    // child until its exec closes it (O_CLOEXEC applies at exec), holding
    // the flock for that window; an immediate retry then saw "another
    // worker" (a gate flake in interrupted_transfer_resumes_...). A real
    // concurrent download holds it for seconds to minutes, so ~1s of
    // retries absorbs the transient without masking contention.
    let mut attempts = 0;
    while lock.try_lock().is_err() {
        attempts += 1;
        if attempts >= 20 {
            return Err("Artifact is being downloaded by another worker".to_string());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let part = PathBuf::from(format!("{}.part", destination.display()));
    let checkpoint_path = PathBuf::from(format!("{}.download.json", destination.display()));
    let mut checkpoint: Checkpoint = fs::read(&checkpoint_path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    if !checkpoint.url.is_empty() && checkpoint.url != url {
        return Err("Artifact filename is already owned by a different source URL".into());
    }
    if destination.is_file() && validate(destination) {
        let size = destination.metadata().map_err(|e| e.to_string())?.len();
        progress(size, size);
        return Ok(());
    }
    // Migrate interrupted downloads created by older SUSI versions.
    if destination.is_file() && !part.exists() {
        fs::rename(destination, &part).map_err(|e| e.to_string())?;
    }
    let mut offset = part.metadata().map(|m| m.len()).unwrap_or(0);
    // A prefix without a strong validator cannot be safely joined to a new revision.
    if offset > 0 && checkpoint.validator.is_none() {
        OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&part)
            .map_err(|e| e.to_string())?;
        offset = 0;
    }
    let mut headers: Vec<(String, String)> = vec![("Accept-Encoding".into(), "identity".into())];
    if let Some(token) = token {
        headers.push(("Authorization".into(), format!("Bearer {token}")));
    }
    if offset > 0 {
        headers.push(("Range".into(), format!("bytes={offset}-")));
        if let Some(v) = &checkpoint.validator {
            headers.push(("If-Range".into(), v.clone()));
        }
    }
    if cancelled() {
        return Err("Download cancelled".into());
    }
    let header_refs: Vec<(&str, &str)> = headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let response = susi_http_transport::http_call("GET", url, &header_refs, timeout_secs, 10)
        .map_err(|e| format!("Transfer: {e}"))?;
    if response.status == 416 {
        let remote_len = response
            .header("content-range")
            .and_then(|s| s.strip_prefix("bytes */"))
            .and_then(|n| n.parse::<u64>().ok());
        if remote_len == Some(offset) && validate(&part) {
            publish(&part, destination, url, expected_sha256)?;
            progress(offset, offset);
            return Ok(());
        }
        // The next bounded retry starts fresh; don't recurse indefinitely.
        OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&part)
            .map_err(|e| e.to_string())?;
        return Err("Remote artifact changed or incomplete payload; restarting".into());
    }
    if !(200..300).contains(&response.status) {
        return Err(format!("HTTP {}", response.status));
    }
    let partial = response.status == 206;
    let content_length = response
        .header("content-length")
        .and_then(|v| v.parse::<u64>().ok());
    let (start, total) = if partial {
        let (start, end, total) = response
            .header("content-range")
            .and_then(range_bounds)
            .ok_or("Missing or invalid Content-Range")?;
        if start != offset {
            return Err("Server returned the wrong resume offset".into());
        }
        if let Some(length) = content_length {
            if length != end - start + 1 {
                return Err("Inconsistent range length".into());
            }
        }
        (start, total)
    } else {
        (0, content_length.unwrap_or(0))
    };
    let validator = response
        .header("etag")
        .filter(|v| !v.starts_with("W/"))
        .map(str::to_owned);
    if partial
        && checkpoint.validator.is_some()
        && validator.is_some()
        && checkpoint.validator != validator
    {
        OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&part)
            .map_err(|e| e.to_string())?;
        return Err("Artifact validator changed; restarting".into());
    }
    checkpoint.url = url.into();
    checkpoint.validator = validator;
    crate::susi_config::atomic_write_bytes(
        &checkpoint_path,
        &serde_json::to_vec(&checkpoint).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    if total > start {
        let parent = destination.parent().ok_or("Missing model directory")?;
        let free = super::hardware::HardwareProfiler::get_free_disk_bytes(parent);
        if free < (total - start).saturating_add(256 * 1024 * 1024) {
            return Err("Insufficient free disk for remaining download".into());
        }
    }
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .append(partial)
        .truncate(!partial)
        .open(&part)
        .map_err(|e| e.to_string())?;
    let mut downloaded = start;
    let mut buffer = vec![0; 1024 * 1024];
    progress(downloaded, total);
    let mut body = response.into_reader();
    loop {
        if cancelled() {
            return Err("Download cancelled".into());
        }
        let count = body.read(&mut buffer).map_err(|e| format!("Read: {e}"))?;
        if count == 0 {
            break;
        }
        file.write_all(&buffer[..count])
            .map_err(|e| e.to_string())?;
        downloaded += count as u64;
        progress(downloaded, total);
    }
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    if total > 0 && downloaded != total {
        return Err("Incomplete response body".into());
    }
    if !validate(&part) {
        return Err("Artifact validation failed".into());
    }
    publish(&part, destination, url, expected_sha256)?;
    progress(downloaded, downloaded);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    fn workspace(label: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("susi_transfer_{label}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn server(
        responses: Vec<(&'static str, &'static str)>,
    ) -> (String, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/model.gguf", listener.local_addr().unwrap());
        let thread = std::thread::spawn(move || {
            for (expected, response) in responses {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    socket.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                assert!(String::from_utf8_lossy(&request)
                    .to_lowercase()
                    .contains(expected));
                socket.write_all(response.as_bytes()).unwrap();
            }
        });
        (url, thread)
    }

    #[test]
    fn interrupted_transfer_resumes_and_publishes_atomically() {
        let dir = workspace("resume");
        let path = dir.join("model.gguf");
        let (url, server) = server(vec![
            ("get /model.gguf", "HTTP/1.1 200 OK\r\nContent-Length: 10\r\nETag: \"v1\"\r\nConnection: close\r\n\r\nabcde"),
            ("range: bytes=5-", "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 5-9/10\r\nContent-Length: 5\r\nETag: \"v1\"\r\nConnection: close\r\n\r\nfghij"),
        ]);
        let validate = |p: &Path| fs::read(p).is_ok_and(|b| b == b"abcdefghij");
        assert!(transfer(&url, &path, 5, None, &|| false, &|_, _| {}, &validate, None).is_err());
        assert!(!path.exists());
        assert!(dir.join("model.gguf.part").exists());
        transfer(&url, &path, 5, None, &|| false, &|_, _| {}, &validate, None).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"abcdefghij");
        assert!(!dir.join("model.gguf.part").exists());
        server.join().unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn ignored_range_restarts_without_appending() {
        let dir = workspace("ignored_range");
        let path = dir.join("model.gguf");
        let (url, server) = server(vec![("range: bytes=5-", "HTTP/1.1 200 OK\r\nContent-Length: 10\r\nETag: \"v2\"\r\nConnection: close\r\n\r\nabcdefghij")]);
        fs::write(dir.join("model.gguf.part"), b"XXXXX").unwrap();
        fs::write(
            dir.join("model.gguf.download.json"),
            serde_json::to_vec(&Checkpoint {
                url: url.clone(),
                validator: Some("\"v1\"".into()),
            })
            .unwrap(),
        )
        .unwrap();
        transfer(
            &url,
            &path,
            5,
            None,
            &|| false,
            &|_, _| {},
            &|p| fs::read(p).is_ok_and(|b| b == b"abcdefghij"),
            None,
        )
        .unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"abcdefghij");
        server.join().unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn mismatched_resume_offset_never_publishes() {
        let dir = workspace("bad_range");
        let path = dir.join("model.gguf");
        let (url, server) = server(vec![("range: bytes=5-", "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 4-9/10\r\nContent-Length: 6\r\nETag: \"v1\"\r\nConnection: close\r\n\r\nefghij")]);
        fs::write(dir.join("model.gguf.part"), b"abcde").unwrap();
        fs::write(
            dir.join("model.gguf.download.json"),
            serde_json::to_vec(&Checkpoint {
                url: url.clone(),
                validator: Some("\"v1\"".into()),
            })
            .unwrap(),
        )
        .unwrap();
        assert!(transfer(&url, &path, 5, None, &|| false, &|_, _| {}, &|_| true, None).is_err());
        assert!(!path.exists());
        assert_eq!(fs::read(dir.join("model.gguf.part")).unwrap(), b"abcde");
        server.join().unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn published_digest_gates_publication_and_is_recorded() {
        use sha2::Digest;
        let body = b"abcdefghij";
        let good = hex::encode(sha2::Sha256::digest(body));
        let response =
            "HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\nabcdefghij";

        let dir = workspace("digest_bad");
        let path = dir.join("model.gguf");
        let (url, srv) = server(vec![("get /model.gguf", response)]);
        let bad = "0".repeat(64);
        let err = transfer(
            &url,
            &path,
            5,
            None,
            &|| false,
            &|_, _| {},
            &|_| true,
            Some(&bad),
        )
        .unwrap_err();
        assert!(err.contains("Checksum mismatch"), "{err}");
        assert!(!path.exists());
        srv.join().unwrap();
        fs::remove_dir_all(dir).unwrap();

        let dir = workspace("digest_good");
        let path = dir.join("model.gguf");
        let (url, srv) = server(vec![("get /model.gguf", response)]);
        transfer(
            &url,
            &path,
            5,
            None,
            &|| false,
            &|_, _| {},
            &|_| true,
            Some(&good),
        )
        .unwrap();
        assert_eq!(fs::read(&path).unwrap(), body);
        let provenance: super::super::lifecycle::ModelProvenance =
            serde_json::from_slice(&fs::read(dir.join("model.provenance.json")).unwrap()).unwrap();
        assert_eq!(provenance.original_checksum.as_deref(), Some(good.as_str()));
        assert!(super::super::lifecycle::ModelManager::verify_model_integrity(&path).is_ok());
        fs::write(&path, b"tampered!!").unwrap();
        assert!(super::super::lifecycle::ModelManager::verify_model_integrity(&path).is_err());
        srv.join().unwrap();
        fs::remove_dir_all(dir).unwrap();

        assert_eq!(
            linked_etag_digest(&format!("\"{}\"", good.to_uppercase())),
            Some(good)
        );
        assert_eq!(linked_etag_digest("\"v1\""), None);
    }

    #[test]
    fn ranges_are_strict() {
        assert_eq!(range_bounds("bytes 5-9/10"), Some((5, 9, 10)));
        for bad in ["bytes 9-5/10", "bytes 0-10/10", "bytes */10", "bytes 0-9/*"] {
            assert!(range_bounds(bad).is_none());
        }
    }
    #[test]
    fn query_is_not_part_of_filename() {
        assert_eq!(
            artifact_name("https://host/model.gguf?download=true").unwrap(),
            "model.gguf"
        );
        assert!(artifact_name("https://host/").is_err());
    }
}
