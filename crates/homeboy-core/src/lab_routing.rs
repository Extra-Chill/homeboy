//! Core Lab routing service used by the CLI route adapter.

use std::time::Duration;

use serde_json::json;

use crate::command_execution_plan::{
    CommandSourceMaterialization, CommandSourcePolicy, CommandWorkspacePolicy, LabRoutePlan,
};
use crate::lab_contract::{
    LabCommandContract, LabCommandPortability, LabCommandRouteContract, LabSourcePathMode,
    LabWorkspaceModePolicy,
};
use crate::observation::records::RunEvidenceCommands;
use crate::observation::RunStatus;
use crate::Result;

pub const DEFAULT_LAB_DISPATCH_TIMEOUT_SECS: u64 = 9 * 60;
pub const LAB_DISPATCH_TIMEOUT_ENV: &str = "HOMEBOY_LAB_DISPATCH_TIMEOUT_SECS";
pub const LAB_TRACE_DISPATCH_TIMEOUT_ENV: &str = "HOMEBOY_LAB_TRACE_DISPATCH_TIMEOUT_SECS";

/// Phase tag used for Lab trace dispatch observation metadata payloads. Kept in
/// core so the routing service owns the observation envelope shape rather than
/// the CLI adapter.
const LAB_DISPATCH_PHASE: &str = "route_lab_dispatch";

pub struct LabRoutingRequest<'a> {
    pub command: Option<crate::lab_offload::LabOffloadCommand>,
    pub normalized_args: &'a [String],
    pub explicit_runner: Option<&'a str>,
    pub placement: homeboy_cli_contract::Placement,
    pub allow_local_fallback: bool,
    pub allow_dirty_lab_workspace: bool,
    pub skip_deps_hydration: bool,
    pub capture_patch: bool,
    pub mutation_flag: Option<&'a str>,
    pub timeout: Option<Duration>,
    pub active_run_id: Option<&'a str>,
    pub detach_after_handoff: bool,
    pub output_file_requested: bool,
    pub read_only_polling: bool,
    /// Controller-local `--output` path forwarded to the offload executor so the
    /// durable agent-task run id can be persisted before long execution (#5684).
    pub local_output_file: Option<&'a str>,
    pub durable_agent_task_plan: Option<&'a crate::agent_task_scheduler::AgentTaskPlan>,
    /// Controller checkout selected independently of CLI argv, such as the
    /// logical primary workspace of a materialized retry plan.
    pub source_path: Option<&'a std::path::Path>,
    /// Controller-derived evidence carried with a staged source snapshot. It
    /// never alters runner-side snapshot validation or workspace policy.
    pub verified_cook_baseline: Option<&'a serde_json::Value>,
    /// Require controller bundle materialization for the selected source
    /// checkout before any runner-side Git transport is attempted.
    pub require_controller_git_bundle: bool,
    pub job_overrides: crate::lab_offload::LabJobOverrides,
}

pub(crate) fn route_lab_offload(
    request: LabRoutingRequest<'_>,
) -> Result<crate::lab_offload::LabOffloadOutcome> {
    if let Some(timeout) = request.timeout {
        return execute_lab_offload_with_timeout(request, timeout);
    }

    crate::lab_offload::execute_lab_offload(request)
}

/// Retrieval guidance for a persisted Homeboy run, derived solely from a run id.
///
/// Lives in core so the Lab routing service can render run-retrieval guidance
/// into offload output without depending on CLI command modules. The CLI trace
/// adapter re-exports this type for backward compatibility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedRunRetrieval {
    pub run_id: String,
    pub commands: RunEvidenceCommands,
    pub export_command: String,
}

impl PersistedRunRetrieval {
    pub fn for_run(run_id: &str) -> Self {
        Self {
            run_id: run_id.to_string(),
            commands: RunEvidenceCommands::for_run_id(run_id),
            export_command: format!(
                "homeboy runs export --run {run_id} --output homeboy-run-{run_id}"
            ),
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        json!({
            "persisted_run_id": self.run_id,
            "id_scope": "persisted_homeboy_run",
            "retrieval_commands": {
                "evidence": self.commands.evidence_command,
                "artifacts": self.commands.artifacts_command,
                "export": self.export_command,
            }
        })
    }
}

/// Observation lifecycle for a Lab dispatch, implemented by the CLI adapter.
///
/// The core routing service owns the *control flow* (when to finish, which
/// [`RunStatus`] applies, and the metadata envelope) while the adapter owns the
/// *implementation* (persisting trace runs). This keeps observation wiring out
/// of `commands/route.rs` while avoiding a core dependency on CLI command
/// modules.
pub trait LabDispatchObserver {
    /// The run id of the active dispatch observation, if one was started.
    fn run_id(&self) -> Option<&str>;

