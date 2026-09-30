//! Resumable, checksummed model downloads.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadPlan {
    pub resume_from: u64,
    pub expected_sha256: String,
    pub complete: bool,
}

/// Plan a (re)download from existing bytes and a publisher checksum.
#[must_use]
pub fn plan_download(have_bytes: u64, total_bytes: u64, sha256: &str) -> DownloadPlan {
    DownloadPlan {
        resume_from: have_bytes.min(total_bytes),
        expected_sha256: sha256.into(),
        complete: have_bytes >= total_bytes && !sha256.is_empty(),
    }
}

#[must_use]
pub fn checksum_ok(got: &str, expected: &str) -> bool {
    !expected.is_empty() && got.eq_ignore_ascii_case(expected)
}

#[cfg(test)]
mod resumable_downloads_tests {
    use super::*;

    #[test]
    fn resumable_downloads_resume_and_verify() {
        let p = plan_download(100, 1000, "abc");
        assert_eq!(p.resume_from, 100);
        assert!(!p.complete);
        assert!(checksum_ok("ABC", "abc"));
        assert!(!checksum_ok("nope", "abc"));
    }
}
