//! Generic core retains the original no-unrequested-download-path guard.
//! Full lifecycle, routing, explicit-request and process checks follow their owners
//! in commercial crates/susi-cli-runtime/tests/core_never_downloads_models_unasked.rs.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // Original test assertions.
use std::path::Path;

fn try_block_end(src: &str, open: usize) -> Option<usize> {
    let bytes = src.as_bytes();
    if bytes.get(open) != Some(&b'{') {
        return None;
    }
    // Braces inside string literals, char literals and comments do not count
    // (e.g. a test that writes "{corrupt json" must not unbalance its module).
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 1;
            }
            b'r' if matches!(bytes.get(i + 1), Some(b'"') | Some(b'#'))
                && (i == 0 || !is_ident(bytes[i - 1])) =>
            {
                let mut hashes = 0;
                let mut j = i + 1;
                while bytes.get(j) == Some(&b'#') {
                    hashes += 1;
                    j += 1;
                }
                if bytes.get(j) == Some(&b'"') {
                    let close: Vec<u8> = std::iter::once(b'"')
                        .chain(std::iter::repeat_n(b'#', hashes))
                        .collect();
                    j += 1;
                    while j < bytes.len() && !bytes[j..].starts_with(&close) {
                        j += 1;
                    }
                    i = j + close.len() - 1;
                }
            }
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'\'' => {
                // A char literal like '{' or '\''; a lifetime has no closing quote nearby.
                if bytes.get(i + 2) == Some(&b'\'') {
                    i += 2;
                } else if bytes.get(i + 1) == Some(&b'\\') {
                    if let Some(off) = bytes[i + 2..].iter().take(10).position(|&c| c == b'\'') {
                        i += 2 + off;
                    }
                }
            }
            b'{' => depth += 1,
            b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn test_module_ranges(src: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for (at, _) in src.match_indices("#[cfg(test)]") {
        let rest = &src[at..];
        let Some(mod_at) = rest.find("mod ") else {
            continue;
        };
        // Only further attributes (e.g. `#[allow(clippy::unwrap_used)]`) or comments may sit
        // between `#[cfg(test)]` and `mod`; anything else means the cfg is not
        // on this module.
        let between = &rest["#[cfg(test)]".len()..mod_at];
        let only_attributes = between
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .all(|line| (line.starts_with("#[") && line.ends_with(']')) || line.starts_with("//"));
        if !only_attributes {
            continue;
        }
        let Some(open) = rest[mod_at..].find(['{', ';']) else {
            continue;
        };
        let open = at + mod_at + open;
        // Unbalanced (braces in literals): keep the module, over-approximating.
        if let Some(end) = try_block_end(src, open) {
            out.push((at, end));
        }
    }
    out
}

fn without_test_modules(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut from = 0;
    for (start, end) in test_module_ranges(src) {
        if start >= from {
            out.push_str(&src[from..start]);
            from = end;
        }
    }
    out.push_str(&src[from..]);
    out
}

fn code_only(body: &str) -> String {
    body.lines()
        .map(|line| {
            if line.trim_start().starts_with("//") {
                ""
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn first_party_code() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut stack = vec![root.join("src"), root.join("crates")];
    let mut out = Vec::new();
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // Integration tests and fixtures may exercise the explicit
                // commands; only shipped code is pinned here.
                if path.file_name().is_some_and(|n| n == "tests") {
                    continue;
                }
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                out.push((rel, code_only(&without_test_modules(&text))));
            }
        }
    }
    out
}

#[test]
fn the_unrequested_download_paths_are_gone() {
    let mut found = Vec::new();
    for (rel, code) in first_party_code() {
        for gone in [
            "fn spawn_background_hardware_model_provisioner",
            "fn provision_and_retry",
            "fn prepare_hardware_optimal_models",
            "fn retry_after_provisioning",
        ] {
            if code.contains(gone) {
                found.push(format!("{rel}: {gone}"));
            }
        }
    }
    assert!(
        found.is_empty(),
        "removed download paths returned: {found:?}"
    );
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}
