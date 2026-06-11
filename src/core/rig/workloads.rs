//! Rig-owned extension workload resolution.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::core::engine::invocation::InvocationRequirements;

use super::spec::{RigSpec, TraceDependencySpec};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RigWorkloadKind {
    Bench,
    Trace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RigWorkloadPathExpansion {
    pub declared_path: String,
    pub expanded_path: PathBuf,
}

pub fn extension_ids_for_workloads(rig_spec: &RigSpec, kind: RigWorkloadKind) -> Vec<String> {
    let mut ids: Vec<String> = match kind {
        RigWorkloadKind::Bench => rig_spec.bench_workloads.keys().cloned().collect(),
        RigWorkloadKind::Trace => rig_spec.trace_workloads.keys().cloned().collect(),
    };
    ids.sort();
    ids
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

pub fn workload_path_expansions_for_extension(
    rig_spec: &RigSpec,
    kind: RigWorkloadKind,
    package_root: Option<&Path>,
    extension_id: &str,
) -> Vec<RigWorkloadPathExpansion> {
    let workloads = match kind {
        RigWorkloadKind::Bench => &rig_spec.bench_workloads,
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

pub fn invocation_requirements_for_extension_workloads(
    rig_spec: &RigSpec,
    kind: RigWorkloadKind,
    extension_id: &str,
) -> InvocationRequirements {
    let workloads = match kind {
        RigWorkloadKind::Bench => &rig_spec.bench_workloads,
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
