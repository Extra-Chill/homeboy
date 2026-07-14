//! Command-to-Lab placement resolution and descriptor projection.

use std::collections::BTreeSet;

use crate::cli_surface::Commands;
use crate::command_contract::spec::{
    AGENT_TASK_AUTH_STATUS_LAB_LABEL, AGENT_TASK_CONTROLLER_FROM_SPEC_LAB_LABEL,
    AGENT_TASK_CONTROLLER_RESUME_LAB_LABEL, AGENT_TASK_FANOUT_COOK_BATCH_LAB_LABEL,
    AGENT_TASK_FANOUT_RUN_PLAN_LAB_LABEL, AGENT_TASK_FANOUT_STATUS_LAB_LABEL,
    AGENT_TASK_FANOUT_SUBMIT_BATCH_LAB_LABEL, AGENT_TASK_PROMOTE_LAB_LABEL,
    AGENT_TASK_PROVIDERS_LAB_LABEL, AGENT_TASK_RUN_LAB_LABEL, AGENT_TASK_STATUS_LAB_LABEL,
    RUNTIME_REFRESH_LAB_LABEL,
};
use crate::command_contract::CommandDescriptor;
use crate::commands::{adapter, agent_task};
use crate::core::agent_tasks::provider::{default_backend, provider_requires_cwd_git_checkout};
use crate::core::engine::execution_context::{self, ResolveOptions};
use crate::core::extension::ExtensionCapability;

use super::{
    CommandPortabilityContract, LabCommandContract, LabCommandPortability, LabWorkspaceModePolicy,
    LAB_AGENT_TASK_SECRET_ENV_SOURCES, LAB_NO_EXTRA_CAPABILITIES,
};

const AGENT_TASK_COOK_MISSING_VERIFY_GATE_REASON: &str =
    "agent-task cook requires at least one deterministic --verify or --private-verify gate";
pub(crate) const AGENT_TASK_COOK_COORDINATOR_CONTROLLER_REASON: &str =
    "agent-task cook is a controller-owned coordinator: it resolves the managed target, ingests provider artifacts, promotes candidates, runs deterministic gates, and finalizes. Offload the materialized agent-task run-plan provider attempt instead.";
const AGENT_TASK_FANOUT_COOK_BATCH_DRY_RUN_CONTROLLER_REASON: &str =
    "agent-task fanout cook-batch --dry-run is controller-local planning; it does not execute cooks and should not offload or materialize the controller cwd";
pub(crate) const AGENT_TASK_FANOUT_COORDINATOR_CONTROLLER_REASON: &str =
    "agent-task fanout coordination is controller-owned so durable batch state, worktree ownership, and recovery remain available; generated independent cooks may use their own Lab placement";

impl Commands {
    pub fn lab_contract(&self) -> Option<LabCommandContract> {
        let mut contract = self.portability_contract().lab_command()?;

        if let Commands::AgentTask(args) = self {
            if matches!(args.command, agent_task::AgentTaskCommand::Promote(_))
                || agent_task_controller_materializes_worktree(&args.command)
                || agent_task_provider_requires_cwd_git_checkout(&args.command)
            {
                contract.workspace_mode_policy = LabWorkspaceModePolicy::GitCheckoutRequired;
            }
        }

        Some(contract)
    }

