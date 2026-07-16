use chrono::Utc;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use uuid::Uuid;

use crate::agent_task_lifecycle::{self, AgentTaskRunState};
use crate::agent_task_schedule::AgentTaskPlan;
use crate::{paths, Error, Result};

mod types;

pub use types::*;

/// Child runs inherit the current run-scoped notification route at submission.
pub fn submit_plan_batch(
    plan: &AgentTaskPlan,
    requested_batch_id: Option<&str>,
) -> Result<AgentTaskBatchRecord> {
    submit_plan_batch_with(plan, requested_batch_id, |child_plan, run_id| {
        agent_task_lifecycle::submit_plan(child_plan, Some(run_id))
    })
}

fn submit_plan_batch_with<F>(
    plan: &AgentTaskPlan,
    requested_batch_id: Option<&str>,
    mut submit_child: F,
) -> Result<AgentTaskBatchRecord>
where
    F: FnMut(&AgentTaskPlan, &str) -> Result<crate::agent_task_lifecycle::AgentTaskRunRecord>,
{
    if plan.tasks.is_empty() {
        return Err(Error::validation_invalid_argument(
            "input",
            "agent-task batch requires at least one task",
            None,
            None,
        ));
    }
    if !plan.output_dependencies.is_empty() {
        return Err(Error::validation_invalid_argument(
            "input",
            "agent-task batch submit supports independent tasks; use fanout submit/run-plan for dependent workflow plans",
            Some(plan.plan_id.clone()),
            None,
        ));
    }

    let batch_id = requested_batch_id
        .map(sanitize_id)
        .unwrap_or_else(|| format!("agent-task-batch-{}", Uuid::new_v4()));
    let mut child_run_ids = HashSet::new();
    let child_run_ids = plan
        .tasks
        .iter()
        .map(|task| {
            let child_run_id = child_run_id(&batch_id, &task.task_id);
            if !child_run_ids.insert(child_run_id.clone()) {
                return Err(Error::validation_invalid_argument(
                    "task_id",
                    format!(
                        "agent-task batch child run id '{}' is duplicated after sanitizing task ids",
                        child_run_id
                    ),
                    Some(task.task_id.clone()),
                    None,
                ));
            }
            if agent_task_lifecycle::run_record_exists(&child_run_id)? {
                return Err(Error::validation_invalid_argument(
                    "batch_id",
                    format!(
                        "agent-task batch child run id '{}' already exists; choose a different batch id",
                        child_run_id
                    ),
                    Some(batch_id.clone()),
                    None,
                ));
            }
            Ok(child_run_id)
        })
        .collect::<Result<Vec<_>>>()?;

    // Persist the batch boundary before creating children. A later submission
    // failure must still leave an inspectable batch identity for recovery.
    let mut record = AgentTaskBatchRecord {
        schema: AGENT_TASK_BATCH_SCHEMA.to_string(),
        batch_id,
        plan_id: plan.plan_id.clone(),
        state: AgentTaskBatchState::Queued,
        submitted_at: now_timestamp(),
        updated_at: None,
        task_count: plan.tasks.len(),
        child_runs: plan
            .tasks
            .iter()
            .zip(&child_run_ids)
            .map(|(task, run_id)| AgentTaskBatchChildRun {
                task_id: task.task_id.clone(),
                run_id: run_id.clone(),
                state: AgentTaskRunState::Queued,
            })
            .collect(),
        metadata: batch_metadata(plan),
    };
    write_batch(&record)?;

    for (index, task) in plan.tasks.iter().enumerate() {
        let child_run_id = child_run_ids[index].clone();
        let child_plan = child_plan(plan, task.clone(), &record.batch_id);
        let child_record = submit_child(&child_plan, &child_run_id)?;
        let child = &mut record.child_runs[index];
        child.run_id = child_record.run_id;
        child.state = child_record.state;
        record.updated_at = Some(now_timestamp());
        write_batch(&record)?;
    }

    Ok(record)
}

