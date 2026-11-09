//! JSON Schema 2020-12 `inputSchema`s declared as Rust struct shapes.
//!
//! [`input_schema!`](crate::input_schema) takes a struct declaration with
//! doc comments and the `#[serde(default)]` / `#[serde(rename = "...")]`
//! field attributes, and produces the schema an MCP client sees: one
//! property per field (doc comment as `description`), `Option` fields
//! nullable and optional, defaulted fields optional with their `default`,
//! nested structs under `$defs`. The struct itself becomes a marker type;
//! the handler keeps reading its `serde_json::Value` arguments.

use super::model::JsonObject;
use serde_json::{json, Value};

/// A Rust type's JSON Schema.
pub trait SchemaType {
    /// `Option` fields are never `required`.
    const OPTIONAL: bool = false;
    /// Whether a `#[serde(default)]` field of this type advertises its
    /// default value.
    const SERIALIZABLE: bool = true;
    fn schema(defs: &mut JsonObject) -> Value;
    /// The `default` a `#[serde(default)]` field advertises.
    fn default_value() -> Option<Value>;
}

/// A struct declared with [`input_schema!`](crate::input_schema).
pub trait StructSchema {
    const NAME: &'static str;
    fn object(defs: &mut JsonObject) -> JsonObject;
}

macro_rules! primitive {
    ($($ty:ty => $schema:tt, $default:expr;)*) => {$(
        impl SchemaType for $ty {
            fn schema(_: &mut JsonObject) -> Value {
                json!($schema)
            }
            fn default_value() -> Option<Value> {
                Some(json!($default))
            }
        }
    )*};
}

primitive! {
    String => {"type": "string"}, "";
    bool => {"type": "boolean"}, false;
    u64 => {"type": "integer", "format": "uint64", "minimum": 0}, 0;
    u32 => {"type": "integer", "format": "uint32", "minimum": 0}, 0;
    u16 => {"type": "integer", "format": "uint16", "minimum": 0, "maximum": 65535}, 0;
    f64 => {"type": "number", "format": "double"}, 0.0;
}

impl SchemaType for Value {
    fn schema(_: &mut JsonObject) -> Value {
        json!({})
    }
    fn default_value() -> Option<Value> {
        Some(Value::Null)
    }
}

impl SchemaType for serde_json::Map<String, Value> {
    fn schema(_: &mut JsonObject) -> Value {
        json!({"type": "object", "additionalProperties": true})
    }
    fn default_value() -> Option<Value> {
        Some(json!({}))
    }
}

impl<T: SchemaType> SchemaType for Vec<T> {
    fn schema(defs: &mut JsonObject) -> Value {
        json!({"type": "array", "items": T::schema(defs)})
    }
    fn default_value() -> Option<Value> {
        T::SERIALIZABLE.then(|| json!([]))
    }
}

impl<T: SchemaType> SchemaType for Option<T> {
    const OPTIONAL: bool = true;
    const SERIALIZABLE: bool = T::SERIALIZABLE;
    fn schema(defs: &mut JsonObject) -> Value {
        let mut schema = T::schema(defs);
        let Value::Object(map) = &mut schema else {
            return schema;
        };
        if map.contains_key("$ref") {
            return json!({"anyOf": [schema, {"type": "null"}]});
        }
        match map.get_mut("type") {
            Some(Value::String(t)) => {
                let t = std::mem::take(t);
                map.insert("type".into(), json!([t, "null"]));
            }
            Some(Value::Array(types)) => types.push(json!("null")),
            _ => {}
        }
        schema
    }
    fn default_value() -> Option<Value> {
        T::SERIALIZABLE.then_some(Value::Null)
    }
}

