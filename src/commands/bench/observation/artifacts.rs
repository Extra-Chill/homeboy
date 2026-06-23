use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use homeboy::core::artifacts as artifact_links;
use homeboy::core::engine::run_dir::{self, RunDir};
use homeboy::core::extension::bench::{
    BenchDiagnostic, BenchDiagnosticSource, BenchResults,
};
use homeboy::core::observation::{finding_records_from_budget, ArtifactRecord};

use super::lifecycle::BenchObservation;

pub(super) fn record_bench_observation_artifacts(
    observation: &BenchObservation,
    workflow: &mut homeboy::core::extension::bench::BenchRunWorkflowResult,
    run_dir: &RunDir,
) {
    if let Some(results) = workflow.results.as_mut() {
        workflow
            .diagnostics
            .extend(persist_bench_result_artifact_paths(
                observation,
                results,
                run_dir,
            ));
        rewrite_bench_results_file(results, run_dir);
    }

    record_if_exists(
        observation,
        "bench_results",
        run_dir.step_file(run_dir::files::BENCH_RESULTS),
    );
    record_if_exists(
        observation,
        "resource_summary",
        run_dir.step_file(run_dir::files::RESOURCE_SUMMARY),
    );
    record_memory_timeline_artifacts(observation, run_dir);

    let Some(results) = workflow.results.as_ref() else {
        return;
    };
    observation.0.record_findings(&finding_records_from_budget(
        observation.run_id(),
        &results.budget_findings,
    ));
}

pub(super) fn record_if_exists(observation: &BenchObservation, kind: &str, path: PathBuf) {
    observation.0.record_artifact_if_file(kind, &path);
}

pub(super) fn record_memory_timeline_artifacts(observation: &BenchObservation, run_dir: &RunDir) {
    let Ok(entries) = fs::read_dir(run_dir.path()) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.starts_with("bench-memory-timeline")
            && (name.ends_with(".json") || name.ends_with(".csv"))
        {
            observation
                .0
                .record_artifact_if_file("bench_memory_timeline", &path);
        }
    }
}

fn persist_bench_result_artifact_paths(
    observation: &BenchObservation,
    results: &mut BenchResults,
    run_dir: &RunDir,
) -> Vec<BenchDiagnostic> {
    let mut diagnostics = Vec::new();
    let shared_state = results
        .run_metadata
        .as_ref()
        .and_then(|metadata| metadata.shared_state.as_deref());
    for scenario in &mut results.scenarios {
        for (name, artifact) in &mut scenario.artifacts {
            if let Some(diagnostic) = persist_bench_artifact(
                observation,
                &scenario.id,
                None,
                name,
                artifact,
                run_dir,
                shared_state,
            ) {
                diagnostics.push(diagnostic);
            }
        }
        if let Some(runs) = &mut scenario.runs {
            for (run_index, run) in runs.iter_mut().enumerate() {
                for (name, artifact) in &mut run.artifacts {
                    if let Some(diagnostic) = persist_bench_artifact(
                        observation,
                        &scenario.id,
                        Some(run_index),
                        name,
                        artifact,
                        run_dir,
                        shared_state,
                    ) {
                        diagnostics.push(diagnostic);
                    }
                }
            }
        }
    }
    diagnostics
}

fn persist_bench_artifact(
    observation: &BenchObservation,
    scenario_id: &str,
    run_index: Option<usize>,
    name: &str,
    artifact: &mut homeboy::core::extension::bench::BenchArtifact,
    run_dir: &RunDir,
    shared_state: Option<&str>,
) -> Option<BenchDiagnostic> {
    let kind = artifact.kind.clone().unwrap_or_else(|| name.to_string());
    let metadata = bench_artifact_metadata(scenario_id, run_index, name, artifact);

    if let Some(url) = artifact.url.clone() {
        return match observation.0.store().record_url_artifact_with_metadata(
            observation.run_id(),
            &kind,
            &url,
            metadata,
        ) {
            Ok(record) => {
                apply_recorded_bench_artifact_links(scenario_id, run_index, name, artifact, &record)
            }
            Err(error) => Some(bench_artifact_diagnostic(
                scenario_id,
                run_index,
                name,
                "bench_artifact_url_record_failed",
                format!("failed to record bench URL artifact `{name}`: {error}"),
                serde_json::json!({ "url": url }),
            )),
        };
    }

    let original_path = artifact.path.clone()?;
    if bench_artifact_path_is_blocked(&original_path) {
        return Some(bench_artifact_diagnostic(
            scenario_id,
            run_index,
            name,
            "bench_artifact_path_blocked",
            format!("blocked bench artifact path `{original_path}` for `{name}`"),
            serde_json::json!({ "path": original_path }),
        ));
    }

    let path = resolve_bench_artifact_path(&original_path, run_dir, shared_state);
    let record = if path.is_file() {
        observation.0.store().record_artifact_with_metadata(
            observation.run_id(),
            "bench_artifact",
            &path,
            metadata,
        )
    } else if path.is_dir() {
        observation
            .0
            .store()
            .record_directory_artifact_with_metadata(
                observation.run_id(),
                "bench_artifact",
                &path,
                metadata,
            )
    } else {
        return Some(bench_artifact_diagnostic(
            scenario_id,
            run_index,
            name,
            "bench_artifact_path_missing",
            format!("bench artifact path for `{name}` was not found: {original_path}"),
            serde_json::json!({
                "path": original_path,
                "resolved_path": path.to_string_lossy().to_string(),
            }),
        ));
    };

    match record {
        Ok(record) => {
            artifact.path = Some(record.path.clone());
            apply_recorded_bench_artifact_links(scenario_id, run_index, name, artifact, &record)
        }
        Err(error) => Some(bench_artifact_diagnostic(
            scenario_id,
            run_index,
            name,
            "bench_artifact_record_failed",
            format!("failed to persist bench artifact `{name}`: {error}"),
            serde_json::json!({
                "path": original_path,
                "resolved_path": path.to_string_lossy().to_string(),
            }),
        )),
    }
}

