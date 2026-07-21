use std::collections::BTreeMap;
use std::path::Path;

use homeboy::core::engine::run_dir::RunDir;
use homeboy::core::observation::{NewFindingRecord, ObservationStore};
use homeboy_extension::trace as extension_trace;

use extension_trace::resolve_declared_trace_artifact_path;

#[derive(Debug, Default)]
pub(super) struct TraceArtifactObservationResult {
    pub missing_declared_artifacts: usize,
    pub invalid_declared_artifacts: usize,
    pub persistence_failures: usize,
    pub trace_results_path: Option<String>,
    pub artifact_dir_path: Option<String>,
    pub declared_artifact_paths: BTreeMap<String, String>,
}

impl TraceArtifactObservationResult {
    pub fn has_declared_artifact_failures(&self) -> bool {
        self.missing_declared_artifacts > 0 || self.invalid_declared_artifacts > 0
    }

    pub fn evidence_promoted(&self) -> bool {
        self.trace_results_path.is_some()
            && !self.has_declared_artifact_failures()
            && self.persistence_failures == 0
    }

    pub fn rewrite_declared_artifact_paths(&self, results: &mut extension_trace::TraceResults) {
        for artifact in &mut results.artifacts {
            if let Some(path) = self.declared_artifact_paths.get(&artifact.path) {
                artifact.path = path.clone();
            }
        }
    }
}

pub(super) fn record_trace_artifacts(
    store: &ObservationStore,
    run_id: &str,
    run_dir: &RunDir,
    results: Option<&extension_trace::TraceResults>,
) -> TraceArtifactObservationResult {
    let mut observation_result = TraceArtifactObservationResult::default();
    let trace_results_path =
        run_dir.step_file(homeboy::core::engine::run_dir::files::TRACE_RESULTS);
    observation_result.trace_results_path =
        record_artifact_if_file(store, run_id, "trace-results", &trace_results_path);
    if trace_results_path.is_file() && observation_result.trace_results_path.is_none() {
        observation_result.persistence_failures += 1;
    }
    let artifact_dir = run_dir.path().join("artifacts");
    if let Some(results) = results {
        for artifact in &results.artifacts {
            if let Some(resolved) =
                declared_trace_artifact_candidate(artifact, run_dir, &artifact_dir)
            {
                record_declared_artifact(
                    store,
                    run_id,
                    run_dir.path(),
                    artifact,
                    &resolved,
                    &mut observation_result,
                );
            } else {
                record_unresolved_declared_artifact(
                    store,
                    run_id,
                    run_dir.path(),
                    artifact,
                    &mut observation_result,
                );
            }
        }
    }
    let artifact_dir_exists = artifact_dir.is_dir();
    observation_result.artifact_dir_path =
        record_artifact_dir(store, run_id, "trace-artifacts", &artifact_dir);
    if artifact_dir_exists && observation_result.artifact_dir_path.is_none() {
        observation_result.persistence_failures += 1;
    }
    observation_result
}

fn declared_trace_artifact_candidate(
    artifact: &extension_trace::TraceArtifact,
    run_dir: &RunDir,
    artifact_dir: &Path,
) -> Option<std::path::PathBuf> {
    if let Some(path) = resolve_declared_trace_artifact_path(&artifact.path, run_dir, artifact_dir)
    {
        return Some(path);
    }

    let relative = Path::new(&artifact.path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return None;
    }

    Some(run_dir.path().join(relative))
}

fn record_artifact_if_file(
    store: &ObservationStore,
    run_id: &str,
    kind: &str,
    path: &Path,
) -> Option<String> {
    if path.is_file() {
        return store
            .record_artifact(run_id, kind, path)
            .ok()
            .map(|artifact| artifact.path);
    }
    None
}

fn record_artifact_dir(
    store: &ObservationStore,
    run_id: &str,
    kind: &str,
    path: &Path,
) -> Option<String> {
    if path.is_dir() {
        return store
            .record_directory_artifact(run_id, kind, path)
            .ok()
            .map(|artifact| artifact.path);
    }
    None
}

