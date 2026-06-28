use super::*;
use crate::core::{agent_task_lifecycle, paths, Error, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

pub fn controller_status_report(loop_id: &str) -> Result<AgentTaskLoopControllerStatusReport> {
    let controller = controller_status(loop_id)?;
    let diagnostics = controller_status_diagnostics(&controller)?;
    Ok(AgentTaskLoopControllerStatusReport {
        schema: AGENT_TASK_LOOP_CONTROLLER_STATUS_SCHEMA.to_string(),
        controller,
        diagnostics,
    })
}

pub fn controller_status_diagnostics(
    record: &AgentTaskLoopControllerRecord,
) -> Result<AgentTaskLoopControllerDiagnostics> {
    controller_status_diagnostics_with(record, Utc::now(), |run_id| {
        agent_task_lifecycle::run_record_exists(run_id)
    })
}

pub(crate) fn controller_status_diagnostics_with<F>(
    record: &AgentTaskLoopControllerRecord,
    now: DateTime<Utc>,
    mut run_exists: F,
) -> Result<AgentTaskLoopControllerDiagnostics>
where
    F: FnMut(&str) -> Result<bool>,
{
    let mut pending_actions = Vec::new();
    let mut stale_pending_action_count = 0;
    let mut orphaned_pending_action_count = 0;
    let acceptance_gates = acceptance_gate_diagnostics(record);
    let failed_child_actions = failed_child_action_diagnostics(record);
    let missing_acceptance_gate_count = acceptance_gates
        .iter()
        .filter(|gate| gate.status == AgentTaskLoopAcceptanceGateStatus::Missing)
        .count();
    let failed_acceptance_gate_count = acceptance_gates
        .iter()
        .filter(|gate| gate.status == AgentTaskLoopAcceptanceGateStatus::Failed)
        .count();
    let pending_acceptance_gate_count = acceptance_gates
        .iter()
        .filter(|gate| gate.status == AgentTaskLoopAcceptanceGateStatus::Pending)
        .count();

    for action in record
        .next_actions
        .iter()
        .filter(|action| action.status == AgentTaskLoopActionStatus::Pending)
    {
        let age_seconds = parse_timestamp(&action.created_at).map(|created_at| {
            now.signed_duration_since(created_at.with_timezone(&Utc))
                .num_seconds()
                .max(0)
        });
        let stale = age_seconds.is_some_and(|age| age >= STALE_PENDING_ACTION_SECONDS);
        let runner_id = action_runner_id(action, record);
        let referenced_run_id = action_referenced_run_id(action, record);
        let missing_referenced_run = if let Some(run_id) = referenced_run_id.as_deref() {
            !run_exists(run_id)?
        } else {
            false
        };
        let orphaned = missing_referenced_run;
        let mut problems = Vec::new();
        if stale {
            problems.push("pending action is older than stale threshold".to_string());
        }
        if missing_referenced_run {
            problems.push("referenced run record is missing".to_string());
        }
        let recovery_commands = if stale || orphaned {
            recovery_commands_for(record, action)
        } else {
            Vec::new()
        };

        if stale {
            stale_pending_action_count += 1;
        }
        if orphaned {
            orphaned_pending_action_count += 1;
        }
        pending_actions.push(AgentTaskLoopPendingActionDiagnostic {
            action_id: action.action_id.clone(),
            action: action_name(&action.action).to_string(),
            dedupe_key: action.dedupe_key.clone(),
            runner_id,
            referenced_run_id,
            created_at: action.created_at.clone(),
            age_seconds,
            stale,
            orphaned,
            problems,
            recovery_commands,
        });
    }

    Ok(AgentTaskLoopControllerDiagnostics {
        schema: "homeboy/agent-task-loop-controller-diagnostics/v1".to_string(),
        stale_pending_threshold_seconds: STALE_PENDING_ACTION_SECONDS,
        summary: AgentTaskLoopControllerDiagnosticSummary {
            pending_action_count: pending_actions.len(),
            failed_child_action_count: failed_child_actions.len(),
            stale_pending_action_count,
            orphaned_pending_action_count,
            acceptance_gate_count: acceptance_gates.len(),
            missing_acceptance_gate_count,
            failed_acceptance_gate_count,
            pending_acceptance_gate_count,
        },
        failed_child_actions,
        pending_actions,
        acceptance_gates,
    })
}

fn failed_child_action_diagnostics(
    record: &AgentTaskLoopControllerRecord,
) -> Vec<AgentTaskLoopFailedChildActionDiagnostic> {
    record
        .next_actions
        .iter()
        .filter(|action| {
            matches!(
                action.status,
                AgentTaskLoopActionStatus::Failed
                    | AgentTaskLoopActionStatus::BlockedRunnerUnavailable
                    | AgentTaskLoopActionStatus::BlockedRemoteMaterialization
                    | AgentTaskLoopActionStatus::BlockedLocalFallbackDenied
            )
        })
        .map(|action| failed_child_action_diagnostic(record, action))
        .collect()
}

fn failed_child_action_diagnostic(
    record: &AgentTaskLoopControllerRecord,
    action: &AgentTaskLoopPolicyActionRecord,
) -> AgentTaskLoopFailedChildActionDiagnostic {
    let child_run_id = action_referenced_run_id(action, record);
    let child_run = child_run_id
        .as_deref()
        .and_then(|run_id| agent_task_lifecycle::status(run_id).ok());
    let child_run_status = child_run
        .as_ref()
        .map(|run| format!("{:?}", run.state).to_ascii_lowercase());
    let aggregate = child_run_id.as_deref().and_then(load_child_aggregate_value);
    let child_task_id = aggregate.as_ref().and_then(first_failed_task_id);
    let top_diagnostic = action_top_diagnostic(action)
        .or_else(|| aggregate.as_ref().and_then(first_diagnostic))
        .unwrap_or_else(|| CollectedDiagnostic {
            class: "controller_child_action_failed".to_string(),
            message: "controller child action failed".to_string(),
        });
    let hydrated_root_cause = child_run
        .as_ref()
        .and_then(root_cause_from_run_evidence)
        .or_else(|| aggregate.as_ref().and_then(root_cause_from_aggregate))
        .filter(|message| message != &top_diagnostic.message)
        .filter(|message| {
            diagnostic_priority("", message) <= diagnostic_priority("", &top_diagnostic.message)
        });
    let evidence_refs = child_run
        .as_ref()
        .map(evidence_refs_from_run)
        .unwrap_or_default();
    let artifact_dir = child_run.as_ref().and_then(run_artifact_dir);
    let owner_surface = classify_failed_child_owner(
        hydrated_root_cause
            .as_deref()
            .unwrap_or(&top_diagnostic.message),
        &evidence_refs,
    );
    let signature_root = hydrated_root_cause
        .clone()
        .unwrap_or_else(|| top_diagnostic.message.clone());
    let failure_signature = failed_child_failure_signature(
        child_run_id.as_deref(),
        child_task_id.as_deref(),
        Some(top_diagnostic.class.as_str()),
        &signature_root,
        &owner_surface,
    );
    let repeated_failure = repeated_failure_diagnostic(record, &failure_signature);
    let next_command = child_run_id
        .as_ref()
        .map(|run_id| format!("homeboy agent-task status {run_id} --full"))
        .unwrap_or_else(|| {
            format!(
                "homeboy agent-task controller run {} --action-id {}",
                record.loop_id, action.action_id
            )
        });

    AgentTaskLoopFailedChildActionDiagnostic {
        action_id: action.action_id.clone(),
        dedupe_key: action.dedupe_key.clone(),
        child_run_id,
        child_task_id,
        child_run_status,
        top_diagnostic: top_diagnostic.message,
        top_diagnostic_class: Some(top_diagnostic.class),
        hydrated_root_cause,
        artifact_dir,
        owner_surface,
        failure_signature,
        repeated_failure,
        next_command,
        evidence_refs,
    }
}

fn action_top_diagnostic(action: &AgentTaskLoopPolicyActionRecord) -> Option<CollectedDiagnostic> {
    action
        .diagnostics
        .first()
        .map(|diagnostic| CollectedDiagnostic {
            class: diagnostic.code.clone(),
            message: diagnostic.message.clone(),
        })
}

fn first_failed_task_id(value: &Value) -> Option<String> {
    value
        .get("outcomes")
        .and_then(Value::as_array)?
        .iter()
        .find(|outcome| {
            outcome
                .get("status")
                .and_then(Value::as_str)
                .is_some_and(|status| status != "succeeded" && status != "no_op")
        })
        .and_then(|outcome| outcome.get("task_id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn run_artifact_dir(run: &agent_task_lifecycle::AgentTaskRunRecord) -> Option<String> {
    run.aggregate_path
        .as_deref()
        .and_then(|path| Path::new(path).parent())
        .map(|path| path.display().to_string())
}

fn failed_child_failure_signature(
    child_run_id: Option<&str>,
    task_id: Option<&str>,
    diagnostic_class: Option<&str>,
    root_message: &str,
    owner_surface: &str,
) -> AgentTaskLoopFailureSignature {
    let normalized_message = normalize_signature_text(root_message);
    let signature_material = format!(
        "{}\n{}\n{}",
        owner_surface,
        diagnostic_class.unwrap_or(""),
        normalized_message
    );
    let digest = format!("sha256:{:x}", Sha256::digest(signature_material.as_bytes()));
    AgentTaskLoopFailureSignature {
        digest,
        task_id: task_id.or(child_run_id).map(str::to_string),
        diagnostic_class: diagnostic_class.map(str::to_string),
        root_message: root_message.to_string(),
        owner_surface: owner_surface.to_string(),
    }
}

fn repeated_failure_diagnostic(
    record: &AgentTaskLoopControllerRecord,
    signature: &AgentTaskLoopFailureSignature,
) -> Option<AgentTaskLoopRepeatedFailureDiagnostic> {
    let matching_failed_child_action_count = record
        .next_actions
        .iter()
        .filter(|action| {
            matches!(action.status, AgentTaskLoopActionStatus::Failed)
                && action_top_diagnostic(action).is_some_and(|diagnostic| {
                    failed_child_failure_signature(
                        action_referenced_run_id(action, record).as_deref(),
                        None,
                        Some(diagnostic.class.as_str()),
                        &diagnostic.message,
                        &signature.owner_surface,
                    )
                    .digest
                        == signature.digest
                })
        })
        .count();
    (matching_failed_child_action_count > 1).then(|| AgentTaskLoopRepeatedFailureDiagnostic {
        matching_failed_child_action_count,
        guidance: "This failure signature has repeated in this controller; inspect the child input or provider boundary before another full rerun.".to_string(),
        next_command: "homeboy agent-task evidence <child-run-id> --failure-only".to_string(),
    })
}

fn normalize_signature_text(message: &str) -> String {
    message
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn load_child_aggregate_value(run_id: &str) -> Option<Value> {
    let (raw, _) = agent_task_lifecycle::aggregate_source(run_id).ok()?;
    serde_json::from_str(&raw).ok()
}

fn evidence_refs_from_run(
    run: &agent_task_lifecycle::AgentTaskRunRecord,
) -> Vec<AgentTaskLoopFailedChildEvidenceRef> {
    let mut refs = Vec::new();
    for artifact in &run.artifact_refs {
        push_failed_child_evidence_ref(
            &mut refs,
            AgentTaskLoopFailedChildEvidenceRef {
                kind: artifact.kind.clone(),
                uri: artifact.uri.clone(),
                label: artifact.label.clone(),
            },
        );
    }
    if let Some(executor) = &run.latest_executor_evidence {
        for evidence in executor.refs() {
            push_failed_child_evidence_ref(
                &mut refs,
                AgentTaskLoopFailedChildEvidenceRef {
                    kind: evidence.kind,
                    uri: evidence.uri,
                    label: evidence.label,
                },
            );
        }
    }
    refs
}

fn root_cause_from_run_evidence(run: &agent_task_lifecycle::AgentTaskRunRecord) -> Option<String> {
    let executor = run.latest_executor_evidence.as_ref()?;
    let mut candidates = Vec::new();
    for evidence in executor.refs() {
        let Some(path) = evidence.uri.strip_prefix("file://") else {
            continue;
        };
        let Ok(raw) = fs::read_to_string(path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        candidates.extend(collect_diagnostics(&value));
    }
    candidates
        .sort_by_key(|diagnostic| diagnostic_priority(&diagnostic.class, &diagnostic.message));
    candidates
        .into_iter()
        .find(|diagnostic| is_root_cause_message(&diagnostic.message))
        .map(|diagnostic| diagnostic.message)
}

fn push_failed_child_evidence_ref(
    refs: &mut Vec<AgentTaskLoopFailedChildEvidenceRef>,
    reference: AgentTaskLoopFailedChildEvidenceRef,
) {
    if reference.uri.trim().is_empty() {
        return;
    }
    if !refs
        .iter()
        .any(|existing| existing.kind == reference.kind && existing.uri == reference.uri)
    {
        refs.push(reference);
    }
}

fn first_diagnostic(value: &Value) -> Option<CollectedDiagnostic> {
    collect_diagnostics(value).into_iter().next()
}

fn root_cause_from_aggregate(value: &Value) -> Option<String> {
    collect_diagnostics(value)
        .into_iter()
        .find(|diagnostic| is_root_cause_message(&diagnostic.message))
        .map(|diagnostic| diagnostic.message)
}

#[derive(Clone)]
struct CollectedDiagnostic {
    class: String,
    message: String,
}

fn collect_diagnostics(value: &Value) -> Vec<CollectedDiagnostic> {
    let mut diagnostics = Vec::new();
    collect_diagnostics_into(value, &mut diagnostics);
    let mut seen = std::collections::HashSet::new();
    diagnostics.retain(|diagnostic| {
        seen.insert((
            diagnostic.class.to_ascii_lowercase(),
            diagnostic.message.clone(),
        ))
    });
    diagnostics
        .sort_by_key(|diagnostic| diagnostic_priority(&diagnostic.class, &diagnostic.message));
    diagnostics
}

fn collect_diagnostics_into(value: &Value, diagnostics: &mut Vec<CollectedDiagnostic>) {
    match value {
        Value::Object(map) => {
            if let Some(Value::Array(items)) = map.get("diagnostics") {
                for diagnostic in items {
                    if let Some(message) = diagnostic
                        .get("message")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|message| !message.is_empty())
                    {
                        let class = diagnostic
                            .get("class")
                            .or_else(|| diagnostic.get("kind"))
                            .or_else(|| diagnostic.get("level"))
                            .and_then(Value::as_str)
                            .unwrap_or("nested")
                            .to_string();
                        diagnostics.push(CollectedDiagnostic {
                            class,
                            message: message.to_string(),
                        });
                    }
                }
            }
            for nested in map.values() {
                collect_diagnostics_into(nested, diagnostics);
            }
        }
        Value::Array(items) => {
            for nested in items {
                collect_diagnostics_into(nested, diagnostics);
            }
        }
        _ => {}
    }
}

fn diagnostic_priority(class: &str, message: &str) -> u8 {
    let text = format!("{} {}", class, message).to_ascii_lowercase();
    if text.contains("typed_artifacts_missing")
        || text.contains("required_typed_artifacts_missing")
        || text.contains("required typed artifacts")
        || text.contains("declared artifact result envelope")
    {
        8
    } else if text.contains("valid") || text.contains("recipe") || text.contains("schema") {
        0
    } else if text.contains("fatal") || text.contains("error") || text.contains("exception") {
        1
    } else if text.contains("registr")
        || text.contains("provider")
        || text.contains("discovery")
        || text.contains("capability")
    {
        2
    } else if text.contains("missing")
        || text.contains("not_found")
        || text.contains("path")
        || text.contains("io")
    {
        3
    } else {
        9
    }
}

fn is_root_cause_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("runtime_task_ability_unavailable")
        || lower.contains("root cause")
        || lower.contains("recipe")
        || lower.contains("validation")
        || lower.contains("php fatal")
        || lower.contains("fatal error")
        || lower.contains("missing required")
        || lower.contains("provider")
        || lower.contains("credential")
        || lower.contains("secret")
}

fn classify_failed_child_owner(
    diagnostic: &str,
    evidence_refs: &[AgentTaskLoopFailedChildEvidenceRef],
) -> String {
    let lower = diagnostic.to_ascii_lowercase();
    if lower.contains("runtime_task_ability_unavailable") || lower.contains("ability unavailable") {
        "agent_runtime".to_string()
    } else if lower.contains("credential") || lower.contains("secret") || lower.contains("token") {
        "provider_credentials".to_string()
    } else if lower.contains("repo spec")
        || lower.contains("spec")
        || lower.contains("invalid input")
    {
        "repo_spec".to_string()
    } else if lower.contains("artifact") {
        "workload_artifacts".to_string()
    } else if lower.contains("agent runtime")
        || lower.contains("agent_runtime")
        || lower.contains("runtime")
        || evidence_refs
            .iter()
            .any(|reference| reference.uri.to_ascii_lowercase().contains("runtime"))
    {
        "agent_runtime".to_string()
    } else if lower.contains("provider") {
        "provider".to_string()
    } else {
        "homeboy".to_string()
    }
}

fn acceptance_gate_diagnostics(
    record: &AgentTaskLoopControllerRecord,
) -> Vec<AgentTaskLoopAcceptanceGateDiagnostic> {
    let mut declared = BTreeSet::new();
    for action in &record.next_actions {
        if let AgentTaskLoopPolicyAction::RunGates {
            bundle_id,
            entity_id,
        } = &action.action
        {
            declared.insert((bundle_id.clone(), entity_id.clone()));
        }
    }
    for result in &record.gate_results {
        declared.insert((result.bundle_id.clone(), result.entity_id.clone()));
    }
    for bundle in &record.gate_bundles {
        if !declared
            .iter()
            .any(|(bundle_id, _)| bundle_id == &bundle.bundle_id)
        {
            declared.insert((bundle.bundle_id.clone(), None));
        }
    }

    declared
        .into_iter()
        .map(|(bundle_id, entity_id)| {
            let result = record
                .gate_results
                .iter()
                .rev()
                .find(|result| result.bundle_id == bundle_id && result.entity_id == entity_id);
            let status =
                AgentTaskLoopAcceptanceGateStatus::from(result.map(|result| result.status));
            let problems = match status {
                AgentTaskLoopAcceptanceGateStatus::Missing => {
                    vec!["acceptance gate has no recorded result".to_string()]
                }
                AgentTaskLoopAcceptanceGateStatus::Failed => {
                    vec!["acceptance gate recorded a failed result".to_string()]
                }
                AgentTaskLoopAcceptanceGateStatus::Pending => {
                    vec!["acceptance gate is pending an external/manual result".to_string()]
                }
                AgentTaskLoopAcceptanceGateStatus::Satisfied
                | AgentTaskLoopAcceptanceGateStatus::Warning => Vec::new(),
            };

            AgentTaskLoopAcceptanceGateDiagnostic {
                bundle_id,
                entity_id,
                status,
                result_id: result.map(|result| result.result_id.clone()),
                result_status: result.map(|result| result.status),
                recorded_at: result.map(|result| result.recorded_at.clone()),
                problems,
            }
        })
        .collect()
}

pub fn create_controller(
    loop_id: &str,
    phase: &str,
    config_version: &str,
) -> Result<AgentTaskLoopControllerRecord> {
    let record = AgentTaskLoopControllerRecord::new(loop_id, phase, config_version);
    write_controller(&record)?;
    Ok(record)
}

pub fn load_controller(loop_id: &str) -> Result<AgentTaskLoopControllerRecord> {
    read_json(&controller_path(&sanitize_loop_id(loop_id))?)
}

pub fn controller_status(loop_id: &str) -> Result<AgentTaskLoopControllerRecord> {
    let mut record = load_controller(loop_id)?;
    let refreshed_child_runs = refresh_stale_running_child_actions(&mut record)?;
    let refreshed_subcontrollers = refresh_subcontroller_statuses(&mut record)?;
    if refreshed_child_runs || refreshed_subcontrollers {
        write_controller(&record)?;
    }
    Ok(record)
}

pub fn list_controllers() -> Result<Vec<AgentTaskLoopControllerRecord>> {
    let root = controllers_root()?;
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(Error::internal_io(
                error.to_string(),
                Some(root.display().to_string()),
            ));
        }
    };
    let mut records = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            Error::internal_io(error.to_string(), Some(root.display().to_string()))
        })?;
        let path = entry.path().join("controller.json");
        if path.exists() {
            records.push(read_json(&path)?);
        }
    }
    records.sort_by(|left: &AgentTaskLoopControllerRecord, right| left.loop_id.cmp(&right.loop_id));
    Ok(records)
}

