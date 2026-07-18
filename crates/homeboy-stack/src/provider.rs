//! Stack-side implementation of core's `StackProvider` hook.
//!
//! Core owns the HTTP API surface; this supplies the stack data by loading and
//! inspecting stack specs and serializing them to JSON.

use homeboy_core::stack_provider::{register_stack_provider, StackProvider};
use homeboy_core::Result;
use serde_json::Value;

use crate::stack;

struct StackProviderImpl;

impl StackProvider for StackProviderImpl {
    fn stack_list_json(&self) -> Result<Value> {
        let stacks = stack::list()?;
        Ok(serde_json::to_value(stacks).unwrap_or(Value::Null))
    }

    fn stack_show_json(&self, id: &str) -> Result<Value> {
        let spec = stack::load(id)?;
        Ok(serde_json::to_value(spec).unwrap_or(Value::Null))
    }

    fn stack_status_json(&self, id: &str) -> Result<Value> {
        let spec = stack::load(id)?;
        let report = stack::status(&spec)?;
        Ok(serde_json::to_value(report).unwrap_or(Value::Null))
    }
}

/// Register the stack provider. Called once at startup by the CLI runtime.
pub fn register() {
    register_stack_provider(Box::new(StackProviderImpl));
}
