use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use clap::Args;
use serde::Serialize;
use serde_json::Value;

use homeboy::core::observation::runs_service;
use homeboy::core::observation::{ArtifactRecord, ObservationStore};
use homeboy::core::Error;

use crate::commands::escape_markdown_table_cell;

#[derive(Args, Debug, Clone)]
pub struct ReportCompareArgs {
    /// Baseline artifact input: local JSON path, run id, or run:artifact / run/artifact ref
    #[arg(long, value_name = "RUN_OR_ARTIFACT")]
    pub old: String,

    /// Candidate artifact input: local JSON path, run id, or run:artifact / run/artifact ref
    #[arg(long, value_name = "RUN_OR_ARTIFACT")]
    pub new: String,

    /// Output format
    #[arg(long, value_parser = ["markdown", "json"], default_value = "markdown")]
    pub format: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReportCompareReport {
    pub markdown: String,
    pub old: ReportArtifactSummary,
    pub new: ReportArtifactSummary,
    pub total: CountDelta,
    pub groups: Vec<NamedCountDelta>,
    pub kinds: Vec<NamedCountDelta>,
    pub fixtures: Vec<NamedCountDelta>,
    pub identities: IdentityDelta,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReportArtifactSummary {
    pub input: String,
    pub source: String,
    pub total_findings: usize,
    pub stable_identity_count: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CountDelta {
    pub old: usize,
    pub new: usize,
    pub delta: isize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct NamedCountDelta {
    pub name: String,
    pub old: usize,
    pub new: usize,
    pub delta: isize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct IdentityDelta {
    pub old: usize,
    pub new: usize,
    pub resolved: usize,
    pub introduced: usize,
    pub persistent: usize,
}

#[derive(Debug, Clone)]
struct ArtifactInput {
    source: String,
    value: Value,
}

#[derive(Debug, Clone, Default)]
struct FindingAggregate {
    total: usize,
    groups: BTreeMap<String, usize>,
    kinds: BTreeMap<String, usize>,
    fixtures: BTreeMap<String, usize>,
    identities: BTreeSet<String>,
}

pub fn render_report_compare_from_args(args: &ReportCompareArgs) -> homeboy::core::Result<String> {
    compare_report_artifacts_from_args(args).map(|report| report.markdown)
}

pub fn compare_report_artifacts_from_args(
    args: &ReportCompareArgs,
) -> homeboy::core::Result<ReportCompareReport> {
    let old = read_artifact_input(&args.old)?;
    let new = read_artifact_input(&args.new)?;
    let old_aggregate = aggregate_findings(&old.value);
    let new_aggregate = aggregate_findings(&new.value);
    let identities = identity_delta(&old_aggregate.identities, &new_aggregate.identities);
    let mut report = ReportCompareReport {
        markdown: String::new(),
        old: ReportArtifactSummary {
            input: args.old.clone(),
            source: old.source,
            total_findings: old_aggregate.total,
            stable_identity_count: old_aggregate.identities.len(),
        },
        new: ReportArtifactSummary {
            input: args.new.clone(),
            source: new.source,
            total_findings: new_aggregate.total,
            stable_identity_count: new_aggregate.identities.len(),
        },
        total: count_delta(old_aggregate.total, new_aggregate.total),
        groups: named_deltas(&old_aggregate.groups, &new_aggregate.groups),
        kinds: named_deltas(&old_aggregate.kinds, &new_aggregate.kinds),
        fixtures: named_deltas(&old_aggregate.fixtures, &new_aggregate.fixtures),
        identities,
    };
    report.markdown = render_markdown(&report);
    Ok(report)
}

fn read_artifact_input(input: &str) -> homeboy::core::Result<ArtifactInput> {
    let path = PathBuf::from(input);
    if path.is_file() {
        let value = read_json_file(&path)?;
        return Ok(ArtifactInput {
            source: path.display().to_string(),
            value,
        });
    }

    let store = ObservationStore::open_initialized()?;
    let artifact = if let Some((run_id, artifact_id)) = split_artifact_ref(input) {
        runs_service::resolve_artifact_for_run(&store, run_id, artifact_id)?
    } else {
        select_json_artifact_for_run(&store, input)?
    };
    let (source, value) = read_artifact_record(artifact)?;
    Ok(ArtifactInput { source, value })
}

fn split_artifact_ref(input: &str) -> Option<(&str, &str)> {
    input
        .split_once(':')
        .or_else(|| input.split_once('/'))
        .filter(|(run_id, artifact_id)| !run_id.is_empty() && !artifact_id.is_empty())
}

fn select_json_artifact_for_run(
    store: &ObservationStore,
    run_id: &str,
) -> homeboy::core::Result<ArtifactRecord> {
    let artifacts = runs_service::list_artifacts_for_run(store, run_id)?;
    artifacts
        .into_iter()
        .find(is_report_json_artifact)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "old/new",
                format!("run `{run_id}` has no JSON report/matrix artifact to compare"),
                Some(run_id.to_string()),
                Some(vec![
                    "Pass an explicit artifact ref such as <run-id>:<artifact-id>.".to_string(),
                    "Run `homeboy runs artifacts <run-id>` to inspect available artifacts."
                        .to_string(),
                ]),
            )
        })
}

fn is_report_json_artifact(artifact: &ArtifactRecord) -> bool {
    let lower_kind = artifact.kind.to_ascii_lowercase();
    let lower_path = artifact.path.to_ascii_lowercase();
    (lower_kind.contains("report")
        || lower_kind.contains("matrix")
        || lower_kind.contains("summary")
        || lower_path.contains("report")
        || lower_path.contains("matrix")
        || lower_path.contains("summary"))
        && (lower_path.ends_with(".json")
            || artifact
                .mime
                .as_deref()
                .is_some_and(|mime| mime.contains("json")))
}

fn read_artifact_record(artifact: ArtifactRecord) -> homeboy::core::Result<(String, Value)> {
    match runs_service::classify_artifact_storage(&artifact) {
        runs_service::ArtifactStorage::LocalFile => {
            let path = PathBuf::from(&artifact.path);
            Ok((
                format!("{}:{}", artifact.run_id, artifact.id),
                read_json_file(&path)?,
            ))
        }
        runs_service::ArtifactStorage::Remote => {
            let tempdir = tempfile::Builder::new()
                .prefix("homeboy-report-compare-")
                .tempdir()
                .map_err(|e| {
                    Error::internal_io(e.to_string(), Some("create tempdir".to_string()))
                })?;
            let output = tempdir
                .path()
                .join(format!("{}.json", safe_file_component(&artifact.id)));
            let download = runs_service::download_remote_artifact(artifact.clone(), Some(output))?;
            let value = read_json_file(&download.output_path)?;
            Ok((format!("{}:{}", artifact.run_id, artifact.id), value))
        }
        runs_service::ArtifactStorage::MetadataOnly => Err(Error::validation_invalid_argument(
            "old/new",
            format!(
                "artifact {} was imported as metadata only; artifact bytes are not available",
                artifact.id
            ),
            Some(artifact.id),
            None,
        )),
        runs_service::ArtifactStorage::Other => Err(Error::validation_invalid_argument(
            "old/new",
            format!(
                "artifact {} is {}, not a JSON file",
                artifact.id, artifact.artifact_type
            ),
            Some(artifact.id),
            None,
        )),
    }
}

fn read_json_file(path: &Path) -> homeboy::core::Result<Value> {
    let raw = fs::read_to_string(path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(format!("read {}", path.display()))))?;
    serde_json::from_str(&raw).map_err(|e| {
        Error::validation_invalid_argument(
            "old/new",
            format!("{} is not valid JSON: {e}", path.display()),
            Some(path.display().to_string()),
            None,
        )
    })
}

fn safe_file_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn aggregate_findings(value: &Value) -> FindingAggregate {
    let mut aggregate = FindingAggregate::default();
    collect_findings(value, None, &mut aggregate);
    if aggregate.total == 0 {
        if let Some(total) = numeric_field(value, &["total_findings", "finding_count", "count"]) {
            aggregate.total = total;
        }
    }
    aggregate
}

fn collect_findings(value: &Value, key: Option<&str>, aggregate: &mut FindingAggregate) {
    match value {
        Value::Array(items) if key.is_some_and(is_finding_array_key) => {
            for item in items.iter().filter(|item| item.is_object()) {
                add_finding(item, aggregate);
                collect_nested_finding_arrays(item, aggregate);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_findings(item, None, aggregate);
            }
        }
        Value::Object(map) => {
            for (child_key, child) in map {
                collect_findings(child, Some(child_key), aggregate);
            }
        }
        _ => {}
    }
}

fn collect_nested_finding_arrays(value: &Value, aggregate: &mut FindingAggregate) {
    if let Value::Object(map) = value {
        for (key, child) in map {
            if child.is_array() && is_finding_array_key(key) {
                collect_findings(child, Some(key), aggregate);
            }
        }
    }
}

fn is_finding_array_key(key: &str) -> bool {
    matches!(
        key,
        "findings"
            | "diagnostics"
            | "budget_findings"
            | "matrix_findings"
            | "issues"
            | "violations"
    )
}

fn add_finding(value: &Value, aggregate: &mut FindingAggregate) {
    aggregate.total += 1;
    increment_if_present(
        &mut aggregate.groups,
        string_field(
            value,
            &["group", "diagnostic_group", "category", "suite", "source"],
        ),
    );
    increment_if_present(
        &mut aggregate.kinds,
        string_field(
            value,
            &["diagnostic_kind", "kind", "rule", "code", "type", "tool"],
        ),
    );
    increment_if_present(
        &mut aggregate.fixtures,
        string_field(
            value,
            &[
                "fixture",
                "fixture_id",
                "scenario_id",
                "test_name",
                "path",
                "file",
            ],
        ),
    );
    if let Some(identity) = string_field(
        value,
        &[
            "stable_id",
            "stable_identity",
            "fingerprint",
            "identity",
            "finding_id",
            "id",
        ],
    ) {
        aggregate.identities.insert(identity);
    }
}

fn string_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn numeric_field(value: &Value, keys: &[&str]) -> Option<usize> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_u64))
        .map(|value| value as usize)
}

