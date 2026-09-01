use homeboy_engine_primitives::content_hash;
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use base64::Engine;
use homeboy_core::api_jobs::{Job, RemoteRunnerSubmissionLookup, RunnerJobLifecycleMetadata};
use homeboy_core::error::{Error, Result};
use homeboy_core::lab_contract::LabRunnerWorkload;
use homeboy_core::source_snapshot::SourceSnapshot;
use homeboy_runner_contract::{
    RunnerApiSubmitOutcome, RunnerApiSubmitRequest, RunnerApiSubmitResponse,
    RUNNER_API_SUBMIT_REQUEST_SCHEMA, RUNNER_API_V1,
};
use reqwest::blocking::Client;

use super::super::broker_http;
use super::super::evidence::mirror_reverse_broker_evidence;
use super::super::Runner;

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
    extension_env_providers: Vec<String>,
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
    let submission_key = run_id.as_deref().map_or_else(
        || format!("reverse-broker:v1:{}:{}", runner.id, uuid::Uuid::new_v4()),
        |run_id| reverse_broker_submission_key(&runner.id, run_id),
    );
    let mut metadata =
        runner_exec_request_metadata(run_id.as_deref(), "reverse_broker", &runner.id);
    metadata["submission_key"] = serde_json::json!(&submission_key);
    let command_assets = durable_command_assets(&command, path_materialization_plan.as_ref())?;
    if !command_assets.is_empty() {
        metadata["command_assets"] = serde_json::json!({
            "schema": "homeboy/reverse-runner-command-assets/v1",
            "assets": command_assets,
        });
    }
    let envelope = runner_api_execution_envelope(RunnerApiExecutionInput {
        runner_id: runner.id.clone(),
        project_id,
        command: command.clone(),
        cwd: cwd.clone(),
        env,
        secret_env_names,
        capture_patch,
        source_snapshot: source_snapshot.clone(),
        path_materialization_plan: path_materialization_plan.clone(),
        workload: lab_runner_workload.clone(),
        metadata,
        lifecycle: RunnerJobLifecycleMetadata {
            source: Some("reverse-broker".to_string()),
            kind: Some("runner.exec".to_string()),
            durable_run_id: run_id.clone(),
            ..Default::default()
        },
        require_paths: require_paths.clone(),
        extension_env_providers,
    })?;
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
    // Reverse jobs hold a renewable owner lease while queued/running. A
    // reconciliation claim is a separate exclusive fence and is never used as
    // ordinary execution ownership.
    let workspace_owner_lease = run_id
        .as_deref()
        .map(homeboy_agents::agent_task_lifecycle::workspace_owner_registration_if_present)
        .transpose()?
        .flatten()
        .map(|(workspace, owner_id)| {
            let token = homeboy_core::broker_auth::broker_submit_token_for_runner(&runner.id)?;
            let data = broker_http::post_json(
                &client,
                broker_url,
                "/runner/workspace-owners/register",
                serde_json::json!({
                    "workspace": workspace,
                    "owner_id": owner_id,
                    "ttl_ms": homeboy_core::workspace_claim::MAX_WORKSPACE_CLAIM_TTL_MS,
                }),
                "register reverse broker workspace owner",
                token.as_deref(),
            )?;
            let lease: homeboy_core::workspace_claim::WorkspaceOwnerLease = serde_json::from_value(
                data.get("workspace_owner_lease")
                    .cloned()
                    .unwrap_or_default(),
            )
            .map_err(|error| {
                Error::validation_invalid_argument(
                    "workspace_owner_lease",
                    format!("malformed reverse broker owner lease: {error}"),
                    None,
                    None,
                )
            })?;
            lease.verify_shape(chrono::Utc::now().timestamp_millis().max(0) as u64)?;
            Ok::<_, Error>(lease)
        })
        .transpose()?;
    let submission = RunnerApiSubmitRequest {
        schema: RUNNER_API_SUBMIT_REQUEST_SCHEMA.to_string(),
        api_version: RUNNER_API_V1,
        submission_key: submission_key.clone(),
        envelope,
        workspace_claim_binding: None,
        workspace_owner_lease: workspace_owner_lease
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|error| {
                Error::internal_json(
                    error.to_string(),
                    Some("serialize workspace owner lease".to_string()),
                )
            })?,
    };
    if detach_after_handoff {
        if let Some(run_id) = run_id.as_deref() {
            homeboy_agents::agent_task_lifecycle::record_lab_offload_submission_envelope(
                run_id,
                &submission,
            )?;
        }
    }
    let broker_token = homeboy_core::broker_auth::broker_submit_token_for_runner(&runner.id)?;
    let data = broker_http::post_json(
        &client,
        broker_url,
        "/runner/jobs",
        serde_json::to_value(&submission).map_err(|err| {
            Error::internal_json(
                err.to_string(),
                Some("serialize reverse runner job request".to_string()),
            )
        })?,
        "submit reverse runner job",
        broker_token.as_deref(),
    );
    let data = data.and_then(|data| {
        if let Some(response) = data.get("response") {
            let response: RunnerApiSubmitResponse = serde_json::from_value(response.clone())
                .map_err(|error| {
                    Error::internal_json(
                        error.to_string(),
                        Some("parse runner submit response".to_string()),
                    )
                })?;
            if let RunnerApiSubmitOutcome::Rejected { failure } = response.outcome {
                return Err(Error::validation_invalid_argument(
                    "runner_submission",
                    failure.message,
                    None,
                    None,
                ));
            }
        }
        Ok(data)
    });
    let data = match data {
        Ok(data) => data,
        Err(error) => {
            let submission_key = Some(submission_key.clone());
            let accepted =
                submission_key.as_deref().map(|submission_key| {
                    broker_http::post_json(
                    &client,
                    broker_url,
                    "/runner/jobs/submissions/lookup",
                    serde_json::json!({ "runner_id": runner.id, "submission_key": submission_key }),
                    "look up ambiguous reverse broker submission",
                    broker_token.as_deref(),
                )
                .and_then(|data| {
                    serde_json::from_value::<RemoteRunnerSubmissionLookup>(
                        data.get("result").cloned().unwrap_or_default(),
                    )
                    .map_err(|parse_error| Error::internal_json(
                        parse_error.to_string(),
                        Some("parse reverse broker submission lookup".to_string()),
                    ))
                })
                });
            if let Some(Ok(RemoteRunnerSubmissionLookup::Accepted { job })) = accepted.as_ref() {
                // The broker accepted this exact immutable request. Keep its
                // original owner lease and continue through normal binding.
                serde_json::json!({ "job": job })
            } else {
                let non_acceptance = matches!(
                    accepted.as_ref(),
                    Some(Ok(RemoteRunnerSubmissionLookup::Absent
                        | RemoteRunnerSubmissionLookup::Expired { .. }))
                );
                if !non_acceptance {
                    return Err(Error::new(
                        error.code,
                        error.message,
                        serde_json::json!({
                            "workspace_owner_lease_recovery": {
                                "schema": homeboy_core::workspace_claim::WORKSPACE_OWNER_RELEASE_RECOVERY_SCHEMA,
                                "lease": workspace_owner_lease,
                                "submission_key": submission_key,
                                "lookup": accepted.as_ref().and_then(|result| result.as_ref().err()).map(ToString::to_string),
                            }
                        }),
                    ));
                }
                if let Some(lease) = workspace_owner_lease.as_ref() {
                    let cleanup = broker_http::post_json(
                        &client,
                        broker_url,
                        "/runner/workspace-owners/release",
                        serde_json::json!({ "workspace_owner_lease": lease }),
                        "rollback reverse broker workspace owner",
                        broker_token.as_deref(),
                    );
                    if let Err(cleanup_error) = cleanup {
                        return Err(Error::new(
                            error.code,
                            error.message,
                            serde_json::json!({
                                "workspace_owner_lease_cleanup": {
                                    "schema": homeboy_core::workspace_claim::WORKSPACE_OWNER_RELEASE_RECOVERY_SCHEMA,
                                    "lease": lease,
                                    "error": cleanup_error.message,
                                }
                            }),
                        ));
                    }
                }
                return Err(error);
            }
        }
    };
    let job_value = data
        .get("job")
        .ok_or_else(|| Error::internal_unexpected("reverse broker submit returned no job"))?;
    let job: Job = serde_json::from_value(job_value.clone()).map_err(|err| {
        Error::internal_json(
            err.to_string(),
            Some("parse reverse broker job".to_string()),
        )
    })?;
    return complete_submitted_runner_job(
        SubmittedRunnerJobFlow {
            runner,
            mode: RunnerExecMode::ReverseBroker,
            transport: "reverse_broker",
            runner_job_transport: "broker",
            timeout_label: "reverse runner job",
            cwd: cwd.clone(),
            command: command.clone(),
            redaction_env: &redaction_env,
            secret_env_names: &redaction_secret_env_names,
            source_snapshot: source_snapshot.clone(),
            path_materialization_plan: path_materialization_plan.clone(),
            require_paths: require_paths.clone(),
            lab_runner_workload: lab_runner_workload.clone(),
            run_id: run_id.clone(),
            run_id_owns_generic_exec,
            detach_after_handoff,
            mirror_evidence,
            print_handoff_output,
            handoff_endpoint: Some(broker_url),
        },
        job,
        |_| Ok(()),
        |_| Ok(()),
        |current| {
            fetch_daemon_job_resilient(&client, broker_url, &current.id.to_string()).map_err(
                |err| {
                    terminal_runner_poll_failure(
                        runner,
                        &cwd,
                        &command,
                        current,
                        "reverse_broker",
                        path_materialization_plan.as_ref(),
                        &source_snapshot,
                        &require_paths,
                        None,
                        None,
                        err,
                    )
                },
            )
        },
        |job| fetch_daemon_events(&client, broker_url, &job.id.to_string()),
        |job, events, result| {
            let request = crate::evidence::MirrorEvidenceRequest::new(
                runner,
                &cwd,
                &command,
                job,
                events,
                result,
                run_id.as_deref(),
                lab_runner_workload
                    .as_ref()
                    .and_then(|workload| workload.notification_route.as_ref()),
            );
            let request = if run_id_owns_generic_exec {
                request.with_generic_runner_exec_run()
            } else if run_id.is_some() {
                request.with_agent_task_run()
            } else {
                request
            };
            mirror_reverse_broker_evidence(crate::evidence::ReverseBrokerEvidenceContext {
                request,
                broker_url,
            })
            .and_then(|evidence| {
                evidence
                    .map(|evidence| {
                        Ok(MirroredJobEvidence {
                            run_id: evidence.run.id,
                            patch: evidence.patch,
                            artifacts: crate::evidence::controller_artifact_metadata(
                                &evidence.runs,
                            )?,
                        })
                    })
                    .transpose()
            })
        },
        || Ok(()),
        |_, _| Ok(()),
    );
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
                "sha256": content_hash::sha256_hex(&content),
                "content_base64": base64::engine::general_purpose::STANDARD.encode(content),
            }))
        })
        .collect()
}
