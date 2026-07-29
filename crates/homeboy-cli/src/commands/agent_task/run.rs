//! Durable run lifecycle handlers: cook, run-plan, run, run-next, submit,
//! resume, and retry.

use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use homeboy::agents::agent_tasks::dispatch_service;
use homeboy::agents::agent_tasks::lifecycle as agent_task_lifecycle;
use homeboy::agents::agent_tasks::provider::ExtensionProviderAgentTaskExecutor;
use homeboy::agents::agent_tasks::scheduler::{
    AgentTaskAggregate, AgentTaskExecutorAdapter, AgentTaskPlan,
};
use homeboy::agents::agent_tasks::service as agent_task_service;
use homeboy::core::command_invocation::CommandInvocation;
use homeboy::core::defaults;
use homeboy::core::worktree_providers::{
    provision_apply_enabled_worktree_provider_from_config, WorktreeProviderCreateIntent,
};

use super::super::agent_task_dispatch::DispatchArgs;
use super::super::CmdResult;
use super::args::{
    AgentTaskCookArgs, CookContinueArgs, PromotionProviderArgs, RetryArgs, RunArgs, RunPlanArgs,
    StatusArgs, SubmitArgs,
};

const MAX_PROMOTION_PROVIDER_REQUEST_BYTES: u64 = 16 * 1024 * 1024;

/// Operator-facing durable identity block for a Cook that has just become
/// addressable.
///
/// A Cook's run id is the only handle an interrupted caller has. Everything
/// after durable submission — gate toolchain preflight, transport preparation,
/// Lab materialization, provider execution — can outlive a client timeout, so
/// the handle and its follow-up commands are assembled here and printed the
/// first time the controller reports identity (#10419, #9163).
pub(crate) fn durable_cook_identity_lines(cook_id: Option<&str>, run_id: &str) -> Vec<String> {
    let cook_suffix = cook_id
        .filter(|cook_id| *cook_id != run_id)
        .map(|cook_id| format!(" (cook `{cook_id}`)"))
        .unwrap_or_default();
    vec![
        format!("cook: durable run id `{run_id}`{cook_suffix} — persisted before materialization."),
        format!("cook: status -> homeboy agent-task status {run_id}"),
        format!("cook: logs   -> homeboy agent-task logs {run_id}"),
        format!("cook: cancel -> homeboy agent-task cancel {run_id}"),
    ]
}

/// Print the durable identity block unconditionally.
///
/// Deliberately not TTY-gated. The clients that most need this — agent/Discord
/// bridges, CI, and anything invoking Homeboy as a tool call — are exactly the
/// non-TTY callers that a TTY-gated status line silently skips, which is how an
/// interrupted Lab Cook ended up with no reported task identity at all (#10419).
pub(crate) fn announce_durable_cook_identity(cook_id: Option<&str>, run_id: &str) {
    for line in durable_cook_identity_lines(cook_id, run_id) {
        eprintln!("{line}");
    }
}

/// Serialize a completed run aggregate and, when the run did not fully succeed,
/// surface a prominent top-level `failure_reasons` summary so the operator sees
/// the root cause (recipe validation, PHP fatal, provider registration, missing
/// path) without hand-digging the nested outcome JSON (#3806). The full nested
/// payload is preserved unchanged; this only ADDS the surfaced summary.
fn aggregate_value_with_failure_reasons(aggregate: &AgentTaskAggregate) -> Value {
    let aggregate = homeboy::agents::agent_task_artifacts::reviewer_facing_aggregate(aggregate);
    let mut value = serde_json::to_value(&aggregate).unwrap_or(Value::Null);
    let failure_reasons = super::status::failure_reasons_from_aggregate(&aggregate);
    if !failure_reasons.is_empty() {
        if let Value::Object(map) = &mut value {
            map.insert("failure_reasons".to_string(), Value::Array(failure_reasons));
        }
    }
    value
}

pub(crate) fn run_cook(args: AgentTaskCookArgs) -> CmdResult<Value> {
    validate_cook_request(&args)?;
    run_cook_with_executor(args, ExtensionProviderAgentTaskExecutor::discover())
}

