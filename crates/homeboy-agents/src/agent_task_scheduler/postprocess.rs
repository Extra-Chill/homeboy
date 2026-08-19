//! Plan-native execution of generic artifact postprocess actions.

use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use homeboy_engine_primitives::content_hash;
use serde::{Deserialize, Serialize};

use super::*;

pub(super) fn run_postprocess_steps(
    plan: &AgentTaskPlan,
    run_id: Option<&str>,
    outcomes: &mut Vec<AgentTaskOutcome>,
    events: &mut Vec<AgentTaskProgressEvent>,
    cancellation: &AgentTaskCancellationToken,
) {
    for step in &plan.postprocess_steps {
        let checkpoint = postprocess_checkpoint_path(run_id, &step.id);
        let identity = checkpoint_identity(plan, run_id, step, outcomes);
        let outcome =
            if let Some(outcome) = read_checkpoint(&checkpoint, run_id, &step.id, &identity) {
                outcome
            } else if cancellation.is_cancelled() {
                postprocess_outcome(
                    step,
                    AgentTaskOutcomeStatus::Cancelled,
                    "cancelled before execution",
                    None,
                )
            } else if let Some(message) = failed_dependency_message(step, outcomes) {
                postprocess_failure_outcome(step, &message)
            } else {
                match start_or_reconcile_worker(plan, step, run_id, outcomes, &identity) {
                    Ok(outcome) => outcome,
                    Err(message) => postprocess_failure_outcome(step, &message),
                }
            };
        if outcome.status != AgentTaskOutcomeStatus::Cancelled
            && !outcome.metadata["checkpoint_write_failed"]
                .as_bool()
                .unwrap_or(false)
            && !checkpoint.is_file()
        {
            if let Err(error) =
                write_checkpoint(&checkpoint, run_id, &identity, outcomes, step, &outcome)
            {
                let outcome = checkpoint_failure_outcome(step, error.message);
                events.push(event(
                    &step.id,
                    AgentTaskScheduleSupport::state_for_outcome(&outcome),
                    1,
                    outcome.summary.clone(),
                ));
                outcomes.push(outcome);
                continue;
            }
        }
        events.push(event(
            &step.id,
            AgentTaskScheduleSupport::state_for_outcome(&outcome),
            1,
            outcome.summary.clone(),
        ));
        outcomes.push(outcome);
    }
}

/// Runs a persisted request from the internal Homeboy worker command. The worker,
/// rather than its scheduler parent, owns the side-effecting helper and all durable
/// completion records. A controller crash therefore cannot repeat a completed helper.
pub fn run_postprocess_worker(request_path: &Path) -> homeboy_core::Result<()> {
    let request: PostprocessWorkerRequest =
        serde_json::from_slice(&std::fs::read(request_path).map_err(|error| {
            homeboy_core::Error::internal_io(
                error.to_string(),
                Some(request_path.display().to_string()),
            )
        })?)
        .map_err(|error| {
            homeboy_core::Error::internal_json(
                error.to_string(),
                Some(request_path.display().to_string()),
            )
        })?;
    if request.schema != WORKER_REQUEST_SCHEMA {
        return Err(homeboy_core::Error::validation_invalid_argument(
            "postprocess_worker.schema",
            "unsupported postprocess worker request",
            Some(request.schema),
            None,
        ));
    }
    let checkpoint = postprocess_checkpoint_path(Some(&request.run_id), &request.step.id);
    if read_checkpoint(
        &checkpoint,
        Some(&request.run_id),
        &request.step.id,
        &request.fingerprint,
    )
    .is_some()
    {
        return Ok(());
    }
    if request.claim_delay_millis > 0 {
        thread::sleep(Duration::from_millis(request.claim_delay_millis));
    }
    let claim = acquire_claim(&postprocess_claim_path(
        Some(&request.run_id),
        &request.step.id,
    ))
    .map_err(homeboy_core::Error::internal_unexpected)?;
    // Another worker may have completed while this worker waited for ownership.
    let outcome = read_checkpoint(
        &checkpoint,
        Some(&request.run_id),
        &request.step.id,
        &request.fingerprint,
    )
    .or_else(|| {
        recover_completed_attempt(&request.step, Some(&request.run_id), &request.fingerprint).ok()
    })
    .unwrap_or_else(|| {
        execute_postprocess_step(
            &request.step,
            Some(&request.run_id),
            &request.dependencies,
            &request.fingerprint,
        )
    });
    write_checkpoint(
        &checkpoint,
        Some(&request.run_id),
        &request.fingerprint,
        &request.dependencies,
        &request.step,
        &outcome,
    )?;
    drop(claim);
    Ok(())
}

fn start_or_reconcile_worker(
    plan: &AgentTaskPlan,
    step: &AgentTaskArtifactPostprocessStep,
    run_id: Option<&str>,
    outcomes: &[AgentTaskOutcome],
    fingerprint: &str,
) -> std::result::Result<AgentTaskOutcome, String> {
    start_or_reconcile_worker_with_retry(plan, step, run_id, outcomes, fingerprint, true)
}

