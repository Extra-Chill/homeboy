//! Rig-owned extension workload resolution.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use homeboy_core::engine::invocation::InvocationRequirements;
use homeboy_core::{Error, Result};

use super::spec::{
    LifecycleContract, LifecycleWorkloadKind, LifecycleWorkloadRef, RigSpec, TraceDependencySpec,
    WorkloadSpec,
};

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

impl From<LifecycleWorkloadKind> for RigWorkloadKind {
    fn from(kind: LifecycleWorkloadKind) -> Self {
        match kind {
            LifecycleWorkloadKind::Bench => RigWorkloadKind::Bench,
            LifecycleWorkloadKind::Fuzz => RigWorkloadKind::Fuzz,
            LifecycleWorkloadKind::Trace => RigWorkloadKind::Trace,
        }
    }
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

/// Resolve the `lifecycle` contract a rig-owned workload entry declares.
///
/// This is the reader that makes `WorkloadSpec.lifecycle` load-bearing: a
/// `kind: "lifecycle"` pipeline step names a workload instead of restating the
/// contract, so one declaration serves every op the rig runs against it.
///
/// Fail-closed by construction — every ambiguity is an error, never a guess:
///
/// - unknown extension key → error naming the keys that are declared
/// - `path` matching no declared workload → error naming the declared paths
/// - no `path` and zero or multiple workloads carrying a contract → error
/// - selected workload declares no `lifecycle` → error
pub fn workload_lifecycle_contract<'a>(
    rig_spec: &'a RigSpec,
    reference: &LifecycleWorkloadRef,
) -> Result<&'a LifecycleContract> {
    let kind_label = reference.kind.as_str();
    let setting = format!("pipeline.lifecycle.workload.{kind_label}");
    let extension = reference.extension.as_str();
    let rig_id = rig_spec.id.as_str();
    let workloads = workload_map(rig_spec, RigWorkloadKind::from(reference.kind));

    let entries: &[WorkloadSpec] = match workloads.get(extension) {
        Some(entries) => entries.as_slice(),
        None => {
            let mut declared = workloads.keys().cloned().collect::<Vec<String>>();
            declared.sort();
            return Err(workload_ref_error(
                &setting,
                format!(
                    "rig '{rig_id}' declares no {kind_label}_workloads for extension '{extension}'{}",
                    declared_suffix("declared extensions", &declared)
                ),
            ));
        }
    };

    let selected: &WorkloadSpec = match reference.path.as_deref() {
        Some(path) => match entries.iter().find(|entry| entry.path() == path) {
            Some(entry) => entry,
            None => {
                let declared = entries
                    .iter()
                    .map(|entry| entry.path().to_string())
                    .collect::<Vec<String>>();
                return Err(workload_ref_error(
                    &setting,
                    format!(
                        "rig '{rig_id}' declares no {kind_label} workload with path '{path}' for extension '{extension}'{}",
                        declared_suffix("declared paths", &declared)
                    ),
                ));
            }
        },
        None => {
            let mut candidates = entries
                .iter()
                .filter(|entry| entry.lifecycle().is_some())
                .collect::<Vec<&WorkloadSpec>>();
            if candidates.len() != 1 {
                let declared = candidates
                    .iter()
                    .map(|entry| entry.path().to_string())
                    .collect::<Vec<String>>();
                let problem = if candidates.is_empty() {
                    format!(
                        "no {kind_label} workload for extension '{extension}' in rig '{rig_id}' declares a lifecycle contract"
                    )
                } else {
                    format!(
                        "{} {kind_label} workloads for extension '{extension}' in rig '{rig_id}' declare a lifecycle contract; set `workload.path` to pick one{}",
                        candidates.len(),
                        declared_suffix("candidates", &declared)
                    )
                };
                return Err(workload_ref_error(&setting, problem));
            }
            candidates.remove(0)
        }
    };

    match selected.lifecycle() {
        Some(contract) => Ok(contract),
        None => Err(workload_ref_error(
            &setting,
            format!(
                "{kind_label} workload '{}' for extension '{extension}' in rig '{rig_id}' declares no lifecycle contract",
                selected.path()
            ),
        )),
    }
}

fn workload_ref_error(setting: &str, problem: String) -> Error {
    Error::validation_invalid_argument(setting, problem, None, None)
}

fn declared_suffix(label: &str, values: &[String]) -> String {
    if values.is_empty() {
        String::new()
    } else {
        format!(" ({label}: {})", values.join(", "))
    }
}

fn workload_map(
    rig_spec: &RigSpec,
    kind: RigWorkloadKind,
) -> &std::collections::HashMap<String, Vec<WorkloadSpec>> {
    match kind {
        RigWorkloadKind::Bench => &rig_spec.bench_workloads,
        RigWorkloadKind::Fuzz => &rig_spec.fuzz_workloads,
        RigWorkloadKind::Trace => &rig_spec.trace_workloads,
    }
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
#[path = "../../../tests/core/rig/workloads_test.rs"]
mod workloads_test;
