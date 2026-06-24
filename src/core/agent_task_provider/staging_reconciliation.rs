//! Runtime dependency conflict reconciliation for staged provider plugins
//! (#6223).
//!
//! A provider plugin can carry a vendored runtime library that SHADOWS the
//! runtime-provided version (e.g. the runtime/overlay supplies a Composer
//! package that the staged plugin also vendors). When that happens, plugin
//! activation can fatal at runtime. Homeboy owns the contract and the
//! readiness check at the orchestration boundary: before dispatch it validates
//! the *effective staged plugin* against the provider's declared runtime
//! staging contract and refuses the run with an actionable owner/package/
//! contract message instead of letting the runtime emit a raw fatal.
//!
//! Both Lab and local execution funnel through the same
//! [`reconcile_staged_plugins`] check, so the reconciled staging contract is
//! shared rather than re-implemented per execution surface.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::core::agent_task::AgentTaskComponentContract;
use crate::core::{Error, Result};

use super::runtime_types::{AgentTaskRuntimeReconciledPackage, AgentTaskRuntimeStagingContract};

/// A single detected conflict: a staged plugin vendors a package the runtime
/// owns. Carries enough provenance to name the owner, the package, and the
/// exact shadowing path in an actionable error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StagingReconciliationConflict {
    /// The staged plugin (component contract slug) that shadows a runtime package.
    pub plugin: String,
    /// The package the runtime owns (e.g. a Composer package name).
    pub package: String,
    /// The runtime/overlay that supplies the canonical copy of the package.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// The staged-plugin-relative path whose presence proves the shadowing.
    pub vendored_subpath: String,
    /// The absolute path inside the staged plugin that vendors the package.
    pub vendored_path: String,
    /// Optional per-package remediation declared by the contract.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

/// Structured readiness outcome of reconciling a staged plugin against the
/// runtime staging contract. Mirrors the shape a runtime/Codebox can return so
/// Homeboy can either compute readiness itself or accept a structured result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct StagingReadiness {
    /// True when no staged plugin shadows a runtime-owned package.
    pub ready: bool,
    /// Conflicts found, empty when ready.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<StagingReconciliationConflict>,
}

impl StagingReadiness {
    fn ready() -> Self {
        Self {
            ready: true,
            conflicts: Vec::new(),
        }
    }
}