    pub fn portability_contract(&self) -> CommandPortabilityContract {
        let contract = match self {
            Commands::AgentTask(agent_task::AgentTaskArgs {
                command: agent_task::AgentTaskCommand::Cook(args),
            }) if !args.gates.has_deterministic_gate() => LabCommandContract::local_only(
                AGENT_TASK_RUN_LAB_LABEL,
                AGENT_TASK_COOK_MISSING_VERIFY_GATE_REASON,
            ),
            Commands::AgentTask(agent_task::AgentTaskArgs {
                command: agent_task::AgentTaskCommand::Cook(_),
            }) => LabCommandContract::local_only(
                AGENT_TASK_RUN_LAB_LABEL,
                AGENT_TASK_COOK_COORDINATOR_CONTROLLER_REASON,
            ),
            Commands::AgentTask(agent_task::AgentTaskArgs {
                command:
                    agent_task::AgentTaskCommand::RunPlan(_)
                    | agent_task::AgentTaskCommand::Retry(agent_task::RetryArgs { run: true, .. }),
            }) => LabCommandContract::portable(
                AGENT_TASK_RUN_LAB_LABEL,
                None,
                true,
                LAB_NO_EXTRA_CAPABILITIES,
            )
            .with_secret_env_sources(LAB_AGENT_TASK_SECRET_ENV_SOURCES),
            Commands::AgentTask(agent_task::AgentTaskArgs {
                command: agent_task::AgentTaskCommand::Promote(_),
            }) => LabCommandContract::portable(
                AGENT_TASK_PROMOTE_LAB_LABEL,
                Some("--to-worktree"),
                false,
                LAB_NO_EXTRA_CAPABILITIES,
            ),
            Commands::AgentTask(agent_task::AgentTaskArgs {
                command: agent_task::AgentTaskCommand::Providers(_),
            }) => LabCommandContract::explicit_runner_simple(AGENT_TASK_PROVIDERS_LAB_LABEL),
            Commands::AgentTask(agent_task::AgentTaskArgs {
                command:
                    agent_task::AgentTaskCommand::Fanout(agent_task::AgentTaskFanoutArgs {
                        command: agent_task::AgentTaskFanoutCommand::RunPlan(_),
                    }),
            }) => LabCommandContract::local_only(
                AGENT_TASK_FANOUT_RUN_PLAN_LAB_LABEL,
                AGENT_TASK_FANOUT_COORDINATOR_CONTROLLER_REASON,
            ),
            Commands::AgentTask(agent_task::AgentTaskArgs {
                command:
                    agent_task::AgentTaskCommand::Fanout(agent_task::AgentTaskFanoutArgs {
                        command:
                            agent_task::AgentTaskFanoutCommand::CookBatch(
                                agent_task::AgentTaskFanoutCookBatchArgs { dry_run: true, .. },
                            ),
                    }),
            }) => LabCommandContract::local_only(
                AGENT_TASK_FANOUT_COOK_BATCH_LAB_LABEL,
                AGENT_TASK_FANOUT_COOK_BATCH_DRY_RUN_CONTROLLER_REASON,
            ),
            Commands::AgentTask(agent_task::AgentTaskArgs {
                command:
                    agent_task::AgentTaskCommand::Fanout(agent_task::AgentTaskFanoutArgs {
                        command:
                            agent_task::AgentTaskFanoutCommand::CookBatch(
                                agent_task::AgentTaskFanoutCookBatchArgs { dry_run: false, .. },
                            ),
                    }),
            }) => LabCommandContract::local_only(
                AGENT_TASK_FANOUT_COOK_BATCH_LAB_LABEL,
                AGENT_TASK_FANOUT_COORDINATOR_CONTROLLER_REASON,
            ),
            Commands::AgentTask(agent_task::AgentTaskArgs {
                command:
                    agent_task::AgentTaskCommand::Fanout(agent_task::AgentTaskFanoutArgs {
                        command: agent_task::AgentTaskFanoutCommand::SubmitBatch(_),
                    }),
            }) => {
                LabCommandContract::explicit_runner_simple(AGENT_TASK_FANOUT_SUBMIT_BATCH_LAB_LABEL)
                    .with_secret_env_sources(LAB_AGENT_TASK_SECRET_ENV_SOURCES)
            }
            Commands::AgentTask(agent_task::AgentTaskArgs {
                command:
                    agent_task::AgentTaskCommand::Fanout(agent_task::AgentTaskFanoutArgs {
                        command:
                            agent_task::AgentTaskFanoutCommand::Status(_)
                            | agent_task::AgentTaskFanoutCommand::Artifacts(_),
                    }),
            }) => {
                LabCommandContract::runner_resident_read_polling(AGENT_TASK_FANOUT_STATUS_LAB_LABEL)
            }
            Commands::AgentTask(agent_task::AgentTaskArgs {
                command:
                    agent_task::AgentTaskCommand::Controller(agent_task::AgentTaskControllerArgs {
                        command:
                            agent_task::AgentTaskControllerCommand::FromSpec(
                                agent_task::AgentTaskControllerFromSpecArgs {
                                    resume: true, ..
                                },
                            )
                            | agent_task::AgentTaskControllerCommand::RunFromSpec(_)
                            | agent_task::AgentTaskControllerCommand::Materialize(_),
                    }),
            }) => LabCommandContract::portable(
                AGENT_TASK_CONTROLLER_FROM_SPEC_LAB_LABEL,
                None,
                false,
                LAB_NO_EXTRA_CAPABILITIES,
            )
            .with_secret_env_sources(LAB_AGENT_TASK_SECRET_ENV_SOURCES),
            Commands::AgentTask(agent_task::AgentTaskArgs {
                command:
                    agent_task::AgentTaskCommand::Controller(agent_task::AgentTaskControllerArgs {
                        command: agent_task::AgentTaskControllerCommand::Resume(_),
                    }),
            }) => LabCommandContract::runner_resident(AGENT_TASK_CONTROLLER_RESUME_LAB_LABEL),
            Commands::AgentTask(agent_task::AgentTaskArgs {
                command:
                    agent_task::AgentTaskCommand::Run(_) | agent_task::AgentTaskCommand::RunNext,
            }) => LabCommandContract::runner_resident(AGENT_TASK_STATUS_LAB_LABEL),
            Commands::AgentTask(agent_task::AgentTaskArgs {
                command:
                    agent_task::AgentTaskCommand::Status(_)
                    | agent_task::AgentTaskCommand::Logs(_)
                    | agent_task::AgentTaskCommand::Artifacts(_)
                    | agent_task::AgentTaskCommand::Evidence(_)
                    | agent_task::AgentTaskCommand::Review(_)
                    | agent_task::AgentTaskCommand::List(_)
                    | agent_task::AgentTaskCommand::Active(_)
                    | agent_task::AgentTaskCommand::Latest(_),
            }) => LabCommandContract::runner_resident_read_polling(AGENT_TASK_STATUS_LAB_LABEL),
            Commands::AgentTask(agent_task::AgentTaskArgs {
                command:
                    agent_task::AgentTaskCommand::Auth(agent_task::AgentTaskAuthArgs {
                        command: agent_task::AgentTaskAuthCommand::Status(_),
                    }),
            }) => LabCommandContract::explicit_runner_simple(AGENT_TASK_AUTH_STATUS_LAB_LABEL),
            Commands::Bench(args) => return args.portability_contract(),
            Commands::Fuzz(args) => {
                return CommandPortabilityContract::lab_optional(args.lab_contract())
            }
            Commands::Extension(args) if args.is_update_command() => {
                LabCommandContract::explicit_runner_simple(args.update_command_label())
            }
            Commands::Extension(args) if args.is_runner_resident_read_command() => {
                LabCommandContract::runner_resident(args.runner_resident_read_command_label())
            }
            Commands::Runtime(args) if args.is_refresh_command() => {
                LabCommandContract::explicit_runner_simple(RUNTIME_REFRESH_LAB_LABEL)
            }
            Commands::Worktree(args) => {
                return CommandPortabilityContract::lab_optional(args.lab_contract());
            }
            Commands::Fleet(_) => {
                return CommandPortabilityContract::lab_optional(adapter::lab_contract(self));
            }
            Commands::Review(args) => {
                return CommandPortabilityContract::lab_optional(args.lab_contract())
            }
            Commands::Refactor(args) if args.is_hot_resource_command() => {
                LabCommandContract::portable(
                    "refactor",
                    args.lab_offload_writes_local_state()
                        .then_some("--write/--commit"),
                    false,
                    LAB_NO_EXTRA_CAPABILITIES,
                )
            }
            Commands::Rig(args) => return args.portability_contract(),
            Commands::Trace(args) => return args.portability_contract(),
            Commands::Tunnel(args) => return args.portability_contract(),
            _ => return CommandPortabilityContract::none(),
        };
        CommandPortabilityContract::lab(contract)
    }

