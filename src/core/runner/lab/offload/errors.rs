//! Durable-run-id emission, disconnect handling, capability/runner fallback
//! errors, missing-patch diagnostics, and runner failure-context helpers.

use super::*;

pub(crate) fn with_lab_apply_patch_step(
    plan: HomeboyPlan,
    apply_output: Option<RunnerWorkspaceApplyOutput>,
) -> HomeboyPlan {
    let mut inputs = PlanValues::new();
    if let Some(apply_output) = apply_output {
        inputs = inputs.json("apply", &apply_output);
    } else {
        inputs = inputs.json(
            "apply",
            serde_json::json!({
                "applied": false,
                "reason": "no_patch",
            }),
        );
    }

    with_step(
        plan,
        PlanStep::builder(
            "lab.apply_patch",
            "lab.apply_patch",
            PlanStepStatus::Success,
        )
        .inputs(inputs)
        .build(),
    )
}

/// Persist and print the durable agent-task run id *before* the long-running
/// provider execution starts (#5684).
///
/// For Lab/offloaded cooks the run id is the operator handle for status, logs,
/// artifacts, cancellation, retry and review. Provider execution can block the
/// foreground process well past a local shell timeout, so the handle must be
/// emitted up front: written immediately to the controller-local `--output`
/// file as structured JSON (when requested) and printed to stdout/stderr with
/// the exact follow-up commands. Even if the local process is then
/// timed out or interrupted, the run id remains discoverable from this initial
/// output rather than requiring the operator to guess from `agent-task active`
/// / `agent-task list` / `runs list`.
pub(crate) fn emit_durable_run_id_before_execution(
    run_id: &str,
    runner_id: &str,
    local_output_file: Option<&str>,
    messages: &mut Vec<String>,
) {
    let status_command = format!("homeboy agent-task status {run_id}");
    let logs_command = format!("homeboy agent-task logs {run_id}");

    if let Some(path) = local_output_file {
        let envelope = serde_json::json!({
            "success": true,
            "data": {
                "status": "dispatched_pending_execution",
                "schema": "homeboy/lab-offload-durable-run/v1",
                "durable_run_id": run_id,
                "run_id": run_id,
                "runner_id": runner_id,
                "note": "Durable agent-task run id persisted before long-running Lab execution. Track it with the retrieval commands; this file is overwritten with the final result when execution completes.",
                "retrieval_commands": {
                    "status": status_command,
                    "logs": logs_command,
                },
            },
        });
        let serialized = serde_json::to_string_pretty(&envelope)
            .unwrap_or_else(|_| format!("{{\"durable_run_id\":\"{run_id}\"}}"));
        if let Err(err) = write_local_output_file_atomically(path, &serialized) {
            eprintln!(
                "Lab offload: warning: could not pre-write durable run id `{run_id}` to --output `{path}`: {err}"
            );
        } else {
            eprintln!(
                "Lab offload: durable agent-task run id `{run_id}` written to --output `{path}` before execution."
            );
        }
    }

    eprintln!(
        "Lab offload: agent-task run id `{run_id}` persisted before provider execution starts."
    );
    eprintln!("Next: {status_command}");
    eprintln!("Next: {logs_command}");

    messages.push(format!(
        "Lab offload: durable agent-task run id `{run_id}` (persisted before execution). Track with `{status_command}` and `{logs_command}`."
    ));
}

/// Atomically write `contents` to `path` (temp file + rename) so a concurrent
/// reader never observes a half-written durable-run-id envelope.
pub(crate) fn write_local_output_file_atomically(
    path: &str,
    contents: &str,
) -> std::io::Result<()> {
    use std::io::Write;
    let target = std::path::Path::new(path);
    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("output");
    let temp_name = format!(".{file_name}.{}.tmp", std::process::id());
    let temp = target.with_file_name(temp_name);
    {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(contents.as_bytes())?;
        if !contents.ends_with('\n') {
            file.write_all(b"\n")?;
        }
        file.sync_all()?;
    }
    match std::fs::rename(&temp, target) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = std::fs::remove_file(&temp);
            Err(err)
        }
    }
}

