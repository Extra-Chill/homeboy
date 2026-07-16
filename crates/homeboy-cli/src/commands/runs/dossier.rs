use serde::Serialize;
use serde_json::Value;

use homeboy::core::observation::evidence_report;
use homeboy::core::observation::{runs_service, ObservationStore, RunEvidenceCommands, RunRecord};
use homeboy::core::validation_progress::ValidationProgressLedger;

use super::common::RunSummary;
use super::{reconcile, run_summary, CmdResult, RunsOutput};

#[derive(Serialize)]
pub struct RunsDossierOutput {
    pub command: &'static str,
    pub run_id: String,
    pub run_ref: String,
    pub status: RunsDossierStatus,
    pub run: RunSummary,
    pub refs: RunsDossierRefs,
    pub failure: evidence_report::EvidenceFailureSummary,
    pub env: RunsDossierEnvSummary,
    pub artifacts: RunsDossierArtifactSummary,
    pub inspection_commands: Vec<RunsDossierCommandHint>,
    pub repair_commands: Vec<RunsDossierCommandHint>,
    pub next_commands: Vec<RunsDossierCommandHint>,
}

#[derive(Serialize)]
pub struct RunsDossierStatus {
    pub status: String,
    pub stale: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
}

#[derive(Serialize)]
pub struct RunsDossierRefs {
    pub show_command: String,
    #[serde(flatten)]
    pub evidence_commands: RunEvidenceCommands,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handoff_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_ref: Option<String>,
}

#[derive(Serialize)]
pub struct RunsDossierEnvSummary {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    pub values_redacted: bool,
    pub key_count: usize,
    pub secret_key_count: usize,
    pub public_key_count: usize,
    pub shadowed_key_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    pub note: String,
}

#[derive(Serialize)]
pub struct RunsDossierArtifactSummary {
    pub count: usize,
    pub file_count: usize,
    pub directory_count: usize,
    pub url_count: usize,
    pub missing_count: usize,
    pub reviewer_visible_count: usize,
    pub fetchable_count: usize,
    pub artifacts: Vec<RunsDossierArtifactRef>,
}

#[derive(Serialize)]
pub struct RunsDossierArtifactRef {
    pub artifact_id: String,
    pub kind: String,
    #[serde(rename = "type")]
    pub artifact_type: String,
    pub ref_id: String,
    pub reviewer_visible: bool,
    pub visibility_hint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fetch_command: Option<String>,
}

#[derive(Serialize)]
pub struct RunsDossierCommandHint {
    pub label: String,
    pub command: String,
    pub reason: String,
}

pub fn runs_dossier(run_id: &str) -> CmdResult<RunsOutput> {
    let store = ObservationStore::open_initialized()?;
    reconcile::reconcile_owned_stale_running_runs(&store, 1000)?;
    runs_service::refresh_mirrored_daemon_evidence_best_effort(run_id);
    let run = runs_service::require_run(&store, run_id)?;
    let artifacts = runs_service::list_artifacts_for_run(&store, run_id)?;
    let artifact_index = evidence_report::evidence_artifact_index(&artifacts);
    let failure = evidence_report::evidence_failure_summary(&run);
    let stale_reason = reconcile::running_status_note(&run);
    let env = env_summary(&run);
    let validation_progress = ValidationProgressLedger::from_run(&run);

    Ok((
        RunsOutput::Dossier(RunsDossierOutput {
            command: "runs.dossier",
            run_id: run.id.clone(),
            run_ref: format!("homeboy://run/{}", run.id),
            status: RunsDossierStatus {
                status: run.status.clone(),
                stale: stale_reason.is_some(),
                stale_reason,
                category: status_category(&run, &failure),
                finished_at: run.finished_at.clone(),
            },
            run: run_summary(run.clone()),
            refs: refs(&run),
            failure,
            env,
            artifacts: artifact_summary(artifact_index),
            inspection_commands: inspection_commands(&run),
            repair_commands: repair_commands(&run, &validation_progress),
            next_commands: next_commands(&run, validation_progress.as_ref()),
        }),
        0,
    ))
}