    /// Finish the observation with the given status and metadata, returning the
    /// persisted run retrieval guidance when a run was recorded.
    fn finish(
        self: Box<Self>,
        status: RunStatus,
        metadata: serde_json::Value,
    ) -> Option<PersistedRunRetrieval>;
}

/// No-op observer used when a command does not participate in Lab dispatch
/// observation (everything except `trace`).
pub struct NoopLabDispatchObserver;

impl LabDispatchObserver for NoopLabDispatchObserver {
    fn run_id(&self) -> Option<&str> {
        None
    }

    fn finish(
        self: Box<Self>,
        _status: RunStatus,
        _metadata: serde_json::Value,
    ) -> Option<PersistedRunRetrieval> {
        None
    }
}

/// Typed output of an offloaded Lab dispatch, ready for the CLI adapter to
/// render to stdout/stderr/exit-code and optional `--output` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabRouteOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub output_file_content: Option<String>,
}

/// Typed outcome of routing a command through the Lab service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LabRouteOutcome {
    /// The command should continue executing locally on the controller.
    RunLocal,
    /// The command was handed to a runner and is still in flight.
    InFlight(LabRouteOutput),
    /// The command was offloaded to a runner; render the captured output.
    Offloaded(LabRouteOutput),
}

/// Orchestrate a Lab dispatch end-to-end: run the offload, drive the dispatch
/// observation lifecycle, and transform the runner outcome into typed output.
///
/// This is the core entrypoint that lets `commands/route.rs` stay a thin
/// adapter: it builds the [`LabRoutingRequest`], supplies a runner id and an
/// observer, and renders the returned [`LabRouteOutcome`]. All status-decision
/// and output-shaping policy lives here.
pub fn dispatch_lab_offload(
    request: LabRoutingRequest<'_>,
    runner_id: Option<&str>,
    observer: Box<dyn LabDispatchObserver>,
) -> Result<LabRouteOutcome> {
    lab_offload_outcome_to_route_outcome(route_lab_offload(request), runner_id, observer)
}

fn lab_offload_outcome_to_route_outcome(
    outcome: Result<crate::lab_offload::LabOffloadOutcome>,
    runner_id: Option<&str>,
    observer: Box<dyn LabDispatchObserver>,
) -> Result<LabRouteOutcome> {
    match outcome {
        Err(err) => {
            let _ = observer.finish(
                RunStatus::Error,
                lab_dispatch_metadata(
                    runner_id,
                    "error",
                    json!({
                        "error": {
                            "code": err.code.as_str(),
                            "message": err.message,
                            "details": err.details,
                            "hints": err.hints,
                        }
                    }),
                ),
            );
            Err(err)
        }
        Ok(crate::lab_offload::LabOffloadOutcome::RunLocal {
            metadata, messages, ..
        }) => {
            let _ = observer.finish(
                RunStatus::Skipped,
                lab_dispatch_metadata(
                    runner_id,
                    "run_local",
                    json!({
                        "metadata": metadata,
                        "messages": messages,
                    }),
                ),
            );
            for message in messages {
                eprintln!("{message}");
            }
            Ok(LabRouteOutcome::RunLocal)
        }
        Ok(crate::lab_offload::LabOffloadOutcome::Offloaded {
            stdout,
            stderr,
            exit_code,
            output_file_content,
            ..
        }) => {
            let mut finish_metadata = json!({ "exit_code": exit_code });
            attach_handoff_metadata(&mut finish_metadata, &stdout);
            let retrieval = observer.finish(
                if exit_code == 0 {
                    RunStatus::Pass
                } else {
                    RunStatus::Fail
                },
                lab_dispatch_metadata(runner_id, "offloaded_complete", finish_metadata),
            );
            let stdout = stdout_with_persisted_run_retrieval(&stdout, retrieval.as_ref());
            Ok(LabRouteOutcome::Offloaded(LabRouteOutput {
                stdout,
                stderr,
                exit_code,
                output_file_content,
            }))
        }
        Ok(crate::lab_offload::LabOffloadOutcome::InFlight {
            stdout,
            stderr,
            exit_code,
            output_file_content,
            ..
        }) => {
            let retrieval = observer.run_id().map(PersistedRunRetrieval::for_run);
            let stdout = stdout_with_in_flight_status(&stdout, retrieval.as_ref());
            let output_file_content = output_file_content
                .as_deref()
                .map(|content| stdout_with_in_flight_status(content, retrieval.as_ref()))
                .or_else(|| Some(stdout.clone()));
            Ok(LabRouteOutcome::InFlight(LabRouteOutput {
                stdout,
                stderr,
                exit_code,
                output_file_content,
            }))
        }
    }
}

