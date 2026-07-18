//! Rig-owned extension workload resolution.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use homeboy_core::engine::invocation::InvocationRequirements;
use homeboy_core::{Error, Result};

use super::spec::{RigSpec, TraceDependencySpec};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RigWorkloadKind {
    Bench,
    Fuzz,
    Trace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RigWorkloadPathExpansion {
    pub declared_path: String,
    pub expanded_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RigExtensionWorkloadInputs {
    pub workload_paths: Vec<PathBuf>,
    pub env_provider_extensions: Vec<String>,
    pub invocation_requirements: InvocationRequirements,
}

pub fn component_ids_for_workload(
    rig_spec: &RigSpec,
    kind: RigWorkloadKind,
    explicit_component: Option<&str>,
) -> Vec<String> {
    if let Some(component) = explicit_component {
        return vec![component.to_string()];
    }

    match kind {
        RigWorkloadKind::Bench => match rig_spec.bench.as_ref() {
            Some(bench) if !bench.components.is_empty() => bench.components.clone(),
            Some(bench) => bench.default_component.iter().cloned().collect(),
            None => Vec::new(),
        },
        RigWorkloadKind::Fuzz => rig_spec
            .fuzz
            .iter()
            .flat_map(|fuzz| fuzz.default_component.iter().cloned())
            .collect(),
        RigWorkloadKind::Trace => Vec::new(),
    }
}

pub fn required_component_id_for_workload(
    rig_spec: &RigSpec,
    kind: RigWorkloadKind,
    explicit_component: Option<&str>,
) -> Result<String> {
    if let Some(component) = component_ids_for_workload(rig_spec, kind, explicit_component)
        .into_iter()
        .next()
    {
        return Ok(component);
    }

    let setting = match kind {
        RigWorkloadKind::Bench => "bench.default_component",
        RigWorkloadKind::Fuzz => "fuzz.default_component",
        RigWorkloadKind::Trace => "trace.default_component",
    };
    Err(Error::validation_invalid_argument(
        setting,
        format!(
            "rig '{}' does not declare {setting}; pass a component id or add {setting} to the rig spec",
            rig_spec.id
        ),
        None,
        None,
    ))
}

pub fn extension_ids_for_workloads(rig_spec: &RigSpec, kind: RigWorkloadKind) -> Vec<String> {
    let mut ids: Vec<String> = match kind {
        RigWorkloadKind::Bench => rig_spec.bench_workloads.keys().cloned().collect(),
        RigWorkloadKind::Fuzz => rig_spec.fuzz_workloads.keys().cloned().collect(),
        RigWorkloadKind::Trace => rig_spec.trace_workloads.keys().cloned().collect(),
    };
    ids.sort();
    ids
}

pub fn required_extension_ids_for_workload(
    rig_spec: &RigSpec,
    kind: RigWorkloadKind,
    explicit_component: Option<&str>,
) -> Vec<String> {
    let workload_extensions = extension_ids_for_workloads(rig_spec, kind);
    let mut extension_ids = BTreeSet::new();
    for extension_id in &workload_extensions {
        extension_ids.extend(env_provider_extensions_for_extension_workloads(
            rig_spec,
            kind,
            extension_id,
        ));
    }
    extension_ids.extend(workload_extensions);
    extension_ids.extend(component_extension_ids_for_workload(
        rig_spec,
        kind,
        explicit_component,
    ));
    extension_ids.into_iter().collect()
}

pub fn component_extension_ids_for_workload(
    rig_spec: &RigSpec,
    kind: RigWorkloadKind,
    explicit_component: Option<&str>,
) -> Vec<String> {
    let component_ids = component_ids_for_workload(rig_spec, kind, explicit_component);

    component_ids
        .iter()
        .filter_map(|component_id| rig_spec.components.get(component_id))
        .filter_map(|component| component.extensions.as_ref())
        .flat_map(|extensions| extensions.keys().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub fn workloads_for_extension(
    rig_spec: &RigSpec,
    kind: RigWorkloadKind,
    package_root: Option<&Path>,
    extension_id: &str,
) -> Vec<PathBuf> {
    workload_path_expansions_for_extension(rig_spec, kind, package_root, extension_id)
        .into_iter()
        .map(|expansion| expansion.expanded_path)
        .collect()
}

pub fn extension_workload_inputs(
    rig_spec: &RigSpec,
    kind: RigWorkloadKind,
    package_root: Option<&Path>,
    extension_id: &str,
) -> RigExtensionWorkloadInputs {
    RigExtensionWorkloadInputs {
        workload_paths: workloads_for_extension(rig_spec, kind, package_root, extension_id),
        env_provider_extensions: env_provider_extensions_for_extension_workloads(
            rig_spec,
            kind,
            extension_id,
        ),
        invocation_requirements: invocation_requirements_for_extension_workloads(
            rig_spec,
            kind,
            extension_id,
        ),
    }
}

pub fn workload_path_expansions_for_extension(
    rig_spec: &RigSpec,
    kind: RigWorkloadKind,
    package_root: Option<&Path>,
    extension_id: &str,
) -> Vec<RigWorkloadPathExpansion> {
    let workloads = match kind {
        RigWorkloadKind::Bench => &rig_spec.bench_workloads,
        RigWorkloadKind::Fuzz => &rig_spec.fuzz_workloads,
        RigWorkloadKind::Trace => &rig_spec.trace_workloads,
    };

    workloads
        .get(extension_id)
        .into_iter()
        .flat_map(|paths| paths.iter())
        .map(|workload| RigWorkloadPathExpansion {
            declared_path: workload.path().to_string(),
            expanded_path: expand_workload_path(rig_spec, package_root, workload.path()),
        })
        .collect()
}

pub fn env_provider_extensions_for_extension_workloads(
    rig_spec: &RigSpec,
    kind: RigWorkloadKind,
    extension_id: &str,
) -> Vec<String> {
    let workloads = match kind {
        RigWorkloadKind::Bench => &rig_spec.bench_workloads,
        RigWorkloadKind::Fuzz => &rig_spec.fuzz_workloads,
        RigWorkloadKind::Trace => &rig_spec.trace_workloads,
    };
    let Some(entries) = workloads.get(extension_id) else {
        return Vec::new();
    };

    let mut extensions = BTreeSet::new();
    for entry in entries {
        extensions.extend(
            entry
                .env_provider_extensions()
                .iter()
                .filter(|extension| !extension.is_empty())
                .cloned(),
        );
    }

    extensions.into_iter().collect()
}

pub fn trace_dependencies_for_extension(
    rig_spec: &RigSpec,
    package_root: Option<&Path>,
    extension_id: &str,
) -> Vec<TraceDependencySpec> {
    let Some(entries) = rig_spec.trace_workloads.get(extension_id) else {
        return Vec::new();
    };

    let mut dependencies = Vec::new();
    for workload in entries {
        for dependency in workload.trace_dependencies() {
            let mut dependency = dependency.clone();
            if let Some(path) = dependency.path.as_deref() {
                dependency.path = Some(
                    expand_workload_path(rig_spec, package_root, path)
                        .to_string_lossy()
                        .to_string(),
                );
            }
            dependencies.push(dependency);
        }
    }
    dependencies
}

pub fn runner_capabilities_for_extension(rig_spec: &RigSpec, extension_id: &str) -> Vec<String> {
    let Some(entries) = rig_spec.trace_workloads.get(extension_id) else {
        return Vec::new();
    };

    let mut capabilities = BTreeSet::new();
    for workload in entries {
        capabilities.extend(
            workload
                .runner_capabilities()
                .iter()
                .filter(|capability| !capability.is_empty())
                .cloned(),
        );
    }
    capabilities.into_iter().collect()
}

/// Return the scoped check groups required by all rig-owned workloads for an
/// extension.
///
/// `None` means at least one relevant workload omits `check_groups` (or the
/// extension declares no rig-owned workloads), so callers should keep the full
/// `rig check` behaviour. `Some(groups)` means every workload opted into scoped
/// preflights; an empty vector intentionally means no rig check-pipeline step is
/// required.
pub fn check_groups_for_extension_workloads(
    rig_spec: &RigSpec,
    kind: RigWorkloadKind,
    extension_id: &str,
) -> Option<Vec<String>> {
    let workloads = match kind {
        RigWorkloadKind::Bench => &rig_spec.bench_workloads,
        RigWorkloadKind::Fuzz => &rig_spec.fuzz_workloads,
        RigWorkloadKind::Trace => &rig_spec.trace_workloads,
    };
    let entries = workloads.get(extension_id)?;

    let mut groups = BTreeSet::new();
    for workload in entries {
        let required = workload.check_groups()?;
        groups.extend(required.iter().filter(|group| !group.is_empty()).cloned());
    }

    Some(groups.into_iter().collect())
}

/// Return scenario-scoped bench preflight groups.
///
/// `None` preserves full `rig check` when no scenario is selected or any
/// selected scenario has no explicit mapping. `Some(groups)` means the bench
/// spec opted every selected scenario into scoped preflight checks.
pub fn check_groups_for_bench_scenarios(
    rig_spec: &RigSpec,
    scenario_ids: &[String],
) -> Option<Vec<String>> {
    if scenario_ids.is_empty() {
        return None;
    }
    let bench = rig_spec.bench.as_ref()?;

    let mut groups = BTreeSet::new();
    groups.extend(
        bench
            .check_groups
            .iter()
            .filter(|group| !group.is_empty())
            .cloned(),
    );
    for scenario_id in scenario_ids {
        let scenario_groups = bench.scenario_check_groups.get(scenario_id)?;
        groups.extend(
            scenario_groups
                .iter()
                .filter(|group| !group.is_empty())
                .cloned(),
        );
    }

    Some(groups.into_iter().collect())
}

pub fn invocation_requirements_for_extension_workloads(
    rig_spec: &RigSpec,
    kind: RigWorkloadKind,
    extension_id: &str,
) -> InvocationRequirements {
    let workloads = match kind {
        RigWorkloadKind::Bench => &rig_spec.bench_workloads,
        RigWorkloadKind::Fuzz => &rig_spec.fuzz_workloads,
        RigWorkloadKind::Trace => &rig_spec.trace_workloads,
    };
    let Some(entries) = workloads.get(extension_id) else {
        return InvocationRequirements::default();
    };

    let port_range_size = entries
        .iter()
        .filter_map(|entry| entry.port_range_size())
        .max();
    let mut named_leases = BTreeSet::new();
    for entry in entries {
        named_leases.extend(
            entry
                .named_leases()
                .iter()
                .filter(|name| !name.is_empty())
                .cloned(),
        );
    }

    InvocationRequirements {
        port_range_size,
        named_leases: named_leases.into_iter().collect(),
    }
}

fn expand_workload_path(rig_spec: &RigSpec, package_root: Option<&Path>, path: &str) -> PathBuf {
    let path = match package_root {
        Some(root) => path.replace("${package.root}", &root.to_string_lossy()),
        None => path.to_string(),
    };
    PathBuf::from(super::expand::expand_vars(rig_spec, &path))
}

#[cfg(test)]
#[path = "../../../../tests/core/rig/workloads_test.rs"]
mod workloads_test;
