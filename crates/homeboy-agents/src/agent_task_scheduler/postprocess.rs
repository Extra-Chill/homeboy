//! Plan-native execution of generic artifact postprocess actions.

use std::path::{Path, PathBuf};

use homeboy_engine_primitives::content_hash;

use super::*;

pub(super) fn run_postprocess_steps(
    plan: &AgentTaskPlan,
    run_id: Option<&str>,
    outcomes: &mut Vec<AgentTaskOutcome>,
    events: &mut Vec<AgentTaskProgressEvent>,
    cancellation: &AgentTaskCancellationToken,
) {
    for step in &plan.postprocess_steps {
        let outcome = if cancellation.is_cancelled() {
            postprocess_outcome(
                step,
                AgentTaskOutcomeStatus::Cancelled,
                "cancelled before execution",
                None,
            )
        } else if let Some(message) = failed_dependency_message(step, outcomes) {
            postprocess_outcome(step, AgentTaskOutcomeStatus::Failed, &message, None)
        } else {
            execute_postprocess_step(step, run_id, outcomes)
        };
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
                metadata: serde_json::json!({ "step_kind": "artifact_postprocess", "required": step.required, "root": root }),
                ..Default::default()
            }
        }
        Err(error) => postprocess_outcome(
            step,
            AgentTaskOutcomeStatus::Failed,
            &error.message,
            Some(error.message.clone()),
        ),
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
                .join(homeboy_core::paths::sanitize_path_segment(&artifact.id));
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
}
