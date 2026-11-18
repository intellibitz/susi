#![no_main]

use libfuzzer_sys::fuzz_target;
use susi_core::evidence::EvidenceRecord;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _: Result<EvidenceRecord, _> = serde_json::from_str(s);
    }
});
