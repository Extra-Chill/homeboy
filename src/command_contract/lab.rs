//! Lab portability contracts.
//!
//! Owns the data describing whether a command can be offloaded to a Lab
//! runner (`LabCommandContract`, `LabCommandPortability`, `LabSourcePathMode`,
//! `LabWorkspaceModePolicy`, `LabCommandRequiredTool`), the per-command
//! `Commands::lab_contract` resolution, and the helpers that surface
//! Lab-specific information through `CommandDescriptor` and the public
//! `Commands` accessors (`supports_lab_runner`,
//! `lab_runner_unsupported_reason`, `lab_offload_mutation_flag`).

use crate::cli_surface::Commands;
use crate::command_contract::CommandDescriptor;
use crate::commands::agent_task;
use crate::core::agent_tasks::provider::provider_requires_cwd_git_checkout;

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
                    agent_task::AgentTaskCommand::Cook(_)
                        | agent_task::AgentTaskCommand::Dispatch(_)
                        | agent_task::AgentTaskCommand::Loop(_)
                        | agent_task::AgentTaskCommand::RunPlan(_)
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
                    agent_task::AgentTaskCommand::Status(_)
                        | agent_task::AgentTaskCommand::Logs(_)
                        | agent_task::AgentTaskCommand::Artifacts(_)
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

