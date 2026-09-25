//! Resumable artifact transfer. A model becomes visible only after validation.
use reqwest::blocking::Client;
use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE, ETAG, IF_RANGE, RANGE};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

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
) -> Result<(), String> {
    let lock_path = PathBuf::from(format!("{}.lock", destination.display()));
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path)
        .map_err(|e| e.to_string())?;
    lock.try_lock()
        .map_err(|_| "Artifact is being downloaded by another worker".to_string())?;
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
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(timeout_secs.max(1)))
        .build()
        .map_err(|e| e.to_string())?;
    let mut request = client.get(url).header("Accept-Encoding", "identity");
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    if offset > 0 {
        request = request.header(RANGE, format!("bytes={offset}-"));
        if let Some(v) = &checkpoint.validator {
            request = request.header(IF_RANGE, v);
        }
    }
    if cancelled() {
        return Err("Download cancelled".into());
    }
    let mut response = request.send().map_err(|e| format!("Transfer: {e}"))?;
    if response.status().as_u16() == 416 {
        let remote_len = response
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.strip_prefix("bytes */"))
            .and_then(|n| n.parse::<u64>().ok());
        if remote_len == Some(offset) && validate(&part) {
            fs::rename(&part, destination).map_err(|e| e.to_string())?;
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
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status().as_u16()));
    }
    let partial = response.status().as_u16() == 206;
    let (start, total) = if partial {
        let (start, end, total) = response
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|h| h.to_str().ok())
            .and_then(range_bounds)
            .ok_or("Missing or invalid Content-Range")?;
        if start != offset {
            return Err("Server returned the wrong resume offset".into());
        }
        if let Some(length) = response.content_length() {
            if length != end - start + 1 {
                return Err("Inconsistent range length".into());
            }
        }
        (start, total)
    } else {
        (
            0,
            response
                .headers()
                .get(CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
        )
    };
    let validator = response
        .headers()
        .get(ETAG)
        .and_then(|v| v.to_str().ok())
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
    fs::write(
        &checkpoint_path,
        serde_json::to_vec(&checkpoint).map_err(|e| e.to_string())?,
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
    loop {
        if cancelled() {
            return Err("Download cancelled".into());
        }
        let count = response
            .read(&mut buffer)
            .map_err(|e| format!("Read: {e}"))?;
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
    fs::rename(&part, destination).map_err(|e| e.to_string())?;
    progress(downloaded, downloaded);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert!(transfer(&url, &path, 5, None, &|| false, &|_, _| {}, &validate).is_err());
        assert!(!path.exists());
        assert!(dir.join("model.gguf.part").exists());
        transfer(&url, &path, 5, None, &|| false, &|_, _| {}, &validate).unwrap();
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
        transfer(&url, &path, 5, None, &|| false, &|_, _| {}, &|p| {
            fs::read(p).is_ok_and(|b| b == b"abcdefghij")
        })
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
        assert!(transfer(&url, &path, 5, None, &|| false, &|_, _| {}, &|_| true).is_err());
        assert!(!path.exists());
        assert_eq!(fs::read(dir.join("model.gguf.part")).unwrap(), b"abcde");
        server.join().unwrap();
        fs::remove_dir_all(dir).unwrap();
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
