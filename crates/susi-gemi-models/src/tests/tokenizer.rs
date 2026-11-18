use crate::ModelManager;
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
    assert!(ModelManager::valid_tokenizer(&path));
    fs::remove_file(path).unwrap();
}

#[test]
fn accepts_unigram_and_rejects_unrelated_json() {
    let unigram = write(
        "unigram",
        r#"{"version":"1.0","model":{"type":"Unigram","vocab":[["hello",-1.0]]}}"#,
    );
    assert!(ModelManager::valid_tokenizer(&unigram));
    fs::remove_file(unigram).unwrap();

    let unrelated = write("unrelated", r#"{"model":{"type":"BPE"}}"#);
    assert!(!ModelManager::valid_tokenizer(&unrelated));
    fs::remove_file(unrelated).unwrap();
}

#[test]
fn a_malformed_or_oversized_file_is_rejected() {
    let malformed = write("malformed", "not-json");
    assert!(!ModelManager::valid_tokenizer(&malformed));
    fs::remove_file(malformed).unwrap();
}