fn increment_if_present(counts: &mut BTreeMap<String, usize>, value: Option<String>) {
    if let Some(value) = value {
        *counts.entry(value).or_default() += 1;
    }
}

fn count_delta(old: usize, new: usize) -> CountDelta {
    CountDelta {
        old,
        new,
        delta: new as isize - old as isize,
    }
}

fn named_deltas(
    old: &BTreeMap<String, usize>,
    new: &BTreeMap<String, usize>,
) -> Vec<NamedCountDelta> {
    old.keys()
        .chain(new.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|name| {
            let old_count = old.get(name).copied().unwrap_or_default();
            let new_count = new.get(name).copied().unwrap_or_default();
            NamedCountDelta {
                name: name.clone(),
                old: old_count,
                new: new_count,
                delta: new_count as isize - old_count as isize,
            }
        })
        .collect()
}

fn identity_delta(old: &BTreeSet<String>, new: &BTreeSet<String>) -> IdentityDelta {
    IdentityDelta {
        old: old.len(),
        new: new.len(),
        resolved: old.difference(new).count(),
        introduced: new.difference(old).count(),
        persistent: old.intersection(new).count(),
    }
}

fn render_markdown(report: &ReportCompareReport) -> String {
    let mut out = String::new();
    out.push_str("# Report Compare\n\n");
    out.push_str(&format!("- **Old:** `{}`\n", report.old.source));
    out.push_str(&format!("- **New:** `{}`\n", report.new.source));
    out.push_str(&format!(
        "- **Total findings:** {} -> {} ({})\n",
        report.total.old,
        report.total.new,
        format_delta(report.total.delta)
    ));
    out.push_str(&format!(
        "- **Stable identities:** resolved {}, new {}, persistent {}\n",
        report.identities.resolved, report.identities.introduced, report.identities.persistent
    ));
    render_delta_table(&mut out, "Groups", &report.groups);
    render_delta_table(&mut out, "Kinds", &report.kinds);
    render_delta_table(&mut out, "Fixtures", &report.fixtures);
    out
}