/// Resume a Cook from its immutable recipe rather than asking the operator to
/// replay prompt, provider, gate, workspace, or disclosure arguments.
pub(crate) fn continue_cook(args: CookContinueArgs) -> CmdResult<Value> {
    let recipe =
        agent_task_service::load_recipe(&args.cook_or_attempt_id).or_else(|cook_error| {
            agent_task_service::load_recipe_for_attempt(&args.cook_or_attempt_id)?.ok_or(cook_error)
        })?;
    let run_id = agent_task_service::resolve_cook_continuation_run_id(&args.cook_or_attempt_id)?;
    let record = agent_task_service::reconcile_recipe_attempt_for_continuation(&recipe, &run_id)?;
    if !matches!(
        record.state,
        agent_task_lifecycle::AgentTaskRunState::Succeeded
            | agent_task_lifecycle::AgentTaskRunState::CandidateRecoverable
            | agent_task_lifecycle::AgentTaskRunState::PartialRecoverable
            | agent_task_lifecycle::AgentTaskRunState::PartialFailure
            | agent_task_lifecycle::AgentTaskRunState::Failed
            | agent_task_lifecycle::AgentTaskRunState::Cancelled
    ) {
        return Ok((
            cook_continuation_status(&recipe.cook_id, &run_id, &format!("{:?}", record.state)),
            0,
        ));
    }

    if matches!(
        record.state,
        agent_task_lifecycle::AgentTaskRunState::Succeeded
            | agent_task_lifecycle::AgentTaskRunState::CandidateRecoverable
            | agent_task_lifecycle::AgentTaskRunState::PartialRecoverable
    ) {
        // Explicit recovery claims the exact Cook without exposing it to the
        // generic daemon queue between rearm and execution.
        let Some(claim) =
            agent_task_service::claim_continuation_for_recovery(&recipe.cook_id, &run_id)?
        else {
            return Ok((
                cook_continuation_pending(&recipe.cook_id, &run_id, &format!("{:?}", record.state)),
                0,
            ));
        };
        let mut result = None;
        let historical_terminal =
            recipe.runtime_generation != homeboy::core::build_identity::current().display;
        let dispatcher = |dispatch_recipe: &Value| {
            crate::commands::infra::route::reconstruct_cook_attempt_dispatcher(dispatch_recipe)
        };
        let execute = |options| {
            let executor = ExtensionProviderAgentTaskExecutor::discover();
            let cook = if historical_terminal {
                agent_task_service::run_terminal_cook_continuation(options, executor)?
            } else {
                agent_task_service::run_cook(options, executor)?
            };
            let exit_code = cook.exit_code;
            result = Some(cook.value);
            Ok(exit_code)
        };
        let exit_code = if historical_terminal {
            agent_task_service::consume_claimed_terminal_with_dispatcher(
                claim, dispatcher, execute,
            )?
        } else {
            agent_task_service::consume_claimed_with_dispatcher(claim, dispatcher, execute)?
        };
        let value = cook_report_with_continuation(
            serde_json::to_value(result.ok_or_else(|| {
                homeboy::core::Error::internal_unexpected(
                    "claimed Cook continuation returned no result",
                )
            })?)
            .unwrap_or(Value::Null),
        );
        return Ok((
            super::status::compact_cook_report(value, args.full),
            exit_code,
        ));
    }

    let dispatcher = crate::commands::infra::route::reconstruct_cook_attempt_dispatcher(
        &recipe.promotion_transport["attempt_dispatch"],
    )?;
    let mut options = agent_task_service::reconstruct_options_with_dispatcher(&recipe, dispatcher)?;
    let attempt = recipe
        .attempts
        .iter()
        .find(|attempt| attempt.run_id == run_id)
        .ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "cook_or_attempt_id",
                "selected attempt is absent from its durable Cook recipe",
                Some(run_id.clone()),
                None,
            )
        })?;
    options.initial_run_id = attempt.run_id.clone();
    options.initial_plan = attempt.plan.clone();
    let result =
        agent_task_service::run_cook(options, ExtensionProviderAgentTaskExecutor::discover())?;
    let value =
        cook_report_with_continuation(serde_json::to_value(result.value).unwrap_or(Value::Null));
    Ok((
        super::status::compact_cook_report(value, args.full),
        result.exit_code,
    ))
}

fn cook_continuation_pending(cook_id: &str, run_id: &str, provider_state: &str) -> Value {
    serde_json::json!({
        "schema": "homeboy/agent-task-cook/v1",
        "cook_id": cook_id,
        "latest_run_id": run_id,
        "status": "continuation_pending",
        "provider": { "state": provider_state, "run_id": run_id },
        "remaining_phases": ["harvest", "review", "gates", "promotion", "finalization"],
        "continuation_command": format!("homeboy agent-task cook-continue {run_id}"),
    })
}

