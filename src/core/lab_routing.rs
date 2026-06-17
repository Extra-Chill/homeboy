//! Core Lab routing service used by the CLI route adapter.

use std::time::Duration;

use crate::command_contract::{
    LabCommandContract, LabCommandPortability, LabCommandRequiredTool, LabSourcePathMode,
    LabWorkspaceModePolicy,
};
use crate::core::command_execution_plan::{
    CommandPortability, CommandSourcePolicy, CommandWorkspacePolicy, LabRoutePlan,
};
use crate::core::runners;
use crate::core::Result;

pub const DEFAULT_LAB_DISPATCH_TIMEOUT_SECS: u64 = 9 * 60;
pub const LAB_DISPATCH_TIMEOUT_ENV: &str = "HOMEBOY_LAB_DISPATCH_TIMEOUT_SECS";
pub const LAB_TRACE_DISPATCH_TIMEOUT_ENV: &str = "HOMEBOY_LAB_TRACE_DISPATCH_TIMEOUT_SECS";

pub struct LabRoutingRequest<'a> {
    pub command: Option<runners::LabOffloadCommand>,
    pub normalized_args: &'a [String],
    pub explicit_runner: Option<&'a str>,
    pub force_hot: bool,
    pub allow_local_hot: bool,
    pub allow_local_fallback: bool,
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
        allow_local_hot: request.allow_local_hot,
        allow_local_fallback: request.allow_local_fallback,
        allow_dirty_lab_workspace: request.allow_dirty_lab_workspace,
        capture_patch: request.capture_patch,
        mutation_flag: request.mutation_flag,
    })
}

pub fn lab_offload_command_from_contract(
    contract: LabCommandContract,
    required_extensions: Vec<String>,
) -> runners::LabOffloadCommand {
    let plan = lab_route_plan_from_contract(contract, required_extensions);
    runners::LabOffloadCommand {
        hot_label: contract.hot_label,
        portable: matches!(plan.portability, CommandPortability::Portable),
        default_lab_offload: plan.default_lab_offload,
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
        requires_extension_parity: plan.requires_extension_parity,
        required_extensions: plan.required_extensions,
        requires_playwright: plan.requires_playwright,
        infer_source_path_tools: plan.infer_source_path_tools,
        release_gate: plan.release_gate,
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
    plan.default_lab_offload = contract.default_lab_offload;
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
    plan.requires_extension_parity = contract.requires_extension_parity;
    plan.required_extensions = required_extensions;
    plan.requires_playwright = contract
        .extra_required_tools
        .iter()
        .any(|tool| matches!(tool, LabCommandRequiredTool::Playwright));
    plan.infer_source_path_tools = contract.infer_source_path_tools;
    plan.release_gate = contract.release_gate;
    plan
}

pub fn lab_trace_dispatch_timeout() -> Duration {
    lab_dispatch_timeout()
}

pub fn lab_dispatch_timeout() -> Duration {
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
    let allow_local_hot = request.allow_local_hot;
    let allow_local_fallback = request.allow_local_fallback;
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
            allow_local_hot,
            allow_local_fallback,
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
        LabCommandContract, LabCommandPortability, LabSourcePathMode, LAB_TRACE_EXTRA_TOOLS,
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
            default_lab_offload: true,
            source_path_mode: LabSourcePathMode::CwdOrPathFlag,
            workspace_mode_policy: LabWorkspaceModePolicy::GitCheckoutRequired,
            mutation_flag: Some("--keep-overlay"),
            requires_extension_parity: true,
            extra_required_tools: LAB_TRACE_EXTRA_TOOLS,
            infer_source_path_tools: false,
            release_gate: false,
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
        assert!(command.default_lab_offload);
        assert_eq!(command.unsupported_reason, None);
        assert_eq!(
            command.workspace_mode_policy,
            runners::LabOffloadWorkspaceModePolicy::GitCheckoutRequired
        );
        assert!(command.requires_extension_parity);
        assert_eq!(command.required_extensions, vec!["wordpress", "playwright"]);
        assert!(command.requires_playwright);
        assert!(!command.infer_source_path_tools);
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
        assert!(plan.default_lab_offload);
        assert!(plan.requires_extension_parity);
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
            allow_local_hot: false,
            allow_local_fallback: false,
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
}