pub(crate) fn in_flight_daemon_disconnect_error(
    runner_id: &str,
    job_id: &str,
    run_id: Option<&str>,
    reason: &str,
    err: &Error,
) -> Error {
    let runner_exec_prefix = "homeboy runner exec ".to_string() + runner_id + " --";
    let runner_runs_list =
        format!("{runner_exec_prefix} homeboy runs list --status running --limit 20");
    let runner_job_logs = format!("homeboy runner job logs {runner_id} {job_id} --follow");
    let runner_job_cancel = format!("homeboy runner job cancel {runner_id} {job_id}");
    let runner_run_show = format!("{runner_exec_prefix} homeboy runs show <run-id>");
    let runner_run_evidence = format!("{runner_exec_prefix} homeboy runs evidence <run-id>");
    let runner_run_artifacts = format!("{runner_exec_prefix} homeboy runs artifacts <run-id>");
    let mut disconnected = Error::new(
        ErrorCode::RunnerControllerDisconnected,
        format!(
            "Lab offload controller disconnected while runner `{runner_id}` daemon job `{job_id}` was still in flight; recover from the durable runner job id: {}",
            err.message
        ),
        serde_json::json!({
            "status": "recoverable_followup_required",
            "runner_id": runner_id,
            "job_id": job_id,
            "durable_run_id": run_id,
            "reason": reason,
            "recovery": {
                "mode": "durable_runner_job",
                "job_logs": runner_job_logs,
                "job_cancel": runner_job_cancel,
                "runner_runs_list": runner_runs_list,
                "runner_run_show": runner_run_show,
                "runner_run_evidence": runner_run_evidence,
                "runner_run_artifacts": runner_run_artifacts,
            },
            "source": err.details,
        }),
    );
    for hint in lab_offload_handoff_hints(
        runner_id,
        None,
        job_id,
        None,
        DaemonJobHandoffState::InFlight,
        true,
    ) {
        disconnected = disconnected.with_hint(hint);
    }
    disconnected.retryable = Some(true);
    disconnected
}

pub(crate) fn in_flight_daemon_disconnect_outcome(
    plan: HomeboyPlan,
    runner_id: &str,
    job_id: &str,
    run_id: &str,
    reason: &str,
    err: &Error,
) -> LabOffloadOutcome {
    let plan = with_step(
        plan,
        PlanStep::builder("lab.exec.detached", "lab.exec.detached", PlanStepStatus::PartialSuccess)
            .skip_reason(format!(
                "controller disconnected after durable run `{run_id}` dispatched to runner job `{job_id}`"
            ))
            .build(),
    );
    let error = in_flight_daemon_disconnect_error(runner_id, job_id, Some(run_id), reason, err);
    let details = serde_json::json!({
        "status": "dispatched_detached",
        "followup_required": true,
        "durable_run_id": run_id,
        "runner_id": runner_id,
        "job_id": job_id,
        "reason": reason,
        "message": error.message,
        "retrieval_commands": {
            "status": format!("homeboy agent-task status {run_id}"),
            "logs": format!("homeboy agent-task logs {run_id}"),
            "artifacts": format!("homeboy agent-task artifacts {run_id}"),
            "runner_job_logs": format!("homeboy runner job logs {runner_id} {job_id} --follow")
        }
    });
    let stdout = serde_json::to_string_pretty(&serde_json::json!({
        "success": true,
        "data": details,
    }))
    .unwrap_or_else(|_| {
        format!(
            "Lab offload detached after dispatch. Durable run `{run_id}` continues remotely; inspect with `homeboy agent-task status {run_id}`."
        )
    });
    let mut stderr = format!(
        "Lab offload detached after dispatch: durable agent-task run `{run_id}` continues remotely on runner `{runner_id}` daemon job `{job_id}`.\n"
    );
    stderr.push_str(&format!("Reason: {reason}\n"));
    stderr.push_str(&format!("Next: homeboy agent-task status {run_id}\n"));
    stderr.push_str(&format!("Next: homeboy agent-task logs {run_id}\n"));
    stderr.push_str(&format!("Next: homeboy agent-task artifacts {run_id}\n"));
    stderr.push_str(&format!(
        "Runner job: homeboy runner job logs {runner_id} {job_id} --follow\n"
    ));

    LabOffloadOutcome::Offloaded {
        plan,
        stdout: format!("{stdout}\n"),
        stderr,
        exit_code: 0,
        output_file_content: None,
    }
}

pub(crate) fn automatic_capability_fallback(
    plan: HomeboyPlan,
    runner_id: &str,
    runner_status: &RunnerStatusReport,
    reason: String,
) -> LabOffloadOutcome {
    LabOffloadOutcome::RunLocal {
        metadata: Some(lab_offload_metadata(
            &plan,
            "automatic",
            Some(runner_id),
            Some(status_tunnel_mode(runner_status).metadata_value()),
            "fallback",
            None,
            Some(&reason),
        )),
        plan,
        messages: vec![format!("Lab offload: {reason}; running locally.")],
    }
}

