use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::commands::utils::output::command_output_stem;

use super::{ReconcileOutput, ReconcileRunIssueTotals};

pub(super) enum OutputInspection {
    Missing(String),
    Malformed(String),
    Valid(Value),
}

pub(super) fn discover_output_dir(output_dir: Option<String>) -> homeboy::core::Result<PathBuf> {
    match output_dir.or_else(|| std::env::var("HOMEBOY_OUTPUT_DIR").ok()) {
        Some(dir) if !dir.trim().is_empty() => Ok(PathBuf::from(dir)),
        _ => Err(homeboy::core::Error::validation_invalid_argument(
            "output-dir",
            "Missing --output-dir and HOMEBOY_OUTPUT_DIR is not set",
            None,
            Some(vec![
                "Pass --output-dir <dir>".to_string(),
                "Set HOMEBOY_OUTPUT_DIR to the structured output directory".to_string(),
            ]),
        )),
    }
}

fn normalize_reconcile_run_commands(commands: Vec<String>) -> Vec<String> {
    commands
        .into_iter()
        .flat_map(|raw| {
            raw.split(',')
                .map(str::trim)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|command| !command.is_empty())
        .collect()
}

pub(super) struct ReconcileRunSource {
    pub(super) command: String,
    pub(super) path: PathBuf,
}

pub(super) fn reconcile_run_sources(
    output_dir: &Path,
    commands: Vec<String>,
    from_output: Vec<(String, String)>,
) -> Vec<ReconcileRunSource> {
    if !from_output.is_empty() {
        let mut mapped = BTreeMap::new();
        for (raw, path) in from_output {
            if let Some(command) = quality_base_command(&raw) {
                mapped.insert(
                    command.to_string(),
                    ReconcileRunSource {
                        command: command.to_string(),
                        path: PathBuf::from(path),
                    },
                );
            }
        }
        return mapped.into_values().collect();
    }

    let mut sources = BTreeMap::new();
    for raw in normalize_reconcile_run_commands(commands) {
        if let Some(command) = quality_base_command(&raw) {
            sources.insert(
                command.to_string(),
                output_dir.join(format!("{}.json", command_output_stem(&raw))),
            );
        }
    }

    sources
        .into_iter()
        .map(|(command, path)| ReconcileRunSource { command, path })
        .collect()
}

fn quality_base_command(command: &str) -> Option<&'static str> {
    let command = command
        .trim()
        .strip_prefix("review ")
        .unwrap_or(command.trim());
    match command.split_whitespace().next()? {
        "audit" => Some("audit"),
        "lint" => Some("lint"),
        "test" => Some("test"),
        _ => None,
    }
}

pub(super) fn inspect_reconcile_run_output(path: &Path) -> OutputInspection {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => {
            return OutputInspection::Missing(format!(
                "No structured output found at {}",
                path.display()
            ))
        }
    };

    if metadata.len() == 0 {
        return OutputInspection::Missing(format!(
            "Structured output is empty at {}",
            path.display()
        ));
    }

    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) => {
            return OutputInspection::Malformed(format!(
                "Could not read structured output at {}: {}",
                path.display(),
                err
            ))
        }
    };

    match serde_json::from_str(&raw) {
        Ok(value) => OutputInspection::Valid(value),
        Err(err) => OutputInspection::Malformed(format!(
            "Structured output is malformed at {}: {}",
            path.display(),
            err
        )),
    }
}

pub(super) fn component_id_from_native_output(value: &Value) -> Option<String> {
    value
        .pointer("/data/component_id")
        .or_else(|| value.pointer("/data/component"))
        .or_else(|| value.get("component_id"))
        .or_else(|| value.get("component"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToString::to_string)
}

pub(super) fn aggregate_reconcile_output(
    output: &ReconcileOutput,
) -> (ReconcileRunIssueTotals, usize) {
    if let Some(result) = &output.result {
        let mut issue_totals = ReconcileRunIssueTotals::default();
        let mut failures = 0;
        for execution in &result.executions {
            match execution.outcome {
                homeboy::issues::apply::ExecutionOutcome::Filed { .. } => {
                    issue_totals.issues_created += 1;
                }
                homeboy::issues::apply::ExecutionOutcome::Updated { .. }
                | homeboy::issues::apply::ExecutionOutcome::UpdatedClosed { .. } => {
                    issue_totals.issues_updated += 1;
                }
                homeboy::issues::apply::ExecutionOutcome::Closed { .. }
                | homeboy::issues::apply::ExecutionOutcome::ClosedDuplicate { .. } => {
                    issue_totals.issues_closed += 1;
                }
                homeboy::issues::apply::ExecutionOutcome::Failed { .. } => {
                    failures += 1;
                }
                homeboy::issues::apply::ExecutionOutcome::Skipped => {}
            }
        }
        (issue_totals, failures)
    } else {
        (
            ReconcileRunIssueTotals {
                issues_created: output.plan_summary.file_new,
                issues_updated: output.plan_summary.update + output.plan_summary.update_closed,
                issues_closed: output.plan_summary.close + output.plan_summary.close_duplicate,
            },
            0,
        )
    }
}
