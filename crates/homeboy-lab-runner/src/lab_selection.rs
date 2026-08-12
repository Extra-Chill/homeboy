use std::io::Read;
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use crate::daemon_repair;
use crate::resolve_lab_runner_hint;
use homeboy_core::daemon::DaemonRepairStep;
use homeboy_core::error::{ActionSafety, ExecutableAction};
use homeboy_core::lab_contract::LabCommandPortability;
use homeboy_core::runtime_promotion::RuntimePromotionWaitEvent;
use homeboy_core::{Error, ErrorCode, Result};

use super::{
    default_lab_runner_availability, load, status, LabOffloadCommand, LabRunnerGateMode,
    RunnerActiveJobSource, RunnerAvailability, RunnerConnectReport, RunnerStatusReport,
    RunnerTunnelMode,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabRunnerSelectionSource {
    Explicit,
    Default,
}

impl LabRunnerSelectionSource {
    pub(super) fn metadata_value(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Default => "automatic",
        }
    }

    pub(super) fn gate_mode(self) -> LabRunnerGateMode {
        match self {
            Self::Explicit => LabRunnerGateMode::Explicit,
            Self::Default => LabRunnerGateMode::Automatic,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LabRunnerSelection {
    pub(super) runner_id: String,
    pub(super) source: LabRunnerSelectionSource,
    pub(super) mode: RunnerTunnelMode,
}

#[derive(Debug, Clone)]
pub(super) enum LabRunnerPreparation {
    Ready {
        connect_authority: Option<RunnerConnectReport>,
    },
    FallBackLocal {
        reason: String,
    },
}

impl PartialEq for LabRunnerPreparation {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Ready { .. }, Self::Ready { .. }) => true,
            (Self::FallBackLocal { reason: left }, Self::FallBackLocal { reason: right }) => {
                left == right
            }
            _ => false,
        }
    }
}

impl Eq for LabRunnerPreparation {}

/// A side-effect-free placement question. Wrappers call this before durable run
/// creation and setup; execution rechecks the same live admission facts.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlacementReadinessRequest {
    /// Versioned public request contract. This typed surface was never
    /// released, so v2 is the first supported external schema.
    pub schema: String,
    pub runner_id: String,
    pub allow_queue: bool,
    pub durable_workload: bool,
    /// Only compiler-recognised invocations are accepted at the public
    /// boundary. Requirements are derived below, never supplied by callers.
    pub invocation: PlacementReadinessInvocation,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlacementReadinessInvocation {
    AgentTaskCook {
        provider: String,
        source_path: String,
        /// Optional serialized durable plan. v2 callers omit this; v3 callers
        /// let mutation-free preflight compile the same runner requirements as execution.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        durable_plan: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// Controller pins are typed data, never caller-provided executable
        /// readiness probes. They let preflight report materialization drift.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        runtime_identity:
            Box<Option<homeboy_core::agent_task_config::ResolvedAgentTaskRuntimeIdentity>>,
    },
    CapabilityAudit {
        source_path: String,
        capability_id: String,
    },
}

/// The immutable admission contract shared by mutation-free preflight and the
/// execution path. Callers construct it before any setup; execution evaluates
/// it again against its fresh runner snapshot before reserving daemon capacity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabAdmissionPlan {
    /// The controller source that will be snapshotted and materialized on the
    /// runner. Keeping this in the compiled plan prevents readiness from
    /// validating a command detached from its workspace contract.
    pub source_path: std::path::PathBuf,
    pub capability: super::PreparedLabRunnerCapability,
    pub toolchain: Option<super::RunnerCapabilityPreflight>,
    /// Public preflight uses runner snapshots only and therefore reports
    /// executable extension probes as an explicit unknown/blocked condition.
    pub executable_probe_required: bool,
}

/// The sole typed route-to-command boundary for Lab admission. Public
/// placement supplies its invocation and observed provider decision; execution
/// supplies the command route selected by its normal command dispatcher.
pub enum RoutedLabAdmissionInput<'a> {
    Placement {
        invocation: &'a PlacementReadinessInvocation,
        provider_admission:
            Option<&'a homeboy_agents::agent_task_provider::AgentTaskProviderAdmissionPlan>,
    },
    Execution {
        command: &'a LabOffloadCommand,
    },
}

#[derive(Debug, Clone)]
pub struct RoutedLabAdmissionCommand {
    pub command: LabOffloadCommand,
    pub source_path: std::path::PathBuf,
}

pub fn build_routed_lab_admission_command(
    input: RoutedLabAdmissionInput<'_>,
    source_path: &std::path::Path,
) -> RoutedLabAdmissionCommand {
    let command = match input {
        RoutedLabAdmissionInput::Placement {
            invocation,
            provider_admission,
        } => {
            let (hot_label, extensions, required_capabilities) = match invocation {
                PlacementReadinessInvocation::AgentTaskCook { .. } => (
                    "agent-task cook",
                    provider_admission
                        .map(|plan| plan.required_extension_ids.clone())
                        .unwrap_or_default(),
                    vec!["extension_parity".to_string()],
                ),
                PlacementReadinessInvocation::CapabilityAudit { capability_id, .. } => {
                    ("audit capability", Vec::new(), vec![capability_id.clone()])
                }
            };
            homeboy_core::lab_routing::lab_offload_command_from_contract(
                homeboy_core::lab_contract::LabCommandContract::portable(
                    hot_label,
                    None,
                    !extensions.is_empty(),
                    &[],
                )
                .with_extra_required_capabilities(required_capabilities),
                extensions,
            )
        }
        RoutedLabAdmissionInput::Execution { command } => command.clone(),
    };
    RoutedLabAdmissionCommand {
        command,
        source_path: source_path.to_path_buf(),
    }
}

/// Compile every runner admission requirement from the trusted routed command.
/// Both execution and public preflight use this boundary so neither can omit a
/// command-prefix tool, extension probe, or opaque capability.
pub fn compile_lab_admission_plan(
    command: &LabOffloadCommand,
    source_path: &std::path::Path,
    command_prefix_required_tools: &[super::RunnerRequiredTool],
) -> Result<LabAdmissionPlan> {
    if !command.is_portable() {
        return Err(Error::validation_invalid_argument(
            "invocation",
            "placement invocation must compile to a portable Lab command",
            None,
            None,
        ));
    }
    let capability = super::prepare_lab_runner_capability(super::LabRunnerCapabilityContract {
        command: command.hot_label,
        required_tools: command_prefix_required_tools.to_vec(),
        required_capabilities: command
            .required_capabilities
            .iter()
            .map(|capability| capability.name.clone())
            .collect(),
    });
    Ok(LabAdmissionPlan {
        source_path: source_path.to_path_buf(),
        toolchain: crate::lab_capabilities::toolchain_readiness_preflight(command)?,
        capability,
        executable_probe_required: false,
    })
}

pub(crate) fn compile_execution_lab_admission_plan(
    command: &LabOffloadCommand,
    source_path: &std::path::Path,
    command_prefix_required_tools: &[super::RunnerRequiredTool],
) -> Result<LabAdmissionPlan> {
    let routed = build_routed_lab_admission_command(
        RoutedLabAdmissionInput::Execution { command },
        source_path,
    );
    compile_lab_admission_plan(
        &routed.command,
        &routed.source_path,
        command_prefix_required_tools,
    )
}

/// Add controller-owned durable task requirements to an already routed plan.
/// This is shared by public preflight and execution, so a ready snapshot cannot
/// omit a runner capability that execution will later reject.
pub fn project_durable_agent_task_capabilities(
    plan: &mut LabAdmissionPlan,
    durable_plan: Option<&homeboy_agents::agent_task_scheduler::AgentTaskPlan>,
) -> Result<()> {
    let Some(durable_plan) = durable_plan else {
        return Ok(());
    };
    for task in &durable_plan.tasks {
        let requirements = task.capability_requirements().map_err(|message| {
            Error::validation_invalid_argument(
                "capability_requirements",
                message,
                Some(task.task_id.clone()),
                Some(vec!["Use homeboy/agent-task-capability-requirements/v1 with explicit provider, runner, and attached-tool declarations.".to_string()]),
            )
        })?;
        plan.capability
            .required_capabilities
            .extend(requirements.runner);
    }
    plan.capability.required_capabilities.sort();
    plan.capability.required_capabilities.dedup();
    Ok(())
}

