//! Bounded structural validation for Hugging Face `tokenizer.json` files.
//!
//! The inference plug owns tokenization at runtime. Core still has to reject
//! truncated or unrelated JSON before it publishes a downloaded tokenizer or
//! admits a local model, so this validator checks the stable serialization
//! contract without pulling the full tokenizer runtime into the core binary.

use serde_json::Value;
use std::fs::File;
use std::io::Read;
use std::path::Path;

const MAX_TOKENIZER_BYTES: u64 = 128 * 1024 * 1024;

/// Return `None` only when the file could not be read; invalid content is a
/// durable negative verdict and returns `Some(false)`.
pub(crate) fn judge(path: &Path) -> Option<bool> {
    let file = File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    if length == 0 || length > MAX_TOKENIZER_BYTES {
        return Some(false);
    }
    let mut bytes = Vec::with_capacity(length as usize);
    let read = file
        .take(MAX_TOKENIZER_BYTES.saturating_add(1))
        .read_to_end(&mut bytes);
    if read.is_err() {
        return None;
    }
    if bytes.len() as u64 != length || bytes.len() as u64 > MAX_TOKENIZER_BYTES {
        return Some(false);
    }
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => return Some(false),
    };
    Some(is_tokenizer_document(&value))
}

pub(crate) fn valid(path: &Path) -> bool {
    judge(path).unwrap_or(false)
}

fn is_tokenizer_document(value: &Value) -> bool {
    let Some(root) = value.as_object() else {
        return false;
    };
    if root.get("version").and_then(Value::as_str) != Some("1.0") {
        return false;
    }
    let Some(model) = root.get("model").and_then(Value::as_object) else {
        return false;
    };
    let Some(model_type) = model.get("type").and_then(Value::as_str) else {
        return false;
    };
    let vocab_valid = match model_type {
        "BPE" | "WordPiece" | "WordLevel" => model
            .get("vocab")
            .and_then(Value::as_object)
            .is_some_and(valid_object_vocab),
        "Unigram" => model
            .get("vocab")
            .and_then(Value::as_array)
            .is_some_and(|vocab| valid_unigram_vocab(vocab)),
        _ => false,
    };
    if !vocab_valid {
        return false;
    }
    if model_type == "BPE"
        && !model
            .get("merges")
            .and_then(Value::as_array)
            .is_some_and(|merges| !merges.is_empty())
    {
        return false;
    }
    root.get("added_tokens").is_none_or(valid_added_tokens)
}

fn valid_object_vocab(vocab: &serde_json::Map<String, Value>) -> bool {
    !vocab.is_empty()
        && vocab.iter().all(|(token, id)| {
            !token.is_empty() && id.as_u64().is_some_and(|id| u32::try_from(id).is_ok())
        })
}

fn valid_unigram_vocab(vocab: &[Value]) -> bool {
    !vocab.is_empty()
        && vocab.iter().all(|entry| {
            let Some(pair) = entry.as_array() else {
                return false;
            };
            pair.len() == 2
                && pair[0].as_str().is_some_and(|token| !token.is_empty())
                && pair[1].as_f64().is_some()
        })
}

fn valid_added_tokens(tokens: &Value) -> bool {
    tokens.as_array().is_some_and(|tokens| {
        tokens.iter().all(|token| {
            let Some(token) = token.as_object() else {
                return false;
            };
            token
                .get("id")
                .and_then(Value::as_u64)
                .is_some_and(|id| u32::try_from(id).is_ok())
                && token.get("content").and_then(Value::as_str).is_some()
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "susi-tokenizer-{label}-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn write(label: &str, value: &str) -> PathBuf {
        let path = path(label);
        fs::write(&path, value).unwrap();
        path
    }

    #[test]
    fn accepts_a_serialized_bpe_tokenizer() {
        let path = write(
            "bpe",
            r#"{"version":"1.0","model":{"type":"BPE","vocab":{"hello":0},"merges":["h e"]},"added_tokens":[]}"#,
        );
        assert_eq!(judge(&path), Some(true));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn accepts_unigram_and_rejects_unrelated_json() {
        let unigram = write(
            "unigram",
            r#"{"version":"1.0","model":{"type":"Unigram","vocab":[["hello",-1.0]]}}"#,
        );
        assert_eq!(judge(&unigram), Some(true));
        fs::remove_file(unigram).unwrap();

        let unrelated = write("unrelated", r#"{"model":{"type":"BPE"}}"#);
        assert_eq!(judge(&unrelated), Some(false));
        fs::remove_file(unrelated).unwrap();
    }

    #[test]
    fn a_malformed_or_oversized_file_is_rejected() {
        let malformed = write("malformed", "not-json");
        assert_eq!(judge(&malformed), Some(false));
        fs::remove_file(malformed).unwrap();
    }
}