fn cook_continuation_status(cook_id: &str, run_id: &str, provider_state: &str) -> Value {
    serde_json::json!({
        "schema": "homeboy/agent-task-cook/v1",
        "cook_id": cook_id,
        "latest_run_id": run_id,
        "status": "in_flight",
        "provider": { "state": provider_state, "run_id": run_id },
        "remaining_phases": ["harvest", "review", "gates", "promotion", "finalization"],
        "continuation_command": format!("homeboy agent-task cook-continue {run_id}"),
    })
}

fn cook_report_with_continuation(mut value: Value) -> Value {
    if value.get("status").and_then(Value::as_str) != Some("in_flight") {
        return value;
    }
    let run_id = value
        .get("latest_run_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let provider_state = value
        .get("attempts")
        .and_then(Value::as_array)
        .and_then(|attempts| attempts.last())
        .and_then(|attempt| attempt.get("run_state"))
        .cloned()
        .unwrap_or(Value::Null);
    if let Value::Object(report) = &mut value {
        report.insert(
            "provider".to_string(),
            serde_json::json!({ "state": provider_state, "run_id": run_id }),
        );
        report.insert(
            "remaining_phases".to_string(),
            serde_json::json!(["harvest", "review", "gates", "promotion", "finalization"]),
        );
        report.insert(
            "continuation_command".to_string(),
            serde_json::json!(format!("homeboy agent-task cook-continue {run_id}")),
        );
    }
    value
}

/// Converge a Cook promotion destination before compiling a task plan. This is
/// controller-owned so local and Lab dispatch use the same managed checkout.
pub(crate) fn provision_cook_destination(args: &AgentTaskCookArgs) -> homeboy::core::Result<Value> {
    let direct_path = Path::new(&args.to_worktree);
    if direct_path.is_dir() {
        homeboy::core::worktree_providers::validate_task_worktree_root(
            direct_path,
            &args.to_worktree,
        )?;
        let path = std::fs::canonicalize(direct_path).map_err(|error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some(direct_path.display().to_string()),
            )
        })?;
        return Ok(serde_json::json!({
            "action": "existing",
            "kind": "direct_task_worktree",
            "handle": args.to_worktree,
            "path": path,
        }));
    }
    if let Some(record) =
        homeboy::core::worktree::resolve_workspace_ref_if_present(&args.to_worktree)?
    {
        if record.state() != &homeboy::core::worktree::TaskWorktreeState::Active {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "to_worktree",
                format!(
                    "Homeboy workspace `{}` is no longer active",
                    record.handle()
                ),
                Some(args.to_worktree.clone()),
                None,
            ));
        }
        let path = PathBuf::from(record.path());
        homeboy::core::worktree_providers::validate_task_worktree_root(&path, &args.to_worktree)?;
        return Ok(
            serde_json::json!({ "action": "existing", "kind": record.source_kind(), "handle": args.to_worktree, "path": path }),
        );
    }

    let config = defaults::load_config();
    match homeboy::core::worktree_providers::resolve_apply_enabled_worktree_provider_from_config(
        &args.to_worktree,
        &config,
        None,
    ) {
        Ok(resolution) => {
            homeboy::core::worktree_providers::validate_task_worktree_root(
                Path::new(&resolution.worktree.path),
                &args.to_worktree,
            )?;
            return Ok(
                serde_json::json!({ "action": "existing", "kind": "provider", "provider": resolution.provider_id, "handle": resolution.worktree.handle, "path": resolution.worktree.path, "branch": resolution.worktree.branch }),
            );
        }
        Err(error)
            if error
                .details
                .get("worktree_provider_lookup")
                .and_then(Value::as_str)
                == Some("not_found") => {}
        Err(error) => return Err(error),
    }

    let repo = args.dispatch.repo.clone().ok_or_else(|| {
        homeboy::core::Error::validation_missing_argument(vec![
            "--repo <repo> is required to create a missing --to-worktree destination".to_string(),
        ])
    })?;
    let head = args.head.clone().ok_or_else(|| {
        homeboy::core::Error::validation_missing_argument(vec![
            "--head <branch> is required to create a missing --to-worktree destination".to_string(),
        ])
    })?;
    let task_url = args.dispatch.task_url.clone().ok_or_else(|| {
        homeboy::core::Error::validation_missing_argument(vec![
            "--task-url <url> is required to create a missing --to-worktree destination"
                .to_string(),
        ])
    })?;
    provision_apply_enabled_worktree_provider_from_config(
        &WorktreeProviderCreateIntent {
            handle: args.to_worktree.clone(),
            repo,
            base: args.base.clone(),
            head,
            task_url,
        },
        &config,
    )
    .map(|provision| {
        serde_json::json!({
            "action": provision.action,
            "provider": provision.resolution.provider_id,
            "idempotency_key": provision.idempotency_key,
            "handle": provision.resolution.worktree.handle,
            "path": provision.resolution.worktree.path,
            "branch": provision.resolution.worktree.branch,
        })
    })
}