fn validate_placement_readiness_request(request: &PlacementReadinessRequest) -> Result<()> {
    if request.schema != "homeboy/placement-readiness/v2"
        && request.schema != "homeboy/placement-readiness/v3"
    {
        return Err(Error::validation_invalid_argument(
            "schema",
            "placement readiness accepts homeboy/placement-readiness/v2 or v3 requests",
            Some(request.schema.clone()),
            None,
        ));
    }
    let (_, _, provider, source_path_inputs) = admission_identity(request);
    if source_path_inputs.iter().any(|path| path.trim().is_empty())
        || provider
            .as_ref()
            .is_some_and(|provider| provider.trim().is_empty())
    {
        return Err(Error::validation_invalid_argument(
            "invocation",
            "placement invocation requires a non-empty provider and source path",
            None,
            None,
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlacementReadinessState {
    Ready,
    Queueable,
    Blocked,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PlacementReadinessPredicate {
    pub id: String,
    pub satisfied: bool,
}

/// The stable v1 recovery-action projection. Typed executable metadata is
/// additive so existing readers retain their `{command, requires_confirmation}`
/// contract.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct PlacementRecoveryAction {
    pub command: String,
    pub requires_confirmation: bool,
}

/// A snapshot, not a reservation. It cannot create a rig/run/runner mutation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PlacementReadiness {
    pub schema: &'static str,
    pub workload_family: String,
    pub command: String,
    pub runner_id: String,
    pub state: PlacementReadinessState,
    pub predicates: Vec<PlacementReadinessPredicate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_admission:
        Option<homeboy_agents::agent_task_provider::AgentTaskProviderAdmissionPlan>,
    /// Stable v1 typed recovery actions retained from merged #11455.
    pub recovery_actions: Vec<ExecutableAction>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub recovery_action_projections: Vec<PlacementRecoveryAction>,
    /// A complete typed input emitted by the same compiler execution uses.
    /// Passing it back to `runner preflight --request` cannot silently drop a
    /// provider, source, toolchain, or capability requirement.
    pub compiled_request: PlacementReadinessRequest,
    pub revalidate_before_execution: bool,
}

struct PlacementReadinessObservation {
    status: RunnerStatusReport,
    capacity: Option<usize>,
    mode: RunnerTunnelMode,
    capability_inventory: Option<super::RunnerCapabilityInventory>,
    provider_catalog: Option<Vec<homeboy_agents::agent_tasks::provider::AgentTaskExecutorProvider>>,
    command_prefix_required_tools: Vec<super::RunnerRequiredTool>,
}

pub fn placement_readiness(request: &PlacementReadinessRequest) -> Result<PlacementReadiness> {
    placement_readiness_with_transport(request, |request, source_path| {
        let runner = load(&request.runner_id)?;
        let status = status(&request.runner_id)?;
        let command_prefix = crate::lab_command::lab_offload_command_prefix(
            source_path,
            super::remote_runner_homeboy_path(&runner, "placement readiness")?,
        );
        Ok(PlacementReadinessObservation {
            capacity: runner.settings.concurrency_limit,
            mode: status_tunnel_mode(&status),
            capability_inventory: None,
            provider_catalog: matches!(
                request.invocation,
                PlacementReadinessInvocation::AgentTaskCook { .. }
            )
            .then(|| crate::capabilities::runner_agent_task_provider_catalog(&request.runner_id))
            .transpose()?,
            status,
            command_prefix_required_tools: command_prefix.required_tools,
        })
    })
}

/// Evaluate readiness through an injected, read-only runner observation. Tests
/// use this same public-path function rather than a helper-only projection.
fn placement_readiness_with_transport(
    request: &PlacementReadinessRequest,
    observe: impl FnOnce(
        &PlacementReadinessRequest,
        &std::path::Path,
    ) -> Result<PlacementReadinessObservation>,
) -> Result<PlacementReadiness> {
    validate_placement_readiness_request(request)?;
    let (_, _, _, source_path_inputs) = admission_identity(request);
    let source_path = std::path::Path::new(
        source_path_inputs
            .first()
            .expect("validated invocation has one source path"),
    );
    let observation = observe(request, source_path)?;
    let provider_admission =
        provider_admission_for_request(request, observation.provider_catalog.as_deref());
    let routed = build_routed_lab_admission_command(
        RoutedLabAdmissionInput::Placement {
            invocation: &request.invocation,
            provider_admission: provider_admission.as_ref(),
        },
        source_path,
    );
    let mut plan = compile_lab_admission_plan(
        &routed.command,
        &routed.source_path,
        &observation.command_prefix_required_tools,
    )?;
    let durable_plan = match &request.invocation {
        PlacementReadinessInvocation::AgentTaskCook { durable_plan, .. } => durable_plan
            .as_ref()
            .map(|value| serde_json::from_value(value.clone()))
            .transpose()
            .map_err(|error| {
                Error::validation_invalid_argument(
                    "durable_plan",
                    format!("invalid durable agent-task plan: {error}"),
                    None,
                    None,
                )
            })?,
        PlacementReadinessInvocation::CapabilityAudit { .. } => None,
    };
    project_durable_agent_task_capabilities(&mut plan, durable_plan.as_ref())?;
    // Public preflight is mutation-free. Execution evaluates the same probes
    // after its workspace and runtime materialization have completed.
    plan.executable_probe_required = plan.toolchain.is_some();
    plan.toolchain = None;
    if plan.executable_probe_required {
        return Err(Error::validation_invalid_argument(
            "invocation",
            "placement readiness is blocked: extension executable probe required; upgrade the extension to advertise runner capabilities or run execution admission",
            None,
            None,
        ));
    }
    let capability = match observation.capability_inventory {
        Some(inventory) => super::evaluate_lab_runner_capabilities_for_inventory(
            &request.runner_id,
            &plan.capability,
            &inventory,
            super::LabRunnerGateMode::Explicit,
        ),
        None => super::evaluate_lab_runner_capabilities_for_runner(
            &load(&request.runner_id)?,
            &plan.capability,
            super::LabRunnerGateMode::Explicit,
        )?,
    };
    Ok(placement_readiness_from_status_with_catalog(
        request,
        &observation.status,
        observation.capacity,
        observation.mode,
        capability,
        observation.provider_catalog.as_deref(),
    ))
}

fn placement_readiness_from_status(
    request: &PlacementReadinessRequest,
    status: &RunnerStatusReport,
    capacity: Option<usize>,
    mode: RunnerTunnelMode,
    capability: super::LabRunnerGateDecision,
) -> PlacementReadiness {
    placement_readiness_from_status_with_catalog(request, status, capacity, mode, capability, None)
}

fn placement_readiness_from_status_with_catalog(
    request: &PlacementReadinessRequest,
    status: &RunnerStatusReport,
    capacity: Option<usize>,
    mode: RunnerTunnelMode,
    capability: super::LabRunnerGateDecision,
    provider_catalog: Option<&[homeboy_agents::agent_tasks::provider::AgentTaskExecutorProvider]>,
) -> PlacementReadiness {
    let (workload_family, command, provider, source_path_inputs) = admission_identity(request);
    let provider_admission = provider_admission_for_request(request, provider_catalog);
    let availability = RunnerAvailability::from_status_parts(
        request.runner_id.clone(),
        status.connected,
        status.admission_blocking_stale_daemon().is_some(),
        status.active_jobs.len(),
        &status.active_job_state,
        capacity,
    );
    let queueable = request.allow_queue
        && request.durable_workload
        && mode == RunnerTunnelMode::Reverse
        && availability.is_capacity_exhausted();
    let compatible = matches!(capability, super::LabRunnerGateDecision::Eligible);
    let inputs_declared = source_path_inputs
        .iter()
        .all(|path| !path.trim().is_empty())
        && provider
            .as_ref()
            .is_none_or(|provider| !provider.trim().is_empty());
    let provider_ready = provider_admission
        .as_ref()
        .is_none_or(|plan| plan.is_ready());
    let state = if availability.accepts_jobs && compatible && inputs_declared && provider_ready {
        PlacementReadinessState::Ready
    } else if queueable && compatible && inputs_declared && provider_ready {
        PlacementReadinessState::Queueable
    } else {
        PlacementReadinessState::Blocked
    };
    let recovery_actions = if !compatible {
        match capability {
            super::LabRunnerGateDecision::Missing { remediation, .. } => remediation
                .into_iter()
                .enumerate()
                .map(|(index, command)| {
                    ExecutableAction::new(
                        format!("runner.capability.remediation.{index}"),
                        "Inspect runner capability remediation",
                        "homeboy",
                        ["runner", "doctor", &request.runner_id],
                        ActionSafety::ReadOnly,
                    )
                    .with_evidence(serde_json::json!({ "remediation": command }))
                })
                .collect(),
            super::LabRunnerGateDecision::Eligible => Vec::new(),
        }
    } else if availability.accepts_jobs || queueable {
        Vec::new()
    } else if let Some(action) = status.admission_action() {
        vec![action]
    } else {
        vec![ExecutableAction::new(
            "runner.status",
            "Inspect runner status",
            "homeboy",
            ["runner", "status", &request.runner_id, "--full"],
            ActionSafety::ReadOnly,
        )]
    };
    let recovery_action_projections = recovery_actions
        .iter()
        .map(|action| PlacementRecoveryAction {
            command: action.render_command(),
            requires_confirmation: !action.required_confirmations.is_empty()
                || !matches!(action.safety, ActionSafety::ReadOnly),
        })
        .collect();
    PlacementReadiness {
        schema: "homeboy/placement-readiness/v2",
        workload_family,
        command,
        runner_id: request.runner_id.clone(),
        state,
        predicates: vec![
            PlacementReadinessPredicate {
                id: "runner_connected".to_string(),
                satisfied: availability.connected,
            },
            PlacementReadinessPredicate {
                id: "daemon_fresh".to_string(),
                satisfied: status.admission_blocking_stale_daemon().is_none(),
            },
            PlacementReadinessPredicate {
                id: "active_jobs_authoritative".to_string(),
                satisfied: matches!(
                    status.active_job_state,
                    super::RunnerActiveJobState::Available
                ),
            },
            PlacementReadinessPredicate {
                id: "capacity_available".to_string(),
                satisfied: !availability
                    .reasons
                    .iter()
                    .any(|reason| reason == "capacity_reached"),
            },
            PlacementReadinessPredicate {
                id: "durable_reverse_queue".to_string(),
                satisfied: queueable,
            },
            PlacementReadinessPredicate {
                id: "required_capabilities".to_string(),
                satisfied: compatible,
            },
            PlacementReadinessPredicate {
                id: "source_path_inputs_declared".to_string(),
                satisfied: source_path_inputs
                    .iter()
                    .all(|path| !path.trim().is_empty()),
            },
            PlacementReadinessPredicate {
                id: "provider_declared".to_string(),
                satisfied: provider
                    .as_ref()
                    .is_none_or(|provider| !provider.trim().is_empty()),
            },
        ]
        .into_iter()
        .chain(
            provider_admission
                .iter()
                .flat_map(|plan| plan.predicates.iter())
                .map(|predicate| PlacementReadinessPredicate {
                    id: predicate.id.clone(),
                    satisfied: predicate.satisfied,
                }),
        )
        .collect(),
        provider_admission,
        recovery_actions,
        recovery_action_projections,
        compiled_request: request.clone(),
        revalidate_before_execution: true,
    }
}

fn admission_identity(
    request: &PlacementReadinessRequest,
) -> (String, String, Option<String>, Vec<String>) {
    match &request.invocation {
        PlacementReadinessInvocation::AgentTaskCook {
            provider,
            source_path,
            ..
        } => (
            "agent-task".to_string(),
            "agent-task cook".to_string(),
            Some(provider.clone()),
            vec![source_path.clone()],
        ),
        PlacementReadinessInvocation::CapabilityAudit { source_path, .. } => (
            "audit".to_string(),
            "audit capability".to_string(),
            None,
            vec![source_path.clone()],
        ),
    }
}

fn provider_admission_for_request(
    request: &PlacementReadinessRequest,
    provider_catalog: Option<&[homeboy_agents::agent_tasks::provider::AgentTaskExecutorProvider]>,
) -> Option<homeboy_agents::agent_task_provider::AgentTaskProviderAdmissionPlan> {
    let PlacementReadinessInvocation::AgentTaskCook {
        provider,
        selector,
        model,
        runtime_identity,
        ..
    } = &request.invocation
    else {
        return None;
    };
    Some(match provider_catalog {
        Some(catalog) => {
            homeboy_agents::agent_task_provider::AgentTaskProviderAdmissionPlan::compile(
                homeboy_agents::agent_task_provider::AgentTaskProviderAdmissionRequest {
                    backend: provider.clone(),
                    selector: selector.clone(),
                    model: model.clone(),
                    runtime_identity: runtime_identity.as_ref().clone(),
                },
                catalog,
            )
        }
        None => {
            homeboy_agents::agent_task_provider::AgentTaskProviderAdmissionPlan::compile_unobserved(
                homeboy_agents::agent_task_provider::AgentTaskProviderAdmissionRequest {
                    backend: provider.clone(),
                    selector: selector.clone(),
                    model: model.clone(),
                    runtime_identity: runtime_identity.as_ref().clone(),
                },
            )
        }
    })
}

static HANDOFF_CONNECT_LOCKS: OnceLock<Mutex<std::collections::BTreeMap<String, Arc<Mutex<()>>>>> =
    OnceLock::new();
const HANDOFF_CONNECT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const HANDOFF_CONNECT_STATUS_CONVERGENCE_TIMEOUT: Duration = Duration::from_millis(500);
// A reconnect owner and its contending handoff share this bounded window. The
// owner's short automatic-connect timeout remains separate from admission.
const HANDOFF_ADMISSION_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) fn prepare_lab_runner_for_offload(
    selection: &LabRunnerSelection,
) -> Result<LabRunnerPreparation> {
    let runner = load(&selection.runner_id)?;
    if runner.kind != super::RunnerKind::Ssh {
        return Err(Error::validation_invalid_argument(
            "runner",
            "Lab offload requires a remote direct SSH or reverse-connected runner; local runners would execute on this machine",
            Some(runner.id),
            Some(vec![
                "Register a direct SSH runner or configure a reverse-connected runner before using Lab offload.".to_string(),
            ]),
        ));
    }

    prepare_lab_runner_for_offload_with(selection, status, |runner_id| {
        connect_runner_for_offload(runner_id, selection.source)
    })
}

/// Prepare an explicitly selected runner before a controller-owned cook pins
/// its runtime generation. This reuses the normal Lab readiness policy,
/// including daemon freshness repair and connection ownership protections.
pub fn prepare_explicit_lab_runner_for_offload(runner_id: &str) -> Result<()> {
    let selection = LabRunnerSelection {
        runner_id: runner_id.to_string(),
        source: LabRunnerSelectionSource::Explicit,
        mode: runner_status_tunnel_mode(runner_id),
    };
    match prepare_lab_runner_for_offload(&selection)? {
        LabRunnerPreparation::Ready { .. } => Ok(()),
        LabRunnerPreparation::FallBackLocal { reason } => Err(Error::internal_unexpected(format!(
            "explicit Lab runner preparation unexpectedly requested local fallback: {reason}"
        ))),
    }
}

pub(super) fn preflight_lab_runner_availability(
    command: &LabOffloadCommand,
    selection: &LabRunnerSelection,
    detach_after_handoff: bool,
    has_durable_agent_task_plan: bool,
    connect_authority: Option<&RunnerConnectReport>,
) -> Result<RunnerStatusReport> {
    let (availability, status) =
        preflight_lab_runner_availability_with(selection, status, connect_authority)?;
    if availability.accepts_jobs {
        return Ok(status);
    }
    if allows_detached_reverse_capacity_queue(
        detach_after_handoff,
        has_durable_agent_task_plan,
        selection,
        &availability,
    ) {
        return Ok(status);
    }

    let eligible = if matches!(selection.source, LabRunnerSelectionSource::Default) {
        default_lab_runner_availability().unwrap_or_else(|_| vec![availability.clone()])
    } else {
        vec![availability.clone()]
    };
    Err(lab_runner_availability_error(
        command.hot_label,
        Some(&availability),
        Some(&status),
        eligible,
    ))
}

/// Capacity admission is only valid for a detached, durable reverse-broker
/// handoff. Every other availability failure remains a preflight failure.
pub(super) fn allows_detached_reverse_capacity_queue(
    detach_after_handoff: bool,
    has_durable_agent_task_plan: bool,
    selection: &LabRunnerSelection,
    availability: &RunnerAvailability,
) -> bool {
    detach_after_handoff
        && has_durable_agent_task_plan
        && selection.mode == RunnerTunnelMode::Reverse
        && availability.is_capacity_exhausted()
}

fn preflight_lab_runner_availability_with(
    selection: &LabRunnerSelection,
    status_fn: impl Fn(&str) -> Result<RunnerStatusReport>,
    connect_authority: Option<&RunnerConnectReport>,
) -> Result<(RunnerAvailability, RunnerStatusReport)> {
    let capacity = load(&selection.runner_id)?.settings.concurrency_limit;
    preflight_lab_runner_availability_from_status(selection, status_fn, capacity, connect_authority)
}

pub(super) fn preflight_lab_runner_availability_from_status(
    selection: &LabRunnerSelection,
    status_fn: impl Fn(&str) -> Result<RunnerStatusReport>,
    capacity: Option<usize>,
    connect_authority: Option<&RunnerConnectReport>,
) -> Result<(RunnerAvailability, RunnerStatusReport)> {
    let status =
        authoritative_status_for_preflight(status_fn(&selection.runner_id)?, connect_authority)?;
    let availability = RunnerAvailability::from_status_parts(
        selection.runner_id.clone(),
        status.connected,
        status.admission_blocking_stale_daemon().is_some(),
        status.active_jobs.len(),
        &status.active_job_state,
        capacity,
    );
    Ok((availability, status))
}

pub(super) fn authoritative_status_for_preflight(
    mut status: RunnerStatusReport,
    connect_authority: Option<&RunnerConnectReport>,
) -> Result<RunnerStatusReport> {
    let Some(connect_authority) = connect_authority else {
        return Ok(status);
    };
    let session = status.session.as_ref().ok_or_else(|| {
        Error::internal_unexpected(
            "runner connect succeeded but the subsequent status omitted its session",
        )
    })?;
    let same_endpoint = session.local_url == connect_authority.local_url
        && session.tunnel_pid == connect_authority.tunnel_pid
        && session.remote_daemon_pid == connect_authority.remote_daemon_pid;
    let daemon_is_fresh = status.daemon_freshness.as_ref().is_some_and(|freshness| {
        freshness.fresh
            && freshness.pid == connect_authority.remote_daemon_pid
            && freshness.lease_id == session.remote_daemon_lease_id
            && status.admission_blocking_stale_daemon().is_none()
    });
    if !connect_authority.connected
        || session.mode != RunnerTunnelMode::DirectSsh
        || !same_endpoint
        || !daemon_is_fresh
    {
        return Err(Error::validation_invalid_argument(
            "runner",
            "runner connect evidence did not converge to a matching fresh daemon and available job admission",
            Some(connect_authority.runner_id.clone()),
            None,
        ));
    }
    let local_url = verified_loopback_local_url(session, connect_authority)?;
    let health =
        crate::connection::probe_verified_direct_daemon_health(&local_url).map_err(|error| {
            Error::validation_invalid_argument(
                "runner",
                format!("verified runner daemon health probe failed: {error}"),
                Some(connect_authority.runner_id.clone()),
                None,
            )
        })?;
    let health_identity_matches = health.build_identity.as_deref().is_none_or(|identity| {
        [
            session.homeboy_build_identity.as_deref(),
            connect_authority.homeboy_build_identity.as_deref(),
            status
                .daemon_freshness
                .as_ref()
                .and_then(|freshness| freshness.daemon_build_identity.as_deref()),
        ]
        .into_iter()
        .flatten()
        .all(|expected| expected == identity)
    });
    if !health.freshness.fresh
        || health
            .pid
            .is_some_and(|pid| Some(pid) != connect_authority.remote_daemon_pid)
        || health
            .freshness
            .lease_id
            .as_deref()
            .is_some_and(|lease_id| Some(lease_id) != session.remote_daemon_lease_id.as_deref())
        || !health_identity_matches
    {
        return Err(Error::validation_invalid_argument(
            "runner",
            "verified runner daemon health did not match the connected session",
            Some(connect_authority.runner_id.clone()),
            None,
        ));
    }
    let (active_jobs, stale_jobs) = crate::connection::probe_verified_direct_daemon_jobs(
        &connect_authority.runner_id,
        session,
    )?;
    let daemon_active_jobs = status
        .daemon_freshness
        .as_ref()
        .map(|freshness| freshness.active_jobs)
        .expect("fresh daemon evidence was required above");
    if daemon_active_jobs != active_jobs.len() {
        return Err(Error::validation_invalid_argument(
            "runner",
            format!(
                "verified daemon reports {daemon_active_jobs} active job(s), but typed /jobs exposed {}",
                active_jobs.len()
            ),
            Some(connect_authority.runner_id.clone()),
            None,
        ));
    }
    status.connected = true;
    status.state = super::RunnerSessionState::Connected;
    status.active_job_count = active_jobs.len();
    status.stale_runner_job_count = stale_jobs.len();
    status.active_runner_jobs = active_jobs.iter().map(Into::into).collect();
    status.active_jobs = active_jobs;
    status.stale_runner_jobs = stale_jobs.iter().map(Into::into).collect();
    status.active_job_state = super::RunnerActiveJobState::Available;
    status.active_job_source = Some(RunnerActiveJobSource::DirectDaemon);
    status.active_job_error = None;
    Ok(status)
}

fn verified_loopback_local_url(
    session: &super::RunnerSession,
    connect_authority: &RunnerConnectReport,
) -> Result<String> {
    let local_url = session.local_url.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "runner",
            "verified runner connect has no local daemon URL",
            Some(connect_authority.runner_id.clone()),
            None,
        )
    })?;
    let parsed = reqwest::Url::parse(local_url).map_err(|_| {
        Error::validation_invalid_argument(
            "runner",
            "verified runner connect has an invalid local daemon URL",
            Some(connect_authority.runner_id.clone()),
            None,
        )
    })?;
    let host = parsed
        .host_str()
        .and_then(|host| host.parse::<std::net::IpAddr>().ok());
    if parsed.scheme() != "http"
        || parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || !host.is_some_and(|host| host.is_loopback())
        || parsed.port_or_known_default() != session.local_port
    {
        return Err(Error::validation_invalid_argument(
            "runner",
            "verified runner connect local daemon URL is not a matching loopback endpoint",
            Some(connect_authority.runner_id.clone()),
            None,
        ));
    }
    Ok(local_url.to_string())
}

