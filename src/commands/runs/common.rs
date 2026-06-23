//! Shared helpers for the `runs` subcommand family.
//!
//! Hosts duration parsing (used by `--since` flags), JSONPath compilation
//! (used by `query`/`drift`), and the artifact-row loader (used by
//! `query`/`drift` to project JSON over imported run artifacts).

use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;
use std::time::Duration;

use homeboy::core::observation::{ArtifactRecord, ObservationStore, RunListFilter, RunRecord};
use homeboy::core::rig::RigRunArtifactIndex;
use homeboy::core::runners::{
    is_reportable_artifact_evidence_path, is_retrievable_runner_artifact,
};
use homeboy::core::Error;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize, Deserialize, Clone)]
pub struct RunSummary {
    pub id: String,
    pub kind: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub component_id: Option<String>,
    pub rig_id: Option<String>,
    pub git_sha: Option<String>,
    pub command: Option<String>,
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_index: Option<RigRunArtifactIndex>,
}

pub fn run_summaries_with_artifact_indexes(
    store: &ObservationStore,
    run_records: Vec<RunRecord>,
) -> homeboy::core::Result<Vec<RunSummary>> {
    let rig_run_ids = run_records
        .iter()
        .filter(|run| run.kind == "rig" && run.rig_id.is_some())
        .map(|run| run.id.clone())
        .collect::<Vec<_>>();
    let mut artifacts_by_run = store.list_artifacts_for_runs(&rig_run_ids)?;
    Ok(run_records
        .into_iter()
        .map(|run| {
            let artifacts = artifacts_by_run.remove(&run.id).unwrap_or_default();
            run_summary_with_artifact_index(run, &artifacts)
        })
        .collect())
}

fn run_summary_with_artifact_index(run: RunRecord, artifacts: &[ArtifactRecord]) -> RunSummary {
    let artifact_index = homeboy::core::rig::artifact_index_for_run_with_artifacts(&run, artifacts);
    RunSummary {
        artifact_index,
        ..super::run_summary(run)
    }
}

/// Convert a `--since <duration>` flag value into an RFC-3339 timestamp.
///
/// Accepts the same forms as elsewhere in the runs surface (s, m, h, d).
/// Returned timestamp is `now - duration` so the caller can compare with
/// `started_at >= threshold` semantics.
pub fn since_threshold(raw: &str) -> homeboy::core::Result<String> {
    let duration = parse_duration(raw)?;
    let chrono_duration = chrono::Duration::from_std(duration).map_err(|e| {
        Error::validation_invalid_argument("since", e.to_string(), Some(raw.to_string()), None)
    })?;
    Ok((chrono::Utc::now() - chrono_duration).to_rfc3339())
}

/// Parse a duration string with units (s, m, h, d).
///
/// Mirrors the parser in `bundle.rs`. Kept here so the gh-actions / query /
/// drift commands can share the same surface without re-implementing it.
pub fn parse_duration(raw: &str) -> homeboy::core::Result<Duration> {
    let trimmed = raw.trim();
    let split = trimmed
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(trimmed.len());
    let (amount, unit) = trimmed.split_at(split);
    if amount.is_empty() || unit.is_empty() || !unit.chars().all(|ch| ch.is_ascii_alphabetic()) {
        return Err(Error::validation_invalid_argument(
            "since",
            "expected duration like 30m, 24h, or 7d",
            Some(raw.to_string()),
            None,
        ));
    }
    let amount = amount.parse::<u64>().map_err(|_| {
        Error::validation_invalid_argument(
            "since",
            "duration amount must be a positive integer",
            Some(raw.to_string()),
            None,
        )
    })?;
    if amount == 0 {
        return Err(Error::validation_invalid_argument(
            "since",
            "duration amount must be greater than zero",
            Some(raw.to_string()),
            None,
        ));
    }
    let seconds = match unit {
        "s" | "sec" | "secs" | "second" | "seconds" => amount,
        "m" | "min" | "mins" | "minute" | "minutes" => amount * 60,
        "h" | "hr" | "hrs" | "hour" | "hours" => amount * 60 * 60,
        "d" | "day" | "days" => amount * 60 * 60 * 24,
        _ => {
            return Err(Error::validation_invalid_argument(
                "since",
                "duration unit must be one of s, m, h, or d",
                Some(raw.to_string()),
                None,
            ))
        }
    };
    Ok(Duration::from_secs(seconds))
}

/// Compile a JSONPath expression. Returns a structured validation error on
/// invalid syntax instead of panicking. Schema-blind: the engine doesn't know
/// what the JSON looks like, only how to walk it.
pub fn compile_jsonpath(expr: &str) -> homeboy::core::Result<serde_json_path::JsonPath> {
    serde_json_path::JsonPath::parse(expr).map_err(|e| {
        Error::validation_invalid_argument(
            "jsonpath",
            format!("invalid JSONPath expression: {e}"),
            Some(expr.to_string()),
            None,
        )
    })
}

