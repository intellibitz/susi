//! Lightweight JSON Payload Schema Validator (Swarm OS Bullet 62)
//!
//! A built-in validator to ensure payload correctness on the wire
//! before delivering messages or syscalls to a cell.

use serde_json::Value;

/// Represents the expected type of a JSON field.
#[derive(Debug, Clone, PartialEq)]
pub enum SchemaType {
    String,
    Number,
    Boolean,
    Object,
    Array,
    Any,
}

/// A simple schema definition mapping field names to expected types.
#[derive(Debug, Clone)]
pub struct PayloadSchema {
    fields: std::collections::HashMap<String, SchemaType>,
    allow_unknown: bool,
}

impl PayloadSchema {
    pub fn new(allow_unknown: bool) -> Self {
        Self {
            fields: std::collections::HashMap::new(),
            allow_unknown,
        }
    }

    /// Adds a required field and its expected type to the schema.
    pub fn add_field(mut self, name: &str, expected_type: SchemaType) -> Self {
        self.fields.insert(name.to_string(), expected_type);
        self
    }

    /// Validates a JSON value against the schema.
    pub fn validate(&self, payload: &Value) -> Result<(), String> {
        let obj = payload.as_object().ok_or("Payload must be a JSON object")?;

        // 1. Check all required fields are present and of the correct type
        for (field, expected_type) in &self.fields {
            let val = obj.get(field).ok_or_else(|| format!("Missing required field: {}", field))?;
            
            let matches = match expected_type {
                SchemaType::String => val.is_string(),
                SchemaType::Number => val.is_number(),
                SchemaType::Boolean => val.is_boolean(),
                SchemaType::Object => val.is_object(),
                SchemaType::Array => val.is_array(),
                SchemaType::Any => true,
            };

            if !matches {
                return Err(format!("Field '{}' has invalid type. Expected {:?}", field, expected_type));
            }
        }

        // 2. Check for unknown fields if strict mode is on
        if !self.allow_unknown {
            for key in obj.keys() {
                if !self.fields.contains_key(key) {
                    return Err(format!("Unknown field not allowed in strict schema: {}", key));
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_schema_validation() {
        let schema = PayloadSchema::new(false)
            .add_field("command", SchemaType::String)
            .add_field("timeout_ms", SchemaType::Number);

        // Valid payload
        let valid = json!({
            "command": "git status",
            "timeout_ms": 5000
        });
        assert!(schema.validate(&valid).is_ok());

        // Missing field
        let missing = json!({
            "command": "ls"
        });
        assert!(schema.validate(&missing).is_err());

        // Invalid type
        let invalid_type = json!({
            "command": "ls",
            "timeout_ms": "5000" // String instead of Number
        });
        assert!(schema.validate(&invalid_type).is_err());

        // Unknown field
        let unknown = json!({
            "command": "ls",
            "timeout_ms": 100,
            "extra": true
        });
        assert!(schema.validate(&unknown).is_err());
    }
}
