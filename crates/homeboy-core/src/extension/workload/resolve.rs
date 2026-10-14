//! Resolve which extension provides a workload kind for a component.

use homeboy_extension_contract::workload_kind::is_valid_workload_kind;
use homeboy_extension_contract::WorkloadKindConfig;

use crate::component::Component;
use crate::engine::execution_context::{self, ExecutionContext, ResolveOptions};
use crate::error::{Error, Result};
use crate::extension::resolve::{resolve_workload_execution_context, ExtensionExecutionContext};

/// Where a workload runs: a component id and/or path, optionally an
/// in-memory component (rig-pinned runs) and one-shot extension overrides.
#[derive(Debug, Clone, Default)]
pub struct WorkloadTarget {
    pub component_id: Option<String>,
    pub path_override: Option<String>,
    pub extension_overrides: Vec<String>,
    pub component_override: Option<Component>,
}

/// A kind resolved against a component: the component/source context, the
/// providing extension's execution context, and the kind's declaration.
#[derive(Debug, Clone)]
pub struct ResolvedWorkloadKind {
    pub kind: String,
    pub context: ExecutionContext,
    pub extension: ExtensionExecutionContext,
    pub config: WorkloadKindConfig,
}

impl ResolvedWorkloadKind {
    pub fn extension_id(&self) -> &str {
        &self.extension.extension_id
    }
}

pub fn validate_kind(kind: &str) -> Result<()> {
    if is_valid_workload_kind(kind) {
        Ok(())
    } else {
        Err(Error::validation_invalid_argument(
            "kind",
            "workload kind must be a lowercase slug of [a-z0-9_-] starting with a letter",
            Some(kind.to_string()),
            None,
        ))
    }
}

pub fn resolve_kind(kind: &str, target: WorkloadTarget) -> Result<ResolvedWorkloadKind> {
    validate_kind(kind)?;
    let options = ResolveOptions {
        component_id: target.component_id.clone(),
        path_override: target.path_override.clone(),
        capability: None,
        extension_overrides: target.extension_overrides.clone(),
        ..ResolveOptions::default()
    };
    let context = execution_context::resolve_with_component(&options, target.component_override)?;
    let (extension, config) = resolve_workload_execution_context(&context.component, kind)?;
    Ok(ResolvedWorkloadKind {
        kind: kind.to_string(),
        context,
        extension,
        config,
    })
}