fn status_category(
    run: &RunRecord,
    failure: &evidence_report::EvidenceFailureSummary,
) -> Option<String> {
    if !failure.gate_failures.is_empty() {
        return Some("gate_failure".to_string());
    }
    if failure.error.is_some() || !failure.failure.is_null() {
        return Some("execution_failure".to_string());
    }
    matches!(run.status.as_str(), "stale").then(|| "stale_running_record".to_string())
}

fn refs(run: &RunRecord) -> RunsDossierRefs {
    RunsDossierRefs {
        show_command: format!("homeboy runs show {}", shell_arg(&run.id)),
        evidence_commands: RunEvidenceCommands::for_run_id(&shell_arg(&run.id)),
        job_ref: first_string_at_any(
            &run.metadata_json,
            &[
                &["job_ref"],
                &["runner_job_id"],
                &["job", "id"],
                &["identity", "runner_job_id"],
            ],
        ),
        handoff_ref: first_string_at_any(
            &run.metadata_json,
            &[
                &["handoff_ref"],
                &["handoff", "ref"],
                &["handoff", "id"],
                &["plan_id"],
            ],
        ),
        result_ref: first_string_at_any(
            &run.metadata_json,
            &[
                &["result_ref"],
                &["result", "ref"],
                &["result", "id"],
                &["output_ref"],
            ],
        ),
    }
}

fn env_summary(run: &RunRecord) -> RunsDossierEnvSummary {
    let Some(envelope) = run.metadata_json.get("env_resolution") else {
        return RunsDossierEnvSummary {
            available: false,
            schema: None,
            values_redacted: true,
            key_count: 0,
            secret_key_count: 0,
            public_key_count: 0,
            shadowed_key_count: 0,
            command: None,
            note: "No redacted environment provenance metadata recorded for this run.".to_string(),
        };
    };
    let keys = envelope
        .get("keys")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let values_redacted = envelope
        .get("values_redacted")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    RunsDossierEnvSummary {
        available: true,
        schema: envelope
            .get("schema")
            .and_then(Value::as_str)
            .map(str::to_string),
        values_redacted,
        key_count: keys.len(),
        secret_key_count: keys
            .iter()
            .filter(|key| key.get("classification").and_then(Value::as_str) == Some("secret"))
            .count(),
        public_key_count: keys
            .iter()
            .filter(|key| key.get("classification").and_then(Value::as_str) == Some("public"))
            .count(),
        shadowed_key_count: keys
            .iter()
            .filter(|key| {
                key.get("shadowed_source_layers")
                    .and_then(Value::as_array)
                    .is_some_and(|layers| !layers.is_empty())
            })
            .count(),
        command: values_redacted.then(|| format!("homeboy runs env {}", shell_arg(&run.id))),
        note: if values_redacted {
            "Redacted environment provenance is available.".to_string()
        } else {
            "Environment provenance exists but is not marked redacted; Homeboy will not print values."
                .to_string()
        },
    }
}

