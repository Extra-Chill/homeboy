//! Bridges between Lab offload execution and the local AgentTask lifecycle.
//!
//! - Inline `--plan` JSON arguments are materialized to a synced workspace file
//!   so the runner can resolve them remotely.
//! - Once the runner streams output back, `mirror_agent_task_run_plan_lifecycle`
//!   replays the run-plan aggregate into the controller's lifecycle store so
//!   `homeboy agent-task status/logs` keeps working transparently.
//! - The legacy dispatch-envelope parser is retained only for pre-typed runners
//!   that cannot surface the handoff through workload events/results.

use std::fs;

use crate::core::agent_task::AgentTaskEvidenceRef;
use crate::core::agent_task_lifecycle::{
    cook_attempt_run_id, record_runner_job_identity, AgentTaskArtifactRef, AgentTaskRunRecord,
    AgentTaskRunState,
};
use crate::core::agent_tasks::lifecycle as agent_task_lifecycle;
use crate::core::agent_tasks::provider::{
    dependency_failure_patterns, AgentTaskProviderDependencyFailurePattern,
};
use crate::core::agent_tasks::scheduler::{
    AgentTaskAggregate, AgentTaskAggregateTotals, AgentTaskPlan,
};
use crate::core::api_jobs::JobEvent;
#[cfg(test)]
use crate::core::api_jobs::JobEventKind;
use crate::core::artifact_manifest::ArtifactManifest;
use crate::core::engine::local_files::write_file_owner_only;
use crate::core::lab_contract::{
    AgentTaskDispatchIdentity, RunnerWorkloadAgentTask,
    RunnerWorkloadAgentTaskLifecycleMirrorPolicy,
};
use crate::core::notification_route::NotificationRoute;
use crate::core::runner::agent_task_lifecycle_event::{
    agent_task_run_plan_lifecycle_event_from_job_events, is_agent_task_run_plan_envelope,
    parse_offloaded_run_plan_envelope,
};
use crate::core::{config, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[cfg(test)]
use super::super::lab_args::materialize_inline_agent_task_json_specs_in_args;
use super::super::lab_args::AgentTaskInlineJsonSpec;
use super::super::lab_workspaces::{workspace_mapping_entry, LabWorkspaceMappingEntry};
use super::super::{sync_workspace, RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions};
use super::args_util::{subcommand_index, ArgEditor, CommandInvocation};

#[cfg(test)]
fn materialize_inline_agent_task_tasks_arg_with(
    args: &[String],
    mut sync: impl FnMut(&str) -> Result<Option<(String, LabWorkspaceMappingEntry)>>,
) -> Result<(Vec<String>, Option<LabWorkspaceMappingEntry>)> {
    let (rewritten, entries) = materialize_inline_agent_task_json_specs_in_args(args, |spec| {
        if matches!(
            spec.role,
            "agent_task_tasks_remapped" | "agent_task_attempt_plan_remapped"
        ) {
            sync(spec.spec)
        } else {
            Ok(None)
        }
    })?;
    Ok((
        rewritten,
        entries.into_iter().next().map(|entry| entry.entry),
    ))
}

pub(super) fn sync_inline_agent_task_file(
    runner_id: &str,
    spec: AgentTaskInlineJsonSpec<'_>,
) -> Result<Option<(String, LabWorkspaceMappingEntry)>> {
    serde_json::from_str::<serde_json::Value>(spec.spec).map_err(|err| {
        Error::internal_json(
            err.to_string(),
            Some("parse remapped agent-task plan".to_string()),
        )
    })?;

    let temp = tempfile::tempdir().map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("create remapped agent-task plan workspace".to_string()),
        )
    })?;
    let plan_file = temp.path().join(spec.filename);
    if spec.role == "agent_task_attempt_plan_remapped" {
        write_private_remapped_agent_task_plan(&plan_file, spec.spec)?;
    } else {
        fs::write(&plan_file, spec.spec).map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some("write remapped agent-task plan".to_string()),
            )
        })?;
    }
    let synced = sync_workspace(
        runner_id,
        RunnerWorkspaceSyncOptions {
            path: temp.path().display().to_string(),
            mode: RunnerWorkspaceSyncMode::Snapshot,
            controller_routed_git: false,
            changed_since_base: None,
            git_fetch_refs: Vec::new(),
            snapshot_includes: Vec::new(),
            allow_dirty_lab_workspace: false,
            run_isolation_token: None,
        },
    )?
    .0;
    let remote_spec = format!(
        "@{}/{}",
        synced.remote_path.trim_end_matches('/'),
        spec.filename
    );
    let entry = workspace_mapping_entry(spec.role, &synced);
    Ok(Some((remote_spec, entry)))
}

fn write_private_remapped_agent_task_plan(path: &std::path::Path, contents: &str) -> Result<()> {
    write_file_owner_only(path, contents, "write remapped agent-task plan")
}

pub(super) fn mirror_agent_task_run_plan_lifecycle(
    args: &[String],
    agent_task: Option<&RunnerWorkloadAgentTask>,
    notification_route: Option<&NotificationRoute>,
    stdout: &str,
    output_file_content: Option<&str>,
    job_events: Option<&[JobEvent]>,
) -> Result<()> {
    if let Some(agent_task) = agent_task {
        return mirror_typed_agent_task_run_plan_lifecycle(
            agent_task,
            notification_route,
            job_events,
        );
    }

    mirror_legacy_agent_task_run_plan_lifecycle(
        args,
        notification_route,
        stdout,
        output_file_content,
        job_events,
    )
}

fn mirror_typed_agent_task_run_plan_lifecycle(
    agent_task: &RunnerWorkloadAgentTask,
    notification_route: Option<&NotificationRoute>,
    job_events: Option<&[JobEvent]>,
) -> Result<()> {
    if agent_task.lifecycle_mirror_policy
        != RunnerWorkloadAgentTaskLifecycleMirrorPolicy::RunPlanAggregate
    {
        return Ok(());
    }
    let Some(plan_spec) = agent_task.plan_ref.as_deref() else {
        return Ok(());
    };
    if plan_spec == "-" {
        return Ok(());
    }
    let Some(event) = agent_task_run_plan_lifecycle_event_from_job_events(job_events) else {
        return Ok(());
    };
    mirror_agent_task_run_plan_aggregate(
        plan_spec,
        &agent_task.run_id,
        event.aggregate,
        notification_route,
        Some(&event.identity),
    )
}

fn mirror_legacy_agent_task_run_plan_lifecycle(
    args: &[String],
    notification_route: Option<&NotificationRoute>,
    stdout: &str,
    output_file_content: Option<&str>,
    job_events: Option<&[JobEvent]>,
) -> Result<()> {
    let Some((plan_spec, run_id)) = agent_task_run_plan_recording_args(args) else {
        return Ok(());
    };
    if plan_spec == "-" {
        return Ok(());
    }
    // Legacy runner compatibility: only pre-typed runner workloads reach this
    // branch, so argv/stdout recovery is intentionally centralized here.
    let (aggregate, dispatch_identity) =
        match agent_task_run_plan_lifecycle_event_from_job_events(job_events) {
            Some(event) => (event.aggregate, Some(event.identity)),
            None => (
                legacy_agent_task_run_plan_aggregate(stdout, output_file_content)?,
                None,
            ),
        };
    mirror_agent_task_run_plan_aggregate(
        &plan_spec,
        &run_id,
        aggregate,
        notification_route,
        dispatch_identity.as_ref(),
    )
}

fn legacy_agent_task_run_plan_aggregate(
    stdout: &str,
    output_file_content: Option<&str>,
) -> Result<AgentTaskAggregate> {
    let envelope = parse_offloaded_run_plan_envelope(agent_task_run_plan_lifecycle_output(
        stdout,
        output_file_content,
    ))?;
    if !is_agent_task_run_plan_envelope(&envelope) {
        return Err(Error::internal_unexpected(
            "legacy agent-task run-plan output did not contain an aggregate envelope",
        ));
    }
    let Some(aggregate_value) = envelope.get("data").cloned() else {
        return Err(Error::internal_unexpected(
            "legacy agent-task run-plan output aggregate envelope was missing data",
        ));
    };
    serde_json::from_value(aggregate_value).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("parse legacy offloaded agent-task aggregate".to_string()),
        )
    })
}

