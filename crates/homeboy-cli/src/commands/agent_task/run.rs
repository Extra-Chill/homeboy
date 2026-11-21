//! Durable run lifecycle handlers: cook, run-plan, run,
//! resume, and retry.

pub(super) use serde_json::{json, Value};
pub(super) use std::collections::BTreeMap;
pub(super) use std::io::{Read, Write};
pub(super) use std::path::{Path, PathBuf};
pub(super) use std::process::Command;
pub(super) use std::sync::Arc;
pub(super) use std::time::Duration;

pub(super) use homeboy::agents::agent_task_service as agent_task_service_direct;
pub(super) use homeboy::agents::agent_task_service::cook_registered_component_by_id;
pub(super) use homeboy::agents::agent_task_timeout::effective_provider_timeout_ms;
pub(super) use homeboy::agents::agent_tasks::dispatch_service;
pub(super) use homeboy::agents::agent_tasks::lifecycle as agent_task_lifecycle;
pub(super) use homeboy::agents::agent_tasks::provider::ExtensionProviderAgentTaskExecutor;
pub(super) use homeboy::agents::agent_tasks::review_dossier::PrTitlePolicy;
pub(super) use homeboy::agents::agent_tasks::scheduler::{
    AgentTaskAggregate, AgentTaskPlan, SharedAgentTaskExecutor,
};
pub(super) use homeboy::agents::agent_tasks::service as agent_task_service;
pub(super) use homeboy::core::command_invocation::CommandInvocation;
pub(super) use homeboy::core::engine::shell::quote_args;
pub(super) use homeboy::core::Error;

pub(super) use super::super::agent_task_dispatch::DispatchArgs;
pub(super) use super::super::CmdResult;
pub(super) use super::args::{
    AgentTaskCookArgs, AgentTaskProviderEvidenceInput, PlacementUpdateArgs, PromotionProviderArgs,
    RetryArgs, RunArgs, RunPlanArgs, ValidatePlanArgs,
};
pub(super) use super::default_branch::DefaultBranchRequest;
pub(super) use super::gate_contract::{
    reject_gates_unexecutable_under_resolved_placement, validate_cargo_gate_contracts,
    validate_gate_contracts, GateContractValidation,
};

pub(super) const MAX_PROMOTION_PROVIDER_REQUEST_BYTES: u64 = 16 * 1024 * 1024;
/// Provider evidence is streamed into an immutable, digest-addressed projection.
/// This ceiling bounds both controller disk use and Lab transport without putting
/// fixture bytes in Cook command state or JSON output.
pub(super) const MAX_PROVIDER_EVIDENCE_BYTES: u64 = 64 * 1024 * 1024;
pub(super) const PREVIEW_STDIN_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const PREVIEW_PROGRESS_HEARTBEAT: Duration = Duration::from_secs(5);

/// Add a registered component's `default_verify_gates` when the caller
/// supplied none. Defaults belong to the component, so they run in its
/// repository-relative directory. Runs before request validation, which
/// requires a gate.
pub(super) fn apply_component_default_verify_gates(
    args: &mut AgentTaskCookArgs,
    component: Option<&homeboy::core::component::Component>,
) -> homeboy::core::Result<()> {
    let Some(component) = component else {
        return Ok(());
    };
    if args.gates.has_deterministic_gate() {
        return Ok(());
    }
    let component_cwd = component_gate_cwd(component)?;
    for gate in component_default_gates(&component.default_verify_gates, &component_cwd) {
        args.gates.push_controller_verify(gate);
    }
    Ok(())
}

