//! Cross-run hotspot aggregation over persisted fuzz artifacts.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::Path;

use clap::Args;
use serde::Serialize;
use serde_json::Value;

use homeboy::core::observation::runs_service;
use homeboy::core::observation::{ArtifactRecord, ObservationStore, RunRecord};
use homeboy::core::Error;

use super::common::SkippedArtifactRow;
use super::{CmdResult, RunsOutput};

#[derive(Args, Clone, Debug)]
pub struct RunsHotspotsArgs {
    /// One or more persisted Homeboy run ids to inspect.
    #[arg(value_name = "RUN_ID", required = true)]
    pub run_ids: Vec<String>,

    /// Maximum ranked hotspots to return.
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct RunsHotspotsOutput {
    pub command: &'static str,
    pub run_ids: Vec<String>,
    pub inspected_artifact_count: usize,
    pub matched_artifact_count: usize,
    pub skipped_artifact_count: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped_artifacts: Vec<SkippedArtifactRow>,
    pub hotspots: Vec<HotspotRanking>,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct HotspotRanking {
    pub rank: usize,
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub score: f64,
    pub occurrences: u64,
    pub run_count: usize,
    pub run_ids: Vec<String>,
    pub sources: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
struct HotspotPoint {
    key: String,
    label: Option<String>,
    score: f64,
    source: String,
}

#[derive(Debug, Clone)]
struct LoadedFuzzArtifact {
    run_id: String,
    artifact_kind: String,
    json: Value,
}

#[derive(Default)]
struct HotspotAccumulator {
    label: Option<String>,
    score: f64,
    occurrences: u64,
    run_ids: BTreeSet<String>,
    sources: BTreeSet<String>,
}

pub fn runs_hotspots(args: RunsHotspotsArgs) -> CmdResult<RunsOutput> {
    let limit = args.limit.clamp(1, 500);
    let store = ObservationStore::open_initialized()?;
    let loaded = load_fuzz_artifacts_for_runs(&store, &args.run_ids)?;
    let hotspots = rank_hotspots(&loaded.artifacts, limit);

    Ok((
        RunsOutput::Hotspots(RunsHotspotsOutput {
            command: "runs.hotspots",
            run_ids: args.run_ids,
            inspected_artifact_count: loaded.inspected_artifact_count,
            matched_artifact_count: loaded.artifacts.len(),
            skipped_artifact_count: loaded.skipped.len(),
            skipped_artifacts: loaded.skipped,
            hotspots,
        }),
        0,
    ))
}

pub(crate) fn fuzz_hotspot_lines(run: &Value) -> Vec<String> {
    let Some(run_id) = string_field(run, &["id"]) else {
        return Vec::new();
    };
    let Some(artifacts) = value_at(run, &["artifacts"]).and_then(Value::as_array) else {
        return Vec::new();
    };
    let loaded = artifacts
        .iter()
        .filter(|artifact| is_fuzz_artifact_value(artifact))
        .filter_map(|artifact| {
            if string_field(artifact, &["type", "artifact_type"]).as_deref() != Some("file") {
                return None;
            }
            let path = string_field(artifact, &["path"])?;
            let file = File::open(path).ok()?;
            let json = serde_json::from_reader::<_, Value>(file).ok()?;
            if !is_fuzz_json(&json) {
                return None;
            }
            Some(LoadedFuzzArtifact {
                run_id: run_id.clone(),
                artifact_kind: string_field(artifact, &["kind"])
                    .unwrap_or_else(|| "fuzz".to_string()),
                json,
            })
        })
        .collect::<Vec<_>>();
    let hotspots = rank_hotspots(&loaded, 5);
    if hotspots.is_empty() {
        return Vec::new();
    }

    let mut lines = vec!["Hotspots:".to_string(), "  Fuzz hotspots:".to_string()];
    for hotspot in hotspots {
        let label = hotspot
            .label
            .as_deref()
            .filter(|label| *label != hotspot.key)
            .map(|label| format!(" ({label})"))
            .unwrap_or_default();
        lines.push(format!(
            "    #{} {}{} score={} occurrences={} runs={}",
            hotspot.rank,
            hotspot.key,
            label,
            format_score(hotspot.score),
            hotspot.occurrences,
            hotspot.run_count
        ));
    }
    lines
}

struct LoadedFuzzArtifacts {
    artifacts: Vec<LoadedFuzzArtifact>,
    skipped: Vec<SkippedArtifactRow>,
    inspected_artifact_count: usize,
}

fn load_fuzz_artifacts_for_runs(
    store: &ObservationStore,
    run_ids: &[String],
) -> homeboy::core::Result<LoadedFuzzArtifacts> {
    let runs = run_ids
        .iter()
        .map(|run_id| require_run(store, run_id))
        .collect::<homeboy::core::Result<Vec<_>>>()?;
    let mut artifacts_by_run = store.list_artifacts_for_runs(run_ids)?;
    let mut artifacts = Vec::new();
    let mut skipped = Vec::new();
    let mut inspected_artifact_count = 0;

    for run in runs {
        for artifact in artifacts_by_run.remove(&run.id).unwrap_or_default() {
            if !is_fuzz_artifact(&artifact) {
                continue;
            }
            inspected_artifact_count += 1;
            match read_artifact_json(&run, &artifact) {
                Ok(json) if is_fuzz_json(&json) => artifacts.push(LoadedFuzzArtifact {
                    run_id: run.id.clone(),
                    artifact_kind: artifact.kind.clone(),
                    json,
                }),
                Ok(_) => skipped.push(skipped_artifact(
                    &run,
                    &artifact,
                    "artifact JSON is not a recognized fuzz payload",
                )),
                Err(reason) => skipped.push(skipped_artifact(&run, &artifact, reason)),
            }
        }
    }

    Ok(LoadedFuzzArtifacts {
        artifacts,
        skipped,
        inspected_artifact_count,
    })
}

fn require_run(store: &ObservationStore, run_id: &str) -> homeboy::core::Result<RunRecord> {
    store.get_run(run_id)?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "run_id",
            format!("run record not found: {run_id}"),
            Some(run_id.to_string()),
            None,
        )
    })
}