fn start_or_reconcile_worker_with_retry(
    plan: &AgentTaskPlan,
    step: &AgentTaskArtifactPostprocessStep,
    run_id: Option<&str>,
    outcomes: &[AgentTaskOutcome],
    fingerprint: &str,
    allow_startup_recovery: bool,
) -> std::result::Result<AgentTaskOutcome, String> {
    #[cfg(test)]
    let _ = allow_startup_recovery;
    let run_id = run_id.unwrap_or("unrecorded-run");
    let root = postprocess_root(Some(run_id), &step.id);
    std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    if let Some(outcome) = reconcile_existing_worker(step, run_id, fingerprint)? {
        return Ok(outcome);
    }
    let request_path = root.join("request.json");
    let request = PostprocessWorkerRequest {
        schema: WORKER_REQUEST_SCHEMA.to_string(),
        run_id: run_id.to_string(),
        step: step.clone(),
        fingerprint: fingerprint.to_string(),
        dependencies: outcomes.to_vec(),
        plan_id: plan.plan_id.clone(),
        claim_delay_millis: std::env::var("HOMEBOY_POSTPROCESS_WORKER_CLAIM_DELAY_MS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or_default(),
    };
    write_json_atomically(&request_path, &request).map_err(|error| error.message)?;
    #[cfg(test)]
    {
        run_postprocess_worker(&request_path)
            .map_err(|error| error.message)
            .and_then(|_| {
                read_checkpoint(
                    &postprocess_checkpoint_path(Some(run_id), &step.id),
                    Some(run_id),
                    &step.id,
                    fingerprint,
                )
                .ok_or_else(|| "artifact postprocess worker did not write checkpoint".to_string())
            })
    }
    #[cfg(not(test))]
    let worker = postprocess_worker_executable();
    #[cfg(not(test))]
    let child = Command::new(worker)
        .args(["self", "postprocess-worker", "--request"])
        .arg(&request_path)
        .spawn()
        .map_err(|error| error.to_string())?;
    #[cfg(not(test))]
    let spawned = PostprocessWorkerSpawn {
        schema: WORKER_SPAWN_SCHEMA.to_string(),
        worker_id: uuid::Uuid::new_v4().to_string(),
        pid: child.id(),
        spawned_unix_secs: now_unix_secs(),
        start_identity: homeboy_core::process::process_start_identity(child.id())
            .map_err(|error| error.to_string())?
            .ok_or_else(|| {
                "artifact postprocess worker exited before startup identity capture".to_string()
            })?,
    };
    #[cfg(not(test))]
    write_json_atomically(&postprocess_worker_path(Some(run_id), &step.id), &spawned)
        .map_err(|error| error.message)?;
    #[cfg(not(test))]
    let checkpoint = postprocess_checkpoint_path(Some(run_id), &step.id);
    #[cfg(not(test))]
    for _ in 0..3000 {
        if let Some(outcome) = read_checkpoint(&checkpoint, Some(run_id), &step.id, fingerprint) {
            return Ok(outcome);
        }
        if !worker_is_alive(&spawned) {
            // A worker that dies before claiming has not run a helper. Retry one
            // fresh process rather than checkpointing a failure that would block
            // safe recovery after a startup race.
            if allow_startup_recovery {
                let _ = std::fs::remove_file(postprocess_worker_path(Some(run_id), &step.id));
                return start_or_reconcile_worker_with_retry(
                    plan,
                    step,
                    Some(run_id),
                    outcomes,
                    fingerprint,
                    false,
                );
            }
            return Err("artifact postprocess worker died before completion".to_string());
        }
        thread::sleep(Duration::from_millis(100));
    }
    #[cfg(not(test))]
    Err("artifact postprocess worker did not complete before scheduler wait limit".to_string())
}

fn postprocess_worker_executable() -> PathBuf {
    let configured = std::env::var_os("HOMEBOY_POSTPROCESS_WORKER").map(PathBuf::from);
    // Archive-replayed integration tests execute from a rooted helper copy while
    // their compile-time override can still name the discarded build output.
    // Prefer the configured executable when it exists; otherwise use the live
    // test helper path inherited by the child.
    if let Some(worker) = configured.as_ref().filter(|worker| worker.is_file()) {
        return worker.clone();
    }
    if let Some(worker) = std::env::var_os("CARGO_BIN_EXE_homeboy")
        .map(PathBuf::from)
        .filter(|worker| worker.is_file())
    {
        return worker;
    }
    configured.unwrap_or_else(|| std::env::current_exe().expect("current Homeboy executable"))
}

/// A restarted scheduler must adopt the durable worker it finds. The worker owns
/// the helper claim, so starting another process before proving that owner dead
/// would turn a scheduler restart into a competing execution attempt.
fn reconcile_existing_worker(
    step: &AgentTaskArtifactPostprocessStep,
    run_id: &str,
    fingerprint: &str,
) -> std::result::Result<Option<AgentTaskOutcome>, String> {
    let checkpoint = postprocess_checkpoint_path(Some(run_id), &step.id);
    if let Some(outcome) = read_checkpoint(&checkpoint, Some(run_id), &step.id, fingerprint) {
        return Ok(Some(outcome));
    }
    let worker_path = postprocess_worker_path(Some(run_id), &step.id);
    let worker = std::fs::read(&worker_path)
        .ok()
        .and_then(|raw| serde_json::from_slice::<PostprocessWorkerSpawn>(&raw).ok())
        .filter(|worker| worker.schema == WORKER_SPAWN_SCHEMA);
    if let Some(worker) = worker {
        if worker_is_alive(&worker) {
            if let Some(outcome) = wait_for_postprocess_completion(
                &checkpoint,
                Some(&worker),
                Some(run_id),
                step,
                fingerprint,
            )? {
                return Ok(Some(outcome));
            }
        }
        if let Ok(outcome) = recover_completed_attempt(step, Some(run_id), fingerprint) {
            return Ok(Some(outcome));
        }
        let _ = std::fs::remove_file(&worker_path);
    } else if let Ok(outcome) = recover_completed_attempt(step, Some(run_id), fingerprint) {
        return Ok(Some(outcome));
    }

    let claim = postprocess_claim_path(Some(run_id), &step.id);
    if claim.exists() {
        if claim_is_recoverable(&claim) {
            let _ = std::fs::remove_file(&claim);
        } else if let Some(outcome) =
            wait_for_postprocess_completion(&checkpoint, None, Some(run_id), step, fingerprint)?
        {
            return Ok(Some(outcome));
        }
    }
    Ok(None)
}

fn wait_for_postprocess_completion(
    checkpoint: &Path,
    worker: Option<&PostprocessWorkerSpawn>,
    run_id: Option<&str>,
    step: &AgentTaskArtifactPostprocessStep,
    fingerprint: &str,
) -> std::result::Result<Option<AgentTaskOutcome>, String> {
    let claim = postprocess_claim_path(run_id, &step.id);
    for _ in 0..3000 {
        if let Some(outcome) = read_checkpoint(checkpoint, run_id, &step.id, fingerprint) {
            return Ok(Some(outcome));
        }
        if worker.is_some_and(|worker| !worker_is_alive(worker))
            || worker.is_none() && (!claim.exists() || claim_is_recoverable(&claim))
        {
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err("artifact postprocess worker did not complete before scheduler wait limit".to_string())
}

fn failed_dependency_message(
    step: &AgentTaskArtifactPostprocessStep,
    outcomes: &[AgentTaskOutcome],
) -> Option<String> {
    step.depends_on.iter().find_map(|dependency| {
        outcomes
            .iter()
            .find(|outcome| outcome.task_id == *dependency)
            .filter(|outcome| {
                !matches!(
                    outcome.status,
                    AgentTaskOutcomeStatus::Succeeded | AgentTaskOutcomeStatus::NoOp
                )
            })
            .map(|_| {
                format!(
                    "artifact postprocess '{}' blocked by unsuccessful dependency '{}'",
                    step.id, dependency
                )
            })
            .or_else(|| {
                (!outcomes
                    .iter()
                    .any(|outcome| outcome.task_id == *dependency))
                .then(|| {
                    format!(
                        "artifact postprocess '{}' waited for missing dependency '{}'",
                        step.id, dependency
                    )
                })
            })
    })
}

fn execute_postprocess_step(
    step: &AgentTaskArtifactPostprocessStep,
    run_id: Option<&str>,
    outcomes: &[AgentTaskOutcome],
    fingerprint: &str,
) -> AgentTaskOutcome {
    let root = postprocess_root(run_id, &step.id);
    let attempt = root.join("staging").join(uuid::Uuid::new_v4().to_string());
    let input = attempt.join("input");
    let output = attempt.join("output");
    let result = (|| {
        std::fs::create_dir_all(&output).map_err(|error| {
            homeboy_core::Error::internal_io(error.to_string(), Some(output.display().to_string()))
        })?;
        materialize_dependency_artifacts(&input, &step.depends_on, outcomes)?;
        let mut postprocess_plan = step.plan.clone();
        if let Some(root) = postprocess_plan.artifact_roots.first_mut() {
            root.path = output.display().to_string();
            root.persisted_ref = Some(format!("file://{}", output.display()));
        }
        homeboy_core::artifact_postprocess::run_artifact_postprocess_plan(
            &postprocess_plan,
            &homeboy_core::artifacts::ArtifactPostprocessContext {
                artifact_root: &output,
                input_root: Some(&input),
                path_expander: None,
            },
        )
    })();
    let outcome = match result {
        Ok(result) => {
            let failed = !result.success;
            let artifacts = result.outputs.iter().flat_map(|output| output.artifacts.iter()).filter_map(|artifact| {
                let path = PathBuf::from(&artifact.path);
                let bytes = std::fs::read(&path).ok()?;
                Some(AgentTaskArtifact {
                    id: artifact.id.clone().unwrap_or_else(|| format!("{}:{}", step.id, artifact.path)),
                    kind: artifact.kind.clone(),
                    name: path.file_name().map(|name| name.to_string_lossy().to_string()),
                    path: Some(path.display().to_string()),
                    size_bytes: Some(bytes.len() as u64),
                    sha256: Some(content_hash::sha256_hex(&bytes)),
                    metadata: serde_json::json!({ "source": "homeboy.artifact-postprocess", "postprocess_step": step.id, "artifact_metadata": artifact.metadata }),
                    ..Default::default()
                })
            }).collect();
            let evidence_refs = result
                .reviewer_refs
                .iter()
                .map(|reference| AgentTaskEvidenceRef {
                    kind: reference.kind.clone(),
                    uri: reference.url.clone(),
                    label: Some(reference.label.clone()),
                })
                .collect();
            let status = if failed && step.required {
                AgentTaskOutcomeStatus::Failed
            } else {
                AgentTaskOutcomeStatus::Succeeded
            };
            let summary = if failed {
                "artifact postprocess completed with failed optional actions"
            } else {
                "artifact postprocess completed"
            };
            AgentTaskOutcome {
                task_id: step.id.clone(),
                status,
                summary: Some(summary.to_string()),
                artifacts,
                evidence_refs,
                failure_classification: (failed && step.required)
                    .then_some(AgentTaskFailureClassification::ExecutionFailed),
                diagnostics: result
                    .outputs
                    .iter()
                    .filter(|output| !output.success)
                    .map(|output| AgentTaskDiagnostic {
                        class: "artifact_postprocess".to_string(),
                        message: output
                            .error
                            .clone()
                            .unwrap_or_else(|| "artifact postprocess helper failed".to_string()),
                        data: serde_json::to_value(output).unwrap_or(serde_json::Value::Null),
                    })
                    .collect(),
                outputs: serde_json::json!({ "artifact_postprocess": result }),
                metadata: serde_json::json!({ "step_kind": "artifact_postprocess", "required": step.required, "optional_failure": failed && !step.required, "root": root.join("output") }),
                ..Default::default()
            }
        }
        Err(error) => postprocess_failure_outcome(step, &error.message),
    };
    // Each attempt receives a permanent version directory. Consumers resolve the
    // small `current.json` pointer; no output directory is ever replaced.
    let version = root
        .join("versions")
        .join(attempt.file_name().expect("attempt id"));
    let outcome = rebase_outcome_paths(outcome, &output, &version);
    let completion = PostprocessCompletion {
        schema: COMPLETION_SCHEMA.to_string(),
        run_id: run_id.unwrap_or("unrecorded-run").to_string(),
        step_id: step.id.clone(),
        fingerprint: fingerprint.to_string(),
        exit_codes: completion_exit_codes(&outcome),
        output_digest: output_digest(&output).unwrap_or_default(),
        outcome: outcome.clone(),
    };
    if let Err(error) = write_completion(&attempt.join("completion.json"), &completion) {
        return checkpoint_failure_outcome(step, error.message);
    }
    if let Err(error) = promote_attempt(&root, &attempt, &completion) {
        return checkpoint_failure_outcome(step, error.message);
    }
    outcome
}

fn postprocess_root(run_id: Option<&str>, step_id: &str) -> PathBuf {
    homeboy_core::artifacts::root()
        .unwrap_or_else(|_| PathBuf::from(".homeboy-artifacts"))
        .join("agent-task")
        .join("postprocess")
        .join(homeboy_core::paths::sanitize_path_segment(
            run_id.unwrap_or("unrecorded-run"),
        ))
        .join(homeboy_core::paths::sanitize_path_segment(step_id))
}

fn postprocess_checkpoint_path(run_id: Option<&str>, step_id: &str) -> PathBuf {
    postprocess_root(run_id, step_id).join("checkpoint.json")
}

fn postprocess_claim_path(run_id: Option<&str>, step_id: &str) -> PathBuf {
    postprocess_root(run_id, step_id).join("claim.json")
}

fn postprocess_worker_path(run_id: Option<&str>, step_id: &str) -> PathBuf {
    postprocess_root(run_id, step_id).join("worker.json")
}

fn recover_completed_attempt(
    step: &AgentTaskArtifactPostprocessStep,
    run_id: Option<&str>,
    fingerprint: &str,
) -> homeboy_core::Result<AgentTaskOutcome> {
    let root = postprocess_root(run_id, &step.id);
    let staging = root.join("staging");
    let entries = match std::fs::read_dir(&staging) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(homeboy_core::Error::internal_unexpected(
                "completed postprocess stage is absent",
            ))
        }
        Err(error) => {
            return Err(homeboy_core::Error::internal_io(
                error.to_string(),
                Some(staging.display().to_string()),
            ))
        }
    };
    for entry in entries.flatten() {
        let attempt = entry.path();
        let completion_path = attempt.join("completion.json");
        let completion: PostprocessCompletion = match std::fs::read(&completion_path)
            .ok()
            .and_then(|raw| serde_json::from_slice::<PostprocessCompletion>(&raw).ok())
        {
            Some(completion)
                if completion.schema == COMPLETION_SCHEMA
                    && completion.run_id == run_id.unwrap_or("unrecorded-run")
                    && completion.step_id == step.id
                    && completion.fingerprint == fingerprint =>
            {
                completion
            }
            _ => {
                let _ = std::fs::remove_dir_all(&attempt);
                continue;
            }
        };
        let staged_output = attempt.join("output");
        let promoted_output = promoted_output_path(&root, &completion.output_digest);
        let staged_matches = output_digest(&staged_output)
            .map(|digest| digest == completion.output_digest)
            .unwrap_or(false);
        let promoted_matches = output_digest(&promoted_output)
            .map(|digest| digest == completion.output_digest)
            .unwrap_or(false);
        if staged_matches {
            promote_attempt(&root, &attempt, &completion)?;
            discard_other_staged_attempts(&staging, &attempt);
            return Ok(completion.outcome);
        }
        if promoted_matches {
            discard_other_staged_attempts(&staging, &attempt);
            return Ok(completion.outcome);
        }
        let _ = std::fs::remove_dir_all(&attempt);
    }
    Err(homeboy_core::Error::internal_unexpected(
        "valid completed postprocess stage is absent",
    ))
}

fn discard_other_staged_attempts(staging: &Path, completed_attempt: &Path) {
    if let Ok(entries) = std::fs::read_dir(staging) {
        for entry in entries.flatten() {
            let attempt = entry.path();
            if attempt != completed_attempt {
                let _ = std::fs::remove_dir_all(attempt);
            }
        }
    }
}

fn write_completion(path: &Path, completion: &PostprocessCompletion) -> homeboy_core::Result<()> {
    let bytes = serde_json::to_vec_pretty(completion).map_err(|error| {
        homeboy_core::Error::internal_json(error.to_string(), Some(path.display().to_string()))
    })?;
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    std::fs::write(&temporary, bytes).map_err(|error| {
        homeboy_core::Error::internal_io(error.to_string(), Some(temporary.display().to_string()))
    })?;
    std::fs::rename(&temporary, path).map_err(|error| {
        homeboy_core::Error::internal_io(error.to_string(), Some(path.display().to_string()))
    })
}

fn promote_attempt(
    root: &Path,
    attempt: &Path,
    completion: &PostprocessCompletion,
) -> homeboy_core::Result<()> {
    let staged = attempt.join("output");
    if output_digest(&staged)? != completion.output_digest {
        return Err(homeboy_core::Error::internal_unexpected(
            "postprocess staging output digest changed before promotion",
        ));
    }
    std::fs::create_dir_all(root).map_err(|error| {
        homeboy_core::Error::internal_io(error.to_string(), Some(root.display().to_string()))
    })?;
    let version = root
        .join("versions")
        .join(attempt.file_name().expect("attempt id"));
    if output_digest(&version).ok().as_deref() == Some(&completion.output_digest) {
        write_current_pointer(root, &version, completion)?;
        return Ok(());
    }
    if version.exists() {
        return Err(homeboy_core::Error::internal_unexpected(
            "postprocess artifact version already exists with a different digest",
        ));
    }
    if let Some(parent) = version.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            homeboy_core::Error::internal_io(error.to_string(), Some(parent.display().to_string()))
        })?;
    }
    std::fs::rename(&staged, &version).map_err(|error| {
        homeboy_core::Error::internal_io(error.to_string(), Some(version.display().to_string()))
    })?;
    write_current_pointer(root, &version, completion)?;
    Ok(())
}

