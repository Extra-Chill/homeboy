use serde_json::Value;

use crate::cli_surface::{CommandArgumentProvenance, Commands, Placement};
use crate::command_contract::CommandSpec;

use super::agent_task_summary::{agent_task_summary_kind, render_agent_task_summary};
use super::output_runtime::{CommandPresentation, CommandRun};
use super::{adapter, runner};

type JsonRun = (homeboy::core::Result<Value>, i32);

const MAX_REFRESH_PROJECTION_BYTES: usize = 8 * 1024;
const MAX_REFRESH_TEXT_BYTES: usize = 256;
const MAX_REFRESH_PHASES: usize = 12;
const MAX_RELEASE_STDOUT_BYTES: usize = 16 * 1024;
const MAX_RELEASE_ARTIFACTS: usize = 16;
const MAX_RELEASE_URLS: usize = 16;
const MAX_RELEASE_WARNINGS: usize = 8;
const MAX_RELEASE_EVIDENCE_REFS: usize = 16;
const MAX_RELEASE_ACTIONS: usize = 4;
const MAX_RELEASE_TEXT_BYTES: usize = 512;

/// Dispatch a command to its handler and map the structured result to JSON.
pub fn run(
    command: Commands,
    spec: &CommandSpec,
    placement: Placement,
) -> (homeboy::core::Result<Value>, i32) {
    crate::commands::utils::tty::status("homeboy is working...");

    dispatch(command, spec, placement)
}

pub(crate) fn run_command_output(
    command: Commands,
    spec: &CommandSpec,
    output_file: Option<&str>,
    provenance: &CommandArgumentProvenance,
    placement: Placement,
) -> CommandRun {
    crate::commands::utils::tty::status("homeboy is working...");
    let run = match command {
        Commands::AgentTask(mut args) => {
            let summary_kind = agent_task_summary_kind_for_output(&args);
            let bounded_operation = agent_task_bounded_operation(&args);
            let redact_cancel = matches!(
                &args.command,
                crate::commands::agent_task::AgentTaskCommand::Cancel(_)
            );
            if matches!(
                &args.command,
                crate::commands::agent_task::AgentTaskCommand::Cook(cook_args) if !cook_args.preview
            ) {
                if let Some(path) = output_file {
                    let full = agent_task_requests_full_output(&args);
                    let lease = match super::output_runtime::CookOutputLease::claim(path) {
                        Ok(lease) => lease,
                        Err(error) => {
                            return CommandRun::from_stdout_result(Err(error), 2)
                                .with_command(spec.name)
                                // The rejected invocation never owns this path.
                                .with_output_file_already_written();
                        }
                    };
                    // The output lease is deliberately silent until this parent
                    // exists. A killed client can therefore always resolve or
                    // cancel the identity in its first published envelope.
                    let cook_args = match &mut args.command {
                        crate::commands::agent_task::AgentTaskCommand::Cook(cook_args) => cook_args,
                        _ => unreachable!("Cook output branch has a Cook command"),
                    };
                    if let Err(error) =
                        crate::commands::agent_task::run::preflight_cook_execution_request(
                            cook_args,
                            Some(provenance),
                        )
                    {
                        let result = Err(error);
                        let _ = lease.finish(
                            &result,
                            2,
                            &crate::commands::utils::response::CommandIdentity::with_operation(
                                "agent-task",
                                "cook",
                            ),
                            None,
                        );
                        return CommandRun::from_stdout_result(result, 2)
                            .with_command(spec.name)
                            .with_output_file_already_written();
                    }
                    let cook_id = cook_args
                        .dispatch
                        .run_id
                        .clone()
                        .unwrap_or_else(|| format!("agent-task-{}", uuid::Uuid::new_v4()));
                    cook_args.dispatch.run_id = Some(cook_id.clone());
                    let launcher_id = uuid::Uuid::new_v4().to_string();
                    let bootstrap = (|| {
                        let store = homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
                        // Local detach claims first and may already have
                        // published supervision. Reuse that owner id so this
                        // bootstrap is an identity-matched re-entry, not a
                        // second launcher stealing the handoff (#14768). A
                        // missing parent still needs a live claim so a bare
                        // record is never parked without an owning PID
                        // (#14710, #14706).
                        let launcher_id = store
                            .read_record(&cook_id)
                            .ok()
                            .and_then(|record| {
                                record.metadata["detached_cook_handoff"]["launcher_id"]
                                    .as_str()
                                    .filter(|id| !id.is_empty())
                                    .map(str::to_string)
                            })
                            .unwrap_or(launcher_id);
                        homeboy::agents::agent_task_lifecycle::claim_detached_cook_handoff_parent_in_store(
                            &store, &cook_id, &launcher_id,
                        )
                    })();
                    if let Err(error) = bootstrap {
                        let result = Err(error);
                        let _ = lease.finish(
                            &result,
                            2,
                            &crate::commands::utils::response::CommandIdentity::with_operation(
                                "agent-task",
                                "cook",
                            ),
                            None,
                        );
                        return CommandRun::from_stdout_result(result, 2)
                            .with_command(spec.name)
                            .with_output_file_already_written();
                    }
                    if let Err(error) = lease.progress(
                        "submission_bootstrap",
                        Some(&cook_id),
                        Some(&cook_id),
                        Some("durable Cook submission is preparing"),
                    ) {
                        let result: homeboy::core::Result<Value> =
                            terminalize_cook_output_bootstrap_failure(&cook_id, &error)
                                .and(Err(error));
                        let _ = lease.finish(
                            &result,
                            2,
                            &crate::commands::utils::response::CommandIdentity::with_operation(
                                "agent-task",
                                "cook",
                            ),
                            None,
                        );
                        return CommandRun::from_stdout_result(result, 2)
                            .with_command(spec.name)
                            .with_output_file_already_written();
                    }
                    let progress =
                        |phase: &str,
                         cook_id: Option<&str>,
                         run_id: Option<&str>,
                         activity: Option<&str>,
                         _terminal_retry_command: Option<&str>| {
                            lease.progress(phase, cook_id, run_id, activity)
                        };
                    let (result, exit_code) = map(
                        crate::commands::agent_task::run_with_cook_progress_and_provenance(
                            args,
                            Some(&progress),
                            Some(provenance),
                        ),
                    );
                    if let Err(error) = &result {
                        // A bootstrap parent is a real lifecycle record, not an
                        // output-only marker. If normal preparation never
                        // materialized its first attempt, terminalize that parent
                        // before replacing the in-flight envelope.
                        let terminalize = (|| {
                            let store = homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
                            if store.cook_index_exists(&cook_id) {
                                return Ok(());
                            }
                            let plan = store.read_controller_plan(&cook_id)?;
                            homeboy::agents::agent_task_lifecycle::record_pre_execution_failure_in_store(
                                &store,
                                &cook_id,
                                &plan,
                                "output_bootstrap",
                                error,
                            )?;
                            Ok(())
                        })();
                        if let Err(terminal_error) = terminalize {
                            let result = Err(terminal_error);
                            let _ = lease.finish(
                                &result,
                                2,
                                &crate::commands::utils::response::CommandIdentity::with_operation(
                                    "agent-task",
                                    "cook",
                                ),
                                None,
                            );
                            return CommandRun::from_stdout_result(result, 2)
                                .with_command(spec.name)
                                .with_output_file_already_written();
                        }
                    }
                    if let Err(error) = lease.finish(
                        &result,
                        exit_code,
                        &crate::commands::utils::response::CommandIdentity::with_operation(
                            "agent-task",
                            "cook",
                        ),
                        None,
                    ) {
                        return CommandRun::from_stdout_result(Err(error), 2)
                            .with_command(spec.name);
                    }
                    return agent_task_command_run(
                        result,
                        exit_code,
                        summary_kind,
                        full,
                        bounded_operation,
                        redact_cancel,
                    )
                    .with_command(spec.name)
                    .with_output_file_already_written();
                }
            }
            let full = agent_task_requests_full_output(&args);
            let result = dispatch(Commands::AgentTask(args), spec, placement);
            agent_task_command_run(
                result.0,
                result.1,
                summary_kind,
                full,
                bounded_operation,
                redact_cancel,
            )
        }
        Commands::Runner(args) if refresh_homeboy_uses_bounded_output(&args) => {
            refresh_homeboy_command_run(args, output_file)
        }
        Commands::Ssh(args)
            if matches!(
                args.subcommand,
                Some(super::ssh::SshSubcommand::List { full: false })
            ) =>
        {
            let result = dispatch(Commands::Ssh(args), spec, placement);
            super::ssh::compact_list_command_run(result.0, result.1)
        }
        Commands::Runner(args) if runner::is_compact_doctor_stdout(&args) => {
            let result = dispatch(Commands::Runner(args), spec, placement);
            super::runner::doctor::compact_command_run(result.0, result.1)
        }
        Commands::Runner(args) => runner::run_command_output(args),
        Commands::Bench(args) => {
            let summarize = args.is_run_invocation() && !args.wants_full_json();
            command_run_with_summary(
                dispatch(Commands::Bench(args), spec, placement),
                |payload, _| {
                    summarize
                        .then(|| super::bench_summary::render_bench_summary(payload))
                        .flatten()
                },
            )
        }
        Commands::Cleanup(args) => {
            let summarize = matches!(
                args.command,
                Some(crate::commands::cleanup::CleanupCommand::Artifacts(_))
                    | Some(crate::commands::cleanup::CleanupCommand::AutomaticRetention)
            ) && !homeboy::core::lab_routing::is_lab_offload_subprocess();
            command_run_with_summary(
                dispatch(Commands::Cleanup(args), spec, placement),
                |payload, _| {
                    summarize
                        .then(|| super::cleanup::render_cleanup_summary(payload))
                        .flatten()
                },
            )
        }
        Commands::Runs(args) => {
            let operator_output = !homeboy::core::lab_routing::is_lab_offload_subprocess();
            let summarize_show = args.show_summary_eligible() && operator_output;
            let summarize_dossier = args.dossier_summary_eligible() && operator_output;
            let summarize_proof = args.proof_summary_eligible() && operator_output;
            let result = dispatch(Commands::Runs(args), spec, placement);
            if summarize_show {
                runs_show_command_run(result)
            } else {
                command_run_with_summary(result, |payload, _| {
                    if let Some(rendered) =
                        super::runs_summary::render_runs_field_selection(payload)
                    {
                        Some(rendered)
                    } else if summarize_show {
                        super::runs_summary::render_runs_show_summary(payload)
                    } else if summarize_dossier {
                        super::runs_dossier_summary::render_runs_dossier_summary(payload)
                    } else if summarize_proof {
                        super::runs_proof_summary::render_runs_proof_summary(payload)
                    } else {
                        None
                    }
                })
            }
        }
        Commands::Release(args) => {
            let full = args.requests_full_output();
            release_command_run(
                dispatch(Commands::Release(args), spec, placement),
                output_file,
                full,
            )
        }
        command => {
            let (stdout_result, exit_code) = dispatch(command, spec, placement);
            CommandRun::from_stdout_result(stdout_result, exit_code)
        }
    };

    run.with_command(spec.name)
}

