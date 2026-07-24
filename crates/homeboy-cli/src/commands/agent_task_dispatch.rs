use clap::Args;

use homeboy::agents::agent_tasks::dispatch_service::{
    AgentTaskDispatchCommand, DispatchCoreInputs,
};

/// Provider dispatch inputs: how the agent task is provisioned and how many
/// times it may run. Controls the task prompts, provider/client context, and
/// the execution/retry budget shared by every dispatch entrypoint.
//
// Implementation note (not shown in `--help`): this group is flattened into
// `DispatchArgs` so the `--tasks/--provider-config/--client-context/--attempts/
// --queue-only` flags are declared once and stay identical across carriers
// (#5187).
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

    /// Maximum total provider executions per task, including same-provider
    /// retries and provider rotations. `--attempts 1` runs exactly once.
    #[arg(
        long = "max-provider-executions",
        alias = "attempts",
        default_value_t = 1,
        value_name = "N"
    )]
    pub attempts: u32,

    /// Same-provider retries allowed after the first provider execution.
    #[arg(
        long = "max-same-provider-retries",
        alias = "same-provider-retries",
        default_value_t = 0,
        value_name = "N"
    )]
    pub same_provider_retries: u32,

    /// Cross-provider rotations allowed after the first provider execution.
    #[arg(
        long = "max-provider-rotations",
        alias = "provider-rotations",
        default_value_t = 0,
        value_name = "N"
    )]
    pub provider_rotations: u32,

    /// Persist the run for a daemon/runner but do not execute immediately.
    #[arg(long)]
    pub queue_only: bool,

    /// Provider wall-clock timeout in milliseconds. Defaults to Homeboy's provider timeout.
    #[arg(long = "timeout-ms", value_name = "MS")]
    pub timeout_ms: Option<u64>,

    #[arg(
        long = "resolved-provider-policy",
        hide = true,
        value_name = "JSON",
        value_parser = parse_resolved_provider_policy
    )]
    pub resolved_provider_policy:
        Option<homeboy::agents::agent_task_dispatch_service::ResolvedAgentTaskProviderPolicy>,
}

fn parse_resolved_provider_policy(
    raw: &str,
) -> Result<homeboy::agents::agent_task_dispatch_service::ResolvedAgentTaskProviderPolicy, String> {
    serde_json::from_str(raw)
        .map_err(|error| format!("invalid resolved provider policy JSON: {error}"))
}

impl From<DispatchCoreArgs> for DispatchCoreInputs {
    fn from(args: DispatchCoreArgs) -> Self {
        DispatchCoreInputs {
            tasks_json: args.tasks_json,
            provider_config: args.provider_config,
            client_context: args.client_context,
            attempts: args.attempts,
            same_provider_retries: args.same_provider_retries,
            provider_rotations: args.provider_rotations,
            queue_only: args.queue_only,
            timeout_ms: args.timeout_ms,
            resolved_provider_policy: args.resolved_provider_policy,
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
            task_id: None,
            core: args.core.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_isolated_home;
    use homeboy::agents::agent_task::AgentTaskRequest;
    use homeboy::agents::agent_task_dispatch_service::ResolvedAgentTaskProviderPolicy;
    use homeboy::agents::agent_task_prompts;
    use homeboy::agents::agent_tasks::dispatch_service;
    use homeboy::agents::agent_tasks::scheduler::AgentTaskPlan;
    use homeboy::agents::agent_tasks::scheduler::{
        AgentTaskExecutionContext, AgentTaskExecutorAdapter,
    };
    use homeboy::agents::agent_tasks::{
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
    fn dispatch_adapter_resolves_stored_prompt_reference_before_queueing_plan() {
        with_isolated_home(|_| {
            agent_task_prompts::save_prompt("homeboy-7388", "Cook from stored prompt.")
                .expect("save prompt");

            let (value, exit_code) = dispatch_service::run_dispatch_command(
                dispatch_args(DispatchArgOverrides {
                    prompt: Some("@prompt:homeboy-7388".to_string()),
                    run_id: Some("dispatch-stored-prompt".to_string()),
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
            let plan_path = value["plan_path"].as_str().expect("plan path");
            let raw = std::fs::read_to_string(plan_path).expect("read queued plan");
            let plan: AgentTaskPlan = serde_json::from_str(&raw).expect("decode queued plan");

            assert_eq!(plan.tasks[0].instructions, "Cook from stored prompt.");
            assert_eq!(
                plan.tasks[0].metadata["prompt_source"],
                "@prompt:homeboy-7388"
            );
            assert_eq!(
                plan.tasks[0].metadata["prompt_store"]["reference"],
                "prompt:homeboy-7388"
            );
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
                    same_provider_retries: 0,
                    provider_rotations: 0,
                    queue_only: false,
                    timeout_ms: None,
                    resolved_provider_policy: None,
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
                    same_provider_retries: 0,
                    provider_rotations: 0,
                    queue_only: false,
                    timeout_ms: None,
                    resolved_provider_policy: None,
                },
            }
            .into(),
            |_| Ok(None),
        )
        .expect_err("missing backend should fail");

        assert!(error.message.contains("requires --backend"));
    }

    #[test]
    fn submitted_provider_policy_ignores_runner_default_backend() {
        let mut args = dispatch_args(DispatchArgOverrides::default());
        args.backend = None;
        args.model = Some("controller-model".to_string());
        args.required_capabilities = vec!["runner-capability".to_string()];
        args.secret_env = vec!["RUNNER_SECRET".to_string()];
        args.core.resolved_provider_policy = Some(ResolvedAgentTaskProviderPolicy {
            backend: "controller-backend".to_string(),
            selector: Some("controller-selector".to_string()),
            model: Some("controller-model".to_string()),
            rotation: None,
            rotation_starts_with_first_entry: false,
            retry: Default::default(),
            liveness_timeout_ms: None,
            runtime_identity: None,
        });

        let request = dispatch_service::resolve_dispatch_request_with_default(args.into(), |_| {
            Ok(Some("runner-backend".to_string()))
        })
        .expect("submitted policy request");

        assert_eq!(request.backend, "controller-backend");
        assert_eq!(request.selector.as_deref(), Some("controller-selector"));
        assert_eq!(request.model.as_deref(), Some("controller-model"));
        assert_eq!(request.required_capabilities, ["runner-capability"]);
        assert_eq!(request.secret_env, ["RUNNER_SECRET"]);
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
                same_provider_retries: overrides.core.same_provider_retries,
                provider_rotations: overrides.core.provider_rotations,
                queue_only: overrides.core.queue_only,
                timeout_ms: overrides.core.timeout_ms,
                resolved_provider_policy: overrides.core.resolved_provider_policy,
            },
        }
    }
}
