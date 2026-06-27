//! Cross-run hotspot aggregation over persisted fuzz artifacts.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::Path;

use clap::Args;
use serde::Serialize;
use serde_json::Value;

use homeboy::core::fuzz::{
    compare_fuzz_hotspot_cohorts, parse_fuzz_hotspot_set_value, parse_fuzz_observation_set_value,
    rank_fuzz_observation_set_hotspots, FuzzHotspotCohortComparison, FuzzHotspotCohortItem,
};
use homeboy::core::observation::runs_service;
use homeboy::core::observation::{ArtifactRecord, ObservationStore, RunRecord};
use homeboy::core::Error;

use super::common::SkippedArtifactRow;
use super::{CmdResult, RunsOutput};

#[derive(Args, Clone, Debug)]
pub struct RunsHotspotsArgs {
    /// One or more persisted Homeboy run ids to inspect.
    #[arg(value_name = "RUN_ID")]
    pub run_ids: Vec<String>,

    /// Baseline run id for threshold-free cohort comparison.
    #[arg(long = "baseline-run", value_name = "RUN_ID")]
    pub baseline_runs: Vec<String>,

    /// Candidate run id for threshold-free cohort comparison.
    #[arg(long = "candidate-run", value_name = "RUN_ID")]
    pub candidate_runs: Vec<String>,

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cohort_comparison: Option<FuzzHotspotCohortComparison>,
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
    validate_hotspot_args(&args)?;
    let store = ObservationStore::open_initialized()?;
    let ranking_run_ids = if args.run_ids.is_empty() {
        combined_run_ids(&args.baseline_runs, &args.candidate_runs)
    } else {
        args.run_ids.clone()
    };
    let loaded = load_fuzz_artifacts_for_runs(&store, &ranking_run_ids)?;
    let hotspots = rank_hotspots(&loaded.artifacts, limit);
    let cohort_comparison = cohort_comparison(&store, &args.baseline_runs, &args.candidate_runs)?;

    Ok((
        RunsOutput::Hotspots(RunsHotspotsOutput {
            command: "runs.hotspots",
            run_ids: ranking_run_ids,
            inspected_artifact_count: loaded.inspected_artifact_count,
            matched_artifact_count: loaded.artifacts.len(),
            skipped_artifact_count: loaded.skipped.len(),
            skipped_artifacts: loaded.skipped,
            hotspots,
            cohort_comparison,
        }),
        0,
    ))
}

fn validate_hotspot_args(args: &RunsHotspotsArgs) -> homeboy::core::Result<()> {
    if args.run_ids.is_empty() && args.baseline_runs.is_empty() && args.candidate_runs.is_empty() {
        return Err(Error::validation_invalid_argument(
            "run_id",
            "provide at least one run id or a baseline/candidate cohort".to_string(),
            None,
            Some(vec![
                "Use `homeboy runs hotspots <run-id> ...` for aggregate ranking.".to_string(),
                "Use `homeboy runs hotspots --baseline-run <run-id> --candidate-run <run-id>` for threshold-free cohort comparison.".to_string(),
            ]),
        ));
    }
    if args.baseline_runs.is_empty() != args.candidate_runs.is_empty() {
        return Err(Error::validation_invalid_argument(
            "baseline-run",
            "baseline and candidate cohorts must both be provided".to_string(),
            None,
            Some(vec![
                "Add one or more `--baseline-run` values and one or more `--candidate-run` values."
                    .to_string(),
            ]),
        ));
    }
    Ok(())
}

