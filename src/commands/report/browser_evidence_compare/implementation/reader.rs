use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use homeboy::core::extension::trace::trace_browser_summary_has_signal;
use homeboy::core::extension::TraceBrowserEvidenceAdapterConfig;

use super::super::types::ArtifactRef;
use super::parse::{
    artifact_ref, assertion_stats, browser_metric_names, collect_artifacts,
    collect_declared_artifact_map_adapters, collect_declared_browser_summary_adapters,
    collect_matrix, collect_metric_object, collect_requests, collect_top_level_numbers,
    error_count, first_number, first_string, lifecycle_metric_names,
};
use super::{BrowserEvidenceSample, EvidenceSet, SampleContext};

pub(in crate::commands::report::browser_evidence_compare) fn read_evidence_set(
    root: &Path,
    include_local_paths: bool,
    adapters: &[TraceBrowserEvidenceAdapterConfig],
) -> homeboy::core::Result<EvidenceSet> {
    let mut notes = Vec::new();
    let mut files = Vec::new();
    collect_json_files(root, &mut files).map_err(|e| {
        homeboy::core::Error::internal_unexpected(format!(
            "Failed to read browser evidence directory {}: {}",
            root.display(),
            e
        ))
    })?;
    files.sort();

    let mut parsed = Vec::new();
    for file in files {
        let raw = match std::fs::read_to_string(&file) {
            Ok(raw) => raw,
            Err(err) => {
                notes.push(format!(
                    "skipped unreadable artifact {}: {}",
                    file.display(),
                    err
                ));
                continue;
            }
        };
        let value = match serde_json::from_str::<Value>(&raw) {
            Ok(value) => value,
            Err(err) => {
                notes.push(format!(
                    "skipped invalid JSON artifact {}: {}",
                    file.display(),
                    err
                ));
                continue;
            }
        };
        parsed.push((file, value));
    }

    // Files that other trace files declare as artifacts (e.g. artifacts/*-metrics.json) are
    // ingested into their owning sample below; reading them again as standalone evidence would
    // double count metrics into a phantom variant, so collect their canonical paths first and
    // skip them as top-level evidence sources.
    let declared_artifact_paths = declared_artifact_paths(&parsed, adapters);

    let mut samples = Vec::new();
    let mut artifacts = BTreeSet::new();
    for (file, value) in &parsed {
        if is_declared_artifact_file(file, &declared_artifact_paths) {
            continue;
        }
        let source = artifact_ref(root, file, include_local_paths, None);
        let source_dir = file.parent().map(Path::to_path_buf);
        let before = samples.len();
        collect_samples(
            value,
            &SampleContext::default(),
            &source,
            source_dir.as_deref(),
            &mut samples,
            &mut artifacts,
            adapters,
        );
        if samples.len() == before {
            collect_provenance_artifacts(value, &source, &mut artifacts, adapters);
        }
    }

    if samples.is_empty() {
        notes.push("no browser evidence samples found".to_string());
    }

    for sample in &samples {
        artifacts.extend(sample.artifacts.iter().cloned());
    }

    Ok(EvidenceSet {
        samples,
        artifacts,
        notes,
    })
}

pub(in crate::commands::report::browser_evidence_compare) fn read_evidence_dirs(
    roots: &[PathBuf],
    include_local_paths: bool,
    adapters: &[TraceBrowserEvidenceAdapterConfig],
) -> homeboy::core::Result<EvidenceSet> {
    let mut merged = EvidenceSet {
        samples: Vec::new(),
        artifacts: BTreeSet::new(),
        notes: Vec::new(),
    };
    for root in roots {
        match read_evidence_set(root, include_local_paths, adapters) {
            Ok(mut set) => {
                merged.samples.append(&mut set.samples);
                merged.artifacts.append(&mut set.artifacts);
                merged.notes.append(&mut set.notes);
            }
            Err(err) => merged.notes.push(format!(
                "skipped unreadable evidence directory {}: {}",
                root.display(),
                err.message
            )),
        }
    }
    if roots.is_empty() {
        merged
            .notes
            .push("no browser evidence directories were provided".to_string());
    }
    Ok(merged)
}

/// Collect the canonical filesystem paths of every JSON file declared as an artifact by any
/// parsed trace file. Used to avoid treating artifact-backed metric files as standalone
/// evidence samples.
fn declared_artifact_paths(
    parsed: &[(PathBuf, Value)],
    adapters: &[TraceBrowserEvidenceAdapterConfig],
) -> BTreeSet<PathBuf> {
    let mut declared = BTreeSet::new();
    for (file, value) in parsed {
        let Some(source_dir) = file.parent() else {
            continue;
        };
        let mut refs = BTreeSet::new();
        collect_artifact_targets(value, &mut refs, adapters);
        for artifact in refs {
            if let Some(path) = resolve_local_artifact_path(source_dir, &artifact.target) {
                if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
                    declared.insert(canonical_path(&path));
                }
            }
        }
    }
    declared
}

