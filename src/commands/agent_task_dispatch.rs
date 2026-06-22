use clap::Args;

use homeboy::core::agent_tasks::dispatch_service::{AgentTaskDispatchCommand, DispatchCoreInputs};

/// CLI surface for the dispatch inputs shared across dispatch carriers. Flattened
/// into [`DispatchArgs`] so the `--tasks/--provider-config/--client-context/
/// --attempts/--queue-only` flags stay identical while the field group is
/// declared once (#5187). The `#[arg]` attributes reproduce the original flag
/// names, value names, and defaults exactly.
#[derive(Args, Debug, Clone)]
pub struct DispatchCoreArgs {
    /// JSON array/object of task prompts for waves. Supports @file and - for stdin.
    #[arg(long = "tasks", value_name = "JSON")]
    pub tasks_json: Option<String>,

    /// Provider config JSON object, @file, or - for stdin. Merged with workspace metadata.
    #[arg(long = "provider-config", value_name = "JSON")]
    pub provider_config: Option<String>,

    /// Opaque client context JSON object, @file, or - for stdin.
    #[arg(long = "client-context", value_name = "JSON")]
    pub client_context: Option<String>,

    /// Attempts per task, including the first attempt.
    #[arg(long, default_value_t = 1, value_name = "N")]
    pub attempts: u32,

    /// Persist the run for a daemon/runner but do not execute immediately.
    #[arg(long)]
    pub queue_only: bool,
}

impl From<DispatchCoreArgs> for DispatchCoreInputs {
    fn from(args: DispatchCoreArgs) -> Self {
        DispatchCoreInputs {
            tasks_json: args.tasks_json,
            provider_config: args.provider_config,
            client_context: args.client_context,
            attempts: args.attempts,
            queue_only: args.queue_only,
        }
    }
}

#[derive(Args, Debug, Clone)]
pub struct DispatchArgs {
    /// Inline prompt, @file, or - for stdin. Repeat --task for waves.
    #[arg(long, value_name = "PROMPT")]
    pub prompt: Option<String>,

    /// Additional task prompt, @file, or - for stdin. Each --task creates one minion cell.
    #[arg(long = "task", value_name = "PROMPT")]
    pub tasks: Vec<String>,

    /// Existing local repo checkout or worktree path to cook in.
    #[arg(long, value_name = "PATH")]
    pub cwd: Option<String>,

    /// Homeboy workspace ID or existing local workspace path to cook in.
    #[arg(long, value_name = "ID_OR_PATH")]
    pub workspace: Option<String>,

    /// Repo/component slug for metadata and task grouping, e.g. sample-plugin.
    #[arg(long, value_name = "REPO")]
    pub repo: Option<String>,

    /// Issue, PR, or tracker URL the task is cooking.
    #[arg(long, value_name = "URL")]
    pub task_url: Option<String>,

    /// Executor backend to request. Defaults to the configured coding backend.
    #[arg(long, value_name = "BACKEND")]
    pub backend: Option<String>,

    /// Optional provider id when more than one provider exists for the backend.
    #[arg(
        long,
        visible_alias = "dispatch-provider-id",
        value_name = "PROVIDER_ID"
    )]
    pub selector: Option<String>,

    /// Optional model override passed through to the provider.
    #[arg(long, value_name = "MODEL")]
    pub model: Option<String>,

    /// Controller-owned abstract executor capabilities compiled from workflow declarations.
    #[arg(skip)]
    pub required_capabilities: Vec<String>,

    /// Secret environment variable name to hydrate for the provider. Repeatable.
    #[arg(long = "secret-env", value_name = "ENV")]
    pub secret_env: Vec<String>,

    /// Maximum number of task cells to run at once.
    #[arg(long, default_value_t = 1, value_name = "N")]
    pub concurrency: usize,

    /// Optional durable run id. Generated when omitted.
    #[arg(long, value_name = "ID")]
    pub run_id: Option<String>,

    #[command(flatten)]
    pub core: DispatchCoreArgs,
}