pub fn status(batch_id: &str) -> Result<AgentTaskBatchStatusReport> {
    let mut batch = read_batch(batch_id)?;
    let mut changed = false;
    let mut unavailable_child_runs = Vec::new();
    for child in &mut batch.child_runs {
        match agent_task_lifecycle::status(&child.run_id) {
            Ok(record) => {
                if child.state != record.state {
                    child.state = record.state;
                    changed = true;
                }
            }
            Err(error) => {
                unavailable_child_runs.push(child_issue(
                    child,
                    format!("unable to read child run status: {}", error.message),
                ));
            }
        }
    }
    let mut totals = totals_for_children(&batch.child_runs);
    totals.unavailable = unavailable_child_runs.len();
    let state = state_for_totals(&totals);
    if batch.state != state {
        batch.state = state;
        changed = true;
    }
    if changed {
        batch.updated_at = Some(now_timestamp());
        write_batch(&batch)?;
    }

    Ok(AgentTaskBatchStatusReport {
        schema: AGENT_TASK_BATCH_STATUS_SCHEMA,
        commands: commands(&batch.batch_id),
        next_actions: next_actions(&unavailable_child_runs),
        batch,
        totals,
        unavailable_child_runs,
    })
}

pub fn artifacts(batch_id: &str) -> Result<AgentTaskBatchArtifactsReport> {
    let report = status(batch_id)?;
    let mut unavailable_child_runs = report.unavailable_child_runs.clone();
    let child_runs = report
        .batch
        .child_runs
        .into_iter()
        .filter_map(
            |child| match agent_task_lifecycle::artifacts(&child.run_id) {
                Ok(artifacts) => {
                    let artifact_count = artifacts.artifacts.len();
                    let evidence_ref_count = artifacts.evidence_refs.len();
                    Some(Ok(AgentTaskBatchChildArtifacts {
                        task_id: child.task_id,
                        run_id: child.run_id,
                        state: child.state,
                        artifact_count,
                        evidence_ref_count,
                        artifacts,
                    }))
                }
                Err(error) => {
                    if !unavailable_child_runs
                        .iter()
                        .any(|issue| issue.run_id == child.run_id)
                    {
                        unavailable_child_runs.push(child_issue(
                            &child,
                            format!("unable to read child run artifacts: {}", error.message),
                        ));
                    }
                    None
                }
            },
        )
        .collect::<Result<Vec<_>>>()?;
    let manifest = artifacts_manifest(&child_runs);
    let summary = AgentTaskBatchArtifactsSummary {
        child_runs: child_runs.len(),
        artifacts: manifest.artifacts.len(),
        evidence_refs: manifest.evidence_refs.len(),
    };

    Ok(AgentTaskBatchArtifactsReport {
        schema: AGENT_TASK_BATCH_ARTIFACTS_SCHEMA,
        batch_id: report.batch.batch_id,
        summary,
        manifest,
        next_actions: next_actions(&unavailable_child_runs),
        unavailable_child_runs,
        child_runs,
    })
}

fn artifacts_manifest(
    children: &[AgentTaskBatchChildArtifacts],
) -> AgentTaskBatchArtifactsManifest {
    let mut manifest = AgentTaskBatchArtifactsManifest::default();
    for child in children {
        for artifact in &child.artifacts.artifacts {
            manifest.artifacts.push(AgentTaskBatchArtifactEntry {
                task_id: child.task_id.clone(),
                run_id: child.run_id.clone(),
                state: child.state,
                artifact: artifact.clone(),
            });
        }
        for evidence_ref in &child.artifacts.evidence_refs {
            manifest.evidence_refs.push(AgentTaskBatchEvidenceRefEntry {
                task_id: child.task_id.clone(),
                run_id: child.run_id.clone(),
                state: child.state,
                evidence_ref: evidence_ref.clone(),
            });
        }
    }
    manifest
}

fn child_plan(
    source: &AgentTaskPlan,
    mut task: crate::agent_task::AgentTaskRequest,
    batch_id: &str,
) -> AgentTaskPlan {
    let task_id = task.task_id.clone();
    task.parent_plan_id
        .get_or_insert_with(|| batch_id.to_string());
    let mut metadata = match task.metadata {
        Value::Object(object) => object,
        Value::Null => serde_json::Map::new(),
        other => {
            let mut object = serde_json::Map::new();
            object.insert("base".to_string(), other);
            object
        }
    };
    metadata.insert("batch_id".to_string(), json!(batch_id));
    task.metadata = Value::Object(metadata);

    let mut child = AgentTaskPlan::new(format!("{}/{}", source.plan_id, task.task_id), vec![task]);
    child.group_key = source
        .group_key
        .clone()
        .or_else(|| Some(batch_id.to_string()));
    child.component_contracts = source.component_contracts.clone();
    if let Some(outputs) = source.artifact_outputs.get(&task_id) {
        child.artifact_outputs.insert(task_id, outputs.clone());
    }
    child.options = source.options.clone();
    child.options.max_concurrency = 1;
    child.metadata = json!({
        "batch_id": batch_id,
        "parent_plan_id": source.plan_id,
    });
    child.rebuild_homeboy_plan();
    child
}