fn is_declared_artifact_file(file: &Path, declared: &BTreeSet<PathBuf>) -> bool {
    declared.contains(&canonical_path(file))
}

fn canonical_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Recursively collect every declared artifact reference (both `artifacts` arrays and adapter
/// artifact maps) from a parsed JSON value.
fn collect_artifact_targets(
    value: &Value,
    artifacts: &mut BTreeSet<ArtifactRef>,
    adapters: &[TraceBrowserEvidenceAdapterConfig],
) {
    match value {
        Value::Object(object) => {
            collect_object_artifacts(object, artifacts, adapters);
            for value in object.values() {
                collect_artifact_targets(value, artifacts, adapters);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_artifact_targets(value, artifacts, adapters);
            }
        }
        _ => {}
    }
}

fn collect_json_files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_json_files(&path, out)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
            out.push(path);
        }
    }
    Ok(())
}

fn collect_samples(
    value: &Value,
    inherited: &SampleContext,
    source: &ArtifactRef,
    source_dir: Option<&Path>,
    samples: &mut Vec<BrowserEvidenceSample>,
    artifacts: &mut BTreeSet<ArtifactRef>,
    adapters: &[TraceBrowserEvidenceAdapterConfig],
) {
    match value {
        Value::Object(object) => collect_object_samples(
            object, inherited, source, source_dir, samples, artifacts, adapters,
        ),
        Value::Array(array) => {
            for item in array {
                collect_samples(
                    item, inherited, source, source_dir, samples, artifacts, adapters,
                );
            }
        }
        _ => {}
    }
}

fn collect_object_samples(
    object: &Map<String, Value>,
    inherited: &SampleContext,
    source: &ArtifactRef,
    source_dir: Option<&Path>,
    samples: &mut Vec<BrowserEvidenceSample>,
    artifacts: &mut BTreeSet<ArtifactRef>,
    adapters: &[TraceBrowserEvidenceAdapterConfig],
) {
    let context = context_for_object(object, inherited);
    let runs = object.get("runs").and_then(Value::as_array);

    if has_browser_signal(object, adapters) && runs.is_none() {
        samples.push(sample_from_object(
            object, &context, source, source_dir, adapters,
        ));
    } else if has_provenance_signal(object) {
        collect_object_artifacts(object, artifacts, adapters);
        artifacts.insert(source.clone());
    }

    if let Some(data) = object.get("data") {
        collect_samples(
            data, &context, source, source_dir, samples, artifacts, adapters,
        );
    }
    for key in ["scenarios", "profiles", "variants", "matrix", "results"] {
        if let Some(value) = object.get(key) {
            collect_samples(
                value, &context, source, source_dir, samples, artifacts, adapters,
            );
        }
    }
    if let Some(runs) = runs {
        for (index, run) in runs.iter().enumerate() {
            let mut run_context = context.clone();
            if run_context.profile.is_none() {
                run_context.profile = Some(format!("repeat-{}", index + 1));
            }
            collect_samples(
                run,
                &run_context,
                source,
                source_dir,
                samples,
                artifacts,
                adapters,
            );
        }
    }
}

fn context_for_object(object: &Map<String, Value>, inherited: &SampleContext) -> SampleContext {
    let mut context = inherited.clone();
    context.scenario = first_string(object, &["scenario_id", "scenario", "id"])
        .or(context.scenario)
        .filter(|value| value != "results" && value != "data");
    context.profile = first_string(
        object,
        &["profile_id", "profile", "browser_profile", "name"],
    )
    .or(context.profile);
    for key in ["matrix", "variant", "axes", "settings"] {
        if let Some(value) = object.get(key) {
            collect_matrix(value, key, &mut context.matrix);
        }
    }
    context
}

fn has_browser_signal(
    object: &Map<String, Value>,
    adapters: &[TraceBrowserEvidenceAdapterConfig],
) -> bool {
    [
        "assertions",
        "requests",
        "network_requests",
        "request_summary",
        "browser_metrics",
        "lifecycle_metrics",
        "dom_lifecycle",
        "console_errors",
        "page_errors",
        "errors",
    ]
    .iter()
    .any(|key| object.contains_key(*key))
        || first_number(
            object,
            &[
                "request_count",
                "requests_total",
                "dom_content_loaded_ms",
                "load_event_ms",
                "lcp_ms",
            ],
        )
        .is_some()
        || object
            .get("summary")
            .and_then(Value::as_object)
            .is_some_and(|summary| {
                summary.contains_key("assertions")
                    || trace_browser_summary_has_signal(summary, adapters)
                    || summary
                        .get("metrics")
                        .and_then(Value::as_object)
                        .is_some_and(|metrics| {
                            has_metric_object_signal(metrics, &browser_metric_names())
                                || has_metric_object_signal(metrics, &lifecycle_metric_names())
                        })
            })
}

