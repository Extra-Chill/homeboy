//! Component resolution for the audit engine, inverted behind a provider.
//!
//! The audit engine needs a little data about the component it is auditing —
//! the resolved on-disk source path, the installed extension ids, the
//! component's own audit detector rules, and the audit-scope exclude globs. It
//! used to reach that by calling `homeboy_core::component::{load, resolve_effective,
//! discover_from_portable, validate_local_path}` and `component::scope::*`
//! directly, which coupled `code_audit` to the `component` feature layer and was
//! the last cross-layer blocker to extracting audit into its own crate.
//!
//! Instead, audit defines the slim view it needs (`AuditComponentInfo`) plus a
//! provider trait; the component layer registers an implementation at startup
//! (same pattern as the extension-manifest / fixability / runner-evidence /
//! tunnel provider hooks). When no provider is registered — e.g. audit running
//! standalone — the no-op provider resolves nothing, which every call site
//! already tolerates (path audits fall back to the raw path; config/doc lookups
//! contribute no component-derived rules).

use std::path::Path;
use std::sync::Mutex;

use homeboy_audit_contract::AuditConfig;

use homeboy_error::Result;

/// The slim, owned view of a component that the audit engine needs.
///
/// Owning the data (rather than borrowing a `Component`) keeps the provider
/// boundary clean: the audit engine never sees the full component type.
#[derive(Debug, Clone, Default)]
pub struct AuditComponentInfo {
    /// Resolved absolute on-disk source path (the component's `local_path`).
    pub local_path: String,
    /// Ids of extensions installed for this component.
    pub extension_ids: Vec<String>,
    /// The component's own audit detector rules, if it declares any.
    pub audit_rules: Option<AuditConfig>,
    /// Audit-scope exclude globs (from the component's effective `audit` scope).
    pub audit_scope_excludes: Vec<String>,
}

/// The component-resolution contract the audit engine depends on. Implemented by
/// the component layer and registered at startup; audit calls it without
/// depending on component behavior.
pub trait AuditComponentProvider: Send + Sync {
    /// Resolve a registered component by id (like `component::load`). Returns
    /// `None` when the component is not registered.
    fn resolve_by_id(&self, component_id: &str) -> Option<AuditComponentInfo>;

    /// Resolve a component's effective config by id and validate its local path
    /// (like `component::resolve_effective` + `validate_local_path`). Errors when
    /// the component cannot be resolved or its path is invalid.
    fn resolve_effective(&self, component_id: &str) -> Result<AuditComponentInfo>;

    /// Discover a component from a portable `homeboy.json` at `root` (like
    /// `component::discover_from_portable`). Returns `None` when absent/invalid.
    fn discover_from_portable(&self, root: &Path) -> Option<AuditComponentInfo>;
}

/// Default provider used when no component layer is registered: resolves
/// nothing. Every audit call site treats a missing component as "no
/// component-derived configuration", so behavior degrades to a raw-path audit.
struct NoopProvider;

impl AuditComponentProvider for NoopProvider {
    fn resolve_by_id(&self, _component_id: &str) -> Option<AuditComponentInfo> {
        None
    }

    fn resolve_effective(&self, component_id: &str) -> Result<AuditComponentInfo> {
        Err(homeboy_error::Error::internal_unexpected(format!(
            "no audit component provider registered; cannot resolve component `{component_id}`"
        )))
    }

    fn discover_from_portable(&self, _root: &Path) -> Option<AuditComponentInfo> {
        None
    }
}

static PROVIDER: Mutex<Option<Box<dyn AuditComponentProvider>>> = Mutex::new(None);

/// Register the audit component provider. Called once at binary startup by the
/// component layer (via the CLI). Replaces any previously registered provider.
pub fn register_audit_component_provider(provider: Box<dyn AuditComponentProvider>) {
    let mut guard = PROVIDER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = Some(provider);
}

/// Resolve a registered component by id via the registered provider.
pub(crate) fn resolve_by_id(component_id: &str) -> Option<AuditComponentInfo> {
    with_provider(|p| p.resolve_by_id(component_id))
}

/// Resolve + validate a component's effective config by id via the provider.
pub(crate) fn resolve_effective(component_id: &str) -> Result<AuditComponentInfo> {
    with_provider(|p| p.resolve_effective(component_id))
}

/// Discover a component from a portable config at `root` via the provider.
pub(crate) fn discover_from_portable(root: &Path) -> Option<AuditComponentInfo> {
    with_provider(|p| p.discover_from_portable(root))
}

fn with_provider<T>(f: impl FnOnce(&dyn AuditComponentProvider) -> T) -> T {
    let guard = PROVIDER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match guard.as_ref() {
        Some(provider) => f(provider.as_ref()),
        None => f(&NoopProvider),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_provider_resolves_nothing() {
        assert!(NoopProvider.resolve_by_id("anything").is_none());
        assert!(NoopProvider
            .discover_from_portable(Path::new("/nope"))
            .is_none());
        assert!(NoopProvider.resolve_effective("anything").is_err());
    }
}