fn promoted_output_path(root: &Path, digest: &str) -> PathBuf {
    let pointer = root.join("current.json");
    serde_json::from_slice::<PostprocessArtifactPointer>(
        &std::fs::read(pointer).unwrap_or_default(),
    )
    .ok()
    .filter(|value| value.output_digest == digest)
    .map(|value| root.join(value.version))
    .unwrap_or_else(|| root.join("missing-version"))
}

fn write_current_pointer(
    root: &Path,
    version: &Path,
    completion: &PostprocessCompletion,
) -> homeboy_core::Result<()> {
    let relative = version
        .strip_prefix(root)
        .unwrap_or(version)
        .to_string_lossy()
        .to_string();
    write_json_atomically(
        &root.join("current.json"),
        &PostprocessArtifactPointer {
            schema: "homeboy/agent-task-postprocess-artifact-pointer/v1".to_string(),
            version: relative,
            output_digest: completion.output_digest.clone(),
            fingerprint: completion.fingerprint.clone(),
        },
    )
}

fn output_digest(path: &Path) -> homeboy_core::Result<String> {
    if !path.is_dir() {
        return Err(homeboy_core::Error::internal_unexpected(format!(
            "postprocess output {} is absent",
            path.display()
        )));
    }
    let mut files = Vec::new();
    collect_output_files(path, path, &mut files)?;
    files.sort();
    Ok(content_hash::sha256_hex(files.join("\n").as_bytes()))
}

fn collect_output_files(
    root: &Path,
    path: &Path,
    files: &mut Vec<String>,
) -> homeboy_core::Result<()> {
    for entry in std::fs::read_dir(path)
        .map_err(|error| {
            homeboy_core::Error::internal_io(error.to_string(), Some(path.display().to_string()))
        })?
        .flatten()
    {
        let entry_path = entry.path();
        if entry_path.is_dir() {
            collect_output_files(root, &entry_path, files)?;
        } else if entry_path.is_file() {
            let relative = entry_path.strip_prefix(root).unwrap_or(&entry_path);
            let bytes = std::fs::read(&entry_path).map_err(|error| {
                homeboy_core::Error::internal_io(
                    error.to_string(),
                    Some(entry_path.display().to_string()),
                )
            })?;
            files.push(format!(
                "{}:{}",
                relative.display(),
                content_hash::sha256_hex(&bytes)
            ));
        }
    }
    Ok(())
}

fn completion_exit_codes(outcome: &AgentTaskOutcome) -> Vec<Option<i32>> {
    outcome.outputs["artifact_postprocess"]["outputs"]
        .as_array()
        .map(|outputs| {
            outputs
                .iter()
                .map(|output| output["exit_code"].as_i64().map(|code| code as i32))
                .collect()
        })
        .unwrap_or_default()
}

fn rebase_outcome_paths(outcome: AgentTaskOutcome, from: &Path, to: &Path) -> AgentTaskOutcome {
    let mut source_roots = vec![from.to_path_buf()];
    if let Ok(canonical) = from.canonicalize() {
        if canonical != from {
            source_roots.push(canonical);
        }
    }
    let Ok(mut value) = serde_json::to_value(&outcome) else {
        return outcome;
    };
    rebase_json_paths(&mut value, &source_roots, to);
    serde_json::from_value(value).unwrap_or(outcome)
}

