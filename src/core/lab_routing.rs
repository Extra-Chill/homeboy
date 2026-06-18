//! Core Lab routing service used by the CLI route adapter.

use std::time::Duration;

use serde_json::json;

use crate::command_contract::{
    LabCommandContract, LabCommandPortability, LabCommandRequiredTool, LabSourcePathMode,
    LabWorkspaceModePolicy,
};
use crate::core::command_execution_plan::{
    CommandPortability, CommandSourcePolicy, CommandWorkspacePolicy, LabRoutePlan,
};
use crate::core::observation::records::RunEvidenceCommands;
use crate::core::observation::RunStatus;
use crate::core::runners;
use crate::core::Result;

pub const DEFAULT_LAB_DISPATCH_TIMEOUT_SECS: u64 = 9 * 60;
pub const LAB_DISPATCH_TIMEOUT_ENV: &str = "HOMEBOY_LAB_DISPATCH_TIMEOUT_SECS";
pub const LAB_TRACE_DISPATCH_TIMEOUT_ENV: &str = "HOMEBOY_LAB_TRACE_DISPATCH_TIMEOUT_SECS";

/// Phase tag used for Lab trace dispatch observation metadata payloads. Kept in
/// core so the routing service owns the observation envelope shape rather than
/// the CLI adapter.
const LAB_DISPATCH_PHASE: &str = "route_lab_dispatch";

pub struct LabRoutingRequest<'a> {
    pub command: Option<runners::LabOffloadCommand>,
    pub normalized_args: &'a [String],
    pub explicit_runner: Option<&'a str>,
    pub force_hot: bool,
    pub local_policy: runners::LabLocalExecutionPolicy,
    pub allow_dirty_lab_workspace: bool,
    pub capture_patch: bool,
    pub mutation_flag: Option<&'a str>,
    pub timeout: Option<Duration>,
    pub active_run_id: Option<&'a str>,
}

pub fn route_lab_offload(request: LabRoutingRequest<'_>) -> Result<runners::LabOffloadOutcome> {
    if let Some(timeout) = request.timeout {
        return execute_lab_offload_with_timeout(request, timeout);
    }

    runners::execute_lab_offload(runners::LabOffloadRequest {
        command: request.command,
        normalized_args: request.normalized_args,
        explicit_runner: request.explicit_runner,
        force_hot: request.force_hot,
        local_policy: request.local_policy,
        allow_dirty_lab_workspace: request.allow_dirty_lab_workspace,
        capture_patch: request.capture_patch,
        mutation_flag: request.mutation_flag,
    })
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
}

