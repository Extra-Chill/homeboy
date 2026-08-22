//! Stack-side implementation of core's `StackProvider` hook.
//!
//! Core owns the HTTP API surface; this supplies the stack data by loading and
//! inspecting stack specs and serializing them to JSON.

use homeboy_core::paths;
use homeboy_core::stack_provider::{register_stack_provider, StackProvider};
use homeboy_core::Result;
use serde_json::Value;
use std::path::PathBuf;

use crate::stack;

struct StackProviderImpl;

/// Each provider method answers one HTTP request, which is one unit of work, so
/// this is a boundary resolution rather than an ambient one (#7505). It is
/// deliberately per-call: the provider is a long-lived singleton and capturing a
/// root at registration would pin it to whatever home happened to be current
/// then.
fn config_root() -> Result<PathBuf> {
    paths::homeboy()
}

impl StackProvider for StackProviderImpl {
    fn stack_list_json(&self) -> Result<Value> {
        let stacks = stack::list(&config_root()?)?;
        Ok(serde_json::to_value(stacks).unwrap_or(Value::Null))
    }

    fn stack_show_json(&self, id: &str) -> Result<Value> {
        let spec = stack::load(&config_root()?, id)?;
        Ok(serde_json::to_value(spec).unwrap_or(Value::Null))
    }

    fn stack_status_json(&self, id: &str) -> Result<Value> {
        let spec = stack::load(&config_root()?, id)?;
        let report = stack::status(&spec)?;
        Ok(serde_json::to_value(report).unwrap_or(Value::Null))
    }
}

/// Register the stack provider. Called once at startup by the CLI runtime.
pub fn register() {
    register_stack_provider(Box::new(StackProviderImpl));
}
