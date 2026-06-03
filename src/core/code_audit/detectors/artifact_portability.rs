use std::path::Path;

use crate::core::code_audit::conventions::AuditFinding;
use crate::core::code_audit::findings::{Finding, Severity};
use crate::core::component::ArtifactPortabilityConfig;
use crate::core::observation::{ArtifactRecord, ObservationStore, RunListFilter, RunRecord};
use serde_json::Value;

pub(crate) const DEFAULT_OBSERVATION_RUN_WINDOW: usize = 1000;

#[derive(Debug, Default)]
pub(crate) struct ArtifactPortabilityReport {
    pub(crate) findings: Vec<Finding>,
    pub(crate) runs_scanned: usize,
    pub(crate) artifacts_scanned: usize,
    pub(crate) metadata_fields_scanned: usize,
    pub(crate) run_window: usize,
}

#[cfg(test)]
pub(crate) fn run(component_id: &str) -> Vec<Finding> {
    run_with_config(component_id, &ArtifactPortabilityConfig::default())
}

#[cfg(test)]
pub(crate) fn run_with_config(
    component_id: &str,
    config: &ArtifactPortabilityConfig,
) -> Vec<Finding> {
    run_report_with_config(component_id, config).findings
}

pub(crate) fn run_report(component_id: &str) -> ArtifactPortabilityReport {
    run_report_with_config(component_id, &ArtifactPortabilityConfig::default())
}

pub(crate) fn run_report_with_config(
    component_id: &str,
    config: &ArtifactPortabilityConfig,
) -> ArtifactPortabilityReport {
    let run_window = config
        .observation_run_window
        .unwrap_or(DEFAULT_OBSERVATION_RUN_WINDOW)
        .clamp(1, 1000);
    let mut report = ArtifactPortabilityReport {
        run_window,
        ..Default::default()
    };
    let Ok(store) = ObservationStore::open_initialized() else {
        return report;
    };
    let Ok(runs) = store.list_runs(RunListFilter {
        kind: None,
        component_id: Some(component_id.to_string()),
        status: None,
        rig_id: None,
        limit: Some(run_window as i64),
    }) else {
        return report;
    };
    let artifact_root = crate::core::artifact_root().ok();
    let path_policy = config.with_generic_defaults();

    for run in runs {
        report.runs_scanned += 1;
        let Ok(artifacts) = store.list_artifacts(&run.id) else {
            continue;
        };
        report.artifacts_scanned += artifacts.len();
        let artifact_paths: Vec<String> = artifacts
            .iter()
            .map(|artifact| artifact.path.clone())
            .collect();
        for artifact in artifacts {
            if artifact.artifact_type != "file" && artifact.artifact_type != "directory" {
                continue;
            }
            if artifact_path_is_portable(&artifact.path, artifact_root.as_deref(), &path_policy) {
                continue;
            }
            report.findings.push(artifact_path_finding(&run, &artifact));
        }
        let metadata_scan = metadata_path_findings(
            &run,
            &artifact_paths,
            artifact_root.as_deref(),
            &path_policy,
        );
        report.metadata_fields_scanned += metadata_scan.fields_scanned;
        report.findings.extend(metadata_scan.findings);
    }

    report
        .findings
        .sort_by(|a, b| a.file.cmp(&b.file).then(a.description.cmp(&b.description)));
    report
}

fn artifact_path_is_portable(
    path: &str,
    artifact_root: Option<&Path>,
    config: &ArtifactPortabilityConfig,
) -> bool {
    if path.starts_with("runner-artifact://") || path.starts_with("metadata-only:") {
        return true;
    }
    let path_ref = Path::new(path);
    if let Some(root) = artifact_root {
        if path_ref.starts_with(root) {
            return true;
        }
    }
    !path_ref.is_absolute() && !looks_like_runtime_temp_path(path, config)
}

