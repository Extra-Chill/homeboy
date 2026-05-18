//! Rig-owned extension workload resolution.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::core::engine::invocation::InvocationRequirements;

use super::spec::RigSpec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RigWorkloadKind {
    Bench,
    Trace,
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
    let workloads = match kind {
        RigWorkloadKind::Bench => &rig_spec.bench_workloads,
        RigWorkloadKind::Trace => &rig_spec.trace_workloads,
    };

    workloads
        .get(extension_id)
        .into_iter()
        .flat_map(|paths| paths.iter())
        .map(|workload| expand_workload_path(rig_spec, package_root, workload.path()))
        .collect()
}

/// Return the scoped check groups required by all rig-owned workloads for an
/// extension.
///
/// `None` means at least one relevant workload uses the legacy string shorthand
/// (or the extension declares no rig-owned workloads), so callers should keep
/// the historical full `rig check` behaviour. `Some(groups)` means every
/// workload opted into scoped preflights; an empty vector intentionally means no
/// rig check-pipeline step is required.
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
    let expanded = super::expand::expand_vars(rig_spec, path);
    let expanded = match package_root {
        Some(root) => expanded.replace("${package.root}", &root.to_string_lossy()),
        None => expanded,
    };
    PathBuf::from(expanded)
}

#[cfg(test)]
#[path = "../../../tests/core/rig/workloads_test.rs"]
mod workloads_test;
