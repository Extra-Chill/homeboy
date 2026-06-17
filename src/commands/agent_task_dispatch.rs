use clap::Args;
use serde::Serialize;
use serde_json::Value;

use homeboy::core::agent_tasks::dispatch_service::{self, AgentTaskDispatchRequest};
use homeboy::core::agent_tasks::provider::{
    default_backend_for_component, ExtensionProviderAgentTaskExecutor,
};
use homeboy::core::agent_tasks::scheduler::{AgentTaskAggregate, AgentTaskExecutorAdapter};
use homeboy::core::agent_tasks::AgentTaskAggregateReport;

use super::{CmdResult, GlobalArgs};

#[derive(Args, Debug, Clone)]
pub struct DispatchArgs {
    /// Inline prompt, @file, or - for stdin. Repeat --task for waves.
    #[arg(long, value_name = "PROMPT")]
    pub prompt: Option<String>,

    /// Additional task prompt, @file, or - for stdin. Each --task creates one minion cell.
    #[arg(long = "task", value_name = "PROMPT")]
    pub tasks: Vec<String>,

    /// JSON array/object of task prompts for waves. Supports @file and - for stdin.
    #[arg(long = "tasks", value_name = "JSON")]
    pub tasks_json: Option<String>,

    /// Existing local repo checkout or worktree path to cook in.
    #[arg(long, value_name = "PATH")]
    pub cwd: Option<String>,

    /// Homeboy workspace ID or existing local workspace path to cook in.
    #[arg(long, value_name = "ID_OR_PATH")]
    pub workspace: Option<String>,

    /// Repo/component slug for metadata and task grouping, e.g. data-machine.
    #[arg(long, value_name = "REPO")]
    pub repo: Option<String>,

    /// Issue, PR, or tracker URL the task is cooking.
    #[arg(long, value_name = "URL")]
    pub task_url: Option<String>,

    /// Executor backend to request. Defaults to the configured coding backend.
    #[arg(long, value_name = "BACKEND")]
    pub backend: Option<String>,

    /// Optional provider selector/id when more than one backend provider exists.
    #[arg(long, value_name = "SELECTOR")]
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

    /// Provider config JSON object, @file, or - for stdin. Merged with workspace metadata.
    #[arg(long = "provider-config", value_name = "JSON")]
    pub provider_config: Option<String>,

    /// Opaque client context JSON object, @file, or - for stdin.
    #[arg(long = "client-context", value_name = "JSON")]
    pub client_context: Option<String>,

    /// Maximum number of task cells to run at once.
    #[arg(long, default_value_t = 1, value_name = "N")]
    pub concurrency: usize,

    /// Attempts per task, including the first attempt.
    #[arg(long, default_value_t = 1, value_name = "N")]
    pub attempts: u32,

    /// Optional durable run id. Generated when omitted.
    #[arg(long, value_name = "ID")]
    pub run_id: Option<String>,

    /// Persist the run for a daemon/runner but do not execute immediately.
    #[arg(long)]
    pub queue_only: bool,
}

pub fn run(args: DispatchArgs, _global: &GlobalArgs) -> CmdResult<Value> {
    dispatch_with_executor(args, ExtensionProviderAgentTaskExecutor::discover())
}

pub(crate) fn cook(args: DispatchArgs, _global: &GlobalArgs) -> CmdResult<Value> {
    cook_with_executor(args, ExtensionProviderAgentTaskExecutor::discover())
}

fn cook_with_executor<E>(args: DispatchArgs, executor: E) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter,
{
    let target_worktree = args.workspace.clone();
    let (mut value, exit_code) = dispatch_with_executor(args, executor)?;
    value["handoff"] = cook_handoff(&value, target_worktree.as_deref());
    Ok((value, exit_code))
}

pub(crate) fn dispatch_with_executor<E>(args: DispatchArgs, executor: E) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter,
{
    let result = dispatch_service::dispatch(dispatch_request_from_args(args)?, executor)?;
    Ok((command_json_value(result.value)?, result.exit_code))
}

