// SPDX-License-Identifier: Apache-2.0
//! Serde adapters for the shared lossless native-path wire contract.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use std::path::{Path, PathBuf};
use susi_paths::native_path::{decode, encode};

pub fn serialize<S: Serializer>(path: &Path, serializer: S) -> Result<S::Ok, S::Error> {
    encode(path)
        .map_err(serde::ser::Error::custom)?
        .serialize(serializer)
}

pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<PathBuf, D::Error> {
    decode(&Value::deserialize(deserializer)?).map_err(serde::de::Error::custom)
}

/// Pair `with = "...::option"` with `default` to retain missing-field behavior.
pub mod option {
    use super::*;

    pub fn serialize<S: Serializer>(
        path: &Option<PathBuf>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        path.as_deref()
            .map(encode)
            .transpose()
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<PathBuf>, D::Error> {
        Option::<Value>::deserialize(deserializer)?
            .as_ref()
            .map(decode)
            .transpose()
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Paths {
        #[serde(with = "super")]
        path: PathBuf,
        #[serde(default, with = "super::option")]
        optional: Option<PathBuf>,
    }

    #[test]
    fn strings_null_and_missing_optional_fields_keep_serde_behavior() {
        let paths = Paths {
            path: "plain/path".into(),
            optional: None,
        };
        assert_eq!(
            serde_json::to_value(&paths).unwrap(),
            json!({"path":"plain/path","optional":null})
        );
        assert_eq!(
            serde_json::from_value::<Paths>(json!({"path":"plain/path"})).unwrap(),
            paths
        );
        assert!(serde_json::from_value::<Paths>(json!({"path":null})).is_err());
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn required_and_optional_native_paths_roundtrip_through_serde() {
        #[cfg(unix)]
        let wire = json!({"platform":"unix","units":[112,255]});
        #[cfg(windows)]
        let wire = json!({"platform":"windows","units":[112,55296]});
        let paths = Paths {
            path: decode(&wire).unwrap(),
            optional: Some(decode(&wire).unwrap()),
        };
        let json = json!({"path":wire,"optional":wire});
        assert_eq!(serde_json::to_value(&paths).unwrap(), json);
        assert_eq!(serde_json::from_value::<Paths>(json).unwrap(), paths);
    }
}