pub(super) fn fail_if_no_default_runner_accepts_jobs(command: &LabOffloadCommand) -> Result<()> {
    if !command.is_portable() || !command.routing_policy.default_lab_offload {
        return Ok(());
    }
    fail_if_no_default_runner_accepts_jobs_with(command, default_lab_runner_availability()?)
}

pub(super) fn fail_if_no_default_runner_accepts_jobs_with(
    command: &LabOffloadCommand,
    eligible: Vec<RunnerAvailability>,
) -> Result<()> {
    if eligible.is_empty()
        || eligible
            .iter()
            .any(|availability| availability.accepts_jobs)
    {
        return Ok(());
    }

    // Ordinary auto-routed portable work may remain local when every default
    // runner is full. A release gate is evidence of the configured Lab policy,
    // so it must never silently become controller-local merely because capacity
    // changed after selection.
    if !command.routing_policy.release_gate {
        return Ok(());
    }

    Err(lab_runner_availability_error(
        command.hot_label,
        None,
        None,
        eligible,
    ))
}

pub(super) fn lab_runner_availability_error(
    command_label: &str,
    selected: Option<&RunnerAvailability>,
    selected_status: Option<&RunnerStatusReport>,
    eligible: Vec<RunnerAvailability>,
) -> Error {
    let selected_runner_id = selected.map(|availability| availability.runner_id.clone());
    let reasons: Vec<String> = selected
        .map(|availability| availability.reasons.clone())
        .unwrap_or_else(|| {
            eligible
                .iter()
                .flat_map(|availability| availability.reasons.iter().cloned())
                .collect()
        });
    let message = if let Some(runner_id) = selected_runner_id.as_deref() {
        format!(
            "Lab offload selected runner `{runner_id}` for `{command_label}`, but that runner cannot accept jobs"
        )
    } else {
        format!(
            "Lab offload found eligible runners for `{command_label}`, but none can accept jobs"
        )
    };

    let stale_daemon_recovery = selected_status
        .and_then(|status| status.stale_daemon.as_ref())
        .map(|warning| warning.refresh_command.clone());
    let mut tried = vec![
        "Wait for an active Lab runner job to finish, then retry.".to_string(),
        "Choose another available runner with --runner <runner-id>.".to_string(),
        "Inspect availability with `homeboy runner status <runner-id> --json`.".to_string(),
    ];
    if let Some(recovery) = stale_daemon_recovery.as_ref() {
        tried.insert(
            0,
            format!("Refresh the connected stale runner: `{recovery}`."),
        );
    }

    Error::new(
        ErrorCode::ValidationInvalidArgument,
        format!("Invalid argument 'runner': {message}"),
        serde_json::json!({
            "field": "runner",
            "problem": message,
            "id": selected_runner_id,
            "runner_availability": {
                "selected": selected,
                "eligible": eligible,
                "reasons": reasons,
            },
            "runner_status": selected_status,
            "stale_daemon_recovery_command": stale_daemon_recovery,
            "tried": tried,
        }),
    )
}