/// Reconcile staged plugin component contracts against a provider's runtime
/// staging contract. Returns the structured readiness so callers can record it
/// as evidence; use [`ensure_staged_plugins_reconciled`] for the gating variant
/// that turns a conflict into an actionable pre-dispatch error.
pub fn reconcile_staged_plugins(
    contract: &AgentTaskRuntimeStagingContract,
    component_contracts: &[AgentTaskComponentContract],
) -> StagingReadiness {
    if contract.reconciled_packages.is_empty() {
        return StagingReadiness::ready();
    }

    let mut conflicts = Vec::new();
    for component in component_contracts {
        if !contract_is_staged_plugin(component) {
            continue;
        }
        let Some(plugin_root) = component
            .path
            .as_deref()
            .filter(|path| !path.trim().is_empty())
        else {
            continue;
        };
        let plugin_root = PathBuf::from(plugin_root);
        let plugin_label = component
            .slug
            .clone()
            .or_else(|| {
                plugin_root
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| plugin_root.display().to_string());

        for package in &contract.reconciled_packages {
            collect_package_conflicts(&plugin_root, &plugin_label, package, &mut conflicts);
        }
    }

    if conflicts.is_empty() {
        StagingReadiness::ready()
    } else {
        StagingReadiness {
            ready: false,
            conflicts,
        }
    }
}

/// Gate a dispatch on the reconciled staging contract. When the contract
/// enforces pre-dispatch validation and a staged plugin shadows a runtime
/// package, this returns an actionable error that names the owner, the package,
/// and the shadowing contract — not a raw runtime fatal.
pub fn ensure_staged_plugins_reconciled(
    contract: &AgentTaskRuntimeStagingContract,
    component_contracts: &[AgentTaskComponentContract],
) -> Result<StagingReadiness> {
    let readiness = reconcile_staged_plugins(contract, component_contracts);
    if readiness.ready || !contract.enforces_pre_dispatch() {
        return Ok(readiness);
    }
    Err(staging_reconciliation_error(contract, &readiness.conflicts))
}

fn contract_is_staged_plugin(component: &AgentTaskComponentContract) -> bool {
    // Treat any component the runtime loads/activates as a plugin as in scope.
    // `loadAs == "plugin"` is the canonical signal; `activate` also implies a
    // loaded plugin. Core stays runtime-agnostic — it never assumes WordPress.
    component
        .load_as
        .as_deref()
        .is_some_and(|load_as| load_as.eq_ignore_ascii_case("plugin"))
        || component.activate == Some(true)
}

fn collect_package_conflicts(
    plugin_root: &Path,
    plugin_label: &str,
    package: &AgentTaskRuntimeReconciledPackage,
    conflicts: &mut Vec<StagingReconciliationConflict>,
) {
    for subpath in package.effective_vendor_subpaths() {
        let vendored_path = plugin_root.join(&subpath);
        if vendored_path.exists() {
            conflicts.push(StagingReconciliationConflict {
                plugin: plugin_label.to_string(),
                package: package.name.clone(),
                owner: package.owner.clone(),
                vendored_subpath: subpath,
                vendored_path: vendored_path.display().to_string(),
                remediation: package.remediation.clone(),
            });
        }
    }
}

fn staging_reconciliation_error(
    contract: &AgentTaskRuntimeStagingContract,
    conflicts: &[StagingReconciliationConflict],
) -> Error {
    let summary = conflicts
        .iter()
        .map(|conflict| {
            let owner = conflict.owner.as_deref().unwrap_or("the runtime");
            format!(
                "staged plugin `{}` vendors `{}` (owned by `{}`) at `{}`",
                conflict.plugin, conflict.package, owner, conflict.vendored_subpath
            )
        })
        .collect::<Vec<_>>()
        .join("; ");

    let mut hints = vec![
        "Rebuild the staged plugin without the vendored runtime package so the runtime-provided version is used.".to_string(),
        "This is a runtime dependency contract conflict reconciled by Homeboy before dispatch, not a task failure — no task cells were queued.".to_string(),
    ];
    for conflict in conflicts {
        if let Some(owner) = conflict.owner.as_deref() {
            hints.push(format!(
                "`{}` is supplied by `{}`; remove `{}` from staged plugin `{}` so it cannot shadow the runtime copy and fatal on activation.",
                conflict.package, owner, conflict.vendored_path, conflict.plugin
            ));
        }
        if let Some(remediation) = conflict.remediation.as_deref() {
            hints.push(remediation.to_string());
        }
    }
    if let Some(remediation) = contract.remediation.as_deref() {
        hints.push(remediation.to_string());
    }

    let details = serde_json::json!({
        "conflicts": conflicts,
    })
    .to_string();

    Error::validation_invalid_argument(
        "staged_plugin",
        format!(
            "Homeboy refused dispatch: runtime dependency reconciliation found {} staged plugin package conflict(s): {summary}. A vendored runtime library would shadow the runtime-provided version and fatal during plugin activation.",
            conflicts.len()
        ),
        Some(details),
        Some(hints),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::agent_task::AgentTaskComponentContract;

    fn package(name: &str, owner: &str) -> AgentTaskRuntimeReconciledPackage {
        AgentTaskRuntimeReconciledPackage {
            name: name.to_string(),
            owner: Some(owner.to_string()),
            ..AgentTaskRuntimeReconciledPackage::default()
        }
    }

    fn plugin_contract(slug: &str, path: &Path) -> AgentTaskComponentContract {
        AgentTaskComponentContract {
            slug: Some(slug.to_string()),
            path: Some(path.display().to_string()),
            load_as: Some("plugin".to_string()),
            activate: Some(true),
            extra: Default::default(),
        }
    }

    #[test]
    fn ready_when_no_vendored_package_shadows_runtime() {
        let plugin = tempfile::tempdir().expect("plugin dir");
        let contract = AgentTaskRuntimeStagingContract {
            reconciled_packages: vec![package("acme/runtime-lib", "wordpress-7.0")],
            ..AgentTaskRuntimeStagingContract::default()
        };

        let readiness = ensure_staged_plugins_reconciled(
            &contract,
            &[plugin_contract("provider-plugin", plugin.path())],
        )
        .expect("clean staged plugin reconciles");

        assert!(readiness.ready);
        assert!(readiness.conflicts.is_empty());
    }

    #[test]
    fn conflict_names_owner_package_and_path() {
        let plugin = tempfile::tempdir().expect("plugin dir");
        let vendored = plugin.path().join("vendor/acme/runtime-lib");
        std::fs::create_dir_all(&vendored).expect("create vendored dir");
        let contract = AgentTaskRuntimeStagingContract {
            reconciled_packages: vec![package("acme/runtime-lib", "wordpress-7.0")],
            ..AgentTaskRuntimeStagingContract::default()
        };

        let err = ensure_staged_plugins_reconciled(
            &contract,
            &[plugin_contract("provider-plugin", plugin.path())],
        )
        .expect_err("shadowed runtime package is refused before dispatch");

        assert_eq!(err.details["field"], "staged_plugin");
        assert!(err.message.contains("acme/runtime-lib"));
        assert!(err.message.contains("wordpress-7.0"));
        assert!(err.message.contains("provider-plugin"));
        assert!(err.message.contains("shadow"));
        let tried = err.details["tried"].as_array().expect("hints");
        assert!(tried.iter().any(|hint| hint
            .as_str()
            .is_some_and(|hint| hint.contains("Rebuild the staged plugin"))));
        // The structured conflict provenance is preserved in the error id payload.
        let conflicts: serde_json::Value =
            serde_json::from_str(err.details["id"].as_str().expect("id payload"))
                .expect("conflicts json");
        assert_eq!(conflicts["conflicts"][0]["package"], "acme/runtime-lib");
        assert_eq!(conflicts["conflicts"][0]["owner"], "wordpress-7.0");
    }

    #[test]
    fn declared_vendor_subpaths_override_default() {
        let plugin = tempfile::tempdir().expect("plugin dir");
        let vendored = plugin.path().join("lib/embedded/runtime-lib");
        std::fs::create_dir_all(&vendored).expect("create vendored dir");
        let contract = AgentTaskRuntimeStagingContract {
            reconciled_packages: vec![AgentTaskRuntimeReconciledPackage {
                name: "acme/runtime-lib".to_string(),
                owner: Some("wordpress-7.0".to_string()),
                vendor_subpaths: vec!["lib/embedded/runtime-lib".to_string()],
                ..AgentTaskRuntimeReconciledPackage::default()
            }],
            ..AgentTaskRuntimeStagingContract::default()
        };

        let readiness = reconcile_staged_plugins(
            &contract,
            &[plugin_contract("provider-plugin", plugin.path())],
        );

        assert!(!readiness.ready);
        assert_eq!(readiness.conflicts.len(), 1);
        assert_eq!(
            readiness.conflicts[0].vendored_subpath,
            "lib/embedded/runtime-lib"
        );
    }

    #[test]
    fn validate_before_dispatch_false_records_without_gating() {
        let plugin = tempfile::tempdir().expect("plugin dir");
        std::fs::create_dir_all(plugin.path().join("vendor/acme/runtime-lib"))
            .expect("create vendored dir");
        let contract = AgentTaskRuntimeStagingContract {
            reconciled_packages: vec![package("acme/runtime-lib", "wordpress-7.0")],
            validate_before_dispatch: Some(false),
            ..AgentTaskRuntimeStagingContract::default()
        };

        // Codebox-delegated readiness: the contract is recorded and conflicts are
        // still computed for evidence, but Homeboy does not hard-gate dispatch.
        let readiness = ensure_staged_plugins_reconciled(
            &contract,
            &[plugin_contract("provider-plugin", plugin.path())],
        )
        .expect("non-enforcing contract does not gate dispatch");

        assert!(!readiness.ready);
        assert_eq!(readiness.conflicts.len(), 1);
    }

    #[test]
    fn non_plugin_component_is_ignored() {
        let plugin = tempfile::tempdir().expect("plugin dir");
        std::fs::create_dir_all(plugin.path().join("vendor/acme/runtime-lib"))
            .expect("create vendored dir");
        let contract = AgentTaskRuntimeStagingContract {
            reconciled_packages: vec![package("acme/runtime-lib", "wordpress-7.0")],
            ..AgentTaskRuntimeStagingContract::default()
        };
        let component = AgentTaskComponentContract {
            slug: Some("not-a-plugin".to_string()),
            path: Some(plugin.path().display().to_string()),
            load_as: Some("library".to_string()),
            activate: Some(false),
            extra: Default::default(),
        };

        let readiness = ensure_staged_plugins_reconciled(&contract, &[component])
            .expect("non-plugin components are out of scope");

        assert!(readiness.ready);
    }
}
