// SPDX-License-Identifier: Apache-2.0
//! JSON parsing with bounded protocol-envelope nesting headroom.

use serde::Deserialize;
use serde_json::Value;

/// Maximum container nesting accepted by this protocol parser.
pub const MAX_DEPTH: usize = 160;

/// Parse one JSON value after independently bounding its lexical nesting.
///
/// This does not change the ordinary parser's recursion limit or authorize
/// deeper tool inputs. Callers retain their native argument admission checks.
pub fn parse(bytes: &[u8]) -> Result<Value, String> {
    let (mut depth, mut quoted, mut escaped) = (0_usize, false, false);
    for &byte in bytes {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
            continue;
        }
        match byte {
            b'"' => quoted = true,
            b'[' | b'{' => {
                depth = depth
                    .checked_add(1)
                    .filter(|depth| *depth <= MAX_DEPTH)
                    .ok_or_else(|| "protocol JSON depth limit exceeded".to_owned())?;
            }
            b']' | b'}' => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| "unmatched protocol JSON closing delimiter".to_owned())?;
            }
            _ => {}
        }
    }
    if quoted || depth != 0 {
        return Err("incomplete protocol JSON value".to_owned());
    }
    // Only this deserializer relaxes serde's default limit, after the separate
    // scan has capped every container regardless of the eventual value shape.
    let mut parser = serde_json::Deserializer::from_slice(bytes);
    parser.disable_recursion_limit();
    let value = Value::deserialize(&mut parser).map_err(|error| error.to_string())?;
    parser.end().map_err(|error| error.to_string())?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn arrays(depth: usize) -> String {
        format!("{}0{}", "[".repeat(depth), "]".repeat(depth))
    }

    #[test]
    fn protocol_wrappers_preserve_admitted_depths_124_through_126() {
        for depth in [124, 125, 126] {
            let native = format!("{{\"unused\":{}}}", arrays(depth));
            let expected: Value = serde_json::from_str(&native).unwrap();
            let wire = format!(
                "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{{\"name\":\"fixture\",\"arguments\":{{\"arguments\":{native}}}}}}}"
            );
            assert!(serde_json::from_str::<Value>(&wire).is_err());
            assert_eq!(
                parse(wire.as_bytes()).unwrap()["params"]["arguments"]["arguments"],
                expected
            );
        }
    }

    #[test]
    fn transport_headroom_does_not_relax_native_128_container_admission() {
        for depth in [127, 128] {
            let native = format!("{{\"unused\":{}}}", arrays(depth));
            assert!(serde_json::from_str::<Value>(&native).is_err());
            assert!(parse(native.as_bytes()).is_ok());
        }
        assert!(serde_json::from_str::<Value>(&arrays(127)).is_ok());
        assert!(serde_json::from_str::<Value>(&arrays(128)).is_err());
    }

    #[test]
    fn lexical_depth_160_is_accepted_and_161_is_rejected_before_deserialization() {
        assert!(parse(arrays(MAX_DEPTH).as_bytes()).is_ok());
        let mut objects = "0".to_owned();
        for _ in 0..MAX_DEPTH {
            objects = format!("{{\"v\":{objects}}}");
        }
        assert!(parse(objects.as_bytes()).is_ok());
        for bytes in [arrays(MAX_DEPTH + 1), "[".repeat(MAX_DEPTH + 1)] {
            assert!(parse(bytes.as_bytes()).unwrap_err().contains("depth limit"));
        }
    }

    #[test]
    fn quoted_delimiters_and_escaped_quotes_and_backslashes_do_not_add_depth() {
        for count in 0..9 {
            let value = json!({"text": format!("{}\"{}", "\\".repeat(count), "[{}]".repeat(200))});
            assert_eq!(parse(&serde_json::to_vec(&value).unwrap()).unwrap(), value);
            let outside = format!("\"{}\"{}", "\\\\".repeat(count), "[".repeat(MAX_DEPTH + 1));
            assert!(parse(outside.as_bytes())
                .unwrap_err()
                .contains("depth limit"));
        }
        assert_eq!(parse(br#""\u0022\u005c[[[""#).unwrap(), json!("\"\\[[["));
    }

    #[test]
    fn balanced_counts_do_not_replace_syntax_and_end_validation() {
        for bytes in [
            b"".as_slice(),
            b"]",
            b"[}",
            b"{\"x\":]",
            b"[1,]",
            b"true false",
            b"{}[]",
            b"\"unterminated\\\"",
            b"\"\\x\"",
            b"\"\xff\"",
        ] {
            assert!(parse(bytes).is_err());
        }
        assert!(parse("[".repeat(MAX_DEPTH).as_bytes()).is_err());
        let malformed = format!("{}{{]}}{}", "[".repeat(158), "]".repeat(158));
        assert!(parse(malformed.as_bytes()).is_err());
        for value in [
            Value::Null,
            json!(true),
            json!(4),
            json!(""),
            json!([]),
            json!({}),
        ] {
            assert_eq!(parse(&serde_json::to_vec(&value).unwrap()).unwrap(), value);
        }
    }
}
