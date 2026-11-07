//! Standard-library JSON Schema (draft 2020-12 subset) validation for tool
//! input and output schemas.
//!
//! Until 2026-11-07 GMCP compiled schemas through the `jsonschema` crate,
//! which brought 29 third-party crates into the four pillars' dependency
//! reach for a keyword set the tool catalog never used. This validator
//! implements exactly the keywords Susi's tool schemas, the MCP protocol
//! schemas and the extension packs use, and **fails closed at compile time**
//! on any other validation keyword: a schema that needs something else is
//! rejected when the tool registers, never silently accepted at call time.
//!
//! Supported: `type` (one or many; `integer` is a number with no fractional
//! part), `properties`, `required`, `additionalProperties` (bool or schema),
//! `items`, `prefixItems`, `minItems`, `maxItems`, `uniqueItems`, `enum`,
//! `const`, `minimum`, `maximum`, `exclusiveMinimum`, `exclusiveMaximum`,
//! `multipleOf`, `minLength`, `maxLength` (characters), `propertyNames`,
//! `minProperties`, `maxProperties`, `anyOf`, `oneOf`, `allOf`, `not`,
//! boolean schemas, and `$ref` to a local `#/$defs/...`, `#/definitions/...`
//! or `#` pointer (JSON Pointer escapes honored; depth-bounded, so cycles
//! fail rather than loop). Annotations (`title`, `description`, `default`,
//! `examples`, `format`, `$schema`, `$id`, `$comment`, `deprecated`,
//! `readOnly`, `writeOnly`, `$defs`, `definitions`) and `x-`-prefixed
//! vendor extension keywords are accepted and ignored, as draft 2020-12 vocabulary specifies for `format` without the
//! format-assertion vocabulary. `pattern` and `patternProperties` are not
//! implemented (no tool schema uses them; a regular-expression engine is a
//! dependency this crate does not carry) and are rejected at compile time.
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::fmt;

/// Deepest `$ref` chain and nesting a validation follows before it gives up.
const MAX_DEPTH: usize = 64;

const ANNOTATIONS: &[&str] = &[
    "title",
    "description",
    "default",
    "examples",
    "format",
    "$schema",
    "$id",
    "$comment",
    "deprecated",
    "readOnly",
    "writeOnly",
    "$defs",
    "definitions",
];

const KEYWORDS: &[&str] = &[
    "type",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "prefixItems",
    "minItems",
    "maxItems",
    "uniqueItems",
    "enum",
    "const",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "multipleOf",
    "minLength",
    "maxLength",
    "propertyNames",
    "minProperties",
    "maxProperties",
    "anyOf",
    "oneOf",
    "allOf",
    "not",
    "$ref",
];

/// One validation failure: where in the instance (a JSON Pointer, `""` for
/// the root) and what was expected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub instance_path: String,
    pub message: String,
}