pub(crate) fn record_cook_provision(plan: &mut AgentTaskPlan, provision: Value) {
    if let Some(task) = plan.tasks.first_mut() {
        if !task.metadata.is_object() {
            task.metadata = serde_json::json!({});
        }
        task.metadata["worktree_provision"] = provision;
    }
}

pub(crate) fn validate_cook_request(args: &AgentTaskCookArgs) -> homeboy::core::Result<()> {
    if args.goal.is_some() && !args.dispatch.tasks.is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "task",
            "agent-task cook uses --goal as framing metadata and --prompt as its one source; --goal and --task conflict",
            None,
            Some(vec![
                "Use: homeboy agent-task cook --goal 'Describe the outcome' --prompt @task.txt --to-worktree sample-plugin@fix-issue --verify 'npm test'".to_string(),
            ]),
        ));
    }
    let dispatch = dispatch_args_for_cook(args);
    dispatch_service::validate_single_cook_prompt_source(
        dispatch.prompt.as_deref(),
        &dispatch.tasks,
        dispatch.core.tasks_json.as_deref(),
    )?;
    if !args.gates.has_deterministic_gate() && !args.no_finalize {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "verify",
            "agent-task cook requires at least one deterministic --verify or --private-verify gate before it can commit, push, and open a PR",
            None,
            Some(vec!["Provide a deterministic verification gate, e.g. --verify \"cargo test\".".to_string()]),
        ));
    }
    if args.dispatch.core.queue_only {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "queue-only",
            "agent-task cook cannot queue its controller-owned lifecycle",
            None,
            None,
        ));
    }
    Ok(())
}

pub(crate) fn promotion_provider(args: PromotionProviderArgs) -> CmdResult<Value> {
    let mut request = Vec::new();
    std::io::stdin()
        .take(MAX_PROMOTION_PROVIDER_REQUEST_BYTES + 1)
        .read_to_end(&mut request)
        .map_err(|error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some("read agent-task promotion provider request".to_string()),
            )
        })?;
    if request.len() as u64 > MAX_PROMOTION_PROVIDER_REQUEST_BYTES {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "promotion-provider request",
            format!(
                "promotion provider request exceeds {MAX_PROMOTION_PROVIDER_REQUEST_BYTES} bytes"
            ),
            None,
            None,
        ));
    }
    let request = String::from_utf8(request).map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "promotion-provider request",
            format!("promotion provider request is not UTF-8: {error}"),
            None,
            None,
        )
    })?;
    homeboy::agents::agent_task_promotion::apply_materialized_workspace_patch(
        Path::new(&args.workspace),
        &request,
    )
    .and_then(|response| {
        serde_json::from_str(&response).map_err(|error| {
            homeboy::core::Error::internal_json(
                error.to_string(),
                Some("serialize agent-task promotion provider response".to_string()),
            )
        })
    })
    .map(|value| (value, 0))
}

pub(super) fn run_cook_with_executor<E>(args: AgentTaskCookArgs, executor: E) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    run_cook_with_executor_and_dispatcher(args, executor, None)
}

pub(crate) fn run_cook_with_executor_and_dispatcher<E>(
    args: AgentTaskCookArgs,
    executor: E,
    attempt_dispatcher: Option<
        Arc<dyn crate::agents::agent_task_service::AgentTaskCookAttemptDispatcher>,
    >,
) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    run_cook_with_executor_and_dispatcher_with_progress(args, executor, attempt_dispatcher, None)
}