/// Apply a compiled JSONPath to a JSON value and return all matched nodes,
/// each as an owned `Value` clone.
pub fn eval_jsonpath(path: &serde_json_path::JsonPath, value: &Value) -> Vec<Value> {
    path.query(value).all().into_iter().cloned().collect()
}

/// Render a JSON value as a flat scalar string suitable for grouping or
/// table display. Returns `None` for objects, arrays, and `null`.
///
/// Inlined into `distribution_share` callers as a closure so the helper
/// stays private to its only caller; cross-file callers should pass a
/// custom projection if they need different scalar semantics.
fn scalar_label(value: &Value) -> Option<String> {
    if let Some(s) = value.as_str() {
        return Some(s.to_string());
    }
    if let Some(b) = value.as_bool() {
        return Some(b.to_string());
    }
    if let Some(n) = value.as_number() {
        return Some(n.to_string());
    }
    None
}

/// Loaded artifact row: the raw JSON parsed from the artifact's stored file
/// plus the run that owns it. Used by `query` and `drift` for
/// schema-blind projections.
pub struct ArtifactJsonRow {
    pub run: RunRecord,
    pub artifact_kind: String,
    /// Local path on disk. Held for diagnostics so callers can mention which
    /// file produced a finding; not currently consumed by core primitives.
    #[allow(dead_code)]
    pub artifact_path: String,
    pub json: Value,
}

