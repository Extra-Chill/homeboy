use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::component::Component;
use crate::error::{Error, Result};
use crate::project::{
    component_local_path_blockers, load, resolve_project_components, Project,
    ProjectComponentAttachment,
};

use super::{
    attach_discovered_component_path, clear_component_attachments, project_component_ids,
    remove_components, set_component_attachments,
};

#[derive(Debug, Clone, Serialize)]
pub struct ProjectComponentsOutput {
    pub action: String,
    pub project_id: String,
    pub component_ids: Vec<String>,
    pub component_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attached_component_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attached_path: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<Component>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch: Option<BatchComponentAttachmentOutput>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum BatchComponentAttachmentFailurePolicy {
    #[default]
    Continue,
    FailFast,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum BatchComponentAttachmentWorktreePolicy {
    #[default]
    Include,
    Skip,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchComponentAttachmentInput {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchComponentAttachmentItemStatus {
    Attached,
    Skipped,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct BatchComponentAttachmentError {
    pub code: String,
    pub message: String,
    pub details: serde_json::Value,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<String>,
    pub action: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BatchComponentAttachmentItem {
    pub status: BatchComponentAttachmentItemStatus,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    pub attempted_command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<BatchComponentAttachmentError>,
    pub evidence_ref: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BatchComponentAttachmentOutput {
    pub failure_policy: BatchComponentAttachmentFailurePolicy,
    pub worktree_policy: BatchComponentAttachmentWorktreePolicy,
    pub attached_count: usize,
    pub skipped_count: usize,
    pub failed_count: usize,
    pub exit_code: i32,
    pub summary: Vec<String>,
    pub summary_truncated: bool,
    pub items: Vec<BatchComponentAttachmentItem>,
}

pub fn list_components(project_id: &str) -> Result<ProjectComponentsOutput> {
    let project = load(project_id)?;
    build_components_output(project_id, "list", &project)
}

pub fn set_components(project_id: &str, json_spec: &str) -> Result<ProjectComponentsOutput> {
    let raw = crate::config::read_json_spec_to_string(json_spec)?;
    let attachments: Vec<ProjectComponentAttachment> = serde_json::from_str(&raw).map_err(|e| {
        Error::validation_invalid_json(
            e,
            Some("parse project component attachments".to_string()),
            None,
        )
    })?;

    set_component_attachments(project_id, attachments)?;
    let project = load(project_id)?;
    build_components_output(project_id, "set", &project)
}

pub fn attach_component_path_report(
    project_id: &str,
    local_path: &Path,
) -> Result<ProjectComponentsOutput> {
    let attached_component_id = attach_discovered_component_path(project_id, local_path)?;
    let project = load(project_id)?;
    build_components_summary(
        project_id,
        "attach_path",
        &project,
        Some(attached_component_id),
        Some(local_path.to_string_lossy().to_string()),
    )
}

pub fn attach_component_paths_report(
    project_id: &str,
    inputs: Vec<BatchComponentAttachmentInput>,
    failure_policy: BatchComponentAttachmentFailurePolicy,
    worktree_policy: BatchComponentAttachmentWorktreePolicy,
) -> Result<ProjectComponentsOutput> {
    if inputs.is_empty() {
        return Err(Error::validation_invalid_argument(
            "paths",
            "At least one component path is required",
            Some(project_id.to_string()),
            None,
        ));
    }

    let mut items = Vec::with_capacity(inputs.len());
    let mut stopped = false;
    for (index, input) in inputs.into_iter().enumerate() {
        let attempted_command = format!(
            "homeboy project components attach-path {} {}",
            project_id, input.path
        );
        let evidence_ref = format!("#/batch/items/{index}");
        let (status, component_id, skip_reason, error) = if stopped {
            (
                BatchComponentAttachmentItemStatus::Skipped,
                None,
                Some("not attempted after fail_fast failure".to_string()),
                None,
            )
        } else if worktree_policy == BatchComponentAttachmentWorktreePolicy::Skip
            && Path::new(&input.path).join(".git").is_file()
        {
            (
                BatchComponentAttachmentItemStatus::Skipped,
                None,
                Some("git worktree skipped by worktree_policy".to_string()),
                None,
            )
        } else {
            match attach_discovered_component_path(project_id, Path::new(&input.path)) {
                Ok(component_id) => (
                    BatchComponentAttachmentItemStatus::Attached,
                    Some(component_id),
                    None,
                    None,
                ),
                Err(error) => {
                    stopped = failure_policy == BatchComponentAttachmentFailurePolicy::FailFast;
                    let action = error
                        .hints
                        .first()
                        .map(|hint| hint.message.clone())
                        .unwrap_or_else(|| {
                            "Inspect the retained error details and repair the component path."
                                .to_string()
                        });
                    (
                        BatchComponentAttachmentItemStatus::Failed,
                        None,
                        None,
                        Some(BatchComponentAttachmentError {
                            code: error.code.as_str().to_string(),
                            message: error.message,
                            details: error.details,
                            hints: error.hints.into_iter().map(|hint| hint.message).collect(),
                            action,
                        }),
                    )
                }
            }
        };
        items.push(BatchComponentAttachmentItem {
            status,
            path: input.path,
            reference: input.reference,
            attempted_command,
            component_id,
            skip_reason,
            error,
            evidence_ref,
        });
    }

    let attached_count = items
        .iter()
        .filter(|item| matches!(item.status, BatchComponentAttachmentItemStatus::Attached))
        .count();
    let skipped_count = items
        .iter()
        .filter(|item| matches!(item.status, BatchComponentAttachmentItemStatus::Skipped))
        .count();
    let failed_count = items
        .iter()
        .filter(|item| matches!(item.status, BatchComponentAttachmentItemStatus::Failed))
        .count();
    const SUMMARY_LIMIT: usize = 8;
    let summary: Vec<_> = items
        .iter()
        .filter_map(|item| match item.status {
            BatchComponentAttachmentItemStatus::Attached => Some(format!("attached {}", item.path)),
            BatchComponentAttachmentItemStatus::Skipped => Some(format!(
                "skipped {}: {}",
                item.path,
                item.skip_reason.as_deref().unwrap_or("skipped")
            )),
            BatchComponentAttachmentItemStatus::Failed => Some(format!(
                "failed {} [{}]",
                item.path,
                item.error
                    .as_ref()
                    .map(|error| error.code.as_str())
                    .unwrap_or("unknown")
            )),
        })
        .take(SUMMARY_LIMIT)
        .collect();
    let batch = BatchComponentAttachmentOutput {
        failure_policy,
        worktree_policy,
        attached_count,
        skipped_count,
        failed_count,
        exit_code: i32::from(failed_count > 0),
        summary,
        summary_truncated: items.len() > SUMMARY_LIMIT,
        items,
    };
    let project = load(project_id).ok();
    Ok(ProjectComponentsOutput {
        action: "attach_paths".to_string(),
        project_id: project_id.to_string(),
        component_ids: project
            .as_ref()
            .map(project_component_ids)
            .unwrap_or_default(),
        component_count: project
            .as_ref()
            .map(|project| project.components.len())
            .unwrap_or_default(),
        attached_component_id: None,
        attached_path: None,
        components: Vec::new(),
        warnings: project
            .as_ref()
            .map(component_local_path_blockers)
            .unwrap_or_default(),
        batch: Some(batch),
    })
}

pub fn remove_components_report(
    project_id: &str,
    component_ids: Vec<String>,
) -> Result<ProjectComponentsOutput> {
    remove_components(project_id, component_ids)?;
    let project = load(project_id)?;
    build_components_summary(project_id, "remove", &project, None, None)
}

pub fn clear_components(project_id: &str) -> Result<ProjectComponentsOutput> {
    clear_component_attachments(project_id)?;
    let project = load(project_id)?;
    build_components_summary(project_id, "clear", &project, None, None)
}

fn build_components_output(
    project_id: &str,
    action: &str,
    project: &Project,
) -> Result<ProjectComponentsOutput> {
    let components = resolve_project_components(project)?;

    Ok(ProjectComponentsOutput {
        action: action.to_string(),
        project_id: project_id.to_string(),
        component_ids: project_component_ids(project),
        component_count: project.components.len(),
        attached_component_id: None,
        attached_path: None,
        components,
        warnings: Vec::new(),
        batch: None,
    })
}

fn build_components_summary(
    project_id: &str,
    action: &str,
    project: &Project,
    attached_component_id: Option<String>,
    attached_path: Option<String>,
) -> Result<ProjectComponentsOutput> {
    Ok(ProjectComponentsOutput {
        action: action.to_string(),
        project_id: project_id.to_string(),
        component_ids: project_component_ids(project),
        component_count: project.components.len(),
        attached_component_id,
        attached_path,
        components: Vec::new(),
        warnings: component_local_path_blockers(project),
        batch: None,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        attach_component_paths_report, build_components_summary, remove_components_report,
        BatchComponentAttachmentFailurePolicy, BatchComponentAttachmentInput,
        BatchComponentAttachmentItemStatus, BatchComponentAttachmentWorktreePolicy,
    };
    use crate::project::{load, save, Project, ProjectComponentAttachment};
    use crate::test_support::with_isolated_home;
    use std::fs;

    fn batch_input(path: impl Into<String>) -> BatchComponentAttachmentInput {
        BatchComponentAttachmentInput {
            path: path.into(),
            reference: None,
        }
    }

    fn write_component(path: &std::path::Path, id: &str) {
        fs::create_dir_all(path).expect("component directory");
        fs::write(path.join("homeboy.json"), format!(r#"{{"id":"{id}"}}"#))
            .expect("component config");
    }

    #[test]
    fn attach_path_summary_omits_resolved_component_payload() {
        let project = Project {
            id: "site".to_string(),
            components: vec![ProjectComponentAttachment {
                id: "plugin".to_string(),
                local_path: "/repo/plugin".to_string(),
                remote_path: None,
                deployment_provider: None,
            }],
            ..Default::default()
        };

        let output = build_components_summary(
            "site",
            "attach_path",
            &project,
            Some("plugin".to_string()),
            Some("/repo/plugin".to_string()),
        )
        .expect("summary output");

        assert_eq!(output.component_ids, vec!["plugin"]);
        assert_eq!(output.component_count, 1);
        assert_eq!(output.attached_component_id.as_deref(), Some("plugin"));
        assert!(output.components.is_empty());
        assert!(!output.warnings.is_empty());
    }

    #[test]
    fn remove_component_succeeds_when_unrelated_remaining_path_is_stale() {
        with_isolated_home(|_| {
            save(&Project {
                id: "site".to_string(),
                components: vec![
                    ProjectComponentAttachment {
                        id: "remove-me".to_string(),
                        local_path: "/tmp/homeboy-remove-me-missing".to_string(),
                        remote_path: None,
                        deployment_provider: None,
                    },
                    ProjectComponentAttachment {
                        id: "stale-remaining".to_string(),
                        local_path: "/tmp/homeboy-stale-remaining-missing".to_string(),
                        remote_path: None,
                        deployment_provider: None,
                    },
                ],
                ..Default::default()
            })
            .expect("save project");

            let output = remove_components_report("site", vec!["remove-me".to_string()])
                .expect("remove should not resolve unrelated stale component paths");

            assert_eq!(output.component_ids, vec!["stale-remaining"]);
            assert_eq!(output.component_count, 1);
            assert!(output.components.is_empty());
            assert!(output.warnings.iter().any(|warning| warning.contains(
                "Component 'stale-remaining' local_path '/tmp/homeboy-stale-remaining-missing' does not exist"
            )));
            assert!(!output
                .warnings
                .iter()
                .any(|warning| warning.contains("remove-me")));

            let project = load("site").expect("project still loads");
            assert_eq!(project.components.len(), 1);
            assert_eq!(project.components[0].id, "stale-remaining");
        });
    }

    #[test]
    fn batch_attachment_continues_after_failure_and_retains_diagnostics() {
        with_isolated_home(|home| {
            save(&Project {
                id: "site".to_string(),
                ..Default::default()
            })
            .expect("save project");
            let first = home.path().join("first");
            let second = home.path().join("second");
            write_component(&first, "first");
            write_component(&second, "second");
            let missing = home.path().join("missing");

            let output = attach_component_paths_report(
                "site",
                vec![
                    BatchComponentAttachmentInput {
                        path: first.to_string_lossy().to_string(),
                        reference: Some("primary:first".to_string()),
                    },
                    batch_input(missing.to_string_lossy()),
                    batch_input(second.to_string_lossy()),
                ],
                BatchComponentAttachmentFailurePolicy::Continue,
                BatchComponentAttachmentWorktreePolicy::Include,
            )
            .expect("batch output");
            let batch = output.batch.expect("batch details");

            assert_eq!(batch.attached_count, 2);
            assert_eq!(batch.failed_count, 1);
            assert_eq!(batch.skipped_count, 0);
            assert_eq!(batch.exit_code, 1);
            assert!(matches!(
                batch.items[0].status,
                BatchComponentAttachmentItemStatus::Attached
            ));
            assert_eq!(batch.items[0].reference.as_deref(), Some("primary:first"));
            assert!(matches!(
                batch.items[1].status,
                BatchComponentAttachmentItemStatus::Failed
            ));
            let error = batch.items[1].error.as_ref().expect("retained error");
            assert!(!error.code.is_empty());
            assert!(!error.action.is_empty());
            assert!(batch.items[1]
                .attempted_command
                .contains("attach-path site"));
            assert_eq!(batch.items[1].evidence_ref, "#/batch/items/1");
            assert!(matches!(
                batch.items[2].status,
                BatchComponentAttachmentItemStatus::Attached
            ));
        });
    }

    #[test]
    fn batch_attachment_fail_fast_marks_remaining_inputs_skipped() {
        with_isolated_home(|home| {
            save(&Project {
                id: "site".to_string(),
                ..Default::default()
            })
            .expect("save project");
            let later = home.path().join("later");
            write_component(&later, "later");

            let output = attach_component_paths_report(
                "site",
                vec![
                    batch_input(home.path().join("missing").to_string_lossy()),
                    batch_input(later.to_string_lossy()),
                ],
                BatchComponentAttachmentFailurePolicy::FailFast,
                BatchComponentAttachmentWorktreePolicy::Include,
            )
            .expect("batch output");
            let batch = output.batch.expect("batch details");

            assert_eq!(batch.failed_count, 1);
            assert_eq!(batch.skipped_count, 1);
            assert!(matches!(
                batch.items[1].status,
                BatchComponentAttachmentItemStatus::Skipped
            ));
            assert_eq!(
                batch.items[1].skip_reason.as_deref(),
                Some("not attempted after fail_fast failure")
            );
        });
    }

    #[test]
    fn batch_attachment_skips_git_worktrees_when_requested() {
        with_isolated_home(|home| {
            save(&Project {
                id: "site".to_string(),
                ..Default::default()
            })
            .expect("save project");
            let worktree = home.path().join("component-worktree");
            write_component(&worktree, "component-worktree");
            fs::write(
                worktree.join(".git"),
                "gitdir: /tmp/component/.git/worktrees/test\n",
            )
            .expect("worktree marker");

            let output = attach_component_paths_report(
                "site",
                vec![batch_input(worktree.to_string_lossy())],
                BatchComponentAttachmentFailurePolicy::Continue,
                BatchComponentAttachmentWorktreePolicy::Skip,
            )
            .expect("batch output");
            let batch = output.batch.expect("batch details");

            assert_eq!(batch.exit_code, 0);
            assert_eq!(batch.skipped_count, 1);
            assert!(matches!(
                batch.items[0].status,
                BatchComponentAttachmentItemStatus::Skipped
            ));
            assert_eq!(
                batch.items[0].skip_reason.as_deref(),
                Some("git worktree skipped by worktree_policy")
            );
        });
    }

    #[test]
    fn batch_attachment_bounds_summary_without_dropping_failure_evidence() {
        with_isolated_home(|home| {
            save(&Project {
                id: "site".to_string(),
                ..Default::default()
            })
            .expect("save project");
            let inputs = (0..9)
                .map(|index| {
                    batch_input(
                        home.path()
                            .join(format!("missing-{index}"))
                            .to_string_lossy(),
                    )
                })
                .collect();

            let output = attach_component_paths_report(
                "site",
                inputs,
                BatchComponentAttachmentFailurePolicy::Continue,
                BatchComponentAttachmentWorktreePolicy::Include,
            )
            .expect("batch output");
            let batch = output.batch.expect("batch details");

            assert_eq!(batch.failed_count, 9);
            assert_eq!(batch.summary.len(), 8);
            assert!(batch.summary_truncated);
            assert_eq!(batch.items.len(), 9);
            assert!(batch.items.iter().all(|item| item.error.is_some()));
            assert_eq!(batch.items[8].evidence_ref, "#/batch/items/8");
        });
    }
}