fn record_declared_artifact(
    store: &ObservationStore,
    run_id: &str,
    run_root: &Path,
    artifact: &extension_trace::TraceArtifact,
    resolved: &Path,
    result: &mut TraceArtifactObservationResult,
) {
    let metadata = match std::fs::metadata(resolved) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            result.missing_declared_artifacts += 1;
            record_declared_artifact_finding(
                store,
                run_id,
                "trace.artifact.missing",
                "error",
                format!(
                    "trace result declared artifact '{}' at '{}', but the path does not exist",
                    artifact.label, artifact.path
                ),
                artifact,
                run_root,
                resolved,
            );
            return;
        }
        Err(error) => {
            result.invalid_declared_artifacts += 1;
            record_declared_artifact_finding(
                store,
                run_id,
                "trace.artifact.metadata_error",
                "error",
                format!(
                    "trace result declared artifact '{}' at '{}' could not be inspected: {error}",
                    artifact.label, artifact.path
                ),
                artifact,
                run_root,
                resolved,
            );
            return;
        }
    };

    let record_result = if metadata.is_file() {
        store.record_artifact(run_id, "trace-artifact", resolved)
    } else if metadata.is_dir() {
        store.record_directory_artifact(run_id, "trace-artifact", resolved)
    } else {
        result.invalid_declared_artifacts += 1;
        record_declared_artifact_finding(
            store,
            run_id,
            "trace.artifact.unsupported_type",
            "error",
            format!(
                "trace result declared artifact '{}' at '{}' is neither a file nor a directory",
                artifact.label, artifact.path
            ),
            artifact,
            run_root,
            resolved,
        );
        return;
    };

    match record_result {
        Ok(record) => {
            result
                .declared_artifact_paths
                .insert(artifact.path.clone(), record.path);
        }
        Err(error) => {
            result.invalid_declared_artifacts += 1;
            result.persistence_failures += 1;
            record_declared_artifact_finding(
                store,
                run_id,
                "trace.artifact.record_failed",
                "error",
                format!(
                    "trace result declared artifact '{}' at '{}' could not be persisted: {}",
                    artifact.label, artifact.path, error.message
                ),
                artifact,
                run_root,
                resolved,
            );
        }
    }
}

fn record_unresolved_declared_artifact(
    store: &ObservationStore,
    run_id: &str,
    run_root: &Path,
    artifact: &extension_trace::TraceArtifact,
    result: &mut TraceArtifactObservationResult,
) {
    let declared_path = Path::new(&artifact.path);
    if declared_path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        result.invalid_declared_artifacts += 1;
        record_declared_artifact_finding(
            store,
            run_id,
            "trace.artifact.invalid_path",
            "error",
            format!(
                "trace result declared artifact '{}' at '{}' is outside the trace run directory",
                artifact.label, artifact.path
            ),
            artifact,
            run_root,
            declared_path,
        );
        return;
    }

    result.missing_declared_artifacts += 1;
    let resolved = if declared_path.is_absolute() {
        declared_path.to_path_buf()
    } else {
        run_root.join(declared_path)
    };
    record_declared_artifact_finding(
        store,
        run_id,
        "trace.artifact.missing",
        "error",
        format!(
            "trace result declared artifact '{}' at '{}', but the path does not exist",
            artifact.label, artifact.path
        ),
        artifact,
        run_root,
        &resolved,
    );
}