pub struct ArtifactJsonLoadResult {
    pub rows: Vec<ArtifactJsonRow>,
    pub skipped: Vec<SkippedArtifactRow>,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct SkippedArtifactRow {
    pub run_id: String,
    pub artifact_id: String,
    pub artifact_kind: String,
    pub artifact_type: String,
    pub path: String,
    pub reason: String,
}

/// Load every JSON artifact attached to runs matching the filter.
///
/// Schema-blind: artifacts whose stored file is missing, unreadable, not valid
/// JSON, or metadata-only are skipped with diagnostics. Non-file artifact types
/// like images, zips, and URLs are ignored because callers project JSON rows.
pub fn load_artifact_rows(
    store: &ObservationStore,
    filter: RunListFilter,
    since: Option<&str>,
) -> homeboy::core::Result<ArtifactJsonLoadResult> {
    let threshold = since.map(since_threshold).transpose()?;
    let run_artifacts = store.list_run_artifacts(filter, threshold.as_deref())?;

    let mut rows = Vec::new();
    let mut skipped = Vec::new();
    for run_artifact in run_artifacts {
        let run = run_artifact.run;
        let artifact = run_artifact.artifact;
        if artifact.artifact_type != "file" {
            if artifact.artifact_type == "metadata-only" {
                skipped.push(skipped_artifact(
                    &run,
                    &artifact,
                    "artifact bytes are not available in this imported metadata-only bundle",
                ));
            }
            continue;
        }
        if !is_reportable_artifact_evidence_path(&artifact.path) {
            skipped.push(skipped_artifact(
                &run,
                &artifact,
                "artifact path is not locally accessible or retrievable",
            ));
            continue;
        }
        if is_retrievable_runner_artifact(&artifact.path) {
            skipped.push(skipped_artifact(
                &run,
                &artifact,
                "artifact is a retrievable runner artifact ref; download it before JSON projection",
            ));
            continue;
        }
        let path = Path::new(&artifact.path);
        let file = match File::open(path) {
            Ok(file) => file,
            Err(_) => {
                skipped.push(skipped_artifact(
                    &run,
                    &artifact,
                    "artifact file is missing or unreadable",
                ));
                continue;
            }
        };
        let json = match serde_json::from_reader::<_, Value>(file) {
            Ok(json) => json,
            Err(err) if err.is_io() => {
                skipped.push(skipped_artifact(
                    &run,
                    &artifact,
                    "artifact file is missing or unreadable",
                ));
                continue;
            }
            Err(_) => {
                skipped.push(skipped_artifact(
                    &run,
                    &artifact,
                    "artifact file is not valid JSON",
                ));
                continue;
            }
        };
        rows.push(ArtifactJsonRow {
            run,
            artifact_kind: artifact.kind,
            artifact_path: artifact.path,
            json,
        });
    }
    Ok(ArtifactJsonLoadResult { rows, skipped })
}

fn skipped_artifact(
    run: &RunRecord,
    artifact: &ArtifactRecord,
    reason: impl Into<String>,
) -> SkippedArtifactRow {
    SkippedArtifactRow {
        run_id: run.id.clone(),
        artifact_id: artifact.id.clone(),
        artifact_kind: artifact.kind.clone(),
        artifact_type: artifact.artifact_type.clone(),
        path: artifact.path.clone(),
        reason: reason.into(),
    }
}

/// Tally the share of each scalar value of `metric_path` across `rows`.
///
/// Schema-blind: returns `(value, count, share)` tuples sorted by descending
/// count. Non-scalar matches and missing matches are tallied under
/// `missing_count` so callers can report coverage.
pub fn distribution_share(
    rows: &[ArtifactJsonRow],
    metric_path: &serde_json_path::JsonPath,
) -> DistributionSnapshot {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut total = 0usize;
    let mut missing = 0usize;
    for row in rows {
        let matches = eval_jsonpath(metric_path, &row.json);
        let mut row_had_scalar = false;
        for matched in matches {
            if let Some(scalar) = scalar_label(&matched) {
                *counts.entry(scalar).or_insert(0) += 1;
                total += 1;
                row_had_scalar = true;
            }
        }
        if !row_had_scalar {
            missing += 1;
        }
    }
    let mut values: Vec<(String, usize, f64)> = counts
        .into_iter()
        .map(|(value, count)| {
            let share = if total == 0 {
                0.0
            } else {
                count as f64 / total as f64
            };
            (value, count, share)
        })
        .collect();
    values.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    DistributionSnapshot {
        total_observations: total,
        missing_row_count: missing,
        values,
    }
}

/// Snapshot of a categorical distribution over a metric.
#[derive(Debug, Clone, PartialEq)]
pub struct DistributionSnapshot {
    pub total_observations: usize,
    pub missing_row_count: usize,
    /// `(value, count, share)` triples sorted by descending count. `share`
    /// is in `0.0..=1.0`.
    pub values: Vec<(String, usize, f64)>,
}

/// Shared test fixture: a run whose owner PID is unreachable, so it reads as
/// dead/abandoned. Used by both `tests` and `bundle_import_tests`.
#[cfg(test)]
pub(super) fn dead_owned_run(id: &str) -> RunRecord {
    RunRecord {
        id: id.to_string(),
        kind: "bench".to_string(),
        component_id: Some("homeboy".to_string()),
        started_at: "2026-05-02T16:46:46Z".to_string(),
        finished_at: None,
        status: "running".to_string(),
        command: Some("homeboy bench".to_string()),
        cwd: Some("/tmp/homeboy-fixture".to_string()),
        homeboy_version: Some("test-version".to_string()),
        git_sha: Some("abc123".to_string()),
        rig_id: Some("studio".to_string()),
        metadata_json: serde_json::json!({ "homeboy_run_owner": { "pid": u32::MAX } }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_duration_accepts_supported_units() {
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("2h").unwrap(), Duration::from_secs(7200));
        assert_eq!(parse_duration("3d").unwrap(), Duration::from_secs(259_200));
    }

    #[test]
    fn parse_duration_rejects_zero_and_unknown_units() {
        assert!(parse_duration("0s").is_err());
        assert!(parse_duration("10x").is_err());
        assert!(parse_duration("h").is_err());
    }

    #[test]
    fn eval_jsonpath_matches_root_property() {
        let path = compile_jsonpath("$.color").expect("compile");
        let value = serde_json::json!({ "color": "red" });
        let matches = eval_jsonpath(&path, &value);
        assert_eq!(matches, vec![Value::String("red".into())]);
    }

    #[test]
    fn eval_jsonpath_matches_array_wildcard() {
        let path = compile_jsonpath("$.runs[*].status").expect("compile");
        let value = serde_json::json!({
            "runs": [
                { "status": "pass" },
                { "status": "fail" },
                { "status": "pass" }
            ]
        });
        let matches = eval_jsonpath(&path, &value);
        assert_eq!(matches.len(), 3);
    }

    #[test]
    fn distribution_share_tallies_scalars_and_misses() {
        let path = compile_jsonpath("$.kind").expect("compile");
        let rows = vec![
            ArtifactJsonRow {
                run: sample_run("a"),
                artifact_kind: "k".into(),
                artifact_path: "/dev/null".into(),
                json: serde_json::json!({ "kind": "alpha" }),
            },
            ArtifactJsonRow {
                run: sample_run("b"),
                artifact_kind: "k".into(),
                artifact_path: "/dev/null".into(),
                json: serde_json::json!({ "kind": "alpha" }),
            },
            ArtifactJsonRow {
                run: sample_run("c"),
                artifact_kind: "k".into(),
                artifact_path: "/dev/null".into(),
                json: serde_json::json!({ "kind": "beta" }),
            },
            ArtifactJsonRow {
                run: sample_run("d"),
                artifact_kind: "k".into(),
                artifact_path: "/dev/null".into(),
                json: serde_json::json!({ "other": true }),
            },
        ];
        let snap = distribution_share(&rows, &path);
        assert_eq!(snap.total_observations, 3);
        assert_eq!(snap.missing_row_count, 1);
        assert_eq!(snap.values[0].0, "alpha");
        assert_eq!(snap.values[0].1, 2);
        assert!((snap.values[0].2 - 2.0 / 3.0).abs() < 1e-9);
    }

    fn sample_run(id: &str) -> RunRecord {
        RunRecord {
            id: id.to_string(),
            kind: "gh-actions".into(),
            component_id: Some("homeboy".into()),
            started_at: "2026-05-04T00:00:00Z".into(),
            finished_at: None,
            status: "pass".into(),
            command: None,
            cwd: None,
            homeboy_version: None,
            git_sha: None,
            rig_id: None,
            metadata_json: Value::Null,
        }
    }
}