fn stdout_with_in_flight_status(stdout: &str, retrieval: Option<&PersistedRunRetrieval>) -> String {
    let mut value = serde_json::from_str::<serde_json::Value>(stdout).unwrap_or_else(|_| {
        json!({
            "status": "running",
            "output": stdout,
        })
    });
    let run_id = retrieval
        .map(|retrieval| retrieval.run_id.clone())
        .or_else(|| metadata_path_string(&value, &["identity", "run_id"]))
        .or_else(|| metadata_path_string(&value, &["persisted_run_id"]))
        .or_else(|| metadata_path_string(&value, &["durable_run_id"]));
    let runner_id = metadata_path_string(&value, &["identity", "runner_id"])
        .or_else(|| metadata_path_string(&value, &["runner_id"]));
    let runner_job_id = metadata_path_string(&value, &["identity", "runner_job_id"])
        .or_else(|| metadata_path_string(&value, &["job_id"]));
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "status".to_string(),
            serde_json::Value::String("running".to_string()),
        );
        let mut in_flight = json!({
            "status": "running",
            "run_id": run_id,
            "runner_id": runner_id,
            "runner_job_id": runner_job_id,
        });
        if let Some(in_flight_object) = in_flight.as_object_mut() {
            if let Some(retrieval) = retrieval {
                in_flight_object.insert("homeboy_persisted_run".to_string(), retrieval.to_json());
                in_flight_object.insert(
                    "watch_command".to_string(),
                    serde_json::Value::String(format!("homeboy runs watch {}", retrieval.run_id)),
                );
            }
            if let (Some(runner_id), Some(runner_job_id)) = (runner_id, runner_job_id) {
                in_flight_object.insert(
                    "runner_job_watch_command".to_string(),
                    serde_json::Value::String(format!(
                        "homeboy runner job logs {runner_id} {runner_job_id} --follow"
                    )),
                );
            }
        }
        object.insert("homeboy_in_flight".to_string(), in_flight);
        if let Some(retrieval) = retrieval {
            object.insert("homeboy_persisted_run".to_string(), retrieval.to_json());
        }
    }
    serde_json::to_string_pretty(&value)
        .map(|mut rendered| {
            rendered.push('\n');
            rendered
        })
        .unwrap_or_else(|_| stdout.to_string())
}

fn attach_handoff_metadata(metadata: &mut serde_json::Value, stdout: &str) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(stdout) else {
        return;
    };
    if value.get("schema").and_then(serde_json::Value::as_str)
        != Some(crate::lab_contract::LAB_RUNNER_HANDOFF_ENVELOPE_SCHEMA)
    {
        return;
    }
    let Some(object) = metadata.as_object_mut() else {
        return;
    };
    for (target, path) in [
        ("runner_id", &["identity", "runner_id"][..]),
        ("runner_job_id", &["identity", "runner_job_id"][..]),
        ("job_id", &["job_id"][..]),
        ("durable_run_id", &["durable_run_id"][..]),
        ("handoff_id", &["identity", "handoff_id"][..]),
    ] {
        if let Some(value) = metadata_path_string(&value, path) {
            object.insert(target.to_string(), serde_json::Value::String(value));
        }
    }
    if let Some(commands) = value.get("follow_commands") {
        object.insert("follow_commands".to_string(), commands.clone());
    }
    if let Some(evidence) = value.get("evidence") {
        object.insert("evidence".to_string(), evidence.clone());
    }
}

fn metadata_path_string(value: &serde_json::Value, path: &[&str]) -> Option<String> {
    let mut current = value;
    for segment in path {
        current = current.get(*segment)?;
    }
    current.as_str().map(str::to_string)
}

fn lab_dispatch_metadata(
    runner_id: Option<&str>,
    status: &str,
    extra: serde_json::Value,
) -> serde_json::Value {
    let mut dispatch = json!({
        "phase": LAB_DISPATCH_PHASE,
        "runner_id": runner_id,
        "status": status,
    });
    if let (Some(object), Some(extra)) = (dispatch.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            object.insert(key.clone(), value.clone());
        }
    }
    json!({ "lab_dispatch": dispatch })
}

/// Render offloaded stdout, attaching persisted-run retrieval guidance when a
/// run was recorded. Mirrors the legacy CLI helper but is owned by core so the
/// transformation is testable without constructing a clap `Cli`.
pub(crate) fn stdout_with_persisted_run_retrieval(
    stdout: &str,
    retrieval: Option<&PersistedRunRetrieval>,
) -> String {
    let Some(retrieval) = retrieval else {
        return stdout.to_string();
    };

    if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(stdout) {
        if let Some(object) = value.as_object_mut() {
            object.insert("homeboy_persisted_run".to_string(), retrieval.to_json());
        }
        if let Ok(mut rendered) = serde_json::to_string_pretty(&value) {
            rendered.push('\n');
            return rendered;
        }
    }

    let mut rendered = stdout.to_string();
    if !rendered.ends_with('\n') {
        rendered.push('\n');
    }
    rendered.push('\n');
    rendered.push_str("# Homeboy persisted run\n\n");
    rendered.push_str(&format!(
        "- **Persisted Homeboy run ID:** `{}`\n",
        retrieval.run_id
    ));
    rendered.push_str("- **ID scope:** runtime, temp, and artifact identifiers above are offload context; use the persisted Homeboy run ID for local retrieval.\n");
    rendered.push_str("- **Retrieve evidence:** `");
    rendered.push_str(&retrieval.commands.evidence_command);
    rendered.push_str("`\n");
    rendered.push_str("- **List artifacts:** `");
    rendered.push_str(&retrieval.commands.artifacts_command);
    rendered.push_str("`\n");
    rendered.push_str("- **Export run bundle:** `");
    rendered.push_str(&retrieval.export_command);
    rendered.push_str("`\n");
    rendered
}

