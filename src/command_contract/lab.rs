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

/// Routing-policy flags shared by every Lab command representation
/// (`LabCommandContract`, `LabRoutePlan`, `LabOffloadCommand`). These four
/// booleans travel together as one cohesive policy as a command is resolved
/// from its contract into a route plan and finally an offload command, so they
/// live in a single embedded struct rather than being duplicated field-by-field
/// across the three layers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LabRoutingPolicy {
    /// Whether the command offloads to a default Lab runner without an explicit
    /// `--runner` selection.
    pub default_lab_offload: bool,
    /// Whether source-path tool inference applies to this command.
    pub infer_source_path_tools: bool,
    /// Whether this command is a release gate whose routing fidelity matters
    /// for validating a release (lint/test/audit). When true, force-local
    /// bypass and stale-runner local fallback fail closed under the
    /// `/release_gate/local_hot` policy if a default Lab runner is configured.
    /// See issues #4603 / #4605.
    pub release_gate: bool,
    /// Whether the command requires extension parity between controller and
    /// runner before offloading.
    pub requires_extension_parity: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LabCommandContract {
    pub hot_label: &'static str,
    pub portability: LabCommandPortability,
    pub source_path_mode: LabSourcePathMode,
    pub workspace_mode_policy: LabWorkspaceModePolicy,
    pub mutation_flag: Option<&'static str>,
    pub extra_required_tools: &'static [LabCommandRequiredTool],
    /// Routing-policy flags shared across the Lab command layers.
    pub routing_policy: LabRoutingPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabCommandPortability {
    Portable,
    LocalOnly(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabSourcePathMode {
    CwdOrPathFlag,
    RunnerResident,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabWorkspaceModePolicy {
    ChangedSinceGitElseSnapshot,
    Git,
    GitCheckoutRequired,
    RunnerResident,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabCommandRequiredTool {
    Playwright,
}

pub const LAB_TRACE_EXTRA_TOOLS: &[LabCommandRequiredTool] = &[LabCommandRequiredTool::Playwright];
const LAB_NO_EXTRA_TOOLS: &[LabCommandRequiredTool] = &[];
const RIG_UP_LAB_UNSUPPORTED_REASON: &str = "`rig up` stays local because rig pipelines manage local services, leases, ports, and declared filesystem paths that the current single-workspace Lab snapshot cannot safely mirror.";
const AGENT_TASK_RUN_LAB_LABEL: &str = "agent-task dispatch/cook/loop/run-plan/retry --run";
const AGENT_TASK_CONTROLLER_FROM_SPEC_LAB_LABEL: &str =
    "agent-task controller from-spec --resume/materialize";
const AGENT_TASK_CONTROLLER_RESUME_LAB_LABEL: &str = "agent-task controller resume";
const AGENT_TASK_STATUS_LAB_LABEL: &str = "agent-task status/logs/artifacts/review";
const AGENT_TASK_PROVIDERS_LAB_LABEL: &str = "agent-task providers";
const AGENT_TASK_AUTH_STATUS_LAB_LABEL: &str = "agent-task auth status";
pub(crate) const LINT_LAB_LABEL: &str = "lint";
pub(crate) const TEST_LAB_LABEL: &str = "test";
pub(crate) const AUDIT_LAB_LABEL: &str = "audit";
pub(crate) const BENCH_LAB_LABEL: &str = "bench";
const TRACE_LAB_LABEL: &str = "trace";
#[cfg(test)]
const REFACTOR_LAB_LABEL: &str = "refactor";
const RIG_CHECK_LAB_LABEL: &str = "rig check";
const TUNNEL_PREVIEW_CONSUMER_RUN_LAB_LABEL: &str = "tunnel preview-consumer run";
const TUNNEL_SERVICE_EXPOSE_LAB_LABEL: &str = "tunnel service expose";
const TUNNEL_SERVICE_START_LAB_LABEL: &str = "tunnel service start";

struct LabSupportedCommandSummary {
    #[cfg(test)]
    contract_labels: &'static [&'static str],
    message_label: &'static str,
    hint_label: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabRunnerSupportSummary {
    pub supported_labels: Vec<&'static str>,
    pub unsupported_message: String,
    pub hint: String,
}

const LAB_SUPPORTED_COMMAND_SUMMARIES: &[LabSupportedCommandSummary] = &[
    LabSupportedCommandSummary {
        #[cfg(test)]
        contract_labels: &[AGENT_TASK_RUN_LAB_LABEL],
        message_label: "agent-task dispatch/cook/loop/run-plan",
        hint_label: "agent-task dispatch/cook/loop/run-plan",
    },
    LabSupportedCommandSummary {
        #[cfg(test)]
        contract_labels: &[
            AGENT_TASK_CONTROLLER_FROM_SPEC_LAB_LABEL,
            AGENT_TASK_CONTROLLER_RESUME_LAB_LABEL,
        ],
        message_label: "agent-task controller from-spec --resume/materialize/resume",
        hint_label: "agent-task controller from-spec --resume/materialize/resume",
    },
    LabSupportedCommandSummary {
        #[cfg(test)]
        contract_labels: &[AGENT_TASK_RUN_LAB_LABEL],
        message_label: "agent-task retry --run",
        hint_label: "agent-task retry --run",
    },
    LabSupportedCommandSummary {
        #[cfg(test)]
        contract_labels: &[AGENT_TASK_STATUS_LAB_LABEL, AGENT_TASK_PROVIDERS_LAB_LABEL],
        message_label: "agent-task status/logs/artifacts/review/providers",
        hint_label: "agent-task status/logs/artifacts/review/providers",
    },
    LabSupportedCommandSummary {
        #[cfg(test)]
        contract_labels: &[AGENT_TASK_AUTH_STATUS_LAB_LABEL],
        message_label: AGENT_TASK_AUTH_STATUS_LAB_LABEL,
        hint_label: AGENT_TASK_AUTH_STATUS_LAB_LABEL,
    },
    LabSupportedCommandSummary {
        #[cfg(test)]
        contract_labels: &[LINT_LAB_LABEL],
        message_label: LINT_LAB_LABEL,
        hint_label: "full lint",
    },
    LabSupportedCommandSummary {
        #[cfg(test)]
        contract_labels: &[TEST_LAB_LABEL],
        message_label: TEST_LAB_LABEL,
        hint_label: "full test",
    },
    LabSupportedCommandSummary {
        #[cfg(test)]
        contract_labels: &[AUDIT_LAB_LABEL],
        message_label: AUDIT_LAB_LABEL,
        hint_label: AUDIT_LAB_LABEL,
    },
    LabSupportedCommandSummary {
        #[cfg(test)]
        contract_labels: &[BENCH_LAB_LABEL],
        message_label: BENCH_LAB_LABEL,
        hint_label: "bench run",
    },
    LabSupportedCommandSummary {
        #[cfg(test)]
        contract_labels: &[TRACE_LAB_LABEL],
        message_label: TRACE_LAB_LABEL,
        hint_label: TRACE_LAB_LABEL,
    },
    LabSupportedCommandSummary {
        #[cfg(test)]
        contract_labels: &[REFACTOR_LAB_LABEL],
        message_label: "refactor source runs",
        hint_label: "refactor source runs",
    },
    LabSupportedCommandSummary {
        #[cfg(test)]
        contract_labels: &[RIG_CHECK_LAB_LABEL],
        message_label: RIG_CHECK_LAB_LABEL,
        hint_label: RIG_CHECK_LAB_LABEL,
    },
    LabSupportedCommandSummary {
        #[cfg(test)]
        contract_labels: &[TUNNEL_PREVIEW_CONSUMER_RUN_LAB_LABEL],
        message_label: TUNNEL_PREVIEW_CONSUMER_RUN_LAB_LABEL,
        hint_label: TUNNEL_PREVIEW_CONSUMER_RUN_LAB_LABEL,
    },
    LabSupportedCommandSummary {
        #[cfg(test)]
        contract_labels: &[TUNNEL_SERVICE_EXPOSE_LAB_LABEL],
        message_label: TUNNEL_SERVICE_EXPOSE_LAB_LABEL,
        hint_label: TUNNEL_SERVICE_EXPOSE_LAB_LABEL,
    },
    LabSupportedCommandSummary {
        #[cfg(test)]
        contract_labels: &[TUNNEL_SERVICE_START_LAB_LABEL],
        message_label: TUNNEL_SERVICE_START_LAB_LABEL,
        hint_label: TUNNEL_SERVICE_START_LAB_LABEL,
    },
];

pub fn lab_runner_supported_labels() -> Vec<&'static str> {
    LAB_SUPPORTED_COMMAND_SUMMARIES
        .iter()
        .map(|summary| summary.message_label)
        .collect()
}

pub fn lab_runner_support_summary() -> LabRunnerSupportSummary {
    let supported_labels = lab_runner_supported_labels();
    let hint_labels = lab_runner_supported_hint_labels();

    LabRunnerSupportSummary {
        unsupported_message: format!(
            "--runner is only supported for commands with portable Lab offload support: {}",
            human_join(&supported_labels)
        ),
        hint: format!("Current Lab offload support: {}.", human_join(&hint_labels)),
        supported_labels,
    }
}

pub fn lab_runner_unsupported_message() -> String {
    lab_runner_support_summary().unsupported_message
}

pub fn lab_runner_unsupported_hint() -> String {
    lab_runner_support_summary().hint
}

fn lab_runner_supported_hint_labels() -> Vec<&'static str> {
    LAB_SUPPORTED_COMMAND_SUMMARIES
        .iter()
        .map(|summary| summary.hint_label)
        .collect()
}

#[cfg(test)]
fn lab_runner_summary_covers_contract_label(contract_label: &str) -> bool {
    LAB_SUPPORTED_COMMAND_SUMMARIES
        .iter()
        .any(|summary| summary.contract_labels.contains(&contract_label))
}

fn human_join(labels: &[&str]) -> String {
    match labels {
        [] => String::new(),
        [label] => (*label).to_string(),
        [first, second] => format!("{first} and {second}"),
        [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
    }
}

impl Commands {
    pub fn lab_contract(&self) -> Option<LabCommandContract> {
        let mut contract = match self {
            Commands::AgentTask(agent_task::AgentTaskArgs {
                command:
                    agent_task::AgentTaskCommand::Cook(_)
                    | agent_task::AgentTaskCommand::Dispatch(_)
                    | agent_task::AgentTaskCommand::Loop(_)
                    | agent_task::AgentTaskCommand::RunPlan(_)
                    | agent_task::AgentTaskCommand::Retry(agent_task::RetryArgs { run: true, .. }),
            }) => LabCommandContract::portable(
                AGENT_TASK_RUN_LAB_LABEL,
                None,
                true,
                LAB_NO_EXTRA_TOOLS,
            ),
            Commands::AgentTask(agent_task::AgentTaskArgs {
                command: agent_task::AgentTaskCommand::Providers(_),
            }) => LabCommandContract::explicit_runner_simple(AGENT_TASK_PROVIDERS_LAB_LABEL),
            Commands::AgentTask(agent_task::AgentTaskArgs {
                command:
                    agent_task::AgentTaskCommand::Controller(agent_task::AgentTaskControllerArgs {
                        command:
                            agent_task::AgentTaskControllerCommand::FromSpec(
                                agent_task::AgentTaskControllerFromSpecArgs {
                                    resume: true, ..
                                },
                            )
                            | agent_task::AgentTaskControllerCommand::Materialize(_),
                    }),
            }) => LabCommandContract::explicit_runner_simple(
                AGENT_TASK_CONTROLLER_FROM_SPEC_LAB_LABEL,
            ),
            Commands::AgentTask(agent_task::AgentTaskArgs {
                command:
                    agent_task::AgentTaskCommand::Controller(agent_task::AgentTaskControllerArgs {
                        command: agent_task::AgentTaskControllerCommand::Resume(_),
                    }),
            }) => LabCommandContract::runner_resident(AGENT_TASK_CONTROLLER_RESUME_LAB_LABEL),
            Commands::AgentTask(agent_task::AgentTaskArgs {
                command:
                    agent_task::AgentTaskCommand::Status(_)
                    | agent_task::AgentTaskCommand::Logs(_)
                    | agent_task::AgentTaskCommand::Artifacts(_)
                    | agent_task::AgentTaskCommand::Review(_),
            }) => LabCommandContract::runner_resident(AGENT_TASK_STATUS_LAB_LABEL),
            Commands::AgentTask(agent_task::AgentTaskArgs {
                command:
                    agent_task::AgentTaskCommand::Auth(agent_task::AgentTaskAuthArgs {
                        command: agent_task::AgentTaskAuthCommand::Status(_),
                    }),
            }) => LabCommandContract::explicit_runner_simple(AGENT_TASK_AUTH_STATUS_LAB_LABEL),
            Commands::Audit(args) => args.lab_contract()?,
            Commands::Bench(args) => args.lab_contract()?,
            Commands::Extension(args) if args.is_update_command() => {
                LabCommandContract::explicit_runner_simple("extension update")
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
                LabCommandContract::portable_workload(
                    RIG_CHECK_LAB_LABEL,
                    None,
                    false,
                    LAB_NO_EXTRA_TOOLS,
                )
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
                LabCommandContract::explicit_runner_simple(TUNNEL_PREVIEW_CONSUMER_RUN_LAB_LABEL)
            }
            Commands::Tunnel(args) if args.is_service_start() => {
                LabCommandContract::runner_resident(TUNNEL_SERVICE_START_LAB_LABEL)
            }
            Commands::Tunnel(args) if args.is_service_expose() => {
                LabCommandContract::runner_resident(TUNNEL_SERVICE_EXPOSE_LAB_LABEL)
            }
            _ => return None,
        };

        // Agent-task commands whose provider needs a real git checkout of the
        // cwd workspace upgrade to the GitCheckoutRequired policy. This applies
        // uniformly to every resolved agent-task contract; the predicate only
        // returns true for the run/from-spec commands that own a portable or
        // explicit-runner base, so the other arms (which set their own
        // runner-resident policy) are left untouched.
        if let Commands::AgentTask(args) = self {
            if agent_task_provider_requires_cwd_git_checkout(&args.command) {
                contract.workspace_mode_policy = LabWorkspaceModePolicy::GitCheckoutRequired;
            }
        }

        Some(contract)
    }
}

fn agent_task_provider_requires_cwd_git_checkout(command: &agent_task::AgentTaskCommand) -> bool {
    agent_task_provider_requires_cwd_git_checkout_with(
        command,
        || default_backend().ok().flatten(),
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
            let has_workspace = args.cwd.as_ref().is_some_and(|cwd| !cwd.trim().is_empty())
                || args
                    .workspace
                    .as_ref()
                    .is_some_and(|workspace| !workspace.trim().is_empty());
            if !has_workspace {
                return false;
            }
            let backend = args.backend.clone().or_else(default_backend);
            backend.as_ref().is_some_and(|backend| {
                provider_requires_cwd_git_checkout(backend, args.selector.as_deref())
            })
        }
        agent_task::AgentTaskCommand::Controller(agent_task::AgentTaskControllerArgs {
            command:
                agent_task::AgentTaskControllerCommand::FromSpec(
                    agent_task::AgentTaskControllerFromSpecArgs {
                        resume: true,
                        dispatch_backend,
                        dispatch_selector,
                        ..
                    },
                ),
        }) => {
            let backend = dispatch_backend.clone().or_else(default_backend);
            backend.as_ref().is_some_and(|backend| {
                provider_requires_cwd_git_checkout(backend, dispatch_selector.as_deref())
            }) || dispatch_backend
                .as_ref()
                .is_some_and(|backend| !backend.trim().is_empty())
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
            source_path_mode: LabSourcePathMode::CwdOrPathFlag,
            workspace_mode_policy: LabWorkspaceModePolicy::ChangedSinceGitElseSnapshot,
            mutation_flag,
            extra_required_tools,
            routing_policy: LabRoutingPolicy {
                default_lab_offload: true,
                infer_source_path_tools: true,
                release_gate: false,
                requires_extension_parity,
            },
        }
    }

    pub(crate) fn portable_workload(
        hot_label: &'static str,
        mutation_flag: Option<&'static str>,
        requires_extension_parity: bool,
        extra_required_tools: &'static [LabCommandRequiredTool],
    ) -> Self {
        let base = Self::portable(
            hot_label,
            mutation_flag,
            requires_extension_parity,
            extra_required_tools,
        );
        Self {
            routing_policy: LabRoutingPolicy {
                infer_source_path_tools: false,
                ..base.routing_policy
            },
            ..base
        }
    }

    pub(crate) fn explicit_runner(
        hot_label: &'static str,
        mutation_flag: Option<&'static str>,
        requires_extension_parity: bool,
        extra_required_tools: &'static [LabCommandRequiredTool],
    ) -> Self {
        let base = Self::portable_workload(
            hot_label,
            mutation_flag,
            requires_extension_parity,
            extra_required_tools,
        );
        Self {
            routing_policy: LabRoutingPolicy {
                default_lab_offload: false,
                ..base.routing_policy
            },
            ..base
        }
    }

    /// Explicit-runner contract for the common case of a command that takes no
    /// mutation flag, requires no extension parity, and pulls in no extra tools.
    /// Collapses the repeated `explicit_runner(label, None, false,
    /// LAB_NO_EXTRA_TOOLS)` call shape that several agent-task and tunnel arms
    /// share down to a single label argument.
    pub(crate) fn explicit_runner_simple(hot_label: &'static str) -> Self {
        Self::explicit_runner(hot_label, None, false, LAB_NO_EXTRA_TOOLS)
    }

    /// Explicit-runner contract pinned to a runner-resident workspace: the
    /// command operates against state that already lives on the runner, so both
    /// the source-path and workspace-mode policies are runner-resident. Used by
    /// the agent-task status/resume and tunnel service-lifecycle commands, none
    /// of which take a mutation flag, extension parity, or extra tools.
    pub(crate) fn runner_resident(hot_label: &'static str) -> Self {
        Self {
            source_path_mode: LabSourcePathMode::RunnerResident,
            workspace_mode_policy: LabWorkspaceModePolicy::RunnerResident,
            ..Self::explicit_runner_simple(hot_label)
        }
    }

    pub(crate) fn local_only(hot_label: &'static str, reason: &'static str) -> Self {
        Self {
            hot_label,
            portability: LabCommandPortability::LocalOnly(reason),
            source_path_mode: LabSourcePathMode::CwdOrPathFlag,
            workspace_mode_policy: LabWorkspaceModePolicy::ChangedSinceGitElseSnapshot,
            mutation_flag: None,
            extra_required_tools: LAB_NO_EXTRA_TOOLS,
            routing_policy: LabRoutingPolicy {
                default_lab_offload: false,
                infer_source_path_tools: false,
                release_gate: false,
                requires_extension_parity: false,
            },
        }
    }

    /// Mark this contract as a release gate (lint/test/audit) so that the
    /// `/release_gate/local_hot` policy applies to force-local bypass and
    /// stale-runner local fallback when a default Lab runner is configured.
    pub(crate) const fn release_gate(mut self) -> Self {
        self.routing_policy.release_gate = true;
        self
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
        if !contract.routing_policy.requires_extension_parity {
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
    fn test_lab_runner_supported_labels_are_contract_owned() {
        assert_eq!(
            lab_runner_supported_labels().as_slice(),
            &[
                "agent-task dispatch/cook/loop/run-plan",
                "agent-task controller from-spec --resume/materialize/resume",
                "agent-task retry --run",
                "agent-task status/logs/artifacts/review/providers",
                "agent-task auth status",
                "lint",
                "test",
                "audit",
                "bench",
                "trace",
                "refactor source runs",
                "rig check",
                "tunnel preview-consumer run",
                "tunnel service expose",
                "tunnel service start",
            ]
        );
        assert_eq!(
            lab_runner_unsupported_message(),
            "--runner is only supported for commands with portable Lab offload support: agent-task dispatch/cook/loop/run-plan, agent-task controller from-spec --resume/materialize/resume, agent-task retry --run, agent-task status/logs/artifacts/review/providers, agent-task auth status, lint, test, audit, bench, trace, refactor source runs, rig check, tunnel preview-consumer run, tunnel service expose, and tunnel service start"
        );
        assert_eq!(
            lab_runner_unsupported_hint(),
            "Current Lab offload support: agent-task dispatch/cook/loop/run-plan, agent-task controller from-spec --resume/materialize/resume, agent-task retry --run, agent-task status/logs/artifacts/review/providers, agent-task auth status, full lint, full test, audit, bench run, trace, refactor source runs, rig check, tunnel preview-consumer run, tunnel service expose, and tunnel service start."
        );
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
            parsed_command(&["homeboy", "agent-task", "retry", "agent-task-123", "--run"])
                .supports_lab_runner()
        );
        assert!(
            !parsed_command(&["homeboy", "agent-task", "retry", "agent-task-123"])
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
            "tunnel",
            "service",
            "expose",
            "preview",
            "--server",
            "homeboy-lab",
            "--remote-host",
            "127.0.0.1",
            "--remote-port",
            "7331",
            "--auth-mode",
            "ssh-only",
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
        assert!(!parsed_command(&[
            "homeboy", "fleet", "exec", "prod", "--apply", "wp", "plugin", "list",
        ])
        .supports_lab_runner());
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
                "agent-task dispatch/cook/loop/run-plan/retry --run",
            ),
            (
                parsed_command(&["homeboy", "agent-task", "cook", "--prompt", "cook"]),
                "agent-task dispatch/cook/loop/run-plan/retry --run",
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
                "agent-task dispatch/cook/loop/run-plan/retry --run",
            ),
            (
                parsed_command(&["homeboy", "agent-task", "run-plan", "--plan", "@plan.json"]),
                "agent-task dispatch/cook/loop/run-plan/retry --run",
            ),
            (
                parsed_command(&["homeboy", "agent-task", "retry", "agent-task-123", "--run"]),
                "agent-task dispatch/cook/loop/run-plan/retry --run",
            ),
            (
                parsed_command(&["homeboy", "agent-task", "providers"]),
                "agent-task providers",
            ),
            (
                parsed_command(&[
                    "homeboy",
                    "agent-task",
                    "controller",
                    "from-spec",
                    "loop.json",
                    "--resume",
                ]),
                "agent-task controller from-spec --resume/materialize",
            ),
            (
                parsed_command(&[
                    "homeboy",
                    "agent-task",
                    "controller",
                    "materialize",
                    "loop.json",
                ]),
                "agent-task controller from-spec --resume/materialize",
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
            (
                parsed_command(&["homeboy", "rig", "check", "studio"]),
                "rig check",
            ),
        ];

        for (command, label) in supported {
            let contract = command.lab_contract().expect("hot contract");
            assert_eq!(contract.hot_label, label);
            assert!(
                lab_runner_summary_covers_contract_label(contract.hot_label),
                "Lab support summary omitted `{}`",
                contract.hot_label
            );
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
        assert!(lab_runner_summary_covers_contract_label(trace.hot_label));
        assert_eq!(trace.extra_required_tools, LAB_TRACE_EXTRA_TOOLS);
        assert!(!trace.routing_policy.requires_extension_parity);
        assert!(!trace.routing_policy.infer_source_path_tools);
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
        assert!(lint.routing_policy.requires_extension_parity);
        assert!(lint.routing_policy.infer_source_path_tools);
        assert!(lint.routing_policy.release_gate);

        let test_full = parsed_command(&["homeboy", "test"])
            .lab_contract()
            .expect("test contract");
        assert!(test_full.routing_policy.release_gate);

        let audit_full = parsed_command(&["homeboy", "audit"])
            .lab_contract()
            .expect("audit contract");
        assert!(audit_full.routing_policy.release_gate);

        // Changed-scope/local-only variants and non-gate commands are NOT
        // release gates.
        assert!(
            !parsed_command(&["homeboy", "lint", "--changed-since", "origin/main"])
                .lab_contract()
                .expect("changed-scope lint contract")
                .routing_policy
                .release_gate
        );
        assert!(
            !parsed_command(&["homeboy", "bench"])
                .lab_contract()
                .expect("bench contract")
                .routing_policy
                .release_gate
        );
        assert!(
            !parsed_command(&["homeboy", "trace"])
                .lab_contract()
                .expect("trace contract")
                .routing_policy
                .release_gate
        );
        assert!(
            !parsed_command(&["homeboy", "agent-task", "dispatch", "--prompt", "cook"])
                .lab_contract()
                .expect("agent-task contract")
                .routing_policy
                .release_gate
        );

        for args in [
            ["homeboy", "agent-task", "status", "agent-task-123"].as_slice(),
            ["homeboy", "agent-task", "logs", "agent-task-123"].as_slice(),
            ["homeboy", "agent-task", "artifacts", "agent-task-123"].as_slice(),
            ["homeboy", "agent-task", "review", "agent-task-123"].as_slice(),
            ["homeboy", "agent-task", "controller", "resume", "loop-123"].as_slice(),
        ] {
            let contract = parsed_command(args)
                .lab_contract()
                .expect("runner-backed agent-task inspection contract");
            assert_eq!(contract.source_path_mode, LabSourcePathMode::RunnerResident);
            assert_eq!(
                contract.workspace_mode_policy,
                LabWorkspaceModePolicy::RunnerResident
            );
            assert!(!contract.routing_policy.default_lab_offload);
        }

        assert!(
            parsed_command(&[
                "homeboy",
                "agent-task",
                "controller",
                "from-spec",
                "loop.json",
            ])
            .lab_contract()
            .is_none(),
            "from-spec without --resume only writes local controller state"
        );

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
        assert!(!auth_status.routing_policy.default_lab_offload);
        assert!(!auth_status.routing_policy.requires_extension_parity);
        assert!(!auth_status.routing_policy.infer_source_path_tools);

        let tunnel_preview_consumer_run = parsed_command(&[
            "homeboy",
            "tunnel",
            "preview-consumer",
            "run",
            "--config",
            "preview-consumer.json",
            "--preview-public-url",
            "https://preview.example.test/",
        ])
        .lab_contract()
        .expect("tunnel preview-consumer run contract");
        assert!(lab_runner_summary_covers_contract_label(
            tunnel_preview_consumer_run.hot_label
        ));

        let tunnel_service_start = parsed_command(&[
            "homeboy",
            "tunnel",
            "service",
            "start",
            "preview",
            "--cwd",
            "/home/user/Developer/_lab_workspaces/site",
            "--command",
            "npm run dev",
        ])
        .lab_contract()
        .expect("tunnel service start contract");
        assert!(lab_runner_summary_covers_contract_label(
            tunnel_service_start.hot_label
        ));
        assert_eq!(
            tunnel_service_start.source_path_mode,
            LabSourcePathMode::RunnerResident
        );
        assert_eq!(
            tunnel_service_start.workspace_mode_policy,
            LabWorkspaceModePolicy::RunnerResident
        );
        assert!(!tunnel_service_start.routing_policy.default_lab_offload);
        assert!(!tunnel_service_start.routing_policy.infer_source_path_tools);

        let tunnel_service_expose = parsed_command(&[
            "homeboy",
            "tunnel",
            "service",
            "expose",
            "preview",
            "--server",
            "homeboy-lab",
            "--remote-host",
            "127.0.0.1",
            "--remote-port",
            "7331",
            "--auth-mode",
            "ssh-only",
        ])
        .lab_contract()
        .expect("tunnel service expose contract");
        assert!(lab_runner_summary_covers_contract_label(
            tunnel_service_expose.hot_label
        ));
        assert_eq!(
            tunnel_service_expose.source_path_mode,
            LabSourcePathMode::RunnerResident
        );
        assert_eq!(
            tunnel_service_expose.workspace_mode_policy,
            LabWorkspaceModePolicy::RunnerResident
        );
        assert!(!tunnel_service_expose.routing_policy.default_lab_offload);
        assert!(!tunnel_service_expose.routing_policy.infer_source_path_tools);

        let rig = parsed_command(&["homeboy", "rig", "up", "studio"])
            .lab_contract()
            .expect("rig up contract");
        assert_eq!(rig.hot_label, "rig up");
        assert!(matches!(
            rig.portability,
            LabCommandPortability::LocalOnly(reason) if reason.contains("single-workspace Lab snapshot")
        ));

        let fleet = parsed_command(&[
            "homeboy", "fleet", "exec", "prod", "--apply", "wp", "plugin", "list",
        ])
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
    fn agent_task_git_checkout_policy_treats_workspace_like_cwd() {
        let command = parsed_command(&[
            "homeboy",
            "agent-task",
            "dispatch",
            "--workspace",
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
    fn agent_task_git_checkout_policy_covers_controller_from_spec_resume_backend() {
        let command = parsed_command(&[
            "homeboy",
            "agent-task",
            "controller",
            "from-spec",
            "loop.json",
            "--resume",
            "--dispatch-backend",
            "patch-provider",
            "--dispatch-selector",
            "selected",
        ]);
        let Commands::AgentTask(args) = command else {
            panic!("expected agent-task command");
        };

        assert!(agent_task_provider_requires_cwd_git_checkout_with(
            &args.command,
            || None,
            |backend, selector| backend == "patch-provider" && selector == Some("selected"),
        ));
    }

    #[test]
    fn agent_task_git_checkout_policy_requires_git_for_explicit_controller_backend() {
        let command = parsed_command(&[
            "homeboy",
            "agent-task",
            "controller",
            "from-spec",
            "loop.json",
            "--resume",
            "--dispatch-backend",
            "extension-provider",
        ]);
        let Commands::AgentTask(ref args) = command else {
            panic!("expected agent-task command");
        };

        assert!(agent_task_provider_requires_cwd_git_checkout_with(
            &args.command,
            || None,
            |_, _| false,
        ));
        assert_eq!(
            command
                .lab_contract()
                .expect("agent-task controller contract")
                .workspace_mode_policy,
            LabWorkspaceModePolicy::GitCheckoutRequired
        );
    }

    #[test]
    fn agent_task_git_checkout_policy_skips_controller_materialize() {
        let command = parsed_command(&[
            "homeboy",
            "agent-task",
            "controller",
            "materialize",
            "loop.json",
        ]);
        let Commands::AgentTask(args) = command else {
            panic!("expected agent-task command");
        };

        assert!(!agent_task_provider_requires_cwd_git_checkout_with(
            &args.command,
            || Some("patch-provider".to_string()),
            |backend, _| backend == "patch-provider",
        ));
    }

    #[test]
    fn test_lab_runner_unsupported_hot_command_reasons() {
        assert!(parsed_command(&["homeboy", "rig", "up", "studio"])
            .lab_runner_unsupported_reason()
            .expect("rig up reason")
            .contains("single-workspace Lab snapshot"));
        assert!(parsed_command(&[
            "homeboy", "fleet", "exec", "prod", "--apply", "wp", "plugin", "list",
        ])
        .lab_runner_unsupported_reason()
        .expect("fleet exec reason")
        .contains("config parity"));
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
