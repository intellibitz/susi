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
    assert!(compile(json!({ "type": "object", "x-susi-passthrough": true })).is_valid(&json!({})));
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
    let v =
        compile(json!({ "$defs": { "loop": { "$ref": "#/$defs/loop" } }, "$ref": "#/$defs/loop" }));
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
