//! Pin every core tool's ordered metadata and complete generated input schema.
#![allow(clippy::unwrap_used)] // Test-only fixture serialization.

use sha2::{Digest, Sha256};

#[test]
fn catalog_metadata_and_schemas_preserve_the_wire_contract() {
    let definitions = susi_gmcp::tools::args::core_tool_definitions();
    let encoded = serde_json::to_vec(&definitions).unwrap();
    let fingerprint = format!("{:x}", Sha256::digest(encoded));
    // Relative to the 6a262d346 baseline, 6cc460a8b changed only the
    // HarnessRun.rsi_log description. Restoring its old default path in
    // either serialized catalog reproduces the original fingerprint.
    let (count, expected) = if cfg!(feature = "tools-rich") {
        (
            204,
            "a908870e822408487defe8c62d346110c43e512e7854bdc364ef22035a2c8cd4",
        )
    } else {
        (
            199,
            "cd59c259efb1f388fc5cab10c43558fb812ce637111b3a35f2fcd1e6ea3d12d5",
        )
    };
    assert_eq!(definitions.len(), count);
    assert_eq!(
        fingerprint, expected,
        "review intentional wire-contract changes before refreshing this baseline"
    );
}
