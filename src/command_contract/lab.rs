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
const AGENT_TASK_LOOP_MISSING_VERIFY_GATE_REASON: &str =
    "agent-task loop requires at least one deterministic --verify or --private-verify gate";
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
pub(crate) const REVIEW_LAB_LABEL: &str = "review";
pub(crate) const BENCH_LAB_LABEL: &str = "bench";
pub(crate) const FUZZ_LAB_LABEL: &str = "fuzz";
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
        hint_label: LINT_LAB_LABEL,
    },
    LabSupportedCommandSummary {
        #[cfg(test)]
        contract_labels: &[TEST_LAB_LABEL],
        message_label: TEST_LAB_LABEL,
        hint_label: TEST_LAB_LABEL,
    },
    LabSupportedCommandSummary {
        #[cfg(test)]
        contract_labels: &[AUDIT_LAB_LABEL],
        message_label: AUDIT_LAB_LABEL,
        hint_label: AUDIT_LAB_LABEL,
    },
    LabSupportedCommandSummary {
        #[cfg(test)]
        contract_labels: &[REVIEW_LAB_LABEL],
        message_label: REVIEW_LAB_LABEL,
        hint_label: REVIEW_LAB_LABEL,
    },
    LabSupportedCommandSummary {
        #[cfg(test)]
        contract_labels: &[BENCH_LAB_LABEL],
        message_label: BENCH_LAB_LABEL,
        hint_label: "bench run",
    },
    LabSupportedCommandSummary {
        #[cfg(test)]
        contract_labels: &[FUZZ_LAB_LABEL],
        message_label: FUZZ_LAB_LABEL,
        hint_label: "fuzz run",
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
                    | agent_task::AgentTaskCommand::RunPlan(_)
                    | agent_task::AgentTaskCommand::Retry(agent_task::RetryArgs { run: true, .. }),
            }) => LabCommandContract::portable(
                AGENT_TASK_RUN_LAB_LABEL,
                None,
                true,
                LAB_NO_EXTRA_TOOLS,
            ),
            Commands::AgentTask(agent_task::AgentTaskArgs {
                command: agent_task::AgentTaskCommand::Loop(args),
            }) if !args.gates.has_deterministic_gate() => LabCommandContract::local_only(
                AGENT_TASK_RUN_LAB_LABEL,
                AGENT_TASK_LOOP_MISSING_VERIFY_GATE_REASON,
            ),
            Commands::AgentTask(agent_task::AgentTaskArgs {
                command: agent_task::AgentTaskCommand::Loop(_),
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
            Commands::Fuzz(args) => args.lab_contract()?,
            Commands::Extension(args) if args.is_update_command() => {
                LabCommandContract::explicit_runner_simple("extension update")
            }
            Commands::Fleet(args) => args.lab_contract()?,
            Commands::Lint(args) => args.lab_contract()?,
            Commands::Review(args) => args.lab_contract(),
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
            Commands::Fuzz(args) => {
                extension_ids.extend(args.extension_override_ids().iter().cloned())
            }
            Commands::Lint(args) => {
                extension_ids.extend(args.extension_override.extensions.clone())
            }
            Commands::Review(args) => {
                extension_ids.extend(args.extension_override.extensions.clone());
                extension_ids.extend(review_lab_extension_ids(args)?);
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

fn review_lab_extension_ids(
    args: &crate::commands::review::ReviewArgs,
) -> crate::core::Result<Vec<String>> {
    let source_context = execution_context::resolve(&ResolveOptions {
        component_id: args.comp.component.clone(),
        path_override: args.comp.path.clone(),
        capability: None,
        settings_overrides: Vec::new(),
        settings_json_overrides: Vec::new(),
        extension_overrides: args.extension_override.extensions.clone(),
    })?;

    if source_context
        .component
        .has_script(ExtensionCapability::Test)
    {
        return Ok(Vec::new());
    }

    let context = execution_context::resolve(&ResolveOptions {
        component_id: args.comp.component.clone(),
        path_override: args.comp.path.clone(),
        capability: Some(ExtensionCapability::Test),
        settings_overrides: Vec::new(),
        settings_json_overrides: Vec::new(),
        extension_overrides: args.extension_override.extensions.clone(),
    })?;

    Ok(context.extension_id.into_iter().collect())
}

#[cfg(test)]
#[path = "../../tests/command_contract/lab_test.rs"]
mod tests;
