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
    if should_skip_managed_runner_placement(
        request.placement,
        request.explicit_runner,
        homeboy_core::resource_policy_context::is_managed_runner_placement_context(),
    ) {
        return Ok(LabOffloadOutcome::RunLocal {
            plan: disabled_select_runner_plan(
                base_lab_plan(request.command.as_ref()),
                "runner placement already resolved",
            ),
            metadata: None,
            messages: Vec::new(),
        });
    }
    let unsupported_runner_error = |runner_id: &str, message: String| {
        Error::validation_invalid_argument(
            "runner",
            message,
            Some(runner_id.to_string()),
            Some(unsupported_runner_hints(
                runner_id,
                request.normalized_args,
                resolve_lab_runner_hint().hint,
            )),
        )
    };
    let mut plan = base_lab_plan(request.command.as_ref());
    let Some(mut contract) = request.command.clone() else {
        if let Some(runner_id) = request.explicit_runner {
            if is_build_command(request.normalized_args) {
                return Err(unsupported_build_lab_error("runner", Some(runner_id)));
            }
            return Err(unsupported_runner_error(
                runner_id,
                resolve_lab_runner_hint().unsupported_message,
            ));
        }
        if request.placement == homeboy_cli_contract::Placement::Lab {
            if is_build_command(request.normalized_args) {
                return Err(unsupported_build_lab_error("placement_lab", None));
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

    if let homeboy_core::lab_contract::LabCommandPortability::LocalOnly(reason) =
        contract.portability
    {
        if let Some(runner_id) = request.explicit_runner {
            let message = format!(
                "--runner is unavailable for this local-only resource-pressure command. {reason}"
            );
            return Err(unsupported_runner_error(runner_id, message));
        }
        if request.placement == homeboy_cli_contract::Placement::Lab {
            return Err(local_execution_denied_error(reason, None));
        }
        plan = disabled_select_runner_plan(plan, reason);
        return Ok(skipped_automatic_run_local(plan, reason));
    }

    // Commands that explicitly prefer Lab retain that contract regardless of
    // controller pressure. Other portable workspace commands use the single
    // preflight snapshot captured by CliRuntime: cold controllers stay local,
    // while warm/hot controllers may use an eligible default Lab runner.
    //
    // Cheap commands (`offload_only_when_hot`) require a genuinely `hot`
    // machine before auto-offloading, so a merely `warm` controller does not
    // pay the full Lab round-trip for work that finishes faster locally.
    if !contract.routing_policy.default_lab_offload
        && request.placement == homeboy_cli_contract::Placement::Auto
        && contract.source_path_mode
            != homeboy_core::lab_contract::LabSourcePathMode::RunnerResident
        && homeboy_core::resource_policy_context::captured_context().is_some_and(|context| {
            contract
                .routing_policy
                .should_pressure_offload(&context.severity)
        })
    {
        contract.routing_policy.default_lab_offload = true;
    }

    if request.explicit_runner.is_none()
        && !contract.routing_policy.default_lab_offload
        && !request.placement.requests_lab()
    {
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

    // Begin runner-agnostic overhead accounting (#3001). Each setup phase
    // (selection, preflight, workspace sync, output parse, artifact import) is
    // timed independently so reports can separate `lab_overhead_ms` from the
    // workload command duration, regardless of runner transport.
    let mut overhead = LabOffloadOverhead::start();

    let selection_timer = overhead.phase(LabOffloadPhase::Selection);
    let selection =
        resolve_lab_runner_selection(&contract, request.explicit_runner, request.placement)?;
    selection_timer.finish();
    let Some(selection) = selection else {
        if request.placement == homeboy_cli_contract::Placement::Auto {
            fail_if_no_default_runner_accepts_jobs(&contract)?;
        }
        let reason = if request.placement == homeboy_cli_contract::Placement::Local {
            "placement_local_override"
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
        if request.placement == homeboy_cli_contract::Placement::Lab {
            return Err(local_execution_denied_error(reason, None));
        }
        // No runner was selected: record the skip reason as the fallback so the
        // overhead metadata consistently explains why this ran locally.
        overhead.set_fallback_reason(reason);
        return Ok(skipped_automatic_run_local_with_overhead(
            plan, reason, &overhead,
        ));
    };

    // A runner was selected (explicit or default). Record the attempted
    // selection up front so a later connect/preflight fallback still reports
    // what the offload tried first.
    overhead.set_attempted(
        &selection.runner_id,
        selection.source.metadata_value(),
        Some(selection.mode.metadata_value()),
    );

    let release_gate_local_hot_allowed =
        homeboy_core::defaults::resolve_release_gate_local_hot_policy().is_allowed();
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
            " Pass `--placement local` to run it locally instead."
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

    let prepare_timer = overhead.phase(LabOffloadPhase::Preflight);
    let preparation = prepare_lab_runner_for_offload(&selection)?;
    prepare_timer.finish();
    let runner_status = match preparation {
        LabRunnerPreparation::Ready => {
            // Only a detached, controller-owned agent-task plan has a durable
            // continuation and canonical workload suitable for broker queueing.
            // A full runner is otherwise still a readiness failure.
            let runner_status = match preflight_lab_runner_availability(
                &contract,
                &selection,
                request.detach_after_handoff,
                request.durable_agent_task_plan.is_some(),
            ) {
                Ok(status) => status,
                Err(error) => {
                    if contract.routing_policy.release_gate
                        && matches!(selection.source, LabRunnerSelectionSource::Default)
                        && !release_gate_local_hot_allowed
                    {
                        return Err(release_gate_local_hot_denied_error(
                        format!(
                            "Release gate `{}` selected default Lab runner `{}` but it cannot accept jobs ({}); `/release_gate/local_hot` is `fail_closed`, so the gate will not silently fall back to local execution",
                            contract.hot_label, selection.runner_id, error.message
                        ),
                        "release_gate",
                    ));
                    }
                    if request.placement == homeboy_cli_contract::Placement::Auto
                        && matches!(selection.source, LabRunnerSelectionSource::Default)
                    {
                        let reason = format!("runner_unavailable: {}", error.message);
                        overhead.set_fallback_reason(&reason);
                        let mut metadata = lab_offload_metadata(
                            &plan,
                            selection.source.metadata_value(),
                            Some(&selection.runner_id),
                            Some(selection.mode.metadata_value()),
                            "fallback",
                            None,
                            Some(&reason),
                        );
                        attach_lab_offload_overhead(&mut metadata, &overhead);
                        return Ok(LabOffloadOutcome::RunLocal {
                            metadata: Some(metadata),
                            plan,
                            messages: vec![format!("Lab offload: {reason}; running locally.")],
                        });
                    }
                    return Err(error);
                }
            };
            plan = with_step(
                plan,
                PlanStep::ready("lab.connect_runner", "lab.connect_runner").build(),
            );
            runner_status
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
            if request.placement == homeboy_cli_contract::Placement::Lab {
                return Err(local_execution_denied_error(
                    &reason,
                    Some(&selection.runner_id),
                ));
            }
            if !request.allow_local_fallback {
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
            overhead.set_fallback_reason(&reason);
            let mut metadata = lab_offload_metadata(
                &plan,
                selection.source.metadata_value(),
                Some(&selection.runner_id),
                Some(selection.mode.metadata_value()),
                "fallback",
                None,
                Some(&reason),
            );
            attach_lab_offload_overhead(&mut metadata, &overhead);
            return Ok(LabOffloadOutcome::RunLocal {
                metadata: Some(metadata),
                plan,
                messages: vec![format!("Lab offload: {reason}; running locally.")],
            });
        }
    };

    run_lab_offload_inner(
        request,
        selection,
        contract,
        plan,
        messages,
        overhead,
        runner_status,
    )
}

fn should_skip_managed_runner_placement(
    placement: homeboy_cli_contract::Placement,
    explicit_runner: Option<&str>,
    managed_runner_placement: bool,
) -> bool {
    placement == homeboy_cli_contract::Placement::Auto
        && explicit_runner.is_none()
        && managed_runner_placement
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_runner_is_not_suppressed_by_resolved_automatic_placement() {
        // A controller re-entering from a managed runner must still honor a new
        // explicit selection so selection can report that runner's own status.
        assert!(should_skip_managed_runner_placement(
            homeboy_cli_contract::Placement::Auto,
            None,
            true,
        ));
        assert!(!should_skip_managed_runner_placement(
            homeboy_cli_contract::Placement::Auto,
            Some("homeboy-lab"),
            true,
        ));
    }
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