fn terminalize_cook_output_bootstrap_failure(
    cook_id: &str,
    error: &homeboy::core::Error,
) -> homeboy::core::Result<()> {
    let store =
        homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    let plan = store.read_controller_plan(cook_id)?;
    homeboy::agents::agent_task_lifecycle::record_pre_execution_failure_in_store(
        &store,
        cook_id,
        &plan,
        "output_bootstrap",
        error,
    )?;
    Ok(())
}

fn runs_show_command_run((output_file_result, exit_code): JsonRun) -> CommandRun {
    let stdout_result = output_file_result
        .clone()
        .map(|payload| super::runs_summary::project_runs_show_output(&payload));
    let summary_stdout = output_file_result
        .as_ref()
        .ok()
        .and_then(super::runs_summary::render_runs_show_summary);
    CommandRun::from_command_stdout_result("runs", stdout_result, exit_code)
        .with_output_file_result(output_file_result)
        .with_presentation(CommandPresentation {
            stdout: summary_stdout,
            stderr: None,
        })
}

/// Release payloads contain execution plans and step transcripts that can be
/// several megabytes. Keep the lossless result for `--output` and make stdout
/// an operator-facing projection unless the caller explicitly asks for `--full`.
fn release_command_run(
    (output_file_result, exit_code): JsonRun,
    output_file: Option<&str>,
    full: bool,
) -> CommandRun {
    let stdout_result = output_file_result.clone().map(|payload| {
        if full || !is_single_release_execution(&payload) {
            payload
        } else {
            bounded_release_projection(&payload, exit_code, output_file)
        }
    });

    CommandRun::from_command_stdout_result("release", stdout_result, exit_code)
        .with_output_file_result(output_file_result)
}

fn is_single_release_execution(payload: &Value) -> bool {
    payload.get("command").and_then(Value::as_str) == Some("release")
        && payload.get("variant").and_then(Value::as_str) == Some("single")
}

