#![deny(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        unsafe_code,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

//! Search-evidence dispatch for admitted tools.
//!
//! The OS does not embed a weather provider or a search engine. Live evidence
//! comes only from a search tool the operator has admitted.

pub use susi_core;
pub use susi_error;

pub mod live_search;

#[cfg(test)]
mod tests {
    // Test-only assertions in inline unit tests.
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn reexported_susi_error_constructors_preserve_messages() {
        let err = susi_error::EaiError::config("missing search adapter");
        assert!(err.to_string().contains("missing search adapter"));
    }

    #[test]
    fn gather_live_evidence_reports_unavailable_when_no_search_tool_linked() {
        let out =
            live_search::gather_live_evidence("rust release notes", std::path::Path::new("."));
        assert!(out.contains("Live search data unavailable."));
        assert!(out.contains("I will not invent conditions or citations."));
    }

    #[test]
    fn gather_live_evidence_lists_all_three_candidate_search_tools() {
        let out = live_search::gather_live_evidence("weather in tokyo", std::path::Path::new("."));
        assert!(out.contains("brave_search"));
        assert!(out.contains("google_search"));
        assert!(out.contains("web_search"));
    }

    #[test]
    fn gather_live_evidence_handles_empty_goal_without_fabricating_results() {
        let out = live_search::gather_live_evidence("", std::path::Path::new("."));
        assert!(out.contains("Admit a search tool"));
    }

    #[test]
    fn susi_core_reexport_is_reachable_from_vendor_web_root() {
        assert!(!susi_core::plane_bus::tools::exists(
            "__nonexistent_vendor_web_probe__"
        ));
    }
}
