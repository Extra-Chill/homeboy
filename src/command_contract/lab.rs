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
use crate::core::agent_tasks::provider::{default_backend, provider_requires_cwd_git_checkout};
use crate::core::engine::execution_context::{self, ResolveOptions};
use crate::core::extension::ExtensionCapability;
use std::collections::BTreeSet;

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
const RIG_UP_LAB_UNSUPPORTED_REASON: &str = "`rig up` stays local because rig pipelines manage local services, leases, ports, and declared filesystem paths that the current single-workspace Lab snapshot cannot safely mirror.";

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
                let mut contract = LabCommandContract::portable(
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
                        | agent_task::AgentTaskCommand::Review(_)
                ) =>
            {
                LabCommandContract::portable_workload(
                    "agent-task status/logs/artifacts/review",
                    None,
                    false,
                    LAB_NO_EXTRA_TOOLS,
                )
            }
            Commands::AgentTask(args)
                if matches!(args.command, agent_task::AgentTaskCommand::Providers(_)) =>
            {
                LabCommandContract::explicit_runner(
                    "agent-task providers",
                    None,
                    false,
                    LAB_NO_EXTRA_TOOLS,
                )
            }
            Commands::AgentTask(args)
                if matches!(
                    args.command,
                    agent_task::AgentTaskCommand::Auth(agent_task::AgentTaskAuthArgs {
                        command: agent_task::AgentTaskAuthCommand::Status(_),
                    })
                ) =>
            {
                LabCommandContract::explicit_runner(
                    "agent-task auth status",
                    None,
                    false,
                    LAB_NO_EXTRA_TOOLS,
                )
            }
            Commands::Audit(args) => args.lab_contract()?,
            Commands::Bench(args) => args.lab_contract()?,
            Commands::Extension(args) if args.is_update_command() => {
                LabCommandContract::explicit_runner(
                    "extension update",
                    None,
                    false,
                    LAB_NO_EXTRA_TOOLS,
                )
            }
            Commands::Fleet(args) => args.lab_contract()?,
            Commands::Lint(args) => args.lab_contract()?,
            Commands::Refactor(args) if args.is_hot_resource_command() => {
                LabCommandContract::portable(
                    "refactor",
                    args.lab_offload_writes_local_state()
                        .then_some("--write/--commit"),
                    false,
                    LAB_NO_EXTRA_TOOLS,
                )
            }
            Commands::Rig(args) if args.is_check_command() => {
                LabCommandContract::portable_workload("rig check", None, false, LAB_NO_EXTRA_TOOLS)
            }
            Commands::Rig(args) if args.is_hot_resource_command() => {
                LabCommandContract::local_only("rig up", RIG_UP_LAB_UNSUPPORTED_REASON)
            }
            Commands::Test(args) => args.lab_contract(),
            Commands::Trace(args) => {
                let mut contract = LabCommandContract::portable_workload(
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
            Commands::Tunnel(args) if args.is_preview_consumer_run() => {
                LabCommandContract::explicit_runner(
                    "tunnel preview-consumer run",
                    None,
                    false,
                    LAB_NO_EXTRA_TOOLS,
                )
            }
            Commands::Tunnel(args) if args.is_service_start() => {
                LabCommandContract::explicit_runner(
                    "tunnel service start",
                    None,
                    false,
                    LAB_NO_EXTRA_TOOLS,
                )
            }
            _ => return None,
        };

        Some(contract)
    }
}

fn agent_task_provider_requires_cwd_git_checkout(command: &agent_task::AgentTaskCommand) -> bool {
    agent_task_provider_requires_cwd_git_checkout_with(
        command,
        default_backend,
        provider_requires_cwd_git_checkout,
    )
}

