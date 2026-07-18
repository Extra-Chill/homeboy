//! Stack provider hook.
//!
//! Core's HTTP API lists/shows stacks and reports stack status. Loading and
//! inspecting stack specs is stack behavior, so it is inverted behind this
//! provider: core owns the HTTP API surface, the stack layer supplies the data.
//!
//! With no provider registered (no stack subsystem present) the no-op reports
//! no stacks, so the stack HTTP endpoints return empty results.

use std::sync::Mutex;

use serde_json::Value;

use crate::Result;

/// Supplies stack data to core's HTTP API.
pub trait StackProvider: Send + Sync {
    /// All stacks as a JSON array (for `api.stacks.list`).
    fn stack_list_json(&self) -> Result<Value>;

    /// A single stack as JSON (for `api.stacks.show`).
    fn stack_show_json(&self, id: &str) -> Result<Value>;

    /// A stack's status report as JSON (for `api.stacks.status`).
    fn stack_status_json(&self, id: &str) -> Result<Value>;
}

struct NoopProvider;

impl StackProvider for NoopProvider {
    fn stack_list_json(&self) -> Result<Value> {
        Ok(Value::Array(Vec::new()))
    }
    fn stack_show_json(&self, _id: &str) -> Result<Value> {
        Ok(Value::Null)
    }
    fn stack_status_json(&self, _id: &str) -> Result<Value> {
        Ok(Value::Null)
    }
}

fn provider_slot() -> &'static Mutex<Option<Box<dyn StackProvider>>> {
    static PROVIDER: Mutex<Option<Box<dyn StackProvider>>> = Mutex::new(None);
    &PROVIDER
}

/// Register the stack provider. Called once at startup by the stack layer.
pub fn register_stack_provider(provider: Box<dyn StackProvider>) {
    let mut slot = provider_slot().lock().expect("stack provider lock");
    *slot = Some(provider);
}

fn with_provider<T>(f: impl FnOnce(&dyn StackProvider) -> T) -> T {
    let slot = provider_slot().lock().expect("stack provider lock");
    match slot.as_deref() {
        Some(provider) => f(provider),
        None => f(&NoopProvider),
    }
}

pub(crate) fn stack_list_json() -> Result<Value> {
    with_provider(|p| p.stack_list_json())
}

pub(crate) fn stack_show_json(id: &str) -> Result<Value> {
    with_provider(|p| p.stack_show_json(id))
}

pub(crate) fn stack_status_json(id: &str) -> Result<Value> {
    with_provider(|p| p.stack_status_json(id))
}
