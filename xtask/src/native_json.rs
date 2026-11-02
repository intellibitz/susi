//! JSON evidence must not have multiple interpretations of an object key.
//! Serde validates the complete JSON grammar; this bounded structural pass
//! rejects duplicate decoded keys before returning the parsed value.
use serde_json::Value;
use std::collections::BTreeSet;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub fn from_slice(bytes: &[u8]) -> Result<Value> {
    let value: Value = serde_json::from_slice(bytes)?;
    let mut scanner = Scanner { bytes, at: 0 };
    scanner.value(0)?;
    scanner.space();
    if scanner.at != bytes.len() {
        return Err("trailing JSON evidence bytes".into());
    }
    Ok(value)
}

struct Scanner<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Scanner<'_> {
    fn space(&mut self) {
        while self.bytes.get(self.at).is_some_and(u8::is_ascii_whitespace) {
            self.at += 1;
        }
    }

    fn eat(&mut self, expected: u8) -> Result<()> {
        self.space();
        if self.bytes.get(self.at) != Some(&expected) {
            return Err("invalid JSON evidence structure".into());
        }
        self.at += 1;
        Ok(())
    }

    fn string(&mut self) -> Result<String> {
        self.space();
        let start = self.at;
        self.eat(b'"')?;
        let mut escaped = false;
        while let Some(byte) = self.bytes.get(self.at) {
            self.at += 1;
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                return Ok(serde_json::from_slice(&self.bytes[start..self.at])?);
            }
        }
        Err("unterminated JSON evidence string".into())
    }

    fn value(&mut self, depth: usize) -> Result<()> {
        if depth > 128 {
            return Err("JSON evidence nesting limit exceeded".into());
        }
        self.space();
        match self.bytes.get(self.at).copied() {
            Some(b'{') => {
                self.at += 1;
                let mut keys = BTreeSet::new();
                self.space();
                if self.bytes.get(self.at) == Some(&b'}') {
                    self.at += 1;
                    return Ok(());
                }
                loop {
                    let key = self.string()?;
                    if !keys.insert(key) {
                        return Err("duplicate decoded JSON evidence key".into());
                    }
                    self.eat(b':')?;
                    self.value(depth + 1)?;
                    self.space();
                    if self.bytes.get(self.at) == Some(&b'}') {
                        self.at += 1;
                        return Ok(());
                    }
                    self.eat(b',')?;
                }
            }
            Some(b'[') => {
                self.at += 1;
                self.space();
                if self.bytes.get(self.at) == Some(&b']') {
                    self.at += 1;
                    return Ok(());
                }
                loop {
                    self.value(depth + 1)?;
                    self.space();
                    if self.bytes.get(self.at) == Some(&b']') {
                        self.at += 1;
                        return Ok(());
                    }
                    self.eat(b',')?;
                }
            }
            Some(b'"') => {
                self.string()?;
                Ok(())
            }
            Some(_) => {
                let start = self.at;
                while self
                    .bytes
                    .get(self.at)
                    .is_some_and(|b| !b.is_ascii_whitespace() && !b",]}".contains(b))
                {
                    self.at += 1;
                }
                if self.at == start {
                    return Err("empty JSON evidence value".into());
                }
                Ok(())
            }
            None => Err("missing JSON evidence value".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_rejects_duplicate_and_escaped_alias_keys_at_every_depth() {
        for bytes in [
            br#"{"source":1,"source":2}"#.as_slice(),
            br#"{"source":1,"sour\u0063e":2}"#.as_slice(),
            br#"{"nested":[{"x":1,"x":2}]}"#.as_slice(),
            r#"{"\uD834\uDD1E":1,"𝄞":2}"#.as_bytes(),
        ] {
            assert!(from_slice(bytes).is_err());
        }
    }

    #[test]
    fn evidence_accepts_separate_object_keys_and_literal_json_strings() {
        let bytes = br#"{"items":[{"key":1},{"key":2}],"escaped":"{\"key\":1,\"key\":2}","scalars":[null,true,false,-1,1.5]}"#;
        assert_eq!(from_slice(bytes).unwrap()["items"][1]["key"], 2);
    }

    #[test]
    fn evidence_rejects_malformed_trailing_and_excessively_nested_input() {
        for bytes in [b"{".as_slice(), b"{} false", b"{\"x\":1,}", b"[NaN]"] {
            assert!(from_slice(bytes).is_err());
        }
        let excessive = format!("{}0{}", "[".repeat(140), "]".repeat(140));
        assert!(from_slice(excessive.as_bytes()).is_err());
    }
}