fn artifact_path_finding(run: &RunRecord, artifact: &ArtifactRecord) -> Finding {
    portability_finding(
        run,
        "artifact.path",
        &artifact.path,
        format!(
            "Artifact {} ({}) records non-portable path {}",
            artifact.id, artifact.kind, artifact.path
        ),
        "Record copied artifact-store paths or portable artifact tokens instead of runtime temp paths",
    )
}

fn metadata_path_findings(
    run: &RunRecord,
    artifact_paths: &[String],
    artifact_root: Option<&Path>,
    config: &ArtifactPortabilityConfig,
) -> MetadataPathScan {
    let cleanup_promised = metadata_promises_cleanup(&run.metadata_json);
    let mut scan = MetadataPathScan::default();
    for field in metadata_string_fields(&run.metadata_json) {
        scan.fields_scanned += 1;
        if is_runner_artifact_ref(field.value)
            && !artifact_paths.iter().any(|path| path == field.value)
        {
            scan.findings.push(portability_finding(
                run,
                &field.path,
                field.value,
                format!(
                    "Run metadata field {} records remote artifact ref {} without a mirrored artifact record",
                    field.path, field.value
                ),
                "Import the remote artifact into the observation store and reference its mirrored artifact record",
            ));
            continue;
        }

        if !field_expects_portable_artifact_ref(&field.path) {
            continue;
        }
        if !looks_like_local_absolute_path(field.value, artifact_root, config) {
            continue;
        }

        let mut description = format!(
            "Run metadata field {} records local-only artifact path {}",
            field.path, field.value
        );
        if cleanup_promised && Path::new(field.value).exists() {
            description.push_str(" that still exists after cleanup metadata promised cleanup");
        }
        scan.findings.push(portability_finding(
            run,
            &field.path,
            field.value,
            description,
            "Store the artifact under the configured artifact root and persist that path, or persist a runner-artifact:// token with a mirrored artifact row",
        ));
    }
    scan
}

#[derive(Default)]
struct MetadataPathScan {
    findings: Vec<Finding>,
    fields_scanned: usize,
}

fn portability_finding(
    run: &RunRecord,
    field: &str,
    observed_path: &str,
    description: String,
    suggestion: &str,
) -> Finding {
    let command = run.command.as_deref().unwrap_or("<unknown>");
    Finding {
        convention: "artifact_portability".to_string(),
        severity: Severity::Warning,
        file: format!("observation:{}", run.id),
        description: format!("{description}; command `{command}`; field `{field}`"),
        suggestion: format!(
            "{suggestion}. Suggested portable ref: artifact-store path under HOMEBOY_ARTIFACT_ROOT or runner-artifact://<runner>/<run>/<artifact> instead of {observed_path}"
        ),
        kind: AuditFinding::NonPortableArtifactPath,
    }
}

struct MetadataStringField<'a> {
    path: String,
    value: &'a str,
}

fn metadata_string_fields(value: &Value) -> Vec<MetadataStringField<'_>> {
    let mut fields = Vec::new();
    collect_metadata_string_fields(value, "$", &mut fields);
    fields
}

fn collect_metadata_string_fields<'a>(
    value: &'a Value,
    path: &str,
    fields: &mut Vec<MetadataStringField<'a>>,
) {
    match value {
        Value::String(value) => fields.push(MetadataStringField {
            path: path.to_string(),
            value,
        }),
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                collect_metadata_string_fields(item, &format!("{path}[{index}]"), fields);
            }
        }
        Value::Object(map) => {
            for (key, item) in map {
                collect_metadata_string_fields(item, &format!("{path}.{key}"), fields);
            }
        }
        _ => {}
    }
}

fn field_expects_portable_artifact_ref(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    [
        "artifact",
        "evidence",
        "import",
        "output_path",
        "local_output",
        "temp_dir",
        "run_dir",
        "run_path",
        "patch_artifact_path",
    ]
    .iter()
    .any(|marker| path.contains(marker))
}

fn looks_like_local_absolute_path(
    path: &str,
    artifact_root: Option<&Path>,
    _config: &ArtifactPortabilityConfig,
) -> bool {
    if is_runner_artifact_ref(path) || path.starts_with("metadata-only:") {
        return false;
    }
    let path_ref = Path::new(path);
    if !path_ref.is_absolute() {
        return false;
    }
    if let Some(root) = artifact_root {
        if path_ref.starts_with(root) {
            return false;
        }
    }
    true
}