fn has_metric_object_signal(object: &Map<String, Value>, names: &[&str]) -> bool {
    object
        .iter()
        .any(|(key, value)| names.contains(&key.as_str()) && value.as_f64().is_some())
}

fn has_provenance_signal(object: &Map<String, Value>) -> bool {
    object.contains_key("artifacts")
        || first_string(
            object,
            &["artifact", "artifact_id", "manifest", "manifest_id"],
        )
        .is_some()
        || first_string(object, &["id", "name"]).is_some_and(|value| {
            value.contains("artifact") || value.contains("manifest") || value.contains("provenance")
        })
}

fn collect_provenance_artifacts(
    value: &Value,
    source: &ArtifactRef,
    artifacts: &mut BTreeSet<ArtifactRef>,
    adapters: &[TraceBrowserEvidenceAdapterConfig],
) {
    match value {
        Value::Object(object) => {
            if has_provenance_signal(object) {
                collect_object_artifacts(object, artifacts, adapters);
                artifacts.insert(source.clone());
            }
            for value in object.values() {
                collect_provenance_artifacts(value, source, artifacts, adapters);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_provenance_artifacts(value, source, artifacts, adapters);
            }
        }
        _ => {}
    }
}

fn collect_object_artifacts(
    object: &Map<String, Value>,
    artifacts: &mut BTreeSet<ArtifactRef>,
    adapters: &[TraceBrowserEvidenceAdapterConfig],
) {
    collect_artifacts(object, artifacts);
    collect_declared_artifact_map_adapters(object, artifacts, adapters);
}

fn sample_from_object(
    object: &Map<String, Value>,
    context: &SampleContext,
    source: &ArtifactRef,
    source_dir: Option<&Path>,
    adapters: &[TraceBrowserEvidenceAdapterConfig],
) -> BrowserEvidenceSample {
    let mut sample = BrowserEvidenceSample {
        scenario: context.scenario.clone(),
        profile: context.profile.clone(),
        matrix: context.matrix.clone(),
        source_artifact: Some(source.clone()),
        ..BrowserEvidenceSample::default()
    };
    sample.assertions = assertion_stats(object.get("assertions").or_else(|| {
        object
            .get("summary")
            .and_then(Value::as_object)
            .and_then(|summary| summary.get("assertions"))
    }));
    collect_requests(object, &mut sample);
    collect_metric_object(
        object.get("browser_metrics"),
        &mut sample.browser_metrics,
        &browser_metric_names(),
    );
    collect_metric_object(
        object.get("metrics"),
        &mut sample.browser_metrics,
        &browser_metric_names(),
    );
    collect_metric_object(
        object
            .get("summary")
            .and_then(Value::as_object)
            .and_then(|summary| summary.get("metrics")),
        &mut sample.browser_metrics,
        &browser_metric_names(),
    );
    collect_top_level_numbers(object, &mut sample.browser_metrics, &browser_metric_names());
    collect_metric_object(
        object.get("lifecycle_metrics"),
        &mut sample.lifecycle_metrics,
        &lifecycle_metric_names(),
    );
    collect_metric_object(
        object.get("dom_lifecycle"),
        &mut sample.lifecycle_metrics,
        &lifecycle_metric_names(),
    );
    collect_top_level_numbers(
        object,
        &mut sample.lifecycle_metrics,
        &lifecycle_metric_names(),
    );
    if let Some(summary) = object.get("summary").and_then(Value::as_object) {
        collect_declared_browser_summary_adapters(summary, &mut sample, adapters);
    }
    sample.console_errors = sample
        .console_errors
        .or_else(|| error_count(object, &["console_errors", "consoleErrors"]));
    sample.page_errors = sample
        .page_errors
        .or_else(|| error_count(object, &["page_errors", "pageErrors", "errors"]));
    collect_artifacts(object, &mut sample.artifacts);
    collect_declared_artifact_map_adapters(object, &mut sample.artifacts, adapters);
    ingest_artifact_backed_metrics(&mut sample, source_dir, adapters);
    if sample.browser_metrics.is_empty() && sample.lifecycle_metrics.is_empty() {
        sample
            .notes
            .push("timing metrics missing or not numeric".to_string());
    }
    sample
}

