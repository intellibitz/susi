//! Reject duplicate object keys before typed decoding, including embedded
//! Cargo metadata whose dynamic JSON representation would otherwise lose them.
use serde::de::{DeserializeOwned, Error, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Number, Value};

struct Unique(Value);
struct UniqueVisitor;

impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(UniqueVisitor)
    }
}

impl<'de> Visitor<'de> for UniqueVisitor {
    type Value = Unique;
    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("JSON without duplicate object keys")
    }
    fn visit_unit<E: Error>(self) -> Result<Unique, E> {
        Ok(Unique(Value::Null))
    }
    fn visit_bool<E: Error>(self, value: bool) -> Result<Unique, E> {
        Ok(Unique(Value::Bool(value)))
    }
    fn visit_i64<E: Error>(self, value: i64) -> Result<Unique, E> {
        Ok(Unique(Value::Number(value.into())))
    }
    fn visit_u64<E: Error>(self, value: u64) -> Result<Unique, E> {
        Ok(Unique(Value::Number(value.into())))
    }
    fn visit_f64<E: Error>(self, value: f64) -> Result<Unique, E> {
        Number::from_f64(value)
            .map(|n| Unique(Value::Number(n)))
            .ok_or_else(|| E::custom("non-finite JSON number"))
    }
    fn visit_str<E: Error>(self, value: &str) -> Result<Unique, E> {
        Ok(Unique(Value::String(value.into())))
    }
    fn visit_string<E: Error>(self, value: String) -> Result<Unique, E> {
        Ok(Unique(Value::String(value)))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut access: A) -> Result<Unique, A::Error> {
        let mut values = Vec::new();
        while let Some(Unique(value)) = access.next_element()? {
            values.push(value);
        }
        Ok(Unique(Value::Array(values)))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Unique, A::Error> {
        let mut values = Map::new();
        while let Some(key) = access.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(A::Error::custom(format!(
                    "duplicate JSON object key: {key}"
                )));
            }
            let Unique(value) = access.next_value()?;
            values.insert(key, value);
        }
        Ok(Unique(Value::Object(values)))
    }
}

pub(crate) fn parse<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    let Unique(value) = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    serde_json::from_value(value).map_err(|e| e.to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, PartialEq, Deserialize)]
    struct Sample {
        a: u32,
        b: Vec<String>,
    }

    #[test]
    fn parse_accepts_unique_keys_into_typed_values() {
        let parsed: Sample = parse(br#"{"a": 7, "b": ["x", "y"]}"#).unwrap();
        assert_eq!(
            parsed,
            Sample {
                a: 7,
                b: vec!["x".into(), "y".into()]
            }
        );
    }

    #[test]
    fn parse_rejects_top_level_duplicate_key_naming_the_key() {
        let err = parse::<Value>(br#"{"a": 1, "a": 2}"#).unwrap_err();
        assert!(err.contains("duplicate JSON object key: a"), "{err}");
    }

    #[test]
    fn parse_rejects_duplicate_key_nested_in_array_and_object() {
        let nested = parse::<Value>(br#"{"outer": [{"k": 1, "k": 2}]}"#).unwrap_err();
        assert!(nested.contains("duplicate JSON object key: k"), "{nested}");
        let deep = parse::<Value>(br#"{"x": {"y": {"z": 1, "z": 1}}}"#).unwrap_err();
        assert!(deep.contains("duplicate JSON object key: z"), "{deep}");
    }

    #[test]
    fn parse_allows_same_key_in_sibling_objects() {
        let value: Value = parse(br#"[{"k": 1}, {"k": 2}]"#).unwrap();
        assert_eq!(value, serde_json::json!([{"k": 1}, {"k": 2}]));
    }

    #[test]
    fn parse_preserves_every_json_scalar_kind() {
        let value: Value =
            parse(br#"{"n": null, "t": true, "f": false, "neg": -5, "big": 18446744073709551615, "fl": 1.5, "s": "text"}"#)
                .unwrap();
        assert_eq!(value["n"], Value::Null);
        assert_eq!(value["t"], Value::Bool(true));
        assert_eq!(value["f"], Value::Bool(false));
        assert_eq!(value["neg"], serde_json::json!(-5));
        assert_eq!(value["big"], serde_json::json!(u64::MAX));
        assert_eq!(value["fl"], serde_json::json!(1.5));
        assert_eq!(value["s"], serde_json::json!("text"));
    }

    #[test]
    fn parse_reports_malformed_input_and_type_mismatch() {
        assert!(parse::<Value>(b"{\"a\": 1} trailing").is_err());
        assert!(parse::<Value>(b"{\"a\": ").is_err());
        assert!(parse::<Value>(b"").is_err());
        // Valid JSON but the wrong shape for the requested type.
        assert!(parse::<Sample>(br#"{"a": "not-a-number", "b": []}"#).is_err());
        assert!(parse::<Sample>(br#"{"a": 1}"#).is_err());
    }
}