pub(crate) fn automatic_capability_fallback_or_error(
    plan: HomeboyPlan,
    selection: &LabRunnerSelection,
    runner_status: &RunnerStatusReport,
    reason: String,
    remediation: Vec<String>,
    allow_local_fallback: bool,
    deny_local_execution: bool,
) -> Result<LabOffloadOutcome> {
    if deny_local_execution {
        return Err(local_execution_denied_error(
            &reason,
            Some(&selection.runner_id),
        ));
    }
    if !allow_local_fallback {
        return Err(selected_runner_fallback_error(
            selection,
            "Lab offload selected a runner that is missing required capability parity",
            &reason,
            remediation,
        ));
    }

    Ok(automatic_capability_fallback(
        plan,
        &selection.runner_id,
        runner_status,
        reason,
    ))
}

pub(crate) fn selected_runner_fallback_error(
    selection: &LabRunnerSelection,
    message: &str,
    reason: &str,
    mut remediation: Vec<String>,
) -> Error {
    remediation.push(
        "Pass --allow-local-fallback only when local execution is intentional and safe for this controller."
            .to_string(),
    );

    Error::validation_invalid_argument(
        "runner",
        format!("{message}: {reason}"),
        Some(selection.runner_id.clone()),
        Some(remediation),
    )
}

/// Build an actionable diagnostic when a Lab offload write/fix command
/// finished cleanly but the runner returned no source-tree patch.
pub(crate) fn missing_mutation_patch_error(
    normalized_args: &[String],
    mutation_flag: Option<&str>,
    exec_output: &super::super::super::RunnerExecOutput,
) -> Error {
    let flag_label = mutation_flag.unwrap_or("write");
    let original_command = redact_argv_display(normalized_args);
    let remote_command = redact_argv_display(&exec_output.argv);
    let patch_artifact_id = exec_output
        .patch
        .as_ref()
        .and_then(|patch| patch.get("patch_artifact_id"))
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.trim().is_empty());
    let patch_artifact_path = exec_output
        .patch
        .as_ref()
        .and_then(|patch| patch.get("patch_artifact_path"))
        .and_then(serde_json::Value::as_str)
        .filter(|path| !path.trim().is_empty());
    let mut error = Error::new(
        ErrorCode::ValidationInvalidArgument,
        format!(
            "Lab offload write command completed on runner `{}` but returned no source-tree patch to apply for `{flag_label}`",
            exec_output.runner_id
        ),
        serde_json::json!({
            "field": "lab_offload_patch",
            "problem": "missing required source-tree mutation patch",
            "runner_id": exec_output.runner_id,
            "job_id": exec_output.job_id,
            "mirror_run_id": exec_output.mirror_run_id,
            "remote_workspace": exec_output.remote_cwd,
            "remote_command": remote_command,
            "original_command": original_command,
            "mutation_flag": mutation_flag,
            "patch_artifact_id": patch_artifact_id,
            "patch_artifact_path": patch_artifact_path,
            "patch": exec_output.patch,
        }),
    );

    if let Some(run_id) = exec_output.mirror_run_id.as_deref() {
        error = error
            .with_hint(format!("Inspect the Lab run with `homeboy runs show {run_id}`."))
            .with_hint(format!(
                "List mirrored Lab artifacts with `homeboy runs artifacts {run_id}` and verify the runner produced a lint/refactor patch artifact."
            ));
    } else if let Some(job_id) = exec_output.job_id.as_deref() {
        error = error.with_hint(format!(
            "Runner daemon job `{job_id}` finished without a patch artifact; inspect runner job evidence before retrying."
        ));
    }

    if !original_command.is_empty() {
        error = error.with_hint(format!(
            "After runner patch capture is available, retry the intended Homeboy write path: `{original_command}`."
        ));
    }

    error
}

pub(crate) fn append_runner_failure_context_summary(
    stderr: &mut String,
    exec_output: &crate::core::runner::RunnerExecOutput,
) {
    let Some(context) = runner_exec_failure_context_from_output(exec_output) else {
        return;
    };
    let job = context.job_id.as_deref().unwrap_or("unknown runner job");
    let run = context
        .persisted_run_id
        .as_deref()
        .unwrap_or("unknown persisted run");
    let field = context
        .contract_field
        .as_deref()
        .unwrap_or("unknown contract field");
    stderr.push_str(&format!(
        "Lab offload failure context: command `{}` failed on runner `{}`; runner job `{job}`; persisted run `{run}`; contract field `{field}`; reason: {}.\n",
        redact_argv_display(&context.command),
        context.runner_id,
        context.reason
    ));
}