fn mirror_agent_task_run_plan_aggregate(
    plan_spec: &str,
    run_id: &str,
    aggregate: AgentTaskAggregate,
    notification_route: Option<&NotificationRoute>,
    dispatch_identity: Option<&AgentTaskDispatchIdentity>,
) -> Result<()> {
    let raw_plan = config::read_json_spec_to_string(plan_spec)?;
    let plan: AgentTaskPlan = serde_json::from_str(&raw_plan).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some(format!("read agent-task plan {plan_spec}")),
        )
    })?;
    agent_task_lifecycle::submit_plan(&plan, Some(run_id))?;
    if let Some(notification_route) = notification_route {
        crate::core::agent_task_lifecycle::persist_notification_route(run_id, notification_route)?;
    }
    agent_task_lifecycle::mark_running(run_id)?;
    agent_task_lifecycle::record_run_aggregate(run_id, &plan, &aggregate)?;
    if let Some(identity) = dispatch_identity.filter(|identity| {
        !identity.runner_id.trim().is_empty() && !identity.runner_job_id.trim().is_empty()
    }) {
        record_runner_job_identity(run_id, &identity.runner_id, &identity.runner_job_id)?;
    }
    Ok(())
}

fn agent_task_run_plan_lifecycle_output<'a>(
    stdout: &'a str,
    output_file_content: Option<&'a str>,
) -> &'a str {
    output_file_content.unwrap_or(stdout)
}

pub(super) fn parse_offloaded_agent_task_handoff_from_outputs(
    stdout: &str,
    stderr: &str,
) -> Result<Option<AgentTaskLabHandoff>> {
    parse_offloaded_agent_task_handoff(stdout).and_then(|parsed| match parsed {
        Some(handoff) => Ok(Some(handoff)),
        None => parse_offloaded_agent_task_handoff(stderr),
    })
}

pub(super) fn parse_offloaded_agent_task_handoff(
    output: &str,
) -> Result<Option<AgentTaskLabHandoff>> {
    if let Ok(value) = serde_json::from_str::<Value>(output) {
        return agent_task_handoff_value(&value);
    }

    for (index, _) in output.match_indices('{') {
        let mut stream = serde_json::Deserializer::from_str(&output[index..]).into_iter();
        if let Some(Ok(value)) = stream.next() {
            if let Some(handoff) = agent_task_handoff_value(&value)? {
                return Ok(Some(handoff));
            }
        }
    }

    Ok(None)
}

