//! Typed, parser-independent admission and placement preflight contract.
//!
//! Command families translate parsed values into this contract above core.
//! Core deliberately does not know Clap, `Cli`, or a product command enum.

use serde::{Deserialize, Serialize};
use std::sync::{OnceLock, RwLock};

use crate::resource_policy_context::ResourcePolicyContext;
use homeboy_lab_contract::{
    EffectiveExecutionPlacement, ExecutionPlacementDecision, ExecutionPlacementFallback,
    ExecutionPlacementIdentity, ExecutionPlacementOverrideAuthorization,
    ExecutionPlacementRequirement, ExecutionPlacementRunnerSelection, Placement,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedCommandIdentity {
    pub family: String,
    pub operation: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceHeat {
    None,
    Warm,
    Hot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResourceAdmissionRequirement {
    Exempt,
    Required {
        label: String,
        engages_at: ResourceHeat,
    },
}

/// Raw runtime pressure evidence. Adapters report observations, never an
/// admission verdict; the shared evaluator owns the verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResourceAdmissionEvidence {
    Observed { pressure: ResourceHeat },
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResourceAdmissionDecision {
    NotRequired,
    Admitted,
    Rejected {
        label: String,
        engages_at: ResourceHeat,
        evidence: ResourceAdmissionEvidence,
    },
}

/// Derive admission solely from declared requirements and raw runtime evidence.
/// Missing evidence rejects required work so adapters cannot accidentally bless
/// execution by omitting a probe.
pub fn evaluate_resource_admission(
    requirement: &ResourceAdmissionRequirement,
    evidence: ResourceAdmissionEvidence,
) -> ResourceAdmissionDecision {
    let ResourceAdmissionRequirement::Required { label, engages_at } = requirement else {
        return ResourceAdmissionDecision::NotRequired;
    };
    let admitted = matches!(evidence, ResourceAdmissionEvidence::Observed { pressure } if pressure < *engages_at);
    if admitted {
        ResourceAdmissionDecision::Admitted
    } else {
        ResourceAdmissionDecision::Rejected {
            label: label.clone(),
            engages_at: *engages_at,
            evidence,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControllerExecution {
    Ordinary,
    ControllerOnly,
    SplitPlacementCoordinator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeferredWorkloadPolicy {
    Forbidden,
    Eligible,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlacementIntent {
    Auto,
    Local,
    Lab,
    LabOrLocal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerIntent {
    Default,
    Explicit(String),
    ReadinessRepair(String),
    CommandLocal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerNormalization {
    None,
    RunsCommandOption,
    PinnedCookArgv,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LabRouteIntent {
    Unsupported,
    Supported {
        automatic: bool,
    },
    /// Route an existing record read; runner capacity does not admit new work.
    ReadPolling,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceRequirement {
    None,
    CaptureExecution,
}

/// All policy facts required before executing a parsed command.
///
/// Nested enums replace optional bags: adapters state every policy axis and
/// unrecognized command shapes naturally remain non-routable/non-deferred.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedCommandPreflightInput {
    pub identity: ParsedCommandIdentity,
    pub resource_admission: ResourceAdmissionRequirement,
    pub controller_execution: ControllerExecution,
    pub deferred_workload: DeferredWorkloadPolicy,
    pub placement: PlacementIntent,
    pub runner: RunnerIntent,
    pub runner_normalization: RunnerNormalization,
    pub lab_route: LabRouteIntent,
    pub provenance: ProvenanceRequirement,
}

/// Typed runner inventory consumed by a parsed-command preflight.  This is
/// deliberately independent of the Lab runner implementation so any command
/// parser can provide the same evidence to the generic execution contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabReadinessSnapshot {
    pub state: String,
    pub selected_runner_id: Option<String>,
    pub available_runner_ids: Vec<String>,
    pub reasons: Vec<String>,
    pub remediation_commands: Vec<String>,
    /// Connected runners admitted only for a bounded readiness-repair action.
    /// Ordinary workload admission never consumes this evidence.
    #[serde(default)]
    pub repair_admitted_runner_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeferredWorkloadDecision {
    NotApplicable,
    /// Queue the command as a daemon `step` that waits for a Lab runner.
    Defer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FallbackDirective {
    None,
    LocalCapacity,
    LocalAllowed,
    RequiredLabUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenericRoutePolicySnapshot {
    pub command_supports_lab: bool,
    pub automatic_authorized: bool,
    pub selected_runner_id: Option<String>,
}

/// Runtime evidence gathered by a command adapter before resolution. Keeping
/// this separate from parsed intent lets non-Clap command surfaces use the
/// same resolver without importing runner or resource-policy implementations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParsedCommandPolicySnapshot {
    /// Raw controller-pressure evidence consumed by the shared admission
    /// evaluator. This is deliberately not a caller-provided decision.
    pub resource_admission_evidence: ResourceAdmissionEvidence,
    pub resource_policy: Option<ResourcePolicyContext>,
    pub lab_readiness: Option<LabReadinessSnapshot>,
    pub selected_runner_id: Option<String>,
    pub generic_route: GenericRoutePolicySnapshot,
    /// Raw admission facts. The resolver derives the deferred outcome from the
    /// command policy instead of accepting a preassembled result axis.
    pub deferred_pressure_refusal: bool,
    pub runner_admitted: bool,
    pub auto_local_capacity_fallback: bool,
}

/// Resolve generic Lab routing from immutable adapter facts. The caller records
/// command-contract and pressure authorization before route begins.
pub fn resolve_generic_route_runner(
    input: &ParsedCommandPreflightInput,
    policy: &GenericRoutePolicySnapshot,
) -> Option<String> {
    if !policy.command_supports_lab || input.controller_execution != ControllerExecution::Ordinary {
        return None;
    }
    match (&input.runner, &input.placement) {
        (_, PlacementIntent::Local) => None,
        (RunnerIntent::Explicit(_) | RunnerIntent::ReadinessRepair(_), _) => {
            policy.selected_runner_id.clone()
        }
        _ if policy.automatic_authorized => policy.selected_runner_id.clone(),
        _ => None,
    }
}

/// Resolve every execution-policy axis from parser-independent command intent
/// and one immutable runtime snapshot. Consumers receive this completed result
/// and must not reconstruct individual decisions from ambient state.
pub fn resolve_parsed_command_preflight(
    normalized_args: Vec<String>,
    input: ParsedCommandPreflightInput,
    policy: ParsedCommandPolicySnapshot,
) -> crate::Result<ParsedCommandPreflightResult> {
    if policy.generic_route.command_supports_lab
        && !matches!(
            input.lab_route,
            LabRouteIntent::Supported { .. } | LabRouteIntent::ReadPolling
        )
    {
        return Err(crate::Error::validation_invalid_argument(
            "generic_route",
            "generic Lab routing requires a supported Lab route intent",
            None,
            None,
        ));
    }
    if policy.generic_route.selected_runner_id != policy.selected_runner_id {
        return Err(crate::Error::validation_invalid_argument(
            "generic_route.selected_runner_id",
            "generic route runner must equal the primary selected runner",
            policy.generic_route.selected_runner_id.clone(),
            None,
        ));
    }
    let readiness_repair_admitted = match &input.runner {
        RunnerIntent::ReadinessRepair(runner_id) => {
            policy.selected_runner_id.as_deref() == Some(runner_id.as_str())
                && policy.lab_readiness.as_ref().is_some_and(|readiness| {
                    readiness
                        .repair_admitted_runner_ids
                        .iter()
                        .any(|admitted| admitted == runner_id)
                })
        }
        _ => false,
    };
    let selected_runner_admitted = readiness_repair_admitted
        || (policy.runner_admitted
            && policy.lab_readiness.as_ref().is_some_and(|readiness| {
                readiness.state == "connected_ready"
                    && readiness.selected_runner_id == policy.selected_runner_id
                    && readiness
                        .available_runner_ids
                        .iter()
                        .any(|runner| Some(runner) == policy.selected_runner_id.as_ref())
            }));
    let preferred_runner_inadmissible = policy.selected_runner_id.is_some()
        && !selected_runner_admitted
        && !matches!(input.lab_route, LabRouteIntent::ReadPolling);
    // Split-placement Cook keeps its durable admission on the controller but
    // requires an admitted Lab runner for the provider attempt whenever a Lab
    // runner is configured. Auto never substitutes the controller for an
    // unavailable, unverified, or stale Lab: preview and admission both fail
    // with `lab_unavailable` here. Explicit `local` / `lab-or-local` remain the
    // operator-approved local routes, and a host with no configured runner
    // still resolves Auto locally. A capacity-blocked Lab is busy, not a
    // license to probe providers on the controller: refuse with retry advice
    // before provider readiness or durable admission runs.
    let auto_split_placement_requires_lab = matches!(
        (
            &input.controller_execution,
            &input.placement,
            &input.lab_route
        ),
        (
            &ControllerExecution::SplitPlacementCoordinator,
            &PlacementIntent::Auto,
            &LabRouteIntent::Supported { automatic: true }
        )
    ) && matches!(input.runner, RunnerIntent::Default)
        && policy
            .lab_readiness
            .as_ref()
            .is_some_and(|readiness| readiness.state != "absent");
    let required = if matches!(input.placement, PlacementIntent::Lab)
        || matches!(
            input.runner,
            RunnerIntent::Explicit(_) | RunnerIntent::ReadinessRepair(_)
        )
        || auto_split_placement_requires_lab
    {
        ExecutionPlacementRequirement::Lab
    } else {
        ExecutionPlacementRequirement::Either
    };

    // Ordinary automatic routes may still fall through to local. Cook's
    // split-placement route is intentionally excluded because local fallback
    // is an explicit operator choice there.
    let auto_route_fallback = preferred_runner_inadmissible
        && matches!(
            input.placement,
            PlacementIntent::Auto | PlacementIntent::LabOrLocal
        )
        && matches!(input.runner, RunnerIntent::Default)
        && !auto_split_placement_requires_lab;
    if preferred_runner_inadmissible && !auto_route_fallback {
        if auto_split_placement_requires_lab {
            return Err(lab_unavailable_error(
                policy
                    .lab_readiness
                    .as_ref()
                    .expect("auto split placement requires configured Lab readiness"),
            ));
        }
        let runner_reason = policy.selected_runner_id.as_deref().map(|runner_id| {
            lab_route_inadmissible_reason(runner_id, policy.lab_readiness.as_ref())
        });
        let mut remediation = {
            policy
                .lab_readiness
                .as_ref()
                .map(|readiness| {
                    readiness
                        .remediation_commands
                        .iter()
                        .cloned()
                        .chain(readiness.reasons.iter().cloned())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        remediation.extend(runner_reason);
        return Err(selected_runner_readiness_error(
            policy.selected_runner_id.clone(),
            policy.lab_readiness.as_ref(),
            remediation,
        ));
    }
    if policy.auto_local_capacity_fallback
        && !matches!(
            input.placement,
            PlacementIntent::Auto | PlacementIntent::LabOrLocal
        )
    {
        return Err(crate::Error::validation_invalid_argument(
            "fallback",
            "local capacity fallback requires auto or lab-or-local placement",
            None,
            None,
        ));
    }
    let selected_runner_id =
        if auto_route_fallback || matches!(input.placement, PlacementIntent::Local) {
            None
        } else {
            policy.selected_runner_id.clone()
        };
    let selected =
        if matches!(input.placement, PlacementIntent::Local) || selected_runner_id.is_none() {
            EffectiveExecutionPlacement::Local
        } else {
            EffectiveExecutionPlacement::Lab
        };
    // Controller admission covers controller-owned work only. Once the
    // resolved route is Lab and its selected runner passed readiness checks,
    // controller load cannot block the provider attempt. Local and fallback
    // routes remain subject to controller pressure.
    let resource_admission = if selected_runner_admitted
        && matches!(selected, EffectiveExecutionPlacement::Lab)
        && !matches!(
            input.controller_execution,
            ControllerExecution::ControllerOnly
        ) {
        ResourceAdmissionDecision::Admitted
    } else {
        evaluate_resource_admission(
            &input.resource_admission,
            policy.resource_admission_evidence,
        )
    };
    let local_route_admissible = policy.auto_local_capacity_fallback
        || !matches!(
            resource_admission,
            ResourceAdmissionDecision::Rejected { .. }
        );
    if auto_route_fallback && !local_route_admissible {
        let lab_line = policy
            .selected_runner_id
            .as_deref()
            .map(|runner_id| {
                lab_route_inadmissible_reason(runner_id, policy.lab_readiness.as_ref())
            })
            .expect("auto route fallback requires a preferred runner");
        let local_line = local_route_inadmissible_reason(&resource_admission)
            .expect("rejected local admission supplies a route reason");
        return Err(crate::Error::validation_invalid_argument(
            "placement",
            format!("no admissible route: {lab_line}; {local_line}"),
            None,
            Some(vec![lab_line, local_line]),
        ));
    }
    let deferred_workload = match input.deferred_workload {
        DeferredWorkloadPolicy::Forbidden => {
            if policy.deferred_pressure_refusal {
                return Err(crate::Error::validation_invalid_argument(
                    "deferred_workload",
                    "deferred evidence requires an eligible deferred workload policy",
                    None,
                    None,
                ));
            }
            DeferredWorkloadDecision::NotApplicable
        }
        DeferredWorkloadPolicy::Eligible
            if !selected_runner_admitted && policy.deferred_pressure_refusal =>
        {
            DeferredWorkloadDecision::Defer
        }
        DeferredWorkloadPolicy::Eligible => DeferredWorkloadDecision::NotApplicable,
    };
    if auto_split_placement_requires_lab && selected_runner_id.is_none() {
        return Err(lab_unavailable_error(
            policy
                .lab_readiness
                .as_ref()
                .expect("auto split placement requires configured Lab readiness"),
        ));
    }
    let runner = selected_runner_id
        .as_ref()
        .map(|runner_id| ExecutionPlacementRunnerSelection {
            runner_id: runner_id.clone(),
            source: if matches!(
                input.runner,
                RunnerIntent::Explicit(_) | RunnerIntent::ReadinessRepair(_)
            ) {
                homeboy_lab_contract::RunnerSelectionSource::Explicit
            } else {
                homeboy_lab_contract::RunnerSelectionSource::Policy
            },
        });
    let fallback =
        if required != ExecutionPlacementRequirement::Lab && policy.auto_local_capacity_fallback {
            FallbackDirective::LocalCapacity
        } else if auto_route_fallback {
            FallbackDirective::LocalAllowed
        } else if matches!(input.placement, PlacementIntent::Lab) && selected_runner_id.is_none() {
            FallbackDirective::RequiredLabUnavailable
        } else {
            FallbackDirective::None
        };
    // Operator-facing evidence for *why* an operator's Lab-desiring placement
    // resolved to local execution. This was structurally present but never
    // populated, so a silent auto -> local degrade left no trace in preview or
    // dispatch output for the operator to discover later (#14729). Auto route
    // substitution names the skipped runner so the operator is not sent to
    // discover `--placement local` from a different command (#14754).
    let fallback_reason = if auto_route_fallback {
        policy.selected_runner_id.as_deref().map(|runner_id| {
            format!(
                "{}; routing to local.",
                lab_route_inadmissible_reason(runner_id, policy.lab_readiness.as_ref())
            )
        })
    } else {
        (selected == EffectiveExecutionPlacement::Local
            && !matches!(input.placement, PlacementIntent::Local))
        .then(|| {
            policy
                .lab_readiness
                .as_ref()
                .and_then(lab_readiness_fallback_reason)
        })
        .flatten()
    };
    let placement = PlacementDirective {
        requested: match input.placement {
            PlacementIntent::Auto => Placement::Auto,
            PlacementIntent::Local => Placement::Local,
            PlacementIntent::Lab => Placement::Lab,
            PlacementIntent::LabOrLocal => Placement::LabOrLocal,
        },
        required,
        selected,
        runner,
        fallback: ExecutionPlacementFallback {
            local_allowed: required != ExecutionPlacementRequirement::Lab
                && (policy.auto_local_capacity_fallback
                    || matches!(
                        input.placement,
                        PlacementIntent::Auto | PlacementIntent::LabOrLocal
                    )),
            reason: fallback_reason,
        },
        override_authorization: ExecutionPlacementOverrideAuthorization {
            authorized: matches!(input.placement, PlacementIntent::Local),
            authority: matches!(input.placement, PlacementIntent::Local)
                .then(|| "operator --placement local".to_string()),
        },
    };
    let generic_route_runner_id = if selected_runner_id.is_none() {
        None
    } else {
        resolve_generic_route_runner(&input, &policy.generic_route)
    };
    Ok(ParsedCommandPreflightResult {
        normalized_args,
        input,
        resource_admission,
        resource_policy: policy.resource_policy,
        lab_readiness: policy.lab_readiness,
        deferred_workload,
        fallback,
        placement,
        generic_route_runner_id,
        selected_runner_id,
    })
}

/// Render the reason a Lab-desiring placement request degraded to local
/// execution. `readiness.reasons` already carries a specific per-runner
/// explanation (e.g. `capacity_reached`) when at least one runner is
/// registered; a snapshot with no registered runner at all (`absent`) or
/// otherwise blocked state carries no per-runner reasons, so this falls back
/// to a human-readable rendering of the readiness state itself rather than
/// leaving the operator with no explanation at all.
fn lab_readiness_fallback_reason(readiness: &LabReadinessSnapshot) -> Option<String> {
    if !readiness.reasons.is_empty() {
        return Some(
            readiness
                .reasons
                .iter()
                .chain(readiness.remediation_commands.iter())
                .cloned()
                .collect::<Vec<_>>()
                .join("; "),
        );
    }
    let described = match readiness.state.as_str() {
        "connected_ready" => return None,
        "absent" => "no Lab runner is configured",
        "disconnected" => "the configured Lab runner is disconnected",
        "stale" => "the configured Lab runner's admission is stale",
        "connected_ineligible" => "the connected Lab runner is not eligible for this workload",
        "capacity_blocked" => "the configured Lab runner is at capacity",
        READINESS_UNOBSERVED => "the connected Lab runner's readiness is not observed yet",
        other => {
            return Some(format!(
                "no eligible Lab runner is currently ready ({other})"
            ))
        }
    };
    Some(described.to_string())
}

/// Readiness state of a connected runner whose readiness evidence is missing,
/// stale, or failed transiently. Admission waits and re-observes.
pub const READINESS_UNOBSERVED: &str = "readiness_unobserved";

/// Whether a refusal diagnostic (`{details|data: {...}}`) names unobserved
/// readiness, so queued admission requeues instead of failing.
pub fn readiness_unobserved_refusal(diagnostic: &serde_json::Value) -> bool {
    [&diagnostic["details"], &diagnostic["data"]]
        .into_iter()
        .any(|details| details["admission_wait"] == READINESS_UNOBSERVED)
}

/// Structured readiness evidence attached to every Lab readiness refusal so
/// the recorded failure keeps the state and reasons, not only the headline.
fn attach_readiness_evidence(
    mut error: crate::Error,
    readiness: Option<&LabReadinessSnapshot>,
) -> crate::Error {
    let Some(readiness) = readiness else {
        return error;
    };
    if !error.details.is_object() {
        error.details = serde_json::json!({});
    }
    error.details["readiness_state"] = serde_json::json!(readiness.state);
    error.details["readiness_reasons"] = serde_json::json!(readiness.reasons);
    error.details["lab_readiness"] = serde_json::json!(readiness);
    if readiness.state == READINESS_UNOBSERVED {
        error.details["admission_wait"] = serde_json::json!(READINESS_UNOBSERVED);
        error = error.with_retryable(true);
    }
    error
}

/// The refusal for an explicitly selected runner without admitted readiness.
/// It names the readiness state and reasons.
fn selected_runner_readiness_error(
    runner_id: Option<String>,
    readiness: Option<&LabReadinessSnapshot>,
    remediation: Vec<String>,
) -> crate::Error {
    let mut problem = "selected runner requires admitted connected readiness evidence".to_string();
    if let Some(readiness) = readiness {
        problem.push_str(&format!(" (readiness: {}", readiness.state));
        if !readiness.reasons.is_empty() {
            problem.push_str(&format!("; reasons: {}", readiness.reasons.join(", ")));
        }
        problem.push(')');
    }
    attach_readiness_evidence(
        crate::Error::validation_invalid_argument(
            "selected_runner_id",
            problem,
            runner_id,
            Some(remediation),
        ),
        readiness,
    )
}

/// The typed refusal for a Lab-preferring `--placement auto` workload whose
/// configured Lab has no admitted runner. It names the readiness reason and
/// the runner reconnect as the next action; it never offers the controller,
/// because local placement requires an explicit operator choice.
fn lab_unavailable_error(readiness: &LabReadinessSnapshot) -> crate::Error {
    if readiness.state == "capacity_blocked" {
        // Readiness snapshots do not carry a runner-supplied retry interval.
        // Use the same bounded retry advice as CLI pressure admission.
        let retry_after_seconds = 60;
        let reason = if readiness.reasons.is_empty() {
            "capacity".to_string()
        } else {
            readiness.reasons.join("; ")
        };
        return attach_readiness_evidence(
            crate::Error::new(
                crate::ErrorCode::RunnerLabUnavailable,
                format!(
                    "capacity_blocked: {reason}; Lab admission is temporarily blocked. Retry after {retry_after_seconds} seconds; `--placement auto` does not run this workload on the controller."
                ),
                serde_json::json!({
                    "field": "placement",
                    "reason": "capacity_blocked",
                    "retry_after_seconds": retry_after_seconds,
                    "run_created": false,
                }),
            )
            .with_retryable(true),
            Some(readiness),
        );
    }
    let reason = if readiness.reasons.is_empty() {
        lab_readiness_fallback_reason(readiness).unwrap_or_else(|| readiness.state.clone())
    } else {
        readiness.reasons.join("; ")
    };
    let next_action = readiness
        .remediation_commands
        .first()
        .cloned()
        .unwrap_or_else(|| "homeboy runner connect <runner-id>".to_string());
    let error = crate::Error::new(
        crate::ErrorCode::RunnerLabUnavailable,
        format!(
            "lab_unavailable: Lab runner readiness is {} ({reason}); `--placement auto` does not run this workload on the controller. Next: `{next_action}`. Controller execution requires an explicit `--placement local`.",
            readiness.state
        ),
        serde_json::json!({
            "field": "placement",
            "reason": "lab_unavailable",
            "readiness_state": readiness.state,
            "readiness_reasons": readiness.reasons,
            "next_action": next_action,
            "remediation_commands": readiness.remediation_commands,
            "run_created": false,
        }),
    )
    .with_retryable(true);
    attach_readiness_evidence(error, Some(readiness))
}

fn lab_route_inadmissible_reason(
    runner_id: &str,
    readiness: Option<&LabReadinessSnapshot>,
) -> String {
    let detail = readiness
        .and_then(lab_readiness_fallback_reason)
        .unwrap_or_else(|| "no admitted connected readiness evidence".to_string());
    format!("Lab runner {runner_id} inadmissible ({detail})")
}

fn local_route_inadmissible_reason(admission: &ResourceAdmissionDecision) -> Option<String> {
    match admission {
        ResourceAdmissionDecision::Rejected { label, .. } => Some(format!(
            "local inadmissible (resource admission rejected for {label})"
        )),
        _ => None,
    }
}

/// Fully resolved placement policy. Parsed-command preflight owns every policy
/// input; route only binds the identity produced by materialization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacementDirective {
    pub requested: Placement,
    pub required: ExecutionPlacementRequirement,
    pub selected: EffectiveExecutionPlacement,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner: Option<ExecutionPlacementRunnerSelection>,
    pub fallback: ExecutionPlacementFallback,
    pub override_authorization: ExecutionPlacementOverrideAuthorization,
}

impl PlacementDirective {
    /// Bind materialized workload identity without observing command, runner,
    /// resource, or process state. This is deliberately the only late phase.
    pub fn finalize(&self, identity: ExecutionPlacementIdentity) -> ExecutionPlacementDecision {
        ExecutionPlacementDecision::new(
            "lab-route-contract",
            "v1",
            identity,
            self.requested,
            self.required,
            self.selected,
            self.runner.clone(),
            self.fallback.clone(),
            self.override_authorization.clone(),
        )
    }
}

/// The policy a recorded decision was routed under, without its bound identity.
impl From<&ExecutionPlacementDecision> for PlacementDirective {
    fn from(decision: &ExecutionPlacementDecision) -> Self {
        Self {
            requested: decision.requested,
            required: decision.required,
            selected: decision.selected,
            runner: decision.runner.clone(),
            fallback: decision.fallback.clone(),
            override_authorization: decision.override_authorization.clone(),
        }
    }
}

/// The immutable controller-side decision consumed by execution and dispatch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParsedCommandPreflightResult {
    /// The exact argv after runtime normalization. No downstream consumer may
    /// inspect process argv to recreate any part of this decision.
    pub normalized_args: Vec<String>,
    pub input: ParsedCommandPreflightInput,
    pub resource_admission: ResourceAdmissionDecision,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_policy: Option<ResourcePolicyContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lab_readiness: Option<LabReadinessSnapshot>,
    pub deferred_workload: DeferredWorkloadDecision,
    pub fallback: FallbackDirective,
    pub placement: PlacementDirective,
    /// Generic Lab routing authority resolved during preflight. Route consumes
    /// this value and never reopens command policy or pressure state.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generic_route_runner_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_runner_id: Option<String>,
}

impl ParsedCommandPreflightResult {
    pub fn new(
        normalized_args: Vec<String>,
        input: ParsedCommandPreflightInput,
        resource_policy: Option<ResourcePolicyContext>,
        lab_readiness: Option<LabReadinessSnapshot>,
        deferred_workload: DeferredWorkloadDecision,
        fallback: FallbackDirective,
        placement: PlacementDirective,
        selected_runner_id: Option<String>,
    ) -> Self {
        Self {
            normalized_args,
            input,
            resource_admission: ResourceAdmissionDecision::NotRequired,
            resource_policy,
            lab_readiness,
            deferred_workload,
            fallback,
            generic_route_runner_id: None,
            placement,
            selected_runner_id,
        }
    }
}

fn result_storage() -> &'static RwLock<Option<ParsedCommandPreflightResult>> {
    static STORAGE: OnceLock<RwLock<Option<ParsedCommandPreflightResult>>> = OnceLock::new();
    STORAGE.get_or_init(|| RwLock::new(None))
}

/// Capture exactly one completed controller decision for the process. Routing
/// consumes this rather than repeating readiness or placement discovery.
pub fn capture_result(result: ParsedCommandPreflightResult) {
    #[cfg(any(test, feature = "test-support"))]
    if test_scope::capture(&result) {
        return;
    }
    let mut slot = result_storage()
        .write()
        .unwrap_or_else(|error| error.into_inner());
    if slot.is_none() {
        *slot = Some(result);
    }
}

pub fn captured_result() -> Option<ParsedCommandPreflightResult> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(scoped) = test_scope::captured() {
        return scoped;
    }
    result_storage().read().ok().and_then(|slot| slot.clone())
}

#[cfg(any(test, feature = "test-support"))]
pub fn reset_captured_result_for_test() {
    if test_scope::reset() {
        return;
    }
    if let Ok(mut slot) = result_storage().write() {
        *slot = None;
    }
}

/// Give the calling thread its own captured-preflight slot until the returned
/// guard drops.
///
/// The production slot is process-global because one CLI process makes
/// exactly one controller decision. A libtest process runs many "CLI
/// invocations" concurrently, so without a scope one test's captured Lab
/// readiness leaks into another test's preview/dispatch. While a scope is
/// installed, `capture_result`, `captured_result`, and
/// `reset_captured_result_for_test` on this thread touch only the scoped slot,
/// which starts empty. `HomeGuard` installs one for every isolated-home test.
#[cfg(any(test, feature = "test-support"))]
#[must_use = "the captured-preflight scope ends when the guard drops"]
pub fn isolate_captured_result_for_test() -> CapturedResultTestScope {
    CapturedResultTestScope {
        prior: test_scope::install(),
        _not_send: std::marker::PhantomData,
    }
}

/// Guard returned by [`isolate_captured_result_for_test`]. Restores the
/// thread's previous scope (or the process-global slot) on drop.
#[cfg(any(test, feature = "test-support"))]
pub struct CapturedResultTestScope {
    prior: Option<Option<ParsedCommandPreflightResult>>,
    // The scope is thread-local; dropping it on another thread would restore
    // the wrong thread's slot.
    _not_send: std::marker::PhantomData<*const ()>,
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for CapturedResultTestScope {
    fn drop(&mut self) {
        test_scope::restore(self.prior.take());
    }
}

#[cfg(any(test, feature = "test-support"))]
mod test_scope {
    use super::ParsedCommandPreflightResult;
    use std::cell::RefCell;

    thread_local! {
        /// `None`: no scope installed (use the process-global slot).
        /// `Some(slot)`: this thread's isolated captured result.
        static SCOPED: RefCell<Option<Option<ParsedCommandPreflightResult>>> =
            const { RefCell::new(None) };
    }

    pub(super) fn install() -> Option<Option<ParsedCommandPreflightResult>> {
        SCOPED.with(|scoped| scoped.borrow_mut().replace(None))
    }

    pub(super) fn restore(prior: Option<Option<ParsedCommandPreflightResult>>) {
        SCOPED.with(|scoped| *scoped.borrow_mut() = prior);
    }

    pub(super) fn capture(result: &ParsedCommandPreflightResult) -> bool {
        SCOPED.with(|scoped| match scoped.borrow_mut().as_mut() {
            Some(slot) => {
                if slot.is_none() {
                    *slot = Some(result.clone());
                }
                true
            }
            None => false,
        })
    }

    pub(super) fn captured() -> Option<Option<ParsedCommandPreflightResult>> {
        SCOPED.with(|scoped| scoped.borrow().clone())
    }

    pub(super) fn reset() -> bool {
        SCOPED.with(|scoped| match scoped.borrow_mut().as_mut() {
            Some(slot) => {
                *slot = None;
                true
            }
            None => false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_lab_execution_is_admitted_even_when_controller_is_hot() {
        let input = |controller_execution| ParsedCommandPreflightInput {
            identity: ParsedCommandIdentity {
                family: "agent-task".into(),
                operation: vec!["cook".into()],
            },
            resource_admission: ResourceAdmissionRequirement::Required {
                label: "agent-task cook".into(),
                engages_at: ResourceHeat::Warm,
            },
            controller_execution,
            deferred_workload: DeferredWorkloadPolicy::Forbidden,
            placement: PlacementIntent::Lab,
            runner: RunnerIntent::Explicit("lab-a".into()),
            runner_normalization: RunnerNormalization::None,
            lab_route: LabRouteIntent::Supported { automatic: false },
            provenance: ProvenanceRequirement::CaptureExecution,
        };
        let policy = |_controller_execution| ParsedCommandPolicySnapshot {
            resource_admission_evidence: ResourceAdmissionEvidence::Observed {
                pressure: ResourceHeat::Hot,
            },
            resource_policy: None,
            lab_readiness: Some(LabReadinessSnapshot {
                state: "connected_ready".into(),
                selected_runner_id: Some("lab-a".into()),
                available_runner_ids: vec!["lab-a".into()],
                reasons: Vec::new(),
                remediation_commands: Vec::new(),
                repair_admitted_runner_ids: Vec::new(),
            }),
            selected_runner_id: Some("lab-a".into()),
            generic_route: GenericRoutePolicySnapshot {
                command_supports_lab: true,
                automatic_authorized: true,
                selected_runner_id: Some("lab-a".into()),
            },
            deferred_pressure_refusal: false,
            runner_admitted: true,
            auto_local_capacity_fallback: false,
        };

        for controller_execution in [
            ControllerExecution::Ordinary,
            ControllerExecution::SplitPlacementCoordinator,
        ] {
            let result = resolve_parsed_command_preflight(
                vec!["homeboy".into()],
                input(controller_execution),
                policy(controller_execution),
            )
            .expect("ready Lab execution resolves");
            assert_eq!(
                result.resource_admission,
                ResourceAdmissionDecision::Admitted
            );
        }

        let result = resolve_parsed_command_preflight(
            vec!["homeboy".into()],
            input(ControllerExecution::ControllerOnly),
            policy(ControllerExecution::ControllerOnly),
        )
        .expect("controller-only execution resolves");
        assert!(matches!(
            result.resource_admission,
            ResourceAdmissionDecision::Rejected { .. }
        ));

        let mut local_input = input(ControllerExecution::Ordinary);
        local_input.placement = PlacementIntent::Local;
        local_input.runner = RunnerIntent::Default;
        let local = resolve_parsed_command_preflight(
            vec!["homeboy".into()],
            local_input,
            policy(ControllerExecution::Ordinary),
        )
        .expect("explicit local route resolves");
        assert_eq!(local.placement.selected, EffectiveExecutionPlacement::Local);
        assert!(matches!(
            local.resource_admission,
            ResourceAdmissionDecision::Rejected { .. }
        ));

        let mut fallback_input = input(ControllerExecution::Ordinary);
        fallback_input.placement = PlacementIntent::LabOrLocal;
        fallback_input.runner = RunnerIntent::Default;
        for (state, selected_runner_id, available_runner_ids) in [
            ("stale", Some("lab-a"), vec!["lab-a"]),
            ("connected_ready", Some("lab-b"), vec!["lab-b"]),
        ] {
            let mut blocked_policy = policy(ControllerExecution::Ordinary);
            blocked_policy.lab_readiness.as_mut().unwrap().state = state.into();
            blocked_policy
                .lab_readiness
                .as_mut()
                .unwrap()
                .selected_runner_id = selected_runner_id.map(str::to_string);
            blocked_policy
                .lab_readiness
                .as_mut()
                .unwrap()
                .available_runner_ids = available_runner_ids
                .into_iter()
                .map(str::to_string)
                .collect();
            assert!(resolve_parsed_command_preflight(
                vec!["homeboy".into()],
                fallback_input.clone(),
                blocked_policy,
            )
            .is_err());
        }
    }

    #[test]
    fn generic_input_is_a_complete_serializable_contract() {
        let input = ParsedCommandPreflightInput {
            identity: ParsedCommandIdentity {
                family: "fixture".into(),
                operation: vec!["run".into()],
            },
            resource_admission: ResourceAdmissionRequirement::Required {
                label: "fixture run".into(),
                engages_at: ResourceHeat::Warm,
            },
            controller_execution: ControllerExecution::Ordinary,
            deferred_workload: DeferredWorkloadPolicy::Eligible,
            placement: PlacementIntent::LabOrLocal,
            runner: RunnerIntent::Default,
            runner_normalization: RunnerNormalization::None,
            lab_route: LabRouteIntent::Supported { automatic: true },
            provenance: ProvenanceRequirement::CaptureExecution,
        };
        let round_trip: ParsedCommandPreflightInput =
            serde_json::from_value(serde_json::to_value(&input).expect("serialize"))
                .expect("deserialize");
        assert_eq!(round_trip, input);
    }

    #[test]
    fn placement_directive_finalizes_only_late_identity() {
        use homeboy_lab_contract::{
            EffectiveExecutionPlacement, ExecutionPlacementFallback, ExecutionPlacementIdentity,
            ExecutionPlacementOverrideAuthorization, ExecutionPlacementRequirement, Placement,
        };

        let directive = PlacementDirective {
            requested: Placement::LabOrLocal,
            required: ExecutionPlacementRequirement::Either,
            selected: EffectiveExecutionPlacement::Lab,
            runner: None,
            fallback: ExecutionPlacementFallback {
                local_allowed: true,
                reason: Some("ready runner admission".to_string()),
            },
            override_authorization: ExecutionPlacementOverrideAuthorization {
                authorized: false,
                authority: None,
            },
        };
        let decision = directive.finalize(ExecutionPlacementIdentity {
            repository: "fixture".to_string(),
            workspace: "/tmp/fixture".to_string(),
            task: "late-materialized-task".to_string(),
            candidate: Some("candidate".to_string()),
            base: Some("base".to_string()),
        });

        assert_eq!(decision.requested, directive.requested);
        assert_eq!(decision.selected, directive.selected);
        assert_eq!(decision.fallback, directive.fallback);
        assert_eq!(decision.identity.task, "late-materialized-task");
    }

    #[test]
    fn generic_resolver_requires_support_ordinary_execution_and_authorization() {
        let input = ParsedCommandPreflightInput {
            identity: ParsedCommandIdentity {
                family: "fixture".into(),
                operation: vec![],
            },
            resource_admission: ResourceAdmissionRequirement::Exempt,
            controller_execution: ControllerExecution::Ordinary,
            deferred_workload: DeferredWorkloadPolicy::Forbidden,
            placement: PlacementIntent::Auto,
            runner: RunnerIntent::Default,
            runner_normalization: RunnerNormalization::None,
            lab_route: LabRouteIntent::Supported { automatic: true },
            provenance: ProvenanceRequirement::None,
        };
        let policy = GenericRoutePolicySnapshot {
            command_supports_lab: true,
            automatic_authorized: true,
            selected_runner_id: Some("lab-a".into()),
        };
        assert_eq!(
            resolve_generic_route_runner(&input, &policy).as_deref(),
            Some("lab-a")
        );
        assert_eq!(
            resolve_generic_route_runner(
                &input,
                &GenericRoutePolicySnapshot {
                    automatic_authorized: false,
                    ..policy
                }
            ),
            None
        );
    }

    #[test]
    fn resolver_fails_closed_for_impossible_admission_and_deferred_evidence() {
        let input = ParsedCommandPreflightInput {
            identity: ParsedCommandIdentity {
                family: "fixture".into(),
                operation: vec![],
            },
            resource_admission: ResourceAdmissionRequirement::Exempt,
            controller_execution: ControllerExecution::Ordinary,
            deferred_workload: DeferredWorkloadPolicy::Forbidden,
            placement: PlacementIntent::Lab,
            runner: RunnerIntent::Default,
            runner_normalization: RunnerNormalization::None,
            lab_route: LabRouteIntent::Unsupported,
            provenance: ProvenanceRequirement::None,
        };
        let policy = ParsedCommandPolicySnapshot {
            resource_admission_evidence: ResourceAdmissionEvidence::Unavailable,
            resource_policy: None,
            lab_readiness: None,
            selected_runner_id: Some("lab-a".into()),
            generic_route: GenericRoutePolicySnapshot {
                command_supports_lab: false,
                automatic_authorized: false,
                selected_runner_id: Some("lab-a".into()),
            },
            deferred_pressure_refusal: false,
            runner_admitted: false,
            auto_local_capacity_fallback: false,
        };
        assert!(
            resolve_parsed_command_preflight(vec!["fixture".into()], input.clone(), policy)
                .is_err()
        );

        let policy = ParsedCommandPolicySnapshot {
            resource_admission_evidence: ResourceAdmissionEvidence::Unavailable,
            resource_policy: None,
            lab_readiness: None,
            selected_runner_id: None,
            generic_route: GenericRoutePolicySnapshot {
                command_supports_lab: false,
                automatic_authorized: false,
                selected_runner_id: None,
            },
            deferred_pressure_refusal: true,
            runner_admitted: false,
            auto_local_capacity_fallback: false,
        };
        assert!(resolve_parsed_command_preflight(vec!["fixture".into()], input, policy).is_err());
    }

    #[test]
    fn readiness_repair_admission_is_action_and_runner_scoped() {
        let input = ParsedCommandPreflightInput {
            identity: ParsedCommandIdentity {
                family: "extension".into(),
                operation: vec!["setup".into(), "fixture".into()],
            },
            resource_admission: ResourceAdmissionRequirement::Exempt,
            controller_execution: ControllerExecution::Ordinary,
            deferred_workload: DeferredWorkloadPolicy::Forbidden,
            placement: PlacementIntent::Auto,
            runner: RunnerIntent::ReadinessRepair("lab-a".into()),
            runner_normalization: RunnerNormalization::None,
            lab_route: LabRouteIntent::Supported { automatic: false },
            provenance: ProvenanceRequirement::CaptureExecution,
        };
        let policy = ParsedCommandPolicySnapshot {
            resource_admission_evidence: ResourceAdmissionEvidence::Unavailable,
            resource_policy: None,
            lab_readiness: Some(LabReadinessSnapshot {
                state: "capability_blocked".into(),
                selected_runner_id: Some("lab-a".into()),
                available_runner_ids: Vec::new(),
                reasons: vec!["fixture toolchain is unavailable".into()],
                remediation_commands: vec!["homeboy extension setup fixture".into()],
                repair_admitted_runner_ids: vec!["lab-a".into()],
            }),
            selected_runner_id: Some("lab-a".into()),
            generic_route: GenericRoutePolicySnapshot {
                command_supports_lab: true,
                automatic_authorized: false,
                selected_runner_id: Some("lab-a".into()),
            },
            deferred_pressure_refusal: false,
            runner_admitted: false,
            auto_local_capacity_fallback: false,
        };

        let repair = resolve_parsed_command_preflight(
            vec!["extension".into(), "setup".into(), "fixture".into()],
            input.clone(),
            policy.clone(),
        )
        .expect("the bounded setup action is admitted");
        assert_eq!(
            repair.placement.selected,
            homeboy_lab_contract::EffectiveExecutionPlacement::Lab
        );

        let mut unrelated = input;
        unrelated.runner = RunnerIntent::Explicit("lab-a".into());
        assert!(resolve_parsed_command_preflight(
            vec!["bench".into(), "run".into()],
            unrelated,
            policy
        )
        .is_err());
    }

    #[test]
    fn resolver_accepts_an_explicit_runner_from_its_authoritative_ready_snapshot() {
        let input = explicit_runner_input();
        let policy = explicit_runner_policy("connected_ready", vec!["lab-b".into()], true);

        assert!(resolve_parsed_command_preflight(vec!["fixture".into()], input, policy).is_ok());
    }

    #[test]
    fn resolver_admits_automatic_and_explicit_runners_from_fresh_bounded_evidence() {
        let policy = explicit_runner_policy("connected_ready", vec!["lab-b".into()], true);

        assert!(resolve_parsed_command_preflight(
            vec!["fixture".into()],
            explicit_runner_input(),
            policy.clone(),
        )
        .is_ok());

        let mut automatic = explicit_runner_input();
        automatic.placement = PlacementIntent::Auto;
        automatic.runner = RunnerIntent::Default;
        assert!(
            resolve_parsed_command_preflight(vec!["fixture".into()], automatic, policy).is_ok()
        );
    }

    #[test]
    fn auto_routes_to_local_when_the_preferred_lab_runner_is_inadmissible() {
        let input = auto_route_input();
        let policy = auto_route_policy(
            Some("homeboy-lab"),
            "connected_ineligible",
            Vec::new(),
            vec!["unresolved generation projection".into()],
            false,
            ResourceAdmissionEvidence::Unavailable,
        );

        let result = resolve_parsed_command_preflight(vec!["fixture".into()], input, policy)
            .expect("auto falls through to the ready local route");
        assert_eq!(
            result.placement.selected,
            homeboy_lab_contract::EffectiveExecutionPlacement::Local
        );
        assert!(result.placement.runner.is_none());
        assert!(result.selected_runner_id.is_none());
        assert!(result.generic_route_runner_id.is_none());
        assert_eq!(result.fallback, FallbackDirective::LocalAllowed);
        assert_eq!(
            result.placement.fallback.reason.as_deref(),
            Some(
                "Lab runner homeboy-lab inadmissible (unresolved generation projection); routing to local."
            )
        );
    }

    #[test]
    fn split_cook_auto_blocks_stale_lab_instead_of_previewing_local() {
        let input = split_cook_input();
        let policy = auto_route_policy(
            Some("homeboy-lab"),
            "stale",
            Vec::new(),
            vec!["runner daemon is stale".into()],
            false,
            ResourceAdmissionEvidence::Unavailable,
        );

        let error = resolve_parsed_command_preflight(vec!["homeboy".into()], input, policy)
            .expect_err("stale Cook Lab admission must block before execution");
        assert_eq!(error.code, crate::ErrorCode::RunnerLabUnavailable);
        assert_eq!(error.details["field"], "placement");
        assert_eq!(error.details["reason"], "lab_unavailable");
        assert!(error.message.contains("runner daemon is stale"));
    }

    #[test]
    fn split_cook_auto_reports_lab_blocker_without_considering_local() {
        // Cook's split route is Lab-required under Auto, so local admission is
        // never evaluated as an implicit placement transition.
        let mut input = split_cook_input();
        input.resource_admission = ResourceAdmissionRequirement::Required {
            label: "agent-task cook local resource reserve".into(),
            engages_at: ResourceHeat::Warm,
        };
        let mut policy = auto_route_policy(
            Some("homeboy-lab"),
            "connected_ineligible",
            Vec::new(),
            vec!["runner secret route is unavailable".into()],
            false,
            ResourceAdmissionEvidence::Observed {
                pressure: ResourceHeat::None,
            },
        );
        policy.lab_readiness.as_mut().unwrap().remediation_commands =
            vec!["homeboy runner connect homeboy-lab".into()];
        let error = resolve_parsed_command_preflight(vec!["homeboy".into()], input, policy)
            .expect_err("Cook must expose the Lab blocker before local workspace creation");

        assert_eq!(error.code, crate::ErrorCode::RunnerLabUnavailable);
        assert!(error.message.contains("runner secret route is unavailable"));
        assert_eq!(
            error.details["next_action"],
            "homeboy runner connect homeboy-lab"
        );
        assert!(!error.message.contains("local fallback"));
    }

    #[test]
    fn split_cook_auto_without_configured_lab_resolves_local() {
        let result = resolve_parsed_command_preflight(
            vec!["homeboy".into()],
            split_cook_input(),
            auto_route_policy(
                None,
                "absent",
                Vec::new(),
                Vec::new(),
                false,
                ResourceAdmissionEvidence::Unavailable,
            ),
        )
        .expect("a host without a Lab runner keeps Auto local");
        assert_eq!(
            result.placement.selected,
            homeboy_lab_contract::EffectiveExecutionPlacement::Local
        );
        assert_eq!(
            result.placement.fallback.reason.as_deref(),
            Some("no Lab runner is configured")
        );
    }

    #[test]
    fn split_cook_auto_selects_a_ready_lab_runner() {
        let result = resolve_parsed_command_preflight(
            vec!["homeboy".into()],
            split_cook_input(),
            auto_route_policy(
                Some("homeboy-lab"),
                "connected_ready",
                vec!["homeboy-lab".into()],
                Vec::new(),
                true,
                ResourceAdmissionEvidence::Unavailable,
            ),
        )
        .expect("ready Lab admission should be selected");

        assert_eq!(
            result.placement.selected,
            homeboy_lab_contract::EffectiveExecutionPlacement::Lab
        );
        assert_eq!(
            result.placement.required,
            ExecutionPlacementRequirement::Lab
        );
    }

    #[test]
    fn split_cook_explicit_local_stays_local_without_lab_admission() {
        let mut input = split_cook_input();
        input.placement = PlacementIntent::Local;
        let mut policy = auto_route_policy(
            None,
            "stale",
            Vec::new(),
            vec!["runner daemon is stale".into()],
            false,
            ResourceAdmissionEvidence::Unavailable,
        );
        policy.lab_readiness = None;

        let result = resolve_parsed_command_preflight(vec!["homeboy".into()], input, policy)
            .expect("explicit local placement must not inspect Lab admission");
        assert_eq!(
            result.placement.selected,
            homeboy_lab_contract::EffectiveExecutionPlacement::Local
        );
        assert!(result.placement.override_authorization.authorized);
    }

    #[test]
    fn split_cook_lab_or_local_authorizes_stale_lab_fallback() {
        let mut input = split_cook_input();
        input.placement = PlacementIntent::LabOrLocal;
        let result = resolve_parsed_command_preflight(
            vec!["homeboy".into()],
            input,
            auto_route_policy(
                Some("homeboy-lab"),
                "stale",
                Vec::new(),
                vec!["runner daemon is stale".into()],
                false,
                ResourceAdmissionEvidence::Unavailable,
            ),
        )
        .expect("lab-or-local explicitly authorizes local fallback");

        assert_eq!(
            result.placement.selected,
            homeboy_lab_contract::EffectiveExecutionPlacement::Local
        );
        assert_eq!(result.fallback, FallbackDirective::LocalAllowed);
        assert!(result.placement.fallback.local_allowed);
    }

    #[test]
    fn auto_enumerates_every_inadmissible_route_when_none_remain() {
        let mut input = auto_route_input();
        input.resource_admission = ResourceAdmissionRequirement::Required {
            label: "fixture run".into(),
            engages_at: ResourceHeat::Warm,
        };
        let policy = auto_route_policy(
            Some("homeboy-lab"),
            "connected_ineligible",
            Vec::new(),
            vec!["unresolved generation projection".into()],
            false,
            ResourceAdmissionEvidence::Observed {
                pressure: ResourceHeat::Hot,
            },
        );

        let error = resolve_parsed_command_preflight(vec!["fixture".into()], input, policy)
            .expect_err("auto fails only after every route is rejected");
        assert_eq!(error.details["field"], "placement");
        assert!(
            error
                .message
                .contains("Lab runner homeboy-lab inadmissible (unresolved generation projection)"),
            "{}",
            error.message
        );
        assert!(
            error
                .message
                .contains("local inadmissible (resource admission rejected for fixture run)"),
            "{}",
            error.message
        );
        let tried = error.details["tried"]
            .as_array()
            .expect("per-route reasons are enumerated together");
        assert_eq!(tried.len(), 2);
        assert!(tried.iter().any(|route| {
            route.as_str()
                == Some("Lab runner homeboy-lab inadmissible (unresolved generation projection)")
        }));
        assert!(tried.iter().any(|route| {
            route.as_str()
                == Some("local inadmissible (resource admission rejected for fixture run)")
        }));
    }

    #[test]
    fn resolver_rejects_an_explicit_runner_without_ready_inventory_evidence() {
        let input = explicit_runner_input();
        let policy = explicit_runner_policy("stale", Vec::new(), false);

        let error = resolve_parsed_command_preflight(vec!["fixture".into()], input, policy)
            .expect_err("stale runner evidence must fail closed");
        assert_eq!(error.details["field"], "selected_runner_id");
    }

    #[test]
    fn unobserved_readiness_refusal_names_state_and_reasons_and_asks_to_wait() {
        let mut policy = explicit_runner_policy(READINESS_UNOBSERVED, Vec::new(), true);
        policy.lab_readiness.as_mut().unwrap().reasons =
            vec!["daemon_freshness_unavailable".into()];

        let error = resolve_parsed_command_preflight(Vec::new(), explicit_runner_input(), policy)
            .expect_err("unobserved readiness is not admitted");

        assert!(
            error.message.ends_with(
                "(readiness: readiness_unobserved; reasons: daemon_freshness_unavailable)"
            ),
            "{}",
            error.message
        );
        assert_eq!(error.details["readiness_state"], READINESS_UNOBSERVED);
        assert_eq!(
            error.details["lab_readiness"]["reasons"],
            serde_json::json!(["daemon_freshness_unavailable"])
        );
        assert_eq!(error.retryable, Some(true));
        assert!(readiness_unobserved_refusal(
            &serde_json::json!({"details": error.details})
        ));

        // A proven stale runner names its state but does not wait.
        let stale = resolve_parsed_command_preflight(
            Vec::new(),
            explicit_runner_input(),
            explicit_runner_policy("stale", Vec::new(), false),
        )
        .unwrap_err();
        assert!(stale.message.contains("(readiness: stale)"));
        assert!(!readiness_unobserved_refusal(
            &serde_json::json!({"details": stale.details})
        ));
    }

    #[test]
    fn resolver_does_not_admit_a_runner_for_an_unsupported_local_route() {
        let mut input = explicit_runner_input();
        input.lab_route = LabRouteIntent::Unsupported;
        input.placement = PlacementIntent::Auto;
        let mut policy = explicit_runner_policy("connected_ready", vec!["lab-b".into()], false);
        policy.lab_readiness = None;
        policy.selected_runner_id = None;
        policy.generic_route = GenericRoutePolicySnapshot {
            command_supports_lab: false,
            automatic_authorized: false,
            selected_runner_id: None,
        };

        assert!(resolve_parsed_command_preflight(vec!["fixture".into()], input, policy).is_ok());
    }

    fn auto_route_input() -> ParsedCommandPreflightInput {
        ParsedCommandPreflightInput {
            identity: ParsedCommandIdentity {
                family: "fixture".into(),
                operation: vec!["run".into()],
            },
            resource_admission: ResourceAdmissionRequirement::Exempt,
            controller_execution: ControllerExecution::Ordinary,
            deferred_workload: DeferredWorkloadPolicy::Eligible,
            placement: PlacementIntent::Auto,
            runner: RunnerIntent::Default,
            runner_normalization: RunnerNormalization::None,
            lab_route: LabRouteIntent::Supported { automatic: true },
            provenance: ProvenanceRequirement::None,
        }
    }

    fn split_cook_input() -> ParsedCommandPreflightInput {
        let mut input = auto_route_input();
        input.identity.family = "agent-task".into();
        input.identity.operation = vec!["cook".into()];
        input.controller_execution = ControllerExecution::SplitPlacementCoordinator;
        input
    }

    fn auto_route_policy(
        selected_runner_id: Option<&str>,
        state: &str,
        available_runner_ids: Vec<String>,
        reasons: Vec<String>,
        runner_admitted: bool,
        resource_admission_evidence: ResourceAdmissionEvidence,
    ) -> ParsedCommandPolicySnapshot {
        let selected_runner_id = selected_runner_id.map(str::to_string);
        ParsedCommandPolicySnapshot {
            resource_admission_evidence,
            resource_policy: None,
            lab_readiness: Some(LabReadinessSnapshot {
                state: state.into(),
                selected_runner_id: selected_runner_id.clone(),
                available_runner_ids,
                reasons,
                remediation_commands: Vec::new(),
                repair_admitted_runner_ids: Vec::new(),
            }),
            selected_runner_id: selected_runner_id.clone(),
            generic_route: GenericRoutePolicySnapshot {
                command_supports_lab: true,
                automatic_authorized: true,
                selected_runner_id,
            },
            deferred_pressure_refusal: false,
            runner_admitted,
            auto_local_capacity_fallback: false,
        }
    }

    fn explicit_runner_input() -> ParsedCommandPreflightInput {
        ParsedCommandPreflightInput {
            identity: ParsedCommandIdentity {
                family: "fixture".into(),
                operation: vec!["run".into()],
            },
            resource_admission: ResourceAdmissionRequirement::Exempt,
            controller_execution: ControllerExecution::Ordinary,
            deferred_workload: DeferredWorkloadPolicy::Eligible,
            placement: PlacementIntent::Lab,
            runner: RunnerIntent::Explicit("lab-b".into()),
            runner_normalization: RunnerNormalization::None,
            lab_route: LabRouteIntent::Supported { automatic: true },
            provenance: ProvenanceRequirement::None,
        }
    }

    #[test]
    fn automatic_route_records_identity_mismatch_and_recovery() {
        let mut policy = auto_route_policy(
            None,
            "stale",
            Vec::new(),
            vec!["daemon de88dcd != controller 20f3a44".into()],
            false,
            ResourceAdmissionEvidence::Unavailable,
        );
        policy.lab_readiness.as_mut().unwrap().remediation_commands =
            vec!["homeboy runner refresh lab-b".into()];
        let result = resolve_parsed_command_preflight(Vec::new(), auto_route_input(), policy)
            .expect("automatic route can fall back");
        assert_eq!(
            result.placement.selected,
            EffectiveExecutionPlacement::Local
        );
        let reason = result.placement.fallback.reason.unwrap();
        assert!(reason.contains("de88dcd"));
        assert!(reason.contains("20f3a44"));
        assert!(reason.contains("homeboy runner refresh lab-b"));
    }

    #[test]
    fn explicit_cook_route_refuses_identity_mismatch_before_execution() {
        let mut input = explicit_runner_input();
        input.identity.family = "agent-task".into();
        input.identity.operation = vec!["cook".into()];
        input.controller_execution = ControllerExecution::SplitPlacementCoordinator;
        let mut policy = explicit_runner_policy("stale", Vec::new(), false);
        let readiness = policy.lab_readiness.as_mut().unwrap();
        readiness.reasons = vec![
            "active_daemon_control_plane_build_identity != job_command_binary_build_identity: daemon de88dcd, controller 20f3a44".into(),
        ];
        readiness.remediation_commands = vec!["homeboy runner refresh lab-b".into()];

        // Preflight is pure: refusal precedes the execution layer that creates
        // worktrees and durable Cook records.
        let error = resolve_parsed_command_preflight(Vec::new(), input, policy)
            .expect_err("incompatible runner must not reach Cook execution");
        assert_eq!(error.code, crate::ErrorCode::ValidationInvalidArgument);
        let evidence = format!("{error:?}");
        assert!(evidence.contains("de88dcd"));
        assert!(evidence.contains("20f3a44"));
        assert!(evidence.contains("homeboy runner refresh lab-b"));
    }

    #[test]
    fn auto_cook_capacity_refusal_precedes_controller_provider_readiness() {
        for reason in ["pressure", "capacity_reached"] {
            for runner in [None, Some("lab-b")] {
                for preview in [false, true] {
                    let policy = auto_route_policy(
                        runner,
                        "capacity_blocked",
                        Vec::new(),
                        vec![reason.into()],
                        false,
                        ResourceAdmissionEvidence::Observed {
                            pressure: ResourceHeat::None,
                        },
                    );
                    let mut args = vec!["agent-task".into(), "cook".into()];
                    if preview {
                        args.push("--preview".into());
                    }
                    let error = resolve_parsed_command_preflight(args, split_cook_input(), policy)
                        .expect_err("busy Lab must not resolve to controller provider readiness");
                    assert_eq!(error.code, crate::ErrorCode::RunnerLabUnavailable);
                    assert_eq!(error.details["reason"], "capacity_blocked");
                    assert_eq!(error.details["readiness_state"], "capacity_blocked");
                    assert_eq!(
                        error.details["readiness_reasons"],
                        serde_json::json!([reason])
                    );
                    assert_eq!(error.details["retry_after_seconds"], 60);
                    assert_eq!(error.details["run_created"], false);
                    assert_eq!(error.retryable, Some(true));
                    assert!(error
                        .message
                        .contains(&format!("capacity_blocked: {reason}")));
                    assert!(!format!("{error:?}").contains("provider_dispatchability"));
                }
            }
        }
    }

    #[test]
    fn local_cook_ignores_lab_capacity_refusal() {
        let mut input = split_cook_input();
        input.placement = PlacementIntent::Local;
        let policy = auto_route_policy(
            None,
            "capacity_blocked",
            Vec::new(),
            vec!["pressure".into()],
            false,
            ResourceAdmissionEvidence::Observed {
                pressure: ResourceHeat::None,
            },
        );
        let result = resolve_parsed_command_preflight(Vec::new(), input, policy).unwrap();
        assert_eq!(
            result.placement.selected,
            EffectiveExecutionPlacement::Local
        );
    }

    fn explicit_runner_policy(
        state: &str,
        available_runner_ids: Vec<String>,
        runner_admitted: bool,
    ) -> ParsedCommandPolicySnapshot {
        ParsedCommandPolicySnapshot {
            resource_admission_evidence: ResourceAdmissionEvidence::Unavailable,
            resource_policy: None,
            lab_readiness: Some(LabReadinessSnapshot {
                state: state.into(),
                selected_runner_id: Some("lab-b".into()),
                available_runner_ids,
                reasons: Vec::new(),
                remediation_commands: Vec::new(),
                repair_admitted_runner_ids: Vec::new(),
            }),
            selected_runner_id: Some("lab-b".into()),
            generic_route: GenericRoutePolicySnapshot {
                command_supports_lab: true,
                automatic_authorized: true,
                selected_runner_id: Some("lab-b".into()),
            },
            deferred_pressure_refusal: false,
            runner_admitted,
            auto_local_capacity_fallback: false,
        }
    }
}
