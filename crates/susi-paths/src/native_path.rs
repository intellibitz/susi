// SPDX-License-Identifier: Apache-2.0
//! Lossless native paths in JSON: UTF-8 strings, or platform-tagged raw units.
//!
//! Native objects contain exactly `platform` and `units`. Unix units are bytes;
//! Windows units are UTF-16 code units, including unpaired surrogates. A native
//! object from another platform is rejected. No path normalization is performed.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub fn encode(path: &Path) -> Result<Value, String> {
    if let Some(text) = path.to_str() {
        return Ok(Value::String(text.to_owned()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(json!({"platform":"unix", "units":path.as_os_str().as_bytes()}))
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let units: Vec<u16> = path.as_os_str().encode_wide().collect();
        Ok(json!({"platform":"windows", "units":units}))
    }
    #[cfg(not(any(unix, windows)))]
    Err("native path encoding is unavailable on this platform".into())
}

pub fn decode(value: &Value) -> Result<PathBuf, String> {
    if let Some(text) = value.as_str() {
        return Ok(PathBuf::from(text));
    }
    let object = value
        .as_object()
        .ok_or("path must be a string or native path object")?;
    if object.len() != 2 || !object.contains_key("platform") || !object.contains_key("units") {
        return Err("native path requires exactly platform and units".into());
    }
    let platform = object["platform"]
        .as_str()
        .ok_or("native path platform must be a string")?;
    let units = object["units"]
        .as_array()
        .ok_or("native path units must be an array")?;
    #[cfg(unix)]
    if platform == "unix" {
        use std::os::unix::ffi::OsStringExt;
        return Ok(std::ffi::OsString::from_vec(decode_units(units)?).into());
    }
    #[cfg(windows)]
    if platform == "windows" {
        use std::os::windows::ffi::OsStringExt;
        return Ok(std::ffi::OsString::from_wide(&decode_units::<u16>(units)?).into());
    }
    #[cfg(not(any(unix, windows)))]
    let _ = (platform, units);
    Err("native path platform does not match this host".into())
}

#[cfg(any(unix, windows))]
fn decode_units<T: TryFrom<u64>>(units: &[Value]) -> Result<Vec<T>, String> {
    units
        .iter()
        .map(|unit| {
            unit.as_u64()
                .and_then(|number| T::try_from(number).ok())
                .ok_or_else(|| "native path unit is not an in-range unsigned integer".into())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_paths_keep_the_existing_string_wire_form() {
        for text in [
            "",
            ".",
            "relative/.././path",
            "directory/é",
            "C:\\directory\\file",
            "nul\0path",
        ] {
            assert_eq!(encode(Path::new(text)).unwrap(), json!(text));
            assert_eq!(
                decode(&json!(text)).unwrap().as_os_str(),
                std::ffi::OsStr::new(text)
            );
        }
    }

    #[test]
    fn malformed_native_objects_are_rejected_without_coercion() {
        for value in [
            json!(null),
            json!(false),
            json!([]),
            json!(1),
            json!({}),
            json!({"platform":"unix"}),
            json!({"units":[]}),
            json!({"platform":"unix","units":[],"extra":true}),
            json!({"platform":false,"units":[]}),
            json!({"platform":"unix","units":0}),
            json!({"platform":"unknown","units":[]}),
        ] {
            assert!(decode(&value).is_err(), "{value}");
        }
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn native_unit_numbers_are_validated_before_path_construction() {
        let platform = if cfg!(unix) { "unix" } else { "windows" };
        let overflow = if cfg!(unix) { 256 } else { 65536 };
        for unit in [
            json!(null),
            json!(true),
            json!("1"),
            json!({}),
            json!(-1),
            json!(1.5),
            json!(overflow),
        ] {
            assert!(decode(&json!({"platform":platform,"units":[unit]})).is_err());
        }
        let other = if cfg!(unix) { "windows" } else { "unix" };
        assert!(decode(&json!({"platform":other,"units":[65]})).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn unix_bytes_roundtrip_without_utf8_replacement_or_normalization() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let bytes = b"repo-\xff/\xfe\0".to_vec();
        let path = PathBuf::from(std::ffi::OsString::from_vec(bytes.clone()));
        assert!(path.to_str().is_none());
        let wire = encode(&path).unwrap();
        assert_eq!(wire, json!({"platform":"unix","units":bytes}));
        assert_eq!(decode(&wire).unwrap().as_os_str().as_bytes(), bytes);
    }

    #[cfg(windows)]
    #[test]
    fn windows_unpaired_surrogates_roundtrip_as_native_wide_units() {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let units = [0x43, 0x3a, 0x5c, 0xd800, 0x5c, 0xdc00, 0];
        let path = PathBuf::from(std::ffi::OsString::from_wide(&units));
        assert!(path.to_str().is_none());
        let wire = encode(&path).unwrap();
        assert_eq!(wire, json!({"platform":"windows","units":units}));
        assert_eq!(
            decode(&wire)
                .unwrap()
                .as_os_str()
                .encode_wide()
                .collect::<Vec<_>>(),
            units
        );
    }
}