    pub fn lab_route_contract(
        &self,
    ) -> crate::core::Result<Option<super::LabCommandRouteContract>> {
        let Some(contract) = self.lab_contract() else {
            return Ok(None);
        };
        let required_extensions = self.lab_required_extensions()?;
        let mut route = contract.into_route_contract(required_extensions);
        route.workload = match self {
            Commands::Bench(args) => args.lab_rig_workload_arguments(),
            Commands::Fuzz(args) => args.lab_rig_workload_arguments(),
            _ => None,
        };
        Ok(Some(route))
    }

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

    pub fn lab_offload_captures_mutation_patch(&self) -> bool {
        if matches!(
            self,
            Commands::AgentTask(agent_task::AgentTaskArgs {
                command: agent_task::AgentTaskCommand::Promote(agent_task::PromoteArgs {
                    dry_run: true,
                    ..
                }),
            })
        ) {
            return false;
        }
        self.lab_contract()
            .is_some_and(|contract| contract.capture_mutation_patch)
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
            Commands::Fuzz(args) => extension_ids.extend(args.lab_route_required_extension_ids()),
            Commands::Review(args) => {
                extension_ids.extend(args.effective_extension_override_ids().iter().cloned());
                extension_ids.extend(review_lab_extension_ids(args)?);
            }
            Commands::AgentTask(args) => extension_ids.extend(agent_task_lab_extension_ids(args)?),
            _ => {}
        }