pub(crate) fn run_cook_with_executor_and_dispatcher_with_progress<E>(
    args: AgentTaskCookArgs,
    executor: E,
    attempt_dispatcher: Option<
        Arc<dyn crate::agents::agent_task_service::AgentTaskCookAttemptDispatcher>,
    >,
    progress: Option<
        &(dyn Fn(&str, Option<&str>, Option<&str>) -> homeboy::core::Result<()> + Send + Sync),
    >,
) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    validate_cook_request(&args)?;
    // Deterministic gates exist to make *publication* safe: a green gate is the
    // proof a cook may commit, push, and open a PR. A `--no-finalize` cook does
    // none of those, so a gate is not meaningful there — read-only/exploratory
    // cooks legitimately have nothing to verify. The promotion lifecycle already
    // treats an empty gate set as a vacuously-green run
    // (`run_promotion_gates` -> `PromotionGateRun::without_gates`), so relaxing
    // the requirement here is safe end to end (#7608). Finalizing cooks still
    // require a gate, but now say so with a copy-pasteable example instead of a
    // bare rejection.
    let provision = provision_cook_destination(&args)?;

    let mut dispatch_args = dispatch_args_for_cook(&args);
    let requested_cook_id = dispatch_args.run_id.clone();
    if let Some(cook_id) = requested_cook_id.as_deref() {
        dispatch_args.run_id = Some(
            args.attempt_run_id
                .clone()
                .unwrap_or_else(|| agent_task_lifecycle::cook_attempt_run_id(cook_id, 1)),
        );
    }
    let run_id = dispatch_args
        .run_id
        .clone()
        .unwrap_or_else(|| format!("agent-task-{}", uuid::Uuid::new_v4()));
    dispatch_args.run_id = Some(run_id.clone());
    let cook_id = requested_cook_id.clone().unwrap_or_else(|| run_id.clone());
    if let Some(progress) = progress {
        progress("preparing", None, None)?;
    }
    let (run_id, mut initial_plan) = if let Some(attempt_plan) = args.attempt_plan.as_deref() {
        let run_id = dispatch_args.run_id.clone().ok_or_else(|| {
            homeboy::core::Error::internal_unexpected(
                "agent-task cook attempt plan requires an attempt run id".to_string(),
            )
        })?;
        (run_id, agent_task_service::read_plan(attempt_plan)?)
    } else {
        let plan = compile_cook_plan(&args, provision.clone())?;
        (run_id, plan)
    };
    if args.attempt_plan.is_some() {
        record_cook_provision(&mut initial_plan, provision);
        record_cook_goal(&mut initial_plan, args.goal.as_deref());
    }
    // Capture the resolved task workspace before dispatch. The provider may
    // commit and leave a clean tree, so resolving this after it runs would
    // silently widen the promotion range.
    let source_worktree_path = initial_plan
        .tasks
        .first()
        .and_then(|task| task.workspace.root.as_ref())
        .map(std::path::PathBuf::from);
    let task_base_sha = source_worktree_path.as_deref().and_then(git_head_sha);
    let title = args
        .title
        .clone()
        .unwrap_or_else(|| default_loop_title(&args));
    let commit_message = args
        .commit_message
        .clone()
        .unwrap_or_else(|| default_loop_commit_message(&args));
    let selected_model = initial_plan
        .tasks
        .first()
        .and_then(|task| task.executor.model())
        .map(str::to_string);
    let durable_observer = |phase: &str, cook_id: &str, run_id: &str| {
        progress
            .map(|progress| progress(phase, Some(cook_id), Some(run_id)))
            .unwrap_or(Ok(()))
    };
    let result = agent_task_service::run_cook_with_durable_observer(
        agent_task_service::AgentTaskCookServiceOptions {
            cook_id,
            initial_run_id: run_id,
            initial_plan,
            to_worktree: args.to_worktree,
            source_worktree_path,
            provider_command: args.provider_command,
            provider_invocation: (!args.provider_argv.is_empty()).then(|| CommandInvocation {
                argv: args.provider_argv,
                ..Default::default()
            }),
            gates: args.gates.into(),
            max_attempts: args.max_attempts,
            no_finalize: args.no_finalize,
            base: args.base,
            task_base_sha,
            head: args.head,
            title,
            commit_message,
            source_refs: args.dispatch.task_url.into_iter().collect(),
            protected_branches: args.protected_branches,
            ai_tool: super::fanout::resolve_ai_tool_disclosure(
                &args.ai_tool,
                args.dispatch.backend.as_deref(),
                args.dispatch.selector.as_deref(),
                args.dispatch.model.as_deref(),
            ),
            // Model identity comes only from explicit/config/rotation selection
            // (`--model`, provider profile). Disclosure text like
            // `OpenCode (GPT-5.5)` is presentation, not execution provenance, and
            // must not be reverse-parsed into a model — an omitted model stays
            // omitted so finalization records only real model identity (#9789).
            ai_model: selected_model,
            ai_used_for: args.ai_used_for,
            attempt_dispatcher,
            harvest_context:
                homeboy::agents::agent_task_scheduler::HarvestExecutionContext::from_current_process(
                )?,
        },
        executor,
        &durable_observer,
    )?;
    Ok((
        super::status::compact_cook_report(
            cook_report_with_continuation(
                serde_json::to_value(result.value).unwrap_or(Value::Null),
            ),
            args.full,
        ),
        result.exit_code,
    ))
}

