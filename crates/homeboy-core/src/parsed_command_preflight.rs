//! Typed, parser-independent admission and placement preflight contract.
//!
//! Command families translate parsed values into this contract above core.
//! Core deliberately does not know Clap, `Cli`, or a product command enum.

use serde::{Deserialize, Serialize};
use std::sync::{OnceLock, RwLock};

use crate::resource_policy_context::ResourcePolicyContext;
use homeboy_lab_runner_contract::{
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
    Supported { automatic: bool },
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeferredWorkloadDecision {
    NotApplicable,
    Dispatch,
    Defer,
    RunnerIncompatible,
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
    pub runner_incompatible: bool,
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
        (RunnerIntent::Explicit(_), _) => policy.selected_runner_id.clone(),
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
        && !matches!(input.lab_route, LabRouteIntent::Supported { .. })
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
    if policy.selected_runner_id.is_some()
        && (!policy.runner_admitted
            || !policy.lab_readiness.as_ref().is_some_and(|readiness| {
                readiness.state == "connected_ready"
                    && readiness.selected_runner_id == policy.selected_runner_id
                    && readiness
                        .available_runner_ids
                        .iter()
                        .any(|runner| Some(runner) == policy.selected_runner_id.as_ref())
            }))
    {
        return Err(crate::Error::validation_invalid_argument(
            "selected_runner_id",
            "selected runner requires admitted connected readiness evidence",
            policy.selected_runner_id.clone(),
            None,
        ));
    }
    if policy.runner_admitted && policy.runner_incompatible {
        return Err(crate::Error::validation_invalid_argument(
            "runner_admission",
            "runner cannot be both admitted and incompatible",
            None,
            None,
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
    let resource_admission = evaluate_resource_admission(
        &input.resource_admission,
        policy.resource_admission_evidence,
    );
    let deferred_workload = match input.deferred_workload {
        DeferredWorkloadPolicy::Forbidden => {
            if policy.deferred_pressure_refusal || policy.runner_incompatible {
                return Err(crate::Error::validation_invalid_argument(
                    "deferred_workload",
                    "deferred evidence requires an eligible deferred workload policy",
                    None,
                    None,
                ));
            }
            DeferredWorkloadDecision::NotApplicable
        }
        DeferredWorkloadPolicy::Eligible if policy.runner_admitted => {
            DeferredWorkloadDecision::Dispatch
        }
        DeferredWorkloadPolicy::Eligible if policy.runner_incompatible => {
            DeferredWorkloadDecision::RunnerIncompatible
        }
        DeferredWorkloadPolicy::Eligible if policy.deferred_pressure_refusal => {
            DeferredWorkloadDecision::Defer
        }
        DeferredWorkloadPolicy::Eligible => DeferredWorkloadDecision::NotApplicable,
    };
    let required = if matches!(input.placement, PlacementIntent::Lab)
        || matches!(input.runner, RunnerIntent::Explicit(_))
    {
        ExecutionPlacementRequirement::Lab
    } else {
        ExecutionPlacementRequirement::Either
    };
    let selected = if matches!(input.placement, PlacementIntent::Local)
        || policy.selected_runner_id.is_none()
    {
        EffectiveExecutionPlacement::Local
    } else {
        EffectiveExecutionPlacement::Lab
    };
    let runner =
        policy
            .selected_runner_id
            .as_ref()
            .map(|runner_id| ExecutionPlacementRunnerSelection {
                runner_id: runner_id.clone(),
                source: if matches!(input.runner, RunnerIntent::Explicit(_)) {
                    homeboy_lab_runner_contract::RunnerSelectionSource::Explicit
                } else {
                    homeboy_lab_runner_contract::RunnerSelectionSource::Policy
                },
            });
    let fallback = if policy.auto_local_capacity_fallback {
        FallbackDirective::LocalCapacity
    } else if matches!(input.placement, PlacementIntent::Lab) && policy.selected_runner_id.is_none()
    {
        FallbackDirective::RequiredLabUnavailable
    } else {
        FallbackDirective::None
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
            reason: None,
        },
        override_authorization: ExecutionPlacementOverrideAuthorization {
            authorized: matches!(input.placement, PlacementIntent::Local),
            authority: matches!(input.placement, PlacementIntent::Local)
                .then(|| "operator --placement local".to_string()),
        },
    };
    let generic_route_runner_id = resolve_generic_route_runner(&input, &policy.generic_route);
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
        selected_runner_id: policy.selected_runner_id,
    })
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
    let mut slot = result_storage()
        .write()
        .unwrap_or_else(|error| error.into_inner());
    if slot.is_none() {
        *slot = Some(result);
    }
}

pub fn captured_result() -> Option<ParsedCommandPreflightResult> {
    result_storage().read().ok().and_then(|slot| slot.clone())
}

#[cfg(any(test, feature = "test-support"))]
pub fn reset_captured_result_for_test() {
    if let Ok(mut slot) = result_storage().write() {
        *slot = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        use homeboy_lab_runner_contract::{
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
            placement: PlacementIntent::Auto,
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
            runner_incompatible: false,
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
            runner_incompatible: false,
            auto_local_capacity_fallback: false,
        };
        assert!(resolve_parsed_command_preflight(vec!["fixture".into()], input, policy).is_err());
    }
}
