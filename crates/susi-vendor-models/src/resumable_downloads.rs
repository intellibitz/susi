//! Resumable, checksummed model downloads (T-CLAUDE-26).
//!
//! A download is a loop of `Range: bytes=<offset>-` requests: resume from the
//! bytes already on disk, verify sha256 against the hub/catalog value when
//! the body completes, and quarantine (rename to `.corrupt`) rather than
//! delete on mismatch. The transport is injected so tests drive the loop
//! against a local server that cuts connections mid-stream; `ureq` wires the
//! real HTTPS path at the call site.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use susi_error::{EaiError, EaiResult};

/// One ranged GET the downloader wants the transport to perform.
#[derive(Debug, Clone)]
pub struct RangeRequest {
    pub url: String,
    /// `Range: bytes=<from>-` — first byte wanted; `None` = fresh GET.
    pub from: u64,
}

/// What the transport hands back.
#[derive(Debug, Clone)]
pub struct RangeResponse {
    /// HTTP status (200 full body, 206 partial, 416 out of range, others err).
    pub status: u16,
    /// Body bytes (may be shorter than requested when the server cuts).
    pub body: Vec<u8>,
    /// `Accept-Ranges: bytes` advertised.
    pub accept_ranges: bool,
}

#[derive(Debug, Clone)]
pub struct DownloadPlan {
    pub url: String,
    pub dest: PathBuf,
    /// Expected sha256 hex; verification is skipped when absent.
    pub sha256: Option<String>,
    /// Declared size when known (progress/resume sanity).
    pub size: Option<u64>,
    /// Give up after this many transport errors.
    pub max_retries: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DownloadReport {
    /// Bytes written to `dest`.
    pub bytes: u64,
    /// How many range requests were made (1 = no resume needed).
    pub requests: u32,
    /// `true` when sha256 was checked and matched.
    pub verified: bool,
    /// Where a corrupt file was moved, when verification failed.
    pub quarantined_to: Option<PathBuf>,
}

/// Run the resume/verify loop. `transport` performs one ranged GET.
pub fn download(
    plan: &DownloadPlan,
    transport: &dyn Fn(&RangeRequest) -> EaiResult<RangeResponse>,
) -> EaiResult<DownloadReport> {
    let mut have = existing_len(&plan.dest);
    let mut requests = 0u32;
    let mut retries = 0u32;
    let mut f = open_for_append(&plan.dest, have)?;

    loop {
        let resp = match transport(&RangeRequest {
            url: plan.url.clone(),
            from: have,
        }) {
            Ok(r) => r,
            Err(e) => {
                retries += 1;
                if retries > plan.max_retries {
                    return Err(EaiError::network(format!(
                        "download failed after {retries} retries: {e}"
                    )));
                }
                continue;
            }
        };
        retries = 0;
        requests += 1;
        match resp.status {
            200 => {
                // server ignored Range — rewrite from scratch
                f = open_for_append(&plan.dest, 0)?;
                have = 0;
                f.write_all(&resp.body)
                    .map_err(|e| EaiError::io(e.to_string()))?;
                have += resp.body.len() as u64;
            }
            206 => {
                if !resp.accept_ranges {
                    return Err(EaiError::network("server sent 206 without Accept-Ranges"));
                }
                f.write_all(&resp.body)
                    .map_err(|e| EaiError::io(e.to_string()))?;
                have += resp.body.len() as u64;
            }
            416 => {
                // out of range: we already hold everything the server has
                break;
            }
            s => {
                return Err(EaiError::network(format!("unexpected status {s}")));
            }
        }
        // unknown size: a 200 carries the whole body; a 206 needs one more
        // probe, which comes back 416 when we already hold everything.
        let complete = match plan.size {
            Some(total) => have >= total,
            None => resp.status != 206,
        };
        if complete {
            break;
        }
    }
    drop(f);
    finish(plan, have, requests)
}

fn finish(plan: &DownloadPlan, have: u64, requests: u32) -> EaiResult<DownloadReport> {
    if let Some(expected) = &plan.sha256 {
        let actual = sha256_file(&plan.dest)?;
        if !actual.eq_ignore_ascii_case(expected) {
            let q = plan.dest.with_extension("corrupt");
            std::fs::rename(&plan.dest, &q).map_err(|e| EaiError::io(e.to_string()))?;
            return Ok(DownloadReport {
                bytes: have,
                requests,
                verified: false,
                quarantined_to: Some(q),
            });
        }
        return Ok(DownloadReport {
            bytes: have,
            requests,
            verified: true,
            quarantined_to: None,
        });
    }
    Ok(DownloadReport {
        bytes: have,
        requests,
        verified: false,
        quarantined_to: None,
    })
}

/// Hash a completed artifact without materialising the artifact in memory.
///
/// Model weights can be multi-gigabyte files.  Verification is deliberately
/// a bounded-buffer stream so a checksum check cannot turn into an OOM at the
/// end of an otherwise successful download.
fn sha256_file(path: &Path) -> EaiResult<String> {
    use sha2::Digest;

    let mut file = std::fs::File::open(path).map_err(|e| EaiError::io(e.to_string()))?;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|e| EaiError::io(e.to_string()))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Verify an already-staged artifact with the same bounded checksum path used
/// by the downloader's completion step.
pub fn verify_file(path: &Path, expected: &str) -> EaiResult<bool> {
    Ok(sha256_file(path)?.eq_ignore_ascii_case(expected))
}

fn existing_len(dest: &Path) -> u64 {
    std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0)
}