pub(in crate::commands::bench::observation) fn apply_recorded_bench_artifact_links(
    scenario_id: &str,
    run_index: Option<usize>,
    name: &str,
    artifact: &mut homeboy::core::extension::bench::BenchArtifact,
    record: &ArtifactRecord,
) -> Option<BenchDiagnostic> {
    artifact.observation_artifact_id = Some(record.id.clone());
    let public_url = artifact_links::public_artifact_url(record)?;
    artifact.public_url = Some(public_url.clone());
    artifact.viewer_links = artifact_links::cached_validated_viewer_links(record, &public_url);
    artifact.viewer_url = artifact.viewer_links.first().map(|link| link.url.clone());
    let validation = record.metadata_json.get("public_url_validation")?;
    let reachable = validation
        .get("reachable")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    (!reachable).then(|| {
        let error = validation
            .get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("public artifact URL was not reachable");
            bench_artifact_diagnostic(
                scenario_id,
                run_index,
                name,
                "bench_public_artifact_url_unreachable",
                format!(
                    "public artifact URL for bench artifact `{name}` is not reachable; viewer links were not published: {error}"
                ),
                serde_json::json!({
                    "url": validation.get("url").cloned().unwrap_or(serde_json::Value::Null),
                    "status_code": validation.get("status_code").cloned().unwrap_or(serde_json::Value::Null),
                    "error": validation.get("error").cloned().unwrap_or(serde_json::Value::Null),
                }),
            )
    })
}

fn bench_artifact_metadata(
    scenario_id: &str,
    run_index: Option<usize>,
    name: &str,
    artifact: &homeboy::core::extension::bench::BenchArtifact,
) -> serde_json::Value {
    serde_json::json!({
        "source": "bench",
        "scenario_id": scenario_id,
        "run_index": run_index,
        "name": name,
        "kind": artifact.kind,
        "label": artifact.label,
        "original_path": artifact.path,
        "url": artifact.url,
        "viewer": artifact.viewer,
    })
}

fn bench_artifact_path_is_blocked(path: &str) -> bool {
    Path::new(path)
        .components()
        .any(|component| matches!(component, Component::ParentDir))
}

fn bench_artifact_diagnostic(
    scenario_id: &str,
    run_index: Option<usize>,
    name: &str,
    class: &str,
    message: String,
    metadata: serde_json::Value,
) -> BenchDiagnostic {
    let mut diagnostic_metadata = BTreeMap::new();
    diagnostic_metadata.insert("artifact_name".to_string(), serde_json::json!(name));
    if let Some(object) = metadata.as_object() {
        for (key, value) in object {
            diagnostic_metadata.insert(key.clone(), value.clone());
        }
    }
    BenchDiagnostic {
        class: class.to_string(),
        message: Some(message),
        severity: None,
        source: Some(match run_index {
            Some(run_index) => BenchDiagnosticSource::ScenarioRun {
                scenario_id: scenario_id.to_string(),
                run_index,
            },
            None => BenchDiagnosticSource::Scenario {
                scenario_id: scenario_id.to_string(),
            },
        }),
        metadata: diagnostic_metadata,
    }
}

fn rewrite_bench_results_file(results: &BenchResults, run_dir: &RunDir) {
    let Ok(json) = serde_json::to_vec_pretty(results) else {
        return;
    };
    let _ = fs::write(run_dir.step_file(run_dir::files::BENCH_RESULTS), json);
}

fn resolve_bench_artifact_path(
    path: &str,
    run_dir: &RunDir,
    shared_state: Option<&str>,
) -> PathBuf {
    let artifact_path = PathBuf::from(path);
    if artifact_path.exists() {
        return artifact_path;
    }
    if artifact_path.is_absolute() {
        if let Some(shared_state_path) = resolve_shared_state_artifact(&artifact_path, shared_state)
        {
            return shared_state_path;
        }
        if let Some(preserved_path) = resolve_preserved_invocation_artifact(&artifact_path, run_dir)
        {
            return preserved_path;
        }
        return artifact_path;
    }
    let run_dir_path = run_dir.path().join(path);
    if run_dir_path.exists() {
        return run_dir_path;
    }
    artifact_path
}

fn resolve_shared_state_artifact(path: &Path, shared_state: Option<&str>) -> Option<PathBuf> {
    let shared_state = shared_state?;
    let relative = path.strip_prefix("/bench-shared-state").ok()?;
    let candidate = Path::new(shared_state).join(relative);
    if candidate.exists() {
        return Some(candidate);
    }
    None
}

fn resolve_preserved_invocation_artifact(path: &Path, run_dir: &RunDir) -> Option<PathBuf> {
    let mut components = path.components().peekable();
    while let Some(component) = components.next() {
        let name = component.as_os_str().to_string_lossy();
        let Some(short_id) = name.strip_suffix(".a") else {
            continue;
        };

        let mut preserved = run_dir
            .path()
            .join("invocations")
            .join(format!("inv-{short_id}"))
            .join("artifacts");
        for rest in components {
            preserved.push(rest.as_os_str());
        }
        if preserved.exists() {
            return Some(preserved);
        }
        return None;
    }
    None
}
