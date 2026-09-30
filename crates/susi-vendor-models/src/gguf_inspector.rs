//! Offline GGUF header inspector — parses the metadata key/value section of a
//! `.gguf` file without loading tensor data. Used by the launcher and the
//! quant recommender to learn architecture, parameter count, quantisation,
//! context length, layer count and chat template from the file itself.
//!
//! Format (GGUF v2/v3): `"GGUF"` magic, u32 version, u64 tensor_count,
//! u64 metadata_kv_count, then `metadata_kv_count` key/value pairs. Keys are
//! length-prefixed UTF-8 strings; values are one of the fixed `ValueType`s or
//! homogeneous arrays of them.

use susi_error::{EaiError, EaiResult};

const MAGIC: &[u8; 4] = b"GGUF";

/// A parsed GGUF metadata value (arrays of numeric/bool values are kept as
/// their element lists; string arrays are joined is not — kept verbatim).
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    U64(u64),
    I64(i64),
    F64(f64),
    Bool(bool),
    Str(String),
    /// Arrays are flattened to strings/numbers only where trivially shown;
    /// the raw element count and type id are retained for anything else.
    Array {
        elem_type: u32,
        len: u64,
    },
}

impl Value {
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Value::U64(v) => Some(*v),
            Value::I64(v) => u64::try_from(*v).ok(),
            Value::F64(_) | Value::Bool(_) | Value::Str(_) | Value::Array { .. } => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            Value::U64(_)
            | Value::I64(_)
            | Value::F64(_)
            | Value::Bool(_)
            | Value::Array { .. } => None,
        }
    }
}

/// The header facts a launcher cares about, extracted from the raw metadata.
#[derive(Debug, Clone, Default)]
pub struct GgufInfo {
    /// GGUF container version (1–3).
    pub version: u32,
    /// `general.architecture` — e.g. `llama`, `qwen2`, `phi3`.
    pub architecture: Option<String>,
    /// `general.name`, if recorded.
    pub name: Option<String>,
    /// `general.file_type` mapped to its quantisation label (e.g. `Q4_K_M`).
    pub quantization: Option<String>,
    /// Raw `general.file_type` id.
    pub file_type: Option<u32>,
    /// `<arch>.context_length`.
    pub context_length: Option<u64>,
    /// `<arch>.block_count` — transformer layer count.
    pub layer_count: Option<u64>,
    /// `<arch>.embedding_length`.
    pub embedding_length: Option<u64>,
    /// `<arch>.feed_forward_length`.
    pub feed_forward_length: Option<u64>,
    /// Vocabulary size derived from `tokenizer.ggml.tokens` array length.
    pub vocab_size: Option<u64>,
    /// Best-effort parameter estimate from embedding/feed-forward/layers.
    pub parameter_count: Option<u64>,
    /// `tokenizer.chat_template` verbatim, when present.
    pub chat_template: Option<String>,
}

/// Parse a GGUF file from the front of `bytes` (the whole file or just its
/// header region — only the metadata section is read; tensor data is never
/// touched, so callers may pass a truncated prefix containing the full KV
/// section).
pub fn inspect(bytes: &[u8]) -> EaiResult<GgufInfo> {
    let (version, kv) = parse_metadata(bytes)?;
    Ok(derive_info(version, &kv))
}

/// Raw metadata access for callers that need keys beyond [`GgufInfo`].
pub fn metadata(bytes: &[u8]) -> EaiResult<Vec<(String, Value)>> {
    Ok(parse_metadata(bytes)?.1)
}

type Metadata = (u32, Vec<(String, Value)>);

fn parse_metadata(bytes: &[u8]) -> EaiResult<Metadata> {
    let mut r = Reader { bytes, pos: 0 };
    if r.take(4)? != MAGIC {
        return Err(EaiError::config("not a GGUF file: bad magic"));
    }
    let version = r.u32()?;
    if !(1..=3).contains(&version) {
        return Err(EaiError::config(format!(
            "unsupported GGUF version {version}"
        )));
    }
    // v1 stores counts as u32; v2+ as u64.
    let (_tensor_count, kv_count) = if version == 1 {
        (u64::from(r.u32()?), u64::from(r.u32()?))
    } else {
        (r.u64()?, r.u64()?)
    };
    let mut kv = Vec::with_capacity(kv_count.min(4096) as usize);
    for _ in 0..kv_count {
        let key = r.string()?;
        let vtype = r.u32()?;
        let value = r.value(vtype)?;
        kv.push((key, value));
    }
    Ok((version, kv))
}

