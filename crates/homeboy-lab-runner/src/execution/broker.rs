use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use base64::Engine;
use homeboy_core::api_jobs::{Job, JobStatus, RemoteRunnerJobRequest, RunnerJobLifecycleMetadata};
use homeboy_core::error::{Error, Result};
use homeboy_core::lab_contract::LabRunnerWorkload;
use homeboy_core::redaction::redact_argv;
use homeboy_core::source_snapshot::SourceSnapshot;
use reqwest::blocking::Client;
use sha2::{Digest, Sha256};

use super::super::broker_http;
use super::super::evidence::mirror_reverse_broker_evidence;
use super::super::{Runner, RunnerJob};

#[allow(unused_imports)]
use super::*;

pub(crate) fn reverse_broker_submission_key(runner_id: &str, run_id: &str) -> String {
    format!("agent-task:v1:{runner_id}:{run_id}")
}

#[allow(clippy::too_many_arguments)]
pub(super) fn exec_via_reverse_broker(
    runner: &Runner,
    broker_url: &str,
    cwd: String,
    project_id: Option<String>,
    command: Vec<String>,
    env: HashMap<String, String>,
    secret_env_names: Vec<String>,
    capture_patch: bool,
    source_snapshot_override: Option<SourceSnapshot>,
    path_materialization_plan: Option<PathMaterializationPlan>,
    require_paths: Vec<String>,
    lab_runner_workload: Option<LabRunnerWorkload>,
    run_id: Option<String>,
    run_id_owns_generic_exec: bool,
    detach_after_handoff: bool,
    mirror_evidence: bool,
    print_handoff_output: bool,
) -> Result<(RunnerExecOutput, i32)> {
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| Error::internal_unexpected(format!("build broker HTTP client: {err}")))?;
    let source_snapshot = source_snapshot_override.unwrap_or_else(|| {
        homeboy_core::source_snapshot::existing_remote(
            &runner.id,
            &cwd,
            runner.workspace_root.as_deref(),
        )
    });
    persist_runner_execution_transition(
        &RunnerExecutionRecord::planned(
            format!("runner-exec:{}:reverse_broker", runner.id),
            runner.id.clone(),
            "reverse_broker",
        )
        .with_path_materialization_plan(path_materialization_plan.clone())
        .with_orchestration_provenance(orchestration_target_provenance(
            runner,
            None,
            Some(&source_snapshot),
            &[],
        )),
        &cwd,
        &command,
    )?;
    let redaction_env = env.clone();
    let redaction_secret_env_names = secret_env_names.clone();
    let mut env = env;
    // Snapshot the configured command binary into the durable job. A later
    // daemon refresh must not redirect work that has already been accepted.
    if !env.contains_key("HOMEBOY_COMMAND") {
        if let Some(homeboy_path) = runner.settings.homeboy_path.as_deref() {
            env.insert("HOMEBOY_COMMAND".to_string(), homeboy_path.to_string());
        }
    }
    let mut request = RemoteRunnerJobRequest {
        runner_id: runner.id.clone(),
        project_id,
        operation: "runner.exec".to_string(),
        command: command.clone(),
        cwd: Some(cwd.clone()),
        env,
        secret_env_names,
        secret_env_plan: Default::default(),
        env_materialization: None,
        capture_patch,
        source_snapshot: Some(source_snapshot.clone()),
        path_materialization_plan: path_materialization_plan.clone(),
        lab_runner_workload: lab_runner_workload.clone(),
        metadata: Some({
            let mut metadata = runner_exec_request_metadata(run_id.as_deref(), "reverse_broker");
            if let Some(run_id) = run_id.as_deref() {
                metadata["submission_key"] =
                    serde_json::json!(reverse_broker_submission_key(&runner.id, run_id));
            }
            metadata
        }),
        lifecycle: Some(RunnerJobLifecycleMetadata {
            source: Some("reverse-broker".to_string()),
            kind: Some("runner.exec".to_string()),
            durable_run_id: run_id.clone(),
            ..Default::default()
        }),
        require_paths: require_paths.clone(),
    };
    let command_assets = durable_command_assets(&command, path_materialization_plan.as_ref())?;
    if !command_assets.is_empty() {
        request
            .metadata
            .as_mut()
            .expect("reverse broker request metadata")["command_assets"] = serde_json::json!({
            "schema": "homeboy/reverse-runner-command-assets/v1",
            "assets": command_assets,
        });
    }
    if detach_after_handoff {
        if let Some(run_id) = run_id.as_deref() {
            homeboy_agents::agent_task_lifecycle::record_lab_offload_submission_request(
                run_id, &request,
            )?;
        }
    }
    let broker_token = homeboy_core::broker_auth::broker_submit_token_for_runner(&runner.id)?;
    let data = broker_http::post_json(
        &client,
        broker_url,
        "/runner/jobs",
        serde_json::to_value(&request).map_err(|err| {
            Error::internal_json(
                err.to_string(),
                Some("serialize reverse runner job request".to_string()),
            )
        })?,
        "submit reverse runner job",
        broker_token.as_deref(),
    )?;
    let job_value = data
        .get("job")
        .ok_or_else(|| Error::internal_unexpected("reverse broker submit returned no job"))?;
    let mut job: Job = serde_json::from_value(job_value.clone()).map_err(|err| {
        Error::internal_json(
            err.to_string(),
            Some("parse reverse broker job".to_string()),
        )
    })?;
    persist_runner_execution_transition(
        &RunnerExecutionRecord::in_flight(job.id.to_string(), runner.id.clone(), "reverse_broker")
            .with_job_id(job.id.to_string())
            .with_path_materialization_plan(path_materialization_plan.clone())
            .with_orchestration_provenance(orchestration_target_provenance(
                runner,
                None,
                Some(&source_snapshot),
                &[],
            ))
            .with_next_actions(runner_execution_next_actions(
                &runner.id,
                &job.id.to_string(),
            )),
        &cwd,
        &command,
    )?;
    if let Some(run_id) = run_id.as_deref() {
        // Every portable agent-task run is a controller-owned Lab handoff.
        // Persist the accepted daemon job before foreground cook or retry can
        // validate a runner snapshot; metadata-only binding leaves the typed
        // handoff pending and loses that identity at preacceptance (#9240).
        if !run_id_owns_generic_exec {
            homeboy_agents::agent_task_lifecycle::bind_accepted_lab_runner_job(
                &homeboy_core::lab_contract::RunnerJobIdentity::new(
                    run_id,
                    &runner.id,
                    job.id.to_string(),
                ),
                &cwd,
                &command,
            )?;
        } else {
            homeboy_agents::agent_task_lifecycle::record_runner_exec_job_identity(
                run_id,
                &runner.id,
                &job.id.to_string(),
                &cwd,
                &command,
            )?;
        }
    }
    let persisted_run_id = mirror_evidence
        .then(|| persist_lab_offload_handoff_run(runner, &cwd, &command, &job, run_id.as_deref()))
        .flatten();
    validate_generic_exec_mirror_run_id(
        run_id_owns_generic_exec,
        run_id.as_deref(),
        persisted_run_id.as_deref(),
    )?;
    if detach_after_handoff {
        return Ok(detached_handoff_output(
            runner,
            RunnerExecMode::ReverseBroker,
            cwd,
            command,
            source_snapshot,
            job,
            path_materialization_plan,
            require_paths,
            run_id,
            persisted_run_id,
        ));
    }

    let deadline = Instant::now() + runner_exec_wait_timeout();
    while !matches!(
        job.status,
        JobStatus::Succeeded | JobStatus::Failed | JobStatus::Cancelled
    ) {
        if Instant::now() >= deadline {
            let events = fetch_daemon_events(&client, broker_url, &job.id.to_string())
                .map(|events| {
                    redact_runner_job_events(&events, &redaction_env, &redaction_secret_env_names)
                })
                .unwrap_or_default();
            return Err(daemon_job_wait_timeout(
                runner,
                &cwd,
                &command,
                &job,
                &events,
                "reverse runner job",
                true,
            ));
        }
        std::thread::sleep(Duration::from_millis(200));
        let job_id = job.id.to_string();
        job = fetch_daemon_job_resilient(&client, broker_url, &job_id).map_err(|err| {
            terminal_runner_poll_failure(
                runner,
                &cwd,
                &command,
                &job,
                "reverse_broker",
                path_materialization_plan.as_ref(),
                &source_snapshot,
                &require_paths,
                persisted_run_id.as_deref(),
                None,
                err,
            )
        })?;
    }
    let events = redact_runner_job_events(
        &fetch_daemon_events(&client, broker_url, &job.id.to_string())?,
        &redaction_env,
        &redaction_secret_env_names,
    );

    let RunnerJobResultFields {
        result,
        stdout,
        stderr,
        metrics,
        capture,
        exit_code,
    } = runner_job_result_fields(
        &events,
        job.status,
        &redaction_env,
        &redaction_secret_env_names,
    );

    let mirror = if mirror_evidence {
        mirror_reverse_broker_evidence(
            runner,
            broker_url,
            &cwd,
            &command,
            &job,
            &events,
            &result,
            run_id.as_deref(),
            lab_runner_workload
                .as_ref()
                .and_then(|workload| workload.notification_route.as_ref()),
        )?
    } else {
        None
    };
    let patch = mirror.as_ref().and_then(|evidence| evidence.patch.clone());
    let mirror_run_id = mirror.as_ref().map(|evidence| evidence.run.id.clone());
    validate_generic_exec_mirror_run_id(
        run_id_owns_generic_exec,
        run_id.as_deref(),
        mirror_run_id.as_deref(),
    )?;
    fire_runner_direct_notification(
        run_id.as_deref(),
        &job,
        lab_runner_workload
            .as_ref()
            .and_then(|workload| workload.notification_route.as_ref()),
    );
    let artifacts = job.artifacts.clone();
    let mutation_artifacts = mutation_artifacts_from_job(&job, &result);

    if print_handoff_output {
        print_lab_offload_handoff(
            &runner.id,
            Some(&cwd),
            &job.id.to_string(),
            mirror_run_id.as_deref(),
            DaemonJobHandoffState::Terminal(job.status),
        );
    }

    let runner_job = RunnerJob::from_job(&runner.id, "broker", &command, Some(cwd.clone()), &job);
    let runner_result = runner_result(
        Some(&job),
        exit_code,
        &stdout,
        &stderr,
        mirror_run_id.as_deref(),
        mutation_artifacts.clone(),
    );
    let provenance_extensions = required_extensions_for_command(
        &command,
        &super::super::workload::merge_lab_runner_workload_required_extensions(
            Vec::new(),
            lab_runner_workload.as_ref(),
        ),
    );
    let handoff = lab_runner_handoff(
        runner,
        "reverse_broker",
        Some(runner_job.clone()),
        Some(runner_result.clone()),
    );
    let execution_record = runner_execution_record_for_output(
        runner,
        "reverse_broker",
        exit_code,
        Some(job.id.to_string()),
        mirror_run_id.clone(),
        Some(&source_snapshot),
        path_materialization_plan,
        &require_paths,
        &provenance_extensions,
        &artifacts,
        Some(&runner_result),
    );
    persist_runner_execution_transition(&execution_record, &cwd, &command)?;

    Ok((
        RunnerExecOutput {
            variant: "exec",
            command: "runner.exec",
            runner_id: runner.id.clone(),
            dry_run: false,
            mode: RunnerExecMode::ReverseBroker,
            argv: redact_argv(&command),
            remote_cwd: cwd,
            exit_code,
            stdout,
            stderr,
            source_snapshot: Some(source_snapshot.clone()),
            job_id: Some(job.id.to_string()),
            job: Some(job),
            runner_job: Some(runner_job),
            job_events: Some(events),
            mirror_run_id,
            patch,
            mutation_artifacts,
            artifacts,
            promoted_outputs: Vec::new(),
            structured_summaries: Vec::new(),
            metrics,
            capture,
            execution_record: Some(execution_record),
            runner_result: Some(runner_result),
            handoff: Some(handoff),
            diagnostics: runner_exec_diagnostics(runner, Some(&source_snapshot), &require_paths),
        },
        exit_code,
    ))
}

