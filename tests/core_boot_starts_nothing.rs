//! Retained generic core never restores boot-time engine awakening.
use std::path::Path;
#[test]
fn the_engine_awakening_function_is_gone_from_core() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut stack = vec![root.join("src"), root.join("crates")];
    let mut offenders = Vec::new();
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                if text.contains("fn try_awaken_local_engines") {
                    offenders.push(path.display().to_string());
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "no boot-time engine awakening may return: {offenders:?}"
    );
}