pub fn write_controller(record: &AgentTaskLoopControllerRecord) -> Result<()> {
    write_json(&controller_path(&record.loop_id)?, record)
}

pub fn apply_external_event(
    loop_id: &str,
    event: AgentTaskLoopExternalEvent,
) -> Result<AgentTaskLoopControllerRecord> {
    let mut record = load_controller(loop_id)?;
    record.apply_event(event);
    write_controller(&record)?;
    Ok(record)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &PathBuf) -> Result<T> {
    let raw = fs::read_to_string(path)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    serde_json::from_str(&raw)
        .map_err(|error| Error::internal_json(error.to_string(), Some(path.display().to_string())))
}

fn write_json<T: Serialize>(path: &PathBuf, value: &T) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        Error::internal_unexpected(format!("path has no parent: {}", path.display()))
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        Error::internal_io(error.to_string(), Some(parent.display().to_string()))
    })?;
    let json = serde_json::to_string_pretty(value).map_err(|error| {
        Error::internal_json(error.to_string(), Some(path.display().to_string()))
    })?;
    fs::write(path, format!("{json}\n"))
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))
}

pub fn controller_record_path(loop_id: &str) -> Result<PathBuf> {
    controller_path(loop_id)
}

fn controller_path(loop_id: &str) -> Result<PathBuf> {
    Ok(controllers_root()?
        .join(sanitize_loop_id(loop_id))
        .join("controller.json"))
}

fn controllers_root() -> Result<PathBuf> {
    Ok(paths::homeboy_data()?.join("agent-task-loops"))
}
