use serde_json::Value;

use crate::cli_surface::{CommandArgumentProvenance, Commands, Placement};
use crate::command_contract::CommandSpec;

use super::agent_task_summary::{agent_task_summary_kind, render_agent_task_summary};
use super::output_runtime::{CommandPresentation, CommandRun};
use super::{adapter, runner};

type JsonRun = (homeboy::core::Result<Value>, i32);

const MAX_AUDIT_WARNING_SAMPLES: usize = 3;
const MAX_AUDIT_TIMING_SPANS: usize = 8;
const MAX_AUDIT_SCOPE_REASONS: usize = 8;
const MAX_AUDIT_PROJECTION_TEXT_BYTES: usize = 160;
const MAX_AUDIT_PROJECTION_BYTES: usize = 4 * 1024;
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
    let summarize_changed_since_audit = changed_since_audit_uses_bounded_output(&command);
    let run = match command {
        Commands::AgentTask(mut args) => {
            let run_from_spec_output_ref =
                agent_task_controller_run_from_spec_output_ref_eligible(&args, output_file);
            let summary_kind = agent_task_summary_kind_for_output(&args);
            let bounded_operation = agent_task_bounded_operation(&args);
            if matches!(
                &args.command,
                crate::commands::agent_task::AgentTaskCommand::Cook(_)
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
                    let cook_id = cook_args
                        .dispatch
                        .run_id
                        .clone()
                        .unwrap_or_else(|| format!("agent-task-{}", uuid::Uuid::new_v4()));
                    cook_args.dispatch.run_id = Some(cook_id.clone());
                    let bootstrap = (|| {
                        let store = homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
                        homeboy::agents::agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
                            &store, &cook_id,
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
                        return CommandRun::from_stdout_result(Err(error), 2)
                            .with_command(spec.name);
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
                    )
                    .with_command(spec.name)
                    .with_output_file_already_written();
                }
            }
            let full = agent_task_requests_full_output(&args);
            let result = dispatch(Commands::AgentTask(args), spec, placement);
            if let Some(output_file) = run_from_spec_output_ref {
                command_run_with_summary(result, |payload, exit_code| {
                    render_controller_run_from_spec_output_ref(payload, exit_code, output_file)
                })
            } else {
                agent_task_command_run(result.0, result.1, summary_kind, full, bounded_operation)
            }
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
        Commands::Activity(args) => command_run_with_summary(
            dispatch(Commands::Activity(args), spec, placement),
            |payload, _| super::activity::render_activity_summary(payload),
        ),
        Commands::Bench(args) => {
            let summarize = args.is_run_invocation()
                && !args.wants_full_json()
                && !homeboy::core::lab_routing::is_lab_offload_subprocess();
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
                    | Some(crate::commands::cleanup::CleanupCommand::Worktrees(_))
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
            command_run_with_summary(
                dispatch(Commands::Runs(args), spec, placement),
                |payload, _| {
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
                },
            )
        }
        Commands::Release(args) => {
            let full = args.requests_full_output();
            release_command_run(
                dispatch(Commands::Release(args), spec, placement),
                output_file,
                full,
            )
        }
        command if summarize_changed_since_audit => {
            changed_since_audit_command_run(dispatch(command, spec, placement), output_file)
        }
        command => {
            let (stdout_result, exit_code) = dispatch(command, spec, placement);
            CommandRun::from_stdout_result(stdout_result, exit_code)
        }
    };

    run.with_command(spec.name)
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
        "release_commit": step_data("git.commit").and_then(|data| data.get("commit").or_else(|| data.get("sha"))).or_else(|| step_data("git.tag").and_then(|data| data.get("head"))).and_then(Value::as_str).map(bounded_release_text),
        "tag": result.get("tag").and_then(Value::as_str).map(bounded_release_text),
        "push_target": step_data("git.push").and_then(|data| data.get("target").or_else(|| data.get("remote"))).and_then(Value::as_str).map(bounded_release_text),
        "artifacts": artifacts,
        "publication_urls": publications,
        "gates": run_result.and_then(|value| value.get("summary")).map(|summary| serde_json::json!({
            "total": summary.get("total_steps"), "succeeded": summary.get("succeeded"),
            "failed": summary.get("failed"), "skipped": summary.get("skipped"), "missing": summary.get("missing"),
        })),
        "warnings": warnings,
        "evidence_refs": evidence_refs,
        "failure": failure,
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

fn bounded_release_text(value: &str) -> String {
    let mut end = value.len().min(MAX_RELEASE_TEXT_BYTES);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    if end == value.len() {
        value.to_string()
    } else {
        format!("{}...", &value[..end])
    }
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
        Some("lint") | Some("test") | Some("audit") => {
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
                "message": bounded_refresh_text(&error.message),
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
        "verification": failure.get("verification").and_then(Value::as_str).map(bounded_refresh_text),
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

fn changed_since_audit_uses_bounded_output(command: &Commands) -> bool {
    matches!(
        command,
        Commands::Review(crate::commands::review::ReviewArgs {
            command: Some(crate::commands::review::ReviewCommand::Audit(args)),
            ..
        }) if args.audit.changed.changed_since.is_some()
            && !args.audit.full
            && !homeboy::core::lab_routing::is_lab_offload_subprocess()
    )
}

/// Keep changed-since audit terminal output useful at repository scale. The
/// full command payload remains the `--output` artifact and is available on
/// demand through `--full`; this projection is deliberately derived only from
/// aggregate fields, never individual findings or convention reports.
fn changed_since_audit_command_run(
    (output_file_result, exit_code): JsonRun,
    output_file: Option<&str>,
) -> CommandRun {
    let stdout_result = output_file_result
        .clone()
        .map(|payload| bounded_audit_projection(&payload, exit_code, output_file));
    CommandRun::from_command_stdout_result("review", stdout_result, exit_code)
        .with_output_file_result(output_file_result)
}

fn render_changed_since_audit_projection(
    payload: &Value,
    exit_code: i32,
    output_file: Option<&str>,
) -> Value {
    let timing = payload
        .get("timing")
        .and_then(|timing| timing.get("spans"))
        .and_then(Value::as_array)
        .map(|spans| {
            spans
                .iter()
                .take(MAX_AUDIT_TIMING_SPANS)
                .filter_map(|span| {
                    Some(serde_json::json!({
                        "id": bounded_audit_projection_text(span.get("id")?.as_str()?),
                        "status": bounded_audit_projection_text(span.get("status")?.as_str()?),
                        "duration_ms": span.get("duration_ms").and_then(Value::as_f64),
                    }))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let warnings = payload
        .pointer("/summary/warnings")
        .map(audit_warning_summary)
        .unwrap_or_else(|| serde_json::json!({ "count": 0, "samples": [] }));
    serde_json::json!({
        "schema": "homeboy/audit-bounded-output/v1",
        "command": "audit",
        "verdict": if exit_code == 0 { "pass" } else { "fail" },
        "exit_code": exit_code,
        "component_id": payload.get("component_id").and_then(Value::as_str).map(bounded_audit_projection_text),
        "measurement": audit_measurement_summary(payload.get("measurement")),
        "counts": {
            "findings": payload.get("findings").and_then(Value::as_array).map_or(0, Vec::len),
            "warnings": warnings["count"],
            "files_scanned": payload.pointer("/summary/files_scanned").and_then(Value::as_u64),
        },
        "changed_since": audit_changed_since_summary(payload.get("changed_since")),
        "warnings": warnings,
        "timing": timing,
        "full_report": audit_full_report_ref(payload, output_file),
    })
}

fn audit_full_report_ref(payload: &Value, output_file: Option<&str>) -> Value {
    if let Some(reference) = payload.get("full_report").and_then(Value::as_object) {
        return serde_json::json!({
            "schema": "homeboy/audit-full-report-ref/v1",
            "uri": reference.get("uri").and_then(Value::as_str).map(bounded_audit_projection_text),
            "command": reference.get("command").and_then(Value::as_str).map(bounded_audit_projection_text),
        });
    }

    serde_json::json!({
        "schema": "homeboy/audit-full-report-ref/v1",
        "output": output_file.map(bounded_audit_projection_text),
        "command": "rerun with --full for the complete report; --output writes the lossless artifact",
    })
}

fn audit_warning_summary(warnings: &Value) -> Value {
    match warnings {
        Value::Array(warnings) => serde_json::json!({
            "count": warnings.len(),
            "samples": warnings.iter().filter_map(Value::as_str).take(MAX_AUDIT_WARNING_SAMPLES).map(bounded_audit_projection_text).collect::<Vec<_>>(),
        }),
        Value::Number(count) => serde_json::json!({ "count": count, "samples": [] }),
        _ => serde_json::json!({ "count": 0, "samples": [] }),
    }
}

fn audit_measurement_summary(measurement: Option<&Value>) -> Value {
    let Some(measurement) = measurement else {
        return Value::Null;
    };

    serde_json::json!({
        "profile": measurement.get("profile").and_then(Value::as_str).map(bounded_audit_projection_text),
        "complete": measurement.get("complete").and_then(Value::as_bool),
        "narrowed_by": measurement.get("narrowed_by").and_then(Value::as_array).map(|reasons| reasons.iter().filter_map(Value::as_str).take(MAX_AUDIT_SCOPE_REASONS).map(bounded_audit_projection_text).collect::<Vec<_>>()).unwrap_or_default(),
    })
}

fn audit_changed_since_summary(changed_since: Option<&Value>) -> Value {
    let Some(changed_since) = changed_since else {
        return Value::Null;
    };

    serde_json::json!({
        "introduced_findings": changed_since.get("introduced_findings").and_then(Value::as_u64),
        "contextual_findings": changed_since.get("contextual_findings").and_then(Value::as_u64),
    })
}

fn bounded_audit_projection_text(value: &str) -> String {
    if value.len() <= MAX_AUDIT_PROJECTION_TEXT_BYTES {
        return value.to_string();
    }

    let mut end = MAX_AUDIT_PROJECTION_TEXT_BYTES - 3;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &value[..end])
}

fn bounded_audit_projection(payload: &Value, exit_code: i32, output_file: Option<&str>) -> Value {
    let projection = render_changed_since_audit_projection(payload, exit_code, output_file);
    if serde_json::to_vec(&projection).is_ok_and(|bytes| bytes.len() <= MAX_AUDIT_PROJECTION_BYTES)
    {
        return projection;
    }

    serde_json::json!({
        "schema": "homeboy/audit-bounded-output/v1",
        "command": "audit",
        "verdict": if exit_code == 0 { "pass" } else { "fail" },
        "exit_code": exit_code,
        "full_report": audit_full_report_ref(payload, output_file),
    })
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
) -> CommandRun {
    let stdout_result = output_file_result.clone().map(|mut value| {
        if let Some(operation) = bounded_operation {
            value = crate::commands::agent_task::status::bounded_full_operation_report(
                value, operation,
            );
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
        AgentTaskCommand::CookContinue(args) if args.full => Some("cook-continue"),
        _ => None,
    }
}

fn agent_task_requests_full_output(args: &crate::commands::agent_task::AgentTaskArgs) -> bool {
    use crate::commands::agent_task::AgentTaskCommand;

    match &args.command {
        AgentTaskCommand::Cook(args) => args.full,
        AgentTaskCommand::CookContinue(args) => args.full,
        AgentTaskCommand::Status(args) => args.full,
        AgentTaskCommand::Artifacts(args) | AgentTaskCommand::Resume(args) => args.full,
        AgentTaskCommand::Evidence(args) => args.full,
        AgentTaskCommand::Diagnose(args) => args.full,
        AgentTaskCommand::Review(args) => args.full,
        AgentTaskCommand::Promote(args) => args.full,
        AgentTaskCommand::Adopt(args) => args.full,
        AgentTaskCommand::FinalizePr(args) => args.full,
        _ => false,
    }
}

fn agent_task_controller_run_from_spec_output_ref_eligible<'a>(
    args: &crate::commands::agent_task::AgentTaskArgs,
    output_file: Option<&'a str>,
) -> Option<&'a str> {
    let output_file = output_file?;
    match &args.command {
        crate::commands::agent_task::AgentTaskCommand::Controller(controller)
            if matches!(
                &controller.command,
                crate::commands::agent_task::AgentTaskControllerCommand::RunFromSpec(_)
            ) =>
        {
            Some(output_file)
        }
        _ => None,
    }
}

fn render_controller_run_from_spec_output_ref(
    payload: &Value,
    exit_code: i32,
    output_file: &str,
) -> Option<String> {
    if payload.get("schema").and_then(Value::as_str)?
        != "homeboy/agent-task-loop-controller-run-from-spec-result/v1"
    {
        return None;
    }

    let status = payload.get("status")?;
    let controller = status.get("controller")?;
    let diagnostics_summary = status
        .get("diagnostics")
        .and_then(|diagnostics| diagnostics.get("summary"))
        .cloned()
        .unwrap_or(Value::Null);
    let terminal_outcomes = controller
        .get("terminal_outcomes")
        .and_then(Value::as_array)
        .map(|outcomes| outcomes.len())
        .unwrap_or(0);
    let evidence_refs = controller_evidence_refs(controller);

    serde_json::to_string(&serde_json::json!({
        "success": exit_code == 0,
        "data": {
            "schema": "homeboy/agent-task-loop-controller-run-from-spec-output-ref/v1",
            "result_schema": "homeboy/agent-task-loop-controller-run-from-spec-result/v1",
            "loop_id": payload.get("loop_id").cloned().unwrap_or(Value::Null),
            "stopped_reason": payload.get("stopped_reason").cloned().unwrap_or(Value::Null),
            "max_actions": payload.get("max_actions").cloned().unwrap_or(Value::Null),
            "output_file": output_file,
            "result_ref": {
                "kind": "output_file",
                "path": output_file,
                "contains": "complete_json_result"
            },
            "materialization_ref": {
                "kind": "output_file_json_pointer",
                "path": output_file,
                "pointer": "/data/materialization"
            },
            "status_ref": {
                "kind": "output_file_json_pointer",
                "path": output_file,
                "pointer": "/data/status"
            },
            "status_summary": {
                "phase": controller.get("phase").cloned().unwrap_or(Value::Null),
                "state": controller.get("state").cloned().unwrap_or(Value::Null),
                "next_action_count": controller
                    .get("next_actions")
                    .and_then(Value::as_array)
                    .map(|actions| actions.len())
                    .unwrap_or(0),
                "entity_count": controller
                    .get("entities")
                    .and_then(Value::as_object)
                    .map(|entities| entities.len())
                    .unwrap_or(0),
                "terminal_outcome_count": terminal_outcomes,
                "diagnostics": diagnostics_summary,
                "evidence_refs": evidence_refs,
            }
        }
    }))
    .ok()
    .map(|json| format!("{}\n", json))
}

fn controller_evidence_refs(controller: &Value) -> Vec<Value> {
    let mut refs = Vec::new();
    if let Some(entities) = controller.get("entities").and_then(Value::as_object) {
        for entity in entities.values() {
            for key in ["artifacts", "artifact_refs", "evidence"] {
                if let Some(items) = entity.get(key).and_then(Value::as_array) {
                    refs.extend(items.iter().take(8 - refs.len()).cloned());
                    if refs.len() >= 8 {
                        return refs;
                    }
                }
            }
        }
    }
    refs
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
    use crate::commands::agent_task::{
        AgentTaskArgs, AgentTaskCommand, AgentTaskControllerArgs, AgentTaskControllerCommand,
        AgentTaskControllerDispatchArgs, AgentTaskControllerRunFromSpecArgs, StatusArgs,
    };

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
        let run = agent_task_command_run(Ok(payload.clone()), 0, None, false, None);

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
    fn changed_since_audit_projection_is_bounded_and_omits_findings() {
        let payload = serde_json::json!({
            "command": "audit.compared",
            "component_id": "large-component",
            "measurement": { "profile": "pr", "complete": false, "narrowed_by": ["changed-since"] },
            "summary": { "files_scanned": 1200, "warnings": (0..50_000).map(|_| "w".repeat(512)).collect::<Vec<_>>() },
            "findings": (0..50_000).map(|i| serde_json::json!({ "file": format!("src/{i}.rs"), "description": "x".repeat(512) })).collect::<Vec<_>>(),
            "changed_since": { "introduced_findings": 1, "contextual_findings": 49999 },
            "timing": { "spans": (0..50_000).map(|_| serde_json::json!({ "id": "detectors", "status": "ok", "duration_ms": 42.0 })).collect::<Vec<_>>() }
        });

        let rendered =
            serde_json::to_string(&bounded_audit_projection(&payload, 1, Some("audit.json")))
                .expect("projection serializes");

        assert!(
            rendered.len() < 2_000,
            "projection was {} bytes",
            rendered.len()
        );
        assert!(rendered.contains("\"verdict\":\"fail\""));
        assert!(rendered.contains("\"introduced_findings\":1"));
        assert!(rendered.contains("\"count\":50000"));
        assert!(rendered.contains("rerun with --full"));
        assert!(rendered.contains("audit.json"));
        assert!(!rendered.contains("src/49999.rs"));
        assert!(!rendered.contains(&"x".repeat(512)));
        assert!(!rendered.contains(&"w".repeat(512)));
    }

    #[test]
    fn changed_since_audit_projection_uses_durable_report_reference_without_output_file() {
        let payload = serde_json::json!({
            "full_report": {
                "uri": "homeboy://run/run-1/artifact/audit-report",
                "command": "homeboy runs evidence run-1",
            }
        });

        let rendered = bounded_audit_projection(&payload, 1, None);

        assert_eq!(
            rendered["full_report"]["uri"],
            "homeboy://run/run-1/artifact/audit-report"
        );
        assert_eq!(rendered["full_report"]["output"], Value::Null);
    }

    #[test]
    fn changed_since_audit_output_file_is_lossless_while_stdout_is_bounded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("audit.json");
        let payload = serde_json::json!({
            "command": "audit.compared",
            "component_id": "large-component",
            "measurement": { "profile": "pr", "complete": false, "narrowed_by": ["changed-since"] },
            "summary": { "files_scanned": 1, "warnings": [] },
            "findings": [{ "description": "x".repeat(512 * 1024) }],
            "changed_since": { "introduced_findings": 1, "contextual_findings": 0 },
            "timing": { "spans": [] }
        });
        let run = changed_since_audit_command_run((Ok(payload.clone()), 1), Some("audit.json"));
        let stdout = serde_json::to_string(run.stdout_result.as_ref().expect("bounded stdout"))
            .expect("stdout serializes");

        assert!(stdout.len() < 2_000, "stdout was {} bytes", stdout.len());
        assert!(!stdout.contains(&"x".repeat(512)));

        super::super::output_runtime::write_output_file(
            &run,
            crate::command_contract::CommandOutputFileMode::GenericEnvelope,
            Some(path.to_str().expect("utf8 path")),
        );
        let written: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(written["data"], payload);
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
            "failure": { "exit_code": 1, "stdout": "x".repeat(512 * 1024), "stderr": "y".repeat(512 * 1024) },
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

        let projection = bounded_refresh_error_projection(&error, 2, None);
        assert!(
            refresh_envelope_bytes(&projection, 2).expect("envelope serializes")
                <= MAX_REFRESH_PROJECTION_BYTES
        );
        assert_eq!(projection["artifacts"]["run_id"], "run-1");
        assert!(!serde_json::to_string(&projection)
            .unwrap()
            .contains(&"x".repeat(512)));
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
    fn controller_run_from_spec_with_output_file_emits_bounded_result_ref() {
        let large = "x".repeat(2 * 1024 * 1024);
        let payload = serde_json::json!({
            "schema": "homeboy/agent-task-loop-controller-run-from-spec-result/v1",
            "loop_id": "loop-large",
            "max_actions": 3,
            "stopped_reason": "terminal_state",
            "materialization": {
                "spec": { "large": large },
                "proof": { "kind": "materialization-proof" }
            },
            "from_spec": { "initialized": true },
            "results": [{ "large": large }],
            "status": {
                "controller": {
                    "phase": "running",
                    "state": "completed",
                    "next_actions": [{ "action_id": "action-1" }],
                    "entities": {
                        "entity-1": {
                            "evidence": [{ "kind": "proof", "uri": "artifact://proof" }]
                        }
                    },
                    "terminal_outcomes": [{ "outcome_id": "done" }]
                },
                "diagnostics": {
                    "summary": {
                        "pending_action_count": 0,
                        "stale_pending_action_count": 0,
                        "orphaned_pending_action_count": 0,
                        "acceptance_gate_count": 1,
                        "missing_acceptance_gate_count": 0,
                        "failed_acceptance_gate_count": 0
                    }
                }
            }
        });

        let stdout = render_controller_run_from_spec_output_ref(&payload, 0, "result.json")
            .expect("bounded output ref");
        let rendered: Value = serde_json::from_str(&stdout).expect("json stdout");

        assert!(stdout.len() < 4096, "stdout was {} bytes", stdout.len());
        assert!(!stdout.contains(&large));
        assert_eq!(rendered["success"], true);
        assert_eq!(rendered["data"]["loop_id"], "loop-large");
        assert_eq!(rendered["data"]["result_ref"]["path"], "result.json");
        assert_eq!(
            rendered["data"]["materialization_ref"]["pointer"],
            "/data/materialization"
        );
        assert_eq!(rendered["data"]["status_ref"]["pointer"], "/data/status");
        assert_eq!(rendered["data"]["status_summary"]["state"], "completed");
        assert_eq!(
            rendered["data"]["status_summary"]["evidence_refs"][0]["uri"],
            "artifact://proof"
        );
    }

    #[test]
    fn controller_run_from_spec_output_ref_requires_global_output_file() {
        let args = AgentTaskArgs {
            command: AgentTaskCommand::Controller(AgentTaskControllerArgs {
                command: AgentTaskControllerCommand::RunFromSpec(
                    AgentTaskControllerRunFromSpecArgs {
                        spec: "{}".to_string(),
                        inputs: None,
                        policy_results: Vec::new(),
                        max_actions: 1,
                        reconcile_stale: false,
                        replace: false,
                        fork: false,
                        resume_existing: false,
                        dispatch: AgentTaskControllerDispatchArgs {
                            dispatch_backend: None,
                            dispatch_selector: None,
                            dispatch_model: None,
                            dispatch_provider_config: None,
                        },
                    },
                ),
            }),
        };

        assert_eq!(
            agent_task_controller_run_from_spec_output_ref_eligible(&args, Some("result.json")),
            Some("result.json")
        );
        assert_eq!(
            agent_task_controller_run_from_spec_output_ref_eligible(&args, None),
            None
        );
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
