//! Offline GGUF header inspector (no model load).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GgufMeta {
    pub architecture: String,
    pub param_count_hint: u64,
    pub quantization: String,
}

#[must_use]
pub fn parse_gguf_kv_dump(dump: &str) -> Option<GgufMeta> {
    let mut architecture = None;
    let mut params = None;
    let mut quant = None;
    for line in dump.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("general.architecture=") {
            architecture = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("general.parameter_count=") {
            params = v.parse().ok();
        } else if let Some(v) = line.strip_prefix("general.file_type=") {
            quant = Some(v.to_string());
        }
    }
    Some(GgufMeta {
        architecture: architecture?,
        param_count_hint: params.unwrap_or(0),
        quantization: quant.unwrap_or_else(|| "unknown".into()),
    })
}

#[cfg(test)]
mod gguf_inspector_tests {
    use super::*;

    #[test]
    fn gguf_inspector_parses_offline_kv_dump() {
        let m = parse_gguf_kv_dump(
            "general.architecture=llama\ngeneral.parameter_count=7000000000\ngeneral.file_type=Q4_K_M\n",
        )
        .unwrap();
        assert_eq!(m.architecture, "llama");
        assert_eq!(m.param_count_hint, 7_000_000_000);
        assert_eq!(m.quantization, "Q4_K_M");
    }
}