/// A struct field's (or struct's) doc lines as a schema `description`:
/// each line loses one leading space, lines join with `\n`, the whole is
/// trimmed.
pub fn description(lines: &[&str]) -> Option<String> {
    let joined = lines
        .iter()
        .map(|line| line.strip_prefix(' ').unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n");
    let trimmed = joined.trim_matches(|c: char| c.is_ascii_whitespace());
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Insert a nested struct under `$defs` (once) and return its `$ref`.
pub fn reference<T: StructSchema>(defs: &mut JsonObject) -> Value {
    if !defs.contains_key(T::NAME) {
        defs.insert(T::NAME.to_string(), Value::Null);
        let object = T::object(defs);
        defs.insert(T::NAME.to_string(), Value::Object(object));
    }
    json!({"$ref": format!("#/$defs/{}", T::NAME)})
}

/// The root `inputSchema` of `T`: its object schema with the draft
/// declared, `$defs` collected, and the struct's own description dropped.
pub fn root<T: StructSchema>() -> JsonObject {
    let mut defs = JsonObject::new();
    let mut object = T::object(&mut defs);
    object.remove("description");
    object.insert(
        "$schema".into(),
        json!("https://json-schema.org/draft/2020-12/schema"),
    );
    if !defs.is_empty() {
        object.insert("$defs".into(), Value::Object(defs));
    }
    object
}

/// One property of an [`input_schema!`](crate::input_schema) struct.
pub struct Field {
    pub name: &'static str,
    pub docs: Vec<&'static str>,
    pub default: bool,
}

impl Field {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            docs: Vec::new(),
            default: false,
        }
    }
    pub fn add<T: SchemaType>(
        self,
        defs: &mut JsonObject,
        properties: &mut JsonObject,
        required: &mut Vec<Value>,
    ) {
        let mut schema = T::schema(defs);
        if let Value::Object(map) = &mut schema {
            if self.default {
                if let Some(default) = T::default_value() {
                    map.insert("default".into(), default);
                }
            }
            if let Some(text) = description(&self.docs) {
                map.insert("description".into(), Value::String(text));
            }
        }
        if !self.default && !T::OPTIONAL {
            required.push(Value::String(self.name.to_string()));
        }
        properties.insert(self.name.to_string(), schema);
    }
}

/// Assemble a struct's object schema.
pub fn object(docs: &[&str], properties: JsonObject, required: Vec<Value>) -> JsonObject {
    let mut object = JsonObject::new();
    if let Some(text) = description(docs) {
        object.insert("description".into(), Value::String(text));
    }
    object.insert("type".into(), json!("object"));
    object.insert("properties".into(), Value::Object(properties));
    if !required.is_empty() {
        object.insert("required".into(), Value::Array(required));
    }
    object
}

/// Declare an argument shape and derive its MCP `inputSchema`.
#[macro_export]
macro_rules! input_schema {
    ($(
        $(#[doc = $sdoc:literal])*
        $vis:vis struct $name:ident {
            $(
                $(#[$($fattr:tt)*])*
                $fvis:vis $field:ident : $ty:ty
            ),* $(,)?
        }
    )*) => {$(
        $(#[doc = $sdoc])*
        $vis struct $name;

        impl $crate::mcp::schema::StructSchema for $name {
            const NAME: &'static str = stringify!($name);
            fn object(
                defs: &mut $crate::mcp::model::JsonObject,
            ) -> $crate::mcp::model::JsonObject {
                #[allow(unused_mut)]
                let mut properties = $crate::mcp::model::JsonObject::new();
                #[allow(unused_mut)]
                let mut required = Vec::new();
                $(
                    #[allow(unused_mut)]
                    let mut field = $crate::mcp::schema::Field::new(stringify!($field));
                    $( $crate::input_schema!(@attr field; $($fattr)*); )*
                    field.add::<$ty>(defs, &mut properties, &mut required);
                )*
                $crate::mcp::schema::object(&[$($sdoc),*], properties, required)
            }
        }

        impl $crate::mcp::schema::SchemaType for $name {
            const SERIALIZABLE: bool = false;
            fn schema(defs: &mut $crate::mcp::model::JsonObject) -> serde_json::Value {
                $crate::mcp::schema::reference::<$name>(defs)
            }
            fn default_value() -> Option<serde_json::Value> {
                None
            }
        }
    )*};
    (@attr $field:ident; doc = $doc:literal) => {
        $field.docs.push($doc);
    };
    (@attr $field:ident; serde(default)) => {
        $field.default = true;
    };
    (@attr $field:ident; serde(rename = $name:literal)) => {
        $field.name = $name;
    };
}

/// Declare a built-in tool's `*_tool_attr()` definition: its MCP name,
/// description and `inputSchema`.
#[macro_export]
macro_rules! tool_def {
    ($fn_name:ident { name: $name:expr, description: $description:expr, input_schema: $schema:expr $(,)? }) => {
        #[doc = concat!("The MCP definition of `", $name, "`.")]
        pub fn $fn_name() -> $crate::mcp::model::Tool {
            $crate::mcp::model::Tool::new_with_raw(
                $name,
                Some(std::borrow::Cow::Borrowed($description)),
                $schema,
            )
        }
    };
}