fn is_runner_artifact_ref(path: &str) -> bool {
    path.starts_with("runner-artifact://")
}

fn metadata_promises_cleanup(value: &Value) -> bool {
    match value {
        Value::Bool(true) => false,
        Value::Array(items) => items.iter().any(metadata_promises_cleanup),
        Value::Object(map) => map.iter().any(|(key, value)| {
            key.to_ascii_lowercase().contains("cleanup") && cleanup_value_is_promising(value)
                || metadata_promises_cleanup(value)
        }),
        _ => false,
    }
}

fn cleanup_value_is_promising(value: &Value) -> bool {
    match value {
        Value::Bool(value) => *value,
        Value::String(value) => matches!(
            value.to_ascii_lowercase().as_str(),
            "true" | "done" | "cleaned" | "cleanup" | "completed" | "success" | "succeeded"
        ),
        Value::Object(_) | Value::Array(_) => metadata_promises_cleanup(value),
        _ => false,
    }
}

fn looks_like_runtime_temp_path(path: &str, config: &ArtifactPortabilityConfig) -> bool {
    config
        .non_portable_path_prefixes
        .iter()
        .any(|prefix| path.starts_with(prefix))
        || config
            .non_portable_path_contains
            .iter()
            .any(|marker| path.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::observation::{ArtifactRecord, NewRunRecord};
    use crate::test_support::with_isolated_home;

    #[test]
    fn flags_runtime_temp_artifact_paths() {
        with_isolated_home(|_| {
            let store = ObservationStore::open_initialized().expect("store");
            let cwd = std::env::current_dir().expect("cwd");
            let run_record = store
                .start_run(
                    NewRunRecord::builder("observe")
                        .component_id("demo")
                        .command("homeboy observe demo")
                        .cwd_path(&cwd)
                        .build(),
                )
                .expect("run");
            store
                .import_artifact(&ArtifactRecord {
                    id: "artifact-1".to_string(),
                    run_id: run_record.id,
                    kind: "trace-results".to_string(),
                    artifact_type: "file".to_string(),
                    path: "/tmp/homeboy-run-abc/trace.json".to_string(),
                    url: None,
                    sha256: None,
                    size_bytes: None,
                    mime: None,
                    created_at: chrono::Utc::now().to_rfc3339(),
                })
                .expect("artifact");

            let findings = super::run("demo");

            assert_eq!(findings.len(), 1);
            assert_eq!(findings[0].kind, AuditFinding::NonPortableArtifactPath);
            assert!(findings[0]
                .description
                .contains("/tmp/homeboy-run-abc/trace.json"));
            assert!(findings[0]
                .description
                .contains("command `homeboy observe demo`"));
            assert!(findings[0].description.contains("field `artifact.path`"));
            assert!(findings[0].suggestion.contains("Suggested portable ref"));
        });
    }

    #[test]
    fn flags_metadata_artifact_fields_with_local_absolute_paths() {
        with_isolated_home(|_| {
            let store = ObservationStore::open_initialized().expect("store");
            let cwd = std::env::current_dir().expect("cwd");
            let run_record = store
                .start_run(
                    NewRunRecord::builder("runner-exec")
                        .component_id("demo")
                        .command("homeboy test demo --runner lab")
                        .cwd_path(&cwd)
                        .metadata(serde_json::json!({
                            "lab": {
                                "result": {
                                    "local_output_path": "/Users/chris/project/out/test.json"
                                }
                            }
                        }))
                        .build(),
                )
                .expect("run");
            store
                .finish_run(
                    &run_record.id,
                    crate::core::observation::RunStatus::Pass,
                    None,
                )
                .expect("finish");

            let findings = super::run("demo");

            assert_eq!(findings.len(), 1);
            assert!(findings[0]
                .description
                .contains("$.lab.result.local_output_path"));
            assert!(findings[0]
                .description
                .contains("command `homeboy test demo --runner lab`"));
        });
    }

    #[test]
    fn flags_remote_artifact_refs_without_mirrored_artifact_rows() {
        with_isolated_home(|_| {
            let store = ObservationStore::open_initialized().expect("store");
            let cwd = std::env::current_dir().expect("cwd");
            let run_record = store
                .start_run(
                    NewRunRecord::builder("runner-exec")
                        .component_id("demo")
                        .command("homeboy runs bundle demo")
                        .cwd_path(&cwd)
                        .metadata(serde_json::json!({
                            "patch_artifact_path": "runner-artifact://lab/run-1/patch"
                        }))
                        .build(),
                )
                .expect("run");
            store
                .finish_run(
                    &run_record.id,
                    crate::core::observation::RunStatus::Pass,
                    None,
                )
                .expect("finish");

            let findings = super::run("demo");

            assert_eq!(findings.len(), 1);
            assert!(findings[0]
                .description
                .contains("without a mirrored artifact record"));
            assert!(findings[0]
                .suggestion
                .contains("runner-artifact://<runner>/<run>/<artifact>"));
        });
    }

    #[test]
    fn accepts_remote_artifact_refs_with_mirrored_artifact_rows() {
        with_isolated_home(|_| {
            let store = ObservationStore::open_initialized().expect("store");
            let cwd = std::env::current_dir().expect("cwd");
            let run_record = store
                .start_run(
                    NewRunRecord::builder("runner-exec")
                        .component_id("demo")
                        .command("homeboy runs bundle demo")
                        .cwd_path(&cwd)
                        .metadata(serde_json::json!({
                            "patch_artifact_path": "runner-artifact://lab/run-1/patch"
                        }))
                        .build(),
                )
                .expect("run");
            store
                .import_artifact(&ArtifactRecord {
                    id: "patch".to_string(),
                    run_id: run_record.id.clone(),
                    kind: "patch".to_string(),
                    artifact_type: "remote_file".to_string(),
                    path: "runner-artifact://lab/run-1/patch".to_string(),
                    url: None,
                    sha256: None,
                    size_bytes: None,
                    mime: None,
                    created_at: chrono::Utc::now().to_rfc3339(),
                })
                .expect("artifact");
            store
                .finish_run(
                    &run_record.id,
                    crate::core::observation::RunStatus::Pass,
                    None,
                )
                .expect("finish");

            let findings = super::run("demo");

            assert!(findings.is_empty());
        });
    }

    #[test]
    fn flags_cleanup_promised_temp_dirs_that_still_exist() {
        with_isolated_home(|home| {
            let temp_dir = home.path().join("homeboy-run-left-behind");
            std::fs::create_dir_all(&temp_dir).expect("temp dir");
            let store = ObservationStore::open_initialized().expect("store");
            let cwd = std::env::current_dir().expect("cwd");
            let run_record = store
                .start_run(
                    NewRunRecord::builder("observe")
                        .component_id("demo")
                        .command("homeboy observe demo")
                        .cwd_path(&cwd)
                        .metadata(serde_json::json!({
                            "cleanup": "completed",
                            "evidence": {
                                "run_dir": temp_dir.to_string_lossy()
                            }
                        }))
                        .build(),
                )
                .expect("run");
            store
                .finish_run(
                    &run_record.id,
                    crate::core::observation::RunStatus::Pass,
                    None,
                )
                .expect("finish");

            let findings = super::run_with_config(
                "demo",
                &ArtifactPortabilityConfig {
                    non_portable_path_contains: vec!["homeboy-run-".to_string()],
                    ..Default::default()
                },
            );

            assert_eq!(findings.len(), 1);
            assert!(findings[0]
                .description
                .contains("still exists after cleanup metadata promised cleanup"));
        });
    }

    #[test]
    fn accepts_artifact_store_paths() {
        with_isolated_home(|home| {
            let artifact_root = home.path().join("artifacts");
            crate::core::set_artifact_root_override(Some(artifact_root.clone()));

            assert!(artifact_path_is_portable(
                &artifact_root.join("run/artifact.json").to_string_lossy(),
                Some(&artifact_root),
                &ArtifactPortabilityConfig::default().with_generic_defaults()
            ));
        });
    }

    #[test]
    fn default_window_preserves_existing_thousand_run_coverage() {
        with_isolated_home(|_| {
            let store = ObservationStore::open_initialized().expect("store");
            let cwd = std::env::current_dir().expect("cwd");
            for index in 0..60 {
                let run_record = store
                    .start_run(
                        NewRunRecord::builder("observe")
                            .component_id("demo")
                            .command(format!("homeboy observe demo --run {index}"))
                            .cwd_path(&cwd)
                            .metadata(serde_json::json!({
                                "evidence": {
                                    "output_path": format!("/tmp/homeboy-run-{index}/trace.json")
                                }
                            }))
                            .build(),
                    )
                    .expect("run");
                store
                    .finish_run(
                        &run_record.id,
                        crate::core::observation::RunStatus::Pass,
                        None,
                    )
                    .expect("finish");
            }

            let report = super::run_report("demo");

            assert_eq!(report.run_window, DEFAULT_OBSERVATION_RUN_WINDOW);
            assert_eq!(report.runs_scanned, 60);
            assert_eq!(report.metadata_fields_scanned, 120);
            assert_eq!(report.findings.len(), 60);
            assert!(report
                .findings
                .iter()
                .any(|finding| finding.description.contains("homeboy-run-0")));
        });
    }

    #[test]
    fn explicit_run_window_bounds_scan_and_reports_counts() {
        with_isolated_home(|_| {
            let store = ObservationStore::open_initialized().expect("store");
            let cwd = std::env::current_dir().expect("cwd");
            for index in 0..5 {
                let run_record = store
                    .start_run(
                        NewRunRecord::builder("observe")
                            .component_id("demo")
                            .command(format!("homeboy observe demo --run {index}"))
                            .cwd_path(&cwd)
                            .metadata(serde_json::json!({
                                "evidence": {
                                    "output_path": format!("/tmp/homeboy-run-{index}/trace.json")
                                }
                            }))
                            .build(),
                    )
                    .expect("run");
                store
                    .finish_run(
                        &run_record.id,
                        crate::core::observation::RunStatus::Pass,
                        None,
                    )
                    .expect("finish");
            }

            let report = super::run_report_with_config(
                "demo",
                &ArtifactPortabilityConfig {
                    observation_run_window: Some(2),
                    ..Default::default()
                },
            );

            assert_eq!(report.run_window, 2);
            assert_eq!(report.runs_scanned, 2);
            assert_eq!(report.metadata_fields_scanned, 4);
            assert_eq!(report.findings.len(), 2);
            assert!(!report
                .findings
                .iter()
                .any(|finding| finding.description.contains("homeboy-run-0")));
        });
    }

    #[test]
    fn accepts_project_specific_path_markers_from_config() {
        let config = ArtifactPortabilityConfig {
            non_portable_path_contains: vec!["/project-run-".to_string()],
            ..Default::default()
        }
        .with_generic_defaults();

        assert!(!artifact_path_is_portable(
            "/workspace/project-run-abc/trace.json",
            None,
            &config
        ));
        assert!(!artifact_path_is_portable(
            "/workspace/other-run-abc/trace.json",
            None,
            &config
        ));
        assert!(artifact_path_is_portable(
            "artifacts/other-run-abc/trace.json",
            None,
            &config
        ));
    }

    #[test]
    fn homeboy_config_declares_homeboy_run_marker() {
        let raw = std::fs::read_to_string("homeboy.json").expect("homeboy config");
        let component: crate::core::component::Component =
            serde_json::from_str(&raw).expect("component config");
        let audit = component.audit.expect("audit config");

        assert!(audit
            .artifact_portability
            .non_portable_path_contains
            .contains(&"/homeboy-run-".to_string()));
    }
}
