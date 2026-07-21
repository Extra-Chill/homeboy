//! Human-facing resource-policy warning and remediation message builders.
//!
//! Extracted from the resource-policy module root to keep it under the god-file
//! line threshold (#9279). These are pure formatting helpers over the already
//! decided [`HotCommand`], resource severity, and Lab-runner readiness — they
//! make no policy decisions of their own.

use super::{HotCommand, ResourcePolicyWarning};
use crate::commands::resources::{DoctorOutput, ResourceRecommendation};
use crate::runner::runners::LabRunnerReadiness;

pub(super) fn severity_str(recommendation: ResourceRecommendation) -> &'static str {
    match recommendation {
        ResourceRecommendation::Ok => "ok",
        ResourceRecommendation::Warm => "warm",
        ResourceRecommendation::Hot => "hot",
    }
}

pub(super) fn append_local_placement(rerun: &mut Vec<String>, args: &[String]) {
    if !args
        .iter()
        .any(|arg| arg == "--placement" || arg.starts_with("--placement="))
    {
        rerun.push("--placement".to_string());
        rerun.push("local".to_string());
    }
}

pub(super) fn primary_action(
    warning: &ResourcePolicyWarning,
    default_runner: Option<&str>,
) -> String {
    // A portable command's warning names either a concrete default runner
    // ("Lab runner `<id>`") or, when none is configured, states Lab offload is
    // unavailable. Local-only commands say neither. Branch on those facts rather
    // than inferring Lab availability from placeholder substrings.
    if let Some(runner_id) = default_runner {
        if warning.message.contains(&format!("`{runner_id}`")) {
            return format!(
                "Homeboy found Lab runner `{runner_id}`; rerun with --runner {runner_id} or let automatic Lab routing handle this portable command."
            );
        }
    }
    if warning
        .message
        .contains("Lab offload is not currently available")
    {
        // Portable command, but no Lab runner is configured on this host (#7749):
        // do not point the operator at nonexistent Lab infrastructure.
        return "No Homeboy Lab runner is configured on this host, so Lab offload is not available. Defer verification to CI, connect a runner with `homeboy runner connect`, or use the local-hot rerun command only if local execution is explicitly authorized.".to_string();
    }
    if warning.message.contains("--runner") {
        "Pass --runner <id> when Lab offload supports this mode.".to_string()
    } else {
        "Lab routing is not offered because this command is currently local-only under resource policy.".to_string()
    }
}

pub(super) fn warning_message(
    command: HotCommand,
    recommendation: ResourceRecommendation,
    resources: &DoctorOutput,
    lab_readiness: Option<&LabRunnerReadiness>,
) -> String {
    let severity = severity_str(recommendation);
    let reason = primary_reason(resources);
    if command.lab_offload_supported {
        if let Some(readiness) = lab_readiness {
            if let Some(runner_id) = readiness.selected_runner_id.as_deref() {
                return format!(
                "Resource policy warning: machine is {severity}; starting `{}` locally may skew results or add pressure. {reason} Homeboy found Lab runner `{runner_id}`; use --runner {runner_id} to route this portable command through Lab offload, or omit local-hot overrides so automatic Lab routing can use it.",
                command.label
            );
            }
            return format!(
                "Resource policy warning: machine is {severity}; starting `{}` may skew results or add pressure. {reason} Lab runner inventory is {} (available runner IDs: {}). {}",
                command.label,
                readiness.state.as_str(),
                if readiness.available_runner_ids.is_empty() { "none".to_string() } else { readiness.available_runner_ids.join(", ") },
                readiness.remediation_commands.join("; "),
            );
        }
        return format!(
            "Resource policy warning: machine is {severity}; starting `{}` may skew results or add pressure. {reason} No Homeboy Lab runner is configured on this host, so Lab offload is not currently available; connect a runner (`homeboy runner connect`) to enable it, defer verification to CI, or use --placement local to run locally without this warning.",
            command.label
        );
    }

    let local_only_reason = command.lab_offload_unsupported_reason.unwrap_or(
        "This resource-pressure command does not have a portable Lab offload contract yet.",
    );
    format!(
        "Resource policy warning: machine is {severity}; starting `{}` may skew results or add pressure. {reason} {local_only_reason} Use --placement local to run locally without this warning.",
        command.label
    )
}

fn primary_reason(resources: &DoctorOutput) -> String {
    if resources.load.recommendation == ResourceRecommendation::Hot
        || resources.load.recommendation == ResourceRecommendation::Warm
    {
        if let Some(one) = resources.load.one {
            return format!(
                "Load average is {one:.1} across {} CPU(s).",
                resources.load.cpu_count
            );
        }
        return "Load average is elevated.".to_string();
    }

    if let Some(memory) = &resources.memory {
        if memory.recommendation == ResourceRecommendation::Hot
            || memory.recommendation == ResourceRecommendation::Warm
        {
            return format!(
                "Memory is {:.1}% used ({} MB available).",
                memory.used_percent, memory.available_mb
            );
        }
    }

    if resources.processes.recommendation == ResourceRecommendation::Hot
        || resources.processes.recommendation == ResourceRecommendation::Warm
    {
        return format!(
            "{} relevant Homeboy-adjacent process(es) are already active.",
            resources.processes.relevant_count
        );
    }

    if resources.rig_leases.recommendation == ResourceRecommendation::Hot
        || resources.rig_leases.recommendation == ResourceRecommendation::Warm
    {
        return format!(
            "{} rig run lease(s) are already active.",
            resources.rig_leases.active_count
        );
    }

    "Run `homeboy self doctor` for details.".to_string()
}