pub(super) fn dispatch_args_for_cook(args: &AgentTaskCookArgs) -> DispatchArgs {
    let mut dispatch_args = args.dispatch.clone();
    let has_explicit_work = dispatch_args.prompt.is_some()
        || !dispatch_args.tasks.is_empty()
        || dispatch_args.core.tasks_json.is_some();
    if !has_explicit_work {
        dispatch_args.prompt = args.goal.clone();
    }
    dispatch_args
}

/// Compile the one durable provider-cell plan used by local Cook and Lab handoff.
pub(crate) fn compile_cook_plan(
    args: &AgentTaskCookArgs,
    provision: Value,
) -> homeboy::core::Result<AgentTaskPlan> {
    let workspace = provision
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            homeboy::core::Error::internal_unexpected(
                "Cook destination provisioning did not return a task worktree path".to_string(),
            )
        })?
        .to_string();
    let mut dispatch = dispatch_args_for_cook(args);
    // Cook providers always receive the declared task checkout. `--cwd` is a
    // dispatch input, never authority to replace the writable Cook workspace.
    dispatch.cwd = None;
    dispatch.workspace = Some(workspace);
    let mut request = dispatch_service::resolve_dispatch_request(dispatch.into())?;
    let mut plan = match dispatch_service::build_controller_dispatch_plan(&mut request) {
        Ok(plan) => plan,
        // A managed promotion handle may be intentionally unavailable until
        // after provider execution. Keep the compiled task durable and let
        // promotion report its established controlled policy failure.
        Err(error)
            if request.workspace.as_deref() == Some(args.to_worktree.as_str())
                && error.message.contains(
                    "neither an existing directory nor a resolvable managed worktree handle",
                ) =>
        {
            request.workspace = None;
            dispatch_service::build_controller_dispatch_plan(&mut request)?
        }
        Err(error) => return Err(error),
    };
    plan.options.candidate_completion = args.candidate_completion;
    record_cook_provision(&mut plan, provision);
    for task in &mut plan.tasks {
        let root = task.workspace.root.as_deref().ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "workspace",
                "Cook requires a bound task worktree",
                None,
                None,
            )
        })?;
        task.metadata["cook_workspace_identity"] = workspace_identity_attestation(Path::new(root))?;
    }
    homeboy::agents::agent_task_provider::AgentTaskProviderCatalog::discover()
        .validate_explicit_models(&plan)?;
    record_cook_goal(&mut plan, args.goal.as_deref());
    Ok(plan)
}

#[cfg(unix)]
fn workspace_identity_attestation(path: &Path) -> homeboy::core::Result<Value> {
    use std::os::unix::fs::MetadataExt;
    let canonical = std::fs::canonicalize(path).map_err(|error| {
        homeboy::core::Error::internal_io(error.to_string(), Some(path.display().to_string()))
    })?;
    let metadata = std::fs::symlink_metadata(&canonical).map_err(|error| {
        homeboy::core::Error::internal_io(error.to_string(), Some(canonical.display().to_string()))
    })?;
    let git_dir = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(&canonical)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string());
    let git_file = canonical.join(".git");
    let git_metadata = std::fs::symlink_metadata(&git_file).map_err(|error| {
        homeboy::core::Error::internal_io(error.to_string(), Some(git_file.display().to_string()))
    })?;
    let git_content = std::fs::read_to_string(&git_file).map_err(|error| {
        homeboy::core::Error::internal_io(error.to_string(), Some(git_file.display().to_string()))
    })?;
    let gitdir_target = git_content
        .strip_prefix("gitdir: ")
        .map(str::trim)
        .and_then(|target| std::fs::canonicalize(canonical.join(target)).ok());
    Ok(
        serde_json::json!({ "canonical_path": canonical, "device": metadata.dev(), "inode": metadata.ino(), "git_dir": git_dir, "git_file_is_file": git_metadata.file_type().is_file(), "git_file_content": git_content, "gitdir_target": gitdir_target }),
    )
}