fn agent_task_provider_requires_cwd_git_checkout_with(
    command: &agent_task::AgentTaskCommand,
    default_backend: impl FnOnce() -> Option<String>,
    provider_requires_cwd_git_checkout: impl Fn(&str, Option<&str>) -> bool,
) -> bool {
    match command {
        agent_task::AgentTaskCommand::Cook(args) | agent_task::AgentTaskCommand::Dispatch(args) => {
            if !args.cwd.as_ref().is_some_and(|cwd| !cwd.trim().is_empty()) {
                return false;
            }
            let backend = args.backend.clone().or_else(default_backend);
            backend.as_ref().is_some_and(|backend| {
                provider_requires_cwd_git_checkout(backend, args.selector.as_deref())
            })
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

impl LabCommandContract {
    pub(crate) fn portable(
        hot_label: &'static str,
        mutation_flag: Option<&'static str>,
        requires_extension_parity: bool,
        extra_required_tools: &'static [LabCommandRequiredTool],
    ) -> Self {
        Self {
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

    pub(crate) fn portable_workload(
        hot_label: &'static str,
        mutation_flag: Option<&'static str>,
        requires_extension_parity: bool,
        extra_required_tools: &'static [LabCommandRequiredTool],
    ) -> Self {
        Self {
            infer_source_path_tools: false,
            ..Self::portable(
                hot_label,
                mutation_flag,
                requires_extension_parity,
                extra_required_tools,
            )
        }
    }

    pub(crate) fn explicit_runner(
        hot_label: &'static str,
        mutation_flag: Option<&'static str>,
        requires_extension_parity: bool,
        extra_required_tools: &'static [LabCommandRequiredTool],
    ) -> Self {
        Self {
            default_lab_offload: false,
            ..Self::portable_workload(
                hot_label,
                mutation_flag,
                requires_extension_parity,
                extra_required_tools,
            )
        }
    }

    pub(crate) fn local_only(hot_label: &'static str, reason: &'static str) -> Self {
        Self {
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

    pub fn lab_required_extensions(&self) -> crate::core::Result<Vec<String>> {
        let Some(contract) = self.lab_contract() else {
            return Ok(Vec::new());
        };
        if !contract.requires_extension_parity {
            return Ok(Vec::new());
        }

        let mut extension_ids = BTreeSet::new();
        match self {
            Commands::Audit(args) => {
                extension_ids.extend(args.extension_override.extensions.clone())
            }
            Commands::Bench(args) => {
                extension_ids.extend(args.extension_override_ids().iter().cloned())
            }
            Commands::Lint(args) => {
                extension_ids.extend(args.extension_override.extensions.clone())
            }
            Commands::Test(args) => {
                extension_ids.extend(args.extension_override.extensions.clone());
                extension_ids.extend(test_lab_extension_ids(args)?);
            }
            Commands::AgentTask(args) => extension_ids.extend(agent_task_lab_extension_ids(args)?),
            _ => {}
        }

        Ok(extension_ids.into_iter().collect())
    }
}

fn agent_task_lab_extension_ids(
    args: &agent_task::AgentTaskArgs,
) -> crate::core::Result<Vec<String>> {
    let agent_task::AgentTaskCommand::RunPlan(run_plan) = &args.command else {
        return Ok(Vec::new());
    };
    if run_plan.plan.trim() == "-" {
        return Ok(Vec::new());
    }

    let plan = crate::core::agent_tasks::service::read_plan(&run_plan.plan)?;
    Ok(crate::core::agent_tasks::required_extension_ids_for_plan(
        &plan,
    ))
}

fn test_lab_extension_ids(
    args: &crate::commands::test::TestArgs,
) -> crate::core::Result<Vec<String>> {
    let source_context = execution_context::resolve(&ResolveOptions {
        component_id: args.comp.component.clone(),
        path_override: args.comp.path.clone(),
        capability: None,
        settings_overrides: args.setting_args.setting.clone(),
        settings_json_overrides: args.setting_args.setting_json.clone(),
        extension_overrides: args.extension_override.extensions.clone(),
    })?;

    if !args.drift
        && args.ci_job.is_none()
        && source_context
            .component
            .has_script(ExtensionCapability::Test)
    {
        return Ok(Vec::new());
    }

    let context = execution_context::resolve(&ResolveOptions {
        component_id: args.comp.component.clone(),
        path_override: args.comp.path.clone(),
        capability: Some(ExtensionCapability::Test),
        settings_overrides: args.setting_args.setting.clone(),
        settings_json_overrides: args.setting_args.setting_json.clone(),
        extension_overrides: args.extension_override.extensions.clone(),
    })?;

    Ok(context.extension_id.into_iter().collect())
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
        assert!(
            parsed_command(&["homeboy", "agent-task", "review", "agent-task-123"])
                .supports_lab_runner()
        );
        assert!(parsed_command(&["homeboy", "agent-task", "providers"]).supports_lab_runner());
        assert!(parsed_command(&[
            "homeboy",
            "tunnel",
            "preview-consumer",
            "run",
            "--config",
            "preview-consumer.json",
            "--preview-public-url",
            "https://preview.example.test/"
        ])
        .supports_lab_runner());
        assert!(parsed_command(&[
            "homeboy",
            "agent-task",
            "auth",
            "status",
            "--secret-env",
            "OPENAI_API_KEY",
        ])
        .supports_lab_runner());
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
                "agent-task status/logs/artifacts/review",
            ),
            (
                parsed_command(&["homeboy", "agent-task", "logs", "agent-task-123"]),
                "agent-task status/logs/artifacts/review",
            ),
            (
                parsed_command(&["homeboy", "agent-task", "artifacts", "agent-task-123"]),
                "agent-task status/logs/artifacts/review",
            ),
            (
                parsed_command(&["homeboy", "agent-task", "review", "agent-task-123"]),
                "agent-task status/logs/artifacts/review",
            ),
            (
                parsed_command(&["homeboy", "agent-task", "providers"]),
                "agent-task providers",
            ),
            (
                parsed_command(&[
                    "homeboy",
                    "agent-task",
                    "auth",
                    "status",
                    "--secret-env",
                    "OPENAI_API_KEY",
                ]),
                "agent-task auth status",
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

        for args in [
            ["homeboy", "agent-task", "status", "agent-task-123"].as_slice(),
            ["homeboy", "agent-task", "review", "agent-task-123"].as_slice(),
        ] {
            let agent_task_inspection = parsed_command(args)
                .lab_contract()
                .expect("agent-task inspection contract");
            assert!(!agent_task_inspection.requires_extension_parity);
            assert!(!agent_task_inspection.infer_source_path_tools);
        }

        let auth_status = parsed_command(&[
            "homeboy",
            "agent-task",
            "auth",
            "status",
            "--secret-env",
            "OPENAI_API_KEY",
        ])
        .lab_contract()
        .expect("agent-task auth status contract");
        assert!(!auth_status.default_lab_offload);
        assert!(!auth_status.requires_extension_parity);
        assert!(!auth_status.infer_source_path_tools);

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
        assert!(parsed_command(&[
            "homeboy",
            "agent-task",
            "auth",
            "map-env",
            "OPENAI_API_KEY",
            "--from",
            "OPENAI_SOURCE_API_KEY",
        ])
        .lab_contract()
        .is_none());
        assert!(
            parsed_command(&["homeboy", "lint", "--file", "src/main.rs"])
                .lab_contract()
                .is_none()
        );
    }

    #[test]
    fn agent_task_git_checkout_policy_uses_default_backend_when_backend_is_omitted() {
        let command = parsed_command(&[
            "homeboy",
            "agent-task",
            "cook",
            "--cwd",
            "/work/repo",
            "--prompt",
            "cook",
        ]);
        let Commands::AgentTask(args) = command else {
            panic!("expected agent-task command");
        };

        assert!(agent_task_provider_requires_cwd_git_checkout_with(
            &args.command,
            || Some("default-patch-provider".to_string()),
            |backend, selector| backend == "default-patch-provider" && selector.is_none(),
        ));
    }

    #[test]
    fn agent_task_git_checkout_policy_keeps_non_cwd_dispatch_snapshot_eligible() {
        let command = parsed_command(&["homeboy", "agent-task", "cook", "--prompt", "cook"]);
        let Commands::AgentTask(args) = command else {
            panic!("expected agent-task command");
        };

        assert!(!agent_task_provider_requires_cwd_git_checkout_with(
            &args.command,
            || Some("default-patch-provider".to_string()),
            |backend, _| backend == "default-patch-provider",
        ));
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