fn batch_metadata(plan: &AgentTaskPlan) -> Value {
    json!({
        "parent_plan_id": plan.plan_id,
        "group_key": plan.group_key,
        "durable_child_runs": true,
    })
}

fn totals_for_children(children: &[AgentTaskBatchChildRun]) -> AgentTaskBatchTotals {
    let mut totals = AgentTaskBatchTotals::default();
    for child in children {
        match child.state {
            AgentTaskRunState::Queued => totals.queued += 1,
            AgentTaskRunState::Running => totals.running += 1,
            AgentTaskRunState::Succeeded => totals.succeeded += 1,
            AgentTaskRunState::CandidateRecoverable => totals.partial_failure += 1,
            AgentTaskRunState::PartialRecoverable => totals.partial_failure += 1,
            AgentTaskRunState::PartialFailure => totals.partial_failure += 1,
            AgentTaskRunState::Failed => totals.failed += 1,
            AgentTaskRunState::Cancelled => totals.cancelled += 1,
        }
    }
    totals
}

fn state_for_totals(totals: &AgentTaskBatchTotals) -> AgentTaskBatchState {
    if totals.running > 0 {
        AgentTaskBatchState::Running
    } else if totals.queued > 0 {
        AgentTaskBatchState::Queued
    } else if totals.unavailable > 0 {
        AgentTaskBatchState::PartialFailure
    } else if totals.failed > 0 || totals.partial_failure > 0 {
        AgentTaskBatchState::PartialFailure
    } else if totals.cancelled > 0 && totals.succeeded == 0 {
        AgentTaskBatchState::Cancelled
    } else if totals.cancelled > 0 {
        AgentTaskBatchState::PartialFailure
    } else {
        AgentTaskBatchState::Succeeded
    }
}

fn child_issue(child: &AgentTaskBatchChildRun, problem: String) -> AgentTaskBatchChildIssue {
    AgentTaskBatchChildIssue {
        task_id: child.task_id.clone(),
        run_id: child.run_id.clone(),
        last_known_state: Some(child.state),
        status_command: format!("homeboy agent-task status {}", child.run_id),
        artifacts_command: format!("homeboy agent-task artifacts {}", child.run_id),
        problem,
    }
}

fn next_actions(unavailable_child_runs: &[AgentTaskBatchChildIssue]) -> Vec<String> {
    if unavailable_child_runs.is_empty() {
        Vec::new()
    } else {
        vec![
            "partial results only: one or more child runs could not be read from the durable run store".to_string(),
            "inspect unavailable_child_runs for child run ids, last known states, status commands, artifacts commands, and error details".to_string(),
            "if a Lab runner daemon restarted, reconcile runner-side jobs/artifacts before treating the fanout as terminal".to_string(),
        ]
    }
}

fn commands(batch_id: &str) -> AgentTaskBatchCommands {
    AgentTaskBatchCommands {
        status: format!("homeboy agent-task fanout status {batch_id}"),
        artifacts: format!("homeboy agent-task fanout artifacts {batch_id}"),
        run_next: "homeboy agent-task run-next".to_string(),
    }
}

fn child_run_id(batch_id: &str, task_id: &str) -> String {
    sanitize_id(&format!("{batch_id}-{task_id}"))
}

fn sanitize_id(value: &str) -> String {
    let sanitized = paths::sanitize_path_segment(value);
    if sanitized.is_empty() {
        format!("agent-task-batch-{}", Uuid::new_v4())
    } else {
        sanitized
    }
}

fn now_timestamp() -> String {
    Utc::now().to_rfc3339()
}

fn batch_path(batch_id: &str) -> Result<PathBuf> {
    Ok(paths::homeboy_data()?
        .join("agent-task-batches")
        .join(format!("{}.json", sanitize_id(batch_id))))
}

