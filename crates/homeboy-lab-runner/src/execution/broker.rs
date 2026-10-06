use homeboy_engine_primitives::content_hash;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use base64::Engine;
use homeboy_core::api_jobs::{Job, RemoteRunnerSubmissionLookup, RunnerJobLifecycleMetadata};
use homeboy_core::error::{Error, Result};
use homeboy_core::lab_contract::LabRunnerWorkload;
use homeboy_core::secret_env_plan::SecretEnvPlan;
use homeboy_core::source_snapshot::SourceSnapshot;
use homeboy_runner_contract::{
    RunnerApiSubmitOutcome, RunnerApiSubmitRequest, RunnerApiSubmitResponse, WorkspaceOwnerLease,
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
    secret_env_plan: SecretEnvPlan,
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
    let controller_credential_delivery = {
        // SecretEnvPlan is intentionally name-only. The materialization plan is
        // the durable ownership authority, so an ambient controller value never
        // overrides a runner-owned reference with the same name.
        let controller_owned = secret_env_plan
            .env_materialization
            .as_ref()
            .map(|plan| {
                plan.secret_refs
                    .iter()
                    .filter(|secret| secret.owner.as_deref() == Some("controller"))
                    .map(|secret| secret.name.as_str())
                    .collect::<std::collections::BTreeSet<_>>()
            })
            .unwrap_or_default();
        let env: BTreeMap<_, _> = redaction_env
            .iter()
            .filter(|(name, _)| controller_owned.contains(name.as_str()))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        (!env.is_empty()).then_some(homeboy_runner_contract::RunnerCredentialDelivery { env })
    };
    // Durable reverse-runner jobs cannot persist inline secret values
    // (`reject_inline_durable_secret_env`). Strip every planned secret name —
    // including provider credential requirements and env-name aliases — so the
    // stored envelope carries references only, and the worker rehydrates the
    // values from runner-owned sources after replay (Extra-Chill/homeboy#14382).
    let mut env = strip_durable_secret_env_values(env, &secret_env_plan);
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
        secret_env_plan: Some(secret_env_plan),
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
            let lease: WorkspaceOwnerLease = serde_json::from_value(
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
        workspace_owner_lease: workspace_owner_lease.clone(),
        credential_delivery: controller_credential_delivery,
    };
    if detach_after_handoff {
        if let Some(run_id) = run_id.as_deref() {
            let mut durable_submission = submission.clone();
            durable_submission.credential_delivery = None;
            homeboy_agents::agent_task_lifecycle::record_lab_offload_submission_envelope(
                run_id,
                &durable_submission,
            )?;
        }
    }
    let broker_token = homeboy_core::broker_auth::broker_submit_token_for_runner(&runner.id)?;
    let lifecycle_store = if detach_after_handoff && run_id.is_some() {
        Some(homeboy_agents::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?)
    } else {
        None
    };
    let data = submit_reverse_broker_job_with_admission(
        &client,
        broker_url,
        &runner.id,
        &submission_key,
        &submission,
        workspace_owner_lease.as_ref(),
        broker_token.as_deref(),
        lifecycle_store.as_ref(),
        run_id.as_deref(),
    )?;
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
    )
    .map(RunnerExecCompletion::into_output);
}

/// Submit through the broker while preserving the admission fence's ownership
/// result. The submission resolver owns cleanup only after a proven absent or
/// expired lookup; ambiguous lookup and accepted identity retain lease custody.
fn submit_reverse_broker_job_with_admission(
    client: &Client,
    broker_url: &str,
    runner_id: &str,
    submission_key: &str,
    submission: &RunnerApiSubmitRequest,
    workspace_owner_lease: Option<&WorkspaceOwnerLease>,
    broker_token: Option<&str>,
    lifecycle_store: Option<&homeboy_agents::agent_task_lifecycle::AgentTaskLifecycleStore>,
    run_id: Option<&str>,
) -> Result<serde_json::Value> {
    let submit_and_resolve = || {
        submit_and_resolve_reverse_broker_job(
            client,
            broker_url,
            runner_id,
            submission_key,
            submission,
            workspace_owner_lease,
            broker_token,
        )
    };
    let Some((lifecycle_store, run_id)) = lifecycle_store.zip(run_id) else {
        return submit_and_resolve();
    };
    match homeboy_agents::agent_task_lifecycle::with_pending_runner_submission_admission_in_store(
        lifecycle_store,
        run_id,
        submission,
        submit_and_resolve,
    ) {
        Ok(data) => Ok(data),
        Err(
            homeboy_agents::agent_task_lifecycle::PendingRunnerSubmissionAdmissionError::Submission(
                error,
            ),
        ) => {
            // Transport ambiguity retains the original lease custody; the inner
            // submission resolver still owns accepted and proven-absence logic.
            Err(error)
        }
        Err(
            homeboy_agents::agent_task_lifecycle::PendingRunnerSubmissionAdmissionError::Rejected(
                error,
            ),
        ) => release_workspace_owner_lease(
            client,
            broker_url,
            workspace_owner_lease,
            broker_token,
            "rollback reverse broker workspace owner after cancellation fence",
            error,
        ),
    }
}

