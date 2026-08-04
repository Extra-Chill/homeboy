use super::{LabOffloadCommand, RunnerCapabilityPreflight, RunnerToolchainReadinessProbe};

/// Resolve extension-owned usable-toolchain probes before any Lab workspace is
/// hydrated. The operation label is generic Homeboy command metadata; extension
/// manifests own the language, tool, probe, and repair semantics.
pub(super) fn toolchain_readiness_preflight(
    command: &LabOffloadCommand,
) -> homeboy_core::Result<Option<RunnerCapabilityPreflight>> {
    toolchain_readiness_preflight_for_extensions(command.hot_label, &command.required_extensions)
}

/// Compile probes from extension manifests only. This is deliberately crate
/// private: public preflight inputs select a typed workload, never probe text.
pub(crate) fn toolchain_readiness_preflight_for_extensions(
    command: &str,
    extensions: &[String],
) -> homeboy_core::Result<Option<RunnerCapabilityPreflight>> {
    let operation = command.split_whitespace().last().unwrap_or_default();
    let mut probes = Vec::new();
    for extension_id in extensions {
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
                program: probe.program.clone(),
                args: probe.args.clone(),
                repair_command: probe.repair_command.clone(),
                diagnostic_env: probe.diagnostic_env.clone(),
            });
        }
    }
    Ok((!probes.is_empty()).then(|| RunnerCapabilityPreflight {
        command: command.to_string(),
        required_toolchain_probes: probes,
        ..Default::default()
    }))
}
