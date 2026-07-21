use std::collections::HashMap;

use homeboy_core::execution::{
    ChangeArtifact, ChangeArtifactProvenance, ExecutionMode, ExecutionRun, ExecutionStatus,
    ExecutionStepResult,
};
use homeboy_core::plan::PlanSubject;

use super::types::{ReleaseArtifact, ReleaseRun, ReleaseStepResult, ReleaseStepStatus};
use super::version::ChangelogValidationResult;

impl From<&ReleaseRun> for ExecutionRun {
    fn from(run: &ReleaseRun) -> Self {
        let run_id = format!("release.{}", run.component_id);

        Self {
            id: run_id.clone(),
            request_id: None,
            mode: ExecutionMode::Execute,
            subject: PlanSubject {
                component_id: Some(run.component_id.clone()),
                ..PlanSubject::default()
            },
            status: release_status_to_execution_status(&run.result.status),
            steps: run
                .result
                .steps
                .iter()
                .map(ExecutionStepResult::from)
                .collect(),
            artifacts: release_artifacts_from_run(&run_id, &run.result.steps),
            warnings: run.result.warnings.clone(),
            metadata: HashMap::from([("enabled".to_string(), serde_json::json!(run.enabled))]),
        }
    }
}

impl From<&ReleaseStepResult> for ExecutionStepResult {
    fn from(step: &ReleaseStepResult) -> Self {
        Self {
            id: step.id.clone(),
            kind: step.step_type.clone(),
            status: release_status_to_execution_status(&step.status),
            summary: None,
            artifacts: release_artifact_ids(&step.id, step.data.as_ref()),
            warnings: step.warnings.clone(),
            data: step.data.clone(),
            error: step.error.clone(),
        }
    }
}

fn release_status_to_execution_status(status: &ReleaseStepStatus) -> ExecutionStatus {
    match status {
        ReleaseStepStatus::Success => ExecutionStatus::Success,
        ReleaseStepStatus::PartialSuccess => ExecutionStatus::PartialSuccess,
        ReleaseStepStatus::Failed => ExecutionStatus::Failed,
        ReleaseStepStatus::Skipped => ExecutionStatus::Skipped,
        ReleaseStepStatus::Missing => ExecutionStatus::Missing,
    }
}

fn release_artifacts_from_run(run_id: &str, steps: &[ReleaseStepResult]) -> Vec<ChangeArtifact> {
    steps
        .iter()
        .flat_map(|step| release_artifacts_from_step(run_id, step))
        .collect()
}

fn release_artifacts_from_step(run_id: &str, step: &ReleaseStepResult) -> Vec<ChangeArtifact> {
    if step.id == "changelog.finalize" {
        return changelog_artifact_from_step(run_id, step)
            .into_iter()
            .collect();
    }
    if step.id == "version" {
        return version_artifacts_from_step(run_id, step);
    }

    let Some(data) = step.data.as_ref() else {
        return Vec::new();
    };

    let Ok(artifacts) = serde_json::from_value::<Vec<ReleaseArtifact>>(data.clone()) else {
        return Vec::new();
    };

    artifacts
        .into_iter()
        .enumerate()
        .map(|(index, artifact)| {
            let ReleaseArtifact {
                path: release_artifact_path,
                durable_path: _,
                artifact_type,
                platform,
            } = artifact;
            let id = format!("{}.artifact.{}", step.id, index + 1);
            let artifact_type = artifact_type.unwrap_or_else(|| "release_artifact".to_string());
            let mut metadata = HashMap::new();
            if let Some(platform) = platform {
                metadata.insert("platform".to_string(), serde_json::Value::String(platform));
            }

            ChangeArtifact {
                id,
                artifact_type,
                provenance: ChangeArtifactProvenance {
                    source: "release".to_string(),
                    run_id: Some(run_id.to_string()),
                    step_id: Some(step.id.clone()),
                    command: None,
                    captured_at: None,
                },
                title: None,
                summary: None,
                path: Some(release_artifact_path),
                files: Vec::new(),
                diff: None,
                approval_scope: None,
                metadata,
            }
        })
        .collect()
}