impl ValidationError {
    /// The instance location as a JSON Pointer (`/items/0/name`).
    pub fn instance_path(&self) -> &str {
        &self.instance_path
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// One validation pass: the failures so far and whether to stop at the first.
struct Walk {
    errors: Vec<ValidationError>,
    first_only: bool,
    depth: usize,
}

/// A compiled schema: checked for supported keywords and well-typed keyword
/// values.
#[derive(Debug)]
pub struct Validator {
    root: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileError(String);

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CompileError {}

impl Validator {
    /// Compile `schema`. Every subschema is inspected: an unsupported
    /// validation keyword, a mistyped keyword value, an unresolvable local
    /// `$ref` or a remote `$ref` is an error.
    pub fn compile(schema: &Value) -> Result<Self, CompileError> {
        check_schema(schema, schema, "", 0)?;
        Ok(Self {
            root: schema.clone(),
        })
    }

    /// Whether `instance` satisfies the schema.
    pub fn is_valid(&self, instance: &Value) -> bool {
        let mut walk = Walk {
            errors: Vec::new(),
            first_only: true,
            depth: 0,
        };
        self.validate(&self.root, instance, "", &mut walk);
        walk.errors.is_empty()
    }

    /// Every failure, in document order (short-circuits inside `anyOf` /
    /// `oneOf` branches only).
    pub fn iter_errors(&self, instance: &Value) -> Vec<ValidationError> {
        let mut walk = Walk {
            errors: Vec::new(),
            first_only: false,
            depth: 0,
        };
        self.validate(&self.root, instance, "", &mut walk);
        walk.errors
    }

    /// Validate `instance` against `schema`, recording failures in `walk`
    /// (stopping at the first one when it asks). Returns whether the
    /// instance is valid.
    fn validate(&self, schema: &Value, instance: &Value, path: &str, walk: &mut Walk) -> bool {
        if walk.depth > MAX_DEPTH {
            walk.errors
                .push(fail(path, "schema nesting exceeds the supported depth"));
            return false;
        }
        walk.depth += 1;
        let valid = self.validate_in(schema, instance, path, walk);
        walk.depth -= 1;
        valid
    }

    fn validate_in(&self, schema: &Value, instance: &Value, path: &str, walk: &mut Walk) -> bool {
        let first_only = walk.first_only;
        let map = match schema {
            Value::Bool(true) => return true,
            Value::Bool(false) => {
                walk.errors.push(fail(path, "schema rejects every value"));
                return false;
            }
            Value::Object(map) => map,
            Value::Null | Value::Number(_) | Value::String(_) | Value::Array(_) => {
                walk.errors.push(fail(path, "schema is not an object"));
                return false;
            }
        };
        let mut ok = true;
        macro_rules! check {
            ($cond:expr, $msg:expr) => {{
                let passed: bool = $cond;
                if !passed {
                    ok = false;
                    walk.errors.push(fail(path, $msg));
                    if first_only {
                        return false;
                    }
                }
            }};
        }

        if let Some(reference) = map.get("$ref").and_then(Value::as_str) {
            match resolve(&self.root, reference) {
                Some(target) => {
                    if !self.validate(target, instance, path, walk) {
                        ok = false;
                        if first_only {
                            return false;
                        }
                    }
                }
                None => check!(false, format!("unresolvable $ref {reference}")),
            }
        }
        if let Some(expected) = map.get("type") {
            let matches = match expected {
                Value::String(name) => has_type(instance, name),
                Value::Array(names) => names
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|name| has_type(instance, name)),
                Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_) => false,
            };
            check!(
                matches,
                format!(
                    "{} is not of type {}",
                    describe(instance),
                    type_names(expected)
                )
            );
        }
        if let Some(allowed) = map.get("enum").and_then(Value::as_array) {
            check!(
                allowed.iter().any(|v| v == instance),
                format!("{} is not one of the allowed values", describe(instance))
            );
        }
        if let Some(expected) = map.get("const") {
            check!(
                expected == instance,
                format!("{} is not the required constant", describe(instance))
            );
        }

        if let Some(number) = instance.as_f64() {
            if let Some(min) = map.get("minimum").and_then(Value::as_f64) {
                check!(
                    number >= min,
                    format!("{number} is less than the minimum of {min}")
                );
            }
            if let Some(max) = map.get("maximum").and_then(Value::as_f64) {
                check!(
                    number <= max,
                    format!("{number} is greater than the maximum of {max}")
                );
            }
            if let Some(min) = map.get("exclusiveMinimum").and_then(Value::as_f64) {
                check!(
                    number > min,
                    format!("{number} is less than or equal to the minimum of {min}")
                );
            }
            if let Some(max) = map.get("exclusiveMaximum").and_then(Value::as_f64) {
                check!(
                    number < max,
                    format!("{number} is greater than or equal to the maximum of {max}")
                );
            }
            if let Some(step) = map.get("multipleOf").and_then(Value::as_f64) {
                let quotient = number / step;
                check!(
                    step > 0.0 && (quotient - quotient.round()).abs() < 1e-9,
                    format!("{number} is not a multiple of {step}")
                );
            }
        }

        if let Some(text) = instance.as_str() {
            let chars = text.chars().count();
            if let Some(min) = map.get("minLength").and_then(Value::as_u64) {
                check!(
                    chars as u64 >= min,
                    format!("{} is shorter than {min} characters", describe(instance))
                );
            }
            if let Some(max) = map.get("maxLength").and_then(Value::as_u64) {
                check!(
                    chars as u64 <= max,
                    format!("{} is longer than {max} characters", describe(instance))
                );
            }
        }

        if let Some(items) = instance.as_array() {
            if let Some(min) = map.get("minItems").and_then(Value::as_u64) {
                check!(
                    items.len() as u64 >= min,
                    format!("{} has fewer than {min} items", describe(instance))
                );
            }
            if let Some(max) = map.get("maxItems").and_then(Value::as_u64) {
                check!(
                    items.len() as u64 <= max,
                    format!("{} has more than {max} items", describe(instance))
                );
            }
            if map.get("uniqueItems").and_then(Value::as_bool) == Some(true) {
                let unique = items
                    .iter()
                    .enumerate()
                    .all(|(i, a)| items.iter().skip(i + 1).all(|b| a != b));
                check!(
                    unique,
                    format!("{} has non-unique items", describe(instance))
                );
            }
            let prefix = map
                .get("prefixItems")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            if let Some(prefixes) = map.get("prefixItems").and_then(Value::as_array) {
                for (index, (item, sub)) in items.iter().zip(prefixes).enumerate() {
                    let child = format!("{path}/{index}");
                    if !self.validate(sub, item, &child, walk) {
                        ok = false;
                        if first_only {
                            return false;
                        }
                    }
                }
            }
            if let Some(sub) = map.get("items") {
                for (index, item) in items.iter().enumerate().skip(prefix) {
                    let child = format!("{path}/{index}");
                    if !self.validate(sub, item, &child, walk) {
                        ok = false;
                        if first_only {
                            return false;
                        }
                    }
                }
            }
        }

        if let Some(object) = instance.as_object() {
            if let Some(required) = map.get("required").and_then(Value::as_array) {
                for name in required.iter().filter_map(Value::as_str) {
                    check!(
                        object.contains_key(name),
                        format!("{name:?} is a required property")
                    );
                }
            }
            if let Some(min) = map.get("minProperties").and_then(Value::as_u64) {
                check!(
                    object.len() as u64 >= min,
                    format!("object has fewer than {min} properties")
                );
            }
            if let Some(max) = map.get("maxProperties").and_then(Value::as_u64) {
                check!(
                    object.len() as u64 <= max,
                    format!("object has more than {max} properties")
                );
            }
            let properties = map.get("properties").and_then(Value::as_object);
            let names_schema = map.get("propertyNames");
            for (name, value) in object {
                let child = format!("{path}/{}", escape_pointer(name));
                let mut covered = false;
                if let Some(sub) = properties.and_then(|p| p.get(name)) {
                    covered = true;
                    if !self.validate(sub, value, &child, walk) {
                        ok = false;
                        if first_only {
                            return false;
                        }
                    }
                }
                if let Some(sub) = names_schema {
                    let name_value = Value::String(name.clone());
                    if !self.validate(sub, &name_value, &child, walk) {
                        ok = false;
                        if first_only {
                            return false;
                        }
                    }
                }
                if !covered {
                    match map.get("additionalProperties") {
                        Some(Value::Bool(false)) => {
                            check!(
                                false,
                                format!("additional property {name:?} is not allowed")
                            );
                        }
                        Some(sub @ Value::Object(_)) => {
                            let valid = self.validate(sub, value, &child, walk);
                            if !valid {
                                ok = false;
                                if first_only {
                                    return false;
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        if let Some(branches) = map.get("allOf").and_then(Value::as_array) {
            for sub in branches {
                if !self.validate(sub, instance, path, walk) {
                    ok = false;
                    if first_only {
                        return false;
                    }
                }
            }
        }
        if let Some(branches) = map.get("anyOf").and_then(Value::as_array) {
            let any = branches.iter().any(|sub| {
                self.validate(
                    sub,
                    instance,
                    path,
                    &mut Walk {
                        errors: Vec::new(),
                        first_only: true,
                        depth: walk.depth,
                    },
                )
            });
            check!(
                any,
                format!("{} matches none of the alternatives", describe(instance))
            );
        }
        if let Some(branches) = map.get("oneOf").and_then(Value::as_array) {
            let matching = branches
                .iter()
                .filter(|sub| {
                    self.validate(
                        sub,
                        instance,
                        path,
                        &mut Walk {
                            errors: Vec::new(),
                            first_only: true,
                            depth: walk.depth,
                        },
                    )
                })
                .count();
            check!(
                matching == 1,
                format!(
                    "{} matches {matching} of the exclusive alternatives, not exactly one",
                    describe(instance)
                )
            );
        }
        if let Some(sub) = map.get("not") {
            let inner = self.validate(
                sub,
                instance,
                path,
                &mut Walk {
                    errors: Vec::new(),
                    first_only: true,
                    depth: walk.depth,
                },
            );
            check!(
                !inner,
                format!("{} matches the forbidden schema", describe(instance))
            );
        }
        ok
    }
}

fn fail(path: &str, message: impl Into<String>) -> ValidationError {
    ValidationError {
        instance_path: path.to_owned(),
        message: message.into(),
    }
}

fn describe(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => {
            let shown: String = s.chars().take(40).collect();
            if shown.len() < s.len() {
                format!("{shown:?}...")
            } else {
                format!("{shown:?}")
            }
        }
        Value::Array(_) => "array".into(),
        Value::Object(_) => "object".into(),
    }
}

fn type_names(expected: &Value) -> String {
    match expected {
        Value::String(s) => format!("{s:?}"),
        Value::Array(names) => names
            .iter()
            .filter_map(Value::as_str)
            .map(|s| format!("{s:?}"))
            .collect::<Vec<_>>()
            .join(" or "),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_) => {
            "an unknown type".into()
        }
    }
}

fn has_type(value: &Value, name: &str) -> bool {
    match name {
        "null" => value.is_null(),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => {
            value.is_i64() || value.is_u64() || value.as_f64().is_some_and(|f| f.fract() == 0.0)
        }
        _ => false,
    }
}

/// Resolve a local `$ref` (`#`, `#/$defs/x`, `#/definitions/x`, or any
/// `#/`-rooted JSON Pointer into the root schema).
fn resolve<'a>(root: &'a Value, reference: &str) -> Option<&'a Value> {
    let pointer = reference.strip_prefix('#')?;
    if pointer.is_empty() {
        return Some(root);
    }
    let mut current = root;
    for raw in pointer.strip_prefix('/')?.split('/') {
        let token = raw.replace("~1", "/").replace("~0", "~");
        current = match current {
            Value::Object(map) => map.get(&token)?,
            Value::Array(items) => items.get(token.parse::<usize>().ok()?)?,
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => return None,
        };
    }
    Some(current)
}

fn escape_pointer(name: &str) -> String {
    name.replace('~', "~0").replace('/', "~1")
}

/// Walk the schema at compile time: supported keywords only, well-typed
/// keyword values, patterns compiled, local refs resolvable.
fn check_schema(root: &Value, schema: &Value, at: &str, depth: usize) -> Result<(), CompileError> {
    if depth > MAX_DEPTH {
        return Err(CompileError(format!(
            "{at}: schema nesting exceeds {MAX_DEPTH}"
        )));
    }
    let map = match schema {
        Value::Bool(_) => return Ok(()),
        Value::Object(map) => map,
        Value::Null | Value::Number(_) | Value::String(_) | Value::Array(_) => {
            return Err(CompileError(format!(
                "{at}: a schema must be an object or a boolean, not {}",
                describe(schema)
            )))
        }
    };
    let mut seen_keywords = BTreeSet::new();
    for (keyword, value) in map {
        let here = format!("{at}/{keyword}");
        // Vendor extension keywords (`x-susi-passthrough`, ...) are
        // annotations by the specification: unknown keywords carry no
        // validation and are ignored; Susi's own live under an `x-` prefix.
        if ANNOTATIONS.contains(&keyword.as_str()) || keyword.starts_with("x-") {
            if matches!(keyword.as_str(), "$defs" | "definitions") {
                let defs = value
                    .as_object()
                    .ok_or_else(|| CompileError(format!("{here}: must be an object of schemas")))?;
                for (name, sub) in defs {
                    check_schema(root, sub, &format!("{here}/{name}"), depth + 1)?;
                }
            }
            continue;
        }
        if !KEYWORDS.contains(&keyword.as_str()) {
            return Err(CompileError(format!(
                "{here}: unsupported validation keyword {keyword:?} (supported: {})",
                KEYWORDS.join(", ")
            )));
        }
        seen_keywords.insert(keyword.as_str());
        match keyword.as_str() {
            "type" => {
                let valid_name = |v: &Value| {
                    v.as_str().is_some_and(|s| {
                        matches!(
                            s,
                            "null"
                                | "boolean"
                                | "object"
                                | "array"
                                | "string"
                                | "number"
                                | "integer"
                        )
                    })
                };
                let ok = match value {
                    Value::String(_) => valid_name(value),
                    Value::Array(names) => !names.is_empty() && names.iter().all(valid_name),
                    Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_) => false,
                };
                if !ok {
                    return Err(CompileError(format!("{here}: unknown type {value}")));
                }
            }
            "properties" => {
                let props = value
                    .as_object()
                    .ok_or_else(|| CompileError(format!("{here}: must be an object of schemas")))?;
                for (name, sub) in props {
                    check_schema(root, sub, &format!("{here}/{name}"), depth + 1)?;
                }
            }
            "required" => {
                let names = value
                    .as_array()
                    .ok_or_else(|| CompileError(format!("{here}: must be an array of names")))?;
                if !names.iter().all(Value::is_string) {
                    return Err(CompileError(format!("{here}: must be an array of names")));
                }
            }
            "additionalProperties" | "items" | "propertyNames" | "not" => {
                check_schema(root, value, &here, depth + 1)?;
            }
            "prefixItems" | "anyOf" | "oneOf" | "allOf" => {
                let subs = value
                    .as_array()
                    .ok_or_else(|| CompileError(format!("{here}: must be an array of schemas")))?;
                if subs.is_empty() && keyword != "prefixItems" {
                    return Err(CompileError(format!("{here}: must not be empty")));
                }
                for (index, sub) in subs.iter().enumerate() {
                    check_schema(root, sub, &format!("{here}/{index}"), depth + 1)?;
                }
            }
            "minItems" | "maxItems" | "minLength" | "maxLength" | "minProperties"
            | "maxProperties" => {
                if value.as_u64().is_none() {
                    return Err(CompileError(format!(
                        "{here}: must be a non-negative integer"
                    )));
                }
            }
            "minimum" | "maximum" | "exclusiveMinimum" | "exclusiveMaximum" => {
                if value.as_f64().is_none() {
                    return Err(CompileError(format!("{here}: must be a number")));
                }
            }
            "multipleOf" => {
                if !value.as_f64().is_some_and(|f| f > 0.0) {
                    return Err(CompileError(format!("{here}: must be a positive number")));
                }
            }
            "uniqueItems" => {
                if !value.is_boolean() {
                    return Err(CompileError(format!("{here}: must be a boolean")));
                }
            }
            "enum" => {
                if !value.as_array().is_some_and(|a| !a.is_empty()) {
                    return Err(CompileError(format!("{here}: must be a non-empty array")));
                }
            }
            "const" => {}
            "$ref" => {
                let reference = value
                    .as_str()
                    .ok_or_else(|| CompileError(format!("{here}: must be a string")))?;
                if !reference.starts_with('#') {
                    return Err(CompileError(format!(
                        "{here}: only local references are supported, not {reference:?}"
                    )));
                }
                if resolve(root, reference).is_none() {
                    return Err(CompileError(format!(
                        "{here}: unresolvable local reference {reference:?}"
                    )));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Keep the `Map` import meaningful for callers building schemas.
pub type SchemaObject = Map<String, Value>;

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;

    fn compile(schema: Value) -> Validator {
        Validator::compile(&schema).expect("schema compiles")
    }

    #[test]
    fn types_required_and_bounds_behave_like_draft_2020_12() {
        let v = compile(json!({
            "type": "object",
            "properties": {
                "count": { "type": "integer", "minimum": 1, "maximum": 10 },
                "name": { "type": "string", "minLength": 1, "maxLength": 5 },
                "tags": { "type": "array", "items": { "type": "string" }, "maxItems": 2, "uniqueItems": true },
                "mode": { "enum": ["fast", "slow"] },
                "kind": { "const": "tool" },
                "ratio": { "type": "number", "exclusiveMinimum": 0, "multipleOf": 0.5 }
            },
            "required": ["count"],
            "additionalProperties": false
        }));
        assert!(v.is_valid(&json!({ "count": 3 })));
        assert!(v.is_valid(&json!({ "count": 3.0, "name": "ab", "tags": ["a", "b"], "mode": "fast", "kind": "tool", "ratio": 1.5 })));
        for bad in [
            json!({}),
            json!({ "count": 0 }),
            json!({ "count": 11 }),
            json!({ "count": 2.5 }),
            json!({ "count": "3" }),
            json!({ "count": 3, "name": "" }),
            json!({ "count": 3, "name": "toolong" }),
            json!({ "count": 3, "tags": ["a", "a"] }),
            json!({ "count": 3, "tags": ["a", "b", "c"] }),
            json!({ "count": 3, "mode": "medium" }),
            json!({ "count": 3, "kind": "other" }),
            json!({ "count": 3, "ratio": 0 }),
            json!({ "count": 3, "ratio": 0.3 }),
            json!({ "count": 3, "extra": true }),
        ] {
            assert!(!v.is_valid(&bad), "{bad}");
        }
        let errors = v.iter_errors(&json!({ "count": 0, "name": "" }));
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert_eq!(errors[0].instance_path(), "/count");
        assert!(errors[0].to_string().contains("minimum"), "{}", errors[0]);
        assert_eq!(errors[1].instance_path(), "/name");
    }

    #[test]
    fn combinators_refs_and_patterns() {
        let v = compile(json!({
            "$defs": {
                "id": { "type": "string", "minLength": 1 },
                "positive": { "type": "integer", "minimum": 1 }
            },
            "type": "object",
            "properties": {
                "id": { "$ref": "#/$defs/id" },
                "size": { "anyOf": [ { "$ref": "#/$defs/positive" }, { "type": "null" } ] },
                "shape": { "oneOf": [ { "const": "round" }, { "const": "square" } ] },
                "all": { "allOf": [ { "type": "string" }, { "minLength": 2 } ] },
                "never": { "not": { "type": "string" } },
                "meta": { "type": "object", "properties": { "x-a": { "type": "string" } }, "propertyNames": { "maxLength": 8 }, "additionalProperties": false }
            }
        }));
        assert!(v.is_valid(&json!({ "id": "tool-1", "size": null, "shape": "round", "all": "ok", "never": 1, "meta": { "x-a": "b" } })));
        assert!(!v.is_valid(&json!({ "id": "" })));
        assert!(!v.is_valid(&json!({ "size": 0 })));
        assert!(!v.is_valid(&json!({ "shape": "oval" })));
        assert!(!v.is_valid(&json!({ "all": "x" })));
        assert!(!v.is_valid(&json!({ "never": "s" })));
        assert!(!v.is_valid(&json!({ "meta": { "x-a": 1 } })));
        assert!(!v.is_valid(&json!({ "meta": { "plain": "b" } })));
        assert!(!v.is_valid(&json!({ "meta": { "x-toolongname": "b" } })));
    }

    #[test]
    fn boolean_schemas_type_arrays_and_prefix_items() {
        let v = compile(json!({
            "type": "array",
            "prefixItems": [ { "type": "string" }, { "type": ["integer", "null"] } ],
            "items": false,
            "minItems": 1
        }));
        assert!(v.is_valid(&json!(["a"])));
        assert!(v.is_valid(&json!(["a", 2])));
        assert!(v.is_valid(&json!(["a", null])));
        assert!(!v.is_valid(&json!(["a", 2, 3])));
        assert!(!v.is_valid(&json!([])));
        assert!(!v.is_valid(&json!([1])));
        assert!(compile(json!(true)).is_valid(&json!(42)));
        assert!(!compile(json!(false)).is_valid(&json!(42)));
    }

    #[test]
    fn compile_rejects_what_it_does_not_implement_and_bad_keyword_values() {
        for (schema, needle) in [
            (
                json!({ "if": { "type": "string" }, "then": true }),
                "unsupported validation keyword",
            ),
            (
                json!({ "dependentRequired": {} }),
                "unsupported validation keyword",
            ),
            (json!({ "type": "text" }), "unknown type"),
            (json!({ "required": "name" }), "array of names"),
            (json!({ "minimum": "1" }), "must be a number"),
            (json!({ "minLength": -1 }), "non-negative integer"),
            (json!({ "enum": [] }), "non-empty"),
            (json!({ "anyOf": [] }), "must not be empty"),
            (
                json!({ "pattern": "^a$" }),
                "unsupported validation keyword",
            ),
            (
                json!({ "patternProperties": {} }),
                "unsupported validation keyword",
            ),
            (
                json!({ "$ref": "https://example.com/schema" }),
                "only local references",
            ),
            (
                json!({ "$ref": "#/$defs/missing" }),
                "unresolvable local reference",
            ),
            (
                json!({ "properties": { "a": 7 } }),
                "must be an object or a boolean",
            ),
        ] {
            let error = Validator::compile(&schema).expect_err(&schema.to_string());
            assert!(error.to_string().contains(needle), "{schema}: {error}");
        }
    }

    #[test]
    fn annotations_are_accepted_and_ignored() {
        assert!(
            compile(json!({ "type": "object", "x-susi-passthrough": true })).is_valid(&json!({}))
        );
        let v = compile(json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "$id": "urn:susi:test",
            "title": "T",
            "description": "d",
            "type": "string",
            "format": "email",
            "default": "x",
            "examples": ["a"],
            "deprecated": false
        }));
        assert!(v.is_valid(&json!("not-an-email")));
    }

    #[test]
    fn cyclic_refs_fail_instead_of_looping() {
        let v = compile(
            json!({ "$defs": { "loop": { "$ref": "#/$defs/loop" } }, "$ref": "#/$defs/loop" }),
        );
        assert!(!v.is_valid(&json!(1)));
        assert!(v
            .iter_errors(&json!(1))
            .iter()
            .any(|e| e.to_string().contains("depth")));
    }

    #[test]
    fn pointer_escapes_in_paths_and_refs() {
        let v = compile(json!({
            "$defs": { "a/b": { "type": "integer" } },
            "properties": { "x/y": { "$ref": "#/$defs/a~1b" } }
        }));
        let errors = v.iter_errors(&json!({ "x/y": "no" }));
        assert_eq!(errors[0].instance_path(), "/x~1y");
    }
}