fn render_delta_table(out: &mut String, title: &str, rows: &[NamedCountDelta]) {
    if rows.is_empty() {
        return;
    }
    out.push_str(&format!("\n## {title}\n\n"));
    out.push_str("| Name | Old | New | Delta |\n");
    out.push_str("|---|---:|---:|---:|\n");
    for row in rows {
        out.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            escape_markdown_table_cell(&row.name),
            row.old,
            row.new,
            format_delta(row.delta)
        ));
    }
}

fn format_delta(delta: isize) -> String {
    if delta > 0 {
        format!("+{delta}")
    } else {
        delta.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_simplified_matrix_artifacts() {
        let old = serde_json::json!({
            "matrix": {
                "findings": [
                    {"stable_id":"a", "group":"generated", "diagnostic_kind":"generated_document_contains_core_html", "fixture":"one"},
                    {"stable_id":"b", "group":"generated", "diagnostic_kind":"generated_document_contains_core_html", "fixture":"two"},
                    {"stable_id":"c", "group":"runtime", "diagnostic_kind":"runtime_dependency_target_missing", "fixture":"two"}
                ]
            }
        });
        let new = serde_json::json!({
            "matrix": {
                "findings": [
                    {"stable_id":"b", "group":"generated", "diagnostic_kind":"generated_document_contains_core_html", "fixture":"two"},
                    {"stable_id":"d", "group":"runtime", "diagnostic_kind":"runtime_dependency_target_missing", "fixture":"three"}
                ]
            }
        });

        let old_aggregate = aggregate_findings(&old);
        let new_aggregate = aggregate_findings(&new);

        assert_eq!(old_aggregate.total, 3);
        assert_eq!(new_aggregate.total, 2);
        assert_eq!(old_aggregate.groups["generated"], 2);
        assert_eq!(new_aggregate.fixtures["three"], 1);
        assert_eq!(
            identity_delta(&old_aggregate.identities, &new_aggregate.identities).resolved,
            2
        );
        assert_eq!(
            identity_delta(&old_aggregate.identities, &new_aggregate.identities).introduced,
            1
        );
        assert_eq!(
            identity_delta(&old_aggregate.identities, &new_aggregate.identities).persistent,
            1
        );
    }
}