pub fn lab_offload_command_from_contract(
    contract: LabCommandContract,
    required_extensions: Vec<String>,
) -> crate::lab_offload::LabOffloadCommand {
    lab_offload_command_from_route_contract(contract.into_route_contract(required_extensions))
}

pub fn lab_route_plan_from_contract(contract: LabCommandContract) -> LabRoutePlan {
    lab_route_plan_from_route_contract(contract.into_route_contract(Vec::new()))
}

pub fn lab_offload_command_from_route_contract(
    route_contract: LabCommandRouteContract,
) -> crate::lab_offload::LabOffloadCommand {
    crate::lab_offload::LabOffloadCommand {
        command: route_contract.command,
        required_extensions: route_contract.required_extensions,
        required_capabilities: route_contract.required_capabilities,
        workload: route_contract.workload,
    }
}

pub fn lab_route_plan_from_route_contract(route_contract: LabCommandRouteContract) -> LabRoutePlan {
    let contract = route_contract.command;
    let mut plan = match contract.portability {
        LabCommandPortability::Portable => LabRoutePlan::portable(contract.hot_label),
        LabCommandPortability::LocalOnly(reason) => {
            LabRoutePlan::local_only(contract.hot_label, reason)
        }
    };
    plan.routing_policy = contract.routing_policy;
    plan.source_policy = match contract.source_path_mode {
        LabSourcePathMode::CwdOrPathFlag => CommandSourcePolicy::ControllerCwdOrExplicitPath,
        LabSourcePathMode::RunnerResident => CommandSourcePolicy::RunnerResident,
    };
    plan.source_materialization = match contract.source_path_mode {
        LabSourcePathMode::CwdOrPathFlag => CommandSourceMaterialization::ControllerCwdAsPathArg,
        LabSourcePathMode::RunnerResident => CommandSourceMaterialization::None,
    };
    plan.workspace_policy = match contract.workspace_mode_policy {
        LabWorkspaceModePolicy::ChangedSinceGitElseSnapshot => {
            CommandWorkspacePolicy::ChangedSinceGitElseSnapshot
        }
        LabWorkspaceModePolicy::Git => CommandWorkspacePolicy::Git,
        LabWorkspaceModePolicy::GitCheckoutRequired => CommandWorkspacePolicy::GitCheckoutRequired,
        LabWorkspaceModePolicy::RunnerResident => CommandWorkspacePolicy::RunnerResident,
    };
    plan.required_extensions = route_contract.required_extensions;
    plan.required_capabilities = route_contract.required_capabilities;
    plan
}

pub fn lab_trace_dispatch_timeout() -> Duration {
    lab_dispatch_timeout()
}

fn lab_dispatch_timeout() -> Duration {
    std::env::var(LAB_DISPATCH_TIMEOUT_ENV)
        .or_else(|_| std::env::var(LAB_TRACE_DISPATCH_TIMEOUT_ENV))
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(DEFAULT_LAB_DISPATCH_TIMEOUT_SECS))
}

pub fn is_lab_offload_subprocess() -> bool {
    std::env::var(crate::observation::LAB_OFFLOAD_METADATA_ENV)
        .is_ok_and(|value| !value.trim().is_empty())
}