/// Add the component's lint gate when it declares lint support (#15888).
/// Explicit caller gates never suppress it; repository-level checkouts and
/// `--no-component-lint-gate` do. Runs after destination resolution, because
/// the gate is scoped to the Cook's resolved base (#17226).
pub(super) fn apply_component_lint_gate(
    args: &mut AgentTaskCookArgs,
    retained_gate_contract: bool,
    component: Option<&homeboy::core::component::Component>,
) -> homeboy::core::Result<()> {
    let Some(component) = component else {
        return Ok(());
    };
    let siblings = lint_components_in_checkout(
        component,
        homeboy::core::component::inventory::registered_base()?,
    );
    let component_id = &component.id;
    let repository_level = homeboy::core::component::resolution::is_repository_level_checkout(
        Path::new(&component.local_path),
    );
    if repository_level && siblings.len() == 1 {
        eprintln!(
            "cook: skipping default component lint gate for repository-level task '{}' (nested manifests, no root homeboy.json); explicit verification gates are unchanged",
            component_id
        );
    }
    let declares_lint = (!repository_level || siblings.len() > 1)
        && !retained_gate_contract
        && !args.gates.no_component_lint_gate
        && siblings.iter().any(component_declares_lint);
    let already_linted = args
        .gates
        .verify
        .iter()
        .chain(&args.gates.private_verify)
        .any(|program| program.contains("review lint") || program.contains("homeboy lint"));
    if !declares_lint || already_linted {
        return Ok(());
    }
    // Promotion binds this token to the harvested candidate's recorded base SHA.
    let gate = changed_component_lint_gate(&siblings, "__HOMEBOY_COOK_BASE_SHA__")?;
    args.gates.push_controller_verify(gate);
    Ok(())
}

/// Remote identity alone cannot select a component in a monorepo. Keep all
/// registrations in the same checkout so the candidate diff can select owners.
pub(super) fn lint_components_in_checkout(
    selected: &homeboy::core::component::Component,
    registered: Vec<homeboy::core::component::Component>,
) -> Vec<homeboy::core::component::Component> {
    let root = homeboy::core::git::repo_root(Path::new(&selected.local_path));
    let mut components = vec![selected.clone()];
    if let Some(root) = root {
        for component in registered {
            if component.id != selected.id
                && homeboy::core::git::repo_root(Path::new(&component.local_path)).as_ref()
                    == Some(&root)
            {
                components.push(component);
            }
        }
    }
    components.sort_by(|a, b| a.id.cmp(&b.id));
    components
}

/// Resolve scope in the candidate workspace, not the controller checkout:
/// before the provider runs there may be no changed paths to inspect. Snapshot
/// registration paths into the gate so Lab does not need the controller registry.
pub(super) fn changed_component_lint_gate(
    components: &[homeboy::core::component::Component],
    base: &str,
) -> homeboy::core::Result<String> {
    if let [component] = components {
        return Ok(component_lint_gate(
            &component.id,
            &component_gate_cwd(component)?,
            base,
        ));
    }
    let mut gates = vec![];
    for component in components.iter().filter(|c| component_declares_lint(c)) {
        let path = component_gate_cwd(component)?;
        // A failed diff (for example a missing base ref) is infrastructure
        // failure, never evidence that a component has no changes.
        gates.push(format!(
            "if git diff --quiet --no-renames {} -- {}; then :; else changed=$?; if [ \"$changed\" -gt 1 ]; then exit \"$changed\"; fi; {} || exit $?; fi",
            quote_args(&[base.to_string()]),
            quote_args(&[format!(":(literal){path}")]),
            component_lint_gate(&component.id, &path, base),
        ));
    }
    Ok(format!("( {} )", gates.join("; ")))
}

pub(super) fn component_gate_cwd(
    component: &homeboy::core::component::Component,
) -> homeboy::core::Result<String> {
    let root = homeboy::core::git::repo_root(Path::new(&component.local_path));
    durable_component_cwd(component, &component.id, root.as_deref())
}

/// A component declares lint when it owns a `scripts.lint` command or links an
/// extension that provides the lint capability. Unresolvable extension
/// metadata is not treated as a declaration: the Cook stays admissible and the
/// gate is simply not inferred.
pub(super) fn component_declares_lint(component: &homeboy::core::component::Component) -> bool {
    use homeboy_extension_contract::ExtensionCapability;
    component.has_script(ExtensionCapability::Lint)
        || matches!(
            homeboy_core::extension::resolve::resolve_execution_context_if_available(
                component,
                ExtensionCapability::Lint,
            ),
            Ok(Some(_))
        )
}

