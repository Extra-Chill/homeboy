//! Rig provider hook.
//!
//! A few core surfaces read the rig subsystem: the HTTP API lists/shows/checks
//! rigs, and scope resolution expands a rig's component paths into scope refs
//! and component records. Loading + expanding rig specs is rig behavior, so it
//! is inverted behind this provider: core owns the HTTP API and scope model,
//! the rig layer supplies the rig data.
//!
//! With no provider registered (no rig subsystem present) the no-op reports no
//! rigs, so rig-scoped resolution is empty and the rig HTTP endpoints return
//! empty results.

use serde_json::Value;

use crate::component::Component;
use crate::scope::ScopeComponentRef;
use crate::Result;

/// Supplies rig data to core's HTTP API and scope resolution.
pub trait RigProvider: Send + Sync {
    /// All rigs as a JSON array (for `api.rigs.list`).
    fn rig_list_json(&self) -> Result<Value>;

    /// A single rig as JSON (for `api.rigs.show`).
    fn rig_show_json(&self, id: &str) -> Result<Value>;

    /// A rig's check report as JSON (for `api.rigs.check`).
    fn rig_check_json(&self, id: &str) -> Result<Value>;

    /// Every rig id (for enumerating rig scopes).
    fn rig_ids(&self) -> Result<Vec<String>>;

    /// Resolve a rig's components into scope refs (expanded paths + remotes).
    fn resolve_rig_scope(&self, rig_id: &str) -> Result<Vec<ScopeComponentRef>>;

    /// Resolve a rig's components into discovered component records.
    fn resolve_rig_component_records(&self, rig_id: &str) -> Result<Vec<Component>>;
}

struct NoopProvider;

impl RigProvider for NoopProvider {
    fn rig_list_json(&self) -> Result<Value> {
        Ok(Value::Array(Vec::new()))
    }
    fn rig_show_json(&self, _id: &str) -> Result<Value> {
        Ok(Value::Null)
    }
    fn rig_check_json(&self, _id: &str) -> Result<Value> {
        Ok(Value::Null)
    }
    fn rig_ids(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
    fn resolve_rig_scope(&self, _rig_id: &str) -> Result<Vec<ScopeComponentRef>> {
        Ok(Vec::new())
    }
    fn resolve_rig_component_records(&self, _rig_id: &str) -> Result<Vec<Component>> {
        Ok(Vec::new())
    }
}

homeboy_engine_primitives::provider_registry! {
    provider: dyn RigProvider,
    noop: NoopProvider,
    /// Register the rig provider. Called once at startup by the rig layer.
    register: pub fn register_rig_provider,
    /// Run `f` against the registered provider, or the no-op provider if none
    /// is registered.
    with: fn with_provider,
}

pub(crate) fn rig_list_json() -> Result<Value> {
    with_provider(|p| p.rig_list_json())
}

pub(crate) fn rig_show_json(id: &str) -> Result<Value> {
    with_provider(|p| p.rig_show_json(id))
}

pub(crate) fn rig_check_json(id: &str) -> Result<Value> {
    with_provider(|p| p.rig_check_json(id))
}

pub(crate) fn rig_ids() -> Result<Vec<String>> {
    with_provider(|p| p.rig_ids())
}

pub(crate) fn resolve_rig_scope(rig_id: &str) -> Result<Vec<ScopeComponentRef>> {
    with_provider(|p| p.resolve_rig_scope(rig_id))
}

pub(crate) fn resolve_rig_component_records(rig_id: &str) -> Result<Vec<Component>> {
    with_provider(|p| p.resolve_rig_component_records(rig_id))
}