fn combined_run_ids(baseline_runs: &[String], candidate_runs: &[String]) -> Vec<String> {
    baseline_runs
        .iter()
        .chain(candidate_runs.iter())
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn cohort_comparison(
    store: &ObservationStore,
    baseline_runs: &[String],
    candidate_runs: &[String],
) -> homeboy::core::Result<Option<FuzzHotspotCohortComparison>> {
    if baseline_runs.is_empty() && candidate_runs.is_empty() {
        return Ok(None);
    }

    let baseline = load_fuzz_artifacts_for_runs(store, baseline_runs)?;
    let candidate = load_fuzz_artifacts_for_runs(store, candidate_runs)?;
    let baseline_hotspots = rank_hotspots(&baseline.artifacts, usize::MAX);
    let candidate_hotspots = rank_hotspots(&candidate.artifacts, usize::MAX);

    Ok(Some(compare_fuzz_hotspot_cohorts(
        cohort_id("baseline", baseline_runs),
        cohort_id("candidate", candidate_runs),
        &cohort_items(&baseline_hotspots),
        &cohort_items(&candidate_hotspots),
    )))
}

fn cohort_id(prefix: &str, run_ids: &[String]) -> String {
    if run_ids.len() == 1 {
        return run_ids[0].clone();
    }
    format!("{prefix}:{}", run_ids.join(","))
}

fn cohort_items(hotspots: &[HotspotRanking]) -> Vec<FuzzHotspotCohortItem> {
    hotspots
        .iter()
        .map(|hotspot| FuzzHotspotCohortItem {
            key: hotspot.key.clone(),
            label: hotspot.label.clone(),
            score: hotspot.score,
            occurrences: hotspot.occurrences,
            run_count: hotspot.run_count,
            rank: hotspot.rank,
        })
        .collect()
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
    let mut points = typed_hotspots(json);
    if !points.is_empty() {
        return points;
    }

    points = standardized_hotspots(json);
    if !points.is_empty() {
        return points;
    }

    points.extend(finding_hotspots(json));
    points.extend(coverage_gap_hotspots(json));
    points
}

fn typed_hotspots(json: &Value) -> Vec<HotspotPoint> {
    let mut points = Vec::new();
    collect_typed_hotspots(json, &mut points);
    points
}

fn collect_typed_hotspots(json: &Value, points: &mut Vec<HotspotPoint>) {
    if let Some(set) = parse_fuzz_hotspot_set_value(json) {
        points.extend(set.items.into_iter().map(|item| HotspotPoint {
            key: item.id,
            label: item.label,
            score: item.relative_score.unwrap_or(item.value),
            source: format!("typed:{}", set.id),
        }));
        return;
    }

    if let Some(observation_set) = parse_fuzz_observation_set_value(json) {
        let hotspot_set = rank_fuzz_observation_set_hotspots(&observation_set);
        points.extend(hotspot_set.items.into_iter().map(|item| HotspotPoint {
            key: item.id,
            label: item.label,
            score: item.relative_score.unwrap_or(item.value),
            source: format!("typed:{}", hotspot_set.id),
        }));
        return;
    }

    match json {
        Value::Array(items) => {
            for item in items {
                collect_typed_hotspots(item, points);
            }
        }
        Value::Object(object) => {
            for (key, item) in object {
                if key != "prior_observations" {
                    collect_typed_hotspots(item, points);
                }
            }
        }
        _ => {}
    }
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
    fn typed_hotspots_take_precedence_over_loose_payloads() {
        let points = extract_hotspot_points(&json!({
            "schema": "homeboy/fuzz-result-envelope/v1",
            "hotspots": {
                "schema": "homeboy/fuzz-hotspot-set/v1",
                "id": "set-1",
                "items": [
                    {
                        "id": "route:search",
                        "dimension": "route",
                        "kind": "request",
                        "metric": "duration",
                        "value": 123.0,
                        "unit": "ms",
                        "basis": "per_case",
                        "sample_count": 10,
                        "rank": 1,
                        "relative_score": 0.91,
                        "label": "Search route",
                        "labels": ["read", "hot"],
                        "evidence_refs": ["case-log:1"],
                        "artifact_refs": ["profile.json"]
                    }
                ]
            },
            "campaign": {
                "findings": [{ "id": "fallback", "severity": "critical" }]
            }
        }));

        assert_eq!(points.len(), 1);
        assert_eq!(points[0].key, "route:search");
        assert_eq!(points[0].label.as_deref(), Some("Search route"));
        assert_eq!(points[0].score, 0.91);
        assert_eq!(points[0].source, "typed:set-1");
    }

    #[test]
    fn nested_typed_hotspots_use_contract_score() {
        let points = extract_hotspot_points(&json!({
            "schema": "homeboy/fuzz-result-envelope/v1",
            "metadata": {
                "hotspots": {
                    "schema": "homeboy/fuzz-hotspot-set/v1",
                    "id": "metadata-set",
                    "items": [
                        {
                            "id": "route:checkout",
                            "dimension": "route",
                            "metric": "duration",
                            "value": 950.0,
                            "unit": "ms",
                            "relative_score": 0.82
                        }
                    ]
                }
            }
        }));

        assert_eq!(points.len(), 1);
        assert_eq!(points[0].key, "route:checkout");
        assert_eq!(points[0].score, 0.82);
        assert_eq!(points[0].source, "typed:metadata-set");
    }

    #[test]
    fn typed_observation_sets_are_ranked_as_hotspot_points() {
        let points = extract_hotspot_points(&json!({
            "schema": "homeboy/fuzz-observation-set/v1",
            "version": 1,
            "id": "observations-1",
            "observations": [
                {
                    "id": "obs-1",
                    "family": "timing",
                    "subject": "search route",
                    "metric": "duration",
                    "value": 120.0,
                    "unit": "ms",
                    "fingerprint": "route:search"
                }
            ]
        }));

        assert_eq!(points.len(), 1);
        assert_eq!(points[0].key, "route:search");
        assert_eq!(points[0].label.as_deref(), Some("search route"));
        assert_eq!(points[0].score, 1.0);
        assert_eq!(points[0].source, "typed:observations-1-hotspots");
    }

    #[test]
    fn cohort_comparison_accepts_typed_observation_artifact_inputs() {
        let baseline = rank_hotspots(
            &[artifact(
                "baseline-run",
                json!({
                    "schema": "homeboy/fuzz-observation-set/v1",
                    "version": 1,
                    "id": "baseline-observations",
                    "observations": [
                        {
                            "id": "baseline-slow-query",
                            "family": "query",
                            "subject": "query-a",
                            "metric": "duration",
                            "value": 20.0,
                            "unit": "ms",
                            "fingerprint": "query-a:duration"
                        }
                    ]
                }),
            )],
            usize::MAX,
        );
        let candidate = rank_hotspots(
            &[artifact(
                "candidate-run",
                json!({
                    "schema": "homeboy/fuzz-observation-set/v1",
                    "version": 1,
                    "id": "candidate-observations",
                    "observations": [
                        {
                            "id": "candidate-slow-query",
                            "family": "query",
                            "subject": "query-a",
                            "metric": "duration",
                            "value": 40.0,
                            "unit": "ms",
                            "fingerprint": "query-a:duration"
                        }
                    ]
                }),
            )],
            usize::MAX,
        );

        let comparison = compare_fuzz_hotspot_cohorts(
            "baseline-run",
            "candidate-run",
            &cohort_items(&baseline),
            &cohort_items(&candidate),
        );

        assert_eq!(comparison.item_count, 1);
        assert_eq!(comparison.items[0].key, "query-a:duration");
        assert_eq!(comparison.items[0].change_kind, "unchanged");
        assert!(serde_json::to_value(&comparison)
            .expect("serialize comparison")
            .get("status")
            .is_none());
    }

    #[test]
    fn standardized_hotspots_extract_nested_observation_payloads() {
        let points = extract_hotspot_points(&json!({
            "schema": "homeboy/fuzz-campaign/v1",
            "metadata": {
                "runner_fuzz_result": {
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