pub(crate) fn dispatch_request_from_args(
    args: DispatchArgs,
) -> homeboy::core::Result<AgentTaskDispatchRequest> {
    dispatch_request_from_args_with_default(args, default_backend_for_component)
}

fn dispatch_request_from_args_with_default(
    args: DispatchArgs,
    default_backend: impl FnOnce(Option<&str>) -> homeboy::core::Result<Option<String>>,
) -> homeboy::core::Result<AgentTaskDispatchRequest> {
    let backend = match args.backend {
        Some(backend) => backend,
        None => default_backend(args.repo.as_deref())?.ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "backend",
                "agent-task dispatch requires --backend because no default backend policy is configured",
                None,
                Some(vec![
                    "Set agent_task.default_backend in component, extension, or Homeboy config policy, or pass --backend explicitly.".to_string(),
                ]),
            )
        })?,
    };

    Ok(AgentTaskDispatchRequest {
        prompt: args.prompt,
        tasks: args.tasks,
        tasks_json: args.tasks_json,
        cwd: args.cwd,
        workspace: args.workspace,
        repo: args.repo,
        task_url: args.task_url,
        backend,
        selector: args.selector,
        model: args.model,
        required_capabilities: args.required_capabilities,
        secret_env: args.secret_env,
        provider_config: args.provider_config,
        client_context: args.client_context,
        concurrency: args.concurrency,
        attempts: args.attempts,
        run_id: args.run_id,
        queue_only: args.queue_only,
    })
}

fn command_json_value<T: Serialize>(value: T) -> homeboy::core::Result<Value> {
    serde_json::to_value(value)
        .map_err(|error| homeboy::core::Error::internal_json(error.to_string(), None))
}

fn cook_handoff(value: &Value, to_worktree: Option<&str>) -> Value {
    let run_id = value["run_id"].as_str().unwrap_or("<run-id>");
    let aggregate_path = value["aggregate_path"].as_str().unwrap_or(run_id);
    let aggregate_review = value
        .get("aggregate")
        .and_then(|aggregate| serde_json::from_value::<AgentTaskAggregate>(aggregate.clone()).ok())
        .map(|aggregate| AgentTaskAggregateReport::from(aggregate.outcomes))
        .unwrap_or_else(|| AgentTaskAggregateReport::from(Vec::new()));
    let promotion_commands =
        cook_promotion_commands(aggregate_path, to_worktree, &aggregate_review);
    let patch_artifact_produced = !promotion_commands.is_empty();
    let mut next_actions = Vec::new();

    if patch_artifact_produced {
        next_actions.push("patch artifact produced; promote one candidate before claiming worktree changes or PR completion".to_string());
        if to_worktree.is_some() {
            next_actions.push(
                "run one `promote_commands` entry to apply the patch into the target worktree"
                    .to_string(),
            );
        } else {
            next_actions.push(format!(
                "run `homeboy agent-task review {run_id} --to-worktree <handle>` to generate exact promote commands"
            ));
        }
        next_actions.push(format!(
            "after promotion and verification, run `homeboy agent-task finalize-pr --run-id {run_id} --path <promoted-worktree-path> --title <title> --commit-message <message>`"
        ));
    } else if value["queued"].as_bool().unwrap_or(false) {
        next_actions.push(format!(
            "queued only; run `homeboy agent-task run {run_id}` or let a daemon claim it with `homeboy agent-task run-next`"
        ));
    } else {
        next_actions.push("no patch artifact was produced; inspect `aggregate` candidates before retrying or reporting".to_string());
    }

    serde_json::json!({
        "schema": "homeboy/agent-task-cook-handoff/v1",
        "states": {
            "patch_artifact_produced": patch_artifact_produced,
            "patch_promoted": false,
            "pr_opened": false
        },
        "boundary": if value["queued"].as_bool().unwrap_or(false) {
            "queued"
        } else if patch_artifact_produced {
            "patch_artifact_only"
        } else {
            "no_patch_artifact"
        },
        "promote_commands": promotion_commands,
        "finalize_command": format!(
            "homeboy agent-task finalize-pr --run-id {run_id} --path <promoted-worktree-path> --title <title> --commit-message <message>"
        ),
        "next_actions": next_actions
    })
}