fn open_for_append(dest: &Path, offset: u64) -> EaiResult<std::fs::File> {
    use std::io::Seek;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| EaiError::io(e.to_string()))?;
    }
    if offset == 0 {
        return std::fs::File::create(dest).map_err(|e| EaiError::io(e.to_string()));
    }
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .open(dest)
        .map_err(|e| EaiError::io(e.to_string()))?;
    f.seek(std::io::SeekFrom::Start(offset))
        .map_err(|e| EaiError::io(e.to_string()))?;
    Ok(f)
}

/// sha256 hex of `data` (vendored sha2 — no new deps).
pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    let d = sha2::Sha256::digest(data);
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// Minimal std-only transport for `http://` URLs (loopback/local servers in
/// tests and mirrors). HTTPS goes through ureq at the call site.
pub fn http_transport(req: &RangeRequest) -> EaiResult<RangeResponse> {
    use std::io::{BufRead, BufReader, Read};
    let url = req
        .url
        .strip_prefix("http://")
        .ok_or_else(|| EaiError::config("http_transport only speaks http://"))?;
    let (hostport, path) = url
        .split_once('/')
        .map(|(h, p)| (h, format!("/{p}")))
        .unwrap_or((url, "/".into()));
    let mut s = std::net::TcpStream::connect(hostport)
        .map_err(|e| EaiError::network(format!("connect {hostport}: {e}")))?;
    let range = format!("Range: bytes={}-\r\n", req.from);
    let head =
        format!("GET {path} HTTP/1.1\r\nHost: {hostport}\r\n{range}Connection: close\r\n\r\n");
    s.write_all(head.as_bytes())
        .map_err(|e| EaiError::network(e.to_string()))?;
    let mut r = BufReader::new(s);
    let mut status = String::new();
    r.read_line(&mut status)
        .map_err(|e| EaiError::network(e.to_string()))?;
    let code: u16 = status
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| EaiError::network(format!("bad status line: {status}")))?;
    let mut accept_ranges = false;
    let mut content_len: Option<usize> = None;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line)
            .map_err(|e| EaiError::network(e.to_string()))?
            == 0
        {
            break;
        }
        let l = line.trim();
        if l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            match k.trim().to_ascii_lowercase().as_str() {
                "accept-ranges" => accept_ranges = v.trim().eq_ignore_ascii_case("bytes"),
                "content-length" => content_len = v.trim().parse().ok(),
                _ => {}
            }
        }
    }
    let mut body = Vec::new();
    match content_len {
        Some(n) => {
            body.resize(n, 0);
            r.read_exact(&mut body)
                .map_err(|e| EaiError::network(format!("body truncated: {e}")))?;
        }
        None => {
            r.read_to_end(&mut body)
                .map_err(|e| EaiError::network(e.to_string()))?;
        }
    }
    Ok(RangeResponse {
        status: code,
        body,
        accept_ranges,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    const DATA: &[u8] = b"model-weights-pretend-binary-0123456789abcdef";

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("susi-dl-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Serve DATA over HTTP/1.1, honouring Range, but only write the first
    /// `cut` bytes of each body before closing (simulates a cut connection
    /// — client must retry with a new Range).
    fn serve_cut(cut: usize) -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for s in l.incoming().take(16) {
                let mut s = s.unwrap();
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let from: usize = req
                    .lines()
                    .find_map(|l| l.strip_prefix("Range: bytes="))
                    .and_then(|r| r.split('-').next())
                    .and_then(|r| r.trim().parse().ok())
                    .unwrap_or(0);
                if from >= DATA.len() {
                    let head = "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\n\r\n";
                    let _ = s.write_all(head.as_bytes());
                    continue;
                }
                let end = from.saturating_add(cut).min(DATA.len());
                let body = &DATA[from..end];
                let head = format!(
                    "HTTP/1.1 206 Partial Content\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {from}-{}/43\r\nContent-Length: {}\r\n\r\n",
                    end - 1,
                    body.len()
                );
                let _ = s.write_all(head.as_bytes());
                let _ = s.write_all(body);
            }
        });
        port
    }

    fn plan_for(port: u16, dest: &Path, sha: Option<String>) -> DownloadPlan {
        DownloadPlan {
            url: format!("http://127.0.0.1:{port}/model.gguf"),
            dest: dest.to_path_buf(),
            sha256: sha,
            size: Some(DATA.len() as u64),
            max_retries: 8,
        }
    }

    #[test]
    fn resumable_downloads_completes_over_cut_connections() {
        let port = serve_cut(9); // server sends only 9 bytes per request
        let d = dir("cut");
        let dest = d.join("m.gguf");
        let sha = sha256_hex(DATA);
        let r = download(&plan_for(port, &dest, Some(sha.clone())), &http_transport).unwrap();
        assert!(r.verified);
        assert!(r.requests >= 4); // 44 bytes / 9 per response
        assert_eq!(std::fs::read(&dest).unwrap(), DATA);
    }

    #[test]
    fn resumable_downloads_resumes_partial_file() {
        let port = serve_cut(usize::MAX); // full body per request
        let d = dir("resume");
        let dest = d.join("m.gguf");
        std::fs::write(&dest, &DATA[..20]).unwrap(); // pretend earlier progress
        let sha = sha256_hex(DATA);
        let r = download(&plan_for(port, &dest, Some(sha)), &http_transport).unwrap();
        assert!(r.verified);
        assert_eq!(r.requests, 1); // resumed tail only
        assert_eq!(std::fs::read(&dest).unwrap(), DATA);
    }

    #[test]
    fn resumable_downloads_quarantines_corrupt_files() {
        let port = serve_cut(usize::MAX);
        let d = dir("corrupt");
        let dest = d.join("m.gguf");
        // wrong expected hash → quarantine, no delete
        let r = download(
            &plan_for(port, &dest, Some("00".repeat(32))),
            &http_transport,
        )
        .unwrap();
        assert!(!r.verified);
        assert!(r.quarantined_to.is_some());
        assert!(!dest.exists());
        assert_eq!(std::fs::read(d.join("m.corrupt")).unwrap(), DATA);
    }

    #[test]
    fn resumable_downloads_sha256_helper() {
        // pinned known value
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(sha256_hex(DATA).len(), 64);
    }

    #[test]
    fn resumable_downloads_retries_then_fails() {
        let d = dir("dead");
        let dest = d.join("m.gguf");
        // nothing listening on this port
        let mut p = plan_for(1, &dest, None);
        p.max_retries = 2;
        let e = download(&p, &http_transport).unwrap_err();
        assert!(e.to_string().contains("retries") || e.to_string().contains("connect"));
    }
}