fn execute_lab_offload_with_timeout(
    request: LabRoutingRequest<'_>,
    timeout: Duration,
) -> Result<crate::lab_offload::LabOffloadOutcome> {
    let command = request.command;
    let normalized_args = request.normalized_args.to_vec();
    let explicit_runner = request.explicit_runner.map(str::to_string);
    let placement = request.placement;
    let allow_local_fallback = request.allow_local_fallback;
    let allow_dirty_lab_workspace = request.allow_dirty_lab_workspace;
    let skip_deps_hydration = request.skip_deps_hydration;
    let capture_patch = request.capture_patch;
    let active_run_id = request.active_run_id.map(str::to_string);
    let mutation_flag = request.mutation_flag.map(str::to_string);
    let detach_after_handoff = request.detach_after_handoff;
    let output_file_requested = request.output_file_requested;
    let read_only_polling = request.read_only_polling;
    let local_output_file = request.local_output_file.map(str::to_string);
    let durable_agent_task_plan = request.durable_agent_task_plan.cloned();
    let source_path = request.source_path.map(std::path::Path::to_path_buf);
    let verified_cook_baseline = request.verified_cook_baseline.cloned();
    let require_controller_git_bundle = request.require_controller_git_bundle;
    let job_overrides = request.job_overrides;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = crate::lab_offload::execute_lab_offload(LabRoutingRequest {
            command,
            normalized_args: &normalized_args,
            explicit_runner: explicit_runner.as_deref(),
            placement,
            allow_local_fallback,
            allow_dirty_lab_workspace,
            skip_deps_hydration,
            capture_patch,
            mutation_flag: mutation_flag.as_deref(),
            timeout: None,
            active_run_id: None,
            detach_after_handoff,
            output_file_requested,
            read_only_polling,
            local_output_file: local_output_file.as_deref(),
            durable_agent_task_plan: durable_agent_task_plan.as_ref(),
            source_path: source_path.as_deref(),
            verified_cook_baseline: verified_cook_baseline.as_ref(),
            require_controller_git_bundle,
            job_overrides,
        });
        let _ = tx.send(result);
    });

    rx.recv_timeout(timeout).map_err(|_| {
        let mut error = crate::Error::internal_unexpected(format!(
            "Lab offload dispatch did not finish before timeout after {}s",
            timeout.as_secs()
        ))
        .with_hint("The Lab worker thread may still be dispatching or waiting on the remote runner; do not retry blindly while rig resources may still be leased.".to_string())
        .with_hint("Inspect active remote work with `homeboy runs list --status running --limit 20` and `homeboy runs list --status running --runner <runner-id> --limit 20`.".to_string())
        .with_hint("Wait/reconnect by following the runner daemon job when known: `homeboy runner job logs <runner-id> <job-id> --follow`.".to_string())
        .with_hint("Cancel a known daemon job with `homeboy runner job cancel <runner-id> <job-id>`; then confirm the rig lease clears before retrying.".to_string())
        .with_hint("Run `homeboy runner doctor <runner-id>` if the runner daemon no longer responds.".to_string());
        if let Some(run_id) = active_run_id {
            error.details["active_run_id"] = serde_json::Value::String(run_id.clone());
            error = error.with_hint(format!(
                "Controller dispatch run `{run_id}` remains discoverable; inspect it with `homeboy runs show {run_id}`."
            ));
        }
        error
    })?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command_execution_plan::{
        CommandPortability, CommandSourceMaterialization, CommandSourcePolicy,
        CommandWorkspacePolicy,
    };
    use crate::lab_contract::{
        LabCommandContract, LabCommandPortability, LabRoutingPolicy, LabSourcePathMode,
        LAB_CAPABILITY_PLAYWRIGHT, LAB_TRACE_EXTRA_CAPABILITIES,
    };
    use crate::plan::{HomeboyPlan, PlanKind};
    use std::sync::{Mutex, MutexGuard, OnceLock};

    struct EnvGuard {
        previous: Option<String>,
        _guard: MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn set(value: &str) -> Self {
            let guard = env_lock().lock().unwrap_or_else(|err| err.into_inner());
            let previous = std::env::var(LAB_TRACE_DISPATCH_TIMEOUT_ENV).ok();
            std::env::set_var(LAB_TRACE_DISPATCH_TIMEOUT_ENV, value);
            Self {
                previous,
                _guard: guard,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var(LAB_TRACE_DISPATCH_TIMEOUT_ENV, value),
                None => std::env::remove_var(LAB_TRACE_DISPATCH_TIMEOUT_ENV),
            }
        }
    }

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn lab_contract() -> LabCommandContract {
        LabCommandContract {
            hot_label: "trace",
            portability: LabCommandPortability::Portable,
            source_path_mode: LabSourcePathMode::CwdOrPathFlag,
            workspace_mode_policy: LabWorkspaceModePolicy::GitCheckoutRequired,
            capture_mutation_patch: true,
            mutation_flag: Some("--keep-overlay"),
            extra_required_capabilities: LAB_TRACE_EXTRA_CAPABILITIES,
            secret_env_sources: crate::lab_contract::LAB_TRACE_SECRET_ENV_SOURCES,
            routing_policy: LabRoutingPolicy {
                default_lab_offload: true,
                infer_source_path_tools: false,
                release_gate: false,
                requires_extension_parity: true,
                read_only_polling: false,
            },
        }
    }

    #[test]
    fn lab_offload_command_from_contract_preserves_routing_shape() {
        let command = lab_offload_command_from_contract(
            lab_contract(),
            vec!["wordpress".to_string(), "playwright".to_string()],
        );

        assert_eq!(command.command, lab_contract());
        assert_eq!(command.hot_label, "trace");
        assert!(command.is_portable());
        assert!(command.routing_policy.default_lab_offload);
        assert_eq!(
            command.workspace_mode_policy,
            crate::lab_offload::LabOffloadWorkspaceModePolicy::GitCheckoutRequired
        );
        assert!(command.routing_policy.requires_extension_parity);
        assert_eq!(command.required_extensions, vec!["wordpress", "playwright"]);
        assert_eq!(command.required_capabilities.len(), 1);
        assert_eq!(
            command.required_capabilities[0].name,
            LAB_CAPABILITY_PLAYWRIGHT
        );
        assert!(!command.routing_policy.infer_source_path_tools);
    }

    #[test]
    fn lab_route_plan_from_contract_exposes_generic_policy_shape() {
        let plan = lab_route_plan_from_route_contract(
            lab_contract()
                .into_route_contract(vec!["wordpress".to_string(), "playwright".to_string()]),
        );

        assert_eq!(plan.label, "trace");
        assert_eq!(plan.portability, CommandPortability::Portable);
        assert_eq!(plan.local_only_reason(), None);
        assert_eq!(
            plan.source_policy,
            CommandSourcePolicy::ControllerCwdOrExplicitPath
        );
        assert_eq!(
            plan.source_materialization,
            CommandSourceMaterialization::ControllerCwdAsPathArg
        );
        assert_eq!(
            plan.workspace_policy,
            CommandWorkspacePolicy::GitCheckoutRequired
        );
        assert!(plan.routing_policy.default_lab_offload);
        assert!(plan.routing_policy.requires_extension_parity);
        assert_eq!(plan.required_extensions, vec!["wordpress", "playwright"]);
        assert_eq!(plan.required_capabilities.len(), 1);
        assert_eq!(
            plan.required_capabilities[0].name,
            LAB_CAPABILITY_PLAYWRIGHT
        );
    }

    #[test]
    fn lab_route_plan_from_contract_exposes_local_only_reason() {
        let mut contract = lab_contract();
        contract.portability = LabCommandPortability::LocalOnly("needs controller services");
        contract.source_path_mode = LabSourcePathMode::RunnerResident;
        contract.workspace_mode_policy = LabWorkspaceModePolicy::RunnerResident;

        let plan = lab_route_plan_from_route_contract(contract.into_route_contract(Vec::new()));

        assert_eq!(plan.local_only_reason(), Some("needs controller services"));
        assert_eq!(plan.source_policy, CommandSourcePolicy::RunnerResident);
        assert_eq!(
            plan.source_materialization,
            CommandSourceMaterialization::None
        );
        assert_eq!(
            plan.workspace_policy,
            CommandWorkspacePolicy::RunnerResident
        );
    }

    #[test]
    fn route_lab_offload_runs_non_lab_contract_locally() {
        let outcome = route_lab_offload(LabRoutingRequest {
            command: None,
            normalized_args: &["homeboy".to_string(), "status".to_string()],
            explicit_runner: None,
            placement: homeboy_cli_contract::Placement::Auto,
            allow_local_fallback: false,
            allow_dirty_lab_workspace: false,
            skip_deps_hydration: false,
            capture_patch: false,
            mutation_flag: None,
            timeout: None,
            active_run_id: None,
            detach_after_handoff: false,
            output_file_requested: false,
            read_only_polling: false,
            local_output_file: None,
            durable_agent_task_plan: None,
            source_path: None,
            verified_cook_baseline: None,
            require_controller_git_bundle: false,
            job_overrides: crate::lab_offload::LabJobOverrides::default(),
        })
        .unwrap();

        assert!(matches!(
            outcome,
            crate::lab_offload::LabOffloadOutcome::RunLocal { .. }
        ));
    }

    #[test]
    fn trace_dispatch_timeout_reads_env_override() {
        let _env = EnvGuard::set("7");

        assert_eq!(lab_trace_dispatch_timeout(), Duration::from_secs(7));
    }

    #[test]
    fn test_for_run() {
        let retrieval = PersistedRunRetrieval::for_run("run-xyz");

        assert_eq!(retrieval.run_id, "run-xyz");
        assert_eq!(
            retrieval.commands.evidence_command,
            "homeboy runs evidence run-xyz"
        );
        assert_eq!(
            retrieval.commands.artifacts_command,
            "homeboy runs artifacts run-xyz"
        );
        assert_eq!(
            retrieval.export_command,
            "homeboy runs export --run run-xyz --output homeboy-run-run-xyz"
        );
    }

    #[test]
    fn offloaded_json_stdout_labels_persisted_homeboy_run_id() {
        let retrieval = PersistedRunRetrieval::for_run("trace-run-123");

        let stdout = stdout_with_persisted_run_retrieval(
            r#"{"success":true,"data":{"runtime_id":"runtime-abc","artifact_id":"artifact-xyz"}}"#,
            Some(&retrieval),
        );
        let json: serde_json::Value = serde_json::from_str(&stdout).expect("json stdout");

        assert_eq!(json["data"]["runtime_id"], "runtime-abc");
        assert_eq!(json["data"]["artifact_id"], "artifact-xyz");
        assert_eq!(
            json["homeboy_persisted_run"]["persisted_run_id"],
            "trace-run-123"
        );
        assert_eq!(
            json["homeboy_persisted_run"]["retrieval_commands"]["evidence"],
            "homeboy runs evidence trace-run-123"
        );
        assert_eq!(
            json["homeboy_persisted_run"]["retrieval_commands"]["artifacts"],
            "homeboy runs artifacts trace-run-123"
        );
        assert_eq!(
            json["homeboy_persisted_run"]["retrieval_commands"]["export"],
            "homeboy runs export --run trace-run-123 --output homeboy-run-trace-run-123"
        );
    }

    #[test]
    fn offloaded_error_json_stdout_labels_persisted_homeboy_run_id() {
        let retrieval = PersistedRunRetrieval::for_run("trace-run-err");

        let stdout = stdout_with_persisted_run_retrieval(
            r#"{"success":false,"error":{"code":"remote.command_failed","message":"failed","details":{"temp_id":"tmp-1"}}}"#,
            Some(&retrieval),
        );
        let json: serde_json::Value = serde_json::from_str(&stdout).expect("json stdout");

        assert_eq!(json["error"]["details"]["temp_id"], "tmp-1");
        assert_eq!(
            json["homeboy_persisted_run"]["persisted_run_id"],
            "trace-run-err"
        );
        assert_eq!(
            json["homeboy_persisted_run"]["id_scope"],
            "persisted_homeboy_run"
        );
    }

    #[test]
    fn detached_handoff_maps_to_in_flight_output_and_leaves_run_running() {
        let finished = std::sync::Arc::new(std::sync::Mutex::new(None));
        let observer = Box::new(RecordingObserver {
            run_id: Some("dispatch-run-7448".to_string()),
            finished: finished.clone(),
        });
        let stdout = r#"{
            "schema":"homeboy/runner-exec-handoff/v1",
            "status":"running",
            "identity":{
                "runner_id":"homeboy-lab",
                "runner_job_id":"job-7448",
                "run_id":"agent-task-7448"
            },
            "job_id":"job-7448",
            "follow_commands":{
                "job_logs":"homeboy runner job logs homeboy-lab job-7448 --follow"
            }
        }"#;

        let outcome = lab_offload_outcome_to_route_outcome(
            Ok(crate::lab_offload::LabOffloadOutcome::InFlight {
                plan: test_plan(),
                stdout: stdout.to_string(),
                stderr: String::new(),
                exit_code: 0,
                output_file_content: Some(stdout.to_string()),
            }),
            Some("homeboy-lab"),
            observer,
        )
        .expect("route outcome");

        let LabRouteOutcome::InFlight(output) = outcome else {
            panic!("expected in-flight outcome");
        };
        assert_eq!(output.exit_code, 0);
        let json: serde_json::Value = serde_json::from_str(&output.stdout).expect("json stdout");
        assert_eq!(json["status"], "running");
        assert_eq!(json["homeboy_in_flight"]["status"], "running");
        assert_eq!(
            json["homeboy_in_flight"]["homeboy_persisted_run"]["persisted_run_id"],
            "dispatch-run-7448"
        );
        assert_eq!(
            json["homeboy_in_flight"]["watch_command"],
            "homeboy runs watch dispatch-run-7448"
        );
        assert_eq!(
            json["homeboy_in_flight"]["runner_job_watch_command"],
            "homeboy runner job logs homeboy-lab job-7448 --follow"
        );
        assert!(finished.lock().unwrap().is_none());
    }

    #[test]
    fn terminal_offload_maps_to_terminal_output_and_finishes_run() {
        let finished = std::sync::Arc::new(std::sync::Mutex::new(None));
        let observer = Box::new(RecordingObserver {
            run_id: Some("dispatch-run-terminal".to_string()),
            finished: finished.clone(),
        });

        let outcome = lab_offload_outcome_to_route_outcome(
            Ok(crate::lab_offload::LabOffloadOutcome::Offloaded {
                plan: test_plan(),
                stdout: r#"{"success":true}"#.to_string(),
                stderr: String::new(),
                exit_code: 0,
                output_file_content: None,
            }),
            Some("homeboy-lab"),
            observer,
        )
        .expect("route outcome");

        let LabRouteOutcome::Offloaded(output) = outcome else {
            panic!("expected terminal offload outcome");
        };
        assert_eq!(output.exit_code, 0);
        let json: serde_json::Value = serde_json::from_str(&output.stdout).expect("json stdout");
        assert_eq!(
            json["homeboy_persisted_run"]["persisted_run_id"],
            "dispatch-run-terminal"
        );
        let recorded = finished.lock().unwrap().clone();
        let (status, metadata) = recorded.expect("observer finished");
        assert_eq!(status, RunStatus::Pass);
        assert_eq!(metadata["lab_dispatch"]["status"], "offloaded_complete");
    }

    fn test_plan() -> HomeboyPlan {
        HomeboyPlan::for_description(PlanKind::LabOffload, "test lab offload")
    }

    #[test]
    fn offloaded_text_stdout_appends_persisted_run_retrieval_commands() {
        let retrieval = PersistedRunRetrieval::for_run("trace-run-text");

        let stdout = stdout_with_persisted_run_retrieval(
            "runtime_id=runtime-abc\nartifact_id=artifact-xyz\n",
            Some(&retrieval),
        );

        assert!(stdout.contains("runtime_id=runtime-abc"));
        assert!(stdout.contains("artifact_id=artifact-xyz"));
        assert!(stdout.contains("**Persisted Homeboy run ID:** `trace-run-text`"));
        assert!(stdout.contains("`homeboy runs evidence trace-run-text`"));
        assert!(stdout.contains("`homeboy runs artifacts trace-run-text`"));
        assert!(stdout.contains(
            "`homeboy runs export --run trace-run-text --output homeboy-run-trace-run-text`"
        ));
    }

    #[test]
    fn stdout_without_retrieval_is_unchanged() {
        let stdout = stdout_with_persisted_run_retrieval("{\"ok\":true}\n", None);

        assert_eq!(stdout, "{\"ok\":true}\n");
    }

    #[test]
    fn handoff_stdout_metadata_records_queryable_runner_identity() {
        let mut metadata = serde_json::json!({ "exit_code": 0 });
        attach_handoff_metadata(
            &mut metadata,
            r#"{
                "schema":"homeboy/runner-exec-handoff/v1",
                "identity":{
                    "runner_id":"homeboy-lab",
                    "runner_job_id":"job-7167",
                    "handoff_id":"runner:homeboy-lab:job:job-7167"
                },
                "job_id":"job-7167",
                "durable_run_id":"agent-task-dispatch-7167",
                "follow_commands":{
                    "job_logs":"homeboy runner job logs homeboy-lab job-7167 --follow"
                }
            }"#,
        );

        assert_eq!(metadata["runner_id"], "homeboy-lab");
        assert_eq!(metadata["runner_job_id"], "job-7167");
        assert_eq!(metadata["job_id"], "job-7167");
        assert_eq!(metadata["durable_run_id"], "agent-task-dispatch-7167");
        assert_eq!(metadata["handoff_id"], "runner:homeboy-lab:job:job-7167");
        assert_eq!(
            metadata["follow_commands"]["job_logs"],
            "homeboy runner job logs homeboy-lab job-7167 --follow"
        );
    }

    #[test]
    fn noop_observer_finishes_without_retrieval() {
        let observer: Box<dyn LabDispatchObserver> = Box::new(NoopLabDispatchObserver);

        assert_eq!(observer.run_id(), None);
        assert!(observer
            .finish(RunStatus::Skipped, serde_json::Value::Null)
            .is_none());
    }

    /// Observer that records the finishing status/metadata so the orchestration
    /// control flow can be asserted without persisting a real trace run.
    #[derive(Default)]
    struct RecordingObserver {
        run_id: Option<String>,
        finished: std::sync::Arc<std::sync::Mutex<Option<(RunStatus, serde_json::Value)>>>,
    }

    impl LabDispatchObserver for RecordingObserver {
        fn run_id(&self) -> Option<&str> {
            self.run_id.as_deref()
        }

        fn finish(
            self: Box<Self>,
            status: RunStatus,
            metadata: serde_json::Value,
        ) -> Option<PersistedRunRetrieval> {
            *self.finished.lock().unwrap() = Some((status, metadata));
            self.run_id.as_deref().map(PersistedRunRetrieval::for_run)
        }
    }

    #[test]
    fn test_dispatch_lab_offload() {
        // A non-Lab command (`command: None`) routes to a local run and drives
        // the observer to a `Skipped` finish without offloading.
        let finished = std::sync::Arc::new(std::sync::Mutex::new(None));
        let observer = Box::new(RecordingObserver {
            run_id: None,
            finished: finished.clone(),
        });

        let outcome = dispatch_lab_offload(
            LabRoutingRequest {
                command: None,
                normalized_args: &["homeboy".to_string(), "status".to_string()],
                explicit_runner: None,
                placement: homeboy_cli_contract::Placement::Auto,
                allow_local_fallback: false,
                allow_dirty_lab_workspace: false,
                skip_deps_hydration: false,
                capture_patch: false,
                mutation_flag: None,
                timeout: None,
                active_run_id: None,
                detach_after_handoff: false,
                output_file_requested: false,
                read_only_polling: false,
                local_output_file: None,
                durable_agent_task_plan: None,
                source_path: None,
                verified_cook_baseline: None,
                require_controller_git_bundle: false,
                job_overrides: crate::lab_offload::LabJobOverrides::default(),
            },
            None,
            observer,
        )
        .unwrap();

        assert_eq!(outcome, LabRouteOutcome::RunLocal);
        let recorded = finished.lock().unwrap().clone();
        let (status, metadata) = recorded.expect("observer finished");
        assert_eq!(status, RunStatus::Skipped);
        assert_eq!(metadata["lab_dispatch"]["phase"], LAB_DISPATCH_PHASE);
        assert_eq!(metadata["lab_dispatch"]["status"], "run_local");
    }

    #[test]
    fn lab_dispatch_metadata_merges_extra_into_envelope() {
        let metadata = lab_dispatch_metadata(
            Some("homeboy-lab"),
            "offloaded_complete",
            json!({ "exit_code": 0 }),
        );

        assert_eq!(metadata["lab_dispatch"]["phase"], LAB_DISPATCH_PHASE);
        assert_eq!(metadata["lab_dispatch"]["runner_id"], "homeboy-lab");
        assert_eq!(metadata["lab_dispatch"]["status"], "offloaded_complete");
        assert_eq!(metadata["lab_dispatch"]["exit_code"], 0);
    }
}