fn bounded_release_projection(payload: &Value, exit_code: i32, output_file: Option<&str>) -> Value {
    let result = payload.get("result").unwrap_or(payload);
    let run = result.get("run");
    let run_result = run.and_then(|run| run.get("result"));
    let steps = run_result
        .and_then(|result| result.get("steps"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let step = |name: &str| {
        steps.iter().find(|step| {
            step.get("id").and_then(Value::as_str) == Some(name)
                || step.get("type").and_then(Value::as_str) == Some(name)
        })
    };
    let step_data = |name: &str| step(name).and_then(|step| step.get("data"));
    // A rolled-back release still created real artifacts. The pipeline records
    // them here, so the operator summary must read them instead of reporting
    // null just because the owning step's data is gone.
    let rollback = run_result.and_then(|value| value.get("rollback"));
    let rollback_str = |key: &str| {
        rollback
            .and_then(|value| value.get(key))
            .and_then(Value::as_str)
            .map(bounded_release_text)
    };
    let version = step_data("version");
    let artifacts = step_data("artifacts.authority")
        .and_then(|data| data.get("artifacts"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(MAX_RELEASE_ARTIFACTS)
        .map(|artifact| {
            serde_json::json!({
                "name": artifact.get("path").and_then(Value::as_str).map(|path| bounded_release_text(&release_artifact_name(path))),
                "sha256": artifact.get("sha256").and_then(Value::as_str).map(bounded_release_text),
            })
        })
        .collect::<Vec<_>>();
    let publications = steps
        .iter()
        .filter(|step| {
            step.get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind == "github.release" || kind.starts_with("publish."))
        })
        .filter_map(|step| {
            let data = step.get("data")?;
            let url = release_publication_url(data)?;
            Some(serde_json::json!({
                "target": step.get("type").and_then(Value::as_str).map(bounded_release_text),
                "url": bounded_release_text(url),
            }))
        })
        .take(MAX_RELEASE_URLS)
        .collect::<Vec<_>>();
    let warnings = run_result
        .and_then(|result| result.get("warnings"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .take(MAX_RELEASE_WARNINGS)
        .map(bounded_release_text)
        .collect::<Vec<_>>();
    let mut evidence_refs = result
        .get("readiness")
        .and_then(|readiness| readiness.get("evidence_refs"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(bounded_release_text)
        .collect::<Vec<_>>();
    for key in ["continuation_ref", "reconciliation_ref", "owner_run_ref"] {
        if let Some(reference) = payload
            .get("workspace")
            .and_then(|workspace| workspace.get(key))
            .and_then(Value::as_str)
        {
            evidence_refs.push(bounded_release_text(reference));
        }
    }
    if let Some(path) = output_file {
        evidence_refs.push(format!("output://{}", bounded_release_text(path)));
    }
    evidence_refs.sort();
    evidence_refs.dedup();
    evidence_refs.truncate(MAX_RELEASE_EVIDENCE_REFS);
    let failed_step = steps.iter().find(|step| {
        matches!(
            step.get("status").and_then(Value::as_str),
            Some("failed") | Some("missing")
        )
    });
    let failure = failed_step.map(|step| {
        serde_json::json!({
            "step": step.get("id").and_then(Value::as_str).map(bounded_release_text),
            "type": step.get("type").and_then(Value::as_str).map(bounded_release_text),
            "cause": step.get("error").and_then(Value::as_str).map(bounded_release_text),
            "reproduction_commands": release_reproduction_commands(step, result),
        })
    });
    let projection = serde_json::json!({
        "schema": "homeboy/release-operator-summary/v1",
        "command": "release",
        "exit_code": exit_code,
        "component": result.get("component_id").and_then(Value::as_str).map(bounded_release_text),
        "status": result.get("status").and_then(Value::as_str).map(bounded_release_text),
        "phase": result.get("phase").and_then(Value::as_str).map(bounded_release_text),
        "old_version": version.and_then(|data| data.get("old_version")).and_then(Value::as_str).map(bounded_release_text),
        "new_version": result.get("new_version").and_then(Value::as_str).map(bounded_release_text),
        "release_commit": step_data("git.commit").and_then(|data| data.get("commit").or_else(|| data.get("sha"))).or_else(|| step_data("git.tag").and_then(|data| data.get("head"))).and_then(Value::as_str).map(bounded_release_text).or_else(|| rollback_str("release_commit")),
        "tag": result.get("tag").and_then(Value::as_str).map(bounded_release_text),
        "push_target": step_data("git.push").and_then(|data| data.get("target").or_else(|| data.get("remote"))).and_then(Value::as_str).map(bounded_release_text),
        "artifacts": artifacts,
        "publication_urls": publications,
        "gates": run_result.and_then(|value| value.get("summary")).map(|summary| serde_json::json!({
            "total": summary.get("total_steps"), "succeeded": summary.get("succeeded"),
            "failed": summary.get("failed"), "skipped": summary.get("skipped"), "missing": summary.get("missing"),
        })),
        "rollback": rollback.map(|_| serde_json::json!({
            "status": rollback_str("status"),
            "tag_state": rollback_str("tag_state"),
            "release_commit": rollback_str("release_commit"),
            "original_head": rollback_str("original_head"),
            "final_head": rollback_str("final_head"),
        })),
        "warnings": warnings,
        "evidence_refs": evidence_refs,
        "failure": failure,
        // An incomplete release already knows how to finish itself. Dropping
        // this is what reported a recovered-but-unpublished release as an
        // unexplained failure while holding the exact command (#14577).
        "continuation_command": result.get("continuation_command").and_then(Value::as_str).map(bounded_release_text),
        "full_command": format!("homeboy release {} --full", bounded_release_text(result.get("component_id").and_then(Value::as_str).unwrap_or("<component>"))),
        "output": output_file.map(bounded_release_text),
    });

    bounded_release_envelope(projection, exit_code)
}

fn bounded_release_envelope(projection: Value, exit_code: i32) -> Value {
    if release_envelope_bytes(&projection, exit_code)
        .is_ok_and(|bytes| bytes <= MAX_RELEASE_STDOUT_BYTES)
    {
        return projection;
    }

    let fallback = serde_json::json!({
        "schema": "homeboy/release-operator-summary/v1",
        "command": "release",
        "exit_code": exit_code,
        "full_command": "homeboy release <component> --full",
    });
    debug_assert!(release_envelope_bytes(&fallback, exit_code)
        .is_ok_and(|bytes| bytes <= MAX_RELEASE_STDOUT_BYTES));
    fallback
}

fn release_envelope_bytes(payload: &Value, exit_code: i32) -> serde_json::Result<usize> {
    let response = crate::commands::utils::response::cli_response_for_json_result_for_command(
        &Ok(payload.clone()),
        exit_code,
        "release",
        None,
    );
    serde_json::to_vec(&response).map(|rendered| rendered.len())
}

fn release_artifact_name(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

fn release_publication_url(data: &Value) -> Option<&str> {
    let response = data.get("response").unwrap_or(data);
    let verification = response
        .get("registry_verification")
        .or_else(|| response.get("registryVerification"))
        .unwrap_or(response);
    ["version_url", "versionUrl", "url"]
        .into_iter()
        .find_map(|key| verification.get(key).and_then(Value::as_str))
}

/// Bound long release text while preserving BOTH ends of it.
///
/// Release failure text is two useful things joined together: the head names
/// what failed (`Release step package (package) failed: ...`) and the tail
/// carries what the underlying tool actually said. Keeping only the head meant
/// a packaging failure was reported as
///
/// ```text
/// Package command failed (exit 255): Building release ZIP for <component>...
/// [BUILD] Checking build dependencies...
/// [SUCCESS] All build dependencies found
/// ```
///
/// — several hundred bytes of preamble, with the error that explained it
/// discarded. The operator could not act on that, so every packaging failure
/// cost a trip into raw CI logs to recover output Homeboy had already captured
/// (issue #14859).
///
/// Truncating from the tail instead would be just as wrong in the other
/// direction: `Refusing to release from branch 'x' because the repo default
/// branch is 'main'` is answered entirely by its head. Both ends carry signal,
/// so the middle is what gets dropped, and the elision states how much went.
///
/// The marker is budgeted inside `MAX_RELEASE_TEXT_BYTES` rather than added on
/// top, so this stays within the bound callers already rely on.
fn bounded_release_text(value: &str) -> String {
    if value.len() <= MAX_RELEASE_TEXT_BYTES {
        return value.to_string();
    }

    // Reserved so the marker never pushes the result past the existing bound.
    // `... [<digits>] bytes omitted ...` plus newlines fits comfortably.
    const ELISION_RESERVE: usize = 40;

    let budget = MAX_RELEASE_TEXT_BYTES.saturating_sub(ELISION_RESERVE);
    // The tail gets the larger share: build tools put the actionable error last.
    let head_budget = budget / 3;
    let tail_budget = budget - head_budget;

    let mut head_end = head_budget.min(value.len());
    while head_end > 0 && !value.is_char_boundary(head_end) {
        head_end -= 1;
    }

    let mut tail_start = value.len().saturating_sub(tail_budget);
    while tail_start < value.len() && !value.is_char_boundary(tail_start) {
        tail_start += 1;
    }

    if tail_start <= head_end {
        return value[..head_end].to_string();
    }

    format!(
        "{}\n... [{} bytes omitted] ...\n{}",
        &value[..head_end],
        tail_start - head_end,
        &value[tail_start..]
    )
}

fn release_reproduction_commands(step: &Value, result: &Value) -> Vec<String> {
    let component = result
        .get("component_id")
        .and_then(Value::as_str)
        .map(bounded_release_text)
        .unwrap_or_else(|| "<component>".to_string());
    let gate = step
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| step.get("type").and_then(Value::as_str))
        .and_then(|name| name.strip_prefix("preflight."));
    let command = match gate {
        Some("lint") | Some("test") => {
            format!("homeboy review {} {component}", gate.unwrap())
        }
        Some("package") | Some("build") => format!("homeboy review build {component}"),
        _ => format!("homeboy release {component} --dry-run"),
    };
    vec![command]
        .into_iter()
        .take(MAX_RELEASE_ACTIONS)
        .collect()
}

fn refresh_homeboy_uses_bounded_output(args: &runner::RunnerArgs) -> bool {
    runner::refresh_homeboy_uses_bounded_output(args)
}

/// `refresh-homeboy` can generate a multi-megabyte materialization script or
/// build transcript. Keep stdout safe for terminal and agent consumers while
/// preserving the exact command envelope for `--output` and `--full`.
fn refresh_homeboy_command_run(args: runner::RunnerArgs, output_file: Option<&str>) -> CommandRun {
    let (output_file_result, exit_code) =
        crate::commands::utils::response::map_cmd_result_to_json(runner::run(args));
    let stdout_result = Ok(match &output_file_result {
        Ok(payload) => bounded_refresh_projection(payload, exit_code, output_file),
        Err(error) => bounded_refresh_error_projection(error, exit_code, output_file),
    });
    CommandRun::from_command_stdout_result("runner", stdout_result, exit_code)
        .with_output_file_result(output_file_result)
}

fn bounded_refresh_projection(payload: &Value, exit_code: i32, output_file: Option<&str>) -> Value {
    let artifacts = payload.get("artifacts").cloned().unwrap_or_else(|| serde_json::json!({
        "output": output_file,
        "command": "rerun with --full for the complete result; --output writes the lossless envelope",
    }));
    let projection = serde_json::json!({
        "schema": "homeboy/runner-refresh-homeboy-bounded-output/v1",
        "command": "runner.refresh_homeboy",
        "exit_code": exit_code,
        "runner_id": payload.get("runner_id").and_then(Value::as_str).map(bounded_refresh_text),
        "dry_run": payload.get("dry_run").and_then(Value::as_bool),
        "selected_binary_path": payload.get("selected_binary_path").and_then(Value::as_str).map(bounded_refresh_text),
        "daemon_refreshed": payload.get("daemon_refreshed").and_then(Value::as_bool),
        "reconnect_required": payload.get("reconnect_required").and_then(Value::as_bool),
        "readiness": refresh_readiness_summary(payload.get("readiness")),
        "phases": payload.get("phase_summary").and_then(Value::as_array).map(|phases| phases.iter().take(MAX_REFRESH_PHASES).map(|phase| serde_json::json!({
            "name": phase.get("name").and_then(Value::as_str).map(bounded_refresh_text),
            "status": phase.get("status").and_then(Value::as_str).map(bounded_refresh_text),
            "exit_code": phase.get("exit_code").and_then(Value::as_i64),
        })).collect::<Vec<_>>()).unwrap_or_default(),
        "failure": refresh_failure_summary(payload.get("failure")),
        "artifacts": artifacts,
        "full_command": "homeboy runner refresh-homeboy <runner> --full",
    });
    bounded_refresh_envelope(projection, exit_code)
}

fn bounded_refresh_error_projection(
    error: &homeboy::core::Error,
    exit_code: i32,
    output_file: Option<&str>,
) -> Value {
    bounded_refresh_envelope(
        serde_json::json!({
            "schema": "homeboy/runner-refresh-homeboy-bounded-output/v1",
            "command": "runner.refresh_homeboy",
            "exit_code": exit_code,
            "error": {
                "code": error.code.as_str(),
                "message": bounded_refresh_text(&error.attributed_message()),
                "details": {
                    "_homeboy_actions": bounded_refresh_error_actions(error),
                },
            },
            "artifacts": error.details.get("artifacts").cloned().unwrap_or_else(|| serde_json::json!({
                "output": output_file,
                "command": "rerun with --full for the complete error; --output writes the lossless envelope",
            })),
            "full_command": "homeboy runner refresh-homeboy <runner> --full",
        }),
        exit_code,
    )
}

/// Keep executable recovery actions in the bounded envelope without admitting
/// arbitrary error detail or action evidence beyond the stdout budget.
fn bounded_refresh_error_actions(error: &homeboy::core::Error) -> Vec<Value> {
    error
        .details
        .get(homeboy::core::error::ACTIONS_DETAILS_KEY)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(4)
        .filter_map(|action| {
            let action = action.as_object()?;
            Some(serde_json::json!({
                "id": action.get("id")?.as_str().map(bounded_refresh_text),
                "label": action.get("label")?.as_str().map(bounded_refresh_text),
                "program": action.get("program")?.as_str().map(bounded_refresh_text),
                "args": action.get("args").and_then(Value::as_array).into_iter().flatten()
                    .filter_map(Value::as_str).take(16).map(bounded_refresh_text).collect::<Vec<_>>(),
                "safety": action.get("safety")?.as_str().map(bounded_refresh_text),
                "required_confirmations": action.get("required_confirmations").and_then(Value::as_array).into_iter().flatten()
                    .filter_map(Value::as_str).take(4).map(bounded_refresh_text).collect::<Vec<_>>(),
            }))
        })
        .collect()
}

/// Enforce the budget after command-envelope rendering, because lifted action
/// metadata, diagnostics, and pretty JSON all add bytes beyond the projection.
fn bounded_refresh_envelope(projection: Value, exit_code: i32) -> Value {
    if refresh_envelope_bytes(&projection, exit_code)
        .is_ok_and(|bytes| bytes <= MAX_REFRESH_PROJECTION_BYTES)
    {
        return projection;
    }
    let fallback = serde_json::json!({
        "schema": "homeboy/runner-refresh-homeboy-bounded-output/v1",
        "command": "runner.refresh_homeboy",
        "exit_code": exit_code,
        "full_command": "homeboy runner refresh-homeboy <runner> --full",
    });
    debug_assert!(refresh_envelope_bytes(&fallback, exit_code)
        .is_ok_and(|bytes| bytes <= MAX_REFRESH_PROJECTION_BYTES));
    fallback
}

fn refresh_envelope_bytes(payload: &Value, exit_code: i32) -> serde_json::Result<usize> {
    let response = crate::commands::utils::response::cli_response_for_json_result_for_command(
        &Ok(payload.clone()),
        exit_code,
        "runner",
        None,
    );
    serde_json::to_string_pretty(&response).map(|rendered| rendered.len())
}

fn refresh_readiness_summary(readiness: Option<&Value>) -> Value {
    let Some(readiness) = readiness else {
        return Value::Null;
    };
    serde_json::json!({
        "state": readiness.get("state").and_then(Value::as_str).map(bounded_refresh_text),
        "accepting_jobs": readiness.get("accepting_jobs").and_then(Value::as_bool),
        "daemon_fresh": readiness.get("daemon_fresh").and_then(Value::as_bool),
        "continuation": readiness.get("continuation").and_then(Value::as_str).map(bounded_refresh_text),
    })
}

fn refresh_failure_summary(failure: Option<&Value>) -> Value {
    let Some(failure) = failure else {
        return Value::Null;
    };
    serde_json::json!({
        "exit_code": failure.get("exit_code").and_then(Value::as_i64),
        "message": failure.get("verification").and_then(Value::as_str).filter(|text| !text.trim().is_empty()).map(bounded_refresh_text).or_else(|| failure.get("stderr").and_then(Value::as_str).filter(|text| !text.trim().is_empty()).map(|text| bounded_refresh_text(&text.lines().rev().take(10).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")))),
        "verification": failure.get("verification").and_then(Value::as_str).map(bounded_refresh_text),
        "verification_kind": failure.get("verification_kind").and_then(Value::as_str).map(bounded_refresh_text),
        "job_id": failure.get("job_id").and_then(Value::as_str).map(bounded_refresh_text),
        "mirror_run_id": failure.get("mirror_run_id").and_then(Value::as_str).map(bounded_refresh_text),
    })
}

fn bounded_refresh_text(value: &str) -> String {
    if value.len() <= MAX_REFRESH_TEXT_BYTES {
        return value.to_string();
    }
    let mut end = MAX_REFRESH_TEXT_BYTES - 3;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &value[..end])
}

fn command_run_with_summary(
    (stdout_result, exit_code): JsonRun,
    render: impl FnOnce(&Value, i32) -> Option<String>,
) -> CommandRun {
    let summary_stdout = stdout_result
        .as_ref()
        .ok()
        .and_then(|payload| render(payload, exit_code));

    CommandRun::from_stdout_result(stdout_result, exit_code).with_presentation(
        CommandPresentation {
            stdout: summary_stdout,
            stderr: None,
        },
    )
}

/// `--output` is the durable, lossless response artifact. Stdout is an operator
/// projection only, with the same command payload schema and bounded evidence.
fn agent_task_command_run(
    output_file_result: homeboy::core::Result<Value>,
    exit_code: i32,
    summary_kind: Option<super::agent_task_summary::AgentTaskSummaryKind>,
    full: bool,
    bounded_operation: Option<&'static str>,
    redact_cancel: bool,
) -> CommandRun {
    // Lifecycle records can retain provider launch metadata. Redact once before
    // either the compact projection, `--full`, or `--output` can serialize it.
    let output_file_result = if redact_cancel {
        output_file_result
            .map(|value| homeboy::core::redaction::RedactionPolicy::default().redact_json(&value))
    } else {
        output_file_result
    };
    let stdout_result = output_file_result.clone().map(|mut value| {
        if let Some(operation) = bounded_operation {
            value = if operation == "cancel" {
                crate::commands::agent_task::status::bounded_cancel_report(value)
            } else {
                crate::commands::agent_task::status::bounded_full_operation_report(value, operation)
            };
        } else if !full {
            crate::commands::agent_task::status::project_operator_output(&mut value);
        }
        value
    });
    let summary_stdout = stdout_result
        .as_ref()
        .ok()
        .and_then(|payload| summary_kind.and_then(|kind| render_agent_task_summary(kind, payload)));
    CommandRun::from_command_stdout_result("agent-task", stdout_result, exit_code)
        .with_output_file_result(output_file_result)
        .with_presentation(CommandPresentation {
            stdout: summary_stdout,
            stderr: None,
        })
}

/// Only terminal-facing reports need the bounded operation projection. Handler
/// results remain lossless for internal callers and `--output` artifacts.
fn agent_task_bounded_operation(
    args: &crate::commands::agent_task::AgentTaskArgs,
) -> Option<&'static str> {
    use crate::commands::agent_task::AgentTaskCommand;

    match &args.command {
        AgentTaskCommand::FinalizePr(_) => Some("finalize-pr"),
        AgentTaskCommand::Cook(args) if args.full => Some("cook"),
        AgentTaskCommand::Recover(args) if args.full => Some("recover"),
        AgentTaskCommand::Cancel(args) if !args.full => Some("cancel"),
        _ => None,
    }
}

fn agent_task_requests_full_output(args: &crate::commands::agent_task::AgentTaskArgs) -> bool {
    use crate::commands::agent_task::AgentTaskCommand;

    match &args.command {
        AgentTaskCommand::Cook(args) => args.full,
        AgentTaskCommand::Recover(args) => args.full,
        AgentTaskCommand::Artifacts(args) => args.full,
        AgentTaskCommand::Resume(args) => args.full,
        AgentTaskCommand::Evidence(args) => args.full,
        AgentTaskCommand::Diagnose(args) => args.full,
        AgentTaskCommand::Review(args) => args.full,
        AgentTaskCommand::Promote(args) => args.full,
        AgentTaskCommand::Adopt(args) => args.full,
        AgentTaskCommand::FinalizePr(args) => args.full,
        AgentTaskCommand::Cancel(args) => args.full,
        _ => false,
    }
}

fn agent_task_summary_kind_for_output(
    args: &crate::commands::agent_task::AgentTaskArgs,
) -> Option<super::agent_task_summary::AgentTaskSummaryKind> {
    agent_task_summary_kind_for_output_mode(
        args,
        homeboy::core::lab_routing::is_lab_offload_subprocess(),
    )
}

fn agent_task_summary_kind_for_output_mode(
    args: &crate::commands::agent_task::AgentTaskArgs,
    lab_offload_subprocess: bool,
) -> Option<super::agent_task_summary::AgentTaskSummaryKind> {
    if lab_offload_subprocess {
        None
    } else {
        agent_task_summary_kind(args)
    }
}

fn dispatch(
    command: Commands,
    _spec: &CommandSpec,
    placement: Placement,
) -> (homeboy::core::Result<Value>, i32) {
    if let Commands::Cleanup(args) = command {
        return map(crate::commands::cleanup::run(args, placement));
    }
    let command = match adapter::command_adapter(
        command,
        crate::command_contract::CommandOutputFileMode::None,
    ) {
        Ok(adapter) => return adapter.run(),
        Err(command) => *command,
    };

    if let Commands::AgentTask(args) = command {
        return map(crate::commands::agent_task::run_with_placement(
            args, placement,
        ));
    }

    dispatch_registered(command)
}

pub(crate) fn cleanup_run_auto(
    args: crate::commands::cleanup::CleanupArgs,
) -> crate::commands::CmdResult<serde_json::Value> {
    crate::commands::cleanup::run(args, Placement::Auto)
}

fn dispatch_registered(command: Commands) -> JsonRun {
    macro_rules! dispatch_builtin_json_command {
        ($(($variant:ident, $handler:path, $spec:expr),)*) => {
            match command {
                $(Commands::$variant(args) => map($handler(args)),)*
            }
        };
    }

    crate::builtin_json_command_descriptors!(dispatch_builtin_json_command)
}

fn map<T: serde::Serialize>(result: super::CmdResult<T>) -> JsonRun {
    crate::commands::utils::response::map_cmd_result_to_json(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The failure this protects against: a packaging failure whose summary was
    /// all preamble, because the error a build tool emits comes last and the
    /// bound kept only the first 512 bytes. Every such failure then cost a trip
    /// into raw CI logs to recover text Homeboy had already captured (#14859).
    #[test]
    fn bounded_release_text_keeps_the_error_at_the_end() {
        let preamble = "Package command failed (exit 255): Building release ZIP for extrachill-link-pages...\n";
        let chatter = "[BUILD] Checking build dependencies...\n".repeat(40);
        let real_error = "[ERROR] webpack: Module not found: Can't resolve './missing-entry'";
        let value = format!("{preamble}{chatter}{real_error}");

        assert!(
            value.len() > MAX_RELEASE_TEXT_BYTES,
            "fixture must exceed the bound or it proves nothing"
        );

        let bounded = bounded_release_text(&value);

        assert!(
            bounded.contains(real_error),
            "the actionable error must survive truncation; got: {bounded}"
        );
        assert!(
            bounded.contains("Package command failed (exit 255)"),
            "the head still identifies what failed; got: {bounded}"
        );
        assert!(
            bounded.contains("bytes omitted"),
            "the elision must be visible rather than silent; got: {bounded}"
        );
    }

    /// Tail-truncation would be just as wrong in the other direction: a
    /// preflight refusal is answered entirely by its opening words.
    #[test]
    fn bounded_release_text_keeps_the_reason_at_the_start() {
        let head = "Release step preflight.default_branch failed: Refusing to release from branch 'fix/x' because the repo default branch is 'main'";
        let value = format!("{head}{}", "\ntrailing diagnostic noise".repeat(60));

        let bounded = bounded_release_text(&value);

        assert!(
            bounded.contains("Refusing to release from branch"),
            "the leading reason must survive; got: {bounded}"
        );
    }

    #[test]
    fn bounded_release_text_leaves_short_values_untouched() {
        let value = "sha256:abc123";
        assert_eq!(bounded_release_text(value), value);

        let exact = "x".repeat(MAX_RELEASE_TEXT_BYTES);
        assert_eq!(bounded_release_text(&exact), exact);
    }

    /// Callers bound the rendered envelope, so the elision marker is budgeted
    /// inside the limit rather than added on top of it.
    #[test]
    fn bounded_release_text_respects_the_existing_bound() {
        let value = "y".repeat(MAX_RELEASE_TEXT_BYTES * 8);
        let bounded = bounded_release_text(&value);
        assert!(
            bounded.len() <= MAX_RELEASE_TEXT_BYTES,
            "bounded text grew to {} bytes, past the {} byte budget",
            bounded.len(),
            MAX_RELEASE_TEXT_BYTES
        );
    }

    /// Multi-byte input must not panic or split a character.
    #[test]
    fn bounded_release_text_handles_multibyte_boundaries() {
        let value = "é".repeat(MAX_RELEASE_TEXT_BYTES);
        let bounded = bounded_release_text(&value);
        assert!(bounded.len() <= MAX_RELEASE_TEXT_BYTES);
        assert!(!bounded.is_empty());
    }

    #[test]
    fn output_bootstrap_failure_terminalizes_the_persisted_cook_parent() {
        homeboy::core::test_support::with_isolated_home(|_| {
            let cook_id = "cook-output-bootstrap-failure";
            let store = homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()
                .expect("lifecycle store");
            homeboy::agents::agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
                &store, cook_id,
            )
            .expect("persist Cook parent");
            let error = homeboy::core::Error::internal_io(
                "progress write failed",
                Some("output envelope".to_string()),
            );

            terminalize_cook_output_bootstrap_failure(cook_id, &error)
                .expect("terminalize bootstrap parent");

            let record = store.read_record(cook_id).expect("read Cook parent");
            assert_eq!(
                record.state(),
                homeboy::agents::agent_task_lifecycle::AgentTaskRunState::Failed
            );
            assert_eq!(
                record.metadata["pre_execution_failure"]["phase"],
                "output_bootstrap"
            );
        });
    }

    #[test]
    fn runs_show_stdout_is_compact_while_output_file_payload_stays_lossless() {
        let payload = serde_json::json!({
            "variant": "show",
            "payload": {
                "command": "runs.show",
                "run": {
                    "id": "run-output-detail",
                    "kind": "runner-exec",
                    "status": "failed",
                    "metadata": {
                        "runner_terminal_projection": { "state": "terminal_checkpointed", "status": "failed" },
                        "source_snapshot": { "marker": "full-source-snapshot", "body": "x".repeat(256 * 1024) },
                    },
                    "artifacts": [],
                },
            },
        });

        let run = runs_show_command_run((Ok(payload.clone()), 1));
        let stdout = run.stdout_result.expect("compact stdout");
        let output = run
            .output_file_result
            .expect("lossless output result")
            .expect("lossless output");

        assert!(stdout
            .pointer("/payload/run/metadata/source_snapshot")
            .is_none());
        assert_eq!(
            stdout.pointer("/payload/run/metadata/operator_projection/authoritative_runner_terminal_state/status"),
            Some(&Value::String("failed".to_string()))
        );
        assert_eq!(
            output.pointer("/payload/run/metadata/source_snapshot/marker"),
            Some(&Value::String("full-source-snapshot".to_string()))
        );
        assert_eq!(output, payload);
    }

    /// A rolled-back release must still report the commit it created.
    ///
    /// Regression for #13708: `release_commit` was sourced only from the
    /// `git.commit`/`git.tag` step data. When the pipeline rolled back, that
    /// data was gone and the summary reported `null` while the same payload
    /// recorded the real commit under `rollback`. Operators acting on that
    /// summary can re-release or re-tag a version that already published.
    #[test]
    fn rolled_back_release_summary_reports_the_commit_it_created() {
        let payload = serde_json::json!({
            "result": {
                "component_id": "data-machine-code",
                "status": "partial",
                "phase": "publish",
                "run": { "result": {
                    "status": "partial_success",
                    "steps": [{ "id": "preflight.test", "type": "preflight.test",
                                "status": "failed", "error": "Tests failed (exit code 1)" }],
                    "rollback": {
                        "status": "restored",
                        "tag_state": "not_created",
                        "release_commit": "1e37e68ddbdb149b12c93389dd95d14da4c8082e",
                        "original_head": "d1c8cd8e4d0ed0064c38c56c182a1fa70906533e",
                        "final_head": "d1c8cd8e4d0ed0064c38c56c182a1fa70906533e",
                    },
                }},
            }
        });

        let projection = bounded_release_projection(&payload, 1, None);

        assert_eq!(
            projection.get("release_commit").and_then(Value::as_str),
            Some("1e37e68ddbdb149b12c93389dd95d14da4c8082e"),
            "a rolled-back release must surface the commit it created, not null"
        );
        let rollback = projection
            .get("rollback")
            .expect("rollback evidence must reach the operator summary");
        assert_eq!(
            rollback.get("status").and_then(Value::as_str),
            Some("restored")
        );
        assert_eq!(
            rollback.get("tag_state").and_then(Value::as_str),
            Some("not_created"),
            "operators must be able to tell what happened to the tag"
        );
    }
    #[test]
    fn manifest_dispatches_as_json_workspace_output() {
        let (result, exit_code) = dispatch(
            Commands::Contract(crate::commands::contract::ContractArgs {
                command: crate::commands::contract::ContractCommand::Manifest(
                    crate::commands::manifest::ManifestArgs {},
                ),
            }),
            crate::command_contract::registered_command("contract").unwrap(),
            Placement::Auto,
        );

        assert_eq!(exit_code, 0);
        let value = result.expect("manifest should dispatch as JSON");
        assert_eq!(value["command"], "contract.manifest");
        assert!(value["commands"].is_array());
    }

    #[test]
    fn lab_offload_agent_task_subprocess_keeps_json_stdout() {
        use crate::commands::agent_task::{AgentTaskArgs, AgentTaskCommand, StatusArgs};

        let args = AgentTaskArgs {
            command: AgentTaskCommand::Status(StatusArgs {
                run_id: "run-1".to_string(),
                interval: "5s".to_string(),
                timeout: "30m".to_string(),
                ..Default::default()
            }),
        };

        assert!(agent_task_summary_kind_for_output_mode(&args, false).is_some());
        assert!(agent_task_summary_kind_for_output_mode(&args, true).is_none());
    }

    #[test]
    fn summary_presentation_preserves_structured_result_exit_code_and_file_payload() {
        let payload = serde_json::json!({ "schema": "test/v1", "items": [1, 2, 3] });
        let run = command_run_with_summary((Ok(payload.clone()), 7), |value, exit_code| {
            assert_eq!(value, &payload);
            assert_eq!(exit_code, 7);
            Some("3 items\n".to_string())
        });

        assert_eq!(
            run.stdout_result.as_ref().expect("structured payload"),
            &payload
        );
        assert_eq!(run.exit_code, 7);
        assert_eq!(run.presentation.stdout.as_deref(), Some("3 items\n"));
        assert_eq!(
            run.output_file_result(
                crate::command_contract::CommandOutputFileMode::GenericEnvelope,
            )
            .as_ref()
            .expect("output-file payload"),
            &payload
        );
    }

    #[test]
    fn agent_task_stdout_is_bounded_while_output_file_result_is_lossless() {
        let payload = serde_json::json!({ "stdout": "x".repeat(512 * 1024) });
        let run = agent_task_command_run(Ok(payload.clone()), 0, None, false, None, false);

        assert!(
            run.stdout_result.as_ref().expect("stdout")["stdout"]
                .as_str()
                .expect("string schema preserved")
                .len()
                < 256
        );
        assert_eq!(
            run.output_file_result(crate::command_contract::CommandOutputFileMode::GenericEnvelope)
                .as_ref()
                .expect("lossless output file"),
            &payload
        );
    }

    #[test]
    fn cancel_stdout_is_compact_redacted_and_output_file_retains_full_evidence() {
        let payload = serde_json::json!({
            "schema": "homeboy/control-plane-action-acknowledgement/v1",
            "acknowledgement": "cancel-run:action:cancel:key",
            "run": "cancel-run",
            "outcome": "succeeded",
            "resource": { "run": "cancel-run", "state": "cancelled" },
            "result": { "schema": "homeboy/control-plane-cancel-result/v1", "data": {
                "schema": "homeboy/control-plane-cancel-result/v1",
                "disposition": "cancelled", "terminal": true, "wait_timeout_seconds": 15,
                "waited_seconds": 0, "poll_count": 0
            }},
            "metadata": {
                "provider_launch_environment": { "OPENAI_API_KEY": "actual-secret" },
                "replay_argv": (0..500).map(|_| "x".repeat(256)).collect::<Vec<_>>(),
                "execution_records": (0..500).map(|_| serde_json::json!({ "output": "x".repeat(256) })).collect::<Vec<_>>(),
            }
        });
        let run = agent_task_command_run(Ok(payload.clone()), 0, None, false, Some("cancel"), true);
        let stdout = run.stdout_result.as_ref().expect("bounded stdout");
        let rendered = serde_json::to_vec(stdout).expect("serialize bounded stdout");

        assert!(
            rendered.len() <= 4 * 1024,
            "projection was {} bytes",
            rendered.len()
        );
        assert_eq!(stdout["run"]["state"], "cancelled");
        assert_eq!(stdout["cancellation"]["disposition"], "cancelled");
        assert_eq!(
            stdout["next_action"]["command"],
            "homeboy agent-task status cancel-run"
        );
        assert!(!rendered
            .windows(b"provider_launch_environment".len())
            .any(|window| window == b"provider_launch_environment"));
        let output = run
            .output_file_result(crate::command_contract::CommandOutputFileMode::GenericEnvelope)
            .as_ref()
            .expect("lossless output file");
        assert_eq!(
            output["metadata"]["replay_argv"],
            payload["metadata"]["replay_argv"]
        );
        assert_eq!(
            output["metadata"]["provider_launch_environment"]["OPENAI_API_KEY"],
            "[REDACTED]"
        );

        let full = agent_task_command_run(Ok(payload.clone()), 0, None, true, None, true);
        let full = full.stdout_result.as_ref().expect("full stdout");
        assert_eq!(
            full["metadata"]["replay_argv"],
            payload["metadata"]["replay_argv"]
        );
        assert_eq!(
            full["metadata"]["provider_launch_environment"]["OPENAI_API_KEY"],
            "[REDACTED]"
        );
    }

    #[test]
    fn refresh_homeboy_failed_step_stderr_reaches_summary_and_diagnostics() {
        let payload = serde_json::json!({
            "runner_id": "lab",
            "failure": {"exit_code": 1, "stderr": "immutable Homeboy slot provenance mismatch"},
            "phase_summary": [{"name": "materialize", "status": "failed"}]
        });
        let projection = bounded_refresh_projection(&payload, 1, None);
        let response = crate::commands::utils::response::cli_response_for_json_result_for_command(
            &Ok(projection),
            1,
            "runner",
            None,
        );
        let value = serde_json::to_value(response).unwrap();
        for field in [&value["summary"], &value["diagnostics"]["message"]] {
            assert!(
                field
                    .as_str()
                    .unwrap()
                    .contains("immutable Homeboy slot provenance mismatch"),
                "{value}"
            );
        }
    }

    #[test]
    fn refresh_homeboy_output_file_is_lossless_while_stdout_is_hard_bounded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("refresh.json");
        let payload = serde_json::json!({
            "runner_id": "lab",
            "dry_run": true,
            "selected_binary_path": "/runner/homeboy",
            "daemon_refreshed": false,
            "reconnect_required": true,
            "phase_summary": (0..100).map(|_| serde_json::json!({ "name": "materialize", "status": "failed", "exit_code": 1 })).collect::<Vec<_>>(),
            "failure": { "exit_code": 1, "verification_kind": "history_diverged", "verification": "GitHub API unavailable", "stdout": "x".repeat(512 * 1024), "stderr": "y".repeat(512 * 1024) },
            "plan": { "script": "z".repeat(512 * 1024) },
            "artifacts": {
                "run_id": "run-1",
                "materialization_script": "homeboy://run/run-1/artifact/materialization-script-run-1",
                "build_log": "homeboy://run/run-1/artifact/build-log-run-1"
            }
        });
        let bounded = bounded_refresh_projection(&payload, 1, Some("refresh.json"));
        let rendered = serde_json::to_vec(&bounded).expect("bounded projection serializes");
        assert!(
            refresh_envelope_bytes(&bounded, 1).expect("envelope serializes")
                <= MAX_REFRESH_PROJECTION_BYTES
        );
        assert!(!String::from_utf8_lossy(&rendered).contains(&"x".repeat(512)));
        assert_eq!(bounded["artifacts"]["run_id"], "run-1");
        assert_eq!(bounded["failure"]["verification_kind"], "history_diverged");
        assert_eq!(bounded["failure"]["verification"], "GitHub API unavailable");

        let run = CommandRun::from_command_stdout_result("runner", Ok(bounded), 1)
            .with_output_file_result(Ok(payload.clone()));
        super::super::output_runtime::write_output_file(
            &run,
            crate::command_contract::CommandOutputFileMode::GenericEnvelope,
            Some(path.to_str().expect("utf8 path")),
        );
        let written: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(written["data"], payload);
    }

    #[test]
    fn refresh_homeboy_success_projection_keeps_the_final_envelope_bounded() {
        let payload = serde_json::json!({
            "runner_id": "lab",
            "phase_summary": (0..100).map(|_| serde_json::json!({ "name": "materialize", "status": "succeeded", "exit_code": 0 })).collect::<Vec<_>>(),
            "artifacts": { "run_id": "run-1", "materialization_script": "homeboy://run/run-1/artifact/materialization-script" },
            "plan": { "script": "z".repeat(512 * 1024) },
        });

        let projection = bounded_refresh_projection(&payload, 0, None);
        assert!(
            refresh_envelope_bytes(&projection, 0).expect("envelope serializes")
                <= MAX_REFRESH_PROJECTION_BYTES
        );
        assert_eq!(projection["artifacts"]["run_id"], "run-1");
    }

    #[test]
    fn refresh_homeboy_error_projection_is_bounded_and_keeps_durable_artifacts() {
        let mut error = homeboy::core::Error::validation_invalid_argument(
            "target_dir",
            "x".repeat(512 * 1024),
            None,
            None,
        );
        error.details["artifacts"] = serde_json::json!({
            "run_id": "run-1",
            "error_log": "homeboy://run/run-1/artifact/error-log-run-1",
        });
        error.details["_homeboy_actions"] = serde_json::json!([{
            "id": "inspect-runner-status",
            "label": "inspect runner admission",
            "program": "homeboy",
            "args": ["runner", "status", "homeboy-lab"],
            "safety": "read_only"
        }]);

        let projection = bounded_refresh_error_projection(&error, 2, None);
        assert!(
            refresh_envelope_bytes(&projection, 2).expect("envelope serializes")
                <= MAX_REFRESH_PROJECTION_BYTES
        );
        assert_eq!(projection["artifacts"]["run_id"], "run-1");
        assert_eq!(
            projection["error"]["details"]["_homeboy_actions"][0]["program"],
            "homeboy"
        );
        assert!(!serde_json::to_string(&projection)
            .unwrap()
            .contains(&"x".repeat(512)));
    }

    #[test]
    fn refresh_homeboy_error_projection_names_the_io_cause() {
        let error = homeboy::core::Error::internal_io(
            "runner build identity mismatch",
            Some("workspace materialization protocol".to_string()),
        );

        let projection = bounded_refresh_error_projection(&error, 1, None);
        assert_eq!(
            projection["error"]["message"],
            "IO error: workspace materialization protocol: runner build identity mismatch"
        );
    }

    #[test]
    fn refresh_homeboy_projection_drops_oversized_artifacts_to_fit_the_final_envelope() {
        let payload = serde_json::json!({
            "artifacts": { "run_id": "run-1", "build_log": "x".repeat(512 * 1024) },
        });

        let projection = bounded_refresh_projection(&payload, 1, None);
        assert!(
            refresh_envelope_bytes(&projection, 1).expect("envelope serializes")
                <= MAX_REFRESH_PROJECTION_BYTES
        );
        assert!(projection.get("artifacts").is_none());
    }

    #[test]
    fn absent_summary_and_error_paths_remain_unmodified() {
        let payload = serde_json::json!({ "schema": "test/v1" });
        let run = command_run_with_summary((Ok(payload.clone()), 0), |_, _| None);
        assert_eq!(run.stdout_result.expect("structured payload"), payload);
        assert_eq!(run.presentation, CommandPresentation::default());

        let error =
            homeboy::core::Error::validation_invalid_argument("test", "invalid", None, None);
        let expected_code = error.code;
        let run = command_run_with_summary((Err(error.clone()), 2), |_, _| {
            panic!("renderer must not run for errors")
        });
        assert_eq!(
            run.stdout_result.expect_err("error result").code,
            expected_code
        );
        assert_eq!(run.exit_code, 2);
        assert_eq!(run.presentation, CommandPresentation::default());
    }

    #[test]
    fn release_stdout_is_bounded_and_output_file_remains_lossless() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("release.json");
        let large = "x".repeat(512 * 1024);
        let payload = serde_json::json!({
            "command": "release",
            "variant": "single",
            "result": {
                "component_id": "fixture",
                "status": "released",
                "phase": "publish",
                "new_version": "1.2.3",
                "tag": "v1.2.3",
                "plan": { "changelog": large },
                "run": { "result": {
                    "warnings": (0..100).map(|_| large.clone()).collect::<Vec<_>>(),
                    "summary": { "total_steps": 23, "succeeded": 23, "failed": 0, "skipped": 0, "missing": 0 },
                    "steps": [
                        { "id": "version", "type": "version", "status": "success", "data": { "old_version": "1.2.2", "new_version": "1.2.3", "notes": large } },
                        { "id": "git.commit", "type": "git.commit", "status": "success", "data": { "sha": "abc123" } },
                        { "id": "git.tag", "type": "git.tag", "status": "success", "data": { "head": "abc123" } },
                        { "id": "git.push", "type": "git.push", "status": "success", "data": { "target": "origin/main" } },
                        { "id": "package", "type": "package", "status": "success", "data": { "action": "release.package", "response": { "log": large } } },
                        { "id": "artifacts.authority", "type": "artifacts.authority", "status": "success", "data": { "artifacts": (0..100).map(|i| serde_json::json!({ "path": format!("dist/fixture-{i}.tgz"), "sha256": "a".repeat(64), "log": large })).collect::<Vec<_>>() } },
                        { "id": "github.release", "type": "github.release", "status": "success", "data": { "url": "https://github.com/example/fixture/releases/tag/v1.2.3", "notes": large } },
                        { "id": "publish.npm", "type": "publish.npm", "status": "success", "data": { "response": { "registry_verification": { "version_url": "https://registry.example/fixture/1.2.3" } } } }
                    ]
                } }
            }
        });
        let run = release_command_run((Ok(payload.clone()), 0), Some("release.json"), false);
        let stdout = serde_json::to_vec(run.stdout_result.as_ref().expect("stdout"))
            .expect("stdout serializes");
        let envelope = crate::commands::utils::response::cli_response_for_json_result_for_command(
            &run.stdout_result,
            0,
            "release",
            None,
        );
        let rendered_envelope = serde_json::to_vec(&envelope).expect("envelope serializes");

        assert!(
            stdout.len() <= MAX_RELEASE_STDOUT_BYTES,
            "stdout was {} bytes",
            stdout.len()
        );
        assert!(
            rendered_envelope.len() <= MAX_RELEASE_STDOUT_BYTES,
            "stdout envelope was {} bytes",
            rendered_envelope.len()
        );
        assert!(!String::from_utf8_lossy(&stdout).contains(&large));
        assert_eq!(run.stdout_result.as_ref().unwrap()["component"], "fixture");
        assert_eq!(
            run.stdout_result.as_ref().unwrap()["release_commit"],
            "abc123"
        );
        assert_eq!(
            run.stdout_result.as_ref().unwrap()["push_target"],
            "origin/main"
        );
        assert_eq!(
            run.stdout_result.as_ref().unwrap()["artifacts"]
                .as_array()
                .unwrap()
                .len(),
            MAX_RELEASE_ARTIFACTS
        );
        assert_eq!(
            run.stdout_result.as_ref().unwrap()["publication_urls"][0]["url"],
            "https://github.com/example/fixture/releases/tag/v1.2.3"
        );
        assert_eq!(
            run.stdout_result.as_ref().unwrap()["publication_urls"][1]["url"],
            "https://registry.example/fixture/1.2.3"
        );

        super::super::output_runtime::write_output_file(
            &run,
            crate::command_contract::CommandOutputFileMode::GenericEnvelope,
            Some(path.to_str().expect("utf8 path")),
        );
        let written: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(written["data"], payload);
    }

    #[test]
    fn release_failure_stdout_keeps_bounded_cause_and_reproduction_command() {
        let payload = serde_json::json!({
            "command": "release",
            "variant": "single",
            "result": {
                "component_id": "fixture",
                "status": "failed",
                "run": { "result": { "steps": [{
                    "id": "preflight.lint", "type": "preflight.lint", "status": "failed",
                    "error": "lint output: ".to_string() + &"x".repeat(512 * 1024),
                    "data": { "transcript": "x".repeat(512 * 1024) }
                }] } }
            }
        });
        let run = release_command_run((Ok(payload), 20), None, false);
        let envelope = crate::commands::utils::response::cli_response_for_json_result_for_command(
            &run.stdout_result,
            20,
            "release",
            None,
        );
        let rendered = serde_json::to_vec(&envelope).expect("envelope serializes");

        assert!(
            rendered.len() <= MAX_RELEASE_STDOUT_BYTES,
            "stdout was {} bytes",
            rendered.len()
        );
        assert_eq!(
            envelope
                .diagnostics
                .as_ref()
                .unwrap()
                .failure_digest
                .as_ref()
                .unwrap()
                .next_actions[0]
                .command,
            "homeboy review lint fixture"
        );
        assert!(!String::from_utf8_lossy(&rendered).contains(&"x".repeat(512)));
    }

    #[test]
    fn release_batch_and_utility_output_remain_lossless_on_stdout() {
        let batch = serde_json::json!({
            "command": "release.batch", "variant": "batch",
            "result": { "results": [{ "component_id": "a", "status": "released" }],
                "summary": { "total": 1, "released": 1, "failed": 0, "skipped": 0 } }
        });
        let utility = serde_json::json!({
            "command": "release.changes", "variant": "single",
            "result": { "commits": [{ "sha": "abc123", "subject": "feat: keep output" }] }
        });

        for payload in [batch, utility] {
            let run = release_command_run((Ok(payload.clone()), 0), None, false);
            assert_eq!(run.stdout_result.unwrap(), payload);
        }
    }

    #[test]
    fn release_full_keeps_single_execution_lossless_on_stdout() {
        let payload = serde_json::json!({
            "command": "release", "variant": "single",
            "result": { "component_id": "fixture", "plan": { "changelog": "x".repeat(32 * 1024) } }
        });
        let run = release_command_run((Ok(payload.clone()), 0), None, true);
        assert_eq!(run.stdout_result.unwrap(), payload);
    }

    #[test]
    fn release_failure_with_adversarial_component_id_stays_within_envelope_budget() {
        let payload = serde_json::json!({
            "command": "release", "variant": "single",
            "result": {
                "component_id": "x".repeat(64 * 1024), "status": "failed",
                "run": { "result": { "steps": [{
                    "id": "preflight.lint", "type": "preflight.lint", "status": "failed",
                    "error": "failure"
                }] } }
            }
        });
        let run = release_command_run((Ok(payload), 20), None, false);
        let envelope = crate::commands::utils::response::cli_response_for_json_result_for_command(
            &run.stdout_result,
            20,
            "release",
            None,
        );
        let rendered = serde_json::to_vec(&envelope).expect("envelope serializes");

        assert!(rendered.len() <= MAX_RELEASE_STDOUT_BYTES);
        assert!(
            envelope.data.as_ref().unwrap()["full_command"]
                .as_str()
                .unwrap()
                .len()
                <= "homeboy release ".len() + MAX_RELEASE_TEXT_BYTES + "... --full".len()
        );
    }
}