/// Preserve file-backed argv values past controller cleanup. Values are content
/// addressed and stored only in the broker request, never in controller tempdirs.
fn durable_command_assets(
    command: &[String],
    plan: Option<&PathMaterializationPlan>,
) -> Result<Vec<serde_json::Value>> {
    const MAX_COMMAND_ASSET_BYTES: u64 = 1_048_576;
    const MAX_COMMAND_ASSETS_BYTES: u64 = 3_145_728;
    let Some(plan) = plan else {
        return Ok(Vec::new());
    };
    command
        .iter()
        .filter_map(|argument| argument.strip_prefix('@').map(|path| (argument, path)))
        .map(|(argument, remote_path)| {
            let entry = plan
                .entries
                .iter()
                .find(|entry| {
                    remote_path == entry.remote_path
                        || remote_path
                            .strip_prefix(&entry.remote_path)
                            .is_some_and(|suffix| suffix.starts_with('/'))
                })
                .ok_or_else(|| {
                    Error::validation_invalid_argument(
                        "command",
                        "file-backed command argument is outside the materialization plan",
                        Some(argument.to_string()),
                        None,
                    )
                })?;
            let local = Path::new(entry.local_path.as_deref().ok_or_else(|| {
                Error::validation_invalid_argument(
                    "path_materialization_plan",
                    "command asset materialization entry has no local path",
                    Some(entry.remote_path.clone()),
                    None,
                )
            })?);
            let source = if local.is_file() {
                if remote_path != entry.remote_path {
                    return Err(Error::validation_invalid_argument(
                        "command",
                        "file-backed command argument does not match its materialized file",
                        Some(argument.to_string()),
                        None,
                    ));
                }
                local.to_path_buf()
            } else {
                let relative = remote_path
                    .strip_prefix(&entry.remote_path)
                    .unwrap_or_default()
                    .trim_start_matches('/');
                let relative = Path::new(relative);
                if relative
                    .components()
                    .any(|component| !matches!(component, std::path::Component::Normal(_)))
                {
                    return Err(Error::validation_invalid_argument(
                        "command",
                        "file-backed command argument has an unsafe materialized path",
                        Some(argument.to_string()),
                        None,
                    ));
                }
                local.join(relative)
            };
            if !source.is_file() {
                return Ok(None);
            }
            let source = source.canonicalize().map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!("canonicalize command asset {}", source.display())),
                )
            })?;
            let local = local.canonicalize().map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!(
                        "canonicalize materialization root {}",
                        local.display()
                    )),
                )
            })?;
            if !source.starts_with(&local) {
                return Err(Error::validation_invalid_argument(
                    "command",
                    "file-backed command argument resolves outside the materialization root",
                    Some(argument.to_string()),
                    None,
                ));
            }
            Ok(Some((argument, remote_path, source)))
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .map({
            let mut total = 0u64;
            move |(argument, remote_path, source)| {
                let size = std::fs::metadata(&source)
                    .map_err(|err| {
                        Error::internal_io(
                            err.to_string(),
                            Some(format!("stat command asset {}", source.display())),
                        )
                    })?
                    .len();
                if size > MAX_COMMAND_ASSET_BYTES
                    || total.saturating_add(size) > MAX_COMMAND_ASSETS_BYTES
                {
                    return Err(Error::validation_invalid_argument(
                        "command",
                        "file-backed command assets exceed the size limit",
                        Some(argument.to_string()),
                        None,
                    ));
                }
                total += size;
                Ok((argument, remote_path, source))
            }
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .map(|(argument, remote_path, source)| {
            let content = std::fs::read(&source).map_err(|err| {
                Error::internal_io(
                    err.to_string(),
                    Some(format!("read command asset {}", source.display())),
                )
            })?;
            Ok(serde_json::json!({
                "argument": argument,
                "remote_path": remote_path,
                "sha256": format!("{:x}", Sha256::digest(&content)),
                "content_base64": base64::engine::general_purpose::STANDARD.encode(content),
            }))
        })
        .collect()
}
