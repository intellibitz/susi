#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! Acceptance surface for the VC-203 GMCP completion program.
//!
//! Every queued `T-CODEXGMCPROADMAP-*` task is gated on a `gmcp_req_203_*`
//! test living in this file; this first contract test proves the normative
//! requirements ledger (VC-203-001) is pinned, verbatim, fully classified,
//! and fail-closed against upstream drift.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use susi_paths::test_env::{isolate_home, EnvGuard};

include!("gmcp_requirements/support.rs");
include!("gmcp_requirements/adapter_support.rs");
include!("gmcp_requirements/vc_203_001.rs");
include!("gmcp_requirements/vc_203_002.rs");
include!("gmcp_requirements/vc_203_003.rs");
include!("gmcp_requirements/vc_203_004.rs");
include!("gmcp_requirements/vc_203_005.rs");
include!("gmcp_requirements/vc_203_006.rs");
include!("gmcp_requirements/vc_203_007.rs");
include!("gmcp_requirements/vc_203_008.rs");
include!("gmcp_requirements/vc_203_009.rs");
include!("gmcp_requirements/vc_203_010.rs");
include!("gmcp_requirements/vc_203_011_part1.rs");
include!("gmcp_requirements/vc_203_011_part2.rs");
include!("gmcp_requirements/vc_203_012_part1.rs");
include!("gmcp_requirements/vc_203_012_part2.rs");
include!("gmcp_requirements/vc_203_012_part3.rs");
include!("gmcp_requirements/vc_203_012_part4.rs");
include!("gmcp_requirements/vc_203_012_part5.rs");
include!("gmcp_requirements/vc_203_013.rs");
include!("gmcp_requirements/vc_203_014_part1.rs");
include!("gmcp_requirements/vc_203_014_part2.rs");
include!("gmcp_requirements/vc_203_014_part3.rs");
include!("gmcp_requirements/vc_203_014_part4.rs");
include!("gmcp_requirements/vc_203_016_part1.rs");
include!("gmcp_requirements/vc_203_016_part2.rs");
include!("gmcp_requirements/vc_203_020.rs");