fn is_fuzz_artifact(artifact: &ArtifactRecord) -> bool {
    artifact.kind.contains("fuzz")
        || artifact
            .metadata_json
            .get("schema")
            .and_then(Value::as_str)
            .is_some_and(|schema| schema.contains("fuzz"))
}

fn is_fuzz_artifact_value(artifact: &Value) -> bool {
    string_field(artifact, &["kind"]).is_some_and(|kind| kind.contains("fuzz"))
        || value_at(artifact, &["metadata", "schema"])
            .and_then(Value::as_str)
            .is_some_and(|schema| schema.contains("fuzz"))
}

fn read_artifact_json(run: &RunRecord, artifact: &ArtifactRecord) -> Result<Value, String> {
    let path = if artifact.artifact_type == "file" {
        Path::new(&artifact.path).to_path_buf()
    } else if artifact.artifact_type == "remote_file" {
        runs_service::download_remote_artifact(artifact.clone(), None)
            .map_err(|err| format!("remote artifact could not be downloaded: {err}"))?
            .output_path
    } else {
        return Err(if artifact.artifact_type == "metadata-only" {
            "artifact bytes are not available in this imported metadata-only bundle".to_string()
        } else {
            format!(
                "artifact type `{}` is not a JSON file",
                artifact.artifact_type
            )
        });
    };

    let file =
        File::open(&path).map_err(|_| "artifact file is missing or unreadable".to_string())?;
    serde_json::from_reader::<_, Value>(file)
        .map_err(|_| format!("artifact file is not valid JSON for run `{}`", run.id))
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

fn rank_hotspots(artifacts: &[LoadedFuzzArtifact], limit: usize) -> Vec<HotspotRanking> {
    let mut by_key: BTreeMap<String, HotspotAccumulator> = BTreeMap::new();
    for artifact in artifacts {
        for point in extract_hotspot_points(&artifact.json) {
            let entry = by_key.entry(point.key.clone()).or_default();
            if entry.label.is_none() {
                entry.label = point.label;
            }
            entry.score += point.score;
            entry.occurrences += 1;
            entry.run_ids.insert(artifact.run_id.clone());
            entry.sources.insert(format!(
                "{}:{}:{}",
                artifact.run_id, artifact.artifact_kind, point.source
            ));
        }
    }

    let mut ranked = by_key
        .into_iter()
        .map(|(key, entry)| HotspotRanking {
            rank: 0,
            key,
            label: entry.label,
            score: entry.score,
            occurrences: entry.occurrences,
            run_count: entry.run_ids.len(),
            run_ids: entry.run_ids.into_iter().collect(),
            sources: entry.sources.into_iter().collect(),
        })
        .collect::<Vec<_>>();

    ranked.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| b.run_count.cmp(&a.run_count))
            .then_with(|| b.occurrences.cmp(&a.occurrences))
            .then_with(|| a.key.cmp(&b.key))
    });
    ranked.truncate(limit);
    for (idx, hotspot) in ranked.iter_mut().enumerate() {
        hotspot.rank = idx + 1;
    }
    ranked
}