fn rebase_path(path: &str, source_roots: &[PathBuf], to: &Path) -> Option<String> {
    let path = Path::new(path);
    path.is_absolute()
        .then_some(())
        .and_then(|_| {
            source_roots.iter().find_map(|source| {
                path.strip_prefix(source)
                    .ok()
                    .filter(|relative| is_safe_rebase_relative(relative))
            })
        })
        .map(|relative| {
            if relative.as_os_str().is_empty() {
                to.to_path_buf()
            } else {
                to.join(relative)
            }
            .display()
            .to_string()
        })
}

fn is_safe_rebase_relative(relative: &Path) -> bool {
    !relative.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    })
}

fn rebase_json_paths(value: &mut serde_json::Value, source_roots: &[PathBuf], to: &Path) {
    match value {
        serde_json::Value::String(path) => {
            if let Some(rebased) = rebase_path(path, source_roots, to) {
                *path = rebased;
            }
        }
        serde_json::Value::Array(values) => values
            .iter_mut()
            .for_each(|value| rebase_json_paths(value, source_roots, to)),
        serde_json::Value::Object(values) => values
            .values_mut()
            .for_each(|value| rebase_json_paths(value, source_roots, to)),
        _ => {}
    }
}

fn claim_mutation_path(path: &Path) -> PathBuf {
    path.with_extension("mutation")
}

fn materialize_dependency_artifacts(
    input: &Path,
    dependencies: &[String],
    outcomes: &[AgentTaskOutcome],
) -> homeboy_core::Result<()> {
    for dependency in dependencies {
        let Some(outcome) = outcomes
            .iter()
            .find(|outcome| outcome.task_id == *dependency)
        else {
            continue;
        };
        for artifact in &outcome.artifacts {
            let Some(path) = artifact.path.as_deref().map(PathBuf::from) else {
                continue;
            };
            if !path.is_file() {
                continue;
            }
            let destination = input
                .join(homeboy_core::paths::sanitize_path_segment(dependency))
                .join(materialized_artifact_name(artifact)?);
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent).map_err(|error| {
                    homeboy_core::Error::internal_io(
                        error.to_string(),
                        Some(parent.display().to_string()),
                    )
                })?;
            }
            std::fs::copy(&path, &destination).map_err(|error| {
                homeboy_core::Error::internal_io(
                    error.to_string(),
                    Some(destination.display().to_string()),
                )
            })?;
        }
    }
    Ok(())
}

fn materialized_artifact_name(artifact: &AgentTaskArtifact) -> homeboy_core::Result<String> {
    let candidate = artifact
        .name
        .as_deref()
        .or_else(|| {
            Path::new(&artifact.id)
                .file_name()
                .and_then(|name| name.to_str())
        })
        .unwrap_or(&artifact.id);
    let path = Path::new(candidate);
    if matches!(candidate, "." | "..") {
        return Err(homeboy_core::Error::validation_invalid_argument(
            "artifact.filename",
            "artifact filename cannot be `.` or `..`",
            Some(candidate.to_string()),
            None,
        ));
    }
    if !candidate.is_empty()
        && !matches!(
            path.components().next(),
            Some(Component::CurDir | Component::ParentDir)
        )
        && !path.is_absolute()
        && path.components().count() == 1
        && !candidate.contains(['/', '\\'])
    {
        return Ok(candidate.to_string());
    }
    Ok(homeboy_core::paths::sanitize_path_segment(&artifact.id))
}

fn postprocess_outcome(
    step: &AgentTaskArtifactPostprocessStep,
    status: AgentTaskOutcomeStatus,
    message: &str,
    detail: Option<String>,
) -> AgentTaskOutcome {
    AgentTaskOutcome {
        task_id: step.id.clone(),
        status,
        summary: Some(message.to_string()),
        failure_classification: (status == AgentTaskOutcomeStatus::Failed)
            .then_some(AgentTaskFailureClassification::ExecutionFailed),
        diagnostics: detail
            .into_iter()
            .map(|message| AgentTaskDiagnostic {
                class: "artifact_postprocess".to_string(),
                message,
                data: serde_json::Value::Null,
            })
            .collect(),
        metadata: serde_json::json!({ "step_kind": "artifact_postprocess", "required": step.required }),
        ..Default::default()
    }
}

fn postprocess_failure_outcome(
    step: &AgentTaskArtifactPostprocessStep,
    message: &str,
) -> AgentTaskOutcome {
    let status = if step.required {
        AgentTaskOutcomeStatus::Failed
    } else {
        AgentTaskOutcomeStatus::Succeeded
    };
    let mut outcome = postprocess_outcome(step, status, message, Some(message.to_string()));
    outcome.metadata["optional_failure"] = serde_json::json!(!step.required);
    outcome
}

#[derive(Serialize, Deserialize)]
struct PostprocessCheckpoint {
    schema: String,
    run_id: String,
    step_id: String,
    fingerprint: String,
    dependencies: Vec<AgentTaskOutcome>,
    outcome: AgentTaskOutcome,
}

#[derive(Serialize, Deserialize)]
struct PostprocessClaim {
    schema: String,
    owner_id: String,
    owner_pid: u32,
    heartbeat_unix_secs: u64,
}

#[derive(Serialize, Deserialize)]
struct PostprocessCompletion {
    schema: String,
    run_id: String,
    step_id: String,
    fingerprint: String,
    exit_codes: Vec<Option<i32>>,
    output_digest: String,
    outcome: AgentTaskOutcome,
}

#[derive(Serialize, Deserialize)]
struct PostprocessArtifactPointer {
    schema: String,
    version: String,
    output_digest: String,
    fingerprint: String,
}

#[derive(Serialize, Deserialize)]
struct PostprocessWorkerRequest {
    schema: String,
    run_id: String,
    plan_id: String,
    step: AgentTaskArtifactPostprocessStep,
    fingerprint: String,
    dependencies: Vec<AgentTaskOutcome>,
    #[serde(default)]
    claim_delay_millis: u64,
}

#[derive(Serialize, Deserialize)]
struct PostprocessWorkerSpawn {
    schema: String,
    worker_id: String,
    pid: u32,
    spawned_unix_secs: u64,
    start_identity: homeboy_core::process::ProcessStartIdentity,
}

const CHECKPOINT_SCHEMA: &str = "homeboy/agent-task-postprocess-checkpoint/v2";
const CLAIM_SCHEMA: &str = "homeboy/agent-task-postprocess-claim/v2";
const COMPLETION_SCHEMA: &str = "homeboy/agent-task-postprocess-completion/v1";
const WORKER_REQUEST_SCHEMA: &str = "homeboy/agent-task-postprocess-worker-request/v1";
const WORKER_SPAWN_SCHEMA: &str = "homeboy/agent-task-postprocess-worker-spawn/v1";
const CLAIM_STALE_AFTER_SECS: u64 = 300;
const CLAIM_HEARTBEAT_INTERVAL_SECS: u64 = 1;

struct PostprocessClaimGuard {
    path: PathBuf,
    owner_id: String,
    stop: Arc<AtomicBool>,
    heartbeat: Option<thread::JoinHandle<()>>,
}

struct ClaimMutationGuard(PathBuf);

impl Drop for ClaimMutationGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

impl Drop for PostprocessClaimGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(heartbeat) = self.heartbeat.take() {
            let _ = heartbeat.join();
        }
        if claim_owned_by(&self.path, &self.owner_id) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn acquire_claim(path: &Path) -> std::result::Result<PostprocessClaimGuard, String> {
    let parent = path.parent().expect("claim has parent");
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let claim = PostprocessClaim {
        schema: CLAIM_SCHEMA.to_string(),
        owner_id: uuid::Uuid::new_v4().to_string(),
        owner_pid: std::process::id(),
        heartbeat_unix_secs: now_unix_secs(),
    };
    let bytes = serde_json::to_vec(&claim).map_err(|error| error.to_string())?;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => {
            use std::io::Write;
            if let Err(error) = file.write_all(&bytes) {
                let _ = std::fs::remove_file(path);
                return Err(error.to_string());
            }
            let stop = Arc::new(AtomicBool::new(false));
            let heartbeat = heartbeat_claim(
                path.to_path_buf(),
                claim.owner_id.clone(),
                Arc::clone(&stop),
            );
            Ok(PostprocessClaimGuard {
                path: path.to_path_buf(),
                owner_id: claim.owner_id,
                stop,
                heartbeat: Some(heartbeat),
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let Ok(_mutation) = acquire_claim_mutation(path) else {
                return Err(
                    "artifact postprocess step is already executing under a live claim".to_string(),
                );
            };
            if claim_is_recoverable(path) {
                std::fs::remove_file(path).map_err(|error| error.to_string())?;
                acquire_claim(path)
            } else {
                Err("artifact postprocess step is already executing under a live claim".to_string())
            }
        }
        Err(error) => Err(error.to_string()),
    }
}

fn acquire_claim_mutation(path: &Path) -> std::result::Result<ClaimMutationGuard, String> {
    let mutation = claim_mutation_path(path);
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&mutation)
        .map(|_| ClaimMutationGuard(mutation))
        .map_err(|error| error.to_string())
}

