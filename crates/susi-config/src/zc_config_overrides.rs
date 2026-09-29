//! Config diff: only user overrides vs bundled defaults (goal: empty).

use serde_json::Value;
use std::collections::BTreeMap;

/// Recursively collect dotted paths where `user` differs from `defaults`.
#[must_use]
pub fn user_overrides(defaults: &Value, user: &Value) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    diff_into("", defaults, user, &mut out);
    out
}

fn diff_into(prefix: &str, defaults: &Value, user: &Value, out: &mut BTreeMap<String, Value>) {
    match (defaults, user) {
        (Value::Object(d), Value::Object(u)) => {
            for (k, uv) in u {
                let path = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                match d.get(k) {
                    Some(dv) => diff_into(&path, dv, uv, out),
                    None => {
                        out.insert(path, uv.clone());
                    }
                }
            }
        }
        (d, u) if d != u => {
            out.insert(prefix.to_string(), u.clone());
        }
        _ => {}
    }
}

/// Render a human-readable diff; empty string when no overrides.
#[must_use]
pub fn format_overrides(overrides: &BTreeMap<String, Value>) -> String {
    if overrides.is_empty() {
        return String::new();
    }
    let mut lines = Vec::new();
    for (k, v) in overrides {
        lines.push(format!("{k} = {v}"));
    }
    lines.join("\n")
}

#[cfg(test)]
mod zc_config_overrides_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn zc_config_overrides_empty_when_equal() {
        let d = json!({"a": 1, "b": {"c": true}});
        let o = user_overrides(&d, &d);
        assert!(o.is_empty());
        assert_eq!(format_overrides(&o), "");
    }

    #[test]
    fn zc_config_overrides_lists_only_user_changes() {
        let d = json!({"a": 1, "b": {"c": true, "d": 2}});
        let u = json!({"a": 1, "b": {"c": false, "d": 2}});
        let o = user_overrides(&d, &u);
        assert_eq!(o.len(), 1);
        assert_eq!(o.get("b.c"), Some(&json!(false)));
        assert!(format_overrides(&o).contains("b.c"));
    }
}