impl From<DispatchArgs> for AgentTaskDispatchCommand {
    fn from(args: DispatchArgs) -> Self {
        AgentTaskDispatchCommand {
            prompt: args.prompt,
            tasks: args.tasks,
            cwd: args.cwd,
            workspace: args.workspace,
            repo: args.repo,
            task_url: args.task_url,
            backend: args.backend,
            selector: args.selector,
            model: args.model,
            required_capabilities: args.required_capabilities,
            secret_env: args.secret_env,
            concurrency: args.concurrency,
            run_id: args.run_id,
            core: args.core.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_isolated_home;
    use homeboy::core::agent_task::AgentTaskRequest;
    use homeboy::core::agent_tasks::dispatch_service;
    use homeboy::core::agent_tasks::scheduler::{
        AgentTaskExecutionContext, AgentTaskExecutorAdapter,
    };
    use homeboy::core::agent_tasks::{
        AgentTaskOutcome, AgentTaskOutcomeStatus, AGENT_TASK_OUTCOME_SCHEMA,
    };
    use serde_json::Value;

    #[test]
    fn dispatch_adapter_preserves_queue_only_envelope_shape() {
        with_isolated_home(|_| {
            let workspace = tempfile::tempdir().expect("workspace");

            let (value, exit_code) = dispatch_service::run_dispatch_command(
                dispatch_args(DispatchArgOverrides {
                    prompt: Some("Queue this minion.".to_string()),
                    cwd: Some(workspace.path().display().to_string()),
                    run_id: Some("dispatch-queued".to_string()),
                    core: DispatchCoreInputs {
                        queue_only: true,
                        ..DispatchCoreInputs::default()
                    },
                    ..DispatchArgOverrides::default()
                })
                .into(),
                NoopExecutor,
            )
            .expect("dispatch queued");

            assert_eq!(exit_code, 0);
            assert_eq!(value["schema"], dispatch_service::DISPATCH_RESULT_SCHEMA);
            assert_eq!(value["run_id"], "dispatch-queued");
            assert_eq!(value["queued"], true);
            assert_eq!(value["state"], "queued");
            assert!(value["plan_path"]
                .as_str()
                .expect("plan path")
                .ends_with("plan.json"));
        });
    }

    #[test]
    fn dispatch_request_uses_declared_default_backend_when_backend_is_absent() {
        let request = dispatch_service::resolve_dispatch_request_with_default(
            DispatchArgs {
                prompt: Some("run with default".to_string()),
                tasks: Vec::new(),
                cwd: None,
                workspace: None,
                repo: None,
                task_url: None,
                backend: None,
                selector: None,
                model: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                concurrency: 1,
                run_id: None,
                core: DispatchCoreArgs {
                    tasks_json: None,
                    provider_config: None,
                    client_context: None,
                    attempts: 1,
                    queue_only: false,
                },
            }
            .into(),
            |_| Ok(Some("fake-default".to_string())),
        )
        .expect("default backend request");

        assert_eq!(request.backend, "fake-default");
    }

    #[test]
    fn dispatch_request_errors_when_backend_and_default_are_absent() {
        let error = dispatch_service::resolve_dispatch_request_with_default(
            DispatchArgs {
                prompt: Some("run without default".to_string()),
                tasks: Vec::new(),
                cwd: None,
                workspace: None,
                repo: None,
                task_url: None,
                backend: None,
                selector: None,
                model: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                concurrency: 1,
                run_id: None,
                core: DispatchCoreArgs {
                    tasks_json: None,
                    provider_config: None,
                    client_context: None,
                    attempts: 1,
                    queue_only: false,
                },
            }
            .into(),
            |_| Ok(None),
        )
        .expect_err("missing backend should fail");

        assert!(error.message.contains("requires --backend"));
    }

    struct NoopExecutor;

    impl AgentTaskExecutorAdapter for NoopExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            AgentTaskOutcome {
                schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: request.task_id,
                status: AgentTaskOutcomeStatus::Succeeded,
                summary: Some("ok".to_string()),
                failure_classification: None,
                artifacts: Vec::new(),
                typed_artifacts: Vec::new(),
                evidence_refs: Vec::new(),
                diagnostics: Vec::new(),
                outputs: Value::Null,
                workflow: None,
                follow_up: None,
                metadata: Value::Null,
            }
        }
    }

    #[derive(Default)]
    struct DispatchArgOverrides {
        prompt: Option<String>,
        cwd: Option<String>,
        workspace: Option<String>,
        repo: Option<String>,
        task_url: Option<String>,
        secret_env: Vec<String>,
        concurrency: usize,
        run_id: Option<String>,
        backend: Option<String>,
        core: DispatchCoreInputs,
    }

    fn dispatch_args(overrides: DispatchArgOverrides) -> DispatchArgs {
        DispatchArgs {
            prompt: overrides.prompt,
            tasks: Vec::new(),
            cwd: overrides.cwd,
            workspace: overrides.workspace,
            repo: overrides.repo,
            task_url: overrides.task_url,
            backend: Some(overrides.backend.unwrap_or_else(|| "fixture".to_string())),
            selector: None,
            model: None,
            required_capabilities: Vec::new(),
            secret_env: overrides.secret_env,
            concurrency: if overrides.concurrency == 0 {
                1
            } else {
                overrides.concurrency
            },
            run_id: overrides.run_id,
            core: DispatchCoreArgs {
                tasks_json: overrides.core.tasks_json,
                provider_config: overrides.core.provider_config,
                client_context: overrides.core.client_context,
                attempts: if overrides.core.attempts == 0 {
                    1
                } else {
                    overrides.core.attempts
                },
                queue_only: overrides.core.queue_only,
            },
        }
    }
}
