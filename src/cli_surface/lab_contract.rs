use crate::core::agent_task_provider::provider_requires_cwd_git_checkout;

use super::{CommandDescriptor, Commands};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LabCommandContract {
    pub hot_label: &'static str,
    pub portability: LabCommandPortability,
    pub default_lab_offload: bool,
    pub source_path_mode: LabSourcePathMode,
    pub workspace_mode_policy: LabWorkspaceModePolicy,
    pub mutation_flag: Option<&'static str>,
    pub requires_extension_parity: bool,
    pub extra_required_tools: &'static [LabCommandRequiredTool],
    pub infer_source_path_tools: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabCommandPortability {
    Portable,
    LocalOnly(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabSourcePathMode {
    CwdOrPathFlag,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabWorkspaceModePolicy {
    ChangedSinceGitElseSnapshot,
    Git,
    GitCheckoutRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabCommandRequiredTool {
    Playwright,
}

pub const LAB_TRACE_EXTRA_TOOLS: &[LabCommandRequiredTool] = &[LabCommandRequiredTool::Playwright];
const LAB_NO_EXTRA_TOOLS: &[LabCommandRequiredTool] = &[];
const AUDIT_CHANGED_SINCE_LAB_UNSUPPORTED_REASON: &str = "`audit --changed-since` is not Lab-portable yet because changed-since audit depends on git base refs that the current Lab workspace sync may not have fetched.";
const LINT_CHANGED_SCOPE_LAB_UNSUPPORTED_REASON: &str = "Changed-scope lint runs stay local because changed-file scopes are not represented in the current Lab portability contract yet.";
const TEST_CHANGED_SINCE_LAB_UNSUPPORTED_REASON: &str = "`test --changed-since` is not Lab-portable yet because changed-since test selection depends on git base refs that the current Lab workspace sync may not have fetched.";
const RIG_UP_LAB_UNSUPPORTED_REASON: &str = "`rig up` stays local because rig pipelines manage local services, leases, ports, and declared filesystem paths that the current single-workspace Lab snapshot cannot safely mirror.";
const FLEET_EXEC_LAB_UNSUPPORTED_REASON: &str = "`fleet exec` stays local because it depends on local fleet, project, and server configuration before opening SSH sessions to each project; runner-side config parity is not guaranteed.";

impl Commands {
    pub fn lab_contract(&self) -> Option<LabCommandContract> {
        let contract = match self {
            Commands::AgentTask(args)
                if matches!(
                    args.command,
                    super::agent_task::AgentTaskCommand::Cook(_)
                        | super::agent_task::AgentTaskCommand::Dispatch(_)
                        | super::agent_task::AgentTaskCommand::Loop(_)
                        | super::agent_task::AgentTaskCommand::RunPlan(_)
                ) =>
            {
                let mut contract = lab_portable_contract(
                    "agent-task dispatch/cook/loop/run-plan",
                    None,
                    true,
                    LAB_NO_EXTRA_TOOLS,
                );
                if agent_task_provider_requires_cwd_git_checkout(&args.command) {
                    contract.workspace_mode_policy = LabWorkspaceModePolicy::GitCheckoutRequired;
                }
                contract
            }
            Commands::AgentTask(args)
                if matches!(
                    args.command,
                    super::agent_task::AgentTaskCommand::Status(_)
                        | super::agent_task::AgentTaskCommand::Logs(_)
                        | super::agent_task::AgentTaskCommand::Artifacts(_)
                ) =>
            {
                lab_portable_workload_contract(
                    "agent-task status/logs/artifacts",
                    None,
                    false,
                    LAB_NO_EXTRA_TOOLS,
                )
            }
            Commands::Audit(args) if args.changed_since.is_some() => {
                lab_local_only_contract("audit", AUDIT_CHANGED_SINCE_LAB_UNSUPPORTED_REASON)
            }
            Commands::Audit(args) if args.conventions => return None,
            Commands::Audit(args) => lab_portable_contract(
                "audit",
                (args.baseline_args.baseline || args.baseline_args.ratchet)
                    .then_some("--baseline/--ratchet"),
                true,
                LAB_NO_EXTRA_TOOLS,
            ),
            Commands::Bench(args) if args.is_lab_offload_command() => lab_portable_contract(
                "bench",
                args.lab_offload_writes_local_state()
                    .then_some("--baseline/--ratchet"),
                true,
                LAB_NO_EXTRA_TOOLS,
            ),
            Commands::Extension(args) if args.is_update_command() => {
                lab_explicit_runner_contract("extension update", None, false, LAB_NO_EXTRA_TOOLS)
            }
            Commands::Fleet(args) if args.is_hot_resource_command() => {
                lab_local_only_contract("fleet exec", FLEET_EXEC_LAB_UNSUPPORTED_REASON)
            }
            Commands::Lint(args) if args.is_full_workspace_run() => lab_portable_contract(
                "lint",
                args.fix.then_some("--fix"),
                true,
                LAB_NO_EXTRA_TOOLS,
            ),
            Commands::Lint(args) if args.changed_since.is_some() || args.changed_only => {
                lab_local_only_contract("lint", LINT_CHANGED_SCOPE_LAB_UNSUPPORTED_REASON)
            }
            Commands::Refactor(args) if args.is_hot_resource_command() => lab_portable_contract(
                "refactor",
                args.lab_offload_writes_local_state()
                    .then_some("--write/--commit"),
                false,
                LAB_NO_EXTRA_TOOLS,
            ),
            Commands::Rig(args) if args.is_check_command() => {
                lab_portable_workload_contract("rig check", None, false, LAB_NO_EXTRA_TOOLS)
            }
            Commands::Rig(args) if args.is_hot_resource_command() => {
                lab_local_only_contract("rig up", RIG_UP_LAB_UNSUPPORTED_REASON)
            }
            Commands::Test(args) if args.changed_since.is_none() => lab_portable_contract(
                "test",
                args.write.then_some("--write"),
                true,
                LAB_NO_EXTRA_TOOLS,
            ),
            Commands::Test(_) => {
                lab_local_only_contract("test", TEST_CHANGED_SINCE_LAB_UNSUPPORTED_REASON)
            }
            Commands::Trace(args) => {
                let mut contract = lab_portable_workload_contract(
                    "trace",
                    args.keep_overlay.then_some("--keep-overlay"),
                    false,
                    LAB_TRACE_EXTRA_TOOLS,
                );
                if args.is_compare_target_run() {
                    contract.workspace_mode_policy = LabWorkspaceModePolicy::Git;
                }
                contract
            }
            _ => return None,
        };

        Some(contract)
    }
}

fn agent_task_provider_requires_cwd_git_checkout(
    command: &super::agent_task::AgentTaskCommand,
) -> bool {
    match command {
        super::agent_task::AgentTaskCommand::Cook(args)
        | super::agent_task::AgentTaskCommand::Dispatch(args) => {
            args.cwd.as_ref().is_some_and(|cwd| !cwd.trim().is_empty())
                && provider_requires_cwd_git_checkout(&args.backend, args.selector.as_deref())
        }
        _ => false,
    }
}

pub(super) fn apply_lab_contract_to_descriptor(
    descriptor: &mut CommandDescriptor,
    contract: Option<LabCommandContract>,
) {
    descriptor.supports_lab_runner = contract
        .is_some_and(|contract| matches!(contract.portability, LabCommandPortability::Portable));
    descriptor.lab_runner_unsupported_reason =
        contract.and_then(|contract| match contract.portability {
            LabCommandPortability::Portable => None,
            LabCommandPortability::LocalOnly(reason) => Some(reason),
        });
    descriptor.lab_offload_mutation_flag = contract.and_then(|contract| contract.mutation_flag);
}

fn lab_portable_contract(
    hot_label: &'static str,
    mutation_flag: Option<&'static str>,
    requires_extension_parity: bool,
    extra_required_tools: &'static [LabCommandRequiredTool],
) -> LabCommandContract {
    LabCommandContract {
        hot_label,
        portability: LabCommandPortability::Portable,
        default_lab_offload: true,
        source_path_mode: LabSourcePathMode::CwdOrPathFlag,
        workspace_mode_policy: LabWorkspaceModePolicy::ChangedSinceGitElseSnapshot,
        mutation_flag,
        requires_extension_parity,
        extra_required_tools,
        infer_source_path_tools: true,
    }
}

fn lab_portable_workload_contract(
    hot_label: &'static str,
    mutation_flag: Option<&'static str>,
    requires_extension_parity: bool,
    extra_required_tools: &'static [LabCommandRequiredTool],
) -> LabCommandContract {
    LabCommandContract {
        infer_source_path_tools: false,
        ..lab_portable_contract(
            hot_label,
            mutation_flag,
            requires_extension_parity,
            extra_required_tools,
        )
    }
}

fn lab_explicit_runner_contract(
    hot_label: &'static str,
    mutation_flag: Option<&'static str>,
    requires_extension_parity: bool,
    extra_required_tools: &'static [LabCommandRequiredTool],
) -> LabCommandContract {
    LabCommandContract {
        default_lab_offload: false,
        ..lab_portable_workload_contract(
            hot_label,
            mutation_flag,
            requires_extension_parity,
            extra_required_tools,
        )
    }
}

fn lab_local_only_contract(hot_label: &'static str, reason: &'static str) -> LabCommandContract {
    LabCommandContract {
        hot_label,
        portability: LabCommandPortability::LocalOnly(reason),
        default_lab_offload: false,
        source_path_mode: LabSourcePathMode::CwdOrPathFlag,
        workspace_mode_policy: LabWorkspaceModePolicy::ChangedSinceGitElseSnapshot,
        mutation_flag: None,
        requires_extension_parity: false,
        extra_required_tools: LAB_NO_EXTRA_TOOLS,
        infer_source_path_tools: false,
    }
}