fn extract_hotspot_points(json: &Value) -> Vec<HotspotPoint> {
    let mut points = standardized_hotspots(json);
    if !points.is_empty() {
        return points;
    }

    points.extend(finding_hotspots(json));
    points.extend(coverage_gap_hotspots(json));
    points
}

fn standardized_hotspots(json: &Value) -> Vec<HotspotPoint> {
    let mut points = Vec::new();
    collect_standardized_hotspots(json, "$", &mut points);
    points
}

fn collect_standardized_hotspots(value: &Value, source: &str, points: &mut Vec<HotspotPoint>) {
    if let Some(hotspots) = value.get("hotspots") {
        collect_hotspot_items(hotspots, &format!("{source}.hotspots"), points);
    }

    match value {
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                collect_standardized_hotspots(item, &format!("{source}[{index}]"), points);
            }
        }
        Value::Object(object) => {
            for (key, item) in object {
                if key == "hotspots" || key == "prior_observations" {
                    continue;
                }
                collect_standardized_hotspots(item, &format!("{source}.{key}"), points);
            }
        }
        _ => {}
    }
}

fn collect_hotspot_items(value: &Value, source: &str, points: &mut Vec<HotspotPoint>) {
    let items = value
        .as_array()
        .or_else(|| value.get("items").and_then(Value::as_array));
    let Some(items) = items else {
        return;
    };

    points.extend(items.iter().filter_map(|item| {
        let key = string_field(item, &["key", "id", "fingerprint", "name"]).or_else(|| {
            composite_key(
                item,
                &[
                    "category",
                    "phase",
                    "operation",
                    "hook",
                    "surface_id",
                    "target_id",
                    "operation_id",
                    "case_id",
                ],
            )
        });
        key.map(|key| HotspotPoint {
            key,
            label: string_field(item, &["label", "title", "name"]),
            score: numeric_field(item, &["score", "weight", "count", "value"]).unwrap_or(1.0),
            source: source.to_string(),
        })
    }));
}

fn finding_hotspots(json: &Value) -> Vec<HotspotPoint> {
    [
        &["findings"][..],
        &["campaign", "findings"][..],
        &["metadata", "findings"][..],
    ]
    .into_iter()
    .filter_map(|path| {
        value_at(json, path)
            .and_then(Value::as_array)
            .map(|items| (path, items))
    })
    .flat_map(|(path, items)| {
        items.iter().filter_map(move |item| {
            let key = string_field(item, &["fingerprint", "id"]).or_else(|| {
                composite_key(
                    item,
                    &["surface_id", "target_id", "operation_id", "case_id"],
                )
            });
            key.map(|key| HotspotPoint {
                key,
                label: string_field(item, &["title", "label"]),
                score: severity_score(item).unwrap_or(1.0),
                source: path.join("."),
            })
        })
    })
    .collect()
}