fn version_artifacts_from_step(run_id: &str, step: &ReleaseStepResult) -> Vec<ChangeArtifact> {
    let Some(targets) = step
        .data
        .as_ref()
        .and_then(|data| data.get("targets"))
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };
    let generated_files = step
        .data
        .as_ref()
        .and_then(|data| data.get("generated_files"))
        .and_then(serde_json::Value::as_array)
        .map(|files| {
            files
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<std::collections::BTreeSet<_>>()
        })
        .unwrap_or_default();
    let release_lockfiles = step
        .data
        .as_ref()
        .and_then(|data| data.get("release_lockfiles"))
        .and_then(serde_json::Value::as_array)
        .map(|files| {
            files
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<std::collections::BTreeSet<_>>()
        })
        .unwrap_or_default();

    let mut files = targets
        .iter()
        .filter_map(|target| target.get("file").and_then(serde_json::Value::as_str))
        .flat_map(|file| {
            let mut files = vec![file.to_string()];
            files.extend(
                super::planning_worktree::derived_release_lockfiles(file)
                    .into_iter()
                    .filter(|lockfile| generated_files.contains(lockfile.as_str())),
            );
            files
        })
        .chain(
            release_lockfiles
                .iter()
                .filter(|file| generated_files.contains(*file))
                .map(|file| (*file).to_string()),
        )
        .collect::<Vec<_>>();
    files.sort();
    files.dedup();

    files
        .into_iter()
        .enumerate()
        .map(|(index, file)| ChangeArtifact {
            id: format!("{}.artifact.{}", step.id, index + 1),
            artifact_type: "generated_file".to_string(),
            provenance: ChangeArtifactProvenance {
                source: "release".to_string(),
                run_id: Some(run_id.to_string()),
                step_id: Some(step.id.clone()),
                command: None,
                captured_at: None,
            },
            title: Some("Generated release version metadata".to_string()),
            summary: Some("Homeboy release-generated version or lockfile mutation".to_string()),
            path: Some(file.clone()),
            files: vec![file],
            diff: None,
            approval_scope: None,
            metadata: HashMap::new(),
        })
        .collect()
}

/// Project the release-owned generated file from the changelog lifecycle step.
/// This makes its origin available after the release process exits through the
/// durable execution artifact contract, rather than an in-memory guard value.
fn changelog_artifact_from_step(run_id: &str, step: &ReleaseStepResult) -> Option<ChangeArtifact> {
    let result = serde_json::from_value::<ChangelogValidationResult>(step.data.clone()?).ok()?;
    if !result.changelog_changed {
        return None;
    }

    Some(ChangeArtifact {
        id: format!("{}.artifact.1", step.id),
        artifact_type: "generated_file".to_string(),
        provenance: ChangeArtifactProvenance {
            source: "release".to_string(),
            run_id: Some(run_id.to_string()),
            step_id: Some(step.id.clone()),
            command: None,
            captured_at: None,
        },
        title: Some("Generated changelog".to_string()),
        summary: Some("Homeboy release-generated changelog mutation".to_string()),
        path: Some(result.changelog_path.clone()),
        files: vec![result.changelog_path],
        diff: None,
        approval_scope: None,
        metadata: HashMap::new(),
    })
}

fn release_artifact_ids(step_id: &str, data: Option<&serde_json::Value>) -> Vec<String> {
    let Some(data) = data else {
        return Vec::new();
    };

    let Ok(artifacts) = serde_json::from_value::<Vec<ReleaseArtifact>>(data.clone()) else {
        return Vec::new();
    };

    artifacts
        .iter()
        .enumerate()
        .map(|(index, _)| format!("{}.artifact.{}", step_id, index + 1))
        .collect()
}

#[cfg(test)]
mod tests {
    use homeboy_core::execution::{ExecutionMode, ExecutionRun, ExecutionStatus};

    use super::super::types::{ReleaseRun, ReleaseRunResult, ReleaseStepResult, ReleaseStepStatus};