fn connect_runner_for_offload(
    runner_id: &str,
    source: LabRunnerSelectionSource,
) -> Result<(RunnerConnectReport, i32)> {
    // Serialize with other controller processes before replacing a direct-SSH
    // session. The child `runner connect` receives this lease capability.
    let timeout = lab_connect_timeout(source);
    let lease = homeboy_core::runtime_promotion::acquire_waiting_for_compatible(
        "Lab runner handoff",
        runner_id.to_string(),
        HANDOFF_ADMISSION_TIMEOUT,
        emit_runtime_promotion_wait,
    )?;
    if let Ok(report) = status(runner_id) {
        if report.connected {
            return connected_runner_connect_report(runner_id, report);
        }
    }
    let (stdout, stderr, exit_code, timed_out) =
        run_runner_connect_command(runner_id, timeout, &lease)?;
    if !timed_out && exit_code == 0 {
        match structured_runner_connect_response(&stdout, runner_id) {
            Ok(Some(report)) => return Ok((report, 0)),
            Ok(None) => {}
            Err(reason) => {
                return Ok((
                    failed_runner_connect_report(runner_id, status(runner_id)?, reason),
                    1,
                ));
            }
        }
        if let Some(session) =
            wait_for_live_session(HANDOFF_CONNECT_STATUS_CONVERGENCE_TIMEOUT, |remaining| {
                crate::local_live_session(runner_id, remaining)
            })?
        {
            return connected_runner_connect_report_from_session(
                runner_id,
                session,
                homeboy_core::paths::runner_session_file(runner_id)?
                    .display()
                    .to_string(),
            );
        }
    }
    // The live-session wait has a deadline. Read full status once afterward so
    // an unavailable runner retains the existing detailed diagnostic.
    let status = status(runner_id)?;

    if status.connected {
        return connected_runner_connect_report(runner_id, status);
    }

    let reason = if timed_out {
        format!("runner connect timed out after {}s", timeout.as_secs())
    } else {
        let detail = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        if detail.is_empty() {
            format!("runner connect exited with code {exit_code}")
        } else {
            format!("runner connect exited with code {exit_code}: {detail}")
        }
    };

    Ok((
        failed_runner_connect_report(runner_id, status, reason),
        exit_code,
    ))
}

#[derive(serde::Deserialize)]
struct RunnerConnectCommandResult {
    schema: String,
    command: String,
    operation: Option<String>,
    success: bool,
    status: String,
    data: RunnerConnectCommandData,
}

#[derive(serde::Deserialize)]
struct RunnerConnectCommandData {
    command: String,
    id: String,
    connection: RunnerConnectCommandConnection,
}

#[derive(serde::Deserialize)]
struct RunnerConnectCommandConnection {
    action: String,
    runner_id: String,
    connected: bool,
    recorded: Option<bool>,
    local_url: Option<String>,
    broker_url: Option<String>,
    controller_id: Option<String>,
    remote_daemon_address: Option<String>,
    tunnel_pid: Option<u32>,
    remote_daemon_pid: Option<u32>,
    homeboy_version: Option<String>,
    homeboy_build_identity: Option<String>,
    session_path: Option<String>,
}

fn structured_runner_connect_response(
    stdout: &str,
    runner_id: &str,
) -> std::result::Result<Option<RunnerConnectReport>, String> {
    const LIMIT: usize = 1024;
    let stdout = stdout.trim();
    if stdout.is_empty() {
        return Ok(None);
    }
    let response: RunnerConnectCommandResult = serde_json::from_str(stdout).map_err(|error| {
        format!(
            "runner connect returned malformed structured output: {error}; stdout={}",
            bounded_connect_diagnostic(stdout, LIMIT)
        )
    })?;
    let connection = response.data.connection;
    if response.schema != "homeboy/command-result/v3"
        || !response.success
        || response.status != "succeeded"
        || response.command != "runner"
        || response.operation.as_deref() != Some("connect")
        || response.data.command != "runner.connect"
        || response.data.id != runner_id
        || connection.action != "connect"
        || connection.runner_id != runner_id
        || !connection.connected
    {
        return Err(format!(
            "runner connect structured response contradicts a successful connected result; stdout={}",
            bounded_connect_diagnostic(stdout, LIMIT)
        ));
    }
    Ok(Some(RunnerConnectReport {
        runner_id: runner_id.to_string(),
        mode: None,
        role: None,
        connected: connection.connected,
        recorded: connection.recorded,
        local_url: connection.local_url,
        broker_url: connection.broker_url,
        controller_id: connection.controller_id,
        remote_daemon_address: connection.remote_daemon_address,
        tunnel_pid: connection.tunnel_pid,
        remote_daemon_pid: connection.remote_daemon_pid,
        connection_warning: None,
        homeboy_version: connection.homeboy_version,
        homeboy_build_identity: connection.homeboy_build_identity,
        session_path: connection.session_path,
        leaseless_recovery: None,
        state_loss_recovery: None,
        leaseless_recovery_evidence: None,
        failure_kind: None,
        failure_message: None,
        failure_evidence: None,
    }))
}

fn bounded_connect_diagnostic(value: &str, limit: usize) -> String {
    let excerpt: String = value.chars().take(limit).collect();
    if excerpt.len() < value.len() {
        format!("{excerpt}...<truncated>")
    } else {
        excerpt
    }
}

fn failed_runner_connect_report(
    runner_id: &str,
    status: RunnerStatusReport,
    reason: String,
) -> RunnerConnectReport {
    RunnerConnectReport {
        runner_id: runner_id.to_string(),
        mode: None,
        role: None,
        connected: false,
        recorded: None,
        local_url: None,
        broker_url: None,
        controller_id: None,
        remote_daemon_address: None,
        tunnel_pid: None,
        remote_daemon_pid: None,
        connection_warning: None,
        homeboy_version: None,
        homeboy_build_identity: None,
        session_path: Some(status.session_path),
        leaseless_recovery: None,
        state_loss_recovery: None,
        leaseless_recovery_evidence: None,
        failure_kind: Some(super::RunnerFailureKind::SshFailure),
        failure_message: Some(reason),
        failure_evidence: None,
    }
}

#[cfg(test)]
mod runner_connect_response_tests {
    use super::structured_runner_connect_response;

    const OBSERVED_V3_CONNECT_RESPONSE: &str = r#"{
        "schema":"homeboy/command-result/v3",
        "command":"runner",
        "operation":"connect",
        "success":true,
        "exit_code":0,
        "status":"succeeded",
        "data":{
            "command":"runner.connect",
            "id":"homeboy-lab",
            "connection":{
                "action":"connect",
                "runner_id":"homeboy-lab",
                "connected":true,
                "local_url":"http://127.0.0.1:53321",
                "tunnel_pid":1604,
                "remote_daemon_pid":1467759,
                "homeboy_version":"homeboy 0.338.0+269cfe6b1198"
            }
        }
    }"#;

    #[test]
    fn accepts_the_observed_v3_successful_connected_runner_response() {
        let report =
            structured_runner_connect_response(OBSERVED_V3_CONNECT_RESPONSE, "homeboy-lab")
                .expect("valid response")
                .expect("structured response");

        assert!(report.connected);
        assert_eq!(report.local_url.as_deref(), Some("http://127.0.0.1:53321"));
        assert_eq!(report.tunnel_pid, Some(1604));
    }

    #[test]
    fn rejects_a_success_envelope_that_reports_a_disconnected_runner() {
        let response =
            OBSERVED_V3_CONNECT_RESPONSE.replace("\"connected\":true", "\"connected\":false");

        let error = structured_runner_connect_response(&response, "homeboy-lab")
            .expect_err("contradictory response is rejected");

        assert!(error.contains("contradicts a successful connected result"));
    }

    #[test]
    fn bounds_malformed_structured_response_diagnostics() {
        let response = format!("{{{}", "x".repeat(2048));

        let error = structured_runner_connect_response(&response, "homeboy-lab")
            .expect_err("malformed response is rejected");

        assert!(error.contains("malformed structured output"));
        assert!(error.contains("...<truncated>"));
    }
}

/// Emit queue admission separately from the terminal command envelope so a
/// human and a streaming caller see progress before the bounded wait ends.
pub(super) fn emit_runtime_promotion_wait(event: RuntimePromotionWaitEvent) {
    eprintln!(
        "{}",
        serde_json::to_string(&event).unwrap_or_else(|_| {
            "{\"schema\":\"homeboy/runtime-promotion-admission/v1\",\"state\":\"queued\",\"resource_class\":\"runtime_promotion\"}".to_string()
        })
    );
}

pub(super) fn wait_for_live_session<Session>(
    timeout: Duration,
    session: Session,
) -> Result<Option<super::RunnerSession>>
where
    Session: Fn(Duration) -> Result<Option<super::RunnerSession>>,
{
    wait_for_live_session_with(
        timeout,
        session,
        std::time::Instant::now,
        std::thread::sleep,
    )
}

pub(super) fn wait_for_live_session_with<Session, Now, Pause>(
    timeout: Duration,
    session: Session,
    mut now: Now,
    mut pause: Pause,
) -> Result<Option<super::RunnerSession>>
where
    Session: Fn(Duration) -> Result<Option<super::RunnerSession>>,
    Now: FnMut() -> std::time::Instant,
    Pause: FnMut(Duration),
{
    let deadline = now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(now());
        if remaining.is_zero() {
            return Ok(None);
        }
        if let Some(session) = session(remaining.min(HANDOFF_CONNECT_POLL_INTERVAL))? {
            return Ok(Some(session));
        }
        let remaining = deadline.saturating_duration_since(now());
        if remaining.is_zero() {
            return Ok(None);
        }
        pause(remaining.min(HANDOFF_CONNECT_POLL_INTERVAL));
    }
}

pub(super) fn contended_runner_unavailable_error(runner_id: &str, lease_error: Error) -> Error {
    Error::new(
        ErrorCode::ValidationInvalidArgument,
        format!(
            "Lab runner `{runner_id}` remained unavailable while another controller owned its reconnect lease: {}",
            lease_error.message
        ),
        serde_json::json!({
            "field": "runner",
            "problem": "a contended reconnect did not publish a healthy runner session before the admission deadline",
            "id": runner_id,
            "reconnect_lease": lease_error.details,
            "tried": [
                format!(
                    "Waited {}s for the reconnect owner to publish a healthy session; it did not complete.",
                    HANDOFF_ADMISSION_TIMEOUT.as_secs()
                ),
                format!("Inspect `homeboy runner status {runner_id} --json` before retrying."),
                format!("Inspect `homeboy runner doctor {runner_id}` if the daemon remains unreachable."),
            ],
        }),
    )
}

pub(super) fn wait_for_contended_runner<Session>(
    lease_error: Error,
    timeout: Duration,
    session: Session,
) -> Result<Option<super::RunnerSession>>
where
    Session: Fn(Duration) -> Result<Option<super::RunnerSession>>,
{
    if !homeboy_core::runtime_promotion::is_contention_error(&lease_error) {
        return Err(lease_error);
    }
    wait_for_live_session(timeout, session)
}

fn connected_runner_connect_report(
    runner_id: &str,
    status: RunnerStatusReport,
) -> Result<(RunnerConnectReport, i32)> {
    let session = status.session.ok_or_else(|| {
        Error::internal_unexpected("connected runner status did not include a session")
    })?;
    connected_runner_connect_report_from_session(runner_id, session, status.session_path)
}

fn connected_runner_connect_report_from_session(
    runner_id: &str,
    session: super::RunnerSession,
    session_path: String,
) -> Result<(RunnerConnectReport, i32)> {
    Ok((
        RunnerConnectReport {
            runner_id: runner_id.to_string(),
            mode: Some(session.mode),
            role: Some(session.role),
            connected: true,
            recorded: None,
            local_url: session.local_url,
            broker_url: session.broker_url,
            controller_id: session.controller_id,
            remote_daemon_address: session.remote_daemon_address,
            tunnel_pid: session.tunnel_pid,
            remote_daemon_pid: session.remote_daemon_pid,
            connection_warning: None,
            homeboy_version: Some(session.homeboy_version),
            homeboy_build_identity: session.homeboy_build_identity,
            session_path: Some(session_path),
            leaseless_recovery: None,
            state_loss_recovery: None,
            leaseless_recovery_evidence: session
                .leaseless_recovery_evidence
                .and_then(|v| serde_json::from_value(v).ok()),
            failure_kind: None,
            failure_message: None,
            failure_evidence: None,
        },
        0,
    ))
}

