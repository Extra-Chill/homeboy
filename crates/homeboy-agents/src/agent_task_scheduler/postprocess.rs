//! Plan-native execution of generic artifact postprocess actions.

use std::path::{Component, Path, PathBuf};
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
        let outcome = if let Some(outcome) =
            read_checkpoint(&checkpoint, run_id, &step.id, &identity)
        {
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
            match acquire_claim(&postprocess_claim_path(run_id, &step.id)) {
                Ok(claim) => {
                    // A prior owner may have finished after our first checkpoint read.
                    let outcome = read_checkpoint(&checkpoint, run_id, &step.id, &identity)
                        .or_else(|| recover_completed_attempt(step, run_id, &identity).ok())
                        .unwrap_or_else(|| {
                            execute_postprocess_step(step, run_id, outcomes, &identity)
                        });
                    let checkpoint_result =
                        write_checkpoint(&checkpoint, run_id, &identity, outcomes, step, &outcome);
                    drop(claim);
                    match checkpoint_result {
                        Ok(()) => outcome,
                        Err(error) => checkpoint_failure_outcome(step, error.message),
                    }
                }
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
    let final_output = root.join("output");
    let outcome = rebase_outcome_paths(outcome, &output, &final_output);
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
        let promoted_output = root.join("output");
        let staged_matches = output_digest(&staged_output)
            .map(|digest| digest == completion.output_digest)
            .unwrap_or(false);
        let promoted_matches = output_digest(&promoted_output)
            .map(|digest| digest == completion.output_digest)
            .unwrap_or(false);
        if staged_matches {
            promote_attempt(&root, &attempt, &completion)?;
            return Ok(completion.outcome);
        }
        if promoted_matches {
            return Ok(completion.outcome);
        }
        let _ = std::fs::remove_dir_all(&attempt);
    }
    Err(homeboy_core::Error::internal_unexpected(
        "valid completed postprocess stage is absent",
    ))
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
    let output = root.join("output");
    if output_digest(&output).ok().as_deref() == Some(&completion.output_digest) {
        return Ok(());
    }
    let previous = root.join(format!("output.previous-{}", uuid::Uuid::new_v4()));
    if output.exists() {
        std::fs::rename(&output, &previous).map_err(|error| {
            homeboy_core::Error::internal_io(error.to_string(), Some(output.display().to_string()))
        })?;
    }
    std::fs::rename(&staged, &output).map_err(|error| {
        homeboy_core::Error::internal_io(error.to_string(), Some(output.display().to_string()))
    })?;
    if previous.exists() {
        std::fs::remove_dir_all(&previous).map_err(|error| {
            homeboy_core::Error::internal_io(
                error.to_string(),
                Some(previous.display().to_string()),
            )
        })?;
    }
    Ok(())
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

fn rebase_outcome_paths(mut outcome: AgentTaskOutcome, from: &Path, to: &Path) -> AgentTaskOutcome {
    let from = from.to_string_lossy().to_string();
    let to = to.to_string_lossy().to_string();
    for artifact in &mut outcome.artifacts {
        if let Some(path) = &artifact.path {
            artifact.path = Some(path.replacen(&from, &to, 1));
        }
    }
    rebase_json_paths(&mut outcome.outputs, &from, &to);
    outcome
}

fn rebase_json_paths(value: &mut serde_json::Value, from: &str, to: &str) {
    match value {
        serde_json::Value::String(path) if path.starts_with(from) => {
            *path = path.replacen(from, to, 1)
        }
        serde_json::Value::Array(values) => values
            .iter_mut()
            .for_each(|value| rebase_json_paths(value, from, to)),
        serde_json::Value::Object(values) => values
            .values_mut()
            .for_each(|value| rebase_json_paths(value, from, to)),
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

const CHECKPOINT_SCHEMA: &str = "homeboy/agent-task-postprocess-checkpoint/v2";
const CLAIM_SCHEMA: &str = "homeboy/agent-task-postprocess-claim/v2";
const COMPLETION_SCHEMA: &str = "homeboy/agent-task-postprocess-completion/v1";
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
    std::fs::read(path)
        .ok()
        .and_then(|raw| serde_json::from_slice::<PostprocessClaim>(&raw).ok())
        .is_some_and(|claim| {
            claim.schema == CLAIM_SCHEMA
                && now_unix_secs().saturating_sub(claim.heartbeat_unix_secs)
                    > CLAIM_STALE_AFTER_SECS
                && !claim_owner_is_alive(claim.owner_pid)
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

    #[test]
    fn materializes_binary_dependency_and_records_postprocessed_artifact() {
        homeboy_core::test_support::with_isolated_home(|home| {
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
                id: "compose".to_string(), depends_on: vec!["capture".to_string()], required: true,
                plan: ArtifactPostprocessPlan {
                    schema: ARTIFACT_POSTPROCESS_PLAN_SCHEMA.to_string(), plan_id: "compose".to_string(),
                    artifact_roots: vec![ArtifactPostprocessRoot { id: "output".to_string(), path: "unused".to_string(), persisted_ref: None, manifest_path: None }],
                    actions: vec![ArtifactPostprocessAction {
                        id: Some("copy".to_string()), helper: "sh".to_string(), action: "-c".to_string(),
                        input: Some("${run.input}/capture/capture.bin".to_string()), output: "result.bin".to_string(),
                        parameters: BTreeMap::from([("args".to_string(), serde_json::json!(["cp \"$HOMEBOY_ARTIFACT_POSTPROCESS_INPUT\" \"$HOMEBOY_ARTIFACT_POSTPROCESS_OUTPUT\""]))]), required: true,
                        side_effects: vec!["artifact_root_output".to_string()],
                    }], reviewer_refs: Vec::new(), metadata: serde_json::json!({}),
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
            assert_eq!(result.status, AgentTaskOutcomeStatus::Succeeded);
            assert_eq!(
                std::fs::read(result.artifacts[0].path.as_deref().expect("path")).expect("output"),
                [0, 255, 17, 42]
            );
            assert_eq!(
                result.artifacts[0].sha256.as_deref().map(str::len),
                Some(64)
            );
        });
    }

    #[test]
    fn optional_step_failure_is_visible_without_failing_the_aggregate() {
        homeboy_core::test_support::with_isolated_home(|_| {
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
                        helper: "sh".to_string(),
                        action: "-c".to_string(),
                        input: None,
                        output: "unused.bin".to_string(),
                        parameters: BTreeMap::from([(
                            "args".to_string(),
                            serde_json::json!(["exit 3"]),
                        )]),
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
                id: "compose".to_string(), depends_on: vec!["capture".to_string()], required: true,
                plan: ArtifactPostprocessPlan {
                    schema: ARTIFACT_POSTPROCESS_PLAN_SCHEMA.to_string(), plan_id: "compose".to_string(),
                    artifact_roots: vec![ArtifactPostprocessRoot { id: "output".to_string(), path: "unused".to_string(), persisted_ref: None, manifest_path: None }],
                    actions: vec![ArtifactPostprocessAction {
                        id: Some("copy".to_string()), helper: "sh".to_string(), action: "-c".to_string(),
                        input: Some("${run.input}/capture/capture.bin".to_string()), output: "result.bin".to_string(),
                        parameters: BTreeMap::from([("args".to_string(), serde_json::json!(["cp \"$HOMEBOY_ARTIFACT_POSTPROCESS_INPUT\" \"$HOMEBOY_ARTIFACT_POSTPROCESS_OUTPUT\"; printf x >> \"$HOMEBOY_ARTIFACT_POSTPROCESS_ARTIFACT_ROOT/count\""]))]), required: true,
                        side_effects: vec!["artifact_root_output".to_string()],
                    }], reviewer_refs: Vec::new(), metadata: serde_json::json!({}),
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
            assert_eq!(std::fs::read_to_string(count).expect("count"), "x");
            assert_eq!(
                resumed.last().expect("resumed outcome").artifacts[0]
                    .path
                    .as_deref(),
                Some(output.as_str())
            );
        });
    }

    #[test]
    fn adopts_completion_after_crash_before_checkpoint_without_reinvoking_helper() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let step = AgentTaskArtifactPostprocessStep {
                id: "compose".to_string(),
                depends_on: Vec::new(),
                required: true,
                plan: ArtifactPostprocessPlan {
                    schema: ARTIFACT_POSTPROCESS_PLAN_SCHEMA.to_string(),
                    plan_id: "compose".to_string(),
                    artifact_roots: vec![ArtifactPostprocessRoot { id: "output".to_string(), path: "unused".to_string(), persisted_ref: None, manifest_path: None }],
                    actions: vec![ArtifactPostprocessAction {
                        id: Some("report".to_string()), helper: "sh".to_string(), action: "-c".to_string(),
                        input: None, output: "report.txt".to_string(),
                        parameters: BTreeMap::from([("args".to_string(), serde_json::json!(["printf x >> \"$HOMEBOY_ARTIFACT_POSTPROCESS_ARTIFACT_ROOT/count\"; printf report > \"$HOMEBOY_ARTIFACT_POSTPROCESS_OUTPUT\""]))]),
                        required: true,
                        side_effects: vec!["artifact_root_output".to_string()],
                    }], reviewer_refs: Vec::new(), metadata: serde_json::json!({}),
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

            // This models a process death immediately after durable completion/promotion and before checkpointing.
            execute_postprocess_step(&step, Some("crash-run"), &[], &identity);
            assert!(
                !root.join("output/old.txt").exists(),
                "promotion replaces prior output atomically"
            );
            assert_eq!(
                std::fs::read_to_string(root.join("output/count")).expect("count"),
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
                std::fs::read_to_string(root.join("output/count")).expect("count"),
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
            let identity =
                checkpoint_identity(&plan, Some("recover-run"), &step, &[producer.clone()]);
            write_checkpoint(
                &checkpoint,
                Some("recover-run"),
                &identity,
                &[producer.clone()],
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
            let identity =
                checkpoint_identity(&plan, Some("restart-run"), &step, &[producer.clone()]);
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