#[cfg(not(unix))]
fn workspace_identity_attestation(path: &Path) -> homeboy::core::Result<Value> {
    let canonical = std::fs::canonicalize(path).map_err(|error| {
        homeboy::core::Error::internal_io(error.to_string(), Some(path.display().to_string()))
    })?;
    Ok(serde_json::json!({ "canonical_path": canonical }))
}

fn record_cook_goal(plan: &mut AgentTaskPlan, goal: Option<&str>) {
    let Some(goal) = goal else {
        return;
    };
    if !plan.metadata.is_object() {
        plan.metadata = serde_json::json!({});
    }
    plan.metadata["cook_goal"] = serde_json::json!(goal);
    for task in &mut plan.tasks {
        if !task.metadata.is_object() {
            task.metadata = serde_json::json!({});
        }
        task.metadata["cook_goal"] = serde_json::json!(goal);
    }
}

fn git_head_sha(path: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(path)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn default_loop_title(args: &AgentTaskCookArgs) -> String {
    let target = args
        .dispatch
        .repo
        .as_deref()
        .or(args.dispatch.task_url.as_deref())
        .unwrap_or("agent task");
    format!("Cook {target}")
}

fn default_loop_commit_message(args: &AgentTaskCookArgs) -> String {
    let target = args.dispatch.repo.as_deref().unwrap_or("agent task");
    format!("fix: cook {target}")
}

pub(super) fn run_plan(args: RunPlanArgs) -> CmdResult<Value> {
    let mut plan = agent_task_service::read_plan(&args.plan)?;
    if let Some(timeout_ms) = args.timeout_ms {
        plan.options.timeout_ms = Some(timeout_ms);
    }
    emit_runner_lifecycle_progress(&plan, args.record_run_id.as_deref());
    run_loaded_plan(
        plan,
        args.record_run_id.as_deref(),
        ExtensionProviderAgentTaskExecutor::discover(),
    )
}

fn emit_runner_lifecycle_progress(plan: &AgentTaskPlan, run_id: Option<&str>) {
    if std::env::var_os(homeboy::core::lab_contract::LAB_EXECUTION_RUNNER_ID_ENV).is_none() {
        return;
    }
    for task in &plan.tasks {
        println!(
            "HOMEBOY_RUNNER_PROGRESS {}",
            serde_json::json!({
                "schema": "homeboy/runner-progress/v1",
                "phase": "provider_dispatch",
                "current_item": task.task_id,
                "metadata": {
                    "agent_task_run_id": run_id,
                    "task_id": task.task_id,
                    "provider": task.executor.backend,
                    "event": "provider_selected",
                },
            })
        );
    }
}

pub(super) fn run_loaded_plan<E>(
    plan: AgentTaskPlan,
    record_run_id: Option<&str>,
    executor: E,
) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter,
{
    let result = agent_task_service::run_loaded_plan(plan, record_run_id, executor)?;
    let value =
        if std::env::var_os(homeboy::core::lab_contract::LAB_EXECUTION_RUNNER_ID_ENV).is_some() {
            aggregate_value_with_failure_reasons(&result.value)
        } else {
            super::status::compact_aggregate_summary(&result.value, record_run_id)
        };
    Ok((value, result.exit_code))
}

pub(super) fn run_submitted(args: RunArgs) -> CmdResult<Value> {
    run_submitted_with_executor(
        args.run_id,
        args.timeout_ms,
        ExtensionProviderAgentTaskExecutor::discover(),
    )
}

pub(super) fn run_submitted_with_executor<E>(
    run_id: String,
    timeout_ms: Option<u64>,
    executor: E,
) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter,
{
    let result =
        agent_task_service::run_submitted_with_timeout(run_id.clone(), timeout_ms, executor)?;
    Ok((
        super::status::compact_aggregate_summary(&result.value, Some(&run_id)),
        result.exit_code,
    ))
}

pub(super) fn run_next() -> CmdResult<Value> {
    run_next_with_executor(ExtensionProviderAgentTaskExecutor::discover())
}

pub(super) fn run_next_with_executor<E>(executor: E) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    let result = agent_task_service::run_next_with_cook_dispatcher(
        executor,
        crate::commands::infra::route::reconstruct_cook_attempt_dispatcher,
    )?;
    let Some(aggregate) = result.value else {
        return Ok((serde_json::json!({ "claimed": false }), 0));
    };
    Ok((
        aggregate_value_with_failure_reasons(&aggregate),
        result.exit_code,
    ))
}