fn lab_connect_timeout(source: LabRunnerSelectionSource) -> Duration {
    match source {
        LabRunnerSelectionSource::Explicit => Duration::from_secs(30),
        LabRunnerSelectionSource::Default => Duration::from_secs(3),
    }
}

fn run_runner_connect_command(
    runner_id: &str,
    timeout: Duration,
    lease: &homeboy_core::runtime_promotion::RuntimePromotionLease,
) -> Result<(String, String, i32, bool)> {
    let exe = std::env::current_exe().map_err(|err| {
        Error::internal_io(err.to_string(), Some("resolve homeboy executable".into()))
    })?;
    let mut command = std::process::Command::new(exe);
    command
        .args(["runner", "connect", runner_id])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    lease.authorize_subprocess(&mut command);
    let mut child = command
        .spawn()
        .map_err(|err| Error::internal_io(err.to_string(), Some("start runner connect".into())))?;
    let deadline = std::time::Instant::now() + timeout;

    loop {
        if let Some(status) = child.try_wait().map_err(|err| {
            Error::internal_io(err.to_string(), Some("wait runner connect".into()))
        })? {
            let mut stdout = String::new();
            if let Some(mut pipe) = child.stdout.take() {
                let _ = pipe.read_to_string(&mut stdout);
            }
            let mut stderr = String::new();
            if let Some(mut pipe) = child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            return Ok((stdout, stderr, status.code().unwrap_or(-1), false));
        }

        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok((String::new(), String::new(), 124, true));
        }

        std::thread::sleep(Duration::from_millis(50));
    }
}

pub(super) fn prepare_lab_runner_for_offload_with(
    selection: &LabRunnerSelection,
    status_fn: impl Fn(&str) -> Result<RunnerStatusReport>,
    connect_fn: impl Fn(&str) -> Result<(RunnerConnectReport, i32)>,
) -> Result<LabRunnerPreparation> {
    let status = status_fn(&selection.runner_id)?;
    if status.connected {
        if let Some(reason) = connected_runner_not_ready_reason(&selection.runner_id, &status) {
            return automatic_fallback_or_explicit_error(
                selection,
                reason,
                format!(
                    "Lab offload runner `{}` is connected but is not ready for remote execution",
                    selection.runner_id
                ),
                daemon_repair_command(&selection.runner_id, &status),
                daemon_repair_action(&selection.runner_id, &status),
            );
        }
        eprintln!(
            "Lab offload: runner `{}` is connected via {} mode.",
            selection.runner_id,
            status_tunnel_mode(&status).label()
        );
        return Ok(LabRunnerPreparation::Ready {
            connect_authority: None,
        });
    }

    if status_tunnel_mode(&status) == RunnerTunnelMode::Reverse {
        let reason = format!(
            "reverse-connected runner `{}` is not currently connected",
            selection.runner_id
        );
        return automatic_fallback_or_explicit_error(
            selection,
            reason,
            format!(
                "Lab offload requires reverse runner `{}` to have an active reverse session",
                selection.runner_id
            ),
            "Start the reverse runner session on the Lab machine before using --runner."
                .to_string(),
            None,
        );
    }

    eprintln!(
        "Lab offload: direct SSH runner `{}` is not connected; attempting connection.",
        selection.runner_id
    );
    let lock = handoff_connect_lock(&selection.runner_id)?;
    let _lock = lock.lock().map_err(|_| {
        Error::internal_unexpected("Lab runner handoff connection lock was poisoned")
    })?;
    // Another concurrent handoff may have connected the runner while this one
    // waited; always re-check before creating another tunnel/session.
    if status_fn(&selection.runner_id)?.connected {
        return Ok(LabRunnerPreparation::Ready {
            connect_authority: None,
        });
    }
    let (report, _) = connect_fn(&selection.runner_id)?;
    if report.connected {
        return Ok(LabRunnerPreparation::Ready {
            connect_authority: Some(report),
        });
    }

    let reason = report
        .failure_message
        .unwrap_or_else(|| "runner connection did not become ready".to_string());

    automatic_fallback_or_explicit_error(
        selection,
        reason,
        format!(
            "Lab offload could not connect runner `{}` before execution",
            selection.runner_id
        ),
        format!(
            "Run `homeboy runner connect {}` for full diagnostics.",
            selection.runner_id
        ),
        None,
    )
}

