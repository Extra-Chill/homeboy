//! Top-level `execute_lab_offload` orchestration and runner-support hints.

use super::*;

/// Record a freshly synced, remapped agent-task workspace entry: append it to
/// the workspace mapping and emit the matching Lab plan step. Shared by the
/// inline tasks-arg and plan-arg materialization in `run_lab_offload_inner`.
pub(crate) fn record_synced_remapped_workspace_entry(
    plan: HomeboyPlan,
    workspace_mapping: &mut Vec<super::super::super::lab_workspaces::LabWorkspaceMappingEntry>,
    entry: Option<super::super::super::lab_workspaces::LabWorkspaceMappingEntry>,
    step_id: &str,
) -> HomeboyPlan {
    let Some(entry) = entry else {
        return plan;
    };
    workspace_mapping.push(entry.clone());
    with_step(
        plan,
        PlanStep::ready(step_id, step_id)
            .inputs(PlanValues::new().json("workspace", &entry))
            .build(),
    )
}

pub fn execute_lab_offload(request: LabOffloadRequest<'_>) -> Result<LabOffloadOutcome> {
    let unsupported_runner_error = |runner_id: &str, message: String| {
        Error::validation_invalid_argument(
            "runner",
            message,
            Some(runner_id.to_string()),
            Some(unsupported_runner_hints(
                runner_id,
                request.normalized_args,
                lab_runner_support_summary().hint,
            )),
        )
    };
    let mut plan = base_lab_plan(request.command.as_ref());
    let Some(contract) = request.command.clone() else {
        if let Some(runner_id) = request.explicit_runner {
            if is_build_command(request.normalized_args) {
                return Err(unsupported_build_lab_error("runner", Some(runner_id)));
            }
            return Err(unsupported_runner_error(
                runner_id,
                lab_runner_support_summary().unsupported_message,
            ));
        }
        if request.local_policy.deny_local_execution() {
            if is_build_command(request.normalized_args) {
                return Err(unsupported_build_lab_error("lab_only", None));
            }
            return Err(local_execution_denied_error(
                "command has no Lab contract",
                None,
            ));
        }
        return Ok(LabOffloadOutcome::RunLocal {
            plan: disabled_select_runner_plan(plan, "command has no Lab contract"),
            metadata: None,
            messages: Vec::new(),
        });
    };

    if !contract.portable {
        if let Some(runner_id) = request.explicit_runner {
            let message = contract.unsupported_reason.map_or_else(
                || lab_runner_support_summary().unsupported_message,
                |reason| format!("--runner is unavailable for this local-only resource-pressure command. {reason}"),
            );
            return Err(unsupported_runner_error(runner_id, message));
        }
        let reason = contract
            .unsupported_reason
            .unwrap_or("command is local-only");
        if request.local_policy.deny_local_execution() {
            return Err(local_execution_denied_error(reason, None));
        }
        plan = disabled_select_runner_plan(plan, reason);
        return Ok(skipped_automatic_run_local(plan, reason));
    }

    if request.explicit_runner.is_none() && !contract.routing_policy.default_lab_offload {
        if request.local_policy.deny_local_execution() {
            return Err(local_execution_denied_error(
                "automatic Lab offload disabled",
                None,
            ));
        }
        return Ok(LabOffloadOutcome::RunLocal {
            plan: disabled_select_runner_plan(plan, "automatic Lab offload disabled"),
            metadata: None,
            messages: Vec::new(),
        });
    }

    preflight_required_git_checkout_workspace(
        contract.workspace_mode_policy,
        request.normalized_args,
    )?;

    let selection = resolve_lab_runner_selection(
        &contract,
        request.explicit_runner,
        request.force_hot,
        request.local_policy.allow_local_hot(),
    )?;
    let Some(selection) = selection else {
        let reason = if request.force_hot && request.local_policy.allow_local_hot() {
            "force_hot_local_override"
        } else if request.force_hot {
            "force_hot"
        } else {
            "no_default_runner"
        };
        plan = with_step(
            plan,
            PlanStep::builder(
                "lab.select_runner",
                "lab.select_runner",
                PlanStepStatus::Skipped,
            )
            .skip_reason(reason)
            .build(),
        );
        if request.local_policy.deny_local_execution() {
            return Err(local_execution_denied_error(reason, None));
        }
        return Ok(skipped_automatic_run_local(plan, reason));
    };

    let release_gate_local_hot_allowed =
        crate::core::defaults::resolve_release_gate_local_hot_policy().is_allowed();
    let mut messages = Vec::new();
    if matches!(selection.source, LabRunnerSelectionSource::Default) {
        // Make the auto-offload visible up front (#3815): the operator did not
        // ask for a runner explicitly, so spell out that this command is about
        // to leave the local machine and run remotely, on which runner, and how
        // to keep it local. Without this the first sign of remote execution is
        // a confusing remote-specific failure (e.g. a local `@file` that does
        // not exist on the runner).
        let local_hot_hint = if contract.routing_policy.release_gate
            && !release_gate_local_hot_allowed
        {
            " Release-gate local-hot fallback is disabled by `/release_gate/local_hot: fail_closed`; repair the runner instead of bypassing Lab routing."
        } else {
            " Pass `--force-hot --allow-local-hot` to run it locally instead."
        };
        let auto_offload_signal = format!(
            "Lab offload: auto-selected default {} runner `{}`; this command will run REMOTELY on that runner, not on this machine.{local_hot_hint}",
            selection.mode.label(),
            selection.runner_id
        );
        eprintln!("{auto_offload_signal}");
        messages.push(auto_offload_signal);
    }

    plan = with_step(
        plan,
        PlanStep::ready("lab.select_runner", "lab.select_runner")
            .inputs(
                PlanValues::new()
                    .string("runner_id", &selection.runner_id)
                    .string("source", selection.source.metadata_value())
                    .string("mode", selection.mode.metadata_value()),
            )
            .build(),
    );

    match prepare_lab_runner_for_offload(&selection)? {
        LabRunnerPreparation::Ready => {
            plan = with_step(
                plan,
                PlanStep::ready("lab.connect_runner", "lab.connect_runner").build(),
            );
        }
        LabRunnerPreparation::FallBackLocal { reason } => {
            plan = with_step(
                plan,
                PlanStep::builder(
                    "lab.connect_runner",
                    "lab.connect_runner",
                    PlanStepStatus::Failed,
                )
                .skip_reason(reason.clone())
                .build(),
            );
            // Release-gate routing safety (#4603): when a release gate's
            // default runner cannot be prepared for remote execution (e.g. a
            // stale daemon / version skew or a failed connection), silently
            // falling back to local execution produces a gate result that is
            // not faithful to the routing policy. Fail closed with a clear
            // diagnostic that surfaces the underlying runner reason, rather
            // than letting a stale launcher route the gate to the controller.
            // The operator-only override is `/release_gate/local_hot: allowed`.
            if contract.routing_policy.release_gate
                && matches!(selection.source, LabRunnerSelectionSource::Default)
                && !release_gate_local_hot_allowed
            {
                return Err(release_gate_local_hot_denied_error(
                    format!(
                        "Release gate `{}` selected default Lab runner `{}` but could not prepare it for remote execution ({}); `/release_gate/local_hot` is `fail_closed`, so the gate will not silently fall back to local execution",
                        contract.hot_label, selection.runner_id, reason
                    ),
                    "release_gate",
                ));
            }
            if request.local_policy.deny_local_execution() {
                return Err(local_execution_denied_error(
                    &reason,
                    Some(&selection.runner_id),
                ));
            }
            if !request.local_policy.allow_local_fallback() {
                return Err(selected_runner_fallback_error(
                    &selection,
                    "Lab offload selected a runner but could not prepare it for remote execution",
                    &reason,
                    vec![format!(
                        "Reconnect runner `{}` before retrying Lab offload.",
                        selection.runner_id
                    )],
                ));
            }
            return Ok(LabOffloadOutcome::RunLocal {
                metadata: Some(lab_offload_metadata(
                    &plan,
                    selection.source.metadata_value(),
                    Some(&selection.runner_id),
                    Some(selection.mode.metadata_value()),
                    "fallback",
                    None,
                    Some(&reason),
                )),
                plan,
                messages: vec![format!("Lab offload: {reason}; running locally.")],
            });
        }
    }

    run_lab_offload_inner(request, selection, contract, plan, messages)
}

pub(crate) fn unsupported_runner_hints(
    runner_id: &str,
    normalized_args: &[String],
    support_hint: String,
) -> Vec<String> {
    let mut hints = vec![support_hint];

    if let Some(commands) = review_lab_fallback_commands(runner_id, normalized_args) {
        hints.push(format!(
            "Scoped `homeboy review` cannot offload yet. Run these full-workspace Lab gates instead: {}; {}; {}.",
            commands.audit, commands.lint, commands.test
        ));
    }

    if let Some(service_command) = tunnel_service_command(normalized_args) {
        hints.push(format!(
            "`tunnel service {service_command} --runner {runner_id}` is not routed directly; inspect runner-side tunnel state with `homeboy runner exec {runner_id} --ssh --raw -- homeboy tunnel service {service_command} ...` until service inspection supports native --runner routing."
        ));
    }

    hints
}