const AGENT_TASK_LAB_HANDOFF_SCHEMA: &str = "homeboy/agent-task-lab-handoff/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(super) struct AgentTaskLabHandoff {
    #[serde(default = "agent_task_lab_handoff_schema")]
    pub schema: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_summary: Option<AgentTaskRunRecordSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregate_summary: Option<AgentTaskAggregateHandoffSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<AgentTaskArtifactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_manifest: Option<ArtifactManifest>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<AgentTaskEvidenceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<AgentTaskRunRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregate: Option<AgentTaskAggregate>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub envelope: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct AgentTaskRunRecordSummary {
    pub run_id: String,
    pub plan_id: String,
    pub state: AgentTaskRunState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregate_path: Option<String>,
    #[serde(default)]
    pub task_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct AgentTaskAggregateHandoffSummary {
    pub plan_id: String,
    pub status: String,
    pub totals: AgentTaskAggregateTotals,
    #[serde(default)]
    pub outcome_count: usize,
}

fn agent_task_lab_handoff_schema() -> String {
    AGENT_TASK_LAB_HANDOFF_SCHEMA.to_string()
}

fn agent_task_handoff_value(value: &Value) -> Result<Option<AgentTaskLabHandoff>> {
    if let Some(handoff_value) = typed_agent_task_handoff_value(value) {
        let mut handoff = serde_json::from_value::<AgentTaskLabHandoff>(handoff_value.clone())
            .map_err(|err| {
                Error::internal_json(
                    err.to_string(),
                    Some("parse typed agent-task Lab handoff".to_string()),
                )
            })?;
        finalize_typed_agent_task_handoff(&mut handoff)?;
        if handoff.envelope.is_null() {
            handoff.envelope = handoff_envelope_from_typed_handoff(&handoff);
        }
        return Ok(Some(handoff));
    }
    Ok(agent_task_dispatch_envelope_value(value).map(AgentTaskLabHandoff::from_dispatch_envelope))
}

fn typed_agent_task_handoff_value(value: &Value) -> Option<&Value> {
    if value.get("schema").and_then(Value::as_str) == Some(AGENT_TASK_LAB_HANDOFF_SCHEMA) {
        return Some(value);
    }
    let data = value.get("data")?;
    (data.get("schema").and_then(Value::as_str) == Some(AGENT_TASK_LAB_HANDOFF_SCHEMA))
        .then_some(data)
}

fn handoff_envelope_from_typed_handoff(handoff: &AgentTaskLabHandoff) -> Value {
    let mut envelope = serde_json::Map::new();
    envelope.insert(
        "schema".to_string(),
        Value::String("homeboy/agent-task-dispatch/v1".to_string()),
    );
    if let Some(run_id) = handoff.run_id.as_ref() {
        envelope.insert("run_id".to_string(), Value::String(run_id.clone()));
    }
    if let Some(record) = handoff.record.as_ref() {
        if let Ok(value) = serde_json::to_value(record) {
            envelope.insert("record".to_string(), value);
        }
    }
    if let Some(aggregate) = handoff.aggregate.as_ref() {
        if let Ok(value) = serde_json::to_value(
            &crate::core::agent_task_artifacts::reviewer_facing_aggregate(aggregate),
        ) {
            envelope.insert("aggregate".to_string(), value);
        }
    }
    if let Some(manifest) = handoff.artifact_manifest.as_ref() {
        if let Ok(value) = serde_json::to_value(manifest) {
            envelope.insert("artifact_manifest".to_string(), value);
        }
    }
    Value::Object(envelope)
}

fn finalize_typed_agent_task_handoff(handoff: &mut AgentTaskLabHandoff) -> Result<()> {
    let Some(manifest) = handoff.artifact_manifest.as_ref() else {
        return Ok(());
    };
    manifest.validate_shape()?;
    let run_id = handoff.run_id.as_deref().unwrap_or("lab-offload");
    let manifest_refs = collect_manifest_artifact_refs(manifest, run_id);
    append_unique_artifact_refs(&mut handoff.artifact_refs, manifest_refs);
    Ok(())
}

impl AgentTaskLabHandoff {
    fn from_dispatch_envelope(envelope: &Value) -> Self {
        let record = envelope
            .get("record")
            .and_then(|value| serde_json::from_value::<AgentTaskRunRecord>(value.clone()).ok());
        let aggregate = envelope
            .get("aggregate")
            .and_then(|value| serde_json::from_value::<AgentTaskAggregate>(value.clone()).ok());
        let run_id = envelope
            .get("run_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| record.as_ref().map(|record| record.run_id.clone()));
        Self {
            schema: agent_task_lab_handoff_schema(),
            run_id,
            record_summary: record.as_ref().map(AgentTaskRunRecordSummary::from_record),
            aggregate_summary: aggregate
                .as_ref()
                .map(AgentTaskAggregateHandoffSummary::from_aggregate),
            artifact_refs: collect_handoff_artifact_refs(record.as_ref(), aggregate.as_ref()),
            artifact_manifest: None,
            evidence_refs: collect_handoff_evidence_refs(aggregate.as_ref()),
            record,
            aggregate,
            envelope: envelope.clone(),
        }
    }
}

impl AgentTaskRunRecordSummary {
    fn from_record(record: &AgentTaskRunRecord) -> Self {
        Self {
            run_id: record.run_id.clone(),
            plan_id: record.plan_id.clone(),
            state: record.state,
            aggregate_path: record.aggregate_path.clone(),
            task_count: record.tasks.len(),
        }
    }
}

impl AgentTaskAggregateHandoffSummary {
    fn from_aggregate(aggregate: &AgentTaskAggregate) -> Self {
        Self {
            plan_id: aggregate.plan_id.clone(),
            status: format!("{:?}", aggregate.status).to_lowercase(),
            totals: aggregate.totals.clone(),
            outcome_count: aggregate.outcomes.len(),
        }
    }
}

fn collect_handoff_artifact_refs(
    record: Option<&AgentTaskRunRecord>,
    aggregate: Option<&AgentTaskAggregate>,
) -> Vec<AgentTaskArtifactRef> {
    let mut refs = record
        .map(|record| record.artifact_refs.clone())
        .unwrap_or_default();
    if let Some(aggregate) = aggregate {
        for outcome in &aggregate.outcomes {
            for artifact in &outcome.artifacts {
                let uri = artifact
                    .url
                    .as_deref()
                    .or(artifact.path.as_deref())
                    .unwrap_or(&artifact.id);
                refs.push(AgentTaskArtifactRef {
                    task_id: outcome.task_id.clone(),
                    kind: artifact.kind.clone(),
                    uri: uri.to_string(),
                    role: artifact.role.clone(),
                    label: artifact.label.clone().or_else(|| artifact.name.clone()),
                    semantic_key: artifact.semantic_key.clone(),
                    size_bytes: artifact.size_bytes,
                });
            }
        }
    }
    refs
}

fn collect_manifest_artifact_refs(
    manifest: &ArtifactManifest,
    run_id: &str,
) -> Vec<AgentTaskArtifactRef> {
    manifest
        .artifacts
        .iter()
        .map(|entry| AgentTaskArtifactRef {
            task_id: entry
                .metadata
                .get("task_id")
                .or_else(|| entry.metadata.get("taskId"))
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .unwrap_or(run_id)
                .to_string(),
            kind: entry.kind.clone(),
            uri: entry
                .public_url
                .as_deref()
                .unwrap_or(&entry.path)
                .to_string(),
            role: entry.role.clone(),
            label: entry.label.clone(),
            semantic_key: entry.semantic_key.clone(),
            size_bytes: entry.size_bytes,
        })
        .collect()
}

fn append_unique_artifact_refs(
    artifact_refs: &mut Vec<AgentTaskArtifactRef>,
    incoming: Vec<AgentTaskArtifactRef>,
) {
    for artifact_ref in incoming {
        if artifact_refs
            .iter()
            .any(|existing| existing == &artifact_ref)
        {
            continue;
        }
        artifact_refs.push(artifact_ref);
    }
}

fn collect_handoff_evidence_refs(
    aggregate: Option<&AgentTaskAggregate>,
) -> Vec<AgentTaskEvidenceRef> {
    aggregate
        .into_iter()
        .flat_map(|aggregate| aggregate.outcomes.iter())
        .flat_map(|outcome| outcome.evidence_refs.iter().cloned())
        .collect()
}

fn agent_task_dispatch_envelope_value(value: &serde_json::Value) -> Option<&serde_json::Value> {
    if value.get("schema").and_then(serde_json::Value::as_str)
        == Some("homeboy/agent-task-dispatch/v1")
    {
        return Some(value);
    }
    let data = value.get("data")?;
    (data.get("schema").and_then(serde_json::Value::as_str)
        == Some("homeboy/agent-task-dispatch/v1"))
    .then_some(data)
}

fn agent_task_run_plan_recording_args(args: &[String]) -> Option<(String, String)> {
    let run_plan_index = subcommand_index(args, "agent-task").and_then(|index| {
        args.get(index + 1)
            .filter(|arg| arg.as_str() == "run-plan")
            .map(|_| index + 1)
    })?;

    let mut plan = None;
    let mut record_run_id = None;
    let mut iter = args.iter().skip(run_plan_index + 1);
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        match arg.as_str() {
            "--plan" => plan = iter.next().cloned(),
            "--record-run-id" => record_run_id = iter.next().cloned(),
            _ => {
                if let Some(value) = arg.strip_prefix("--plan=") {
                    plan = Some(value.to_string());
                } else if let Some(value) = arg.strip_prefix("--record-run-id=") {
                    record_run_id = Some(value.to_string());
                }
            }
        }
    }

    Some((plan?, record_run_id?))
}

pub(super) fn agent_task_dispatch_requested_run_id(args: &[String]) -> Option<String> {
    let invocation = CommandInvocation::for_subcommand(args, "agent-task")?;
    let action_index = invocation.child_index_matching(&["cook", "dispatch"])?;
    invocation
        .option_value_after(action_index, "--run-id")
        .map(str::to_string)
}

pub(super) fn ensure_agent_task_dispatch_run_id(args: &[String]) -> Option<(Vec<String>, String)> {
    ensure_agent_task_dispatch_run_id_with(args, None)
}

/// Like [`ensure_agent_task_dispatch_run_id`] but, when no `--run-id` is already
/// present, uses `preferred` as the injected run id instead of generating a
/// fresh UUID. This lets the offload orchestrator keep the workspace-isolation
/// token and the dispatched `--run-id` identical for a given run (#4393).
pub(super) fn ensure_agent_task_dispatch_run_id_with(
    args: &[String],
    preferred: Option<&str>,
) -> Option<(Vec<String>, String)> {
    if let Some((_, run_id)) = agent_task_run_plan_recording_args(args) {
        return Some((args.to_vec(), run_id));
    }
    let invocation = CommandInvocation::for_subcommand(args, "agent-task")?;
    let action_index = invocation.child_index_matching(&["cook", "dispatch"])?;

    if let Some(run_id) = agent_task_dispatch_requested_run_id(args) {
        return Some((args.to_vec(), run_id));
    }

    let run_id = preferred
        .filter(|id| !id.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("agent-task-{}", uuid::Uuid::new_v4()));
    let out = ArgEditor::new(args)
        .insert_after(action_index, ["--run-id".to_string(), run_id.clone()])
        .into_args();
    Some((out, run_id))
}

/// Give a portable cook one first-attempt id before it crosses the Lab
/// boundary. The cook id remains part of the command's public contract, while
/// this id is the lifecycle record owned by the controller, daemon, and runner.
pub(super) fn ensure_agent_task_lifecycle_identity_with(
    args: &[String],
    preferred: Option<&str>,
    preferred_attempt_run_id: Option<&str>,
) -> Option<(Vec<String>, String)> {
    let (args, run_id) = ensure_agent_task_dispatch_run_id_with(args, preferred)?;
    let invocation = CommandInvocation::for_subcommand(&args, "agent-task")?;
    // A run-plan already carries the controller-owned durable identity in
    // --record-run-id. Unlike cook, it has no attempt id to derive or inject.
    if invocation.child_index_matching(&["run-plan"]).is_some() {
        return Some((args, run_id));
    }
    let action_index = invocation.child_index_matching(&["cook"])?;
    let existing_attempt_run_id = invocation
        .option_value_after(action_index, "--attempt-run-id")
        .map(str::to_string);
    if let Some(attempt_run_id) = existing_attempt_run_id {
        return Some((args, attempt_run_id));
    }

    let attempt_run_id = preferred_attempt_run_id
        .filter(|id| !id.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| cook_attempt_run_id(&run_id, 1));
    let args = ArgEditor::new(&args)
        .insert_after(
            action_index,
            ["--attempt-run-id".to_string(), attempt_run_id.clone()],
        )
        .into_args();
    Some((args, attempt_run_id))
}

#[cfg(test)]
mod lifecycle_identity_tests {
    use super::*;

    #[test]
    fn run_plan_record_identity_is_preserved_for_lab_lifecycle() {
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "run-plan".to_string(),
            "--plan".to_string(),
            "@plan.json".to_string(),
            "--record-run-id".to_string(),
            "retry-8341".to_string(),
        ];

        let (rewritten, run_id) = ensure_agent_task_lifecycle_identity_with(&args, None, None)
            .expect("run-plan has a durable lifecycle identity");

        assert_eq!(rewritten, args);
        assert_eq!(run_id, "retry-8341");
    }
}

/// Resolves the stable per-run isolation token for an agent-task cook offload:
/// the explicit `--run-id` when provided, otherwise a freshly generated run id.
/// Returns `None` for other invocations, which already use unique snapshot
/// workspaces and need no extra isolation.
pub(super) fn agent_task_dispatch_run_isolation_token(args: &[String]) -> Option<String> {
    if let Some(run_id) = agent_task_dispatch_requested_run_id(args) {
        return Some(run_id);
    }
    ensure_agent_task_dispatch_run_id(args).map(|(_, run_id)| run_id)
}

