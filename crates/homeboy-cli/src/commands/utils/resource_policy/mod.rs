mod classification;
mod messages;

use crate::cli_surface::Commands;
use crate::command_contract::LabCommandPortability;
use crate::commands::agent_task;
use crate::runner::runners::LabRunnerReadiness;

use crate::commands::resources::{DoctorOutput, ResourceRecommendation};
use serde::Serialize;

use classification::{
    is_bounded_agent_task_metadata_read, is_controller_owned_fanout_coordination,
    is_lab_offloadable_fanout_coordinator, is_local_registry_management, is_plan_only_command,
};
use messages::{
    append_local_placement, primary_action, runner_pinned_controller_notice, severity_str,
    warning_message,
};

// The captured resource-policy context type, its process-wide store, and the
// runner-placement environment probes moved to `core::resource_policy_context`
// so `core::runner` can read them without a core -> commands dependency edge.
// Re-exported here to keep existing `resource_policy::*` call sites working.
#[cfg(test)]
pub use crate::core::resource_policy_context::reset_captured_context_for_test;
pub use crate::core::resource_policy_context::{
    capture_context, captured_context, clear_managed_runner_placement_context,
    clear_runner_hosted_exec, is_ci_execution, is_managed_runner_placement_context,
    is_runner_hosted_exec, ResourcePolicyContext, ResourcePolicyHostSnapshot,
    ResourcePolicyRunnerSelection,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotCommand {
    pub label: &'static str,
    pub lab_offload_supported: bool,
    pub lab_offload_unsupported_reason: Option<&'static str>,
    pub allows_warm_runner_coordination: bool,
    /// Mirrors the command's `LabRoutingPolicy::offload_only_when_hot`: a cheap
    /// portable command whose pressure threshold is `hot`, not `warm`. Resource
    /// admission must use the same threshold as Lab routing, so a `.cheap()`
    /// command is not warned/refused merely because the controller is `warm`
    /// (#9432).
    pub offload_only_when_hot: bool,
}

impl HotCommand {
    pub(crate) fn lab_supported(label: &'static str) -> Self {
        Self {
            label,
            lab_offload_supported: true,
            lab_offload_unsupported_reason: None,
            allows_warm_runner_coordination: false,
            offload_only_when_hot: false,
        }
    }

    fn local_only(label: &'static str, reason: Option<&'static str>) -> Self {
        Self {
            label,
            lab_offload_supported: false,
            lab_offload_unsupported_reason: reason,
            allows_warm_runner_coordination: false,
            offload_only_when_hot: false,
        }
    }

    /// The controller-side pressure threshold at or above which this command is
    /// warned/refused, matching `LabRoutingPolicy::should_pressure_offload`.
    /// Cheap commands only engage at `hot`; everything else engages once the
    /// machine leaves `ok` (i.e. at `warm`).
    fn engages_resource_admission(&self, recommendation: ResourceRecommendation) -> bool {
        match recommendation {
            ResourceRecommendation::Ok => false,
            ResourceRecommendation::Warm => !self.offload_only_when_hot,
            ResourceRecommendation::Hot => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourcePolicyWarning {
    pub command: &'static str,
    pub recommendation: ResourceRecommendation,
    pub message: String,
}

/// A read-only answer to whether the controller would admit Cook's requested
/// placement now. It is a snapshot, not a route or a runner reservation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CookPreviewPlacementAdmission {
    pub schema: &'static str,
    pub state: CookPreviewPlacementAdmissionState,
    pub revalidate_before_execution: bool,
    pub blockers: Vec<CookPreviewPlacementBlocker>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deferred_to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery: Option<ResourceAdmissionRecovery>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CookPreviewPlacementAdmissionState {
    Admissible,
    Blocked,
    Indeterminate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CookPreviewPlacementBlocker {
    pub id: String,
    pub detail: String,
}

const RESOURCE_ADMISSION_RECOVERY_SCHEMA: &str = "homeboy/resource-admission-recovery/v1";
const PRESSURE_RETRY_AFTER_SECONDS: u64 = 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ResourceAdmissionRecoveryKind {
    Defer,
    LocalOverride,
    RunnerConnection,
    RunnerAvailability,
    RunnerRecovery,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ResourceAdmissionRecoveryChoice {
    kind: ResourceAdmissionRecoveryKind,
    command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_after_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    requires_operator_authorization: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ResourceAdmissionRecovery {
    schema: &'static str,
    run_created: bool,
    choices: Vec<ResourceAdmissionRecoveryChoice>,
}

/// Build a structured `ResourcePolicyContext` from a `DoctorOutput`, the matched
/// hot command, an optional warning, and whether local placement was explicit.
///
/// A free function (rather than an inherent method) because `ResourcePolicyContext`
/// is defined in the `homeboy-core` crate and this construction depends on
/// CLI-layer types.
pub(crate) fn resource_policy_context_from_evaluation(
    command: HotCommand,
    resources: &DoctorOutput,
    warning: Option<&ResourcePolicyWarning>,
    local_override: bool,
    auto_local_capacity_fallback: bool,
    lab_readiness: Option<&LabRunnerReadiness>,
    runner_hosted: bool,
) -> ResourcePolicyContext {
    let runner_selection = runner_selection_context(
        command,
        local_override,
        auto_local_capacity_fallback,
        lab_readiness,
        runner_hosted,
    );
    ResourcePolicyContext {
        command: command.label.to_string(),
        severity: severity_str(resources.recommendation).to_string(),
        local_override,
        warned: warning.is_some(),
        message: warning.map(|warning| warning.message.clone()),
        runner_selection,
        host: ResourcePolicyHostSnapshot {
            load_severity: severity_str(resources.load.recommendation).to_string(),
            load_one: resources.load.one,
            load_five: resources.load.five,
            load_fifteen: resources.load.fifteen,
            cpu_count: resources.load.cpu_count,
            memory_severity: resources
                .memory
                .as_ref()
                .map(|memory| severity_str(memory.recommendation).to_string()),
            memory_used_percent: resources.memory.as_ref().map(|memory| memory.used_percent),
            memory_available_mb: resources.memory.as_ref().map(|memory| memory.available_mb),
            memory_total_mb: resources.memory.as_ref().map(|memory| memory.total_mb),
            relevant_process_count: resources.processes.relevant_count,
            process_severity: severity_str(resources.processes.recommendation).to_string(),
            active_rig_lease_count: resources.rig_leases.active_count,
            rig_lease_severity: severity_str(resources.rig_leases.recommendation).to_string(),
            rig_lease_concurrency_limit: resources.rig_leases.concurrency_limit,
        },
    }
}

/// Serialize a `ResourcePolicyContext` as the JSON value that lands inside
/// observation `metadata_json["resource_policy"]`.
pub(crate) fn resource_policy_context_to_json(
    context: &ResourcePolicyContext,
) -> serde_json::Value {
    serde_json::to_value(context).unwrap_or(serde_json::Value::Null)
}

fn runner_selection_context(
    command: HotCommand,
    local_override: bool,
    auto_local_capacity_fallback: bool,
    lab_readiness: Option<&LabRunnerReadiness>,
    runner_hosted: bool,
) -> ResourcePolicyRunnerSelection {
    if runner_hosted {
        return ResourcePolicyRunnerSelection {
            runner_id: None,
            available_runner_ids: Vec::new(),
            readiness_state: "runner_hosted".to_string(),
            readiness_reasons: Vec::new(),
            remediation_commands: Vec::new(),
            reason: "runner_hosted".to_string(),
        };
    }
    if local_override {
        return ResourcePolicyRunnerSelection {
            runner_id: None,
            available_runner_ids: Vec::new(),
            readiness_state: "local_override".to_string(),
            readiness_reasons: Vec::new(),
            remediation_commands: Vec::new(),
            reason: "placement_local_override".to_string(),
        };
    }
    if auto_local_capacity_fallback {
        let readiness = lab_readiness.expect("capacity fallback requires Lab readiness evidence");
        return ResourcePolicyRunnerSelection {
            runner_id: None,
            available_runner_ids: readiness.available_runner_ids.clone(),
            readiness_state: readiness.state.as_str().to_string(),
            readiness_reasons: readiness.reasons.clone(),
            remediation_commands: readiness.remediation_commands.clone(),
            reason: "local_capacity_fallback".to_string(),
        };
    }
    if command.lab_offload_supported {
        if let Some(readiness) = lab_readiness {
            return ResourcePolicyRunnerSelection {
                runner_id: readiness.selected_runner_id.clone(),
                available_runner_ids: readiness.available_runner_ids.clone(),
                readiness_state: readiness.state.as_str().to_string(),
                readiness_reasons: readiness.reasons.clone(),
                remediation_commands: readiness.remediation_commands.clone(),
                reason: if readiness.selected_runner_id.is_some() {
                    "default_lab_runner".to_string()
                } else {
                    "no_selectable_lab_runner".to_string()
                },
            };
        }
        return ResourcePolicyRunnerSelection {
            runner_id: None,
            available_runner_ids: Vec::new(),
            readiness_state: "absent".to_string(),
            readiness_reasons: Vec::new(),
            remediation_commands: Vec::new(),
            reason: "local_no_default_runner".to_string(),
        };
    }
    ResourcePolicyRunnerSelection {
        runner_id: None,
        available_runner_ids: Vec::new(),
        readiness_state: "not_applicable".to_string(),
        readiness_reasons: Vec::new(),
        remediation_commands: Vec::new(),
        reason: "local_only_contract".to_string(),
    }
}

pub(crate) fn hot_command(command: &Commands) -> Option<HotCommand> {
    if is_cook_preview(command)
        || is_plan_only_command(command)
        || is_controller_owned_fanout_coordination(command)
        || is_bounded_agent_task_metadata_read(command)
        || is_local_registry_management(command)
    {
        return None;
    }

    if matches!(
        command,
        Commands::AgentTask(agent_task::AgentTaskArgs {
            command: agent_task::AgentTaskCommand::Cook(cook),
        }) if cook.dispatch.core.queue_only
    ) {
        return None;
    }

    // Cook's controller remains local, but its materialized provider attempt
    // can run on Lab. Resource policy must recommend that supported boundary,
    // rather than treating the whole coordinator as an unavailable offload.
    if let Commands::AgentTask(agent_task::AgentTaskArgs {
        command: agent_task::AgentTaskCommand::Cook(cook),
    }) = command
    {
        if !cook.gates.has_deterministic_gate() {
            let contract = command.lab_contract()?;
            let LabCommandPortability::LocalOnly(reason) = contract.portability else {
                unreachable!("an unverified cook must retain its local-only portability contract");
            };
            return Some(HotCommand::local_only(contract.hot_label, Some(reason)));
        }
        return Some(HotCommand {
            label: "agent-task cook/run-plan/retry --run",
            lab_offload_supported: true,
            lab_offload_unsupported_reason: None,
            allows_warm_runner_coordination: true,
            offload_only_when_hot: false,
        });
    }

    // The batch-cook fanout coordinator keeps durable batch state, worktree
    // ownership, promotion, gates, and finalization on the controller, but each
    // independent child provider attempt is Lab-eligible (routed by
    // `run_split_placement_fanout`). Treat `fanout run-plan` like a verified
    // cook: a controller-owned coordinator whose provider work can be admitted
    // under warm/hot CPU load when an explicit ready `--runner` is selected,
    // rather than refusing the whole batch as local-only (#9375). Memory and
    // process pressure remain hard local safety gates via
    // `admits_warm_runner_coordination`.
    if is_lab_offloadable_fanout_coordinator(command) {
        return Some(HotCommand {
            label: "agent-task fanout run-plan",
            lab_offload_supported: true,
            lab_offload_unsupported_reason: None,
            allows_warm_runner_coordination: true,
            offload_only_when_hot: false,
        });
    }

    if !command.portability_contract().is_resource_intensive() {
        return None;
    }

    let contract = command.lab_contract()?;

    match contract.portability {
        LabCommandPortability::Portable => {
            let mut hot = HotCommand::lab_supported(contract.hot_label);
            // Adopt the command's own routing threshold so resource admission
            // agrees with Lab routing: a `.cheap()` portable command stays local
            // (no warn/refuse) on a merely `warm` controller, engaging only at
            // `hot` (#9432).
            hot.offload_only_when_hot = contract.routing_policy.offload_only_when_hot;
            Some(hot)
        }
        LabCommandPortability::LocalOnly(reason) => {
            Some(HotCommand::local_only(contract.hot_label, Some(reason)))
        }
    }
}

pub(crate) fn is_cook_preview(command: &Commands) -> bool {
    matches!(
        command,
        Commands::AgentTask(agent_task::AgentTaskArgs {
            command: agent_task::AgentTaskCommand::Cook(cook),
        }) if cook.preview
    )
}

/// Project the resource-admission decision Cook execution would make without
/// routing, refreshing inventory, or reserving a runner. Branches where
/// execution owns a later mutating or runner-specific admission remain explicit
/// `indeterminate` snapshots rather than weaker preview-only refusals.
pub(crate) fn cook_preview_placement_admission(
    command: HotCommand,
    resources: &DoctorOutput,
    placement: crate::cli_surface::Placement,
    runner: Option<&str>,
    detach_after_handoff: bool,
    lab_readiness: Option<&LabRunnerReadiness>,
    replay_args: &[String],
) -> CookPreviewPlacementAdmission {
    // Execution intentionally lets Lab routing validate an explicit runner's
    // availability and capabilities. Preview must preserve that authority.
    if runner.is_some() {
        return CookPreviewPlacementAdmission {
            schema: "homeboy/cook-preview-placement-admission/v1",
            state: CookPreviewPlacementAdmissionState::Indeterminate,
            revalidate_before_execution: true,
            blockers: Vec::new(),
            deferred_to: Some("lab_route".to_string()),
            recovery: None,
        };
    }
    // Detached Cook always asks execution to recheck reverse-runner capacity and
    // submit through its broker queue. This is independent of controller load.
    if detach_after_handoff && !placement.is_explicit_local_override() {
        return CookPreviewPlacementAdmission {
            schema: "homeboy/cook-preview-placement-admission/v1",
            state: CookPreviewPlacementAdmissionState::Indeterminate,
            revalidate_before_execution: true,
            blockers: Vec::new(),
            deferred_to: Some("detached_queue_admission".to_string()),
            recovery: None,
        };
    }
    let Some(warning) = evaluate_with_runner_hint(command, resources, lab_readiness) else {
        return CookPreviewPlacementAdmission {
            schema: "homeboy/cook-preview-placement-admission/v1",
            state: CookPreviewPlacementAdmissionState::Admissible,
            revalidate_before_execution: true,
            blockers: Vec::new(),
            deferred_to: None,
            recovery: None,
        };
    };

    // A stale projection receives one runner-owned authoritative refresh before
    // execution refuses. Preview deliberately does not perform that refresh.
    if matches!(
        lab_readiness.map(|readiness| readiness.state),
        Some(crate::runner::runners::LabRunnerReadinessState::Stale)
    ) {
        return CookPreviewPlacementAdmission {
            schema: "homeboy/cook-preview-placement-admission/v1",
            state: CookPreviewPlacementAdmissionState::Indeterminate,
            revalidate_before_execution: true,
            blockers: Vec::new(),
            deferred_to: Some("lab_inventory_refresh".to_string()),
            recovery: None,
        };
    }
    let runner_admitted = admits_warm_runner_coordination(
        command,
        resources,
        lab_readiness.and_then(|readiness| readiness.selected_runner_id.as_deref()),
        lab_readiness,
    );
    let local_admitted = placement.is_explicit_local_override()
        || admits_auto_local_capacity_fallback(command, resources, lab_readiness, placement);
    if runner_admitted || local_admitted {
        return CookPreviewPlacementAdmission {
            schema: "homeboy/cook-preview-placement-admission/v1",
            state: CookPreviewPlacementAdmissionState::Admissible,
            revalidate_before_execution: true,
            blockers: Vec::new(),
            deferred_to: None,
            recovery: None,
        };
    }

    let mut blockers = vec![CookPreviewPlacementBlocker {
        id: "controller_resource_pressure".to_string(),
        detail: warning.message,
    }];
    if !placement.is_explicit_local_override() {
        if let Some(readiness) = lab_readiness {
            let (id, detail) = match readiness.state {
                crate::runner::runners::LabRunnerReadinessState::Stale => (
                    "lab_inventory_stale",
                    "Lab runner inventory is stale; refresh or reconcile the runner before execution.",
                ),
                crate::runner::runners::LabRunnerReadinessState::Absent => (
                    "no_lab_runner",
                    "No Lab runner is configured for automatic offload.",
                ),
                _ => (
                    "no_admissible_lab_runner",
                    "No ready Lab runner can admit this Cook now.",
                ),
            };
            blockers.push(CookPreviewPlacementBlocker {
                id: id.to_string(),
                detail: detail.to_string(),
            });
        } else {
            blockers.push(CookPreviewPlacementBlocker {
                id: "lab_inventory_unavailable".to_string(),
                detail: "Lab runner inventory could not be observed for this preview.".to_string(),
            });
        }
    }
    CookPreviewPlacementAdmission {
        schema: "homeboy/cook-preview-placement-admission/v1",
        state: CookPreviewPlacementAdmissionState::Blocked,
        revalidate_before_execution: true,
        blockers,
        deferred_to: None,
        recovery: admission_recovery(replay_args, lab_readiness),
    }
}

pub fn evaluate(command: HotCommand, resources: &DoctorOutput) -> Option<ResourcePolicyWarning> {
    evaluate_with_runner_hint(command, resources, None)
}

pub(crate) fn evaluate_with_runner_hint(
    command: HotCommand,
    resources: &DoctorOutput,
    lab_readiness: Option<&LabRunnerReadiness>,
) -> Option<ResourcePolicyWarning> {
    let recommendation = resources.recommendation;
    // Resource admission uses the same pressure threshold as Lab routing: a
    // `.cheap()` command engages only at `hot`, so a merely `warm` controller
    // neither warns nor refuses it (#9432). Everything else engages once the
    // machine leaves `ok`.
    if !command.engages_resource_admission(recommendation) {
        return None;
    }
    Some(ResourcePolicyWarning {
        command: command.label,
        recommendation,
        message: warning_message(command, recommendation, resources, lab_readiness),
    })
}

/// Describe controller-side pressure for a workload explicitly routed to a Lab
/// runner. This is placement evidence, not a local-execution warning: the
/// runner handoff owns reporting an authorized fallback if remote preparation
/// later fails.
pub(crate) fn explicit_runner_controller_notice(
    command: HotCommand,
    resources: &DoctorOutput,
    runner_id: &str,
) -> Option<String> {
    command
        .engages_resource_admission(resources.recommendation)
        .then(|| {
            runner_pinned_controller_notice(command, resources.recommendation, resources, runner_id)
        })
}

/// Cook keeps durable coordination, promotion, and gates on the controller,
/// but its provider attempt can run on a selected Lab runner. A ready explicit
/// or automatically selected runner lets the controller admit that lightweight
/// coordination under warm or hot CPU load, while memory and process pressure
/// remain local safety gates.
pub(crate) fn admits_warm_runner_coordination(
    command: HotCommand,
    resources: &DoctorOutput,
    selected_runner: Option<&str>,
    lab_readiness: Option<&LabRunnerReadiness>,
) -> bool {
    command.allows_warm_runner_coordination
        && matches!(
            resources.recommendation,
            ResourceRecommendation::Warm | ResourceRecommendation::Hot
        )
        && resources
            .memory
            .as_ref()
            .is_none_or(|memory| memory.recommendation == ResourceRecommendation::Ok)
        && resources.processes.recommendation == ResourceRecommendation::Ok
        && selected_runner.is_some_and(|runner_id| {
            lab_readiness.is_some_and(|readiness| {
                readiness.state == crate::runner::runners::LabRunnerReadinessState::ConnectedReady
                    && readiness
                        .available_runner_ids
                        .iter()
                        .any(|available| available == runner_id)
            })
        })
}

/// Permit automatic controller execution only when Lab is disconnected and the
/// local host has measured headroom. This is intentionally narrower than an
/// explicit `--placement local`: missing load observations and every non-load
/// pressure signal fail closed.
pub(crate) fn admits_auto_local_capacity_fallback(
    command: HotCommand,
    resources: &DoctorOutput,
    lab_readiness: Option<&LabRunnerReadiness>,
    placement: crate::cli_surface::Placement,
) -> bool {
    const WARM_LOAD_RATIO: f64 = 0.75;

    if !command.lab_offload_supported || placement != crate::cli_surface::Placement::Auto {
        return false;
    }
    let Some(readiness) = lab_readiness else {
        return false;
    };
    if readiness.state != crate::runner::runners::LabRunnerReadinessState::Disconnected {
        return false;
    }
    let (Some(one), Some(five)) = (resources.load.one, resources.load.five) else {
        return false;
    };
    let cpus = resources.load.cpu_count.max(1) as f64;

    one / cpus < WARM_LOAD_RATIO
        && five / cpus < WARM_LOAD_RATIO
        && resources
            .memory
            .as_ref()
            .is_none_or(|memory| memory.recommendation == ResourceRecommendation::Ok)
        && resources.processes.recommendation == ResourceRecommendation::Ok
        && resources.rig_leases.recommendation == ResourceRecommendation::Ok
}

pub(crate) fn non_interactive_preflight_error(
    warning: &ResourcePolicyWarning,
    local_override: bool,
    interactive: bool,
    recovery: Option<ResourceAdmissionRecovery>,
    runner_offload_admitted: bool,
) -> Option<crate::core::Error> {
    // GitHub Actions runners are ephemeral, single-purpose, and always
    // non-interactive: the warm-machine refusal would fail otherwise-good PR
    // checks with no human to rerun and no Lab runner to route to. Never refuse
    // inside CI (#7735).
    if local_override || interactive || is_runner_hosted_exec() || is_ci_execution() {
        return None;
    }
    if runner_offload_admitted {
        return None;
    }

    let mut error = crate::core::Error::validation_invalid_argument(
        "resource-policy",
        format!(
            "Refusing to start `{}` on a {} machine from a non-interactive shell. {} {}",
            warning.command,
            severity_str(warning.recommendation),
            warning.message,
            primary_action(warning, None),
        ),
        None,
        None,
    );
    if let Some(recovery) = recovery {
        if let Some(choice) = recovery
            .choices
            .iter()
            .find(|choice| choice.kind == ResourceAdmissionRecoveryKind::LocalOverride)
        {
            error.details["rerun_command"] = serde_json::Value::String(choice.command.clone());
        }
        error.details["recovery"] =
            serde_json::to_value(recovery).expect("resource admission recovery serializes");
    }
    error.details["run_created"] = serde_json::Value::Bool(false);
    Some(error)
}

pub(crate) fn admission_recovery(
    args: &[String],
    lab_readiness: Option<&LabRunnerReadiness>,
) -> Option<ResourceAdmissionRecovery> {
    let local_override = local_override_command(args)?;
    let mut choices = vec![
        ResourceAdmissionRecoveryChoice {
            kind: ResourceAdmissionRecoveryKind::Defer,
            command: "homeboy self doctor".to_string(),
            retry_after_seconds: Some(PRESSURE_RETRY_AFTER_SECONDS),
            requires_operator_authorization: None,
        },
        ResourceAdmissionRecoveryChoice {
            kind: ResourceAdmissionRecoveryKind::LocalOverride,
            command: local_override,
            retry_after_seconds: None,
            requires_operator_authorization: Some(true),
        },
    ];
    // An absent inventory is intentional configuration, not a broken runner.
    // A ready inventory also cannot justify repair. Keep recovery action kinds
    // aligned with the observed state so repair is only advertised for stale
    // configured runners.
    if let Some(readiness) = lab_readiness {
        let kind = match readiness.state {
            crate::runner::runners::LabRunnerReadinessState::Absent
            | crate::runner::runners::LabRunnerReadinessState::ConnectedReady => None,
            crate::runner::runners::LabRunnerReadinessState::Disconnected => {
                Some(ResourceAdmissionRecoveryKind::RunnerConnection)
            }
            crate::runner::runners::LabRunnerReadinessState::ConnectedIneligible
            | crate::runner::runners::LabRunnerReadinessState::CapacityBlocked => {
                Some(ResourceAdmissionRecoveryKind::RunnerAvailability)
            }
            crate::runner::runners::LabRunnerReadinessState::Stale => {
                Some(ResourceAdmissionRecoveryKind::RunnerRecovery)
            }
        };
        if let Some(kind) = kind {
            choices.extend(
                readiness
                    .remediation_commands
                    .iter()
                    .cloned()
                    .map(|command| ResourceAdmissionRecoveryChoice {
                        kind: kind.clone(),
                        command,
                        retry_after_seconds: None,
                        requires_operator_authorization: None,
                    }),
            );
        }
    }
    Some(ResourceAdmissionRecovery {
        schema: RESOURCE_ADMISSION_RECOVERY_SCHEMA,
        run_created: false,
        choices,
    })
}

fn local_override_command(args: &[String]) -> Option<String> {
    if args.is_empty() {
        return None;
    }
    let mut command = args.to_vec();
    if let Some(index) = command
        .iter()
        .position(|arg| arg == "--placement" || arg.starts_with("--placement="))
    {
        if command[index] == "--placement" {
            if let Some(value) = command.get_mut(index + 1) {
                *value = "local".to_string();
            } else {
                command.push("local".to_string());
            }
        } else {
            command[index] = "--placement=local".to_string();
        }
    } else {
        command.insert(1, "--placement".to_string());
        command.insert(2, "local".to_string());
    }
    Some(crate::core::engine::shell::quote_args(&command))
}

pub fn rerun_command(
    command: HotCommand,
    args: &[String],
    default_runner: Option<&str>,
) -> Option<String> {
    if args.is_empty() {
        return None;
    }

    let mut rerun = Vec::with_capacity(args.len() + 2);
    rerun.push(args[0].clone());
    if command.lab_offload_supported {
        if let Some(runner_id) = default_runner {
            if !args.iter().any(|arg| {
                arg == "--runner"
                    || arg.starts_with("--runner=")
                    || arg == "--placement"
                    || arg.starts_with("--placement=")
            }) {
                rerun.push("--runner".to_string());
                rerun.push(runner_id.to_string());
            }
        } else if args
            .iter()
            .any(|arg| arg == "--placement" || arg.starts_with("--placement="))
        {
            rerun.extend(args.iter().skip(1).cloned());
            return Some(crate::core::engine::shell::quote_args(&rerun));
        } else {
            // A disconnected or unconfigured Lab must not turn a portable
            // workload into a suggested local hot-machine retry.
            return None;
        }
    } else {
        append_local_placement(&mut rerun, args);
    }
    rerun.extend(args.iter().skip(1).cloned());

    Some(crate::core::engine::shell::quote_args(&rerun))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_surface::{Cli, Placement};
    use crate::commands::resources::{LoadSummary, MemorySummary, ProcessSummary, RigLeaseSummary};
    use crate::test_support::with_isolated_home;
    use clap::Parser;
    fn resources(recommendation: ResourceRecommendation) -> DoctorOutput {
        DoctorOutput {
            command: "self.resources",
            recommendation,
            load: LoadSummary {
                one: Some(9.0),
                five: Some(7.0),
                fifteen: Some(5.0),
                cpu_count: 4,
                recommendation,
            },
            memory: None,
            processes: ProcessSummary {
                relevant_count: 0,
                top_cpu: Vec::new(),
                top_rss: Vec::new(),
                recommendation: ResourceRecommendation::Ok,
            },
            rig_leases: RigLeaseSummary {
                active_count: 0,
                concurrency_limit: None,
                leases: Vec::new(),
                recommendation: ResourceRecommendation::Ok,
            },
            notes: Vec::new(),
        }
    }

    fn lab_supported_hot(label: &'static str) -> HotCommand {
        HotCommand {
            label,
            lab_offload_supported: true,
            lab_offload_unsupported_reason: None,
            allows_warm_runner_coordination: false,
            offload_only_when_hot: false,
        }
    }

    fn ready_lab() -> LabRunnerReadiness {
        LabRunnerReadiness {
            state: crate::runner::runners::LabRunnerReadinessState::ConnectedReady,
            selected_runner_id: Some("homeboy-lab".to_string()),
            available_runner_ids: vec!["homeboy-lab".to_string()],
            reasons: Vec::new(),
            remediation_commands: Vec::new(),
        }
    }

    fn disconnected_lab() -> LabRunnerReadiness {
        LabRunnerReadiness {
            state: crate::runner::runners::LabRunnerReadinessState::Disconnected,
            selected_runner_id: Some("homeboy-lab".to_string()),
            available_runner_ids: Vec::new(),
            reasons: vec!["runner is disconnected".to_string()],
            remediation_commands: vec!["homeboy runner reconnect homeboy-lab".to_string()],
        }
    }

    fn coordination_resources() -> DoctorOutput {
        let mut output = resources(ResourceRecommendation::Warm);
        output.rig_leases.active_count = 1;
        output.rig_leases.recommendation = ResourceRecommendation::Warm;
        output
    }

    fn local_only_hot(label: &'static str, reason: &'static str) -> HotCommand {
        HotCommand {
            label,
            lab_offload_supported: false,
            lab_offload_unsupported_reason: Some(reason),
            allows_warm_runner_coordination: false,
            offload_only_when_hot: false,
        }
    }

    fn cook_hot() -> HotCommand {
        HotCommand {
            label: "agent-task cook/run-plan/retry --run",
            lab_offload_supported: true,
            lab_offload_unsupported_reason: None,
            allows_warm_runner_coordination: true,
            offload_only_when_hot: false,
        }
    }

    fn preview_replay_args() -> Vec<String> {
        vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "inspect".to_string(),
        ]
    }

    #[test]
    fn cook_preview_admission_accepts_auto_placement_with_ready_lab_capacity() {
        let ready = ready_lab();
        let admission = cook_preview_placement_admission(
            cook_hot(),
            &resources(ResourceRecommendation::Hot),
            Placement::Auto,
            None,
            false,
            Some(&ready),
            &preview_replay_args(),
        );

        assert_eq!(
            admission.state,
            CookPreviewPlacementAdmissionState::Admissible
        );
        assert!(admission.blockers.is_empty());
        assert!(admission.recovery.is_none());
    }

    #[test]
    fn cook_preview_admission_blocks_hot_local_placement_without_override() {
        let admission = cook_preview_placement_admission(
            cook_hot(),
            &resources(ResourceRecommendation::Hot),
            Placement::Auto,
            None,
            false,
            None,
            &preview_replay_args(),
        );

        assert_eq!(admission.state, CookPreviewPlacementAdmissionState::Blocked);
        assert_eq!(admission.blockers[0].id, "controller_resource_pressure");
        assert_eq!(admission.blockers[1].id, "lab_inventory_unavailable");
        assert_eq!(
            admission.recovery.expect("recovery").choices[1].command,
            "homeboy --placement local agent-task cook --prompt inspect"
        );
    }

    #[test]
    fn cook_preview_admission_defers_stale_inventory_and_blocks_missing_lab() {
        for (state, expected_state, expected_blocker) in [
            (
                crate::runner::runners::LabRunnerReadinessState::Stale,
                CookPreviewPlacementAdmissionState::Indeterminate,
                None,
            ),
            (
                crate::runner::runners::LabRunnerReadinessState::Absent,
                CookPreviewPlacementAdmissionState::Blocked,
                Some("no_lab_runner"),
            ),
        ] {
            let readiness = LabRunnerReadiness {
                state,
                selected_runner_id: None,
                available_runner_ids: Vec::new(),
                reasons: Vec::new(),
                remediation_commands: vec!["homeboy runner reconcile lab".to_string()],
            };
            let admission = cook_preview_placement_admission(
                cook_hot(),
                &resources(ResourceRecommendation::Hot),
                Placement::Auto,
                None,
                false,
                Some(&readiness),
                &preview_replay_args(),
            );

            assert_eq!(admission.state, expected_state, "{state:?}");
            assert_eq!(
                admission.blockers.get(1).map(|blocker| blocker.id.as_str()),
                expected_blocker,
                "{state:?}"
            );
            assert_eq!(
                admission.deferred_to.as_deref(),
                (state == crate::runner::runners::LabRunnerReadinessState::Stale)
                    .then_some("lab_inventory_refresh")
            );
        }
    }

    #[test]
    fn cook_preview_admission_honors_explicit_local_override() {
        let admission = cook_preview_placement_admission(
            cook_hot(),
            &resources(ResourceRecommendation::Hot),
            Placement::Local,
            None,
            false,
            None,
            &preview_replay_args(),
        );

        assert_eq!(
            admission.state,
            CookPreviewPlacementAdmissionState::Admissible
        );
        assert!(admission.blockers.is_empty());
    }

    #[test]
    fn cook_preview_admission_defers_explicit_runner_to_lab_route() {
        let ready = ready_lab();
        let admission = cook_preview_placement_admission(
            cook_hot(),
            &resources(ResourceRecommendation::Hot),
            Placement::Auto,
            Some("other-lab"),
            false,
            Some(&ready),
            &preview_replay_args(),
        );

        assert_eq!(
            admission.state,
            CookPreviewPlacementAdmissionState::Indeterminate
        );
        assert_eq!(admission.deferred_to.as_deref(), Some("lab_route"));
        assert!(admission.blockers.is_empty());
    }

    #[test]
    fn cook_preview_admission_defers_detached_queue_admission() {
        for recommendation in [ResourceRecommendation::Ok, ResourceRecommendation::Hot] {
            let admission = cook_preview_placement_admission(
                cook_hot(),
                &resources(recommendation),
                Placement::Auto,
                None,
                true,
                Some(&LabRunnerReadiness {
                    state: crate::runner::runners::LabRunnerReadinessState::CapacityBlocked,
                    selected_runner_id: None,
                    available_runner_ids: Vec::new(),
                    reasons: Vec::new(),
                    remediation_commands: Vec::new(),
                }),
                &preview_replay_args(),
            );

            assert_eq!(
                admission.state,
                CookPreviewPlacementAdmissionState::Indeterminate,
                "{recommendation:?}"
            );
            assert_eq!(
                admission.deferred_to.as_deref(),
                Some("detached_queue_admission"),
                "{recommendation:?}"
            );
        }
    }

    #[test]
    fn warns_when_hot_command_runs_on_warm_or_hot_machine() {
        let warning = evaluate(
            lab_supported_hot("bench"),
            &resources(ResourceRecommendation::Hot),
        )
        .expect("hot machines warn");
        assert_eq!(warning.command, "bench");
        assert_eq!(warning.recommendation, ResourceRecommendation::Hot);
        assert!(warning.message.contains("--placement local"));
        assert!(warning
            .message
            .contains("No Homeboy Lab runner is configured on this host"));
        assert!(warning.message.contains("Load average is 9.0"));

        assert!(evaluate(
            lab_supported_hot("bench"),
            &resources(ResourceRecommendation::Warm)
        )
        .is_some());
    }

    fn cheap_hot(label: &'static str) -> HotCommand {
        HotCommand {
            offload_only_when_hot: true,
            ..lab_supported_hot(label)
        }
    }

    #[test]
    fn cheap_command_is_not_warned_or_refused_on_a_warm_controller() {
        // A `.cheap()` portable command (e.g. `agent-task promote --dry-run`)
        // shares Lab routing's pressure threshold: it stays local without warning
        // or refusal on a merely `warm` controller (#9432).
        assert!(evaluate(
            cheap_hot("agent-task promote"),
            &resources(ResourceRecommendation::Warm),
        )
        .is_none());
    }

    #[test]
    fn cheap_command_remains_resource_managed_on_a_hot_controller() {
        // Cheap only defers the threshold to `hot`; genuine hot pressure still
        // warns/refuses (#9432).
        assert!(evaluate(
            cheap_hot("agent-task promote"),
            &resources(ResourceRecommendation::Hot),
        )
        .is_some());
    }

    #[test]
    fn portable_promotion_dry_run_carries_the_cheap_pressure_threshold() {
        // The portable promotion contract calls `.cheap()`; `hot_command` must
        // propagate that threshold so resource admission agrees with Lab routing.
        with_isolated_home(|_| {
            let cli = Cli::parse_from([
                "homeboy",
                "agent-task",
                "promote",
                "candidate-source",
                "--to-worktree",
                "homeboy@promote-9432",
                "--dry-run",
            ]);
            let command = hot_command(&cli.command).expect("promotion is resource managed");
            assert!(command.offload_only_when_hot, "portable promotion is cheap");
            assert!(evaluate(command, &resources(ResourceRecommendation::Warm)).is_none());
            assert!(evaluate(command, &resources(ResourceRecommendation::Hot)).is_some());
        });
    }

    #[test]
    fn warning_names_default_lab_runner_when_available() {
        let warning = evaluate_with_runner_hint(
            lab_supported_hot("agent-task providers"),
            &resources(ResourceRecommendation::Hot),
            Some(&ready_lab()),
        )
        .expect("hot machines warn");

        assert!(warning.message.contains("Lab runner `homeboy-lab`"));
        assert!(warning.message.contains("--runner homeboy-lab"));
        assert!(!warning.message.contains("--runner <id>"));
    }

    #[test]
    fn explicit_runner_notice_separates_controller_overhead_from_runner_workload() {
        let notice = explicit_runner_controller_notice(
            lab_supported_hot("worktree cleanup"),
            &resources(ResourceRecommendation::Warm),
            "homeboy-lab",
        )
        .expect("warm controller reports its own overhead");

        assert!(notice.contains("controller is warm"));
        assert!(
            notice.contains("Workload `worktree cleanup` is routed to Lab runner `homeboy-lab`")
        );
        assert!(notice.contains("Controller preflight and transport overhead remain local"));
        assert!(!notice.contains("starting `worktree cleanup` locally"));
        assert!(!notice.contains("--runner"));
    }

    #[test]
    fn connected_ineligible_runner_is_not_reported_as_absent() {
        let readiness = LabRunnerReadiness {
            state: crate::runner::runners::LabRunnerReadinessState::ConnectedIneligible,
            selected_runner_id: None,
            available_runner_ids: Vec::new(),
            reasons: vec!["active_jobs_unavailable".to_string()],
            remediation_commands: vec!["homeboy runner status homeboy-lab".to_string()],
        };
        let warning = evaluate_with_runner_hint(
            lab_supported_hot("agent-task providers"),
            &resources(ResourceRecommendation::Hot),
            Some(&readiness),
        )
        .expect("hot machines warn");

        assert!(warning.message.contains("connected_ineligible"));
        assert!(warning
            .message
            .contains("homeboy runner status homeboy-lab"));
        assert!(!warning
            .message
            .contains("No Homeboy Lab runner is configured"));
    }

    #[test]
    fn warning_for_local_only_hot_command_explains_runner_unavailability() {
        let warning = evaluate(
            local_only_hot("rig up", "`rig up` stays local for test reasons."),
            &resources(ResourceRecommendation::Hot),
        )
        .expect("hot machines warn");

        assert!(warning.message.contains("`rig up` stays local"));
        assert!(warning.message.contains("--placement local"));
        assert!(!warning.message.contains("--runner <id>"));
    }

    #[test]
    fn changed_and_file_scoped_lint_commands_are_hot() {
        let changed_lint = Cli::parse_from([
            "homeboy",
            "review",
            "lint",
            "--changed-since",
            "origin/main",
        ]);
        let hot = hot_command(&changed_lint.command).expect("changed-scope lint is hot");
        assert_eq!(hot.label, "review lint");
        assert!(hot.lab_offload_supported);
        assert!(hot.lab_offload_unsupported_reason.is_none());

        let file_lint = Cli::parse_from(["homeboy", "review", "lint", "--file", "src/main.rs"]);
        let hot = hot_command(&file_lint.command).expect("file-scope lint is hot");
        assert_eq!(hot.label, "review lint");
        assert!(hot.lab_offload_supported);
        assert!(hot.lab_offload_unsupported_reason.is_none());
    }

    #[test]
    fn rig_source_management_keeps_lab_diagnostics_without_resource_admission() {
        for args in [
            ["homeboy", "rig", "install", "./rig-package"].as_slice(),
            ["homeboy", "rig", "update", "demo-rig"].as_slice(),
            ["homeboy", "rig", "sync", "demo-rig"].as_slice(),
            ["homeboy", "rig", "sources"].as_slice(),
        ] {
            let cli = Cli::parse_from(args);
            let portability = cli.command.portability_contract();
            let contract = portability
                .lab_command()
                .expect("rig source management keeps its Lab diagnostic contract");

            assert!(!portability.is_resource_intensive());
            assert_eq!(
                contract.portability,
                LabCommandPortability::LocalOnly(
                    crate::command_contract::RIG_SOURCE_MANAGEMENT_LAB_UNSUPPORTED_REASON
                )
            );
            assert!(hot_command(&cli.command).is_none());
        }
    }

    #[test]
    fn rig_workloads_remain_resource_managed() {
        for args in [
            ["homeboy", "rig", "up", "demo-rig"].as_slice(),
            ["homeboy", "rig", "check", "demo-rig"].as_slice(),
            ["homeboy", "rig", "run", "demo-rig", "--profile", "smoke"].as_slice(),
        ] {
            let cli = Cli::parse_from(args);

            assert!(cli.command.portability_contract().is_resource_intensive());
            assert!(hot_command(&cli.command).is_some());
        }
    }

    #[test]
    fn bounded_agent_task_metadata_reads_bypass_hot_admission_for_all_runner_states() {
        // These commands read bounded controller metadata. In particular, they
        // must stay available when the selected runner is disconnected, stale,
        // or has conflicting readiness observations, so preflight must not ask
        // Lab for guidance before they run.
        let runner_states = [
            LabRunnerReadiness {
                state: crate::runner::runners::LabRunnerReadinessState::Disconnected,
                selected_runner_id: Some("homeboy-lab".to_string()),
                available_runner_ids: Vec::new(),
                reasons: vec!["runner is disconnected".to_string()],
                remediation_commands: vec!["homeboy runner reconnect homeboy-lab".to_string()],
            },
            LabRunnerReadiness {
                state: crate::runner::runners::LabRunnerReadinessState::Stale,
                selected_runner_id: Some("homeboy-lab".to_string()),
                available_runner_ids: Vec::new(),
                reasons: vec!["runner daemon is stale".to_string()],
                remediation_commands: vec!["homeboy runner refresh homeboy-lab".to_string()],
            },
            LabRunnerReadiness {
                state: crate::runner::runners::LabRunnerReadinessState::ConnectedIneligible,
                selected_runner_id: None,
                available_runner_ids: vec!["homeboy-lab".to_string()],
                reasons: vec!["conflicting runner readiness observations".to_string()],
                remediation_commands: vec!["homeboy runner status homeboy-lab".to_string()],
            },
        ];
        for args in [
            ["homeboy", "agent-task", "status", "agent-task-123"].as_slice(),
            ["homeboy", "agent-task", "logs", "agent-task-123"].as_slice(),
            ["homeboy", "agent-task", "artifacts", "agent-task-123"].as_slice(),
            ["homeboy", "agent-task", "evidence", "agent-task-123"].as_slice(),
            ["homeboy", "agent-task", "diagnose", "agent-task-123"].as_slice(),
            ["homeboy", "agent-task", "review", "agent-task-123"].as_slice(),
            ["homeboy", "agent-task", "reconcile", "agent-task-123"].as_slice(),
            ["homeboy", "agent-task", "list"].as_slice(),
            ["homeboy", "agent-task", "active"].as_slice(),
            ["homeboy", "agent-task", "latest"].as_slice(),
            [
                "homeboy",
                "agent-task",
                "gate-feedback",
                "--promotion",
                "{}",
                "--source-task",
                "{}",
            ]
            .as_slice(),
            ["homeboy", "agent-task", "fanout", "status", "batch-123"].as_slice(),
            ["homeboy", "agent-task", "fanout", "artifacts", "batch-123"].as_slice(),
            ["homeboy", "agent-task", "loop", "status", "loop-123"].as_slice(),
            ["homeboy", "agent-task", "controller", "status", "loop-123"].as_slice(),
            [
                "homeboy",
                "agent-task",
                "controller",
                "diagnose",
                "loop-123",
            ]
            .as_slice(),
            ["homeboy", "agent-task", "controller", "list"].as_slice(),
        ] {
            let cli = Cli::parse_from(args);
            for readiness in &runner_states {
                let warning = hot_command(&cli.command).and_then(|command| {
                    evaluate_with_runner_hint(
                        command,
                        &resources(ResourceRecommendation::Hot),
                        Some(readiness),
                    )
                });
                assert!(
                    warning.is_none(),
                    "{args:?} must not emit hot-machine or Lab guidance while runner is {}",
                    readiness.state.as_str(),
                );
            }
        }
    }

    #[test]
    fn unscoped_provider_discovery_never_interacts_with_a_runner() {
        // Provider discovery reads controller-local manifests. Treating it as
        // an admitted workload made a warm controller capture a resource
        // context, probe Lab readiness, and relocate an unscoped diagnostic
        // read onto a runner whose provider readiness is a different answer
        // (#9763). No pressure level may engage resource admission for it.
        let cli = Cli::parse_from(["homeboy", "agent-task", "providers"]);

        assert!(
            hot_command(&cli.command).is_none(),
            "unscoped provider discovery must not enter resource admission"
        );
        for recommendation in [
            ResourceRecommendation::Ok,
            ResourceRecommendation::Warm,
            ResourceRecommendation::Hot,
        ] {
            let warning = hot_command(&cli.command)
                .and_then(|command| evaluate(command, &resources(recommendation)));
            assert!(
                warning.is_none(),
                "a {recommendation:?} controller must not steer provider discovery to Lab"
            );
        }
    }

    #[test]
    fn explicit_runner_provider_discovery_keeps_its_lab_contract() {
        // Staying out of resource admission must not remove the explicit
        // runner-scoped probe: `--runner <id>` is still the way to ask what a
        // runner's catalog looks like (#9763).
        let cli = Cli::parse_from([
            "homeboy",
            "--runner",
            "homeboy-lab",
            "agent-task",
            "providers",
        ]);

        assert!(cli
            .command
            .portability_contract()
            .lab_command()
            .is_some_and(|contract| matches!(
                contract.portability,
                LabCommandPortability::Portable
            )));
    }

    #[test]
    fn agent_task_provider_replay_remains_resource_managed() {
        // `review` is a bounded metadata read. Provider replay executes work
        // against the selected runner and retains resource admission.
        let args = [
            "homeboy",
            "agent-task",
            "replay-provider-boundary",
            "agent-task-123",
        ];
        let cli = Cli::parse_from(args);
        assert!(
            hot_command(&cli.command).is_some(),
            "{args:?} must retain resource admission"
        );
    }

    #[test]
    fn agent_task_cook_batch_dry_run_does_not_start_hot_workloads() {
        let cli = Cli::parse_from([
            "homeboy",
            "agent-task",
            "fanout",
            "cook-batch",
            "--repo",
            "homeboy",
            "--verify",
            "cargo build -j 3",
            "--dry-run",
            "https://github.com/Extra-Chill/homeboy/issues/7796",
        ]);

        assert!(hot_command(&cli.command).is_none());
    }

    #[test]
    fn controller_owned_cook_batch_coordination_skips_hot_resource_refusal() {
        let cli = Cli::parse_from([
            "homeboy",
            "agent-task",
            "fanout",
            "cook-batch",
            "--repo",
            "homeboy",
            "--verify",
            "cargo test --lib",
            "--run-plan",
            "https://github.com/Extra-Chill/homeboy/issues/8519",
        ]);

        assert!(hot_command(&cli.command).is_none());
    }

    #[test]
    fn rig_source_management_commands_bypass_hot_resource_admission() {
        // Rig registry/source-state management is lightweight controller-local
        // bookkeeping, not a resource-intensive workload, so it must never be
        // refused by warm/hot resource policy (#9428).
        for args in [
            [
                "homeboy",
                "rig",
                "install",
                "https://example.invalid/rig.git",
            ]
            .as_slice(),
            ["homeboy", "rig", "update"].as_slice(),
            ["homeboy", "rig", "update", "--all"].as_slice(),
            ["homeboy", "rig", "sync", "example-rig"].as_slice(),
            ["homeboy", "rig", "sources"].as_slice(),
        ] {
            let cli = Cli::parse_from(args);
            assert!(
                hot_command(&cli.command).is_none(),
                "rig source management must not be resource-managed: {args:?}"
            );
            // The explanatory local-only Lab portability contract is preserved so
            // an explicit unsupported Lab placement still gets a clear diagnostic.
            let contract = cli
                .command
                .lab_contract()
                .expect("rig source management keeps a Lab portability contract");
            assert!(matches!(
                contract.portability,
                crate::command_contract::LabCommandPortability::LocalOnly(_)
            ));
        }
    }

    #[test]
    fn rig_up_and_check_remain_resource_managed() {
        // Genuinely resource-intensive rig commands keep their resource-policy
        // classification (#9428).
        let up = Cli::parse_from(["homeboy", "rig", "up", "example-rig"]);
        assert!(
            hot_command(&up.command).is_some(),
            "rig up remains resource-managed"
        );

        let check = Cli::parse_from(["homeboy", "rig", "check", "example-rig"]);
        assert!(
            hot_command(&check.command).is_some(),
            "rig check remains resource-managed"
        );
    }

    #[test]
    fn default_cook_batch_coordinator_is_controller_owned_and_not_offloadable() {
        // #8025: the default cook-batch invocation (neither --dry-run nor
        // --run-plan) compiles the plan on the controller. It owns worktree
        // creation and the durable batch record, so the coordinator command
        // itself must never be treated as a portable, offloadable hot command.
        // Previously only --run-plan was guarded, so this default variant fell
        // through and could be dispatched to Lab as a single job, timing out
        // before creating its local batch record.
        let cli = Cli::parse_from([
            "homeboy",
            "agent-task",
            "fanout",
            "cook-batch",
            "--repo",
            "homeboy",
            "--verify",
            "cargo test --lib",
            "https://github.com/Extra-Chill/homeboy/issues/8025",
        ]);

        assert!(
            hot_command(&cli.command).is_none(),
            "default cook-batch coordinator must be controller-owned, not offloadable"
        );
    }

    #[test]
    fn verified_agent_task_cook_can_offload_its_provider_attempt() {
        let cli = Cli::parse_from([
            "homeboy",
            "agent-task",
            "cook",
            "--prompt",
            "implement the fix",
            "--to-worktree",
            "homeboy@cook-routing",
            "--verify",
            "cargo test --locked",
        ]);
        let command = hot_command(&cli.command).expect("verified cook is hot");
        assert!(command.lab_offload_supported);
        assert!(command.allows_warm_runner_coordination);
        assert_eq!(command.label, "agent-task cook/run-plan/retry --run");
    }

    #[test]
    fn fanout_run_plan_coordinator_can_offload_its_child_provider_attempts() {
        // A validated batch-cook fanout run-plan must not be refused as
        // local-only under resource policy: the coordinator stays on the
        // controller while each child provider attempt is Lab-eligible (#9375).
        let cli = Cli::parse_from([
            "homeboy",
            "agent-task",
            "fanout",
            "run-plan",
            "--input",
            "@batch-cook-plan.json",
        ]);
        let command = hot_command(&cli.command).expect("fanout run-plan is resource managed");
        assert!(command.lab_offload_supported);
        assert!(command.allows_warm_runner_coordination);
        assert!(command.lab_offload_unsupported_reason.is_none());
        assert_eq!(command.label, "agent-task fanout run-plan");
    }

    #[test]
    fn runner_pinned_fanout_run_plan_admits_an_explicit_ready_runner_on_warm_or_hot_controller() {
        // With an explicit ready runner, the fanout coordinator is admitted on a
        // warm/hot controller so its child cooks can execute on Lab, instead of
        // the whole batch being forced local (#9375).
        let cli = Cli::parse_from([
            "homeboy",
            "--runner",
            "homeboy-lab",
            "agent-task",
            "fanout",
            "run-plan",
            "--input",
            "@batch-cook-plan.json",
        ]);
        let command = hot_command(&cli.command).expect("fanout run-plan is resource managed");
        assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));
        let ready = ready_lab();

        for recommendation in [ResourceRecommendation::Warm, ResourceRecommendation::Hot] {
            let mut resources = coordination_resources();
            resources.recommendation = recommendation;
            resources.load.recommendation = recommendation;
            assert!(admits_warm_runner_coordination(
                command,
                &resources,
                Some("homeboy-lab"),
                Some(&ready),
            ));
        }

        // Without an explicit runner, or with an unavailable one, the warm/hot
        // controller still declines — preserving the memory/process safety
        // boundary shared with the single-cook coordinator.
        let resources = coordination_resources();
        assert!(!admits_warm_runner_coordination(
            command,
            &resources,
            None,
            Some(&ready),
        ));
        assert!(!admits_warm_runner_coordination(
            command,
            &resources,
            Some("missing-lab"),
            Some(&ready),
        ));
    }

    #[test]
    fn automatic_cook_coordination_admits_a_ready_runner_on_warm_or_hot_controller() {
        let cli = Cli::parse_from([
            "homeboy",
            "agent-task",
            "cook",
            "--cwd",
            "/workspace/homeboy@fix-12503-auto-ready-lab",
            "--prompt",
            "implement the fix",
            "--to-worktree",
            "homeboy@cook-routing",
            "--verify",
            "cargo test --locked",
        ]);
        let command = hot_command(&cli.command).expect("cook is resource managed");
        assert_eq!(cli.runner, None);
        let ready = ready_lab();

        for recommendation in [ResourceRecommendation::Warm, ResourceRecommendation::Hot] {
            let mut resources = coordination_resources();
            resources.recommendation = recommendation;
            resources.load.recommendation = recommendation;
            assert!(admits_warm_runner_coordination(
                command,
                &resources,
                Some("homeboy-lab"),
                Some(&ready),
            ));
        }

        let resources = coordination_resources();
        assert!(!admits_warm_runner_coordination(
            command,
            &resources,
            None,
            Some(&ready),
        ));
        assert!(!admits_warm_runner_coordination(
            command,
            &resources,
            Some("missing-lab"),
            Some(&ready),
        ));
    }

    #[test]
    fn automatic_cook_refusal_names_when_no_runner_is_eligible() {
        let _lock = env_lock();
        let _ci = EnvVarGuard::remove("GITHUB_ACTIONS");
        let command = lab_supported_hot("agent-task cook/run-plan/retry --run");
        let unavailable = LabRunnerReadiness {
            state: crate::runner::runners::LabRunnerReadinessState::ConnectedIneligible,
            selected_runner_id: None,
            available_runner_ids: Vec::new(),
            reasons: vec!["all connected runners are at capacity".to_string()],
            remediation_commands: vec!["homeboy runner status homeboy-lab".to_string()],
        };
        let resources = coordination_resources();

        assert!(!admits_warm_runner_coordination(
            command,
            &resources,
            unavailable.selected_runner_id.as_deref(),
            Some(&unavailable),
        ));
        let warning = evaluate_with_runner_hint(command, &resources, Some(&unavailable))
            .expect("hot controller warns");
        let error = non_interactive_preflight_error(&warning, false, false, None, false)
            .expect("no eligible runner refuses local execution");
        assert!(error.message.contains("connected_ineligible"));
        assert!(error.message.contains("homeboy runner status homeboy-lab"));
        assert_eq!(error.details["run_created"], false);
    }

    #[test]
    fn runner_pinned_cook_coordination_rejects_memory_or_process_pressure() {
        let command = HotCommand {
            label: "agent-task cook/run-plan/retry --run",
            lab_offload_supported: true,
            lab_offload_unsupported_reason: None,
            allows_warm_runner_coordination: true,
            offload_only_when_hot: false,
        };
        let ready = ready_lab();
        let mut resources = coordination_resources();
        resources.memory = Some(MemorySummary {
            total_mb: 32_000,
            available_mb: 1_500,
            used_percent: 95.3,
            recommendation: ResourceRecommendation::Warm,
        });
        assert!(!admits_warm_runner_coordination(
            command,
            &resources,
            Some("homeboy-lab"),
            Some(&ready),
        ));

        let mut resources = coordination_resources();
        resources.processes.recommendation = ResourceRecommendation::Hot;
        assert!(!admits_warm_runner_coordination(
            command,
            &resources,
            Some("homeboy-lab"),
            Some(&ready),
        ));
    }

    #[test]
    fn unverified_cook_remains_local_only_with_its_concrete_gate_requirement() {
        let cli = Cli::parse_from([
            "homeboy",
            "agent-task",
            "cook",
            "--prompt",
            "implement the fix",
            "--to-worktree",
            "homeboy@cook-routing",
        ]);

        let command = hot_command(&cli.command).expect("unverified cook is resource managed");
        assert!(!command.lab_offload_supported);
        assert_eq!(
            command.lab_offload_unsupported_reason,
            Some("agent-task cook requires at least one deterministic --verify or --private-verify gate")
        );
        assert!(!admits_warm_runner_coordination(
            command,
            &coordination_resources(),
            Some("homeboy-lab"),
            Some(&ready_lab()),
        ));
    }

    #[test]
    fn queue_only_cook_skips_resource_preflight_for_controller_validation() {
        let cli = Cli::parse_from([
            "homeboy",
            "agent-task",
            "cook",
            "--prompt",
            "implement the fix",
            "--to-worktree",
            "example@worktree",
            "--verify",
            "true",
            "--queue-only",
        ]);

        assert!(hot_command(&cli.command).is_none());
    }

    #[test]
    fn does_not_warn_when_machine_is_ok() {
        assert!(evaluate(
            lab_supported_hot("bench"),
            &resources(ResourceRecommendation::Ok)
        )
        .is_none());
    }

    #[test]
    fn non_interactive_hot_warning_fails_before_starting_command() {
        let _lock = env_lock();
        let _guard = EnvVarGuard::remove(crate::runner::RUNNER_HOSTED_EXEC_ENV);
        let _ci = EnvVarGuard::remove("GITHUB_ACTIONS");
        let warning = evaluate(
            lab_supported_hot("audit"),
            &resources(ResourceRecommendation::Hot),
        )
        .expect("hot machines warn");

        let error = non_interactive_preflight_error(&warning, false, false, None, false)
            .expect("non-interactive hot runs should fail fast");

        assert_eq!(error.code.as_str(), "validation.invalid_argument");
        assert!(error.message.contains("Refusing to start `audit`"));
        assert!(error.message.contains("non-interactive shell"));
        assert!(error
            .message
            .contains("No Homeboy Lab runner is configured on this host"));
        assert!(error.details.get("rerun_command").is_none());
        assert_eq!(error.details["run_created"], false);
    }

    #[test]
    fn non_interactive_hot_warning_allows_default_lab_runner_auto_offload() {
        let _lock = env_lock();
        let _guard = EnvVarGuard::remove(crate::runner::RUNNER_HOSTED_EXEC_ENV);
        let warning = evaluate_with_runner_hint(
            lab_supported_hot("agent-task providers"),
            &resources(ResourceRecommendation::Hot),
            Some(&ready_lab()),
        )
        .expect("hot machines warn");

        assert!(non_interactive_preflight_error(&warning, false, false, None, true).is_none());
    }

    #[test]
    fn non_interactive_hot_warning_allows_explicit_lab_runner_without_default() {
        let _lock = env_lock();
        let _guard = EnvVarGuard::remove(crate::runner::RUNNER_HOSTED_EXEC_ENV);
        let warning = evaluate_with_runner_hint(
            lab_supported_hot("agent-task cook/run-plan/retry --run"),
            &resources(ResourceRecommendation::Hot),
            Some(&ready_lab()),
        )
        .expect("hot machines warn");

        assert!(non_interactive_preflight_error(&warning, false, false, None, true).is_none());
    }

    #[test]
    fn disconnected_lab_allows_auto_placement_on_healthy_multicore_local_capacity() {
        let mut local = resources(ResourceRecommendation::Hot);
        local.load.one = Some(7.7);
        local.load.five = Some(7.5);
        local.load.cpu_count = 18;
        let disconnected = disconnected_lab();

        assert!(admits_auto_local_capacity_fallback(
            lab_supported_hot("agent-task cook/run-plan/retry --run"),
            &local,
            Some(&disconnected),
            crate::cli_surface::Placement::Auto,
        ));
        let context = resource_policy_context_from_evaluation(
            lab_supported_hot("agent-task cook/run-plan/retry --run"),
            &local,
            None,
            false,
            true,
            Some(&disconnected),
            false,
        );
        assert_eq!(context.runner_selection.reason, "local_capacity_fallback");
        assert_eq!(context.runner_selection.readiness_state, "disconnected");
        assert_eq!(context.host.load_one, Some(7.7));
        assert_eq!(context.host.cpu_count, 18);
    }

    #[test]
    fn disconnected_lab_refuses_auto_placement_when_local_capacity_is_saturated() {
        let mut local = resources(ResourceRecommendation::Hot);
        local.load.one = Some(27.0);
        local.load.five = Some(18.0);
        local.load.cpu_count = 18;

        assert!(!admits_auto_local_capacity_fallback(
            lab_supported_hot("agent-task cook/run-plan/retry --run"),
            &local,
            Some(&disconnected_lab()),
            crate::cli_surface::Placement::Auto,
        ));
    }

    #[test]
    fn auto_capacity_fallback_preserves_lab_only_and_lab_capacity_safety() {
        let mut local = resources(ResourceRecommendation::Hot);
        local.load.one = Some(7.7);
        local.load.five = Some(7.5);
        local.load.cpu_count = 18;
        let command = lab_supported_hot("agent-task cook/run-plan/retry --run");
        let capacity_blocked = LabRunnerReadiness {
            state: crate::runner::runners::LabRunnerReadinessState::CapacityBlocked,
            selected_runner_id: Some("homeboy-lab".to_string()),
            available_runner_ids: Vec::new(),
            reasons: vec!["capacity_reached".to_string()],
            remediation_commands: vec!["homeboy runner status homeboy-lab".to_string()],
        };

        assert!(!admits_auto_local_capacity_fallback(
            command,
            &local,
            Some(&disconnected_lab()),
            crate::cli_surface::Placement::Lab,
        ));
        assert!(!admits_auto_local_capacity_fallback(
            command,
            &local,
            Some(&capacity_blocked),
            crate::cli_surface::Placement::Auto,
        ));
    }

    #[test]
    fn non_interactive_local_only_refusal_includes_local_hot_rerun_command() {
        let _lock = env_lock();
        let _guard = EnvVarGuard::remove(crate::runner::RUNNER_HOSTED_EXEC_ENV);
        let _ci = EnvVarGuard::remove("GITHUB_ACTIONS");
        let command = local_only_hot(
            "lint",
            "Changed-scope lint runs stay local because changed-file scopes are not represented in the current Lab portability contract yet.",
        );
        let warning = evaluate(command, &resources(ResourceRecommendation::Warm))
            .expect("warm machines warn");
        let recovery = admission_recovery(
            &[
                "homeboy".to_string(),
                "review".to_string(),
                "lint".to_string(),
                "--changed-since".to_string(),
                "origin/main".to_string(),
            ],
            None,
        );

        let error = non_interactive_preflight_error(&warning, false, false, recovery, false)
            .expect("non-interactive local-only hot runs should fail fast");

        assert_eq!(
            error.details["rerun_command"].as_str(),
            Some("homeboy --placement local review lint --changed-since origin/main")
        );
        assert_eq!(error.details["run_created"], false);
        assert!(error.details.get("resume_command").is_none());
        assert!(error.message.contains("Lab routing is not offered"));
    }

    #[test]
    fn warm_machine_accepts_emitted_local_finalize_recovery_without_argument_surgery() {
        let _lock = env_lock();
        let _guard = EnvVarGuard::remove(crate::runner::RUNNER_HOSTED_EXEC_ENV);
        let _ci = EnvVarGuard::remove("GITHUB_ACTIONS");
        let command = homeboy_agents::agent_task_service::cook_recovery_command_with_prefix(
            "homeboy --placement local",
            &["finalize-pr", "--recover", "cook-local-finalize"],
        );
        let argv = shlex::split(&command).expect("emitted local recovery command is shell-safe");
        let cli = Cli::try_parse_from(argv).expect("emitted local recovery command parses");
        let warning = evaluate(
            lab_supported_hot("agent-task finalize-pr"),
            &resources(ResourceRecommendation::Warm),
        )
        .expect("warm machines require placement admission");

        assert_eq!(cli.placement, crate::cli_surface::Placement::Local);
        assert!(non_interactive_preflight_error(
            &warning,
            cli.placement == crate::cli_surface::Placement::Local,
            false,
            None,
            false,
        )
        .is_none());
    }

    #[test]
    fn portable_refusal_rerun_uses_eligible_lab_runner() {
        let rerun = rerun_command(
            lab_supported_hot("audit"),
            &[
                "homeboy".to_string(),
                "audit".to_string(),
                "--changed-since".to_string(),
                "origin/main".to_string(),
            ],
            Some("homeboy-lab"),
        );

        assert_eq!(
            rerun.as_deref(),
            Some("homeboy --runner homeboy-lab audit --changed-since origin/main")
        );
    }

    #[test]
    fn portable_rerun_preserves_explicit_runner_without_placement() {
        let rerun = rerun_command(
            lab_supported_hot("review test"),
            &[
                "homeboy".to_string(),
                "--runner".to_string(),
                "homeboy-lab".to_string(),
                "review".to_string(),
                "test".to_string(),
                "homeboy".to_string(),
            ],
            Some("homeboy-lab"),
        );

        assert_eq!(
            rerun.as_deref(),
            Some("homeboy --runner homeboy-lab review test homeboy")
        );
    }

    #[test]
    fn portable_rerun_preserves_explicit_placement_without_runner() {
        let rerun = rerun_command(
            lab_supported_hot("review test"),
            &[
                "homeboy".to_string(),
                "--placement".to_string(),
                "local".to_string(),
                "review".to_string(),
                "test".to_string(),
                "homeboy".to_string(),
            ],
            Some("homeboy-lab"),
        );

        assert_eq!(
            rerun.as_deref(),
            Some("homeboy --placement local review test homeboy")
        );
    }

    #[test]
    fn portable_refusal_without_runner_requires_lab_recovery_or_deferral() {
        let _lock = env_lock();
        let _guard = EnvVarGuard::remove(crate::runner::RUNNER_HOSTED_EXEC_ENV);
        let _ci = EnvVarGuard::remove("GITHUB_ACTIONS");
        let warning = evaluate(
            lab_supported_hot("audit"),
            &resources(ResourceRecommendation::Warm),
        )
        .expect("warm machines warn");
        let recovery = admission_recovery(&["homeboy".to_string(), "audit".to_string()], None);
        let error = non_interactive_preflight_error(&warning, false, false, recovery, false)
            .expect("non-interactive hot runs should fail fast");

        assert_eq!(
            error.details["rerun_command"].as_str(),
            Some("homeboy --placement local audit")
        );
        assert!(error.details.get("resume_command").is_none());
        assert!(error
            .message
            .contains("No Homeboy Lab runner is configured on this host"));
        assert!(error
            .message
            .contains("wait for controller pressure to fall"));
        assert!(error
            .message
            .contains("explicit, authorized `--placement local` override"));
    }

    #[test]
    fn disconnected_portable_fuzz_requires_lab_recovery_without_local_rerun() {
        let _lock = env_lock();
        let _ci = EnvVarGuard::remove("GITHUB_ACTIONS");
        let command = Cli::parse_from([
            "homeboy",
            "fuzz",
            "run",
            "--rig",
            "studio",
            "--workload",
            "db-dropin",
        ]);
        let hot = hot_command(&command.command).expect("fuzz run is resource managed");
        assert!(hot.lab_offload_supported, "rig-backed fuzz run is portable");

        let disconnected = LabRunnerReadiness {
            state: crate::runner::runners::LabRunnerReadinessState::Disconnected,
            selected_runner_id: Some("homeboy-lab".to_string()),
            available_runner_ids: Vec::new(),
            reasons: vec!["runner is disconnected".to_string()],
            remediation_commands: vec!["homeboy runner reconnect homeboy-lab".to_string()],
        };
        let warning = evaluate_with_runner_hint(
            hot,
            &resources(ResourceRecommendation::Hot),
            Some(&disconnected),
        )
        .expect("hot controller warns");
        let error = non_interactive_preflight_error(
            &warning,
            false,
            false,
            admission_recovery(
                &["homeboy".to_string(), "fuzz".to_string(), "run".to_string()],
                Some(&disconnected),
            ),
            false,
        )
        .expect("disconnected Lab refuses local execution");

        assert_eq!(
            error.details["rerun_command"].as_str(),
            Some("homeboy --placement local fuzz run")
        );
        assert!(error.details.get("resume_command").is_none());
        assert!(error
            .message
            .contains("homeboy runner reconnect homeboy-lab"));
        assert!(error.message.contains("requires a ready Lab runner"));
    }

    #[test]
    fn absent_lab_recovery_is_replayable_without_runner_repair() {
        let absent = LabRunnerReadiness {
            state: crate::runner::runners::LabRunnerReadinessState::Absent,
            selected_runner_id: None,
            available_runner_ids: Vec::new(),
            reasons: Vec::new(),
            // An intentionally absent Lab must ignore even stale-looking
            // remediation supplied by an upstream inventory projection.
            remediation_commands: vec!["homeboy runner disconnect homeboy-lab".to_string()],
        };
        let recovery = admission_recovery(
            &[
                "homeboy".to_string(),
                "agent-task".to_string(),
                "cook".to_string(),
                "--prompt".to_string(),
                "fix resource admission".to_string(),
            ],
            Some(&absent),
        )
        .expect("argv produces recovery");

        let value = serde_json::to_value(&recovery).expect("recovery serializes");
        assert_eq!(value["schema"], RESOURCE_ADMISSION_RECOVERY_SCHEMA);
        assert_eq!(value["run_created"], false);
        assert_eq!(value["choices"][0]["kind"], "defer");
        assert_eq!(value["choices"][0]["retry_after_seconds"], 60);
        assert_eq!(value["choices"][1]["kind"], "local_override");
        assert_eq!(
            value["choices"][1]["command"],
            "homeboy --placement local agent-task cook --prompt 'fix resource admission'"
        );
        assert_eq!(value["choices"][1]["requires_operator_authorization"], true);
        assert_eq!(value["choices"].as_array().expect("choices").len(), 2);

        let warning = evaluate_with_runner_hint(
            lab_supported_hot("agent-task cook/run-plan/retry --run"),
            &resources(ResourceRecommendation::Hot),
            Some(&absent),
        )
        .expect("hot controller warns");
        let error = non_interactive_preflight_error(&warning, false, false, Some(recovery), false)
            .expect("pre-run admission refuses execution");
        assert_eq!(error.details["run_created"], false);
        assert_eq!(
            error.details["rerun_command"],
            "homeboy --placement local agent-task cook --prompt 'fix resource admission'"
        );
        assert!(error.details.get("resume_command").is_none());
        assert_eq!(
            error.details["recovery"]["schema"],
            RESOURCE_ADMISSION_RECOVERY_SCHEMA
        );
        assert_eq!(error.details["recovery"]["run_created"], false);
        assert_eq!(
            error.details["recovery"]["choices"][1]["command"],
            error.details["rerun_command"]
        );
        assert!(error
            .message
            .contains("No configured Homeboy Lab runner is expected"));
        assert!(!error.message.contains("Follow the listed runner recovery"));
        assert!(error.details["recovery"]["choices"]
            .as_array()
            .expect("serialized recovery choices")
            .iter()
            .all(|choice| choice["command"] != "homeboy runner disconnect homeboy-lab"));
        assert!(!warning
            .message
            .contains("homeboy runner disconnect homeboy-lab"));
    }

    #[test]
    fn stale_configured_lab_recovery_includes_evidence_backed_runner_repair() {
        let recovery = admission_recovery(
            &["homeboy".to_string(), "audit".to_string()],
            Some(&LabRunnerReadiness {
                state: crate::runner::runners::LabRunnerReadinessState::Stale,
                selected_runner_id: Some("homeboy-lab".to_string()),
                available_runner_ids: Vec::new(),
                reasons: vec!["runner daemon is stale".to_string()],
                remediation_commands: vec!["homeboy runner refresh homeboy-lab".to_string()],
            }),
        )
        .expect("argv produces recovery");

        let value = serde_json::to_value(recovery).expect("recovery serializes");
        assert_eq!(value["choices"][2]["kind"], "runner_recovery");
        assert_eq!(
            value["choices"][2]["command"],
            "homeboy runner refresh homeboy-lab"
        );
    }

    #[test]
    fn local_override_replaces_existing_nonlocal_placement() {
        let recovery = admission_recovery(
            &[
                "homeboy".to_string(),
                "--placement".to_string(),
                "auto".to_string(),
                "audit".to_string(),
            ],
            None,
        )
        .expect("argv produces recovery");

        let value = serde_json::to_value(recovery).expect("recovery serializes");
        assert_eq!(
            value["choices"][1]["command"],
            "homeboy --placement local audit"
        );
    }

    #[test]
    fn ready_lab_does_not_advertise_unjustified_runner_repair() {
        let mut readiness = ready_lab();
        readiness.remediation_commands = vec!["homeboy runner doctor homeboy-lab".to_string()];
        let recovery = admission_recovery(
            &["homeboy".to_string(), "audit".to_string()],
            Some(&readiness),
        )
        .expect("argv produces recovery");

        let value = serde_json::to_value(recovery).expect("recovery serializes");
        assert_eq!(value["choices"].as_array().expect("choices").len(), 2);
    }

    #[test]
    fn hot_lab_or_local_requires_admission_before_local_fallback() {
        let _lock = env_lock();
        let _guard = EnvVarGuard::remove(crate::runner::RUNNER_HOSTED_EXEC_ENV);
        let _ci = EnvVarGuard::remove("GITHUB_ACTIONS");
        let placement = crate::cli_surface::Placement::LabOrLocal;
        let disconnected = LabRunnerReadiness {
            state: crate::runner::runners::LabRunnerReadinessState::Disconnected,
            selected_runner_id: None,
            available_runner_ids: Vec::new(),
            reasons: vec!["runner is disconnected".to_string()],
            remediation_commands: vec!["homeboy runner connect homeboy-lab".to_string()],
        };
        let warning = evaluate_with_runner_hint(
            lab_supported_hot("agent-task cook/run-plan/retry --run"),
            &resources(ResourceRecommendation::Hot),
            Some(&disconnected),
        )
        .expect("hot controller warns");

        let error = non_interactive_preflight_error(
            &warning,
            placement.is_explicit_local_override(),
            false,
            None,
            false,
        )
        .expect("hot lab-or-local must stop before routing can dispatch a provider");

        assert!(error.message.contains("homeboy runner connect homeboy-lab"));
        assert!(error
            .message
            .contains("explicit, authorized `--placement local` override"));
    }

    #[test]
    fn runner_hosted_exec_does_not_fail_non_interactive_preflight() {
        let _lock = env_lock();
        let _guard = EnvVarGuard::set(crate::runner::RUNNER_HOSTED_EXEC_ENV, "1");
        let warning = evaluate(
            lab_supported_hot("agent-task cook/run-plan"),
            &resources(ResourceRecommendation::Hot),
        )
        .expect("hot machines warn");

        assert!(non_interactive_preflight_error(&warning, false, false, None, false).is_none());
    }

    #[test]
    fn interactive_or_forced_hot_warning_does_not_fail_preflight() {
        let _lock = env_lock();
        let _guard = EnvVarGuard::remove(crate::runner::RUNNER_HOSTED_EXEC_ENV);
        let warning = evaluate(
            lab_supported_hot("audit"),
            &resources(ResourceRecommendation::Hot),
        )
        .expect("hot machines warn");

        assert!(non_interactive_preflight_error(&warning, false, true, None, false).is_none());
        assert!(non_interactive_preflight_error(&warning, true, false, None, false).is_none());
    }

    #[test]
    fn context_records_severity_warning_and_host_snapshot_when_hot() {
        let resources = resources(ResourceRecommendation::Hot);
        let warning = evaluate(lab_supported_hot("bench"), &resources).expect("warning");
        let context = resource_policy_context_from_evaluation(
            lab_supported_hot("bench"),
            &resources,
            Some(&warning),
            false,
            false,
            Some(&ready_lab()),
            false,
        );

        assert_eq!(context.command, "bench");
        assert_eq!(context.severity, "hot");
        assert!(!context.local_override);
        assert!(context.warned);
        assert!(context
            .message
            .as_deref()
            .expect("message")
            .contains("Resource policy warning"));
        assert_eq!(
            context.runner_selection,
            ResourcePolicyRunnerSelection {
                runner_id: Some("homeboy-lab".to_string()),
                available_runner_ids: vec!["homeboy-lab".to_string()],
                readiness_state: "connected_ready".to_string(),
                readiness_reasons: Vec::new(),
                remediation_commands: Vec::new(),
                reason: "default_lab_runner".to_string(),
            }
        );
        assert_eq!(context.host.load_severity, "hot");
        assert_eq!(context.host.load_one, Some(9.0));
        assert_eq!(context.host.cpu_count, 4);
        assert_eq!(context.host.memory_severity, None);
        assert_eq!(context.host.relevant_process_count, 0);
        assert_eq!(context.host.process_severity, "ok");
        assert_eq!(context.host.active_rig_lease_count, 0);
        assert_eq!(context.host.rig_lease_severity, "ok");
    }

    #[test]
    fn context_records_local_placement_for_hot_machine() {
        let resources = resources(ResourceRecommendation::Hot);
        let warning = evaluate(lab_supported_hot("bench"), &resources).expect("warning");
        let context = resource_policy_context_from_evaluation(
            lab_supported_hot("bench"),
            &resources,
            Some(&warning),
            true,
            false,
            Some(&ready_lab()),
            false,
        );

        assert!(context.local_override);
        assert!(context.warned);
        assert_eq!(context.severity, "hot");
        assert!(context.message.is_some());
        assert_eq!(context.runner_selection.reason, "placement_local_override");
        assert_eq!(context.runner_selection.runner_id, None);
    }

    #[test]
    fn context_records_ok_machine_with_no_warning() {
        let resources = resources(ResourceRecommendation::Ok);
        assert!(evaluate(lab_supported_hot("bench"), &resources).is_none());
        let context = resource_policy_context_from_evaluation(
            lab_supported_hot("bench"),
            &resources,
            None,
            false,
            false,
            None,
            false,
        );

        assert_eq!(context.severity, "ok");
        assert!(!context.warned);
        assert!(context.message.is_none());
        assert!(!context.local_override);
        assert_eq!(context.runner_selection.reason, "local_no_default_runner");
    }

    #[test]
    fn context_includes_memory_snapshot_when_available() {
        let mut resources = resources(ResourceRecommendation::Warm);
        resources.memory = Some(MemorySummary {
            total_mb: 32_000,
            available_mb: 1_500,
            used_percent: 95.3,
            recommendation: ResourceRecommendation::Warm,
        });
        resources.rig_leases.concurrency_limit = Some(8);
        let context = resource_policy_context_from_evaluation(
            lab_supported_hot("bench"),
            &resources,
            None,
            false,
            false,
            None,
            false,
        );

        assert_eq!(context.host.memory_severity.as_deref(), Some("warm"));
        assert_eq!(context.host.memory_used_percent, Some(95.3));
        assert_eq!(context.host.memory_available_mb, Some(1_500));
        assert_eq!(context.host.memory_total_mb, Some(32_000));
        assert_eq!(context.host.rig_lease_concurrency_limit, Some(8));
    }

    #[test]
    fn context_serializes_to_json_with_expected_keys() {
        let resources = resources(ResourceRecommendation::Hot);
        let warning = evaluate(lab_supported_hot("bench"), &resources).expect("warning");
        let context = resource_policy_context_from_evaluation(
            lab_supported_hot("bench"),
            &resources,
            Some(&warning),
            false,
            false,
            Some(&ready_lab()),
            false,
        );
        let value = resource_policy_context_to_json(&context);

        assert_eq!(value["command"], "bench");
        assert_eq!(value["severity"], "hot");
        assert_eq!(value["local_override"], false);
        assert_eq!(value["warned"], true);
        assert!(value["message"].is_string());
        assert_eq!(value["runner_selection"]["runner_id"], "homeboy-lab");
        assert_eq!(
            value["runner_selection"]["available_runner_ids"][0],
            "homeboy-lab"
        );
        assert_eq!(
            value["runner_selection"]["readiness_state"],
            "connected_ready"
        );
        assert_eq!(value["runner_selection"]["reason"], "default_lab_runner");
        assert_eq!(value["host"]["load_severity"], "hot");
        assert_eq!(value["host"]["cpu_count"], 4);
        assert!(value["host"].get("rig_lease_concurrency_limit").is_none());
    }

    #[test]
    fn ci_execution_does_not_fail_non_interactive_preflight() {
        // #7735: inside GitHub Actions the warm-machine refusal must not fire.
        // The runner is ephemeral and non-interactive by design; refusing there
        // fails otherwise-good PR checks.
        let _lock = env_lock();
        let _hosted = EnvVarGuard::remove(crate::runner::RUNNER_HOSTED_EXEC_ENV);
        let _ci = EnvVarGuard::set("GITHUB_ACTIONS", "true");
        let warning = evaluate(
            lab_supported_hot("review test"),
            &resources(ResourceRecommendation::Hot),
        )
        .expect("hot machines warn");

        assert!(non_interactive_preflight_error(&warning, false, false, None, false).is_none());
    }

    #[test]
    fn non_ci_shell_still_refuses_when_warm() {
        // Guard against the CI bypass leaking into ordinary non-interactive
        // shells (e.g. cron, agent runners) where the refusal is still correct.
        let _lock = env_lock();
        let _hosted = EnvVarGuard::remove(crate::runner::RUNNER_HOSTED_EXEC_ENV);
        let _ci = EnvVarGuard::remove("GITHUB_ACTIONS");
        let warning = evaluate(
            lab_supported_hot("review test"),
            &resources(ResourceRecommendation::Hot),
        )
        .expect("hot machines warn");

        assert!(non_interactive_preflight_error(&warning, false, false, None, false).is_some());
    }

    #[test]
    fn portable_warning_without_runner_does_not_advertise_lab() {
        // #7749: on a host with no Lab runner configured, the warning must not
        // recommend connecting/using a Lab runner as if one were available.
        let warning = evaluate(
            lab_supported_hot("review test"),
            &resources(ResourceRecommendation::Hot),
        )
        .expect("hot machines warn");

        assert!(warning
            .message
            .contains("No Homeboy Lab runner is configured on this host"));
        assert!(warning
            .message
            .contains("Lab offload is not currently available"));
        assert!(!warning
            .message
            .contains("Connect a default Homeboy Lab runner"));
        // A genuinely configured runner should still be named.
        let with_runner = evaluate_with_runner_hint(
            lab_supported_hot("review test"),
            &resources(ResourceRecommendation::Hot),
            Some(&ready_lab()),
        )
        .expect("hot machines warn");
        assert!(with_runner.message.contains("Lab runner `homeboy-lab`"));
    }

    struct EnvVarGuard {
        name: &'static str,
        prior: Option<String>,
    }

    impl EnvVarGuard {
        fn set(name: &'static str, value: &str) -> Self {
            let prior = std::env::var(name).ok();
            std::env::set_var(name, value);
            Self { name, prior }
        }

        fn remove(name: &'static str) -> Self {
            let prior = std::env::var(name).ok();
            std::env::remove_var(name);
            Self { name, prior }
        }
    }

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .expect("resource policy env test lock poisoned")
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(value) => std::env::set_var(self.name, value),
                None => std::env::remove_var(self.name),
            }
        }
    }
}