fn cook_promotion_commands(
    source: &str,
    to_worktree: Option<&str>,
    review: &AgentTaskAggregateReport,
) -> Vec<Vec<String>> {
    review
        .apply_candidates
        .iter()
        .flat_map(|candidate| {
            candidate.artifact_ids.iter().map(move |artifact_id| {
                let mut command = vec![
                    "homeboy".to_string(),
                    "agent-task".to_string(),
                    "promote".to_string(),
                    source.to_string(),
                    "--task-id".to_string(),
                    candidate.task_id.clone(),
                    "--artifact-id".to_string(),
                    artifact_id.clone(),
                ];
                if let Some(to_worktree) = to_worktree {
                    command.push("--to-worktree".to_string());
                    command.push(to_worktree.to_string());
                }
                command
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_isolated_home;
    use homeboy::core::agent_task::AgentTaskRequest;
    use homeboy::core::agent_tasks::scheduler::AgentTaskExecutionContext;
    use homeboy::core::agent_tasks::{
        AgentTaskArtifact, AgentTaskOutcome, AgentTaskOutcomeStatus, AGENT_TASK_ARTIFACT_SCHEMA,
        AGENT_TASK_OUTCOME_SCHEMA,
    };

    #[test]
    fn dispatch_adapter_preserves_queue_only_envelope_shape() {
        with_isolated_home(|_| {
            let workspace = tempfile::tempdir().expect("workspace");

            let (value, exit_code) = dispatch_with_executor(
                dispatch_args(DispatchArgOverrides {
                    prompt: Some("Queue this minion.".to_string()),
                    cwd: Some(workspace.path().display().to_string()),
                    queue_only: true,
                    run_id: Some("dispatch-queued".to_string()),
                    ..DispatchArgOverrides::default()
                }),
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
    fn cook_output_marks_patch_artifact_handoff_boundary() {
        with_isolated_home(|_| {
            let workspace = tempfile::tempdir()
                .expect("workspace")
                .keep()
                .display()
                .to_string();
            let patch_path = std::path::Path::new(&workspace).join("changes.patch");
            std::fs::write(&patch_path, "diff --git a/file b/file\n").expect("patch fixture");
            let (value, exit_code) = cook_with_executor(
                dispatch_args(DispatchArgOverrides {
                    prompt: Some("Cook a patch.".to_string()),
                    workspace: Some(workspace.clone()),
                    run_id: Some("cook-handoff".to_string()),
                    ..DispatchArgOverrides::default()
                }),
                PatchExecutor { patch_path },
            )
            .expect("cook run");

            assert_eq!(exit_code, 0);
            assert_eq!(value["handoff"]["states"]["patch_artifact_produced"], true);
            assert_eq!(value["handoff"]["states"]["patch_promoted"], false);
            assert_eq!(value["handoff"]["states"]["pr_opened"], false);
            assert_eq!(value["handoff"]["boundary"], "patch_artifact_only");
            assert_eq!(
                value["handoff"]["promote_commands"][0],
                serde_json::json!([
                    "homeboy",
                    "agent-task",
                    "promote",
                    value["aggregate_path"].as_str().expect("aggregate path"),
                    "--task-id",
                    "cook-task",
                    "--artifact-id",
                    "patch-1",
                    "--to-worktree",
                    workspace
                ])
            );
            assert!(value["handoff"]["finalize_command"]
                .as_str()
                .expect("finalize command")
                .contains("homeboy agent-task finalize-pr --run-id cook-handoff"));
        });
    }

    #[test]
    fn queued_cook_handoff_surfaces_run_command_without_patch_claims() {
        with_isolated_home(|_| {
            let (value, exit_code) = cook_with_executor(
                dispatch_args(DispatchArgOverrides {
                    prompt: Some("Queue this cook.".to_string()),
                    queue_only: true,
                    run_id: Some("cook-queued".to_string()),
                    ..DispatchArgOverrides::default()
                }),
                PatchExecutor {
                    patch_path: std::path::PathBuf::new(),
                },
            )
            .expect("queued cook");

            assert_eq!(exit_code, 0);
            assert_eq!(value["handoff"]["states"]["patch_artifact_produced"], false);
            assert_eq!(value["handoff"]["states"]["patch_promoted"], false);
            assert_eq!(value["handoff"]["states"]["pr_opened"], false);
            assert!(value["handoff"]["next_actions"][0]
                .as_str()
                .expect("next action")
                .contains("homeboy agent-task run cook-queued"));
        });
    }

    #[test]
    fn dispatch_request_uses_declared_default_backend_when_backend_is_absent() {
        let request = dispatch_request_from_args_with_default(
            DispatchArgs {
                prompt: Some("run with default".to_string()),
                tasks: Vec::new(),
                tasks_json: None,
                cwd: None,
                workspace: None,
                repo: None,
                task_url: None,
                backend: None,
                selector: None,
                model: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                provider_config: None,
                client_context: None,
                concurrency: 1,
                attempts: 1,
                run_id: None,
                queue_only: false,
            },
            |_| Ok(Some("fake-default".to_string())),
        )
        .expect("default backend request");

        assert_eq!(request.backend, "fake-default");
    }

    #[test]
    fn dispatch_request_errors_when_backend_and_default_are_absent() {
        let error = dispatch_request_from_args_with_default(
            DispatchArgs {
                prompt: Some("run without default".to_string()),
                tasks: Vec::new(),
                tasks_json: None,
                cwd: None,
                workspace: None,
                repo: None,
                task_url: None,
                backend: None,
                selector: None,
                model: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                provider_config: None,
                client_context: None,
                concurrency: 1,
                attempts: 1,
                run_id: None,
                queue_only: false,
            },
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

    struct PatchExecutor {
        patch_path: std::path::PathBuf,
    }

    impl AgentTaskExecutorAdapter for PatchExecutor {
        fn execute(
            &self,
            _request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            AgentTaskOutcome {
                schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: "cook-task".to_string(),
                status: AgentTaskOutcomeStatus::Succeeded,
                summary: Some("patch ready".to_string()),
                failure_classification: None,
                artifacts: vec![AgentTaskArtifact {
                    schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                    id: "patch-1".to_string(),
                    kind: "patch".to_string(),
                    name: Some("changes.patch".to_string()),
                    path: Some(self.patch_path.display().to_string()),
                    url: None,
                    mime: None,
                    size_bytes: std::fs::metadata(&self.patch_path)
                        .ok()
                        .map(|metadata| metadata.len()),
                    sha256: None,
                    metadata: Value::Null,
                }],
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
        tasks_json: Option<String>,
        cwd: Option<String>,
        workspace: Option<String>,
        repo: Option<String>,
        task_url: Option<String>,
        secret_env: Vec<String>,
        provider_config: Option<String>,
        client_context: Option<String>,
        concurrency: usize,
        attempts: u32,
        queue_only: bool,
        run_id: Option<String>,
        backend: Option<String>,
    }

    fn dispatch_args(overrides: DispatchArgOverrides) -> DispatchArgs {
        DispatchArgs {
            prompt: overrides.prompt,
            tasks: Vec::new(),
            tasks_json: overrides.tasks_json,
            cwd: overrides.cwd,
            workspace: overrides.workspace,
            repo: overrides.repo,
            task_url: overrides.task_url,
            backend: Some(overrides.backend.unwrap_or_else(|| "fixture".to_string())),
            selector: None,
            model: None,
            required_capabilities: Vec::new(),
            secret_env: overrides.secret_env,
            provider_config: overrides.provider_config,
            client_context: overrides.client_context,
            concurrency: if overrides.concurrency == 0 {
                1
            } else {
                overrides.concurrency
            },
            attempts: if overrides.attempts == 0 {
                1
            } else {
                overrides.attempts
            },
            run_id: overrides.run_id,
            queue_only: overrides.queue_only,
        }
    }
}
