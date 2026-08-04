//! Plan-native execution of generic artifact postprocess actions.

use std::path::{Component, Path, PathBuf};
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
                    let outcome = execute_postprocess_step(step, run_id, outcomes);
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
) -> AgentTaskOutcome {
    let root = postprocess_root(run_id, &step.id);
    let input = root.join("input");
    let output = root.join("output");
    let result = (|| {
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
    match result {
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
                metadata: serde_json::json!({ "step_kind": "artifact_postprocess", "required": step.required, "optional_failure": failed && !step.required, "root": root }),
                ..Default::default()
            }
        }
        Err(error) => postprocess_failure_outcome(step, &error.message),
    }
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
    created_at_unix_secs: u64,
}

const CHECKPOINT_SCHEMA: &str = "homeboy/agent-task-postprocess-checkpoint/v2";
const CLAIM_SCHEMA: &str = "homeboy/agent-task-postprocess-claim/v1";
const CLAIM_STALE_AFTER_SECS: u64 = 300;

struct PostprocessClaimGuard(PathBuf);

impl Drop for PostprocessClaimGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn acquire_claim(path: &Path) -> std::result::Result<PostprocessClaimGuard, String> {
    let parent = path.parent().expect("claim has parent");
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let claim = PostprocessClaim {
        schema: CLAIM_SCHEMA.to_string(),
        created_at_unix_secs: now_unix_secs(),
    };
    let bytes = serde_json::to_vec(&claim).map_err(|error| error.to_string())?;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => {
            use std::io::Write;
            file.write_all(&bytes).map_err(|error| error.to_string())?;
            Ok(PostprocessClaimGuard(path.to_path_buf()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let stale = std::fs::read(path)
                .ok()
                .and_then(|raw| serde_json::from_slice::<PostprocessClaim>(&raw).ok())
                .map(|claim| {
                    claim.schema == CLAIM_SCHEMA
                        && now_unix_secs().saturating_sub(claim.created_at_unix_secs)
                            > CLAIM_STALE_AFTER_SECS
                })
                .unwrap_or(false);
            if stale {
                std::fs::remove_file(path).map_err(|error| error.to_string())?;
                acquire_claim(path)
            } else {
                Err("artifact postprocess step is already executing under a live claim".to_string())
            }
        }
        Err(error) => Err(error.to_string()),
    }
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
    use homeboy_core::artifacts::{
        ArtifactPostprocessAction, ArtifactPostprocessPlan, ArtifactPostprocessRoot,
        ARTIFACT_POSTPROCESS_PLAN_SCHEMA,
    };
    use std::collections::BTreeMap;

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
    fn claim_blocks_live_execution_and_recovers_stale_claim() {
        let root = tempfile::tempdir().expect("claim root");
        let path = root.path().join("claim.json");
        let first = acquire_claim(&path).expect("first claim");
        assert!(
            acquire_claim(&path).is_err(),
            "live claim blocks duplicate execution"
        );
        drop(first);
        std::fs::write(
            &path,
            serde_json::to_vec(&PostprocessClaim {
                schema: CLAIM_SCHEMA.to_string(),
                created_at_unix_secs: 0,
            })
            .expect("claim json"),
        )
        .expect("stale claim");
        assert!(acquire_claim(&path).is_ok(), "stale claim is recoverable");
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
}