        Ok(extension_ids.into_iter().collect())
    }
}

pub(crate) fn agent_task_controller_materializes_worktree(
    command: &agent_task::AgentTaskCommand,
) -> bool {
    matches!(
        command,
        agent_task::AgentTaskCommand::Controller(agent_task::AgentTaskControllerArgs {
            command: agent_task::AgentTaskControllerCommand::FromSpec(
                agent_task::AgentTaskControllerFromSpecArgs { resume: true, .. },
            ) | agent_task::AgentTaskControllerCommand::RunFromSpec(_)
                | agent_task::AgentTaskControllerCommand::Materialize(_),
        })
    )
}

pub(crate) fn agent_task_provider_requires_cwd_git_checkout(
    command: &agent_task::AgentTaskCommand,
) -> bool {
    agent_task_provider_requires_cwd_git_checkout_with(
        command,
        || default_backend().ok().flatten(),
        provider_requires_cwd_git_checkout,
    )
}

pub(crate) fn agent_task_provider_requires_cwd_git_checkout_with(
    command: &agent_task::AgentTaskCommand,
    default_backend: impl FnOnce() -> Option<String>,
    provider_requires_cwd_git_checkout: impl Fn(&str, Option<&str>) -> bool,
) -> bool {
    match command {
        agent_task::AgentTaskCommand::Cook(agent_task::AgentTaskCookArgs {
            dispatch: args,
            ..
        }) => {
            let backend = args.backend.clone().or_else(default_backend);
            backend.as_ref().is_some_and(|backend| {
                provider_requires_cwd_git_checkout(backend, args.selector.as_deref())
            }) || args
                .backend
                .as_ref()
                .is_some_and(|backend| !backend.trim().is_empty())
        }
        agent_task::AgentTaskCommand::Controller(agent_task::AgentTaskControllerArgs {
            command:
                agent_task::AgentTaskControllerCommand::FromSpec(
                    agent_task::AgentTaskControllerFromSpecArgs {
                        resume: true,
                        dispatch:
                            agent_task::AgentTaskControllerDispatchArgs {
                                dispatch_backend,
                                dispatch_selector,
                                ..
                            },
                        ..
                    },
                )
                | agent_task::AgentTaskControllerCommand::RunFromSpec(
                    agent_task::AgentTaskControllerRunFromSpecArgs {
                        dispatch:
                            agent_task::AgentTaskControllerDispatchArgs {
                                dispatch_backend,
                                dispatch_selector,
                                ..
                            },
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

pub(crate) fn apply_lab_contract_to_descriptor(
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
    descriptor.lab_offload_captures_mutation_patch =
        contract.is_some_and(|contract| contract.capture_mutation_patch);
    descriptor.lab_offload_mutation_flag = contract.and_then(|contract| contract.mutation_flag);
}

pub(crate) fn agent_task_lab_extension_ids(
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

pub(crate) fn review_lab_extension_ids(
    args: &crate::commands::review::ReviewArgs,
) -> crate::core::Result<Vec<String>> {
    let resolve_for = |capability: Option<ExtensionCapability>| {
        let component_args = args.effective_component_args();
        execution_context::resolve(&ResolveOptions {
            component_id: component_args.component.clone(),
            path_override: component_args.path.clone(),
            capability,
            settings_overrides: Vec::new(),
            settings_profile_json_overrides: Vec::new(),
            settings_json_overrides: Vec::new(),
            extension_overrides: args.effective_extension_override_ids().to_vec(),
        })
    };

    let source_context = resolve_for(None)?;
    if source_context
        .component
        .has_script(ExtensionCapability::Test)
    {
        return Ok(Vec::new());
    }

    let context = resolve_for(Some(ExtensionCapability::Test))?;
    Ok(context.extension_id.into_iter().collect())
}
