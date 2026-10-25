//! The published JSON Schemas are the contract; this validates against them, not a copy.

use jsonschema::Validator;
use serde_json::Value;
use std::sync::OnceLock;

pub const ENTRY_SCHEMA: &str = include_str!("../../../schema/entry.schema.json");
pub const STACK_SCHEMA: &str = include_str!("../../../schema/stack.schema.json");

fn compile(text: &str) -> Validator {
    let schema: Value = serde_json::from_str(text).expect("the bundled schema is JSON");
    jsonschema::validator_for(&schema).expect("the bundled schema is valid")
}

fn errors(validator: &Validator, value: &Value) -> Vec<String> {
    validator
        .iter_errors(value)
        .map(|e| {
            let path = e.instance_path.to_string();
            if path.is_empty() {
                e.to_string()
            } else {
                format!("{path}: {e}")
            }
        })
        .collect()
}

pub fn entry_errors(value: &Value) -> Vec<String> {
    static V: OnceLock<Validator> = OnceLock::new();
    errors(V.get_or_init(|| compile(ENTRY_SCHEMA)), value)
}

pub fn stack_errors(value: &Value) -> Vec<String> {
    static V: OnceLock<Validator> = OnceLock::new();
    errors(V.get_or_init(|| compile(STACK_SCHEMA)), value)
}
