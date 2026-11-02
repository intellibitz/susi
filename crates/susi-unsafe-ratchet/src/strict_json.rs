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