fn coverage_gap_hotspots(json: &Value) -> Vec<HotspotPoint> {
    [&["coverage"][..], &["campaign", "coverage"][..]]
        .into_iter()
        .filter_map(|path| {
            value_at(json, path)
                .and_then(Value::as_array)
                .map(|items| (path, items))
        })
        .flat_map(|(path, coverage_items)| {
            coverage_items.iter().flat_map(move |coverage| {
                coverage
                    .get("gaps")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(move |gap| {
                        let key = string_field(gap, &["id"]).or_else(|| {
                            composite_key(
                                gap,
                                &["surface_id", "target_id", "operation_id", "operation"],
                            )
                        });
                        key.map(|key| HotspotPoint {
                            key,
                            label: string_field(gap, &["label"]),
                            score: 1.0,
                            source: format!("{}.gaps", path.join(".")),
                        })
                    })
            })
        })
        .collect()
}

fn is_fuzz_json(json: &Value) -> bool {
    string_field(json, &["schema"]).is_some_and(|schema| schema.contains("fuzz"))
        || value_at(json, &["campaign"])
            .and_then(|campaign| string_field(campaign, &["schema"]))
            .is_some_and(|schema| schema.contains("fuzz"))
        || value_at(json, &["hotspots"]).is_some()
}

fn value_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(value, |current, key| current.get(key))
}