fn claim_is_recoverable(path: &Path) -> bool {
    // A crash while creating or rewriting a claim can leave an empty/truncated
    // file. It has no valid live owner, so a contender can safely replace it.
    std::fs::read(path)
        .ok()
        .and_then(|raw| serde_json::from_slice::<PostprocessClaim>(&raw).ok())
        .map(|claim| claim.schema == CLAIM_SCHEMA && !claim_owner_is_alive(claim.owner_pid))
        .unwrap_or(true)
}

fn worker_is_alive(worker: &PostprocessWorkerSpawn) -> bool {
    worker.schema == WORKER_SPAWN_SCHEMA
        && matches!(
            homeboy_core::process::process_identity_state_with_start_identity(
                worker.pid,
                None,
                Some(&worker.start_identity),
            ),
            homeboy_core::process::ProcessIdentityState::Live
                | homeboy_core::process::ProcessIdentityState::Unverifiable
        )
}

fn write_json_atomically<T: Serialize>(path: &Path, value: &T) -> homeboy_core::Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| {
        homeboy_core::Error::internal_json(error.to_string(), Some(path.display().to_string()))
    })?;
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    std::fs::write(&temporary, bytes).map_err(|error| {
        homeboy_core::Error::internal_io(error.to_string(), Some(temporary.display().to_string()))
    })?;
    std::fs::rename(temporary, path).map_err(|error| {
        homeboy_core::Error::internal_io(error.to_string(), Some(path.display().to_string()))
    })
}

fn heartbeat_claim(
    path: PathBuf,
    owner_id: String,
    stop: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_secs(CLAIM_HEARTBEAT_INTERVAL_SECS));
            if stop.load(Ordering::SeqCst) || !claim_owned_by(&path, &owner_id) {
                return;
            }
            let Ok(_mutation) = acquire_claim_mutation(&path) else {
                continue;
            };
            if !claim_owned_by(&path, &owner_id) {
                return;
            }
            let Ok(mut claim) = std::fs::read(&path)
                .ok()
                .and_then(|raw| serde_json::from_slice::<PostprocessClaim>(&raw).ok())
                .ok_or(())
            else {
                return;
            };
            claim.heartbeat_unix_secs = now_unix_secs();
            let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
            if serde_json::to_vec(&claim)
                .ok()
                .and_then(|bytes| std::fs::write(&temporary, bytes).ok())
                .is_none()
            {
                return;
            }
            if claim_owned_by(&path, &owner_id) {
                let _ = std::fs::rename(&temporary, &path);
            } else {
                let _ = std::fs::remove_file(&temporary);
                return;
            }
        }
    })
}

fn claim_owned_by(path: &Path, owner_id: &str) -> bool {
    std::fs::read(path)
        .ok()
        .and_then(|raw| serde_json::from_slice::<PostprocessClaim>(&raw).ok())
        .is_some_and(|claim| claim.schema == CLAIM_SCHEMA && claim.owner_id == owner_id)
}