fn submit_and_resolve_reverse_broker_job(
    client: &Client,
    broker_url: &str,
    runner_id: &str,
    submission_key: &str,
    submission: &RunnerApiSubmitRequest,
    workspace_owner_lease: Option<&WorkspaceOwnerLease>,
    broker_token: Option<&str>,
) -> Result<serde_json::Value> {
    let submitted = broker_http::post_json(
        client,
        broker_url,
        "/runner/jobs",
        serde_json::to_value(submission).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize reverse runner job request".to_string()),
            )
        })?,
        "submit reverse runner job",
        broker_token,
    )
    .and_then(|data| {
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
    let Err(submission_error) = submitted else {
        return submitted;
    };

    let lookup = broker_http::post_json(
        client,
        broker_url,
        "/runner/jobs/submissions/lookup",
        serde_json::json!({ "runner_id": runner_id, "submission_key": submission_key }),
        "look up ambiguous reverse broker submission",
        broker_token,
    )
    .and_then(|data| {
        serde_json::from_value::<RemoteRunnerSubmissionLookup>(
            data.get("result").cloned().unwrap_or_default(),
        )
        .map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("parse reverse broker submission lookup".to_string()),
            )
        })
    });
    if let Ok(RemoteRunnerSubmissionLookup::Accepted { job }) = &lookup {
        return Ok(serde_json::json!({ "job": job }));
    }
    let proven_absent = matches!(
        &lookup,
        Ok(RemoteRunnerSubmissionLookup::Absent | RemoteRunnerSubmissionLookup::Expired { .. })
    );
    if !proven_absent {
        return Err(Error::new(
            submission_error.code,
            submission_error.message,
            serde_json::json!({
                "workspace_owner_lease_recovery": {
                    "schema": homeboy_core::workspace_claim::WORKSPACE_OWNER_RELEASE_RECOVERY_SCHEMA,
                    "lease": workspace_owner_lease,
                    "submission_key": submission_key,
                    "lookup": lookup.as_ref().err().map(ToString::to_string),
                }
            }),
        ));
    }
    release_workspace_owner_lease(
        client,
        broker_url,
        workspace_owner_lease,
        broker_token,
        "rollback reverse broker workspace owner",
        submission_error,
    )
}