fn string_field(value: &Value, fields: &[&str]) -> Option<String> {
    fields
        .iter()
        .find_map(|field| value.get(field).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn numeric_field(value: &Value, fields: &[&str]) -> Option<f64> {
    fields
        .iter()
        .find_map(|field| value.get(field).and_then(Value::as_f64))
        .filter(|value| value.is_finite())
}

fn format_score(value: f64) -> String {
    let mut formatted = format!("{value:.3}");
    while formatted.contains('.') && formatted.ends_with('0') {
        formatted.pop();
    }
    if formatted.ends_with('.') {
        formatted.pop();
    }
    formatted
}

fn composite_key(value: &Value, fields: &[&str]) -> Option<String> {
    let parts = fields
        .iter()
        .filter_map(|field| {
            value
                .get(field)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(|part| format!("{field}={part}"))
        })
        .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| parts.join("|"))
}

fn severity_score(value: &Value) -> Option<f64> {
    numeric_field(value, &["score", "weight"]).or_else(|| {
        string_field(value, &["severity"]).map(|severity| match severity.as_str() {
            "critical" => 5.0,
            "high" => 4.0,
            "medium" => 3.0,
            "low" => 2.0,
            _ => 1.0,
        })
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn artifact(run_id: &str, json: Value) -> LoadedFuzzArtifact {
        LoadedFuzzArtifact {
            run_id: run_id.to_string(),
            artifact_kind: "fuzz_result_envelope".to_string(),
            json,
        }
    }

    #[test]
    fn standardized_hotspots_rank_by_total_score_across_runs() {
        let artifacts = vec![
            artifact(
                "run-a",
                json!({
                    "schema": "https://schemas.homeboy.dev/fuzz/result-envelope.json",
                    "hotspots": {
                        "schema": "homeboy/fuzz-hotspots/v1",
                        "items": [
                            { "id": "alpha", "label": "Alpha", "score": 2.5 },
                            { "id": "beta", "score": 4.0 }
                        ]
                    }
                }),
            ),
            artifact(
                "run-b",
                json!({
                    "schema": "https://schemas.homeboy.dev/fuzz/result-envelope.json",
                    "metadata": {
                        "hotspots": [
                            { "id": "alpha", "score": 3.0 },
                            { "id": "gamma", "score": 1.0 }
                        ]
                    }
                }),
            ),
        ];

        let ranked = rank_hotspots(&artifacts, 10);

        assert_eq!(ranked[0].key, "alpha");
        assert_eq!(ranked[0].label.as_deref(), Some("Alpha"));
        assert_eq!(ranked[0].score, 5.5);
        assert_eq!(ranked[0].occurrences, 2);
        assert_eq!(ranked[0].run_ids, vec!["run-a", "run-b"]);
        assert_eq!(ranked[1].key, "beta");
        assert_eq!(ranked[1].score, 4.0);
    }

    #[test]
    fn standardized_hotspots_extract_nested_observation_payloads() {
        let points = extract_hotspot_points(&json!({
            "schema": "homeboy/fuzz-campaign/v1",
            "metadata": {
                "wordpress_fuzz_result": {
                    "cases": [
                        {
                            "metadata": {
                                "observations": [
                                    {
                                        "prior_observations": [
                                            {
                                                "payload": {
                                                    "hotspots": {
                                                        "schema": "homeboy/fuzz-hotspots/v1",
                                                        "items": [
                                                            { "name": "duplicate-prior", "count": 999 }
                                                        ]
                                                    }
                                                }
                                            }
                                        ],
                                        "payload": {
                                            "hotspots": {
                                                "schema": "homeboy/fuzz-hotspots/v1",
                                                "items": [
                                                    {
                                                        "name": "variation_create:added_post_meta",
                                                        "category": "hook",
                                                        "phase": "variation_create",
                                                        "operation": "variation-batch-create",
                                                        "hook": "added_post_meta",
                                                        "count": 750
                                                    }
                                                ]
                                            }
                                        }
                                    }
                                ]
                            }
                        }
                    ]
                }
            }
        }));

        assert_eq!(points.len(), 1);
        assert_eq!(points[0].key, "variation_create:added_post_meta");
        assert_eq!(
            points[0].label.as_deref(),
            Some("variation_create:added_post_meta")
        );
        assert_eq!(points[0].score, 750.0);
        assert!(!points.iter().any(|point| point.key == "duplicate-prior"));
    }

    #[test]
    fn fallback_uses_findings_when_standard_hotspots_are_absent() {
        let artifacts = vec![artifact(
            "run-a",
            json!({
                "schema": "https://schemas.homeboy.dev/fuzz/result-envelope.json",
                "campaign": {
                    "findings": [
                        {
                            "id": "finding-a",
                            "title": "Explodes on update",
                            "severity": "high",
                            "target_id": "target-a",
                            "operation_id": "update"
                        },
                        {
                            "title": "No stable id",
                            "severity": "low",
                            "target_id": "target-b",
                            "operation_id": "read"
                        }
                    ]
                }
            }),
        )];

        let ranked = rank_hotspots(&artifacts, 10);

        assert_eq!(ranked[0].key, "finding-a");
        assert_eq!(ranked[0].score, 4.0);
        assert_eq!(ranked[1].key, "target_id=target-b|operation_id=read");
        assert_eq!(ranked[1].score, 2.0);
    }

    #[test]
    fn fallback_uses_coverage_gaps_when_findings_are_absent() {
        let artifacts = vec![artifact(
            "run-a",
            json!({
                "schema": "https://schemas.homeboy.dev/fuzz/result-envelope.json",
                "campaign": {
                    "coverage": [
                        {
                            "id": "operation-coverage",
                            "gaps": [
                                { "id": "gap-a", "label": "Gap A" },
                                { "target_id": "target-a", "operation_id": "delete" }
                            ]
                        }
                    ]
                }
            }),
        )];

        let ranked = rank_hotspots(&artifacts, 10);

        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].key, "gap-a");
        assert_eq!(ranked[0].label.as_deref(), Some("Gap A"));
        assert_eq!(ranked[1].key, "target_id=target-a|operation_id=delete");
    }

    #[test]
    fn standardized_hotspots_take_precedence_over_fallback_sources() {
        let points = extract_hotspot_points(&json!({
            "schema": "https://schemas.homeboy.dev/fuzz/result-envelope.json",
            "hotspots": [{ "id": "explicit", "score": 9 }],
            "campaign": {
                "findings": [{ "id": "fallback", "severity": "critical" }]
            }
        }));

        assert_eq!(points.len(), 1);
        assert_eq!(points[0].key, "explicit");
    }
}