#[cfg(unix)]
fn claim_owner_is_alive(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    unsafe {
        libc::kill(pid as libc::pid_t, 0) == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

#[cfg(not(unix))]
fn claim_owner_is_alive(pid: u32) -> bool {
    pid == std::process::id()
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn checkpoint_identity(
    plan: &AgentTaskPlan,
    run_id: Option<&str>,
    step: &AgentTaskArtifactPostprocessStep,
    outcomes: &[AgentTaskOutcome],
) -> String {
    let dependencies: Vec<_> = step
        .depends_on
        .iter()
        .filter_map(|id| outcomes.iter().find(|outcome| outcome.task_id == *id))
        .collect();
    let artifact_identities: Vec<_> = dependencies
        .iter()
        .flat_map(|outcome| {
            outcome.artifacts.iter().map(move |artifact| {
                serde_json::json!({
                    "task_id": outcome.task_id,
                    "id": artifact.id,
                    "kind": artifact.kind,
                    "sha256": artifact.sha256.clone().or_else(|| artifact.path.as_deref().and_then(|path| std::fs::read(path).ok()).map(|bytes| content_hash::sha256_hex(&bytes))),
                })
            })
        })
        .collect::<Vec<_>>();
    let payload = serde_json::json!({
        "schema": CHECKPOINT_SCHEMA,
        "run_id": run_id.unwrap_or("unrecorded-run"),
        "plan_id": plan.plan_id,
        "step_id": step.id,
        "plan": step.plan,
        "dependencies": dependencies,
        "artifact_identities": artifact_identities,
    });
    content_hash::sha256_hex(&serde_json::to_vec(&payload).unwrap_or_default())
}

fn read_checkpoint(
    path: &Path,
    run_id: Option<&str>,
    step_id: &str,
    identity: &str,
) -> Option<AgentTaskOutcome> {
    let checkpoint: PostprocessCheckpoint =
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    (checkpoint.schema == CHECKPOINT_SCHEMA
        && checkpoint.run_id == run_id.unwrap_or("unrecorded-run")
        && checkpoint.step_id == step_id
        && checkpoint.fingerprint == identity)
        .then_some(checkpoint.outcome)
}

fn write_checkpoint(
    path: &Path,
    run_id: Option<&str>,
    fingerprint: &str,
    outcomes: &[AgentTaskOutcome],
    step: &AgentTaskArtifactPostprocessStep,
    outcome: &AgentTaskOutcome,
) -> homeboy_core::Result<()> {
    let parent = path.parent().expect("checkpoint has parent");
    std::fs::create_dir_all(parent).map_err(|error| {
        homeboy_core::Error::internal_io(error.to_string(), Some(parent.display().to_string()))
    })?;
    let payload = serde_json::to_vec_pretty(&PostprocessCheckpoint {
        schema: CHECKPOINT_SCHEMA.to_string(),
        run_id: run_id.unwrap_or("unrecorded-run").to_string(),
        step_id: step.id.clone(),
        fingerprint: fingerprint.to_string(),
        dependencies: step
            .depends_on
            .iter()
            .filter_map(|id| {
                outcomes
                    .iter()
                    .find(|outcome| outcome.task_id == *id)
                    .cloned()
            })
            .collect(),
        outcome: outcome.clone(),
    })
    .map_err(|error| {
        homeboy_core::Error::internal_json(error.to_string(), Some(path.display().to_string()))
    })?;
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&temporary, payload).map_err(|error| {
        homeboy_core::Error::internal_io(error.to_string(), Some(temporary.display().to_string()))
    })?;
    std::fs::rename(&temporary, path).map_err(|error| {
        homeboy_core::Error::internal_io(error.to_string(), Some(path.display().to_string()))
    })
}

fn checkpoint_failure_outcome(
    step: &AgentTaskArtifactPostprocessStep,
    message: String,
) -> AgentTaskOutcome {
    let mut outcome = postprocess_outcome(
        step,
        AgentTaskOutcomeStatus::Failed,
        "artifact postprocess side effects completed but checkpoint persistence failed",
        Some(message),
    );
    outcome.metadata["checkpoint_write_failed"] = serde_json::json!(true);
    outcome
}

pub(super) fn recovered_upstream_outcomes(
    plan: &AgentTaskPlan,
    run_id: Option<&str>,
) -> Vec<AgentTaskOutcome> {
    let mut recovered = Vec::new();
    for step in &plan.postprocess_steps {
        let path = postprocess_checkpoint_path(run_id, &step.id);
        let Ok(raw) = std::fs::read(path) else {
            continue;
        };
        let Ok(checkpoint) = serde_json::from_slice::<PostprocessCheckpoint>(&raw) else {
            continue;
        };
        if checkpoint.schema != CHECKPOINT_SCHEMA
            || checkpoint.run_id != run_id.unwrap_or("unrecorded-run")
            || checkpoint.step_id != step.id
        {
            continue;
        }
        let identity = checkpoint_identity(plan, run_id, step, &checkpoint.dependencies);
        if checkpoint.fingerprint != identity {
            continue;
        }
        for outcome in checkpoint.dependencies {
            if !recovered
                .iter()
                .any(|existing: &AgentTaskOutcome| existing.task_id == outcome.task_id)
            {
                recovered.push(outcome);
            }
        }
    }
    recovered
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_task::{
        AgentTaskExecutor, AgentTaskLimits, AgentTaskPolicy, AgentTaskWorkspace,
        AGENT_TASK_REQUEST_SCHEMA,
    };
    use homeboy_core::artifacts::{
        ArtifactPostprocessAction, ArtifactPostprocessPlan, ArtifactPostprocessRoot,
        ARTIFACT_POSTPROCESS_PLAN_SCHEMA,
    };
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn install_test_helper(home: &tempfile::TempDir) {
        let helper = home.path().join("postprocess-helper");
        std::fs::write(
            &helper,
            "#!/bin/sh\nset -e\ncase \"$1\" in\n  copy) cp \"$HOMEBOY_ARTIFACT_POSTPROCESS_INPUT\" \"$HOMEBOY_ARTIFACT_POSTPROCESS_OUTPUT\"; printf x >> \"$HOMEBOY_ARTIFACT_POSTPROCESS_ARTIFACT_ROOT/count\" ;;\n  report) printf x >> \"$HOMEBOY_ARTIFACT_POSTPROCESS_ARTIFACT_ROOT/count\"; printf report > \"$HOMEBOY_ARTIFACT_POSTPROCESS_OUTPUT\" ;;\n  fail) exit 3 ;;\nesac\n",
        )
        .expect("helper");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755))
                .expect("helper permissions");
        }
        let registry = home.path().join("postprocess-helpers.json");
        std::fs::write(
            &registry,
            serde_json::json!({
                "schema": homeboy_core::artifacts::ARTIFACT_POSTPROCESS_HELPER_REGISTRY_SCHEMA,
                "helpers": [{
                    "id": "fixture",
                    "path": helper,
                    "sha256": content_hash::sha256_hex(&std::fs::read(&helper).expect("helper bytes")),
                    "actions": ["copy", "report", "fail"]
                }]
            })
            .to_string(),
        )
        .expect("helper registry");
        std::env::set_var(
            homeboy_core::artifacts::ARTIFACT_POSTPROCESS_HELPER_REGISTRY_ENV,
            registry,
        );
    }

    #[test]
    fn materializes_binary_dependency_and_records_postprocessed_artifact() {
        homeboy_core::test_support::with_isolated_home(|home| {
            install_test_helper(home);
            let source = home.path().join("capture.bin");
            std::fs::write(&source, [0, 255, 17, 42]).expect("binary input");
            let producer = AgentTaskOutcome {
                task_id: "capture".to_string(),
                status: AgentTaskOutcomeStatus::Succeeded,
                artifacts: vec![AgentTaskArtifact {
                    id: "capture.bin".to_string(),
                    kind: "capture".to_string(),
                    path: Some(source.display().to_string()),
                    ..Default::default()
                }],
                ..Default::default()
            };
            let step = AgentTaskArtifactPostprocessStep {
                id: "compose".to_string(),
                depends_on: vec!["capture".to_string()],
                required: true,
                plan: ArtifactPostprocessPlan {
                    schema: ARTIFACT_POSTPROCESS_PLAN_SCHEMA.to_string(),
                    plan_id: "compose".to_string(),
                    artifact_roots: vec![ArtifactPostprocessRoot {
                        id: "output".to_string(),
                        path: "unused".to_string(),
                        persisted_ref: None,
                        manifest_path: None,
                    }],
                    actions: vec![ArtifactPostprocessAction {
                        id: Some("copy".to_string()),
                        helper: "fixture".to_string(),
                        action: "copy".to_string(),
                        input: Some("${run.input}/capture/capture.bin".to_string()),
                        output: "result.bin".to_string(),
                        parameters: BTreeMap::new(),
                        required: true,
                        side_effects: vec!["artifact_root_output".to_string()],
                    }],
                    reviewer_refs: Vec::new(),
                    metadata: serde_json::json!({}),
                },
            };
            let mut outcomes = vec![producer];
            let mut events = Vec::new();
            run_postprocess_steps(
                &AgentTaskPlan {
                    postprocess_steps: vec![step],
                    ..AgentTaskPlan::new("binary", Vec::new())
                },
                Some("run-1"),
                &mut outcomes,
                &mut events,
                &AgentTaskCancellationToken::default(),
            );
            let result = outcomes.last().expect("postprocess outcome");
            assert_eq!(
                result.status,
                AgentTaskOutcomeStatus::Succeeded,
                "postprocess step did not succeed; outcome: {result:#?}"
            );
            let output_path = result.artifacts[0].path.as_deref().expect("path");
            assert_eq!(
                std::fs::read(output_path).unwrap_or_else(|error| panic!(
                    "helper reported success but wrote no output at {output_path}: {error}; \
                     outcome: {result:#?}"
                )),
                [0, 255, 17, 42]
            );
            assert_eq!(
                result.artifacts[0].sha256.as_deref().map(str::len),
                Some(64)
            );
            let reported_output = result.outputs["artifact_postprocess"]["outputs"][0]["output"]
                .as_str()
                .expect("reported output path");
            assert_eq!(reported_output, output_path);
            assert!(
                !reported_output.contains("/staging/"),
                "promoted output path retained staging location: {reported_output}"
            );
        });
    }

    #[test]
    fn optional_step_failure_is_visible_without_failing_the_aggregate() {
        homeboy_core::test_support::with_isolated_home(|home| {
            install_test_helper(home);
            let step = AgentTaskArtifactPostprocessStep {
                id: "optional-compose".to_string(),
                depends_on: vec!["capture".to_string()],
                required: false,
                plan: ArtifactPostprocessPlan {
                    schema: ARTIFACT_POSTPROCESS_PLAN_SCHEMA.to_string(),
                    plan_id: "optional-compose".to_string(),
                    artifact_roots: vec![ArtifactPostprocessRoot {
                        id: "output".to_string(),
                        path: "unused".to_string(),
                        persisted_ref: None,
                        manifest_path: None,
                    }],
                    actions: vec![ArtifactPostprocessAction {
                        id: Some("fail".to_string()),
                        helper: "fixture".to_string(),
                        action: "fail".to_string(),
                        input: None,
                        output: "unused.bin".to_string(),
                        parameters: BTreeMap::new(),
                        required: true,
                        side_effects: vec!["artifact_root_output".to_string()],
                    }],
                    reviewer_refs: Vec::new(),
                    metadata: serde_json::json!({}),
                },
            };
            let mut outcomes = vec![AgentTaskOutcome {
                task_id: "capture".to_string(),
                status: AgentTaskOutcomeStatus::Succeeded,
                ..Default::default()
            }];
            let mut events = Vec::new();
            run_postprocess_steps(
                &AgentTaskPlan {
                    postprocess_steps: vec![step],
                    ..AgentTaskPlan::new("optional", Vec::new())
                },
                Some("optional-run"),
                &mut outcomes,
                &mut events,
                &AgentTaskCancellationToken::default(),
            );

            let optional = outcomes.last().expect("optional outcome");
            assert_eq!(optional.status, AgentTaskOutcomeStatus::Succeeded);
            assert_eq!(optional.metadata["optional_failure"], true);
            assert_eq!(
                AgentTaskScheduleSupport::aggregate_status(&outcomes),
                AgentTaskAggregateStatus::Succeeded
            );
        });
    }

    #[test]
    fn checkpoint_reuses_completed_step_after_interruption() {
        homeboy_core::test_support::with_isolated_home(|home| {
            install_test_helper(home);
            let source = home.path().join("capture.bin");
            std::fs::write(&source, [1, 2, 3]).expect("input");
            let producer = AgentTaskOutcome {
                task_id: "capture".to_string(),
                status: AgentTaskOutcomeStatus::Succeeded,
                artifacts: vec![AgentTaskArtifact {
                    id: "capture.bin".to_string(),
                    kind: "capture".to_string(),
                    path: Some(source.display().to_string()),
                    ..Default::default()
                }],
                ..Default::default()
            };
            let step = AgentTaskArtifactPostprocessStep {
                id: "compose".to_string(),
                depends_on: vec!["capture".to_string()],
                required: true,
                plan: ArtifactPostprocessPlan {
                    schema: ARTIFACT_POSTPROCESS_PLAN_SCHEMA.to_string(),
                    plan_id: "compose".to_string(),
                    artifact_roots: vec![ArtifactPostprocessRoot {
                        id: "output".to_string(),
                        path: "unused".to_string(),
                        persisted_ref: None,
                        manifest_path: None,
                    }],
                    actions: vec![ArtifactPostprocessAction {
                        id: Some("copy".to_string()),
                        helper: "fixture".to_string(),
                        action: "copy".to_string(),
                        input: Some("${run.input}/capture/capture.bin".to_string()),
                        output: "result.bin".to_string(),
                        parameters: BTreeMap::new(),
                        required: true,
                        side_effects: vec!["artifact_root_output".to_string()],
                    }],
                    reviewer_refs: Vec::new(),
                    metadata: serde_json::json!({}),
                },
            };
            let plan = AgentTaskPlan {
                postprocess_steps: vec![step],
                ..AgentTaskPlan::new("resume", Vec::new())
            };
            let mut first = vec![producer.clone()];
            let mut events = Vec::new();
            run_postprocess_steps(
                &plan,
                Some("resume-run"),
                &mut first,
                &mut events,
                &AgentTaskCancellationToken::default(),
            );
            let output = first.last().expect("first outcome").artifacts[0]
                .path
                .clone()
                .expect("output");
            let count = Path::new(&output)
                .parent()
                .expect("output parent")
                .join("count");

            let mut resumed = vec![producer];
            run_postprocess_steps(
                &plan,
                Some("resume-run"),
                &mut resumed,
                &mut events,
                &AgentTaskCancellationToken::default(),
            );
            assert_eq!(
                std::fs::read_to_string(&count).unwrap_or_else(|error| panic!(
                    "helper wrote no count file at {}: {error}; resumed outcomes: {resumed:#?}",
                    count.display()
                )),
                "x"
            );
            assert_eq!(
                resumed.last().expect("resumed outcome").artifacts[0]
                    .path
                    .as_deref(),
                Some(output.as_str())
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn rebase_outcome_paths_handles_canonical_source_aliases_without_prefix_collisions() {
        let root = tempfile::tempdir().expect("root");
        let target = root.path().join("target");
        let alias = root.path().join("alias");
        let source = alias.join("output");
        std::fs::create_dir_all(target.join("output")).expect("source root");
        std::os::unix::fs::symlink(&target, &alias).expect("source alias");
        let canonical = source.canonicalize().expect("canonical source root");
        let version = root.path().join("versions/attempt");
        let lexical = source.join("lexical.txt").display().to_string();
        let canonical_path = canonical.join("canonical.txt").display().to_string();
        let prefix_collision = format!("{}-old/keep.txt", canonical.display());
        let traversal = canonical.join("../sibling/keep.txt").display().to_string();
        let outcome = AgentTaskOutcome {
            artifacts: vec![AgentTaskArtifact {
                path: Some(canonical_path.clone()),
                metadata: serde_json::json!({ "manifest_path": canonical_path }),
                ..Default::default()
            }],
            diagnostics: vec![AgentTaskDiagnostic {
                class: "postprocess".to_string(),
                message: "diagnostic".to_string(),
                data: serde_json::json!({ "path": canonical.join("diagnostic.json") }),
            }],
            outputs: serde_json::json!({
                "lexical": lexical,
                "nested": [canonical_path, prefix_collision, traversal, "relative/output.txt"],
                "url": format!("file://{}", canonical.display()),
            }),
            metadata: serde_json::json!({ "root": canonical }),
            ..Default::default()
        };

        let rebased = rebase_outcome_paths(outcome, &source, &version);

        assert_eq!(
            rebased.artifacts[0].path.as_deref(),
            Some(version.join("canonical.txt").to_string_lossy().as_ref())
        );
        assert_eq!(
            rebased.outputs["lexical"],
            serde_json::json!(version.join("lexical.txt").display().to_string())
        );
        assert_eq!(
            rebased.outputs["nested"][0],
            serde_json::json!(version.join("canonical.txt").display().to_string())
        );
        assert_eq!(rebased.outputs["nested"][1], prefix_collision);
        assert_eq!(rebased.outputs["nested"][2], traversal);
        assert_eq!(rebased.outputs["nested"][3], "relative/output.txt");
        assert_eq!(
            rebased.outputs["url"],
            format!("file://{}", canonical.display())
        );
        assert_eq!(
            rebased.metadata["root"],
            serde_json::json!(version.display().to_string())
        );
        assert_eq!(
            rebased.artifacts[0].metadata["manifest_path"],
            serde_json::json!(version.join("canonical.txt").display().to_string())
        );
        assert_eq!(
            rebased.diagnostics[0].data["path"],
            serde_json::json!(version.join("diagnostic.json").display().to_string())
        );
    }

    #[test]
    fn worker_completion_survives_scheduler_crash_without_reinvoking_helper() {
        homeboy_core::test_support::with_isolated_home(|home| {
            install_test_helper(home);
            let step = AgentTaskArtifactPostprocessStep {
                id: "compose".to_string(),
                depends_on: Vec::new(),
                required: true,
                plan: ArtifactPostprocessPlan {
                    schema: ARTIFACT_POSTPROCESS_PLAN_SCHEMA.to_string(),
                    plan_id: "compose".to_string(),
                    artifact_roots: vec![ArtifactPostprocessRoot {
                        id: "output".to_string(),
                        path: "unused".to_string(),
                        persisted_ref: None,
                        manifest_path: None,
                    }],
                    actions: vec![ArtifactPostprocessAction {
                        id: Some("report".to_string()),
                        helper: "fixture".to_string(),
                        action: "report".to_string(),
                        input: None,
                        output: "report.txt".to_string(),
                        parameters: BTreeMap::new(),
                        required: true,
                        side_effects: vec!["artifact_root_output".to_string()],
                    }],
                    reviewer_refs: Vec::new(),
                    metadata: serde_json::json!({}),
                },
            };
            let plan = AgentTaskPlan {
                postprocess_steps: vec![step.clone()],
                ..AgentTaskPlan::new("resume", Vec::new())
            };
            let identity = checkpoint_identity(&plan, Some("crash-run"), &step, &[]);
            let root = postprocess_root(Some("crash-run"), &step.id);
            std::fs::create_dir_all(root.join("output")).expect("old output");
            std::fs::write(root.join("output/old.txt"), "old").expect("old output");

            // The worker has already invoked the helper and durably recorded its
            // completion. Removing its checkpoint models a scheduler dying before
            // it can observe the worker result; recovery must adopt, never rerun.
            let request = root.join("request.json");
            write_json_atomically(
                &request,
                &PostprocessWorkerRequest {
                    schema: WORKER_REQUEST_SCHEMA.to_string(),
                    run_id: "crash-run".to_string(),
                    plan_id: plan.plan_id.clone(),
                    step: step.clone(),
                    fingerprint: identity.clone(),
                    dependencies: Vec::new(),
                    claim_delay_millis: 0,
                },
            )
            .expect("worker request");
            run_postprocess_worker(&request).expect("worker completion");
            std::fs::remove_file(postprocess_checkpoint_path(Some("crash-run"), &step.id))
                .expect("scheduler did not resume");
            assert!(
                root.join("output/old.txt").exists(),
                "prior artifact versions remain immutable"
            );
            let pointer: PostprocessArtifactPointer =
                serde_json::from_slice(&std::fs::read(root.join("current.json")).expect("pointer"))
                    .expect("pointer json");
            assert!(
                root.join(&pointer.version).is_dir(),
                "pointer atomically selects an immutable version"
            );
            assert_eq!(
                std::fs::read_to_string(root.join(&pointer.version).join("count")).expect("count"),
                "x"
            );
            std::fs::create_dir_all(root.join("staging/incomplete")).expect("incomplete stage");
            std::fs::write(root.join("staging/incomplete/partial"), "partial")
                .expect("incomplete stage");
            std::fs::write(
                postprocess_claim_path(Some("crash-run"), &step.id),
                serde_json::to_vec(&PostprocessClaim {
                    schema: CLAIM_SCHEMA.to_string(),
                    owner_id: "dead-owner".to_string(),
                    owner_pid: u32::MAX,
                    heartbeat_unix_secs: 0,
                })
                .expect("claim"),
            )
            .expect("dead claim");

            let mut outcomes = Vec::new();
            let mut events = Vec::new();
            run_postprocess_steps(
                &plan,
                Some("crash-run"),
                &mut outcomes,
                &mut events,
                &AgentTaskCancellationToken::default(),
            );

            assert_eq!(
                outcomes.last().expect("adopted outcome").status,
                AgentTaskOutcomeStatus::Succeeded
            );
            assert_eq!(
                std::fs::read_to_string(root.join(&pointer.version).join("count")).expect("count"),
                "x",
                "completed helper is invoked once"
            );
            assert!(
                !root.join("staging/incomplete").exists(),
                "incomplete staging is discarded"
            );
            assert!(postprocess_checkpoint_path(Some("crash-run"), &step.id).is_file());
        });
    }

    #[test]
    fn rejects_dot_artifact_filenames() {
        for name in [".", ".."] {
            let artifact = AgentTaskArtifact {
                id: name.to_string(),
                kind: "input".to_string(),
                ..Default::default()
            };
            assert!(
                materialized_artifact_name(&artifact).is_err(),
                "{name} must be rejected"
            );
        }
    }

    #[test]
    fn claim_keeps_long_running_live_owner_and_recovers_dead_owner() {
        let root = tempfile::tempdir().expect("claim root");
        let path = root.path().join("claim.json");
        let first = acquire_claim(&path).expect("first claim");
        let initial: PostprocessClaim =
            serde_json::from_slice(&std::fs::read(&path).expect("claim")).expect("claim json");
        thread::sleep(Duration::from_secs(CLAIM_HEARTBEAT_INTERVAL_SECS + 1));
        let renewed: PostprocessClaim =
            serde_json::from_slice(&std::fs::read(&path).expect("claim")).expect("claim json");
        assert!(
            renewed.heartbeat_unix_secs > initial.heartbeat_unix_secs,
            "active lease heartbeats renew"
        );
        assert!(
            acquire_claim(&path).is_err(),
            "live claim blocks duplicate execution"
        );
        let first_owner: PostprocessClaim =
            serde_json::from_slice(&std::fs::read(&path).expect("claim")).expect("claim json");
        drop(first);
        let replacement = acquire_claim(&path).expect("replacement claim");
        let replacement_owner: PostprocessClaim =
            serde_json::from_slice(&std::fs::read(&path).expect("claim")).expect("claim json");
        assert_ne!(
            first_owner.owner_id, replacement_owner.owner_id,
            "same-process schedulers receive unique owners"
        );
        drop(replacement);
        std::fs::write(
            &path,
            serde_json::to_vec(&PostprocessClaim {
                schema: CLAIM_SCHEMA.to_string(),
                owner_id: "active-owner".to_string(),
                owner_pid: std::process::id(),
                heartbeat_unix_secs: 0,
            })
            .expect("claim json"),
        )
        .expect("old live claim");
        assert!(
            acquire_claim(&path).is_err(),
            "a live owner retains an old lease"
        );
        std::fs::write(
            &path,
            serde_json::to_vec(&PostprocessClaim {
                schema: CLAIM_SCHEMA.to_string(),
                owner_id: "dead-owner".to_string(),
                owner_pid: u32::MAX,
                heartbeat_unix_secs: 0,
            })
            .expect("claim json"),
        )
        .expect("dead stale claim");
        std::fs::write(claim_mutation_path(&path), "takeover in progress")
            .expect("interleaving lock");
        assert!(
            acquire_claim(&path).is_err(),
            "a contender cannot delete a claim while another contender owns takeover"
        );
        std::fs::remove_file(claim_mutation_path(&path)).expect("release interleaving lock");
        assert!(
            acquire_claim(&path).is_ok(),
            "dead stale claim is recoverable"
        );
        std::fs::write(&path, []).expect("empty crashed claim");
        assert!(
            acquire_claim(&path).is_ok(),
            "an empty claim from a crashed owner is recoverable"
        );
    }

    #[test]
    fn checkpoint_identity_rejects_changed_dependency_digest_and_recovers_upstream() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let producer = AgentTaskOutcome {
                task_id: "capture".to_string(),
                status: AgentTaskOutcomeStatus::Succeeded,
                artifacts: vec![AgentTaskArtifact {
                    id: "capture.bin".to_string(),
                    kind: "input".to_string(),
                    sha256: Some("a".repeat(64)),
                    ..Default::default()
                }],
                ..Default::default()
            };
            let step = AgentTaskArtifactPostprocessStep {
                id: "compose".to_string(),
                depends_on: vec!["capture".to_string()],
                required: true,
                plan: ArtifactPostprocessPlan {
                    schema: ARTIFACT_POSTPROCESS_PLAN_SCHEMA.to_string(),
                    plan_id: "compose".to_string(),
                    artifact_roots: vec![ArtifactPostprocessRoot {
                        id: "output".to_string(),
                        path: "unused".to_string(),
                        persisted_ref: None,
                        manifest_path: None,
                    }],
                    actions: Vec::new(),
                    reviewer_refs: Vec::new(),
                    metadata: serde_json::json!({}),
                },
            };
            let plan = AgentTaskPlan {
                postprocess_steps: vec![step.clone()],
                ..AgentTaskPlan::new("recover", Vec::new())
            };
            let checkpoint = postprocess_checkpoint_path(Some("recover-run"), &step.id);
            let identity = checkpoint_identity(
                &plan,
                Some("recover-run"),
                &step,
                std::slice::from_ref(&producer),
            );
            write_checkpoint(
                &checkpoint,
                Some("recover-run"),
                &identity,
                std::slice::from_ref(&producer),
                &step,
                &postprocess_outcome(&step, AgentTaskOutcomeStatus::Succeeded, "done", None),
            )
            .expect("checkpoint");
            assert_eq!(
                recovered_upstream_outcomes(&plan, Some("recover-run"))[0].task_id,
                "capture"
            );
            let mut changed = producer;
            changed.artifacts[0].sha256 = Some("b".repeat(64));
            let stale = checkpoint_identity(&plan, Some("recover-run"), &step, &[changed]);
            assert!(
                read_checkpoint(&checkpoint, Some("recover-run"), &step.id, &stale).is_none(),
                "changed artifact digest invalidates checkpoint"
            );
        });
    }

    #[test]
    fn checkpoint_write_failure_is_terminal_and_visible() {
        let step = AgentTaskArtifactPostprocessStep {
            id: "compose".to_string(),
            depends_on: Vec::new(),
            required: true,
            plan: ArtifactPostprocessPlan {
                schema: ARTIFACT_POSTPROCESS_PLAN_SCHEMA.to_string(),
                plan_id: "compose".to_string(),
                artifact_roots: vec![ArtifactPostprocessRoot {
                    id: "output".to_string(),
                    path: "unused".to_string(),
                    persisted_ref: None,
                    manifest_path: None,
                }],
                actions: Vec::new(),
                reviewer_refs: Vec::new(),
                metadata: serde_json::json!({}),
            },
        };
        let root = tempfile::tempdir().expect("checkpoint root");
        let checkpoint = root.path().join("checkpoint.json");
        std::fs::create_dir(&checkpoint).expect("checkpoint directory");
        let error = write_checkpoint(
            &checkpoint,
            Some("run"),
            "identity",
            &[],
            &step,
            &postprocess_outcome(&step, AgentTaskOutcomeStatus::Succeeded, "done", None),
        )
        .expect_err("write fails after side effect");
        let outcome = checkpoint_failure_outcome(&step, error.message);
        assert_eq!(outcome.status, AgentTaskOutcomeStatus::Failed);
        assert_eq!(outcome.metadata["checkpoint_write_failed"], true);
    }

    #[test]
    fn scheduler_restart_recovers_upstream_without_provider_rerun() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let request = AgentTaskRequest {
                schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
                task_id: "capture".to_string(),
                group_key: None,
                parent_plan_id: None,
                executor: AgentTaskExecutor {
                    backend: "test".to_string(),
                    selector: None,
                    runtime_selection: None,
                    required_capabilities: Vec::new(),
                    secret_env: Vec::new(),
                    model: None,
                    config: serde_json::Value::Null,
                },
                instructions: "capture".to_string(),
                inputs: serde_json::Value::Null,
                source_refs: Vec::new(),
                workspace: AgentTaskWorkspace::default(),
                component_contracts: Vec::new(),
                runtime_tools: Vec::new(),
                policy: AgentTaskPolicy::default(),
                limits: AgentTaskLimits::default(),
                expected_artifacts: Vec::new(),
                artifact_declarations: Vec::new(),
                output_declarations: Vec::new(),
                metadata: serde_json::Value::Null,
            };
            let step = AgentTaskArtifactPostprocessStep {
                id: "compose".to_string(),
                depends_on: vec!["capture".to_string()],
                required: true,
                plan: ArtifactPostprocessPlan {
                    schema: ARTIFACT_POSTPROCESS_PLAN_SCHEMA.to_string(),
                    plan_id: "compose".to_string(),
                    artifact_roots: vec![ArtifactPostprocessRoot {
                        id: "output".to_string(),
                        path: "unused".to_string(),
                        persisted_ref: None,
                        manifest_path: None,
                    }],
                    actions: Vec::new(),
                    reviewer_refs: Vec::new(),
                    metadata: serde_json::json!({}),
                },
            };
            let plan = AgentTaskPlan {
                postprocess_steps: vec![step.clone()],
                ..AgentTaskPlan::new("restart", vec![request])
            };
            let producer = AgentTaskOutcome {
                task_id: "capture".to_string(),
                status: AgentTaskOutcomeStatus::Succeeded,
                ..Default::default()
            };
            let checkpoint = postprocess_checkpoint_path(Some("restart-run"), &step.id);
            let identity = checkpoint_identity(
                &plan,
                Some("restart-run"),
                &step,
                std::slice::from_ref(&producer),
            );
            write_checkpoint(
                &checkpoint,
                Some("restart-run"),
                &identity,
                &[producer],
                &step,
                &postprocess_outcome(&step, AgentTaskOutcomeStatus::Succeeded, "done", None),
            )
            .expect("checkpoint");
            let calls = Arc::new(AtomicUsize::new(0));
            struct Executor(Arc<AtomicUsize>);
            impl AgentTaskExecutorAdapter for Executor {
                fn execute(
                    &self,
                    request: AgentTaskRequest,
                    _context: AgentTaskExecutionContext,
                ) -> AgentTaskOutcome {
                    self.0.fetch_add(1, Ordering::SeqCst);
                    AgentTaskOutcome {
                        task_id: request.task_id,
                        status: AgentTaskOutcomeStatus::Succeeded,
                        ..Default::default()
                    }
                }
            }
            let aggregate = AgentTaskScheduler::new(Executor(Arc::clone(&calls)))
                .with_run_id("restart-run")
                .run(plan);
            assert_eq!(
                calls.load(Ordering::SeqCst),
                0,
                "recovered upstream outcome skips provider dispatch"
            );
            assert_eq!(aggregate.totals.succeeded, 2);
        });
    }
}