/// Pull structured metrics out of declared artifact files (e.g. `artifacts/*-metrics.json`)
/// referenced from a trace sample. Trace producers frequently leave request counts and
/// browser metrics only in an artifact JSON while the top-level trace.json carries a string
/// summary, so without this the reporter would render "No comparable metrics found".
///
/// This is intentionally generic: it resolves each declared artifact relative to the trace
/// file's directory, parses any readable JSON, and reuses the same field-recognition helpers
/// used for inline trace metrics. Values already present from the top-level trace.json take
/// precedence; artifact files only fill in what is missing.
fn ingest_artifact_backed_metrics(
    sample: &mut BrowserEvidenceSample,
    source_dir: Option<&Path>,
    adapters: &[TraceBrowserEvidenceAdapterConfig],
) {
    let Some(source_dir) = source_dir else {
        return;
    };
    let targets = sample
        .artifacts
        .iter()
        .map(|artifact| artifact.target.clone())
        .collect::<Vec<_>>();
    for target in targets {
        let Some(path) = resolve_local_artifact_path(source_dir, &target) else {
            continue;
        };
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        if merge_artifact_metrics(sample, &value, adapters) {
            sample
                .notes
                .push(format!("ingested artifact-backed metrics from {}", target));
        }
    }
}

/// Resolve a declared artifact reference to a local file path relative to the trace file's
/// directory. Rejects parent-directory traversal and remote (URL) targets so only artifacts
/// co-located with the trace evidence are read.
fn resolve_local_artifact_path(source_dir: &Path, target: &str) -> Option<PathBuf> {
    if target.is_empty() || target.contains("://") {
        return None;
    }
    let relative = Path::new(target);
    if relative.is_absolute() {
        return relative.exists().then(|| relative.to_path_buf());
    }
    if relative
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return None;
    }
    let candidate = source_dir.join(relative);
    candidate.exists().then_some(candidate)
}

/// Merge metrics found in an artifact JSON value into the sample, only filling fields that are
/// still empty. Returns true if any metric was contributed by the artifact.
fn merge_artifact_metrics(
    sample: &mut BrowserEvidenceSample,
    value: &Value,
    adapters: &[TraceBrowserEvidenceAdapterConfig],
) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let mut staged = BrowserEvidenceSample::default();
    collect_requests(object, &mut staged);
    collect_metric_object(
        object.get("browser_metrics"),
        &mut staged.browser_metrics,
        &browser_metric_names(),
    );
    collect_metric_object(
        object.get("metrics"),
        &mut staged.browser_metrics,
        &browser_metric_names(),
    );
    collect_metric_object(
        object
            .get("summary")
            .and_then(Value::as_object)
            .and_then(|summary| summary.get("metrics")),
        &mut staged.browser_metrics,
        &browser_metric_names(),
    );
    collect_top_level_numbers(object, &mut staged.browser_metrics, &browser_metric_names());
    collect_metric_object(
        object.get("lifecycle_metrics"),
        &mut staged.lifecycle_metrics,
        &lifecycle_metric_names(),
    );
    collect_metric_object(
        object.get("dom_lifecycle"),
        &mut staged.lifecycle_metrics,
        &lifecycle_metric_names(),
    );
    collect_top_level_numbers(
        object,
        &mut staged.lifecycle_metrics,
        &lifecycle_metric_names(),
    );
    if let Some(summary) = object.get("summary").and_then(Value::as_object) {
        collect_declared_browser_summary_adapters(summary, &mut staged, adapters);
    }
    staged.console_errors = staged
        .console_errors
        .or_else(|| error_count(object, &["console_errors", "consoleErrors"]));
    staged.page_errors = staged
        .page_errors
        .or_else(|| error_count(object, &["page_errors", "pageErrors", "errors"]));

    let mut contributed = false;
    if sample.request_total.is_none() {
        if let Some(request_total) = staged.request_total {
            sample.request_total = Some(request_total);
            contributed = true;
        }
    }
    contributed |= merge_missing_count_map(&mut sample.request_by_host, &staged.request_by_host);
    contributed |= merge_missing_count_map(&mut sample.request_by_type, &staged.request_by_type);
    contributed |= merge_missing_count_map(&mut sample.browser_metrics, &staged.browser_metrics);
    contributed |=
        merge_missing_count_map(&mut sample.lifecycle_metrics, &staged.lifecycle_metrics);
    if sample.console_errors.is_none() {
        if let Some(console_errors) = staged.console_errors {
            sample.console_errors = Some(console_errors);
            contributed = true;
        }
    }
    if sample.page_errors.is_none() {
        if let Some(page_errors) = staged.page_errors {
            sample.page_errors = Some(page_errors);
            contributed = true;
        }
    }
    contributed
}

fn merge_missing_count_map(
    target: &mut BTreeMap<String, f64>,
    source: &BTreeMap<String, f64>,
) -> bool {
    let mut contributed = false;
    for (key, value) in source {
        if !target.contains_key(key) {
            target.insert(key.clone(), *value);
            contributed = true;
        }
    }
    contributed
}