/// Typed outcome of routing a command through the Lab service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LabRouteOutcome {
    /// The command should continue executing locally on the controller.
    RunLocal,
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
    match route_lab_offload(request) {
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
        Ok(runners::LabOffloadOutcome::RunLocal {
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
            if let Some(metadata) = metadata {
                runners::capture_lab_offload_subprocess_metadata(metadata);
            }
            for message in messages {
                eprintln!("{message}");
            }
            Ok(LabRouteOutcome::RunLocal)
        }
        Ok(runners::LabOffloadOutcome::Offloaded {
            stdout,
            stderr,
            exit_code,
            ..
        }) => {
            let retrieval = observer.finish(
                if exit_code == 0 {
                    RunStatus::Pass
                } else {
                    RunStatus::Fail
                },
                lab_dispatch_metadata(
                    runner_id,
                    "offloaded_complete",
                    json!({ "exit_code": exit_code }),
                ),
            );
            let stdout = stdout_with_persisted_run_retrieval(&stdout, retrieval.as_ref());
            Ok(LabRouteOutcome::Offloaded(LabRouteOutput {
                stdout,
                stderr,
                exit_code,
            }))
        }
    }
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
pub fn stdout_with_persisted_run_retrieval(
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
) -> runners::LabOffloadCommand {
    let plan = lab_route_plan_from_contract(contract, required_extensions);
    runners::LabOffloadCommand {
        hot_label: contract.hot_label,
        portable: matches!(plan.portability, CommandPortability::Portable),
        unsupported_reason: match contract.portability {
            LabCommandPortability::Portable => None,
            LabCommandPortability::LocalOnly(reason) => Some(reason),
        },
        source_path_mode: match plan.source_policy {
            CommandSourcePolicy::ControllerCwdOrExplicitPath
            | CommandSourcePolicy::MaterializeControllerPath => {
                runners::LabOffloadSourcePathMode::CwdOrPathFlag
            }
            CommandSourcePolicy::RunnerResident => {
                runners::LabOffloadSourcePathMode::RunnerResident
            }
        },
        workspace_mode_policy: match plan.workspace_policy {
            CommandWorkspacePolicy::ChangedSinceGitElseSnapshot => {
                runners::LabOffloadWorkspaceModePolicy::ChangedSinceGitElseSnapshot
            }
            CommandWorkspacePolicy::Git => runners::LabOffloadWorkspaceModePolicy::Git,
            CommandWorkspacePolicy::GitCheckoutRequired => {
                runners::LabOffloadWorkspaceModePolicy::GitCheckoutRequired
            }
            CommandWorkspacePolicy::RunnerResident => {
                runners::LabOffloadWorkspaceModePolicy::RunnerResident
            }
            CommandWorkspacePolicy::Snapshot => {
                runners::LabOffloadWorkspaceModePolicy::ChangedSinceGitElseSnapshot
            }
        },
        required_extensions: plan.required_extensions,
        requires_playwright: plan.requires_playwright,
        routing_policy: plan.routing_policy,
    }
}

pub fn lab_route_plan_from_contract(
    contract: LabCommandContract,
    required_extensions: Vec<String>,
) -> LabRoutePlan {
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
    plan.workspace_policy = match contract.workspace_mode_policy {
        LabWorkspaceModePolicy::ChangedSinceGitElseSnapshot => {
            CommandWorkspacePolicy::ChangedSinceGitElseSnapshot
        }
        LabWorkspaceModePolicy::Git => CommandWorkspacePolicy::Git,
        LabWorkspaceModePolicy::GitCheckoutRequired => CommandWorkspacePolicy::GitCheckoutRequired,
        LabWorkspaceModePolicy::RunnerResident => CommandWorkspacePolicy::RunnerResident,
    };
    plan.required_extensions = required_extensions;
    plan.requires_playwright = contract
        .extra_required_tools
        .iter()
        .any(|tool| matches!(tool, LabCommandRequiredTool::Playwright));
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
    std::env::var(crate::core::observation::LAB_OFFLOAD_METADATA_ENV)
        .is_ok_and(|value| !value.trim().is_empty())
}

fn execute_lab_offload_with_timeout(
    request: LabRoutingRequest<'_>,
    timeout: Duration,
) -> Result<runners::LabOffloadOutcome> {
    let command = request.command;
    let normalized_args = request.normalized_args.to_vec();
    let explicit_runner = request.explicit_runner.map(str::to_string);
    let force_hot = request.force_hot;
    let local_policy = request.local_policy;
    let allow_dirty_lab_workspace = request.allow_dirty_lab_workspace;
    let capture_patch = request.capture_patch;
    let active_run_id = request.active_run_id.map(str::to_string);
    let mutation_flag = request.mutation_flag.map(str::to_string);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = runners::execute_lab_offload(runners::LabOffloadRequest {
            command,
            normalized_args: &normalized_args,
            explicit_runner: explicit_runner.as_deref(),
            force_hot,
            local_policy,
            allow_dirty_lab_workspace,
            capture_patch,
            mutation_flag: mutation_flag.as_deref(),
        });
        let _ = tx.send(result);
    });

    rx.recv_timeout(timeout).map_err(|_| {
        let mut error = crate::core::Error::internal_unexpected(format!(
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
    use crate::command_contract::{
        LabCommandContract, LabCommandPortability, LabRoutingPolicy, LabSourcePathMode,
        LAB_TRACE_EXTRA_TOOLS,
    };
    use crate::core::command_execution_plan::{
        CommandPortability, CommandSourcePolicy, CommandWorkspacePolicy,
    };
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
            mutation_flag: Some("--keep-overlay"),
            extra_required_tools: LAB_TRACE_EXTRA_TOOLS,
            routing_policy: LabRoutingPolicy {
                default_lab_offload: true,
                infer_source_path_tools: false,
                release_gate: false,
                requires_extension_parity: true,
            },
        }
    }

    #[test]
    fn lab_offload_command_from_contract_preserves_routing_shape() {
        let command = lab_offload_command_from_contract(
            lab_contract(),
            vec!["wordpress".to_string(), "playwright".to_string()],
        );

        assert_eq!(command.hot_label, "trace");
        assert!(command.portable);
        assert!(command.routing_policy.default_lab_offload);
        assert_eq!(command.unsupported_reason, None);
        assert_eq!(
            command.workspace_mode_policy,
            runners::LabOffloadWorkspaceModePolicy::GitCheckoutRequired
        );
        assert!(command.routing_policy.requires_extension_parity);
        assert_eq!(command.required_extensions, vec!["wordpress", "playwright"]);
        assert!(command.requires_playwright);
        assert!(!command.routing_policy.infer_source_path_tools);
    }

    #[test]
    fn lab_route_plan_from_contract_exposes_generic_policy_shape() {
        let plan = lab_route_plan_from_contract(
            lab_contract(),
            vec!["wordpress".to_string(), "playwright".to_string()],
        );

        assert_eq!(plan.label, "trace");
        assert_eq!(plan.portability, CommandPortability::Portable);
        assert_eq!(plan.local_only_reason(), None);
        assert_eq!(
            plan.source_policy,
            CommandSourcePolicy::ControllerCwdOrExplicitPath
        );
        assert_eq!(
            plan.workspace_policy,
            CommandWorkspacePolicy::GitCheckoutRequired
        );
        assert!(plan.routing_policy.default_lab_offload);
        assert!(plan.routing_policy.requires_extension_parity);
        assert_eq!(plan.required_extensions, vec!["wordpress", "playwright"]);
        assert!(plan.requires_playwright);
    }

    #[test]
    fn lab_route_plan_from_contract_exposes_local_only_reason() {
        let mut contract = lab_contract();
        contract.portability = LabCommandPortability::LocalOnly("needs controller services");
        contract.source_path_mode = LabSourcePathMode::RunnerResident;
        contract.workspace_mode_policy = LabWorkspaceModePolicy::RunnerResident;

        let plan = lab_route_plan_from_contract(contract, Vec::new());

        assert_eq!(plan.local_only_reason(), Some("needs controller services"));
        assert_eq!(plan.source_policy, CommandSourcePolicy::RunnerResident);
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
            force_hot: false,
            local_policy: runners::LabLocalExecutionPolicy::default(),
            allow_dirty_lab_workspace: false,
            capture_patch: false,
            mutation_flag: None,
            timeout: None,
            active_run_id: None,
        })
        .unwrap();

        assert!(matches!(
            outcome,
            runners::LabOffloadOutcome::RunLocal { .. }
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
                force_hot: false,
                local_policy: runners::LabLocalExecutionPolicy::default(),
                allow_dirty_lab_workspace: false,
                capture_patch: false,
                mutation_flag: None,
                timeout: None,
                active_run_id: None,
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