pub(super) fn submit(args: SubmitArgs) -> CmdResult<Value> {
    let record = agent_task_service::submit_plan_spec(&args.plan, args.run_id.as_deref())?;
    Ok((serde_json::to_value(record).unwrap_or(Value::Null), 0))
}

pub(super) fn resume(args: StatusArgs) -> CmdResult<Value> {
    run_resume_with_executor_and_bridge(
        args.run_id,
        args.bridge,
        args.since_cursor,
        args.full,
        ExtensionProviderAgentTaskExecutor::discover(),
    )
}

pub(super) fn run_resume_with_executor<E>(run_id: String, executor: E) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter,
{
    run_resume_with_executor_and_bridge(run_id, false, None, false, executor)
}

pub(super) fn run_resume_with_executor_and_bridge<E>(
    run_id: String,
    bridge: bool,
    since_cursor: Option<u64>,
    full: bool,
    executor: E,
) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter,
{
    let needs_transport_recovery =
        bridge && agent_task_service::terminal_transport_recovery_required(&run_id);
    if needs_transport_recovery {
        // Recover the authenticated terminal runner snapshot before `resume`
        // can short-circuit on a previously persisted lossy aggregate.
        agent_task_service::recover_terminal_transport_proxy_evidence(&run_id)?;
    }
    let result = agent_task_service::resume(run_id.clone(), executor)?;
    if bridge {
        // Resume first imports authoritative terminal runner evidence when the
        // local aggregate is absent. Reproject only after that shared recovery
        // contract has persisted the aggregate and identity.
        agent_task_service::reconcile_terminal_artifact_projection(&run_id)?;
        let status = agent_task_service::run_status(&run_id, since_cursor)?;
        return Ok((
            serde_json::to_value(status).unwrap_or(Value::Null),
            result.exit_code,
        ));
    }
    Ok((
        if full {
            aggregate_value_with_failure_reasons(&result.value)
        } else {
            super::status::compact_aggregate_summary(&result.value, Some(&run_id))
        },
        result.exit_code,
    ))
}

pub(super) fn retry(args: RetryArgs) -> CmdResult<Value> {
    let result = agent_task_service::retry(
        &args.run_id,
        args.new_run_id.as_deref(),
        args.run,
        args.force,
    )?;
    if result.run {
        return run_submitted_with_executor(
            result.record.run_id,
            None,
            ExtensionProviderAgentTaskExecutor::discover(),
        );
    }
    Ok((
        serde_json::to_value(result.record).unwrap_or(Value::Null),
        0,
    ))
}

#[cfg(test)]
mod tests {
    use super::{cook_report_with_continuation, durable_cook_identity_lines};

    #[test]
    fn durable_cook_identity_block_leads_with_the_run_id_and_follow_up_commands() {
        let lines = durable_cook_identity_lines(Some("cook-10419"), "cook-10419-attempt-1");

        // The very first thing an interrupted caller sees must be the handle.
        assert!(
            lines[0].contains("cook-10419-attempt-1"),
            "identity block must lead with the durable run id: {lines:?}"
        );
        assert!(lines[0].contains("cook-10419"));
        let joined = lines.join("\n");
        assert!(joined.contains("homeboy agent-task status cook-10419-attempt-1"));
        assert!(joined.contains("homeboy agent-task logs cook-10419-attempt-1"));
        assert!(joined.contains("homeboy agent-task cancel cook-10419-attempt-1"));
    }

    #[test]
    fn durable_cook_identity_block_omits_a_cook_alias_equal_to_the_run_id() {
        let lines = durable_cook_identity_lines(Some("run-9163"), "run-9163");

        assert!(!lines[0].contains("(cook"), "no redundant alias: {lines:?}");
        assert!(lines[0].contains("run-9163"));
    }

    #[test]
    fn in_flight_cook_report_keeps_provider_state_and_managed_continuation_separate() {
        let report = cook_report_with_continuation(serde_json::json!({
            "cook_id": "cook-1",
            "latest_run_id": "cook-1-attempt-1",
            "status": "in_flight",
            "attempts": [{ "run_state": "Running" }],
        }));

        assert_eq!(report["provider"]["state"], "Running");
        assert_eq!(
            report["remaining_phases"],
            serde_json::json!(["harvest", "review", "gates", "promotion", "finalization"])
        );
        assert_eq!(
            report["continuation_command"],
            "homeboy agent-task cook-continue cook-1-attempt-1"
        );
    }
}