pub(super) fn lab_pre_dispatch_failure_message(output: &str) -> Option<String> {
    if let Some(message) = lab_pre_dispatch_structured_dependency_failure_message(output) {
        return Some(message);
    }

    if let Some(message) =
        lab_pre_dispatch_dependency_failure_message(output, &dependency_failure_patterns())
    {
        return Some(message);
    }

    output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

#[derive(Debug, Deserialize)]
struct LabDependencyFailureEnvelope {
    #[serde(default)]
    schema: Option<String>,
    dependency: LabDependencyFailureDependency,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    remediation: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct LabDependencyFailureDependency {
    id: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    path: Option<String>,
}

fn lab_pre_dispatch_structured_dependency_failure_message(output: &str) -> Option<String> {
    output
        .lines()
        .filter_map(|line| serde_json::from_str::<LabDependencyFailureEnvelope>(line.trim()).ok())
        .find(|envelope| envelope.schema.as_deref() == Some("homeboy/lab-dependency-failure/v1"))
        .map(|envelope| structured_dependency_failure_message(&envelope))
}

fn structured_dependency_failure_message(envelope: &LabDependencyFailureEnvelope) -> String {
    let dependency = envelope
        .dependency
        .path
        .as_deref()
        .filter(|path| !path.trim().is_empty())
        .unwrap_or(&envelope.dependency.id);
    let kind = envelope
        .dependency
        .kind
        .as_deref()
        .filter(|kind| !kind.trim().is_empty())
        .unwrap_or("dependency");
    let reason = envelope
        .message
        .as_deref()
        .filter(|message| !message.trim().is_empty())
        .unwrap_or("runtime dependency staging failed");
    let remediation = envelope
        .remediation
        .as_deref()
        .filter(|remediation| !remediation.trim().is_empty())
        .unwrap_or("repair or refresh the runner runtime");
    format!(
        "Lab runtime failed before agent dispatch while staging {kind} `{dependency}`: {reason}. {remediation}, then retry this cook run."
    )
}

fn lab_pre_dispatch_dependency_failure_message(
    output: &str,
    patterns: &[AgentTaskProviderDependencyFailurePattern],
) -> Option<String> {
    let pattern = patterns
        .iter()
        .find(|pattern| dependency_failure_pattern_matches(output, pattern))?;
    let missing_path = first_quoted_dependency_path(output, &pattern.path_contains)
        .unwrap_or_else(|| pattern.label.clone());
    Some(format!(
        "Lab runtime failed before agent dispatch while staging dependency `{missing_path}`. The selected Lab runner has a stale or misconfigured runtime dependency; {}, then retry this cook run.",
        pattern
            .remediation
            .as_deref()
            .unwrap_or("repair or refresh the runner runtime")
    ))
}

fn dependency_failure_pattern_matches(
    output: &str,
    pattern: &AgentTaskProviderDependencyFailurePattern,
) -> bool {
    let lower = output.to_lowercase();
    lower.contains(&pattern.path_contains.to_lowercase())
        && (pattern.error_contains_any.is_empty()
            || pattern
                .error_contains_any
                .iter()
                .any(|needle| lower.contains(&needle.to_lowercase())))
}

fn first_quoted_dependency_path(output: &str, path_contains: &str) -> Option<String> {
    output
        .split(['\'', '"'])
        .find(|part| part.contains(path_contains))
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_offloaded_run_plan_envelope_parser_tolerates_extension_stdout_chatter() {
        let stdout = concat!(
            "Preparing extension runtime...\n",
            "Installing declared dependencies...\n",
            "{\"success\":false,\"data\":{\"status\":\"failed\"}}\n",
            "trailing diagnostic\n"
        );

        let parsed = parse_offloaded_run_plan_envelope(stdout).expect("parse mixed stdout");

        assert_eq!(parsed["success"], false);
        assert_eq!(parsed["data"]["status"], "failed");
    }

    #[test]
    fn legacy_offloaded_run_plan_envelope_parser_selects_aggregate_from_mixed_json() {
        let stdout = concat!(
            "{\"success\":true,\"data\":{\"command\":\"extension.setup\"}}\n",
            "setup complete\n",
            "{\"success\":true,\"data\":{\"schema\":\"homeboy/agent-task-aggregate/v1\",\"plan_id\":\"plan-1\",\"status\":\"succeeded\",\"totals\":{\"succeeded\":6}}}\n"
        );

        let parsed = parse_offloaded_run_plan_envelope(stdout).expect("parse aggregate envelope");

        assert_eq!(parsed["data"]["plan_id"], "plan-1");
        assert_eq!(parsed["data"]["totals"]["succeeded"], 6);
    }

    #[test]
    fn offloaded_dispatch_envelope_parser_selects_structured_failure_from_mixed_stdout() {
        let stdout = concat!(
            "remote setup complete\n",
            "{\"success\":true,\"data\":{\"command\":\"extension.setup\"}}\n",
            "{\"success\":false,\"data\":{\"schema\":\"homeboy/agent-task-dispatch/v1\",\"run_id\":\"run-1\",\"state\":\"failed\",\"record\":{},\"aggregate\":{\"status\":\"failed\"}}}\n"
        );

        let parsed = parse_offloaded_agent_task_handoff(stdout)
            .expect("parse dispatch stdout")
            .map(|handoff| handoff.envelope)
            .expect("dispatch envelope found");

        assert_eq!(parsed["run_id"], "run-1");
        assert_eq!(parsed["aggregate"]["status"], "failed");
    }

    #[test]
    fn offloaded_dispatch_envelope_parser_selects_structured_failure_from_stderr() {
        let stdout = "remote setup complete\n";
        let stderr = concat!(
            "{\n",
            "  \"success\": false,\n",
            "  \"data\": {\n",
            "    \"schema\": \"homeboy/agent-task-dispatch/v1\",\n",
            "    \"run_id\": \"conductor-full-loop-proof-retry3-20260612\",\n",
            "    \"state\": \"failed\",\n",
            "    \"aggregate\": {\n",
            "      \"status\": \"failed\",\n",
            "      \"outcomes\": [{\n",
            "        \"task_id\": \"cook-conductor\",\n",
            "        \"status\": \"failed\",\n",
            "        \"summary\": \"Remote agent task failed.\",\n",
            "        \"metadata\": {\n",
            "          \"provider\": \"remote.agent-task-executor\",\n",
            "          \"runtime_run_result\": {\n",
            "            \"schema\": \"remote/agent-task-run-result/v1\",\n",
            "            \"status\": \"failed\",\n",
            "            \"failure_classification\": \"runtime\"\n",
            "          }\n",
            "        }\n",
            "      }]\n",
            "    }\n",
            "  }\n",
            "}\n"
        );

        let parsed = parse_offloaded_agent_task_handoff_from_outputs(stdout, stderr)
            .expect("parse dispatch outputs")
            .map(|handoff| handoff.envelope)
            .expect("dispatch envelope found");

        assert_eq!(
            parsed["run_id"],
            "conductor-full-loop-proof-retry3-20260612"
        );
        assert_eq!(
            parsed["aggregate"]["outcomes"][0]["task_id"],
            "cook-conductor"
        );
        assert_eq!(
            parsed["aggregate"]["outcomes"][0]["metadata"]["provider"],
            "remote.agent-task-executor"
        );
        assert_eq!(
            parsed["aggregate"]["outcomes"][0]["metadata"]["runtime_run_result"]
                ["failure_classification"],
            "runtime"
        );
    }

    #[test]
    fn offloaded_agent_task_handoff_wraps_legacy_dispatch_envelope() {
        let stdout = concat!(
            "remote setup complete\n",
            "{\"success\":false,\"data\":{",
            "\"schema\":\"homeboy/agent-task-dispatch/v1\",",
            "\"run_id\":\"run-1\",",
            "\"aggregate\":{",
            "\"plan_id\":\"plan-1\",",
            "\"status\":\"failed\",",
            "\"totals\":{\"skipped\":0,\"failed\":1},",
            "\"outcomes\":[{",
            "\"task_id\":\"task-1\",",
            "\"status\":\"failed\",",
            "\"artifacts\":[{\"id\":\"artifact-1\",\"kind\":\"log\",\"path\":\"/tmp/log.txt\"}],",
            "\"evidence_refs\":[{\"kind\":\"logs\",\"uri\":\"homeboy://agent-task/run/run-1/logs\"}]",
            "}]}}}\n"
        );

        let handoff = parse_offloaded_agent_task_handoff(stdout)
            .expect("parse handoff")
            .expect("handoff found");

        assert_eq!(handoff.schema, AGENT_TASK_LAB_HANDOFF_SCHEMA);
        assert_eq!(handoff.run_id.as_deref(), Some("run-1"));
        assert_eq!(
            handoff.aggregate_summary.expect("aggregate summary").status,
            "failed"
        );
        assert_eq!(handoff.artifact_refs[0].uri, "/tmp/log.txt");
        assert_eq!(handoff.evidence_refs[0].kind, "logs");
        assert_eq!(handoff.envelope["schema"], "homeboy/agent-task-dispatch/v1");
    }

    #[test]
    fn offloaded_agent_task_handoff_accepts_typed_data_envelope() {
        let stdout = concat!(
            "runner chatter\n",
            "{\"success\":false,\"data\":{",
            "\"schema\":\"homeboy/agent-task-lab-handoff/v1\",",
            "\"run_id\":\"run-typed\",",
            "\"aggregate_summary\":{",
            "\"plan_id\":\"plan-typed\",",
            "\"status\":\"failed\",",
            "\"totals\":{\"skipped\":0,\"failed\":1},",
            "\"outcome_count\":1},",
            "\"artifact_refs\":[{",
            "\"task_id\":\"task-typed\",",
            "\"kind\":\"review\",",
            "\"uri\":\"homeboy://artifact/review\"}],",
            "\"evidence_refs\":[{",
            "\"kind\":\"review\",",
            "\"uri\":\"homeboy://agent-task/run/run-typed/review\"}]",
            "}}\n"
        );

        let handoff = parse_offloaded_agent_task_handoff(stdout)
            .expect("parse handoff")
            .expect("handoff found");

        assert_eq!(handoff.run_id.as_deref(), Some("run-typed"));
        assert_eq!(handoff.artifact_refs[0].kind, "review");
        assert_eq!(
            handoff.evidence_refs[0].uri,
            "homeboy://agent-task/run/run-typed/review"
        );
        assert_eq!(handoff.envelope["run_id"], "run-typed");
        assert!(handoff.envelope.get("aggregate").is_none());
    }

    #[test]
    fn typed_handoff_imports_valid_artifact_manifest_refs() {
        let stdout = concat!(
            "runner chatter\n",
            "{\"schema\":\"homeboy/agent-task-lab-handoff/v1\",",
            "\"run_id\":\"run-manifest\",",
            "\"artifact_manifest\":{",
            "\"schema\":\"homeboy/artifact-manifest/v1\",",
            "\"artifacts\":[{",
            "\"path\":\"logs/output.log\",",
            "\"kind\":\"log\",",
            "\"role\":\"execution-log\",",
            "\"label\":\"Runner output\",",
            "\"semantic_key\":\"runner.output\",",
            "\"size_bytes\":42,",
            "\"metadata\":{\"task_id\":\"task-from-manifest\"}",
            "}]}}\n"
        );

        let handoff = parse_offloaded_agent_task_handoff(stdout)
            .expect("parse handoff")
            .expect("handoff found");

        assert_eq!(handoff.artifact_refs.len(), 1);
        let artifact_ref = &handoff.artifact_refs[0];
        assert_eq!(artifact_ref.task_id, "task-from-manifest");
        assert_eq!(artifact_ref.kind, "log");
        assert_eq!(artifact_ref.uri, "logs/output.log");
        assert_eq!(artifact_ref.role.as_deref(), Some("execution-log"));
        assert_eq!(artifact_ref.label.as_deref(), Some("Runner output"));
        assert_eq!(artifact_ref.semantic_key.as_deref(), Some("runner.output"));
        assert_eq!(artifact_ref.size_bytes, Some(42));
        assert_eq!(
            handoff.envelope["artifact_manifest"]["schema"],
            "homeboy/artifact-manifest/v1"
        );
    }

    #[test]
    fn typed_handoff_rejects_malformed_artifact_manifest() {
        let stdout = concat!(
            "{\"schema\":\"homeboy/agent-task-lab-handoff/v1\",",
            "\"run_id\":\"run-bad-manifest\",",
            "\"artifact_manifest\":{",
            "\"schema\":\"homeboy/artifact-manifest/v1\",",
            "\"artifacts\":[{\"path\":\"../secret.log\",\"kind\":\"log\"}]",
            "}}\n"
        );

        let err = parse_offloaded_agent_task_handoff(stdout).expect_err("invalid manifest");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("artifact path must be relative"));
    }

    #[test]
    fn typed_run_plan_lifecycle_ignores_stdout_without_job_event() {
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "run-plan".to_string(),
            "--plan".to_string(),
            "@/tmp/plan.json".to_string(),
            "--record-run-id".to_string(),
            "run-1".to_string(),
        ];
        let stdout = "{\"success\":true,\"data\":{\"command\":\"extension.setup\"}}";

        let agent_task = RunnerWorkloadAgentTask {
            run_id: "run-typed-no-event".to_string(),
            plan_ref: Some("@/tmp/plan.json".to_string()),
            resolved_provider_policy: None,
            dispatch_kind: crate::core::lab_contract::RunnerWorkloadAgentTaskDispatchKind::RunPlan,
            lifecycle_mirror_policy: RunnerWorkloadAgentTaskLifecycleMirrorPolicy::RunPlanAggregate,
        };

        mirror_agent_task_run_plan_lifecycle(&args, Some(&agent_task), None, stdout, None, None)
            .expect("typed path does not parse stdout fallback");
    }

    #[test]
    fn typed_run_plan_lifecycle_mirror_uses_workload_event_without_stdout_parsing() {
        crate::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("tempdir");
            let plan_path = temp.path().join("plan.json");
            fs::write(
                &plan_path,
                r#"{"schema":"homeboy/agent-task-plan/v1","plan_id":"plan-typed","tasks":[]}"#,
            )
            .expect("write plan");
            let agent_task = RunnerWorkloadAgentTask {
                run_id: "run-typed-workload".to_string(),
                plan_ref: Some(format!("@{}", plan_path.display())),
                resolved_provider_policy: None,
                dispatch_kind:
                    crate::core::lab_contract::RunnerWorkloadAgentTaskDispatchKind::RunPlan,
                lifecycle_mirror_policy:
                    RunnerWorkloadAgentTaskLifecycleMirrorPolicy::RunPlanAggregate,
            };
            let events = vec![JobEvent {
                sequence: 1,
                job_id: uuid::Uuid::nil(),
                kind: JobEventKind::Progress,
                timestamp_ms: 1,
                message: None,
                data: Some(serde_json::json!({
                    "schema": "homeboy/runner-workload-agent-task-lifecycle-event/v1",
                    "agent_task_lifecycle_event": {
                        "schema": "homeboy/agent-task-run-plan-lifecycle-event/v1",
                        "identity": {
                            "runner_id": "lab-default",
                            "runner_job_id": "job-typed",
                            "run_id": "run-typed-workload"
                        },
                        "aggregate": {
                            "schema":"homeboy/agent-task-aggregate/v1",
                            "plan_id":"plan-typed",
                            "status":"succeeded",
                            "totals":{"skipped":0,"succeeded":0,"failed":0},
                            "outcomes":[]
                        }
                    }
                })),
            }];

            mirror_agent_task_run_plan_lifecycle(
                &[],
                Some(&agent_task),
                None,
                "not json and should not be parsed",
                None,
                Some(&events),
            )
            .expect("typed mirror uses job event");
        });
    }

    #[test]
    fn typed_run_plan_lifecycle_preserves_completed_noop_and_remote_evidence() {
        crate::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("tempdir");
            let plan_path = temp.path().join("plan.json");
            fs::write(
                &plan_path,
                r#"{"schema":"homeboy/agent-task-plan/v1","plan_id":"plan-noop","tasks":[]}"#,
            )
            .expect("write plan");
            let agent_task = RunnerWorkloadAgentTask {
                run_id: "run-completed-noop".to_string(),
                plan_ref: Some(format!("@{}", plan_path.display())),
                resolved_provider_policy: None,
                dispatch_kind:
                    crate::core::lab_contract::RunnerWorkloadAgentTaskDispatchKind::RunPlan,
                lifecycle_mirror_policy:
                    RunnerWorkloadAgentTaskLifecycleMirrorPolicy::RunPlanAggregate,
            };
            let events = vec![JobEvent {
                sequence: 1,
                job_id: uuid::Uuid::nil(),
                kind: JobEventKind::Result,
                timestamp_ms: 1,
                message: None,
                data: Some(serde_json::json!({
                    "agent_task_lifecycle_event": {
                        "schema": "homeboy/agent-task-run-plan-lifecycle-event/v1",
                        "identity": {
                            "runner_id": "lab-default",
                            "runner_job_id": "job-completed-noop",
                            "run_id": "run-completed-noop"
                        },
                        "aggregate": {
                            "schema": "homeboy/agent-task-aggregate/v1",
                            "plan_id": "plan-noop",
                            "status": "succeeded",
                            "totals": {"skipped": 0, "succeeded": 1, "failed": 0},
                            "outcomes": [{
                                "schema": "homeboy/agent-task-outcome/v1",
                                "task_id": "cook",
                                "status": "no_op",
                                "summary": "no_changes",
                                "artifacts": [],
                                "typed_artifacts": [],
                                "evidence_refs": [],
                                "diagnostics": [],
                                "outputs": null,
                                "metadata": {"child_run_id": "attempt-noop-1"}
                            }],
                            "child_runs": [{
                                "task_id": "cook",
                                "run_id": "attempt-noop-1",
                                "state": "succeeded",
                                "metadata": {}
                            }]
                        }
                    }
                })),
            }];

            mirror_agent_task_run_plan_lifecycle(
                &[],
                Some(&agent_task),
                None,
                "not parsed",
                None,
                Some(&events),
            )
            .expect("completed no-op mirror");

            let record = agent_task_lifecycle::status("run-completed-noop").expect("status");
            let artifacts =
                agent_task_lifecycle::artifacts("run-completed-noop").expect("artifacts");
            let (_, aggregate_path) = agent_task_lifecycle::aggregate_source("run-completed-noop")
                .expect("aggregate source");
            let aggregate: AgentTaskAggregate =
                serde_json::from_str(&fs::read_to_string(aggregate_path).expect("aggregate"))
                    .expect("aggregate JSON");

            assert_eq!(record.state, AgentTaskRunState::Succeeded);
            assert_eq!(record.metadata["runner_id"], "lab-default");
            assert_eq!(record.metadata["runner_job_id"], "job-completed-noop");
            assert_eq!(
                aggregate.outcomes[0].status,
                crate::core::agent_task::AgentTaskOutcomeStatus::NoOp
            );
            assert!(artifacts.evidence_refs.iter().any(|reference| {
                reference.kind == "agent-task-child-run"
                    && reference.uri == "homeboy://agent-task/run/attempt-noop-1"
            }));
        });
    }

    #[test]
    fn typed_run_plan_lifecycle_persists_remapped_provider_model_for_publication() {
        crate::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("tempdir");
            let plan_path = temp.path().join("plan.json");
            let plan = AgentTaskPlan::new(
                "plan-remapped-model",
                vec![crate::core::agent_task::AgentTaskRequest {
                    schema: crate::core::agent_task::AGENT_TASK_REQUEST_SCHEMA.to_string(),
                    task_id: "cook".to_string(),
                    group_key: None,
                    parent_plan_id: None,
                    executor: crate::core::agent_task::AgentTaskExecutor {
                        backend: "opencode".to_string(),
                        selector: None,
                        runtime_selection: None,
                        required_capabilities: Vec::new(),
                        secret_env: Vec::new(),
                        model: Some("openai/gpt-5.6-terra".to_string()),
                        config: Value::Null,
                    },
                    instructions: "apply the change".to_string(),
                    inputs: Value::Null,
                    source_refs: Vec::new(),
                    workspace: Default::default(),
                    component_contracts: Vec::new(),
                    policy: Default::default(),
                    limits: Default::default(),
                    expected_artifacts: Vec::new(),
                    artifact_declarations: Vec::new(),
                    metadata: Value::Null,
                }],
            );
            fs::write(
                &plan_path,
                serde_json::to_vec(&plan).expect("serialize remapped plan"),
            )
            .expect("write remapped plan");
            let agent_task = RunnerWorkloadAgentTask {
                run_id: "run-remapped-model".to_string(),
                plan_ref: Some(format!("@{}", plan_path.display())),
                resolved_provider_policy: Some(
                    crate::core::agent_task_dispatch_service::ResolvedAgentTaskProviderPolicy {
                        backend: "opencode".to_string(),
                        selector: None,
                        model: Some("openai/gpt-5.6-terra".to_string()),
                        rotation: None,
                        rotation_starts_with_first_entry: true,
                        retry: Default::default(),
                        liveness_timeout_ms: None,
                    },
                ),
                dispatch_kind:
                    crate::core::lab_contract::RunnerWorkloadAgentTaskDispatchKind::RunPlan,
                lifecycle_mirror_policy:
                    RunnerWorkloadAgentTaskLifecycleMirrorPolicy::RunPlanAggregate,
            };
            let events = vec![JobEvent {
                sequence: 1,
                job_id: uuid::Uuid::nil(),
                kind: JobEventKind::Result,
                timestamp_ms: 1,
                message: None,
                data: Some(serde_json::json!({
                    "agent_task_lifecycle_event": {
                        "schema": "homeboy/agent-task-run-plan-lifecycle-event/v1",
                        "identity": {
                            "runner_id": "lab-default",
                            "runner_job_id": "job-remapped-model",
                            "run_id": "run-remapped-model"
                        },
                        "aggregate": serde_json::to_value(AgentTaskAggregate {
                            schema: "homeboy/agent-task-aggregate/v1".to_string(),
                            plan_id: plan.plan_id.clone(),
                            status: crate::core::agent_task_scheduler::AgentTaskAggregateStatus::Succeeded,
                            totals: AgentTaskAggregateTotals {
                                succeeded: 1,
                                ..Default::default()
                            },
                            outcomes: vec![crate::core::agent_task::AgentTaskOutcome {
                                schema: crate::core::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                                task_id: "cook".to_string(),
                                status: crate::core::agent_task::AgentTaskOutcomeStatus::Succeeded,
                                summary: None,
                                failure_classification: None,
                                artifacts: Vec::new(),
                                typed_artifacts: Vec::new(),
                                evidence_refs: Vec::new(),
                                diagnostics: Vec::new(),
                                outputs: Value::Null,
                                workflow: None,
                                follow_up: None,
                                metadata: serde_json::json!({
                                    "provider_handle": {
                                        "task_id": "cook",
                                        "backend": "opencode",
                                        "run_id": "provider-run-8263",
                                        "metadata": {"provider_owned": true}
                                    }
                                }),
                            }],
                            events: Vec::new(),
                            artifact_lineage: Vec::new(),
                            child_runs: Vec::new(),
                            artifact_bindings: Vec::new(),
                            queue: Default::default(),
                        }).expect("serialize aggregate")
                    }
                })),
            }];

            mirror_agent_task_run_plan_lifecycle(
                &[],
                Some(&agent_task),
                None,
                "not parsed",
                None,
                Some(&events),
            )
            .expect("remapped Lab aggregate mirrored");

            let record = agent_task_lifecycle::status("run-remapped-model").expect("status");
            assert_eq!(record.lifecycle.provider_runtime.len(), 1);
            assert_eq!(
                record.lifecycle.provider_runtime[0].metadata["model"],
                "openai/gpt-5.6-terra"
            );
        });
    }

    #[test]
    fn legacy_run_plan_lifecycle_branch_uses_argv_and_stdout_only_without_typed_workload() {
        crate::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("tempdir");
            let plan_path = temp.path().join("plan.json");
            fs::write(
                &plan_path,
                r#"{"schema":"homeboy/agent-task-plan/v1","plan_id":"plan-legacy","tasks":[]}"#,
            )
            .expect("write plan");
            let args = vec![
                "homeboy".to_string(),
                "agent-task".to_string(),
                "run-plan".to_string(),
                "--plan".to_string(),
                format!("@{}", plan_path.display()),
                "--record-run-id".to_string(),
                "run-legacy-workload".to_string(),
            ];
            let stdout = concat!(
                "runner chatter\n",
                "{\"success\":true,\"data\":{",
                "\"schema\":\"homeboy/agent-task-aggregate/v1\",",
                "\"plan_id\":\"plan-legacy\",",
                "\"status\":\"succeeded\",",
                "\"totals\":{\"skipped\":0,\"succeeded\":0,\"failed\":0},",
                "\"outcomes\":[]}}"
            );

            mirror_agent_task_run_plan_lifecycle(&args, None, None, stdout, None, None)
                .expect("legacy mirror uses stdout fallback");
        });
    }

    #[test]
    fn run_plan_lifecycle_event_is_extracted_from_result_metadata() {
        let aggregate = serde_json::json!({
            "schema": "homeboy/agent-task-run-plan-lifecycle-event/v1",
            "identity": {
                "runner_id": "lab-default",
                "runner_job_id": "job-1",
                "run_id": "run-typed"
            },
            "aggregate": {
                "schema":"homeboy/agent-task-aggregate/v1",
                "plan_id":"plan-from-event",
                "status":"succeeded",
                "totals":{"skipped":0,"succeeded":1,"failed":0},
                "outcomes":[]
            }
        });
        let events = vec![JobEvent {
            sequence: 1,
            job_id: uuid::Uuid::nil(),
            kind: JobEventKind::Result,
            timestamp_ms: 1,
            message: None,
            data: Some(serde_json::json!({
                "exit_code": 0,
                "data": {
                    "agent_task_lifecycle_event": aggregate
                }
            })),
        }];

        let event = agent_task_run_plan_lifecycle_event_from_job_events(Some(&events))
            .expect("typed lifecycle event");

        assert_eq!(event.identity.runner_id, "lab-default");
        assert_eq!(event.aggregate.plan_id, "plan-from-event");
    }

    #[test]
    fn run_plan_lifecycle_prefers_downloaded_output_file_content() {
        let stdout = "{\"success\":true,\"data\":{\"command\":\"agent-task.run-plan\"}}";
        let downloaded_output = concat!(
            "{\"success\":true,\"data\":{",
            "\"schema\":\"homeboy/agent-task-aggregate/v1\",",
            "\"plan_id\":\"plan-from-file\",",
            "\"status\":\"succeeded\",",
            "\"totals\":{\"skipped\":0,\"succeeded\":1,\"failed\":0},",
            "\"outcomes\":[]}}"
        );

        let selected = agent_task_run_plan_lifecycle_output(stdout, Some(downloaded_output));
        let envelope = parse_offloaded_run_plan_envelope(selected).expect("parse selected output");

        assert!(is_agent_task_run_plan_envelope(&envelope));
        assert_eq!(envelope["data"]["plan_id"], "plan-from-file");
    }

    #[test]
    fn agent_task_dispatch_requested_run_id_accepts_cook_and_dispatch() {
        assert_eq!(
            agent_task_dispatch_requested_run_id(&[
                "homeboy".to_string(),
                "agent-task".to_string(),
                "cook".to_string(),
                "--run-id".to_string(),
                "cook-run".to_string(),
            ]),
            Some("cook-run".to_string())
        );
        assert_eq!(
            agent_task_dispatch_requested_run_id(&[
                "homeboy".to_string(),
                "agent-task".to_string(),
                "dispatch".to_string(),
                "--run-id=dispatch-run".to_string(),
            ]),
            Some("dispatch-run".to_string())
        );
        assert_eq!(
            agent_task_dispatch_requested_run_id(&[
                "homeboy".to_string(),
                "agent-task".to_string(),
                "status".to_string(),
                "dispatch-run".to_string(),
            ]),
            None
        );
    }

    #[test]
    fn agent_task_dispatch_requested_run_id_allows_global_flags_before_agent_task() {
        assert_eq!(
            agent_task_dispatch_requested_run_id(&[
                "homeboy".to_string(),
                "agent-task".to_string(),
                "dispatch".to_string(),
                "--run-id=dispatch-run".to_string(),
            ]),
            Some("dispatch-run".to_string())
        );
    }

    #[test]
    fn ensure_agent_task_dispatch_run_id_preserves_existing_id() {
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--run-id".to_string(),
            "cook-run".to_string(),
            "--repo".to_string(),
            "homeboy".to_string(),
        ];

        let (out, run_id) = ensure_agent_task_dispatch_run_id(&args).expect("agent task args");

        assert_eq!(out, args);
        assert_eq!(run_id, "cook-run");
    }

    #[test]
    fn ensure_agent_task_dispatch_run_id_injects_id_before_dispatch_options() {
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--repo".to_string(),
            "homeboy".to_string(),
        ];

        let (out, run_id) = ensure_agent_task_dispatch_run_id(&args).expect("agent task args");

        assert!(run_id.starts_with("agent-task-"));
        // `--run-id` is injected right after the `agent-task <action>` prefix,
        // ahead of the dispatch options.
        assert_eq!(out[0], "homeboy");
        assert_eq!(out[1], "agent-task");
        assert_eq!(out[2], "cook");
        assert_eq!(out[3], "--run-id");
        assert_eq!(out[4], run_id);
        assert_eq!(out[5], "--repo");
        assert_eq!(out[6], "homeboy");
    }

    #[test]
    fn ensure_agent_task_dispatch_run_id_with_uses_preferred_id_when_unset() {
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--repo".to_string(),
            "homeboy".to_string(),
        ];

        let (out, run_id) = ensure_agent_task_dispatch_run_id_with(&args, Some("iso-token"))
            .expect("agent task args");

        assert_eq!(run_id, "iso-token");
        assert!(out.contains(&"--run-id".to_string()));
        assert!(out.contains(&"iso-token".to_string()));
    }

    #[test]
    fn ensure_agent_task_dispatch_run_id_with_preserves_explicit_run_id() {
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--run-id".to_string(),
            "explicit-run".to_string(),
        ];

        let (out, run_id) = ensure_agent_task_dispatch_run_id_with(&args, Some("iso-token"))
            .expect("agent task args");

        // An explicit --run-id always wins over the preferred isolation token.
        assert_eq!(run_id, "explicit-run");
        assert_eq!(out, args);
    }

    #[test]
    fn lab_cook_uses_one_first_attempt_identity_across_the_handoff() {
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--run-id".to_string(),
            "cook-7970".to_string(),
        ];

        let (out, lifecycle_run_id) =
            ensure_agent_task_lifecycle_identity_with(&args, None, None).expect("cook identity");

        assert_eq!(out[3], "--attempt-run-id");
        assert_eq!(out[4], lifecycle_run_id);
        assert_eq!(out[5], "--run-id");
        assert_eq!(out[6], "cook-7970");
        assert!(lifecycle_run_id.starts_with("cook-7970-attempt-1-"));

        let (staged_args, staged_lifecycle_run_id) =
            ensure_agent_task_lifecycle_identity_with(&out, None, None)
                .expect("staged cook identity");

        assert_eq!(staged_args, out);
        assert_eq!(staged_lifecycle_run_id, lifecycle_run_id);
    }

    #[test]
    fn lab_cook_staging_preserves_generated_durable_lifecycle_identity() {
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--repo".to_string(),
            "homeboy".to_string(),
        ];

        let (pre_acceptance_args, pre_acceptance_run_id) =
            ensure_agent_task_lifecycle_identity_with(&args, Some("cook-8005"), None)
                .expect("pre-acceptance cook identity");
        let (staged_args, staged_run_id) = ensure_agent_task_lifecycle_identity_with(
            &pre_acceptance_args,
            Some("other-token"),
            None,
        )
        .expect("staged cook identity");

        assert!(pre_acceptance_run_id.starts_with("cook-8005-attempt-1-"));
        assert_eq!(staged_run_id, pre_acceptance_run_id);
        assert_eq!(staged_args, pre_acceptance_args);
        assert_eq!(
            agent_task_dispatch_requested_run_id(&staged_args),
            Some("cook-8005".to_string())
        );
    }

    #[test]
    fn lab_cook_preserves_explicit_attempt_identity_for_drift_detection() {
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--attempt-run-id".to_string(),
            "unexpected-attempt".to_string(),
            "--run-id".to_string(),
            "cook-8009".to_string(),
        ];

        let (out, lifecycle_run_id) = ensure_agent_task_lifecycle_identity_with(
            &args,
            Some("cook-8009"),
            Some("cook-8009-attempt-1-canonical"),
        )
        .expect("cook identity");

        assert_eq!(out, args);
        assert_eq!(lifecycle_run_id, "unexpected-attempt");
    }

    #[test]
    fn ensure_agent_task_dispatch_run_id_with_uses_materialized_run_plan_id() {
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "run-plan".to_string(),
            "--plan".to_string(),
            "@/runner/retry-plan.json".to_string(),
            "--record-run-id".to_string(),
            "retry-run".to_string(),
        ];

        let (out, run_id) = ensure_agent_task_dispatch_run_id_with(&args, None)
            .expect("materialized run-plan has a durable run id");

        assert_eq!(run_id, "retry-run");
        assert_eq!(out, args);
    }

    #[test]
    fn dispatch_run_isolation_token_reuses_explicit_run_id() {
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "dispatch".to_string(),
            "--run-id".to_string(),
            "explicit-run".to_string(),
        ];

        assert_eq!(
            agent_task_dispatch_run_isolation_token(&args),
            Some("explicit-run".to_string())
        );
    }

    #[test]
    fn dispatch_run_isolation_token_generates_for_unset_run_id() {
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--repo".to_string(),
            "homeboy".to_string(),
        ];

        let token = agent_task_dispatch_run_isolation_token(&args).expect("token");
        assert!(token.starts_with("agent-task-"));
    }

    #[test]
    fn dispatch_run_isolation_token_none_for_non_dispatch_commands() {
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "status".to_string(),
            "run-1".to_string(),
        ];

        assert!(agent_task_dispatch_run_isolation_token(&args).is_none());
    }

    #[test]
    fn ensure_agent_task_dispatch_run_id_ignores_other_agent_task_commands() {
        assert!(ensure_agent_task_dispatch_run_id(&[
            "homeboy".to_string(),
            "agent-task".to_string(),
            "status".to_string(),
            "run-1".to_string(),
        ])
        .is_none());
    }

    #[test]
    fn materializes_inline_agent_task_cook_tasks_json() {
        let prompt = "Cook sensitive implementation details";
        let tasks = serde_json::json!([{ "prompt": prompt }]).to_string();
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--tasks".to_string(),
            tasks.clone(),
            "--concurrency".to_string(),
            "4".to_string(),
        ];

        let (rewritten, entry) = materialize_inline_agent_task_tasks_arg_with(&args, |spec| {
            assert_eq!(spec, tasks);
            Ok(Some(fake_synced_file(
                "@/remote/input/agent-task-tasks.json",
                "agent_task_tasks_remapped",
            )))
        })
        .expect("rewrite tasks arg");

        assert_eq!(
            rewritten,
            vec![
                "homeboy".to_string(),
                "agent-task".to_string(),
                "cook".to_string(),
                "--tasks".to_string(),
                "@/remote/input/agent-task-tasks.json".to_string(),
                "--concurrency".to_string(),
                "4".to_string(),
            ]
        );
        assert!(!rewritten.join(" ").contains(prompt));
        assert_eq!(entry.expect("mapping entry").remote_path(), "/remote/input");
    }

    #[cfg(unix)]
    #[test]
    fn remapped_agent_task_plan_is_owner_only_before_snapshot_sync() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let plan_file = temp.path().join("agent-task-attempt-plan.json");

        write_private_remapped_agent_task_plan(&plan_file, r#"{"plan_id":"private"}"#)
            .expect("write private plan");

        assert_eq!(
            std::fs::metadata(&plan_file)
                .expect("plan metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn leaves_agent_task_tasks_file_specs_in_argv() {
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "dispatch".to_string(),
            "--tasks=@tasks.json".to_string(),
        ];

        let (rewritten, entry) = materialize_inline_agent_task_tasks_arg_with(&args, |spec| {
            assert_eq!(spec, "@tasks.json");
            Ok(None)
        })
        .expect("rewrite tasks arg");

        assert_eq!(rewritten, args);
        assert!(entry.is_none());
    }

    fn fake_synced_file(remote_spec: &str, role: &str) -> (String, LabWorkspaceMappingEntry) {
        let synced = crate::core::runner::RunnerWorkspaceSyncOutput {
            variant: "workspace_sync",
            command: "runner.workspace.sync",
            runner_id: "lab".to_string(),
            local_path: "/local/input".to_string(),
            remote_path: "/remote/input".to_string(),
            materialization_plan:
                crate::core::runner::RunnerWorkspaceMaterializationPlan::from_test_parts(
                    "/remote",
                    "/local/input",
                    "input",
                    "/remote/input",
                    RunnerWorkspaceSyncMode::Snapshot,
                    "snapshot",
                ),
            current_workspace: crate::core::runner::RunnerWorkspaceCurrentSummary {
                local_path: "/local/input".to_string(),
                remote_path: "/remote/input".to_string(),
                sync_mode: RunnerWorkspaceSyncMode::Snapshot,
                materialized: true,
                source_commit: None,
                source_ref: None,
                source_dirty: None,
                synthetic_checkout_commit: None,
                synthetic_checkout_ref: None,
                synthetic_checkout_tree: None,
            },
            workspace_lease: crate::core::runner::RunnerWorkspaceLease {
                runner_id: "lab".to_string(),
                local_path: "/local/input".to_string(),
                remote_path: "/remote/input".to_string(),
                sync_mode: "snapshot".to_string(),
                materialized: true,
                lifecycle_owner: crate::core::runner::RunnerLifecycleOwner::Controller,
                source_commit: None,
                source_ref: None,
                source_dirty: None,
            },
            resource_lifecycle: crate::core::runner::workspace_resource_lifecycle(
                "lab",
                "/remote/input",
                None,
                crate::core::resource_lifecycle_index::ResourceCleanupPolicy::DeleteOnSuccess,
            ),
            sync_mode: RunnerWorkspaceSyncMode::Snapshot,
            snapshot_identity: "snapshot".to_string(),
            counts: crate::core::runner::ByteFileCounts {
                files: 1,
                bytes: 42,
            },
            excludes: Vec::new(),
            includes: Vec::new(),
            workspace_cleanliness: "clean".to_string(),
            validation_dependencies: Vec::new(),
        };
        (
            remote_spec.to_string(),
            workspace_mapping_entry(role, &synced),
        )
    }

    #[test]
    fn pre_dispatch_failure_message_uses_structured_dependency_failure_envelope() {
        let output = r#"runtime setup log
{"schema":"homeboy/lab-dependency-failure/v1","dependency":{"id":"runtime-a","kind":"runtime component","path":"/remote/cache/runtime-a"},"message":"path missing","remediation":"refresh runtime cache"}
trailing log"#;

        let message = lab_pre_dispatch_failure_message(output).expect("message");

        assert!(message.contains("runtime component `/remote/cache/runtime-a`"));
        assert!(message.contains("path missing"));
        assert!(message.contains("refresh runtime cache"));
    }

    #[test]
    fn pre_dispatch_failure_message_uses_declared_dependency_pattern() {
        let output = "Error: lstat '/remote/cache/prepared-dependencies/runtime-a': no such file or directory";
        let patterns = vec![AgentTaskProviderDependencyFailurePattern {
            id: "fixture.dependency".to_string(),
            label: "Fixture dependency".to_string(),
            path_contains: "prepared-dependencies/".to_string(),
            error_contains_any: vec!["no such file or directory".to_string()],
            remediation: Some("refresh fixture dependencies".to_string()),
            extra: Default::default(),
        }];

        let message =
            lab_pre_dispatch_dependency_failure_message(output, &patterns).expect("message");

        assert!(message.contains("prepared-dependencies/runtime-a"));
        assert!(message.contains("refresh fixture dependencies"));
    }
}