fn derive_info(version: u32, kv: &[(String, Value)]) -> GgufInfo {
    let get = |key: &str| kv.iter().find(|(k, _)| k == key).map(|(_, v)| v);
    let num = |key: &str| get(key).and_then(Value::as_u64);
    let text = |key: &str| get(key).and_then(Value::as_str).map(str::to_string);

    let arch = text("general.architecture");
    let arch_key = |suffix: &str| -> String {
        match &arch {
            Some(a) => format!("{a}.{suffix}"),
            None => String::new(),
        }
    };
    let file_type = num("general.file_type").and_then(|v| u32::try_from(v).ok());
    let embedding = num(&arch_key("embedding_length"));
    let feed_forward = num(&arch_key("feed_forward_length"));
    let layers = num(&arch_key("block_count"));
    let vocab = match get("tokenizer.ggml.tokens") {
        Some(Value::Array { len, .. }) => Some(*len),
        _ => num("tokenizer.ggml.tokens_size"),
    };
    let params = match (embedding, feed_forward, layers, vocab) {
        (Some(e), Some(ff), Some(l), v) => {
            // attention+ffn per layer ≈ 4·e² (attn) + 3·e·ff (gated mlp),
            // plus token embeddings e·vocab. Ignores tied outputs/norms.
            let per_layer = 4 * e * e + 3 * e * ff;
            Some(l * per_layer + e * v.unwrap_or(0))
        }
        _ => None,
    };
    GgufInfo {
        version,
        architecture: arch.clone(),
        name: text("general.name"),
        quantization: file_type.map(file_type_label),
        file_type,
        context_length: num(&arch_key("context_length")),
        layer_count: layers,
        embedding_length: embedding,
        feed_forward_length: feed_forward,
        vocab_size: vocab,
        parameter_count: params,
        chat_template: text("tokenizer.chat_template"),
    }
}

