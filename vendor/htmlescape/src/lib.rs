//! Independent HTML text escaping for Susi's internal Tantivy snippet consumer.
//!
//! This deliberately supplies only `encode_minimal`; it is not a general API
//! replacement for the upstream htmlescape package. It embeds no external data.
#![forbid(unsafe_code)]

/// Escape the five ASCII characters needed by the internal snippet renderer.
/// Every other Unicode scalar is preserved, without normalization or decoding.
pub fn encode_minimal(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '"' => output.push_str("&quot;"),
            '&' => output.push_str("&amp;"),
            '\'' => output.push_str("&#x27;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            _ => output.push(ch),
        }
    }
    output
}