fn handoff_connect_lock(runner_id: &str) -> Result<Arc<Mutex<()>>> {
    let locks = HANDOFF_CONNECT_LOCKS.get_or_init(|| Mutex::new(Default::default()));
    let mut locks = locks.lock().map_err(|_| {
        Error::internal_unexpected("Lab runner handoff connection lock registry was poisoned")
    })?;
    Ok(locks
        .entry(runner_id.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone())
}

fn automatic_fallback_or_explicit_error(
    selection: &LabRunnerSelection,
    reason: String,
    explicit_message: String,
    remediation: String,
    action: Option<ExecutableAction>,
) -> Result<LabRunnerPreparation> {
    match selection.source {
        LabRunnerSelectionSource::Default => Ok(LabRunnerPreparation::FallBackLocal { reason }),
        LabRunnerSelectionSource::Explicit => {
            let error = Error::validation_invalid_argument(
                "runner",
                format!("{explicit_message}: {reason}"),
                Some(selection.runner_id.clone()),
                Some(vec![
                    remediation,
                    "Use --placement local to run the command locally instead of offloading."
                        .to_string(),
                ]),
            );
            Err(action.map_or(error.clone(), |action| error.with_action(action)))
        }
    }
}

fn connected_runner_not_ready_reason(
    runner_id: &str,
    status: &RunnerStatusReport,
) -> Option<String> {
    if let Some(warning) = status.admission_blocking_stale_daemon() {
        let restart = daemon_repair_command(runner_id, status);
        if !warning.stale_runtime_paths.is_empty() || !warning.changed_runtime_paths.is_empty() {
            return Some(format!(
                "connected runner `{runner_id}` daemon runtime is stale after runner-side rebuilds or path changes; restart the active daemon with `{restart}`"
            ));
        }
        return Some(format!(
            "connected runner `{runner_id}` daemon is stale (severity={}): {}; refresh with `{restart}`",
            warning.severity, warning.message
        ));
    }

    let session = status.session.as_ref()?;
    match session.mode {
        RunnerTunnelMode::DirectSsh if session.local_url.as_deref().unwrap_or("").is_empty() => {
            Some(format!(
                "direct SSH runner `{runner_id}` has no local daemon URL; reconnect it with `homeboy runner connect {runner_id}`"
            ))
        }
        RunnerTunnelMode::Reverse if session.broker_url.as_deref().unwrap_or("").is_empty() => {
            Some(format!(
                "reverse-connected runner `{runner_id}` has no broker URL; restart the reverse runner session before retrying"
            ))
        }
        _ => None,
    }
}

/// The typed repair a caller should surface or execute for this runner's daemon.
///
/// Steps, not `&&`-joined text: a consumer that wants to run one step, name its
/// code, or attach lease/PID/endpoint values to it can do so, and prose is a
/// rendering of the same list rather than a parallel format. The generic
/// reconnect is the last resort, reached only when neither the freshness report
/// nor the stale-daemon warning knows anything specific about this daemon.
fn daemon_repair_steps(runner_id: &str, status: &RunnerStatusReport) -> Vec<DaemonRepairStep> {
    if let Some(report) = status
        .daemon_freshness
        .as_ref()
        .filter(|report| !report.repair_plan.is_empty())
    {
        return report.repair_plan.clone();
    }
    if let Some(warning) = status.stale_daemon.as_ref() {
        // The warning renders its own text from argv, so a step keeps the
        // action alongside the string instead of forcing a consumer to
        // reconstruct one from the other. A recovery with no typed form (an
        // older warning, or one whose command came from the runner rather than
        // from a builder) degrades to text and is surfaced, not executed.
        let steps: Vec<_> = warning
            .recovery_steps()
            .into_iter()
            .map(|(command, action)| match action {
                Some(action) => {
                    daemon_repair::action_step(daemon_repair::STALE_DAEMON_RECOVERY, action)
                }
                None => daemon_repair::step(daemon_repair::STALE_DAEMON_RECOVERY, command),
            })
            .collect();
        if !steps.is_empty() {
            return steps;
        }
    }
    daemon_repair::reconnect_plan(runner_id)
}

fn daemon_repair_command(runner_id: &str, status: &RunnerStatusReport) -> String {
    daemon_repair::render(&daemon_repair_steps(runner_id, status))
}

/// The one repair a caller may execute directly, when there is exactly one.
///
/// #11104: this used to rebuild a `refresh-homeboy` action from scratch and
/// then lift it only if it rendered to a byte-identical string to the plan's
/// sole command. Five producers emit five different commands, so the equality
/// guard threw away the argv the plan had already computed for four of them.
/// Now the plan carries the action, so the action is simply read off the step.
/// A multi-step plan is a plan, not one action, and still yields `None`; so
/// does a step with no typed form, and so does the read-only diagnosis, which
/// must not be presented as a repair the caller can apply.
fn daemon_repair_action(runner_id: &str, status: &RunnerStatusReport) -> Option<ExecutableAction> {
    let steps = daemon_repair_steps(runner_id, status);
    let recovery_plan: Vec<&str> = steps.iter().map(|step| step.command.as_str()).collect();
    let [step] = steps.as_slice() else {
        return None;
    };
    let action = step.action.clone()?;
    if action.safety != ActionSafety::Mutating {
        return None;
    }
    Some(
        action
            .requiring_confirmation("operator")
            .with_evidence(serde_json::json!({
                "runner_id": runner_id,
                "recovery_plan": recovery_plan,
            })),
    )
}

pub(super) fn resolve_lab_runner_selection(
    command: &LabOffloadCommand,
    explicit_runner: Option<&str>,
    placement: homeboy_lab_runner_contract::Placement,
) -> Result<Option<LabRunnerSelection>> {
    let config = homeboy_core::defaults::load_config();
    let deny_local_bench = config.bench.local_execution.is_denied();
    let release_gate_local_hot_allowed =
        homeboy_core::defaults::resolve_release_gate_local_hot_policy_from(&config).is_allowed();
    let default_runner = if explicit_runner.is_none()
        && command.is_portable()
        && (command.routing_policy.default_lab_offload || placement.requests_lab())
    {
        super::resolve_default_lab_runner()?
    } else {
        None
    };

    resolve_lab_runner_selection_from_placement(
        command,
        explicit_runner,
        placement,
        deny_local_bench,
        release_gate_local_hot_allowed,
        default_runner,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn resolve_lab_runner_selection_from_placement(
    command: &LabOffloadCommand,
    explicit_runner: Option<&str>,
    placement: homeboy_lab_runner_contract::Placement,
    deny_local_bench: bool,
    release_gate_local_hot_allowed: bool,
    default_runner: Option<String>,
) -> Result<Option<LabRunnerSelection>> {
    if let Some(runner_id) = explicit_runner {
        if let LabCommandPortability::LocalOnly(reason) = command.portability {
            let message = format!("--runner is unavailable for this hot command. {reason}");
            return Err(local_only_flag_rejection(
                "runner",
                message,
                Some(runner_id.to_string()),
                command.hot_label,
            ));
        }

        return Ok(Some(LabRunnerSelection {
            runner_id: runner_id.to_string(),
            source: LabRunnerSelectionSource::Explicit,
            mode: runner_status_tunnel_mode(runner_id),
        }));
    }

    if placement == homeboy_lab_runner_contract::Placement::Lab && !command.is_portable() {
        // Surface the command's own local-only reason rather than a generic
        // "portable commands are ..." hint. For a controller-owned coordinator
        // like cook, `--placement lab` is a contradiction the operator should
        // not have to reverse-engineer: the coordinator stays local by design
        // while its provider attempt already routes to Lab automatically (#9373).
        let reason = match command.portability {
            LabCommandPortability::LocalOnly(reason) => reason,
            LabCommandPortability::Portable => "this command runs on the controller",
        };
        let message =
            format!("--placement lab is unavailable for this local-only command. {reason}");
        return Err(local_only_flag_rejection(
            "placement",
            message,
            None,
            command.hot_label,
        ));
    }

    if !command.routing_policy.default_lab_offload && !placement.requests_lab() {
        fail_if_local_bench_denied(command, deny_local_bench)?;
        return Ok(None);
    }

    // Release-gate routing safety (#4603 / #4605): local placement for a release gate silently routes the
    // gate to the controller machine instead of the configured Lab runner,
    // producing a gate result that is not faithful to the routing policy. Fail
    // closed with a clear diagnostic unless the operator explicitly opts back
    // into local execution via config/env, in which case the override is recorded
    // by the offload metadata.
    if command.routing_policy.release_gate
        && placement == homeboy_lab_runner_contract::Placement::Local
    {
        if let Some(runner_id) = default_runner.as_ref() {
            if !release_gate_local_hot_allowed {
                return Err(release_gate_local_hot_denied_error(
                    format!(
                "Release gate `{}` cannot bypass Lab routing with --placement local while default Lab runner `{}` is configured and `/release_gate/local_hot` is `fail_closed`",
                        command.hot_label, runner_id
                    ),
                    "placement",
                ));
            }
        }
    }

    if placement == homeboy_lab_runner_contract::Placement::Local || !command.is_portable() {
        fail_if_local_bench_denied(command, deny_local_bench)?;
        return Ok(None);
    }

    if placement == homeboy_lab_runner_contract::Placement::Lab && default_runner.is_none() {
        return Err(Error::validation_invalid_argument(
            "placement",
            format!(
                "--placement lab requires an eligible Lab runner for `{}`",
                command.hot_label
            ),
            None,
            Some(vec![
                "Connect a Lab runner or use --placement local to run on the controller."
                    .to_string(),
            ]),
        ));
    }

    if default_runner.is_none() {
        fail_if_local_bench_denied(command, deny_local_bench)?;
    }

    default_runner
        .map(|runner_id| {
            Ok(LabRunnerSelection {
                mode: runner_status_tunnel_mode(&runner_id),
                runner_id,
                source: LabRunnerSelectionSource::Default,
            })
        })
        .transpose()
}

/// Build an actionable rejection when `--placement lab` or `--runner` is passed
/// to a controller-owned, local-only command.
///
/// The generic "portable commands are ..." hint left operators guessing which
/// spelling was correct (#9373). This surfaces command-specific remediation so
/// runtime behavior and guidance agree: for a cook/agent-task coordinator it
/// explains that the coordinator stays controller-owned while its provider
/// attempt is dispatched to the selected Lab runner, and names the levers that
/// select it (`--placement lab`, `--runner <runner-id>`, `fanout` for waves).
///
/// A split-placement coordinator that can serve `--placement lab` never reaches
/// here: `route_after_parse` dispatches it (or reports that no Lab runner is
/// ready) before placement resolution. What remains are the cases where the
/// coordinator genuinely cannot place a provider attempt at all — for example a
/// cook with no deterministic gate, or controller-local batch planning — so the
/// remediation must not claim Lab placement is meaningless for cook waves.
fn local_only_flag_rejection(
    field: &'static str,
    message: String,
    value: Option<String>,
    hot_label: &str,
) -> Error {
    let mut hints = Vec::new();
    if hot_label.starts_with("agent-task cook") || hot_label.starts_with("agent-task fanout") {
        hints.push(
            "The cook coordinator is controller-owned by design: only its provider attempt is placed. `--placement lab` and `--runner <runner-id>` select the Lab runner for that attempt; neither offloads the coordinator.".to_string(),
        );
        hints.push(
            "This invocation has no placeable provider attempt, so resolve the reason above first (for example, add a deterministic `--verify` gate to a cook).".to_string(),
        );
        hints.push(
            "To fan out many independent cooks in one operation, use `homeboy agent-task fanout cook-batch --run-plan --placement lab` so the batch coordinator dispatches each child cook's provider attempt to Lab.".to_string(),
        );
    } else {
        hints.push(resolve_lab_runner_hint().hint);
    }
    Error::validation_invalid_argument(field, message, value, Some(hints))
}

fn fail_if_local_bench_denied(command: &LabOffloadCommand, denied: bool) -> Result<()> {
    if !denied || command.hot_label != "bench" {
        return Ok(());
    }

    let config_path = homeboy_core::defaults::config_path()
        .unwrap_or_else(|_| "the global Homeboy config".to_string());
    Err(Error::validation_invalid_argument(
        "bench.local_execution",
        "Refusing to run `homeboy bench` locally because global config `/bench/local_execution` is `denied`",
        Some("denied".to_string()),
        Some(vec![
            "Configure `lab.preferred_runner`, or keep exactly one SSH Lab runner configured, then run `homeboy bench <component>` so Homeboy auto-routes the benchmark to Lab.".to_string(),
            "Use `--runner <runner-id>` only to override an ambiguous or non-default Lab selection.".to_string(),
            format!("Change `/bench/local_execution` in {config_path} to `allowed` before intentionally re-enabling local benchmark execution."),
        ]),
    ))
}

/// Build the fail-closed error for a release-gate routing-policy violation.
///
/// `message` is the already-formatted diagnostic. The remediation always
/// points the operator at the config/env override (the explicit operator-only
/// escape hatch) rather than a convenience CLI flag, so the bypass cannot
/// become a habit.
pub(super) fn release_gate_local_hot_denied_error(message: String, field: &str) -> Error {
    let config_path = homeboy_core::defaults::config_path()
        .unwrap_or_else(|_| "the global Homeboy config".to_string());
    let env_var = homeboy_core::defaults::RELEASE_GATE_LOCAL_HOT_ENV;
    Error::validation_invalid_argument(
        field,
        message,
        Some("fail_closed".to_string()),
        Some(vec![
            "Run the gate with --placement auto or --placement lab so the configured Lab runner routing applies.".to_string(),
            format!("Reconnect or upgrade a stale runner with `homeboy runner doctor <runner-id>` before retrying the gate."),
            format!("To intentionally run a release gate locally, set `/release_gate/local_hot` to `allowed` in {config_path} (the override is recorded in offload metadata)."),
            format!("For a single invocation, export {env_var}=allowed instead of editing config."),
        ]),
    )
}

pub(super) fn runner_status_tunnel_mode(runner_id: &str) -> RunnerTunnelMode {
    status(runner_id).map_or(RunnerTunnelMode::DirectSsh, |status| {
        status_tunnel_mode(&status)
    })
}

pub(super) fn status_tunnel_mode(status: &RunnerStatusReport) -> RunnerTunnelMode {
    status
        .session
        .as_ref()
        .map_or(RunnerTunnelMode::DirectSsh, |session| session.mode.clone())
}

#[cfg(test)]
mod daemon_repair_step_tests {
    use super::*;
    use crate::session::RunnerStaleDaemonWarning;
    use crate::RunnerActiveJobState;
    use homeboy_core::daemon::DaemonFreshnessReport;

    fn status_report(
        runner_id: &str,
        daemon_freshness: Option<DaemonFreshnessReport>,
        stale_daemon: Option<RunnerStaleDaemonWarning>,
    ) -> RunnerStatusReport {
        RunnerStatusReport {
            runner_id: runner_id.to_string(),
            connected: true,
            state: crate::RunnerSessionState::Connected,
            session: None,
            stale_daemon,
            daemon_freshness,
            active_jobs: Vec::new(),
            active_runner_jobs: Vec::new(),
            stale_runner_jobs: Vec::new(),
            active_job_count: 0,
            stale_runner_job_count: 0,
            active_job_state: RunnerActiveJobState::NotQueried,
            active_job_source: None,
            active_job_error: None,
            active_job_recovery_evidence: None,
            session_path: "/tmp/lab.json".to_string(),
        }
    }

    fn freshness_with_plan(repair_plan: Vec<DaemonRepairStep>) -> DaemonFreshnessReport {
        DaemonFreshnessReport {
            fresh: false,
            stale_reason_code: Some(homeboy_core::daemon::DaemonStaleReasonCode::PidDead),
            restartable: false,
            lease_id: Some("lease-dead".to_string()),
            pid: Some(4545),
            recovery_evidence: None,
            ownership_evidence: None,
            adoption_command: None,
            binary_hash: None,
            daemon_version: None,
            daemon_build_identity: None,
            runtime_paths: None,
            active_jobs: 1,
            termination_evidence: None,
            repair_plan,
        }
    }

    #[test]
    fn repair_steps_preserve_the_reports_structured_plan() {
        // #10302: the plan the freshness report already carries is returned as
        // typed steps, so a consumer can name a step's code or attach lease/PID
        // values to it instead of re-parsing `&&`-joined shell text.
        let report = status_report(
            "homeboy-lab",
            Some(freshness_with_plan(vec![daemon_repair::action_step(
                daemon_repair::RUNNER_ADOPT_ORPHAN_LEASE,
                daemon_repair::adopt_orphan_lease_action("homeboy-lab", "lease-dead"),
            )])),
            None,
        );

        let steps = daemon_repair_steps("homeboy-lab", &report);

        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].code, daemon_repair::RUNNER_ADOPT_ORPHAN_LEASE);
        assert_eq!(
            steps[0].command,
            "homeboy runner connect homeboy-lab --adopt-orphan-lease lease-dead --confirm-pid-dead"
        );
        assert_eq!(
            daemon_repair_command("homeboy-lab", &report),
            steps[0].command
        );
    }

    #[test]
    fn stale_daemon_recovery_commands_become_typed_steps() {
        let warning = RunnerStaleDaemonWarning::new(
            "homeboy-lab",
            "homeboy 0.218.0".to_string(),
            "homeboy 0.219.0".to_string(),
            Some("homeboy 0.218.0+old".to_string()),
            Some("homeboy 0.219.0+new".to_string()),
        );
        let expected = warning.recovery_commands.clone();
        let report = status_report("homeboy-lab", None, Some(warning));

        let steps = daemon_repair_steps("homeboy-lab", &report);

        assert_eq!(
            steps
                .iter()
                .map(|step| step.command.clone())
                .collect::<Vec<_>>(),
            expected
        );
        assert!(steps
            .iter()
            .all(|step| step.code == daemon_repair::STALE_DAEMON_RECOVERY));
    }

    #[test]
    fn generic_reconnect_fires_only_when_nothing_specific_is_known() {
        let report = status_report("homeboy-lab", Some(freshness_with_plan(Vec::new())), None);

        let steps = daemon_repair_steps("homeboy-lab", &report);

        assert_eq!(
            steps
                .iter()
                .map(|step| (step.code.as_str(), step.command.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("runner_disconnect", "homeboy runner disconnect homeboy-lab"),
                ("runner_connect", "homeboy runner connect homeboy-lab"),
            ]
        );
        // The rendered prose is the same text the old joined-string fallback
        // produced, so operator-facing messages are unchanged.
        assert_eq!(
            daemon_repair_command("homeboy-lab", &report),
            "homeboy runner disconnect homeboy-lab && homeboy runner connect homeboy-lab"
        );
    }
}

#[cfg(test)]
mod placement_rejection_tests {
    use super::*;
    use homeboy_core::lab_contract::LabCommandContract;

    fn local_only_command(hot_label: &'static str, reason: &'static str) -> LabOffloadCommand {
        LabOffloadCommand {
            command: LabCommandContract::local_only(hot_label, reason),
            required_extensions: Vec::new(),
            required_capabilities: Vec::new(),
            workload: None,
        }
    }

    fn hints(error: &Error) -> Vec<String> {
        error
            .details
            .get("tried")
            .and_then(|value| value.as_array())
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| entry.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn placement_lab_on_cook_yields_cook_aware_remediation() {
        // #9373: `--placement lab` on a cook coordinator that has no placeable
        // provider attempt must not report a generic "portable commands are ..."
        // hint, and must not claim Lab placement is meaningless for cook waves
        // (the documented spelling for a wave). It explains that placement
        // selects the runner for the attempt and names the levers.
        let command = local_only_command(
            "agent-task cook/run-plan/retry --run",
            "agent-task cook is a controller-owned coordinator.",
        );

        let error = resolve_lab_runner_selection_from_placement(
            &command,
            None,
            homeboy_lab_runner_contract::Placement::Lab,
            false,
            false,
            None,
        )
        .expect_err("--placement lab must be rejected for a local-only cook");

        assert_eq!(error.details["field"].as_str(), Some("placement"));
        assert!(
            error.details["problem"]
                .as_str()
                .is_some_and(|problem| problem.contains("controller-owned coordinator")),
            "problem must carry the command's own reason: {}",
            error.details["problem"]
        );
        let hints = hints(&error);
        assert!(
            hints
                .iter()
                .any(|hint| hint.contains("--runner <runner-id>")),
            "cook remediation must name --runner, got {hints:?}"
        );
        assert!(
            hints
                .iter()
                .any(|hint| hint.contains("select the Lab runner for that attempt")),
            "cook remediation must explain what placement selects, got {hints:?}"
        );
        assert!(
            !hints
                .iter()
                .any(|hint| hint.contains("no --placement lab needed")),
            "cook remediation must not contradict documented wave guidance, got {hints:?}"
        );
        assert!(
            hints.iter().any(|hint| hint.contains("fanout")),
            "cook remediation must point at fanout for waves, got {hints:?}"
        );
    }

    #[test]
    fn explicit_runner_on_cook_yields_cook_aware_remediation() {
        let command = local_only_command(
            "agent-task cook/run-plan/retry --run",
            "agent-task cook is a controller-owned coordinator.",
        );

        let error = resolve_lab_runner_selection_from_placement(
            &command,
            Some("homeboy-lab"),
            homeboy_lab_runner_contract::Placement::Auto,
            false,
            false,
            None,
        )
        .expect_err("--runner must be rejected for a local-only cook coordinator");

        assert_eq!(error.details["field"].as_str(), Some("runner"));
        let hints = hints(&error);
        assert!(
            hints
                .iter()
                .any(|hint| hint.contains("--runner <runner-id>")),
            "cook remediation must name --runner, got {hints:?}"
        );
    }

    #[test]
    fn placement_lab_on_non_cook_local_only_keeps_generic_hint() {
        // A non-agent-task local-only command must retain the generic
        // portable-commands hint rather than cook-specific remediation.
        let command = local_only_command("some-other-command", "this command runs locally.");

        let error = resolve_lab_runner_selection_from_placement(
            &command,
            None,
            homeboy_lab_runner_contract::Placement::Lab,
            false,
            false,
            None,
        )
        .expect_err("--placement lab must be rejected for a local-only command");

        let hints = hints(&error);
        assert!(
            !hints.iter().any(|hint| hint.contains("fanout")),
            "non-cook remediation must not mention fanout, got {hints:?}"
        );
    }
}

#[cfg(test)]
mod placement_readiness_tests {
    use super::*;
    use crate::session::RunnerStaleDaemonWarning;
    use crate::{RunnerActiveJobState, RunnerSessionState};

    fn request() -> PlacementReadinessRequest {
        PlacementReadinessRequest {
            schema: "homeboy/placement-readiness/v2".to_string(),
            runner_id: "lab".to_string(),
            allow_queue: false,
            durable_workload: false,
            invocation: PlacementReadinessInvocation::CapabilityAudit {
                source_path: "/workspace/source".to_string(),
                capability_id: "capability.alpha".to_string(),
            },
        }
    }

    fn status() -> RunnerStatusReport {
        RunnerStatusReport {
            runner_id: "lab".to_string(),
            connected: true,
            state: RunnerSessionState::Connected,
            session: None,
            stale_daemon: None,
            daemon_freshness: None,
            active_jobs: Vec::new(),
            active_runner_jobs: Vec::new(),
            stale_runner_jobs: Vec::new(),
            active_job_count: 0,
            stale_runner_job_count: 0,
            active_job_state: RunnerActiveJobState::Available,
            active_job_source: None,
            active_job_error: None,
            active_job_recovery_evidence: None,
            session_path: "/tmp/lab.json".to_string(),
        }
    }

    fn provider(id: &str) -> homeboy_agents::agent_tasks::provider::AgentTaskExecutorProvider {
        serde_json::from_value(serde_json::json!({ "id": id, "backend": "sol" })).expect("provider")
    }

    fn provider_with_runtime(
        id: &str,
        source_revision: &str,
    ) -> homeboy_agents::agent_tasks::provider::AgentTaskExecutorProvider {
        let mut provider = provider(id);
        provider.extra.insert(
            "runtime_materialization_plan".to_string(),
            serde_json::json!({ "source_revision": source_revision }),
        );
        provider
    }

    fn cook_request(selector: Option<&str>) -> PlacementReadinessRequest {
        PlacementReadinessRequest {
            schema: "homeboy/placement-readiness/v2".to_string(),
            runner_id: "lab".to_string(),
            allow_queue: false,
            durable_workload: true,
            invocation: PlacementReadinessInvocation::AgentTaskCook {
                provider: "sol".to_string(),
                source_path: "/workspace/source".to_string(),
                durable_plan: None,
                selector: selector.map(str::to_string),
                model: Some("model".to_string()),
                runtime_identity: Box::new(None),
            },
        }
    }

    fn observed(
        catalog: Vec<homeboy_agents::agent_tasks::provider::AgentTaskExecutorProvider>,
    ) -> PlacementReadinessObservation {
        PlacementReadinessObservation {
            status: status(),
            capacity: Some(1),
            mode: RunnerTunnelMode::DirectSsh,
            capability_inventory: Some(super::super::RunnerCapabilityInventory {
                runtime_ids: std::collections::BTreeSet::from(["runner-homeboy".to_string()]),
                capabilities: std::collections::BTreeSet::from([
                    "git".to_string(),
                    "runner-homeboy".to_string(),
                    "extension_parity".to_string(),
                ]),
            }),
            provider_catalog: Some(catalog),
            command_prefix_required_tools: vec![super::super::RunnerRequiredTool::new(
                "runner-homeboy",
            )],
        }
    }

    fn decide(
        request: &PlacementReadinessRequest,
        status: &RunnerStatusReport,
        capacity: Option<usize>,
        mode: RunnerTunnelMode,
        capability: super::super::LabRunnerGateDecision,
    ) -> PlacementReadiness {
        placement_readiness_from_status(request, status, capacity, mode, capability)
    }

    #[test]
    fn ready_is_read_only_and_requires_execution_revalidation() {
        let result = decide(
            &request(),
            &status(),
            Some(1),
            RunnerTunnelMode::DirectSsh,
            super::super::LabRunnerGateDecision::Eligible,
        );
        assert_eq!(result.state, PlacementReadinessState::Ready);
        assert!(result.recovery_actions.is_empty());
        assert!(result.revalidate_before_execution);
    }

    #[test]
    fn placement_readiness_transport_selects_twelfth_catalog_provider() {
        let providers = (0..12)
            .map(|index| provider(&format!("provider-{index}")))
            .collect::<Vec<_>>();
        let request = cook_request(Some("provider-11"));
        let readiness =
            placement_readiness_with_transport(&request, |_, _| Ok(observed(providers)))
                .expect("readiness");
        assert_eq!(readiness.state, PlacementReadinessState::Ready);
        assert_eq!(
            readiness
                .provider_admission
                .expect("admission")
                .resolved_provider_id
                .as_deref(),
            Some("provider-11")
        );
    }

    #[test]
    fn placement_readiness_transport_blocks_incomplete_pinned_runtime() {
        let mut request = cook_request(Some("provider-11"));
        if let PlacementReadinessInvocation::AgentTaskCook {
            runtime_identity, ..
        } = &mut request.invocation
        {
            *runtime_identity = Box::new(Some(serde_json::from_value(serde_json::json!({
                "runtime_id": "runtime-11", "provider_id": "provider-11", "source_selector": "catalog",
                "source_revision": "abc", "freshness": "pinned", "provider": {}, "materialization_plan": {}
            })).expect("runtime pin")));
        }
        let readiness = placement_readiness_with_transport(&request, |_, _| {
            Ok(observed(vec![provider("provider-11")]))
        })
        .expect("readiness");
        assert!(!readiness.provider_admission.expect("admission").is_ready());
    }

    #[test]
    fn placement_readiness_transport_accepts_matching_materialized_pinned_runtime() {
        let mut request = cook_request(Some("provider-11"));
        if let PlacementReadinessInvocation::AgentTaskCook {
            runtime_identity, ..
        } = &mut request.invocation
        {
            *runtime_identity = Box::new(Some(serde_json::from_value(serde_json::json!({
                "runtime_id": "runtime-11", "provider_id": "provider-11", "source_selector": "catalog",
                "source_revision": "abc", "freshness": "pinned", "provider": {}, "materialization_plan": null
            }))
            .expect("runtime pin")));
        }
        let readiness = placement_readiness_with_transport(&request, |_, _| {
            Ok(observed(vec![provider_with_runtime("provider-11", "abc")]))
        })
        .expect("readiness");
        assert_eq!(readiness.state, PlacementReadinessState::Ready);
        assert!(readiness.provider_admission.expect("admission").is_ready());
    }

    #[test]
    fn placement_readiness_transport_blocks_then_allows_source_derived_tool() {
        let request = cook_request(Some("provider-0"));
        let readiness = placement_readiness_with_transport(&request, |_, _| {
            Ok(PlacementReadinessObservation {
                capability_inventory: Some(super::super::RunnerCapabilityInventory {
                    runtime_ids: std::collections::BTreeSet::new(),
                    capabilities: std::collections::BTreeSet::from([
                        "git".to_string(),
                        "extension_parity".to_string(),
                    ]),
                }),
                ..observed(vec![provider("provider-0")])
            })
        })
        .expect("readiness");
        assert_eq!(readiness.state, PlacementReadinessState::Blocked);
        assert!(readiness
            .predicates
            .iter()
            .any(|predicate| { predicate.id == "required_capabilities" && !predicate.satisfied }));
        let ready = placement_readiness_with_transport(&request, |_, _| {
            Ok(observed(vec![provider("provider-0")]))
        })
        .expect("readiness");
        assert_eq!(ready.state, PlacementReadinessState::Ready);
    }

    #[test]
    fn placement_readiness_transport_blocks_selector_mismatch() {
        let request = cook_request(Some("provider-11"));
        let readiness = placement_readiness_with_transport(&request, |_, _| {
            Ok(observed(vec![provider("provider-0")]))
        })
        .expect("readiness");
        assert_eq!(readiness.state, PlacementReadinessState::Blocked);
        assert!(!readiness.provider_admission.expect("admission").is_ready());
    }

    #[test]
    fn public_and_execution_admission_plans_are_exactly_equal() {
        let request = cook_request(Some("provider-0"));
        let catalog = vec![provider("provider-0")];
        let public =
            placement_readiness_with_transport(&request, |_, _| Ok(observed(catalog.clone())))
                .expect("public readiness");
        let provider_admission = provider_admission_for_request(&request, Some(&catalog));
        let public_route = build_routed_lab_admission_command(
            RoutedLabAdmissionInput::Placement {
                invocation: &request.invocation,
                provider_admission: provider_admission.as_ref(),
            },
            std::path::Path::new("/workspace/source"),
        );
        let execution_dispatch_contract =
            homeboy_core::lab_routing::lab_offload_command_from_contract(
                homeboy_core::lab_contract::LabCommandContract::portable(
                    "agent-task cook",
                    None,
                    false,
                    &[],
                )
                .with_extra_required_capabilities(vec!["extension_parity".to_string()]),
                Vec::new(),
            );
        let execution_route = build_routed_lab_admission_command(
            RoutedLabAdmissionInput::Execution {
                command: &execution_dispatch_contract,
            },
            std::path::Path::new("/workspace/source"),
        );
        let execution = compile_execution_lab_admission_plan(
            &execution_route.command,
            std::path::Path::new("/workspace/source"),
            &[super::super::RunnerRequiredTool::new("runner-homeboy")],
        )
        .expect("execution plan");
        let public_plan = compile_lab_admission_plan(
            &public_route.command,
            &public_route.source_path,
            &[super::super::RunnerRequiredTool::new("runner-homeboy")],
        )
        .expect("public plan");
        assert_eq!(public.compiled_request, request);
        assert_eq!(public_route.command, execution_route.command);
        assert_eq!(public_route.source_path, execution_route.source_path);
        assert_eq!(public_plan, execution);
        assert_eq!(
            execution.source_path,
            std::path::Path::new("/workspace/source")
        );
        assert_eq!(
            execution.capability.required_tools,
            vec![
                super::super::RunnerRequiredTool::git(),
                super::super::RunnerRequiredTool::new("runner-homeboy"),
            ]
        );
    }

    #[test]
    fn public_json_cannot_supply_probe_commands_or_capabilities() {
        let request = request();
        let encoded = serde_json::to_value(&request).expect("encode compiled request");
        let decoded: PlacementReadinessRequest =
            serde_json::from_value(encoded.clone()).expect("decode complete request");
        assert!(matches!(
            decoded.invocation,
            PlacementReadinessInvocation::CapabilityAudit { .. }
        ));
        let mut adversarial = encoded;
        adversarial.as_object_mut().expect("request object").insert(
            "required_toolchain_probes".to_string(),
            serde_json::json!([{ "command": "touch /tmp/pwned" }]),
        );
        assert!(serde_json::from_value::<PlacementReadinessRequest>(adversarial).is_err());
    }

    #[test]
    fn incomplete_provider_or_source_input_cannot_compile_ready() {
        let mut request = request();
        request.invocation = PlacementReadinessInvocation::AgentTaskCook {
            provider: " ".to_string(),
            source_path: " ".to_string(),
            durable_plan: None,
            selector: None,
            model: None,
            runtime_identity: Box::new(None),
        };
        assert!(validate_placement_readiness_request(&request).is_err());
    }

    #[test]
    fn stale_is_blocked_with_bounded_recovery() {
        let mut observed = status();
        observed.stale_daemon = Some(RunnerStaleDaemonWarning::new(
            "lab",
            "old".to_string(),
            "new".to_string(),
            None,
            None,
        ));
        let result = decide(
            &request(),
            &observed,
            Some(1),
            RunnerTunnelMode::DirectSsh,
            super::super::LabRunnerGateDecision::Eligible,
        );
        assert_eq!(result.state, PlacementReadinessState::Blocked);
        assert!(!result.recovery_actions.is_empty());
    }

    #[test]
    fn placement_readiness_accepts_reverse_runner_with_unavailable_verification() {
        let mut observed = status();
        observed.stale_daemon = Some(RunnerStaleDaemonWarning::verification_unavailable(
            "lab",
            "homeboy 0.0.0".to_string(),
            Some("homeboy 0.0.0+test".to_string()),
            "reverse_runner_identity_unavailable",
            "reverse runner identity cannot be verified".to_string(),
        ));

        let result = decide(
            &request(),
            &observed,
            Some(1),
            RunnerTunnelMode::Reverse,
            super::super::LabRunnerGateDecision::Eligible,
        );

        assert_eq!(result.state, PlacementReadinessState::Ready);
        assert!(result
            .predicates
            .iter()
            .any(|predicate| predicate.id == "daemon_fresh" && predicate.satisfied));
    }

    #[test]
    fn placement_readiness_rejects_reverse_runner_with_compared_mismatch() {
        let mut observed = status();
        observed.stale_daemon = Some(RunnerStaleDaemonWarning::new(
            "lab",
            "homeboy 0.0.0".to_string(),
            "homeboy 0.0.1".to_string(),
            Some("homeboy 0.0.0+old".to_string()),
            Some("homeboy 0.0.1+new".to_string()),
        ));

        let result = decide(
            &request(),
            &observed,
            Some(1),
            RunnerTunnelMode::Reverse,
            super::super::LabRunnerGateDecision::Eligible,
        );

        assert_eq!(result.state, PlacementReadinessState::Blocked);
    }

    #[test]
    fn preflight_accepts_reverse_runner_with_unavailable_verification() {
        let selection = LabRunnerSelection {
            runner_id: "lab".to_string(),
            source: LabRunnerSelectionSource::Explicit,
            mode: RunnerTunnelMode::Reverse,
        };
        let mut observed = status();
        observed.stale_daemon = Some(RunnerStaleDaemonWarning::verification_unavailable(
            "lab",
            "homeboy 0.0.0".to_string(),
            Some("homeboy 0.0.0+test".to_string()),
            "reverse_runner_identity_unavailable",
            "reverse runner identity cannot be verified".to_string(),
        ));

        let (availability, _) = preflight_lab_runner_availability_from_status(
            &selection,
            |_| Ok(observed.clone()),
            Some(1),
            None,
        )
        .expect("preflight");

        assert!(availability.accepts_jobs);
    }

    #[test]
    fn preflight_rejects_reverse_runner_with_compared_mismatch() {
        let selection = LabRunnerSelection {
            runner_id: "lab".to_string(),
            source: LabRunnerSelectionSource::Explicit,
            mode: RunnerTunnelMode::Reverse,
        };
        let mut observed = status();
        observed.stale_daemon = Some(RunnerStaleDaemonWarning::new(
            "lab",
            "homeboy 0.0.0".to_string(),
            "homeboy 0.0.1".to_string(),
            Some("homeboy 0.0.0+old".to_string()),
            Some("homeboy 0.0.1+new".to_string()),
        ));

        let (availability, _) = preflight_lab_runner_availability_from_status(
            &selection,
            |_| Ok(observed.clone()),
            Some(1),
            None,
        )
        .expect("preflight");

        assert!(!availability.accepts_jobs);
        assert_eq!(availability.reasons, ["stale_daemon"]);
    }

    #[test]
    fn busy_reverse_runner_is_queueable_only_for_durable_work() {
        let mut observed = status();
        observed.active_jobs.push(
            serde_json::from_value(serde_json::json!({
                "runner_id":"lab", "job_id":"00000000-0000-0000-0000-000000000001",
                "operation":"test", "source":"daemon", "kind":"workload", "status":"running",
                "command":"test", "started_at_ms":0, "elapsed_ms":0
            }))
            .expect("job"),
        );
        let mut request = request();
        request.allow_queue = true;
        request.durable_workload = true;
        let result = decide(
            &request,
            &observed,
            Some(1),
            RunnerTunnelMode::Reverse,
            super::super::LabRunnerGateDecision::Eligible,
        );
        assert_eq!(result.state, PlacementReadinessState::Queueable);
        request.durable_workload = false;
        assert_eq!(
            decide(
                &request,
                &observed,
                Some(1),
                RunnerTunnelMode::Reverse,
                super::super::LabRunnerGateDecision::Eligible
            )
            .state,
            PlacementReadinessState::Blocked
        );
    }

    #[test]
    fn incompatible_is_blocked_with_capability_remediation() {
        let decision = super::super::LabRunnerGateDecision::Missing {
            runner_id: "lab".to_string(),
            command: "runner preflight",
            missing_tools: Vec::new(),
            reason: "missing capability.alpha".to_string(),
            remediation: vec!["configure capability.alpha".to_string()],
        };
        let result = decide(
            &request(),
            &status(),
            Some(1),
            RunnerTunnelMode::DirectSsh,
            decision,
        );
        assert_eq!(result.state, PlacementReadinessState::Blocked);
        assert_eq!(
            result.recovery_actions[0],
            ExecutableAction::new(
                "runner.capability.remediation.0",
                "Inspect runner capability remediation",
                "homeboy",
                ["runner", "doctor", "lab"],
                ActionSafety::ReadOnly,
            )
            .with_evidence(serde_json::json!({ "remediation": "configure capability.alpha" }))
        );
        assert_eq!(
            result.recovery_actions[0].evidence,
            Some(serde_json::json!({ "remediation": "configure capability.alpha" }))
        );
    }

    #[test]
    fn incompatible_busy_reverse_runner_is_blocked_not_queueable() {
        let mut observed = status();
        observed.active_jobs.push(
            serde_json::from_value(serde_json::json!({
                "runner_id":"lab", "job_id":"00000000-0000-0000-0000-000000000001",
                "operation":"test", "source":"daemon", "kind":"workload", "status":"running",
                "command":"test", "started_at_ms":0, "elapsed_ms":0
            }))
            .expect("job"),
        );
        let mut request = request();
        request.allow_queue = true;
        request.durable_workload = true;
        let result = decide(
            &request,
            &observed,
            Some(1),
            RunnerTunnelMode::Reverse,
            super::super::LabRunnerGateDecision::Missing {
                runner_id: "lab".to_string(),
                command: "runner preflight",
                missing_tools: Vec::new(),
                reason: "missing capability.alpha".to_string(),
                remediation: vec!["configure capability.alpha".to_string()],
            },
        );
        assert_eq!(result.state, PlacementReadinessState::Blocked);
    }

    #[test]
    fn disconnected_is_blocked_without_creating_an_admission() {
        let mut observed = status();
        observed.connected = false;
        observed.state = RunnerSessionState::Disconnected;
        let result = decide(
            &request(),
            &observed,
            Some(1),
            RunnerTunnelMode::DirectSsh,
            super::super::LabRunnerGateDecision::Eligible,
        );
        assert_eq!(result.state, PlacementReadinessState::Blocked);
        assert!(result
            .predicates
            .iter()
            .any(|predicate| predicate.id == "runner_connected" && !predicate.satisfied));
        let action = &result.recovery_actions[0];
        assert_eq!(action.id, "runner.connect");
        assert_eq!(action.safety, ActionSafety::Mutating);
        assert_eq!(
            serde_json::to_value(&result).expect("serialized v1 envelope")["recovery_actions"][0]
                ["program"],
            "homeboy"
        );
    }

    #[test]
    fn v1_recovery_actions_keep_the_legacy_shape_with_additive_metadata() {
        let mut observed = status();
        observed.connected = false;
        observed.state = RunnerSessionState::Disconnected;
        let result = decide(
            &request(),
            &observed,
            Some(1),
            RunnerTunnelMode::DirectSsh,
            super::super::LabRunnerGateDecision::Eligible,
        );
        let value = serde_json::to_value(&result).expect("serialize v1 result");
        let legacy = &value["recovery_actions"][0];
        assert_eq!(legacy["program"], "homeboy");
        assert!(legacy.get("args").is_some());
        assert_eq!(
            value["recovery_action_projections"][0]["command"],
            result.recovery_actions[0].render_command()
        );
    }
}