fn agent_task_provider_requires_cwd_git_checkout(command: &agent_task::AgentTaskCommand) -> bool {
    match command {
        agent_task::AgentTaskCommand::Cook(args) | agent_task::AgentTaskCommand::Dispatch(args) => {
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

impl Commands {
    pub fn supports_lab_runner(&self) -> bool {
        self.lab_contract()
            .is_some_and(|contract| matches!(contract.portability, LabCommandPortability::Portable))
    }

    pub fn lab_runner_unsupported_reason(&self) -> Option<&'static str> {
        self.lab_contract()
            .and_then(|contract| match contract.portability {
                LabCommandPortability::Portable => None,
                LabCommandPortability::LocalOnly(reason) => Some(reason),
            })
    }

    pub fn lab_offload_mutation_flag(&self) -> Option<&'static str> {
        self.lab_contract()
            .and_then(|contract| contract.mutation_flag)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_surface::{Cli, Commands};
    use clap::CommandFactory;
    use clap::Parser;

    fn parsed_command(args: &[&str]) -> Commands {
        Cli::try_parse_from(args)
            .expect("CLI args should parse")
            .command
    }

    fn parsed_cli(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).expect("CLI args should parse")
    }

    #[test]
    fn rig_check_supports_lab_runner_but_rig_up_stays_local_only() {
        let rig_check = parsed_command(&["homeboy", "rig", "check", "studio"]);
        let rig_check_descriptor = rig_check.descriptor(false);
        assert!(rig_check_descriptor.supports_lab_runner);
        assert!(rig_check_descriptor.lab_runner_unsupported_reason.is_none());

        let rig_up = parsed_command(&["homeboy", "rig", "up", "studio"]);
        let rig_up_descriptor = rig_up.descriptor(false);
        assert!(!rig_up_descriptor.supports_lab_runner);
        assert!(rig_up_descriptor
            .lab_runner_unsupported_reason
            .is_some_and(|reason| reason.contains("rig up")));
    }

    #[test]
    fn test_supports_lab_runner() {
        assert!(parsed_command(&["homeboy", "lint"]).supports_lab_runner());
        assert!(parsed_command(&["homeboy", "test"]).supports_lab_runner());
        assert!(parsed_command(&["homeboy", "audit"]).supports_lab_runner());
        assert!(parsed_command(&["homeboy", "refactor", "--from", "audit"]).supports_lab_runner());
        assert!(parsed_command(&["homeboy", "refactor", "--all"]).supports_lab_runner());
        assert!(parsed_command(&["homeboy", "bench"]).supports_lab_runner());
        assert!(parsed_command(&[
            "homeboy",
            "bench",
            "matrix",
            "--setting-matrix",
            "clients=10,100"
        ])
        .supports_lab_runner());
        assert!(parsed_command(&["homeboy", "bench", "history", "homeboy"]).supports_lab_runner());
        assert!(parsed_command(&["homeboy", "trace"]).supports_lab_runner());
        assert!(
            parsed_command(&["homeboy", "agent-task", "dispatch", "--prompt", "cook"])
                .supports_lab_runner()
        );
        assert!(
            parsed_command(&["homeboy", "agent-task", "run-plan", "--plan", "@plan.json"])
                .supports_lab_runner()
        );
        assert!(
            parsed_command(&["homeboy", "agent-task", "status", "agent-task-123"])
                .supports_lab_runner()
        );
        assert!(
            parsed_command(&["homeboy", "agent-task", "logs", "agent-task-123"])
                .supports_lab_runner()
        );
        assert!(
            parsed_command(&["homeboy", "agent-task", "artifacts", "agent-task-123"])
                .supports_lab_runner()
        );
        assert!(parsed_command(&[
            "homeboy",
            "agent-task",
            "loop",
            "--to-worktree",
            "homeboy@smoke",
            "--verify",
            "true",
            "--prompt",
            "cook"
        ])
        .supports_lab_runner());
        assert!(!parsed_command(&[
            "homeboy", "refactor", "rename", "--from", "old", "--to", "new",
        ])
        .supports_lab_runner());
        assert!(!parsed_command(&["homeboy", "rig", "up", "studio"]).supports_lab_runner());
        assert!(
            !parsed_command(&["homeboy", "fleet", "exec", "prod", "wp", "plugin", "list"])
                .supports_lab_runner()
        );
        assert!(!parsed_command(&["homeboy", "status"]).supports_lab_runner());
        assert!(!parsed_command(&["homeboy", "bench", "list"]).supports_lab_runner());
        assert!(
            !parsed_command(&["homeboy", "lint", "--changed-since", "origin/main"])
                .supports_lab_runner()
        );
        assert!(
            !parsed_command(&["homeboy", "test", "--changed-since", "origin/main"])
                .supports_lab_runner()
        );

        let cli = parsed_cli(&["homeboy", "lint", "--runner", "lab-a"]);
        assert_eq!(cli.runner.as_deref(), Some("lab-a"));
        assert!(cli.command.supports_lab_runner());

        let cli = parsed_cli(&[
            "homeboy",
            "trace",
            "--runner",
            "homeboy-lab",
            "--allow-local-fallback",
        ]);
        assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));
        assert!(cli.allow_local_fallback);

        let cli = parsed_cli(&["homeboy", "--force-hot", "--allow-local-hot", "bench"]);
        assert!(cli.force_hot);
        assert!(cli.allow_local_hot);
        assert!(cli.command.supports_lab_runner());
    }

    #[test]
    fn test_lab_command_contracts_cover_hot_commands() {
        let supported = [
            (parsed_command(&["homeboy", "lint"]), "lint"),
            (parsed_command(&["homeboy", "test"]), "test"),
            (parsed_command(&["homeboy", "audit"]), "audit"),
            (parsed_command(&["homeboy", "bench"]), "bench"),
            (
                parsed_command(&[
                    "homeboy",
                    "bench",
                    "matrix",
                    "--setting-matrix",
                    "clients=10,100",
                ]),
                "bench",
            ),
            (
                parsed_command(&["homeboy", "bench", "history", "homeboy"]),
                "bench",
            ),
            (parsed_command(&["homeboy", "trace"]), "trace"),
            (
                parsed_command(&["homeboy", "refactor", "--from", "audit"]),
                "refactor",
            ),
            (
                parsed_command(&["homeboy", "agent-task", "dispatch", "--prompt", "cook"]),
                "agent-task dispatch/cook/loop/run-plan",
            ),
            (
                parsed_command(&["homeboy", "agent-task", "cook", "--prompt", "cook"]),
                "agent-task dispatch/cook/loop/run-plan",
            ),
            (
                parsed_command(&[
                    "homeboy",
                    "agent-task",
                    "loop",
                    "--to-worktree",
                    "homeboy@smoke",
                    "--verify",
                    "true",
                    "--prompt",
                    "cook",
                ]),
                "agent-task dispatch/cook/loop/run-plan",
            ),
            (
                parsed_command(&["homeboy", "agent-task", "run-plan", "--plan", "@plan.json"]),
                "agent-task dispatch/cook/loop/run-plan",
            ),
            (
                parsed_command(&["homeboy", "agent-task", "status", "agent-task-123"]),
                "agent-task status/logs/artifacts",
            ),
            (
                parsed_command(&["homeboy", "agent-task", "logs", "agent-task-123"]),
                "agent-task status/logs/artifacts",
            ),
            (
                parsed_command(&["homeboy", "agent-task", "artifacts", "agent-task-123"]),
                "agent-task status/logs/artifacts",
            ),
        ];

        for (command, label) in supported {
            let contract = command.lab_contract().expect("hot contract");
            assert_eq!(contract.hot_label, label);
            assert_eq!(contract.portability, LabCommandPortability::Portable);
            assert_eq!(contract.source_path_mode, LabSourcePathMode::CwdOrPathFlag);
            assert_eq!(
                contract.workspace_mode_policy,
                LabWorkspaceModePolicy::ChangedSinceGitElseSnapshot
            );
        }

        let trace = parsed_command(&["homeboy", "trace"])
            .lab_contract()
            .expect("trace contract");
        assert_eq!(trace.extra_required_tools, LAB_TRACE_EXTRA_TOOLS);
        assert!(!trace.requires_extension_parity);
        assert!(!trace.infer_source_path_tools);
        assert_eq!(
            trace.workspace_mode_policy,
            LabWorkspaceModePolicy::ChangedSinceGitElseSnapshot
        );

        let trace_compare_refs = parsed_command(&[
            "homeboy",
            "trace",
            "compare",
            "woocommerce-gateway-stripe",
            "ece-product-page-waterfall",
            "--baseline-target",
            "origin/develop",
            "--candidate",
            "32f68bb07ac0efa1d754f78e2adc8de115ddca6f",
        ])
        .lab_contract()
        .expect("trace compare contract");
        assert_eq!(
            trace_compare_refs.workspace_mode_policy,
            LabWorkspaceModePolicy::Git
        );

        let lint = parsed_command(&["homeboy", "lint"])
            .lab_contract()
            .expect("lint contract");
        assert!(lint.requires_extension_parity);
        assert!(lint.infer_source_path_tools);

        let agent_task_status =
            parsed_command(&["homeboy", "agent-task", "status", "agent-task-123"])
                .lab_contract()
                .expect("agent-task status contract");
        assert!(!agent_task_status.requires_extension_parity);
        assert!(!agent_task_status.infer_source_path_tools);

        let rig = parsed_command(&["homeboy", "rig", "up", "studio"])
            .lab_contract()
            .expect("rig up contract");
        assert_eq!(rig.hot_label, "rig up");
        assert!(matches!(
            rig.portability,
            LabCommandPortability::LocalOnly(reason) if reason.contains("single-workspace Lab snapshot")
        ));

        let fleet = parsed_command(&["homeboy", "fleet", "exec", "prod", "wp", "plugin", "list"])
            .lab_contract()
            .expect("fleet exec contract");
        assert_eq!(fleet.hot_label, "fleet exec");
        assert!(matches!(
            fleet.portability,
            LabCommandPortability::LocalOnly(reason) if reason.contains("config parity")
        ));

        for args in [
            ["homeboy", "audit", "--changed-since", "origin/main"].as_slice(),
            ["homeboy", "lint", "--changed-since", "origin/main"].as_slice(),
            ["homeboy", "lint", "--changed-only"].as_slice(),
            ["homeboy", "test", "--changed-since", "origin/main"].as_slice(),
        ] {
            let contract = parsed_command(args)
                .lab_contract()
                .expect("scoped hot command should have a Lab plan contract");
            assert!(matches!(
                contract.portability,
                LabCommandPortability::LocalOnly(_)
            ));
        }

        assert!(parsed_command(&["homeboy", "status"])
            .lab_contract()
            .is_none());
        assert!(parsed_command(&["homeboy", "bench", "list"])
            .lab_contract()
            .is_none());
        assert!(parsed_command(&["homeboy", "audit", "--conventions"])
            .lab_contract()
            .is_none());
        assert!(
            parsed_command(&["homeboy", "lint", "--file", "src/main.rs"])
                .lab_contract()
                .is_none()
        );
    }

    #[test]
    fn test_lab_runner_unsupported_hot_command_reasons() {
        assert!(parsed_command(&["homeboy", "rig", "up", "studio"])
            .lab_runner_unsupported_reason()
            .expect("rig up reason")
            .contains("single-workspace Lab snapshot"));
        assert!(
            parsed_command(&["homeboy", "fleet", "exec", "prod", "wp", "plugin", "list"])
                .lab_runner_unsupported_reason()
                .expect("fleet exec reason")
                .contains("config parity")
        );
        assert!(
            parsed_command(&["homeboy", "lint", "--changed-since", "origin/main"])
                .lab_runner_unsupported_reason()
                .expect("changed-scope lint reason")
                .contains("Changed-scope lint runs stay local")
        );
        assert!(
            parsed_command(&["homeboy", "test", "--changed-since", "origin/main"])
                .lab_runner_unsupported_reason()
                .expect("changed-since test reason")
                .contains("test --changed-since")
        );
        assert!(parsed_command(&["homeboy", "status"])
            .lab_runner_unsupported_reason()
            .is_none());
    }

    #[test]
    fn test_lab_runner_flag_is_visible_in_help() {
        let root_help = Cli::command()
            .try_get_matches_from(["homeboy", "--help"])
            .expect_err("help exits")
            .to_string();
        assert!(root_help.contains("--runner"));

        for args in [
            ["homeboy", "rig", "check", "--help"].as_slice(),
            ["homeboy", "build", "--help"].as_slice(),
            ["homeboy", "bench", "list", "--help"].as_slice(),
        ] {
            let help = Cli::command()
                .try_get_matches_from(args)
                .expect_err("help exits")
                .to_string();
            assert!(help.contains("--runner"), "{args:?} help omitted --runner");
        }
    }

    #[test]
    fn test_lab_offload_mutation_flag() {
        assert_eq!(
            parsed_command(&["homeboy", "lint", "--fix"]).lab_offload_mutation_flag(),
            Some("--fix")
        );
        assert_eq!(
            parsed_command(&["homeboy", "test", "--write"]).lab_offload_mutation_flag(),
            Some("--write")
        );
        assert_eq!(
            parsed_command(&["homeboy", "bench", "--baseline"]).lab_offload_mutation_flag(),
            Some("--baseline/--ratchet")
        );
        assert_eq!(
            parsed_command(&["homeboy", "trace", "--keep-overlay"]).lab_offload_mutation_flag(),
            Some("--keep-overlay")
        );
        assert_eq!(
            parsed_command(&["homeboy", "refactor", "--from", "audit", "--write"])
                .lab_offload_mutation_flag(),
            Some("--write/--commit")
        );
        assert_eq!(
            parsed_command(&["homeboy", "audit"]).lab_offload_mutation_flag(),
            None
        );
        assert_eq!(
            parsed_command(&["homeboy", "audit", "--baseline"]).lab_offload_mutation_flag(),
            Some("--baseline/--ratchet")
        );
        assert_eq!(
            parsed_command(&["homeboy", "audit", "--ratchet"]).lab_offload_mutation_flag(),
            Some("--baseline/--ratchet")
        );
    }
}