fn record_declared_artifact_finding(
    store: &ObservationStore,
    run_id: &str,
    rule: &str,
    severity: &str,
    message: String,
    artifact: &extension_trace::TraceArtifact,
    run_root: &Path,
    resolved: &Path,
) {
    let _ = store.record_finding(&NewFindingRecord {
        run_id: run_id.to_string(),
        tool: "trace".to_string(),
        rule: Some(rule.to_string()),
        file: None,
        line: None,
        severity: Some(severity.to_string()),
        fingerprint: Some(format!("{rule}:{}", artifact.path)),
        message,
        fixable: Some(false),
        metadata_json: serde_json::json!({
            "declared_artifact": {
                "label": artifact.label,
                "path": artifact.path,
                "resolved_path": resolved.to_string_lossy(),
                "relative_to_run_dir": resolved.strip_prefix(run_root)
                    .ok()
                    .map(|path| path.to_string_lossy().to_string()),
            }
        }),
    });
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use homeboy::core::engine::run_dir::RunDir;
    use homeboy::core::observation::{NewRunRecord, ObservationStore};
    use homeboy_extension::trace::parsing::{TraceArtifact, TraceResults, TraceStatus};

    fn sample_results(artifacts: Vec<TraceArtifact>) -> TraceResults {
        TraceResults {
            component_id: "homeboy".to_string(),
            scenario_id: "browser-probe".to_string(),
            status: TraceStatus::Pass,
            summary: None,
            failure: None,
            rig: None,
            evidence: None,
            timeline: Vec::new(),
            span_definitions: Vec::new(),
            span_results: Vec::new(),
            assertions: Vec::new(),
            temporal_assertions: Vec::new(),
            artifacts,
            metrics: Default::default(),
            toolchain: None,
            components: None,
            dependencies: Vec::new(),
            preview: None,
        }
    }

    #[test]
    fn declared_nested_artifact_directory_is_recorded_recursively() {
        crate::test_support::with_isolated_home(|_| {
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(
                    NewRunRecord::builder("trace")
                        .component_id("homeboy")
                        .build(),
                )
                .expect("run");
            let run_dir = RunDir::create().expect("run dir");
            std::fs::write(
                run_dir.step_file(homeboy::core::engine::run_dir::files::TRACE_RESULTS),
                "{}",
            )
            .expect("trace results");
            let browser_dir = run_dir
                .path()
                .join("artifacts/provider-artifacts/runtime-abc/files/browser");
            std::fs::create_dir_all(&browser_dir).expect("mkdir browser artifacts");
            std::fs::write(browser_dir.join("network.jsonl"), "{\"url\":\"/\"}\n")
                .expect("write network");
            std::fs::write(browser_dir.join("console.jsonl"), "{\"text\":\"ok\"}\n")
                .expect("write console");
            let mut results = sample_results(vec![TraceArtifact {
                label: "Provider browser probe".to_string(),
                path: "artifacts/provider-artifacts/runtime-abc/files/browser".to_string(),
                kind: None,
            }]);

            let outcome = record_trace_artifacts(&store, &run.id, &run_dir, Some(&results));
            outcome.rewrite_declared_artifact_paths(&mut results);
            let artifacts = store.list_artifacts(&run.id).expect("artifacts");
            let declared_artifact = artifacts
                .iter()
                .find(|artifact| {
                    artifact.kind == "trace-artifact" && artifact.artifact_type == "directory"
                })
                .expect("declared trace artifact");

            assert!(!outcome.has_declared_artifact_failures());
            assert!(outcome.evidence_promoted());
            let persisted = PathBuf::from(&declared_artifact.path);
            assert_eq!(results.artifacts[0].path, declared_artifact.path);
            let scratch_path = run_dir.path().to_path_buf();
            run_dir.finish(true);
            assert!(!scratch_path.exists());
            assert_eq!(
                std::fs::read_to_string(persisted.join("network.jsonl")).expect("network"),
                "{\"url\":\"/\"}\n"
            );
            assert_eq!(
                std::fs::read_to_string(persisted.join("console.jsonl")).expect("console"),
                "{\"text\":\"ok\"}\n"
            );
        });
    }

    #[test]
    fn missing_declared_artifact_records_structured_failure_finding() {
        crate::test_support::with_isolated_home(|_| {
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(
                    NewRunRecord::builder("trace")
                        .component_id("homeboy")
                        .build(),
                )
                .expect("run");
            let run_dir = RunDir::create().expect("run dir");
            let results = sample_results(vec![TraceArtifact {
                label: "network log".to_string(),
                path: "artifacts/provider-artifacts/runtime-missing/files/browser/network.jsonl"
                    .to_string(),
                kind: None,
            }]);

            let outcome = record_trace_artifacts(&store, &run.id, &run_dir, Some(&results));
            let findings = store.list_findings_for_run(&run.id).expect("findings");
            let missing_finding = findings
                .iter()
                .find(|finding| finding.rule.as_deref() == Some("trace.artifact.missing"))
                .expect("missing artifact finding");

            assert!(outcome.has_declared_artifact_failures());
            assert_eq!(outcome.missing_declared_artifacts, 1);
            assert_eq!(missing_finding.tool, "trace");
            assert_eq!(missing_finding.severity.as_deref(), Some("error"));
            assert_eq!(
                missing_finding.metadata_json["declared_artifact"]["label"],
                "network log"
            );
            assert_eq!(
                missing_finding.metadata_json["declared_artifact"]["path"],
                "artifacts/provider-artifacts/runtime-missing/files/browser/network.jsonl"
            );
            let scratch_path = run_dir.path().to_path_buf();
            run_dir.finish(false);
            assert!(scratch_path.exists());
        });
    }
}