/// `general.file_type` → llama.cpp quantisation label (subset of the enum;
/// unknown ids render as `FT_<id>`).
fn file_type_label(ft: u32) -> String {
    match ft {
        0 => "F32".into(),
        1 => "F16".into(),
        2 => "Q4_0".into(),
        3 => "Q4_1".into(),
        6 => "Q5_0".into(),
        7 => "Q5_1".into(),
        8 => "Q8_0".into(),
        10 => "Q2_K".into(),
        11 => "Q3_K_S".into(),
        12 => "Q3_K_M".into(),
        13 => "Q3_K_L".into(),
        14 => "Q4_K_S".into(),
        15 => "Q4_K_M".into(),
        16 => "Q5_K_S".into(),
        17 => "Q5_K_M".into(),
        18 => "Q6_K".into(),
        19 => "IQ2_XXS".into(),
        20 => "IQ2_XS".into(),
        21 => "Q2_K_S".into(),
        22 => "IQ3_XS".into(),
        23 => "IQ3_XXS".into(),
        24 => "IQ1_S".into(),
        25 => "IQ4_NL".into(),
        26 => "IQ3_S".into(),
        27 => "IQ3_M".into(),
        28 => "IQ2_S".into(),
        29 => "IQ2_M".into(),
        30 => "IQ4_XS".into(),
        31 => "IQ1_M".into(),
        32 => "BF16".into(),
        36 => "TQ1_0".into(),
        37 => "TQ2_0".into(),
        39 => "MXFP4".into(),
        other => format!("FT_{other}"),
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> EaiResult<&[u8]> {
        let end = self.pos.saturating_add(n);
        if end > self.bytes.len() {
            return Err(EaiError::config("truncated GGUF header"));
        }
        let s = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    fn u32(&mut self) -> EaiResult<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> EaiResult<u64> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    fn i64(&mut self) -> EaiResult<i64> {
        let b = self.take(8)?;
        Ok(i64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    fn f64(&mut self) -> EaiResult<f64> {
        Ok(f64::from_bits(self.u64()?))
    }

    fn string(&mut self) -> EaiResult<String> {
        let len =
            usize::try_from(self.u64()?).map_err(|_| EaiError::config("GGUF string too long"))?;
        let b = self.take(len)?;
        String::from_utf8(b.to_vec()).map_err(|_| EaiError::config("GGUF key is not UTF-8"))
    }

    fn value(&mut self, vtype: u32) -> EaiResult<Value> {
        Ok(match vtype {
            0 => Value::U64(u64::from(self.take(1)?[0])),
            1 => Value::I64(i64::from(self.take(1)?[0] as i8)),
            2 => {
                let b = self.take(2)?;
                Value::U64(u64::from(u16::from_le_bytes([b[0], b[1]])))
            }
            3 => {
                let b = self.take(2)?;
                Value::I64(i64::from(i16::from_le_bytes([b[0], b[1]])))
            }
            4 => Value::U64(u64::from(self.u32()?)),
            5 => Value::I64(i64::from(self.u32()? as i32)),
            6 => Value::F64(f64::from(self.u32()?)),
            7 => Value::Bool(self.take(1)?[0] != 0),
            8 => Value::Str(self.string()?),
            9 => {
                let elem_type = self.u32()?;
                let len = self.u64()?;
                self.skip_array(elem_type, len)?;
                Value::Array { elem_type, len }
            }
            10 => Value::U64(self.u64()?),
            11 => Value::I64(self.i64()?),
            12 => Value::F64(self.f64()?),
            other => {
                return Err(EaiError::config(format!(
                    "GGUF value type {other} unsupported"
                )))
            }
        })
    }

    fn skip_array(&mut self, elem_type: u32, len: u64) -> EaiResult<()> {
        let fixed = match elem_type {
            0 | 1 | 7 => 1u64,
            2..=3 => 2,
            4..=6 => 4,
            10..=12 => 8,
            // string arrays: walk each element
            8 => {
                for _ in 0..len {
                    let l = usize::try_from(self.u64()?)
                        .map_err(|_| EaiError::config("GGUF string too long"))?;
                    self.take(l)?;
                }
                return Ok(());
            }
            other => {
                return Err(EaiError::config(format!(
                    "GGUF array element type {other} unsupported"
                )))
            }
        };
        let n = fixed.checked_mul(len).and_then(|n| usize::try_from(n).ok());
        match n {
            Some(n) => {
                self.take(n)?;
                Ok(())
            }
            None => Err(EaiError::config("GGUF array too large")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal v3 header with the given key/value pairs.
    fn header(kvs: &[(&str, u32, &[u8])]) -> Vec<u8> {
        let mut b = b"GGUF".to_vec();
        b.extend_from_slice(&3u32.to_le_bytes());
        b.extend_from_slice(&0u64.to_le_bytes()); // tensor_count
        b.extend_from_slice(&(kvs.len() as u64).to_le_bytes());
        for (k, t, v) in kvs {
            b.extend_from_slice(&(k.len() as u64).to_le_bytes());
            b.extend_from_slice(k.as_bytes());
            b.extend_from_slice(&t.to_le_bytes());
            b.extend_from_slice(v);
        }
        b
    }

    fn s(v: &str) -> Vec<u8> {
        let mut b = (v.len() as u64).to_le_bytes().to_vec();
        b.extend_from_slice(v.as_bytes());
        b
    }

    fn llama_header() -> Vec<u8> {
        header(&[
            ("general.architecture", 8, &s("llama")),
            ("general.name", 8, &s("tiny-llama")),
            ("general.file_type", 4, &15u32.to_le_bytes()),
            ("llama.context_length", 4, &4096u32.to_le_bytes()),
            ("llama.block_count", 4, &22u32.to_le_bytes()),
            ("llama.embedding_length", 4, &2048u32.to_le_bytes()),
            ("llama.feed_forward_length", 4, &5632u32.to_le_bytes()),
            ("tokenizer.chat_template", 8, &s("{% for m in messages %}")),
        ])
    }

    #[test]
    fn gguf_inspector_reads_core_facts() {
        let info = inspect(&llama_header()).unwrap();
        assert_eq!(info.version, 3);
        assert_eq!(info.architecture.as_deref(), Some("llama"));
        assert_eq!(info.name.as_deref(), Some("tiny-llama"));
        assert_eq!(info.quantization.as_deref(), Some("Q4_K_M"));
        assert_eq!(info.context_length, Some(4096));
        assert_eq!(info.layer_count, Some(22));
        assert_eq!(info.embedding_length, Some(2048));
        assert_eq!(
            info.chat_template.as_deref(),
            Some("{% for m in messages %}")
        );
    }

    #[test]
    fn gguf_inspector_estimates_parameters() {
        let info = inspect(&llama_header()).unwrap();
        // 22*(4*2048² + 3*2048*5632) ≈ 22*(16.8M + 34.6M) ≈ 1.13B
        let p = info.parameter_count.unwrap();
        assert!(p > 1_000_000_000 && p < 1_300_000_000, "got {p}");
    }

    #[test]
    fn gguf_inspector_reads_vocab_from_token_array() {
        // array of 3 u8 elements as a stand-in token list
        let mut tok = 0u32.to_le_bytes().to_vec();
        tok.extend_from_slice(&3u64.to_le_bytes());
        tok.extend_from_slice(&[1, 2, 3]);
        let bytes = header(&[("tokenizer.ggml.tokens", 9, &tok)]);
        let info = inspect(&bytes).unwrap();
        assert_eq!(info.vocab_size, Some(3));
    }

    #[test]
    fn gguf_inspector_rejects_bad_magic() {
        assert!(inspect(b"NOPE....").is_err());
    }

    #[test]
    fn gguf_inspector_rejects_truncation() {
        let h = llama_header();
        assert!(inspect(&h[..h.len() - 4]).is_err());
    }

    #[test]
    fn gguf_inspector_v1_counts_are_u32() {
        let mut b = b"GGUF".to_vec();
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes()); // tensor_count u32
        b.extend_from_slice(&1u32.to_le_bytes()); // kv_count u32
        b.extend_from_slice(&(4u64).to_le_bytes());
        b.extend_from_slice(b"name");
        b.extend_from_slice(&8u32.to_le_bytes());
        b.extend_from_slice(&s("x"));
        let info = inspect(&b).unwrap();
        assert_eq!(info.version, 1);
    }

    #[test]
    fn gguf_inspector_handles_string_arrays() {
        let mut arr = 8u32.to_le_bytes().to_vec();
        arr.extend_from_slice(&2u64.to_le_bytes());
        arr.extend_from_slice(&s("a"));
        arr.extend_from_slice(&s("bb"));
        let bytes = header(&[("tokenizer.ggml.tokens", 9, &arr)]);
        let info = inspect(&bytes).unwrap();
        assert_eq!(info.vocab_size, Some(2));
    }
}
