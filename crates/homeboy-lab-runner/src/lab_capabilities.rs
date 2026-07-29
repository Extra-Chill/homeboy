use std::path::Path;

use super::{LabOffloadCommand, LabRunnerCapabilityContract, RunnerRequiredTool};
use super::{RunnerCapabilityPreflight, RunnerToolchainReadinessProbe};

pub(super) fn lab_runner_capability_contract(
    command: &LabOffloadCommand,
    source_path: &Path,
    command_prefix_required_tools: &[RunnerRequiredTool],
) -> Option<LabRunnerCapabilityContract> {
    if !command.is_portable() {
        return None;
    }

    let mut required_tools = Vec::new();

    for tool in command_prefix_required_tools {
        push_unique(&mut required_tools, tool.clone());
    }

    let _ = source_path;

    Some(LabRunnerCapabilityContract {
        command: command.hot_label,
        required_tools,
        required_capabilities: command
            .required_capabilities
            .iter()
            .map(|capability| capability.name.clone())
            .collect(),
    })
}

/// Resolve extension-owned usable-toolchain probes before any Lab workspace is
/// hydrated. The operation label is generic Homeboy command metadata; extension
/// manifests own the language, tool, probe, and repair semantics.
pub(super) fn toolchain_readiness_preflight(
    command: &LabOffloadCommand,
) -> homeboy_core::Result<Option<RunnerCapabilityPreflight>> {
    let operation = command
        .hot_label
        .split_whitespace()
        .last()
        .unwrap_or_default();
    let mut probes = Vec::new();
    for extension_id in &command.required_extensions {
        let manifest = homeboy_core::extension_store::load_extension(extension_id)?;
        for probe in &manifest.toolchain_readiness {
            if !probe.capabilities.is_empty()
                && !probe
                    .capabilities
                    .iter()
                    .any(|capability| capability == operation)
            {
                continue;
            }
            probes.push(RunnerToolchainReadinessProbe {
                extension_id: extension_id.clone(),
                id: format!("{extension_id}:{}", probe.id),
                command: probe.command.clone(),
                repair_command: probe.repair_command.clone(),
                diagnostic_env: probe.diagnostic_env.clone(),
            });
        }
    }
    Ok((!probes.is_empty()).then(|| RunnerCapabilityPreflight {
        command: command.hot_label.to_string(),
        required_toolchain_probes: probes,
        ..Default::default()
    }))
}

fn push_unique<T: PartialEq>(items: &mut Vec<T>, item: T) {
    if !items.contains(&item) {
        items.push(item);
    }
}