fn write_batch(record: &AgentTaskBatchRecord) -> Result<()> {
    let path = batch_path(&record.batch_id)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            Error::internal_io(error.to_string(), Some(parent.display().to_string()))
        })?;
    }
    let raw = serde_json::to_string_pretty(record).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some(format!("serialize agent-task batch {}", record.batch_id)),
        )
    })?;
    fs::write(&path, raw)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))
}

fn read_batch(batch_id: &str) -> Result<AgentTaskBatchRecord> {
    let path = batch_path(batch_id)?;
    let raw = fs::read_to_string(&path)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    serde_json::from_str(&raw).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some(format!("parse agent-task batch {}", batch_id)),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_task::{
        AgentTaskArtifact, AgentTaskEvidenceRef, AgentTaskExecutor, AgentTaskOutcome,
        AgentTaskOutcomeStatus, AgentTaskPolicy, AgentTaskRequest, AgentTaskWorkspace,
        AGENT_TASK_ARTIFACT_SCHEMA, AGENT_TASK_OUTCOME_SCHEMA, AGENT_TASK_REQUEST_SCHEMA,
    };
    use crate::agent_task_scheduler::{AgentTaskExecutionContext, AgentTaskExecutorAdapter};
    use crate::agent_task_service;
    use std::collections::HashMap;

    #[test]
    fn batch_submit_persists_parent_and_child_durable_runs() {
        // Hold the process-wide home guard (isolated HOME/XDG_DATA_HOME under
        // the shared lock) so these tests serialize against every other module
        // that mutates the same env — a module-local lock only ordered this
        // module's own tests and raced `with_isolated_home` users elsewhere.
        let _home = crate::test_support::HomeGuard::new();
        let plan = AgentTaskPlan::new("fanout/audit", vec![request("a"), request("b")]);

        let batch = submit_plan_batch(&plan, Some("batch/audit")).expect("batch submitted");

        assert_eq!(batch.batch_id, "batch_audit");
        assert_eq!(batch.state, AgentTaskBatchState::Queued);
        assert_eq!(batch.child_runs.len(), 2);
        assert_eq!(batch.child_runs[0].run_id, "batch_audit-a");
        assert!(agent_task_lifecycle::run_record_exists("batch_audit-a").expect("child exists"));
        assert!(agent_task_lifecycle::run_record_exists("batch_audit-b").expect("child exists"));

        let status = status("batch/audit").expect("batch status");
        assert_eq!(status.totals.queued, 2);
        assert_eq!(status.commands.run_next, "homeboy agent-task run-next");
    }

    #[test]
    fn batch_identity_is_persisted_before_the_first_child_submission() {
        let _home = crate::test_support::HomeGuard::new();
        let plan = AgentTaskPlan::new("fanout/pre-submit", vec![request("a"), request("b")]);
        let mut observed = false;

        let batch = submit_plan_batch_with(&plan, Some("batch/pre-submit"), |child, run_id| {
            let persisted = read_batch("batch/pre-submit").expect("persisted batch identity");
            assert_eq!(persisted.batch_id, "batch_pre-submit");
            assert_eq!(persisted.child_runs.len(), 2);
            assert!(persisted
                .child_runs
                .iter()
                .all(|child| child.state == AgentTaskRunState::Queued));
            observed = true;
            agent_task_lifecycle::submit_plan(child, Some(run_id))
        })
        .expect("batch submitted");

        assert!(observed);
        assert_eq!(batch.child_runs.len(), 2);
    }

    #[test]
    fn batch_children_inherit_the_scoped_notification_route() {
        // Hold the process-wide home guard (isolated HOME/XDG_DATA_HOME under
        // the shared lock) so these tests serialize against every other module
        // that mutates the same env — a module-local lock only ordered this
        // module's own tests and raced `with_isolated_home` users elsewhere.
        let _home = crate::test_support::HomeGuard::new();
        let plan = AgentTaskPlan::new("fanout/routes", vec![request("a"), request("b")]);
        let route =
            crate::notification_route::NotificationRoute::new("extension", "opaque-parent-route")
                .expect("route");

        let batch = crate::notification_route::with_current(Some(route), || {
            submit_plan_batch(&plan, Some("batch-routes")).expect("batch submitted")
        });

        for child in batch.child_runs {
            let record = agent_task_lifecycle::status(&child.run_id).expect("child record");
            assert_eq!(
                record.metadata["notification_route"]["route"],
                "opaque-parent-route"
            );
        }
    }

    #[test]
    fn batch_status_returns_partial_envelope_when_child_record_is_unavailable() {
        // Hold the process-wide home guard (isolated HOME/XDG_DATA_HOME under
        // the shared lock) so these tests serialize against every other module
        // that mutates the same env — a module-local lock only ordered this
        // module's own tests and raced `with_isolated_home` users elsewhere.
        let _home = crate::test_support::HomeGuard::new();
        let plan = AgentTaskPlan::new("fanout/restart", vec![request("a")]);
        submit_plan_batch(&plan, Some("batch/restart")).expect("batch submitted");
        let mut batch = read_batch("batch/restart").expect("batch record");
        batch.child_runs.push(AgentTaskBatchChildRun {
            task_id: "orphan".to_string(),
            run_id: "batch_restart-orphan".to_string(),
            state: AgentTaskRunState::Running,
        });
        batch.task_count = batch.child_runs.len();
        write_batch(&batch).expect("batch rewritten with orphan child");

        let report = status("batch/restart").expect("partial batch status");

        assert_eq!(report.batch.state, AgentTaskBatchState::Running);
        assert_eq!(report.totals.queued, 1);
        assert_eq!(report.totals.running, 1);
        assert_eq!(report.totals.unavailable, 1);
        assert_eq!(report.unavailable_child_runs.len(), 1);
        let issue = &report.unavailable_child_runs[0];
        assert_eq!(issue.task_id, "orphan");
        assert_eq!(issue.run_id, "batch_restart-orphan");
        assert_eq!(issue.last_known_state, Some(AgentTaskRunState::Running));
        assert!(issue.problem.contains("unable to read child run status"));
        assert_eq!(
            issue.status_command,
            "homeboy agent-task status batch_restart-orphan"
        );
        assert!(report
            .next_actions
            .iter()
            .any(|action| action.contains("partial results only")));
    }

    #[test]
    fn batch_artifacts_report_exposes_stable_manifest_and_counts() {
        // Hold the process-wide home guard (isolated HOME/XDG_DATA_HOME under
        // the shared lock) so these tests serialize against every other module
        // that mutates the same env — a module-local lock only ordered this
        // module's own tests and raced `with_isolated_home` users elsewhere.
        let _home = crate::test_support::HomeGuard::new();
        let plan = AgentTaskPlan::new("fanout/artifacts", vec![request("a"), request("b")]);
        submit_plan_batch(&plan, Some("batch/artifacts")).expect("batch submitted");
        agent_task_service::run_submitted("batch_artifacts-a".to_string(), ArtifactExecutor)
            .expect("first child run");
        agent_task_service::run_submitted("batch_artifacts-b".to_string(), ArtifactExecutor)
            .expect("second child run");

        let report = artifacts("batch/artifacts").expect("batch artifacts");

        assert_eq!(report.schema, AGENT_TASK_BATCH_ARTIFACTS_SCHEMA);
        assert_eq!(report.summary.child_runs, 2);
        assert_eq!(report.summary.artifacts, 2);
        assert_eq!(report.summary.evidence_refs, 8);
        assert_eq!(report.child_runs[0].artifact_count, 1);
        assert_eq!(report.child_runs[0].evidence_ref_count, 4);
        assert_eq!(report.manifest.artifacts[0].task_id, "a");
        assert_eq!(report.manifest.artifacts[0].run_id, "batch_artifacts-a");
        assert_eq!(report.manifest.artifacts[0].artifact.id, "artifact-a");
        assert_eq!(report.manifest.evidence_refs[4].task_id, "b");
        assert_eq!(
            report.manifest.evidence_refs[4].evidence_ref.kind,
            "executor-log"
        );
    }

    #[test]
    fn batch_artifacts_preserves_available_refs_when_child_record_is_unavailable() {
        // Hold the process-wide home guard (isolated HOME/XDG_DATA_HOME under
        // the shared lock) so these tests serialize against every other module
        // that mutates the same env — a module-local lock only ordered this
        // module's own tests and raced `with_isolated_home` users elsewhere.
        let _home = crate::test_support::HomeGuard::new();
        let plan = AgentTaskPlan::new("fanout/artifacts-partial", vec![request("a")]);
        submit_plan_batch(&plan, Some("batch/artifacts-partial")).expect("batch submitted");
        agent_task_service::run_submitted(
            "batch_artifacts-partial-a".to_string(),
            ArtifactExecutor,
        )
        .expect("available child run");
        let mut batch = read_batch("batch/artifacts-partial").expect("batch record");
        batch.child_runs.push(AgentTaskBatchChildRun {
            task_id: "orphan".to_string(),
            run_id: "batch_artifacts-partial-orphan".to_string(),
            state: AgentTaskRunState::Running,
        });
        batch.task_count = batch.child_runs.len();
        write_batch(&batch).expect("batch rewritten with orphan child");

        let report = artifacts("batch/artifacts-partial").expect("partial batch artifacts");

        assert_eq!(report.summary.child_runs, 1);
        assert_eq!(report.summary.artifacts, 1);
        assert_eq!(report.summary.evidence_refs, 4);
        assert_eq!(
            report.manifest.artifacts[0].run_id,
            "batch_artifacts-partial-a"
        );
        assert_eq!(report.child_runs[0].run_id, "batch_artifacts-partial-a");
        assert_eq!(report.unavailable_child_runs.len(), 1);
        assert_eq!(report.unavailable_child_runs[0].task_id, "orphan");
        assert_eq!(
            report.unavailable_child_runs[0].artifacts_command,
            "homeboy agent-task artifacts batch_artifacts-partial-orphan"
        );
        assert!(report
            .next_actions
            .iter()
            .any(|action| action.contains("runner daemon restarted")));
    }

    #[test]
    fn batch_submit_rejects_dependent_workflow_plans() {
        // Hold the process-wide home guard (isolated HOME/XDG_DATA_HOME under
        // the shared lock) so these tests serialize against every other module
        // that mutates the same env — a module-local lock only ordered this
        // module's own tests and raced `with_isolated_home` users elsewhere.
        let _home = crate::test_support::HomeGuard::new();
        let mut plan = AgentTaskPlan::new("workflow", vec![request("a"), request("b")]);
        plan.output_dependencies.insert(
            "b".to_string(),
            crate::agent_task_schedule::AgentTaskOutputDependencies {
                depends_on: vec!["a".to_string()],
                bindings: HashMap::new(),
            },
        );

        let error = submit_plan_batch(&plan, Some("workflow")).expect_err("workflow rejected");

        assert!(error.message.contains("independent tasks"));
    }

    fn request(task_id: &str) -> AgentTaskRequest {
        AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: task_id.to_string(),
            group_key: None,
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "test".to_string(),
                selector: None,
                runtime_selection: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                model: None,
                config: Value::Null,
            },
            instructions: "do it".to_string(),
            inputs: Value::Null,
            source_refs: Vec::new(),
            workspace: AgentTaskWorkspace::default(),
            component_contracts: Vec::new(),
            policy: AgentTaskPolicy::default(),
            limits: Default::default(),
            expected_artifacts: Vec::new(),
            artifact_declarations: Vec::new(),
            metadata: Value::Null,
        }
    }

    struct ArtifactExecutor;

    impl AgentTaskExecutorAdapter for ArtifactExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            AgentTaskOutcome {
                schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: request.task_id.clone(),
                status: AgentTaskOutcomeStatus::Succeeded,
                summary: Some("ok".to_string()),
                failure_classification: None,
                artifacts: vec![AgentTaskArtifact {
                    schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                    id: format!("artifact-{}", request.task_id),
                    kind: "report".to_string(),
                    name: Some("report.json".to_string()),
                    label: Some("Report".to_string()),
                    role: Some("report".to_string()),
                    semantic_key: Some("agent_task.report".to_string()),
                    path: Some(format!("artifacts/{}/report.json", request.task_id)),
                    url: None,
                    mime: Some("application/json".to_string()),
                    size_bytes: Some(12),
                    sha256: None,
                    metadata: Value::Null,
                }],
                typed_artifacts: Vec::new(),
                evidence_refs: vec![AgentTaskEvidenceRef {
                    kind: "executor-log".to_string(),
                    uri: format!("homeboy://agent-task/evidence/{}", request.task_id),
                    label: Some("Executor log".to_string()),
                }],
                diagnostics: Vec::new(),
                outputs: Value::Null,
                workflow: None,
                follow_up: None,
                metadata: Value::Null,
            }
        }
    }
}
