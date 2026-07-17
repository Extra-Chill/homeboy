//! Component-side implementation of the audit component provider.
//!
//! The audit engine (`code_audit`) defines `AuditComponentProvider` and calls it
//! to resolve the component under audit without depending on the component
//! layer. This module implements that trait by resolving real `Component`s
//! (`load` / `resolve_effective` + `validate_local_path` / `discover_from_portable`)
//! and projecting each into the slim `AuditComponentInfo` view audit needs. It is
//! registered at binary startup by the CLI, mirroring the extension-manifest /
//! fixability / runner-evidence / tunnel provider hooks.

use std::path::Path;

use super::model::Component;
use super::scope::{resolve_component_scope, ScopeCommand};
use crate::code_audit::component_provider::{
    register_audit_component_provider, AuditComponentInfo, AuditComponentProvider,
};
use crate::Result;

/// Project a resolved `Component` into the audit-relevant view.
fn project(component: &Component) -> AuditComponentInfo {
    AuditComponentInfo {
        local_path: component.local_path.clone(),
        extension_ids: component
            .extensions
            .as_ref()
            .map(|extensions| extensions.keys().cloned().collect())
            .unwrap_or_default(),
        audit_rules: component.audit.clone(),
        audit_scope_excludes: resolve_component_scope(component, ScopeCommand::Audit).exclude,
    }
}

struct ComponentAuditProvider;

impl AuditComponentProvider for ComponentAuditProvider {
    fn resolve_by_id(&self, component_id: &str) -> Option<AuditComponentInfo> {
        super::load(component_id).ok().map(|c| project(&c))
    }

    fn resolve_effective(&self, component_id: &str) -> Result<AuditComponentInfo> {
        let component = super::resolve_effective(Some(component_id), None, None)?;
        super::validate_local_path(&component)?;
        Ok(project(&component))
    }

    fn discover_from_portable(&self, root: &Path) -> Option<AuditComponentInfo> {
        super::discover_from_portable(root).map(|c| project(&c))
    }
}

/// Register the component-backed audit component provider. Called once at binary
/// startup by the CLI.
pub fn register() {
    register_audit_component_provider(Box::new(ComponentAuditProvider));
}