pub(crate) fn append_runner_component_registry_repair_hint(
    stderr: &mut String,
    contract: &LabOffloadCommand,
    runner_id: &str,
    remote_cwd: &str,
    stdout: &str,
    command_stderr: &str,
) {
    if !contract.routing_policy.release_gate
        || !contains_component_not_found(stdout) && !contains_component_not_found(command_stderr)
    {
        return;
    }

    stderr.push_str(&format!(
        "Lab runner registry repair: runner `{runner_id}` did not know the component metadata required by this release gate. Register the synced runner checkout, then retry the original gate: `homeboy runner exec {runner_id} -- homeboy component create --local-path {}`. Inspect runner-side registry state with `homeboy runner exec {runner_id} -- homeboy component list`.\n",
        shell_arg(remote_cwd)
    ));
}

pub(crate) fn contains_component_not_found(output: &str) -> bool {
    output.contains("component.not_found") || output.contains("Component not found")
}

/// Known orchestration context for a Lab offload pre-execution stage. Every
/// field that is populated is woven into a Lab-cannot-proceed error so the
/// operator can self-serve a fix without SSH-ing into the runner to reconstruct
/// which runner/workspace/ref/dependency the offload was working against
/// (#4336).
#[derive(Debug, Clone, Default)]
pub(crate) struct LabOrchestrationContext {
    /// Runner the offload selected to execute on.
    pub(crate) runner_id: Option<String>,
    /// Controller-local primary workspace path being materialized to the runner.
    pub(crate) workspace_path: Option<String>,
    /// Requested ref/base (e.g. the `--changed-since` ref) when one was named.
    pub(crate) ref_base: Option<String>,
    /// Dependency (checkout/override) at fault, when the failure is attributable
    /// to a specific declared dependency rather than the primary workspace.
    pub(crate) dependency: Option<String>,
}

impl LabOrchestrationContext {
    pub(crate) fn for_runner_workspace(runner_id: &str, workspace_path: &str) -> Self {
        Self {
            runner_id: Some(runner_id.to_string()),
            workspace_path: Some(workspace_path.to_string()),
            ..Self::default()
        }
    }

    pub(crate) fn with_ref_base(mut self, ref_base: Option<String>) -> Self {
        self.ref_base = ref_base.filter(|value| !value.trim().is_empty());
        self
    }

    /// True once at least one orchestration fact is known and worth surfacing.
    fn has_context(&self) -> bool {
        self.runner_id.is_some()
            || self.workspace_path.is_some()
            || self.ref_base.is_some()
            || self.dependency.is_some()
    }
}

/// Enrich a Lab-cannot-proceed error with the orchestration context the
/// operator needs to self-serve a fix: the selected runner, the primary
/// workspace path, the ref/base, the dependency at fault (when known), plus a
/// concrete Homeboy command to reconcile runner state and retry.
///
/// Idempotent: re-enriching an already-enriched error (e.g. when the same error
/// bubbles through nested stages) does not duplicate the context block or the
/// fix hints. Keeps the original error code/message; only adds details + hints.
pub(crate) fn enrich_lab_cannot_proceed_error(
    mut error: Error,
    context: &LabOrchestrationContext,
) -> Error {
    if !context.has_context() {
        return error;
    }
    // Idempotency guard: only attach the orchestration context once.
    if error.details.get("lab_orchestration_context").is_some() {
        return error;
    }

    if error.details.is_object() {
        if let Some(map) = error.details.as_object_mut() {
            map.insert(
                "lab_orchestration_context".to_string(),
                serde_json::json!({
                    "runner_id": context.runner_id,
                    "workspace_path": context.workspace_path,
                    "ref_base": context.ref_base,
                    "dependency": context.dependency,
                }),
            );
        }
    }

    if let Some(runner_id) = &context.runner_id {
        error = error.with_hint(format!(
            "Lab cannot proceed on selected runner `{runner_id}`."
        ));
    }
    if let Some(workspace_path) = &context.workspace_path {
        error = error.with_hint(format!("Primary workspace: `{workspace_path}`."));
    }
    if let Some(ref_base) = &context.ref_base {
        error = error.with_hint(format!("Requested ref/base: `{ref_base}`."));
    }
    if let Some(dependency) = &context.dependency {
        error = error.with_hint(format!("Dependency at fault: `{dependency}`."));
    }

    // Concrete Homeboy command to fix it. Kept generic (homeboy subcommands
    // only — no ecosystem tooling) so the core-agnostic-source gate stays green.
    if let Some(runner_id) = &context.runner_id {
        error = error.with_hint(format!(
            "Fix runner state and retry: `homeboy runner status {runner_id}`, then re-materialize dependencies with `homeboy deps install` and re-run the Lab command."
        ));
    } else {
        error = error.with_hint(
            "Fix runner state and retry: select an available runner with `homeboy runner status`, re-materialize dependencies with `homeboy deps install`, and re-run the Lab command."
                .to_string(),
        );
    }

    error
}