/// The canonical lint gate for a component, run from the candidate repository
/// root with a repository-relative component path. It mirrors the component's CI
/// `homeboy review lint`, including its declared baseline policy, and checks
/// only what the candidate changed since its base, so pre-existing findings
/// elsewhere cannot reject an unrelated change (#17226). Promotion binds the
/// recorded base SHA before execution. Cook already
/// placed this workspace, so the gate runs there instead of re-entering
/// resource-policy placement from its sanitized gate home (#16517).
pub(super) fn component_lint_gate(component_id: &str, component_cwd: &str, base: &str) -> String {
    format!(
        "HOMEBOY_LINT_BASE={} homeboy --placement local review lint {} --path {} --changed-since {}",
        quote_args(&[base.to_string()]),
        quote_args(&[component_id.to_string()]),
        quote_args(&[component_cwd.to_string()]),
        quote_args(&[base.to_string()])
    )
}

/// Component defaults, scoped to the component directory. A subshell scopes
/// compound commands too.
pub(super) fn component_default_gates(defaults: &[String], component_cwd: &str) -> Vec<String> {
    defaults
        .iter()
        .map(|gate| {
            if component_cwd == "." {
                gate.clone()
            } else {
                format!(
                    "(cd {} && {})",
                    quote_args(&[component_cwd.to_string()]),
                    gate
                )
            }
        })
        .collect()
}

/// Resolve the same pre-provisioning Cook inputs used by execution. This path
/// intentionally never calls `provision_cook_destination` or a durable service.

