//! Pin every core tool's ordered metadata and complete generated input schema.
#![allow(clippy::unwrap_used)] // Test-only fixture serialization.

use sha2::{Digest, Sha256};

#[test]
fn catalog_metadata_and_schemas_preserve_the_wire_contract() {
    let definitions = susi_gmcp::tools::args::core_tool_definitions();
    let encoded = serde_json::to_vec(&definitions).unwrap();
    let fingerprint = format!("{:x}", Sha256::digest(encoded));
    // Recorded from the unmodified catalog at 6a262d346, before consolidating
    // declarations. This covers descriptions, field/default/nullable metadata,
    // nested definitions, required-key ordering and the complete tool order.
    let (count, expected) = if cfg!(feature = "tools-rich") {
        (
            204,
            "7a89f2f05efb8cfb8e7242036f60b36948348a4fa45045c58d830db764edf82d",
        )
    } else {
        (
            199,
            "c8303f4bff9e9a13fd7da39094683ae27fc4b66912e74c50880710694ad02c52",
        )
    };
    assert_eq!(definitions.len(), count);
    assert_eq!(
        fingerprint, expected,
        "review intentional wire-contract changes before refreshing this baseline"
    );
}