fn release_workspace_owner_lease(
    client: &Client,
    broker_url: &str,
    lease: Option<&WorkspaceOwnerLease>,
    broker_token: Option<&str>,
    action: &str,
    prior_error: Error,
) -> Result<serde_json::Value> {
    let Some(lease) = lease else {
        return Err(prior_error);
    };
    if let Err(cleanup_error) = broker_http::post_json(
        client,
        broker_url,
        "/runner/workspace-owners/release",
        serde_json::json!({ "workspace_owner_lease": lease }),
        action,
        broker_token,
    ) {
        return Err(Error::new(
            prior_error.code,
            prior_error.message,
            serde_json::json!({
                "workspace_owner_lease_cleanup": {
                    "schema": homeboy_core::workspace_claim::WORKSPACE_OWNER_RELEASE_RECOVERY_SCHEMA,
                    "lease": lease,
                    "error": cleanup_error.message,
                }
            }),
        ));
    }
    Err(prior_error)
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

#[cfg(test)]
mod ownership_tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc;
    use std::thread::JoinHandle;

    struct Reply {
        path: &'static str,
        status: Option<u16>,
        body: serde_json::Value,
    }

    fn start_broker(
        replies: Vec<Reply>,
    ) -> (
        String,
        mpsc::Receiver<Vec<(String, serde_json::Value)>>,
        JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind broker fixture");
        let url = format!("http://{}", listener.local_addr().expect("broker addr"));
        let (requests_tx, requests_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for reply in replies {
                let (mut stream, _) = listener.accept().expect("accept broker HTTP request");
                let (path, body) = read_http_request(&mut stream);
                assert_eq!(path, reply.path, "unexpected broker request order");
                requests.push((path, body));
                if let Some(status) = reply.status {
                    let response_body = reply.body.to_string();
                    let reason = if status < 400 { "OK" } else { "Bad Gateway" };
                    write!(
                        stream,
                        "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        response_body.len(),
                        response_body
                    )
                    .expect("write broker response");
                }
            }
            requests_tx
                .send(requests)
                .expect("return captured requests");
        });
        (url, requests_rx, server)
    }

    fn read_http_request(stream: &mut TcpStream) -> (String, serde_json::Value) {
        let mut bytes = Vec::new();
        let mut chunk = [0_u8; 4096];
        let (header_end, content_length) = loop {
            let read = stream.read(&mut chunk).expect("read broker request");
            assert_ne!(read, 0, "client closed before completing request");
            bytes.extend_from_slice(&chunk[..read]);
            if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                let header = String::from_utf8_lossy(&bytes[..end]);
                let content_length = header
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().expect("content length"))
                    })
                    .unwrap_or(0);
                if bytes.len() >= end + 4 + content_length {
                    break (end, content_length);
                }
            }
        };
        let header = String::from_utf8_lossy(&bytes[..header_end]);
        let path = header
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .expect("request target")
            .to_string();
        let body_start = header_end + 4;
        let body = serde_json::from_slice(&bytes[body_start..body_start + content_length])
            .expect("decode broker JSON request");
        (path, body)
    }

    fn lease(owner_id: &str) -> WorkspaceOwnerLease {
        WorkspaceOwnerLease {
            schema: homeboy_runner_contract::WORKSPACE_OWNER_LEASE_SCHEMA.to_string(),
            protocol: homeboy_runner_contract::WorkspaceOwnerLeaseProtocol::current(),
            workspace: homeboy_runner_contract::WorkspaceIdentity::new(
                "managed-workspace",
                "ownership-matrix/repo",
            )
            .expect("workspace identity"),
            owner_id: owner_id.to_string(),
            lifecycle_revision: 7,
            token: "owner-lease-token".to_string(),
            expires_at_ms: chrono::Utc::now().timestamp_millis() as u64 + 60_000,
        }
    }

    fn submission(run_id: &str, lease: Option<WorkspaceOwnerLease>) -> RunnerApiSubmitRequest {
        let submission_key = reverse_broker_submission_key("homeboy-lab", run_id);
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        let envelope = runner_api_execution_envelope(RunnerApiExecutionInput {
            runner_id: "homeboy-lab".to_string(),
            project_id: None,
            command,
            cwd: "/runner/workspace/homeboy".to_string(),
            env: Default::default(),
            secret_env_names: Vec::new(),
            secret_env_plan: None,
            capture_patch: false,
            source_snapshot: SourceSnapshot::default(),
            path_materialization_plan: None,
            require_paths: Vec::new(),
            extension_env_providers: Vec::new(),
            workload: None,
            lifecycle: RunnerJobLifecycleMetadata {
                source: Some("reverse-broker".to_string()),
                kind: Some("runner.exec".to_string()),
                durable_run_id: Some(run_id.to_string()),
                ..Default::default()
            },
            metadata: serde_json::json!({
                "durable_run_id": run_id,
                "submission_key": submission_key,
            }),
        })
        .expect("runner execution envelope");
        RunnerApiSubmitRequest {
            schema: RUNNER_API_SUBMIT_REQUEST_SCHEMA.to_string(),
            api_version: RUNNER_API_V1,
            submission_key,
            envelope,
            workspace_claim_binding: None,
            workspace_owner_lease: lease,
            credential_delivery: None,
        }
    }

    fn job() -> Job {
        Job {
            id: uuid::Uuid::new_v4(),
            operation: "runner.exec".to_string(),
            status: homeboy_core::api_jobs::JobStatus::Queued,
            created_at_ms: 1,
            updated_at_ms: 1,
            started_at_ms: None,
            finished_at_ms: None,
            event_count: 0,
            source_snapshot: None,
            path_materialization_plan: None,
            stale_reason: None,
            daemon_lease_id: None,
            target_runner_id: Some("homeboy-lab".to_string()),
            target_project_id: None,
            claim_id: None,
            claimed_by_runner_id: None,
            claimed_at_ms: None,
            claim_expires_at_ms: None,
            artifacts: Vec::new(),
            runner_job_projection: None,
        }
    }

    fn failed_reply(message: &str) -> serde_json::Value {
        serde_json::json!({
            "success": false,
            "error": { "code": "internal.unexpected", "message": message },
        })
    }

    fn successful_reply(body: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "success": true, "data": { "body": body } })
    }

    struct AcceptedJobContinuation {
        jobs: homeboy_core::api_jobs::JobStore,
    }

    impl homeboy_agents::agent_task_lifecycle::RunnerContinuationProvider for AcceptedJobContinuation {
        fn runner_job_log_snapshot(
            &self,
            _runner_id: &str,
            _job_id: &str,
        ) -> homeboy_core::Result<homeboy_core::api_jobs::RunnerJobLogSnapshot> {
            Err(Error::internal_unexpected(
                "snapshot not used in ownership race",
            ))
        }

        fn is_runner_connected(&self, _runner_id: &str) -> bool {
            true
        }

        fn run_continuation_exec(
            &self,
            _runner_id: &str,
            _cwd: &str,
            _command: &[String],
            _run_id: &str,
        ) -> homeboy_core::Result<i32> {
            Err(Error::internal_unexpected(
                "exec not used in ownership race",
            ))
        }

        fn submit_runner_api_request(
            &self,
            _runner_id: &str,
            submission: homeboy_agents::agent_task_lifecycle::RunnerContinuationSubmission,
        ) -> homeboy_core::Result<Job> {
            match submission {
                homeboy_agents::agent_task_lifecycle::RunnerContinuationSubmission::RunnerApi(
                    request,
                ) => self.jobs.submit_runner_api_request(request),
                homeboy_agents::agent_task_lifecycle::RunnerContinuationSubmission::LegacyReplay(
                    request,
                ) => self.jobs.submit_remote_runner_job(request),
            }
        }

        fn lookup_reverse_broker_submission(
            &self,
            _runner_id: &str,
            submission_key: &str,
        ) -> homeboy_core::Result<RemoteRunnerSubmissionLookup> {
            Ok(self.jobs.lookup_remote_runner_submission(submission_key))
        }
    }

    fn execute(
        url: &str,
        submission: &RunnerApiSubmitRequest,
        store: Option<&homeboy_agents::agent_task_lifecycle::AgentTaskLifecycleStore>,
        run_id: Option<&str>,
    ) -> Result<serde_json::Value> {
        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .expect("HTTP client");
        let lease = submission.workspace_owner_lease.as_ref();
        submit_reverse_broker_job_with_admission(
            &client,
            url,
            "homeboy-lab",
            &submission.submission_key,
            submission,
            lease,
            None,
            store,
            run_id,
        )
    }

    fn prepare_pending_lifecycle(
        run_id: &str,
        submission: &RunnerApiSubmitRequest,
    ) -> homeboy_agents::agent_task_lifecycle::AgentTaskLifecycleStore {
        let store = homeboy_agents::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()
            .expect("lifecycle store");
        let dispatch = submission.envelope.dispatch.as_ref().expect("dispatch");
        let command = dispatch.command.clone();
        homeboy_agents::agent_task_lifecycle::record_lab_offload_planned_in_store(
            &store,
            homeboy_agents::agent_task_lifecycle::LabOffloadProxyPlan {
                run_id,
                runner_id: &dispatch.runner_id,
                remote_workspace: dispatch.cwd.as_deref().expect("cwd"),
                remote_command: &command,
                durable_plan: None,
            },
        )
        .expect("controller proxy");
        homeboy_agents::agent_task_lifecycle::record_lab_offload_submission_intent_in_store(
            &store,
            run_id,
            &dispatch.runner_id,
            dispatch.cwd.as_deref().expect("cwd"),
            &command,
            &[],
        )
        .expect("write-ahead submission intent");
        homeboy_agents::agent_task_lifecycle::record_lab_offload_submission_envelope(
            run_id, submission,
        )
        .expect("persist exact pending HTTP request");
        store
    }

    #[test]
    fn accepted_response_lost_resolves_identity_and_retains_owner_lease() {
        let lease = lease("accepted-response-lost");
        let accepted = job();
        let (url, requests, server) = start_broker(vec![
            Reply {
                path: "/runner/jobs",
                status: None,
                body: serde_json::Value::Null,
            },
            Reply {
                path: "/runner/jobs/submissions/lookup",
                status: Some(200),
                body: successful_reply(serde_json::json!({
                    "result": { "status": "accepted", "job": accepted },
                })),
            },
        ]);
        let submission = submission("accepted-response-lost", Some(lease.clone()));

        let data = execute(&url, &submission, None, None).expect("lookup recovers accepted job");
        let captured = requests.recv().expect("captured broker HTTP requests");
        server.join().expect("broker fixture thread");
        assert_eq!(data["job"]["id"], accepted.id.to_string());
        assert_eq!(captured.len(), 2, "accepted custody must never be released");
        assert_eq!(captured[0].0, "/runner/jobs");
        assert_eq!(captured[1].0, "/runner/jobs/submissions/lookup");
        assert_eq!(
            captured[0].1["workspace_owner_lease"],
            serde_json::to_value(&lease).unwrap()
        );
        assert_eq!(captured[1].1["submission_key"], submission.submission_key);
    }

    #[test]
    fn unavailable_lookup_preserves_ambiguous_owner_lease() {
        let lease = lease("lookup-unavailable");
        let (url, requests, server) = start_broker(vec![
            Reply {
                path: "/runner/jobs",
                status: Some(502),
                body: failed_reply("submit acknowledgement lost"),
            },
            Reply {
                path: "/runner/jobs/submissions/lookup",
                status: Some(503),
                body: failed_reply("lookup unavailable"),
            },
        ]);
        let submission = submission("lookup-unavailable", Some(lease.clone()));

        let error = execute(&url, &submission, None, None).expect_err("ambiguous submit retained");
        let captured = requests.recv().expect("captured broker HTTP requests");
        server.join().expect("broker fixture thread");
        assert_eq!(captured.len(), 2, "ambiguous lease is never released");
        assert_eq!(captured[0].0, "/runner/jobs");
        assert_eq!(captured[1].0, "/runner/jobs/submissions/lookup");
        assert_eq!(
            error.details["workspace_owner_lease_recovery"]["lease"],
            serde_json::to_value(&lease).unwrap()
        );
        assert!(error.details["workspace_owner_lease_recovery"]["lookup"]
            .as_str()
            .unwrap_or_default()
            .contains("lookup unavailable"));
    }

    #[test]
    fn proven_absence_attempts_owner_cleanup_and_retains_recovery_on_cleanup_failure() {
        let lease = lease("cleanup-failure");
        let (url, requests, server) = start_broker(vec![
            Reply {
                path: "/runner/jobs",
                status: Some(502),
                body: failed_reply("submit failed"),
            },
            Reply {
                path: "/runner/jobs/submissions/lookup",
                status: Some(200),
                body: successful_reply(serde_json::json!({ "result": { "status": "absent" } })),
            },
            Reply {
                path: "/runner/workspace-owners/release",
                status: Some(503),
                body: failed_reply("release unavailable"),
            },
        ]);
        let submission = submission("cleanup-failure", Some(lease.clone()));

        let error = execute(&url, &submission, None, None).expect_err("cleanup failure surfaced");
        let captured = requests.recv().expect("captured broker HTTP requests");
        server.join().expect("broker fixture thread");
        assert_eq!(
            captured
                .iter()
                .map(|request| request.0.as_str())
                .collect::<Vec<_>>(),
            vec![
                "/runner/jobs",
                "/runner/jobs/submissions/lookup",
                "/runner/workspace-owners/release",
            ]
        );
        assert_eq!(
            captured[2].1["workspace_owner_lease"],
            serde_json::to_value(&lease).unwrap()
        );
        assert_eq!(
            error.details["workspace_owner_lease_cleanup"]["lease"],
            serde_json::to_value(&lease).unwrap()
        );
        assert!(error.details["workspace_owner_lease_cleanup"]["error"]
            .as_str()
            .unwrap_or_default()
            .contains("release unavailable"));
    }

    #[test]
    fn cancellation_fence_prevents_broker_post_and_releases_only_provisional_owner() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let run_id = "http-fenced-before-post";
            let owner_lease = lease(run_id);
            let submission = submission(run_id, Some(owner_lease.clone()));
            let lifecycle_store = prepare_pending_lifecycle(run_id, &submission);
            let pending = homeboy_agents::agent_task_lifecycle::cancel_run_in_store(
                &lifecycle_store,
                run_id,
                Some("fence before broker POST"),
            )
            .expect("unresolved pending cancellation remains nonterminal");
            assert!(!pending.state.is_terminal());
            assert_eq!(
                pending.metadata["runner_submission_cancellation"]["state"],
                "requested"
            );

            let (url, requests, server) = start_broker(vec![Reply {
                path: "/runner/workspace-owners/release",
                status: Some(200),
                body: successful_reply(serde_json::json!({})),
            }]);
            let error = execute(&url, &submission, Some(&lifecycle_store), Some(run_id))
                .expect_err("fenced intent refuses submit");
            let captured = requests.recv().expect("captured release request");
            server.join().expect("broker fixture thread");
            assert_eq!(captured.len(), 1);
            assert_eq!(captured[0].0, "/runner/workspace-owners/release");
            assert_eq!(
                captured[0].1["workspace_owner_lease"],
                serde_json::to_value(&owner_lease).unwrap()
            );
            assert!(error.message.contains("fenced before POST"));
            let after = lifecycle_store
                .read_record(run_id)
                .expect("read fenced run");
            assert!(
                !after.state.is_terminal(),
                "an unavailable owner is not falsely terminalized"
            );
            assert!(after.metadata.get("run_cancelled").is_none());
        });
    }

    #[test]
    fn acceptance_cancel_race_keeps_http_owner_custody_and_fences_later_post() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let run_id = "http-acceptance-cancel-race";
            let owner_lease = lease(run_id);
            let submission = submission(run_id, Some(owner_lease.clone()));
            let lifecycle_store = prepare_pending_lifecycle(run_id, &submission);
            let jobs = homeboy_core::api_jobs::JobStore::default();
            let _continuation =
                homeboy_agents::agent_task_lifecycle::RunnerContinuationTestGuard::install(
                    Box::new(AcceptedJobContinuation { jobs: jobs.clone() }),
                );

            let listener = TcpListener::bind("127.0.0.1:0").expect("bind racing broker");
            let url = format!("http://{}", listener.local_addr().expect("broker addr"));
            let (post_seen_tx, post_seen_rx) = mpsc::channel();
            let (drop_post_ack_tx, drop_post_ack_rx) = mpsc::channel();
            let (requests_tx, requests_rx) = mpsc::channel();
            let server_jobs = jobs.clone();
            let server = std::thread::spawn(move || {
                let (mut post_stream, _) = listener.accept().expect("runner POST");
                let (path, body) = read_http_request(&mut post_stream);
                assert_eq!(path, "/runner/jobs");
                let request: RunnerApiSubmitRequest =
                    serde_json::from_value(body.clone()).expect("canonical submission request");
                let accepted = server_jobs
                    .submit_runner_api_request(request.clone())
                    .expect("broker stores accepted work before losing acknowledgement");
                post_seen_tx
                    .send(accepted.id)
                    .expect("notify accepted POST");
                drop_post_ack_rx
                    .recv_timeout(Duration::from_secs(5))
                    .expect("test releases lost acknowledgement");
                requests_tx
                    .send((path, body))
                    .expect("record accepted POST");
                drop(post_stream);

                let (mut lookup_stream, _) = listener.accept().expect("submission lookup");
                let (lookup_path, lookup_body) = read_http_request(&mut lookup_stream);
                assert_eq!(lookup_path, "/runner/jobs/submissions/lookup");
                assert_eq!(lookup_body["submission_key"], request.submission_key);
                let lookup = server_jobs.lookup_remote_runner_submission(&request.submission_key);
                let response = successful_reply(serde_json::json!({
                    "result": serde_json::to_value(lookup).expect("lookup wire value"),
                }))
                .to_string();
                write!(
                    lookup_stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    response.len(),
                    response
                )
                .expect("write authoritative accepted lookup");
                requests_tx
                    .send((lookup_path, lookup_body))
                    .expect("record accepted lookup");
            });

            let submit_store = lifecycle_store.clone();
            let submit_request = submission.clone();
            let submit_url = url.clone();
            let submit = std::thread::spawn(move || {
                execute(
                    &submit_url,
                    &submit_request,
                    Some(&submit_store),
                    Some(run_id),
                )
            });
            let accepted_job_id = post_seen_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("HTTP owner accepted POST while admission lock is held");

            let (cancel_started_tx, cancel_started_rx) = mpsc::channel();
            let cancel_store = lifecycle_store.clone();
            let cancel = std::thread::spawn(move || {
                cancel_started_tx.send(()).expect("announce cancellation");
                homeboy_agents::agent_task_lifecycle::cancel_run_in_store(
                    &cancel_store,
                    run_id,
                    Some("cancel while broker acknowledgement is lost"),
                )
            });
            cancel_started_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("cancellation started");
            std::thread::sleep(Duration::from_millis(150));
            assert!(
                !cancel.is_finished(),
                "cancellation must wait for the accepted HTTP submission's owner lock"
            );
            drop_post_ack_tx
                .send(())
                .expect("drop the submit acknowledgement");

            let submit_result = submit
                .join()
                .expect("submit thread joins")
                .expect("accepted identity is recovered by broker lookup");
            assert_eq!(submit_result["job"]["id"], accepted_job_id.to_string());
            let cancel_result = cancel.join().expect("cancel thread joins");
            server.join().expect("broker thread joins");
            let mut observed = vec![requests_rx.recv().unwrap(), requests_rx.recv().unwrap()];
            observed.sort_by(|left, right| left.0.cmp(&right.0));
            assert_eq!(observed[0].0, "/runner/jobs");
            assert_eq!(observed[1].0, "/runner/jobs/submissions/lookup");
            assert_eq!(
                observed[0].1["workspace_owner_lease"],
                serde_json::to_value(&owner_lease).unwrap()
            );
            assert_eq!(
                jobs.get(accepted_job_id).unwrap().status,
                homeboy_core::api_jobs::JobStatus::Queued
            );

            let after_race = lifecycle_store.read_record(run_id).expect("race record");
            assert_eq!(
                after_race.runner_job_id(),
                Some(accepted_job_id.to_string().as_str())
            );
            assert!(
                !after_race.state.is_terminal(),
                "unconfirmed runner cancellation cannot terminalize"
            );
            assert!(
                cancel_result.is_err(),
                "missing runner cancel authority remains an explicit failure"
            );
            assert!(lifecycle_store
                .open_observation_readonly()
                .unwrap()
                .control_plane_event_stream(
                    &homeboy_control_plane_contract::RunId::new(run_id).unwrap()
                )
                .unwrap()
                .unwrap()
                .iter()
                .all(|event| event.kind != "run.cancelled"));

            let (later_url, later_requests, later_server) = start_broker(vec![Reply {
                path: "/runner/workspace-owners/release",
                status: Some(200),
                body: successful_reply(serde_json::json!({})),
            }]);
            let later = execute(
                &later_url,
                &submission,
                Some(&lifecycle_store),
                Some(run_id),
            )
            .expect_err("cancellation fence denies every later HTTP POST");
            let captured = later_requests.recv().expect("provisional lease release");
            later_server.join().expect("later broker fixture");
            assert_eq!(captured.len(), 1);
            assert_eq!(captured[0].0, "/runner/workspace-owners/release");
            assert!(later.message.contains("fenced before POST"));
        });
    }
}