    #[test]
    fn release_run_projects_into_execution_run() {
        let release = ReleaseRun {
            component_id: "homeboy".to_string(),
            enabled: true,
            result: ReleaseRunResult {
                steps: vec![ReleaseStepResult {
                    id: "package".to_string(),
                    step_type: "release.package".to_string(),
                    status: ReleaseStepStatus::Success,
                    data: Some(serde_json::json!([
                        {
                            "path": "artifacts/homeboy.tar.gz",
                            "artifact_type": "archive",
                            "platform": "darwin"
                        }
                    ])),
                    ..Default::default()
                }],
                status: ReleaseStepStatus::Success,
                warnings: vec!["signed artifact missing".to_string()],
                summary: None,
                phase_timings: None,
            },
        };

        let execution = ExecutionRun::from(&release);

        assert_eq!(execution.id, "release.homeboy");
        assert_eq!(execution.mode, ExecutionMode::Execute);
        assert_eq!(execution.status, ExecutionStatus::Success);
        assert_eq!(execution.steps[0].status, ExecutionStatus::Success);
        assert_eq!(execution.steps[0].artifacts, vec!["package.artifact.1"]);
        assert_eq!(execution.artifacts.len(), 1);
        assert_eq!(execution.artifacts[0].artifact_type, "archive");
        assert_eq!(
            execution.artifacts[0].path.as_deref(),
            Some("artifacts/homeboy.tar.gz")
        );
        assert_eq!(execution.artifacts[0].provenance.source, "release");
        assert_eq!(execution.warnings, vec!["signed artifact missing"]);
    }

    #[test]
    fn changelog_finalize_projects_durable_generated_file_provenance() {
        let release = ReleaseRun {
            component_id: "homeboy".to_string(),
            enabled: true,
            result: ReleaseRunResult {
                steps: vec![ReleaseStepResult {
                    id: "changelog.finalize".to_string(),
                    step_type: "changelog.finalize".to_string(),
                    status: ReleaseStepStatus::Success,
                    data: Some(serde_json::json!({
                        "changelog_path": "CHANGELOG.md",
                        "changelog_finalized": true,
                        "changelog_changed": true
                    })),
                    ..Default::default()
                }],
                status: ReleaseStepStatus::Success,
                warnings: Vec::new(),
                summary: None,
                phase_timings: None,
            },
        };

        let execution = ExecutionRun::from(&release);

        assert_eq!(execution.artifacts.len(), 1);
        let artifact = &execution.artifacts[0];
        assert_eq!(artifact.artifact_type, "generated_file");
        assert_eq!(artifact.files, vec!["CHANGELOG.md"]);
        assert_eq!(artifact.provenance.source, "release");
        assert_eq!(
            artifact.provenance.run_id.as_deref(),
            Some("release.homeboy")
        );
        assert_eq!(
            artifact.provenance.step_id.as_deref(),
            Some("changelog.finalize")
        );
        assert!(
            super::super::changelog::generated_file_mutation_is_authorized(Some(
                &artifact.provenance
            ))
        );
    }

    #[test]
    fn version_step_projects_version_targets_and_derived_lockfiles() {
        let release = ReleaseRun {
            component_id: "component".to_string(),
            enabled: true,
            result: ReleaseRunResult {
                steps: vec![ReleaseStepResult {
                    id: "version".to_string(),
                    step_type: "version".to_string(),
                    status: ReleaseStepStatus::Success,
                    data: Some(serde_json::json!({
                        "targets": [{"file": "plugin.php"}, {"file": "package.json"}],
                        "generated_files": ["plugin.php", "package.json", "package-lock.json"]
                    })),
                    ..Default::default()
                }],
                status: ReleaseStepStatus::Success,
                warnings: Vec::new(),
                summary: None,
                phase_timings: None,
            },
        };

        let execution = ExecutionRun::from(&release);
        let files = execution
            .artifacts
            .iter()
            .flat_map(|artifact| artifact.files.iter().cloned())
            .collect::<Vec<_>>();

        assert_eq!(
            files,
            vec!["package-lock.json", "package.json", "plugin.php"]
        );
        assert!(execution.artifacts.iter().all(|artifact| {
            super::super::changelog::generated_file_mutation_is_authorized_for(
                Some(&artifact.provenance),
                "version",
            )
        }));
    }
}