/// One Cook continuation request. `agent-task recover` builds it from the
/// recovery step it performs (and any explicit operator overrides); it is no
#[cfg(unix)]
pub(super) fn workspace_identity_attestation(path: &Path) -> homeboy::core::Result<Value> {
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
pub(super) fn workspace_identity_attestation(path: &Path) -> homeboy::core::Result<Value> {
    let canonical = std::fs::canonicalize(path).map_err(|error| {
        homeboy::core::Error::internal_io(error.to_string(), Some(path.display().to_string()))
    })?;
    Ok(serde_json::json!({ "canonical_path": canonical }))
}

pub(super) fn record_cook_goal(plan: &mut AgentTaskPlan, goal: Option<&str>) {
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

pub(super) fn git_head_sha(path: &Path) -> Option<String> {
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

pub(super) fn run_plan(args: RunPlanArgs) -> CmdResult<Value> {
    let mut plan = agent_task_service::read_plan(&args.plan)?;
    if let Some(timeout_ms) = args.timeout_ms {
        plan.options.timeout_ms = Some(timeout_ms);
    }
    // Managed services own durable process logs, so a direct plan invocation
    // needs a run identity even when the caller did not provide one.
    let record_run_id = args.record_run_id.or_else(|| {
        (!plan.services.is_empty()).then(|| format!("run-plan-{}", uuid::Uuid::new_v4()))
    });
    emit_runner_lifecycle_progress(&plan, record_run_id.as_deref());
    run_loaded_plan(
        plan,
        record_run_id.as_deref(),
        Arc::new(ExtensionProviderAgentTaskExecutor::discover()),
    )
}

pub(super) fn emit_runner_lifecycle_progress(plan: &AgentTaskPlan, run_id: Option<&str>) {
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

pub(super) fn run_loaded_plan(
    plan: AgentTaskPlan,
    record_run_id: Option<&str>,
    executor: SharedAgentTaskExecutor,
) -> CmdResult<Value> {
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
        Arc::new(ExtensionProviderAgentTaskExecutor::discover()),
    )
}

pub(super) fn run_submitted_with_executor(
    run_id: String,
    timeout_ms: Option<u64>,
    executor: SharedAgentTaskExecutor,
) -> CmdResult<Value> {
    if agent_task_lifecycle::exact_record(&run_id)
        .ok()
        .is_some_and(|record| agent_task_lifecycle::is_unmaterialized_cook_admission(&record))
    {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "run_id",
            "unmaterialized Cook admission must continue through its fenced resume path",
            Some(run_id.clone()),
            Some(vec![format!("homeboy agent-task recover {run_id}")]),
        )
        .with_hint(format!("Run `homeboy agent-task recover {run_id}`.")));
    }
    let result =
        agent_task_service::run_submitted_with_timeout(run_id.clone(), timeout_ms, executor)?;
    Ok((
        super::status::compact_aggregate_summary(&result.value, Some(&run_id)),
        result.exit_code,
    ))
}

pub(super) fn validate_plan(args: ValidatePlanArgs) -> CmdResult<Value> {
    let report = agent_task_service_direct::validate_plan_spec(&args.plan);
    let valid = report.valid;
    Ok((
        serde_json::to_value(report).unwrap_or(Value::Null),
        i32::from(!valid),
    ))
}

pub(super) fn placement_update(
    args: PlacementUpdateArgs,
    placement: crate::cli_surface::Placement,
) -> CmdResult<Value> {
    if !placement.is_explicit_local_override() {
        return Err(Error::validation_invalid_argument(
            "placement",
            "placement update requires explicit --placement local authorization",
            None,
            None,
        ));
    }
    let idempotency_key = args
        .idempotency_key
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let acknowledgement = homeboy::agents::orchestration::execute_action_from_current_environment(
        &args.run_id,
        &homeboy_control_plane_contract::ControlPlaneActionRequest {
            schema: homeboy_control_plane_contract::CONTROL_PLANE_ACTION_REQUEST_SCHEMA.to_string(),
            action: homeboy_control_plane_contract::ControlPlaneAction::PlacementUpdate,
            effect_id: homeboy_control_plane_contract::action_effect_id(
                "cli",
                &args.run_id,
                "placement-update",
                &idempotency_key,
            ),
            idempotency_key,
            actor: "homeboy-cli".to_string(),
            expected_updated_at: None,
            parameters: homeboy_control_plane_contract::ControlPlaneActionPayload {
                schema:
                    homeboy_control_plane_contract::CONTROL_PLANE_PLACEMENT_UPDATE_PARAMETERS_SCHEMA
                        .to_string(),
                data: json!({ "placement": "local" }),
            },
            confirmed: args.confirm,
        },
    )?;
    Ok((
        serde_json::to_value(acknowledgement)
            .map_err(|error| Error::internal_json(error.to_string(), None))?,
        0,
    ))
}

#[cfg(test)]
pub(super) fn run_recover_with_executor(
    run_id: String,
    _full: bool,
    idempotency_key: Option<String>,
    executor: SharedAgentTaskExecutor,
) -> CmdResult<Value> {
    let idempotency_key = idempotency_key.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let acknowledgement =
        homeboy::agents::orchestration::execute_recover_action_from_current_environment(
            &run_id,
            &homeboy_control_plane_contract::ControlPlaneActionRequest {
                schema: homeboy_control_plane_contract::CONTROL_PLANE_ACTION_REQUEST_SCHEMA
                    .to_string(),
                action: homeboy_control_plane_contract::ControlPlaneAction::Recover,
                effect_id: homeboy_control_plane_contract::action_effect_id(
                    "cli",
                    &run_id,
                    "recover",
                    &idempotency_key,
                ),
                idempotency_key,
                actor: "homeboy-cli".to_string(),
                expected_updated_at: None,
                parameters: homeboy_control_plane_contract::ControlPlaneActionPayload::recover(),
                confirmed: true,
            },
            executor,
        )?;
    if acknowledgement.outcome == homeboy_control_plane_contract::ControlPlaneActionOutcome::Failed
        && acknowledgement.result.data.get("report").is_none()
    {
        return Err(homeboy_core::Error::validation_invalid_argument(
            "recover",
            acknowledgement
                .message
                .unwrap_or_else(|| "recover action failed".into()),
            None,
            None,
        )
        .into());
    }
    Ok((
        acknowledgement.result.data["report"].clone(),
        acknowledgement.result.data["exit_code"]
            .as_i64()
            .unwrap_or(1) as i32,
    ))
}

/// `retry` submits a fresh run from any run's plan. A Cook ID whose durable
/// state names a recovery step is delegated to `agent-task recover`, the one
/// Cook planner (#15925). Store contention is bounded: it retries with
/// backoff and then reports `observation_store.busy`.
pub(super) fn retry(args: RetryArgs) -> CmdResult<Value> {
    agent_task_service_direct::with_store_contention_backoff(
        "agent-task retry",
        |outcome: &CmdResult<Value>| {
            outcome
                .as_ref()
                .is_err_and(agent_task_service_direct::is_store_contention)
        },
        || retry_cook_or_run(args.clone()),
    )
}

pub(super) fn retry_cook_or_run(mut args: RetryArgs) -> CmdResult<Value> {
    if !explicit_retry(&args) {
        // A plain `retry <cook-id> --run` performs the Cook's one recovery
        // step through `recover` (#15925).
        if agent_task_service_direct::cook_recovery_delegation(&args.run_id).is_some() {
            return super::recover::recover(super::RecoverArgs {
                id: args.run_id,
                ..Default::default()
            });
        }
    }
    // An explicit retry of a mission ID or a superseded attempt targets the
    // Cook's latest durable attempt rather than refusing the ID (#17933).
    let latest = agent_task_service_direct::resolve_retry_run_id(&args.run_id)?;
    if latest != args.run_id {
        eprintln!(
            "Retrying latest attempt {latest} (resolved from {})",
            args.run_id
        );
        args.run_id = latest;
    }
    retry_exact(args)
}

/// Retry inputs beyond a plain `--run`: these reserve a retry directly.
pub(super) fn explicit_retry(args: &RetryArgs) -> bool {
    !args.run || args.new_run_id.is_some() || args.force || args.idempotency_key.is_some()
}

/// Retry exactly the run `args.run_id` resolves to, without consulting Cook
/// recovery. This is the step executor `agent-task recover` calls.
pub(super) fn retry_exact(args: RetryArgs) -> CmdResult<Value> {
    retry_with(
        args,
        Arc::new(ExtensionProviderAgentTaskExecutor::discover()),
    )
}

/// Reserve a retry successor through the control-plane Retry action: the one
/// legal-action entry point with idempotent acknowledgement, eligibility and
/// audit. `agent-task retry` and `agent-task recover` both reserve through here, so
/// a Cook continuation is no longer a side door around the action outbox.
pub(super) fn reserve_retry_through_action(
    run_id: &str,
    new_run_id: Option<String>,
    force: bool,
    route_override: &homeboy::agents::agent_task_service::CookProviderRouteOverride,
    timeout_ms: Option<u64>,
    idempotency_key: String,
) -> homeboy::core::Result<(
    homeboy_control_plane_contract::ControlPlaneActionAcknowledgement,
    homeboy::agents::agent_task_action_result::RetryActionResult,
)> {
    let acknowledgement = homeboy::agents::orchestration::execute_retry_action_from_current_environment_with_preflight(
        run_id,
        &homeboy_control_plane_contract::ControlPlaneActionRequest {
            schema: homeboy_control_plane_contract::CONTROL_PLANE_ACTION_REQUEST_SCHEMA.to_string(),
            action: homeboy_control_plane_contract::ControlPlaneAction::Retry,
            effect_id: homeboy_control_plane_contract::action_effect_id(
                "cli",
                run_id,
                "retry",
                &idempotency_key,
            ),
            idempotency_key,
            actor: "homeboy-cli".to_string(),
            expected_updated_at: None,
            parameters: homeboy_control_plane_contract::ControlPlaneActionPayload {
                schema: homeboy_control_plane_contract::CONTROL_PLANE_RETRY_PARAMETERS_SCHEMA
                    .to_string(),
                data: serde_json::json!({
                    "new_run_id": new_run_id,
                    "force": force,
                    "provider_route": (!route_override.is_empty()).then(|| serde_json::json!({
                        "backend": route_override.backend,
                        "selector": route_override.selector,
                        "model": route_override.model,
                        "acknowledge_model_override": route_override.acknowledge_model_override,
                    })),
                    "timeout_ms": timeout_ms,
                }),
            },
            confirmed: true,
        },
        true,
        crate::commands::infra::route::validate_generic_lab_command_replay_workspace,
    )?;
    let retry = homeboy::agents::agent_task_action_result::retry(&acknowledgement)?;
    Ok((acknowledgement, retry))
}

pub(super) fn retry_with(args: RetryArgs, _executor: SharedAgentTaskExecutor) -> CmdResult<Value> {
    if let Some(error) = explicit_local_retry_admission_refusal(
        homeboy::core::parsed_command_preflight::captured_result().as_ref(),
    ) {
        return Err(error);
    }
    let idempotency_key = args
        .idempotency_key
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    // A Cook's next-attempt provider route is an explicit `recover` override,
    // reserved through the continuation path; plain retry keeps its route.
    let (acknowledgement, retry) = reserve_retry_through_action(
        &args.run_id,
        args.new_run_id.clone(),
        args.force,
        &Default::default(),
        None,
        idempotency_key,
    )?;
    // The first reservation already committed the exact execution intent.
    // Waiting observes its durable outcome, including on action redelivery.
    if args.run {
        return super::status::wait_for_terminal_status(&retry.record.run_id);
    }
    Ok((
        serde_json::to_value(acknowledgement)
            .map_err(|error| Error::internal_json(error.to_string(), None))?,
        0,
    ))
}

pub(super) fn explicit_local_retry_admission_refusal(
    preflight: Option<&homeboy::core::parsed_command_preflight::ParsedCommandPreflightResult>,
) -> Option<Error> {
    let result = preflight?;
    if result.placement.requested != homeboy_lab_contract::Placement::Local {
        return None;
    }
    let homeboy::core::parsed_command_preflight::ResourceAdmissionDecision::Rejected {
        label, ..
    } = &result.resource_admission
    else {
        return None;
    };
    Some(Error::validation_invalid_argument(
        "placement",
        format!("explicit local retry refused: controller resource admission rejected {label}; wait for controller capacity or retry without --placement local"),
        None,
        None,
    ))
}

pub(crate) use super::run_continuation::continue_cook;
#[allow(unused_imports)] // Preserve the existing run module path.
pub(crate) use super::run_continuation::continue_cook_with;
#[cfg(test)]
pub(super) use super::run_continuation::cook_continuation_status;
pub(super) use super::run_continuation::cook_report_with_continuation;
pub(crate) use super::run_continuation::preflight_continue_cook;
pub(crate) use super::run_continuation::CookContinueArgs;
pub(super) use super::run_destination::cook_provision_repository;
#[cfg(test)]
pub(super) use super::run_destination::cook_pull_request_checkout;
#[cfg(test)]
pub(super) use super::run_destination::cook_pull_request_refs;
#[allow(unused_imports)] // Preserve the existing run module path.
pub(crate) use super::run_destination::corrected_cook_base_replay_argv;
pub(crate) use super::run_destination::preflight_cook_source_history;
pub(super) use super::run_destination::project_preview_dirty_admission;
pub(crate) use super::run_destination::provision_cook_destination;
pub(crate) use super::run_destination::resolve_cook_destination;
pub(crate) use super::run_destination::resolve_cook_destination_with_component_lint_gate;
#[cfg(test)]
pub(super) use super::run_destination::resolve_cook_issue;
#[cfg(test)]
pub(super) use super::run_destination::resolve_cook_pull_request;
pub(super) use super::run_destination::validate_cook_cwd_destination_identity;
#[allow(unused_imports)] // Preserve the existing run module path.
pub(crate) use super::run_destination::validate_logical_worktree_handle_path_relationship;
pub(crate) use super::run_destination::ResolvedCookRepository;
pub(super) use super::run_disclosure::aggregate_value_with_failure_reasons;
pub(crate) use super::run_disclosure::announce_durable_cook_identity;
pub(crate) use super::run_disclosure::announce_resolved_execution_placement;
pub(super) use super::run_disclosure::cli_placement_str;
pub(super) use super::run_disclosure::cook_effective_plan_disclosure;
pub(super) use super::run_disclosure::cook_provider_readiness_disclosure;
pub(super) use super::run_disclosure::cook_provider_readiness_projection;
pub(super) use super::run_disclosure::cook_provider_timeout_disclosure;
pub(super) use super::run_disclosure::cook_resolved_policy_disclosure;
pub(crate) use super::run_disclosure::cook_rotation_disclosure;
pub(crate) use super::run_disclosure::detached_cook_route_less_warning;
#[allow(unused_imports)] // Preserve the existing run module path.
pub(crate) use super::run_disclosure::durable_cook_identity_lines;
#[cfg(test)]
pub(super) use super::run_disclosure::format_resolved_execution_placement_line;
pub(super) use super::run_disclosure::run_cook_explicit;
pub(crate) use super::run_execution::compile_cook_plan;
pub(super) use super::run_execution::default_cook_prompt_from_task_url;
#[allow(unused_imports)] // Preserve the existing run module path.
pub(super) use super::run_execution::dispatch_args_for_cook;
pub(crate) use super::run_execution::preflight_cook_execution_request;
pub(crate) use super::run_execution::prepare_cook_request;
pub(crate) use super::run_execution::promotion_provider;
pub(crate) use super::run_execution::record_cook_argument_provenance;
#[allow(unused_imports)] // Preserve the existing run module path.
pub(crate) use super::run_execution::record_cook_provision;
pub(crate) use super::run_execution::require_model_override_acknowledgement_for_cook;
pub(super) use super::run_execution::resolve_cook_execution_budget;
#[allow(unused_imports)] // Preserve the existing run module path.
pub(super) use super::run_execution::resolve_dispatch_prompt;
pub(super) use super::run_execution::resolved_dispatch_args_for_cook;
pub(crate) use super::run_execution::snapshot_cook_prompt;
pub(super) use super::run_execution::snapshot_cook_prompt_bounded;
pub(super) use super::run_execution::validate_cook_provider_execution_plan;
pub(crate) use super::run_execution::validate_cook_request_with_provenance;
pub(super) use super::run_execution::validate_cook_source_authority;
#[cfg(test)]
pub(super) use super::run_execution::validate_cook_subjects;
pub(crate) use super::run_execution::{run_cook, CookEntryContext};
#[cfg(test)]
pub(super) use super::run_execution::{
    admit_local_cook_dispatch_with_clock, record_local_dispatch_wait,
};
#[cfg(test)]
pub(super) use super::run_preview::append_preview_lifecycle_replay_argv;
#[cfg(test)]
pub(super) use super::run_preview::apply_preview_worktree_capacity;
#[allow(unused_imports)] // Preserve the existing run module path.
pub(super) use super::run_preview::bind_cook_preview_lifecycle;
#[cfg(test)]
pub(super) use super::run_preview::clap_replay_units;
#[cfg(test)]
pub(super) use super::run_preview::cook_gate_preflight_workspace;
#[cfg(test)]
pub(super) use super::run_preview::cook_preview_replay_argv;
#[cfg(test)]
pub(super) use super::run_preview::cook_preview_result;
pub(crate) use super::run_preview::cook_replay_argv;
#[cfg(test)]
pub(super) use super::run_preview::cook_value_flags;
pub(super) use super::run_preview::dispatch_command_for_cook;
#[cfg(test)]
pub(super) use super::run_preview::finalize_cook_preview_replay;
pub(super) use super::run_preview::preflight_cook_gates;
pub(super) use super::run_preview::preflight_preview_lab_provider_secret_env;
pub(super) use super::run_preview::preflight_resolved_cook_gates;
pub(crate) use super::run_preview::preview_cook;
#[cfg(test)]
pub(super) use super::run_preview::preview_gate_contract_disclosure;
#[cfg(test)]
pub(super) use super::run_preview::preview_placement_policy_with_admission;
#[cfg(test)]
pub(super) use super::run_preview::preview_replay_has_flag;
#[cfg(test)]
pub(super) use super::run_preview::redact_preview_replay_argv;
#[cfg(test)]
pub(super) use super::run_preview::render_cook_admission;
pub(crate) use super::run_preview::rewrite_cook_identity_replay_argv;
#[cfg(test)]
pub(super) use super::run_preview::PreviewReplayArgv;
#[cfg(test)]
pub(super) use super::run_provider_evidence::absolute_host_paths_in_provider_prompt;
pub(crate) use super::run_provider_evidence::admit_provider_evidence_inputs;
#[cfg(test)]
pub(super) use super::run_provider_evidence::admit_provider_evidence_source;
#[cfg(test)]
pub(super) use super::run_provider_evidence::approved_macos_temporary_root;
#[cfg(test)]
pub(super) use super::run_provider_evidence::approved_provider_evidence_spellings;
#[cfg(test)]
pub(super) use super::run_provider_evidence::classified_absolute_host_paths_in_provider_prompt;
#[cfg(test)]
pub(super) use super::run_provider_evidence::macos_system_alias;
pub(crate) use super::run_provider_evidence::project_admitted_provider_evidence_inputs;
#[cfg(test)]
pub(super) use super::run_provider_evidence::project_admitted_provider_evidence_inputs_at;
#[cfg(test)]
pub(super) use super::run_provider_evidence::project_provider_evidence_inputs;
pub(crate) use super::run_provider_evidence::projected_provider_evidence;
#[cfg(test)]
pub(super) use super::run_provider_evidence::projected_provider_evidence_from_admitted;
pub(crate) use super::run_provider_evidence::projected_provider_evidence_paths;
pub(super) use super::run_provider_evidence::provider_evidence_controller_provenance_from_admitted;
pub(crate) use super::run_provider_evidence::rewrite_provider_evidence_prompt;
#[cfg(test)]
pub(super) use super::run_provider_evidence::secure_provider_evidence_copy;
#[cfg(test)]
pub(super) use super::run_provider_evidence::secure_provider_evidence_copy_with_limit;
pub(crate) use super::run_provider_evidence::validate_provider_evidence_inputs;
#[cfg(test)]
pub(super) use super::run_provider_evidence::validate_provider_evidence_prompt;
#[allow(unused_imports)] // Preserve the existing run module path.
pub(crate) use super::run_provider_evidence::AdmittedProviderEvidenceSource;
#[allow(unused_imports)] // Preserve the existing run module path.
pub(crate) use super::run_repository::canonical_remote_identity;
pub(super) use super::run_repository::cook_component_id;
pub(super) use super::run_repository::cook_component_path_for_repository_name;
#[allow(unused_imports)] // Preserve the existing run module path.
pub(super) use super::run_repository::cook_repository_identities_for_workspace;
pub(crate) use super::run_repository::cook_repository_names_for_selection;
pub(super) use super::run_repository::derived_cook_branch;
pub(super) use super::run_repository::durable_component_cwd;
pub(crate) use super::run_repository::existing_cook_caller_workspace;
pub(super) use super::run_repository::fresh_derived_cook_branch;
pub(super) use super::run_repository::normalize_cook_task_url;
#[allow(unused_imports)] // Preserve the existing run module path.
pub(crate) use super::run_repository::normalize_repository_name;
pub(crate) use super::run_repository::resolve_cook_repository;
pub(super) use super::run_repository::validate_cook_destination_identity;
#[allow(unused_imports)] // Preserve the existing run module path.
pub(crate) use super::run_repository::validate_cook_repository_component_selection;
pub(super) use super::run_repository::validate_cook_worktree_root;
#[allow(unused_imports)] // Preserve the existing run module path.
pub(super) use super::run_repository::CookRepositoryIdentity;

#[cfg(test)]
#[path = "run_preview_tests.rs"]
mod preview_tests;

#[cfg(test)]
#[path = "run_cook_pull_request_ref_tests.rs"]
mod cook_pull_request_ref_tests;

#[cfg(test)]
#[path = "run_require_model_override_acknowledgement_for_cook_tests.rs"]
mod require_model_override_acknowledgement_for_cook_tests;

#[cfg(test)]
#[path = "run_rotation_disclosure_tests.rs"]
mod rotation_disclosure_tests;

#[cfg(test)]
#[path = "run_prompt_input_tests.rs"]
mod prompt_input_tests;

#[cfg(test)]
#[path = "run_provider_evidence_tests.rs"]
mod provider_evidence_tests;

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;

#[cfg(test)]
use super::run_preview::MAX_PREVIEW_REPLAY_ARGS;