fn artifact_summary(index: evidence_report::EvidenceArtifactIndex) -> RunsDossierArtifactSummary {
    let mut reviewer_visible_count = 0;
    let mut fetchable_count = 0;
    let artifacts = index
        .artifacts
        .into_iter()
        .map(|artifact| {
            let reviewer_visible = artifact.address.reviewer_visible;
            if reviewer_visible {
                reviewer_visible_count += 1;
            }
            if artifact.fetch_command.is_some() {
                fetchable_count += 1;
            }
            let target = artifact
                .public_url
                .clone()
                .or_else(|| artifact.fetch_command.clone());
            RunsDossierArtifactRef {
                artifact_id: artifact.id,
                kind: artifact.kind,
                artifact_type: artifact.artifact_type,
                ref_id: format!(
                    "homeboy://run/{}/artifact/{}",
                    artifact.reference.run_id, artifact.reference.id
                ),
                reviewer_visible,
                visibility_hint: if reviewer_visible {
                    "reviewer-visible".to_string()
                } else if artifact.fetch_command.is_some() {
                    "operator-local; fetch before sharing with reviewers".to_string()
                } else {
                    "not reviewer-visible from the recorded address".to_string()
                },
                target,
                fetch_command: artifact.fetch_command,
            }
        })
        .collect();

    RunsDossierArtifactSummary {
        count: index.count,
        file_count: index.file_count,
        directory_count: index.directory_count,
        url_count: index.url_count,
        missing_count: index.missing_count,
        reviewer_visible_count,
        fetchable_count,
        artifacts,
    }
}

fn inspection_commands(run: &RunRecord) -> Vec<RunsDossierCommandHint> {
    let run_id = shell_arg(&run.id);
    let mut commands = vec![
        command_hint(
            "show",
            format!("homeboy runs show {run_id}"),
            "Inspect compact run metadata and artifacts.",
        ),
        command_hint(
            "show-json",
            format!("homeboy runs show {run_id} --json"),
            "Inspect the full persisted run payload.",
        ),
        command_hint(
            "evidence",
            format!("homeboy runs evidence {run_id}"),
            "Inspect reviewer-facing evidence, artifact addresses, failure summary, and retention hints.",
        ),
        command_hint(
            "artifacts",
            format!("homeboy runs artifacts {run_id}"),
            "List all recorded artifact rows.",
        ),
    ];
    if run.metadata_json.get("env_resolution").is_some() {
        commands.push(command_hint(
            "env",
            format!("homeboy runs env {run_id}"),
            "Inspect redacted environment provenance when available.",
        ));
    }
    commands
}

fn repair_commands(
    run: &RunRecord,
    validation_progress: &Option<ValidationProgressLedger>,
) -> Vec<RunsDossierCommandHint> {
    let run_id = shell_arg(&run.id);
    let mut commands = Vec::new();
    if run.status == "running" {
        commands.push(command_hint(
            "reconcile-running",
            "homeboy runs reconcile".to_string(),
            "Mark orphaned running records stale after owner-process checks.",
        ));
    }
    if validation_progress.is_some() {
        commands.push(command_hint(
            "resume-plan",
            format!("homeboy runs resume-plan {run_id}"),
            "Inspect the generic validation-progress ledger before resuming work.",
        ));
    }
    commands
}

fn next_commands(
    run: &RunRecord,
    validation_progress: Option<&ValidationProgressLedger>,
) -> Vec<RunsDossierCommandHint> {
    let run_id = shell_arg(&run.id);
    let mut commands = vec![command_hint(
        "export",
        format!("homeboy runs export --run {run_id} --output <dir>"),
        "Create a portable metadata bundle for handoff or review.",
    )];
    if validation_progress
        .and_then(|ledger| ledger.next_command.as_ref())
        .is_some()
    {
        commands.push(command_hint(
            "resume-plan",
            format!("homeboy runs resume-plan {run_id}"),
            "A next validation command is recorded in the ledger.",
        ));
    }
    commands
}

fn command_hint(label: &str, command: String, reason: &str) -> RunsDossierCommandHint {
    RunsDossierCommandHint {
        label: label.to_string(),
        command,
        reason: reason.to_string(),
    }
}

fn first_string_at_any(value: &Value, paths: &[&[&str]]) -> Option<String> {
    paths.iter().find_map(|path| string_at(value, path))
}

fn string_at(value: &Value, path: &[&str]) -> Option<String> {
    let mut current = value;
    for segment in path {
        current = current.get(*segment)?;
    }
    current.as_str().map(str::to_string)
}

fn shell_arg(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':'))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}
