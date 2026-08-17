use std::path::{Path, PathBuf};

use crate::conventions::AuditFinding;
use crate::findings::{Finding, Severity};
use homeboy_audit_contract::ArtifactPortabilityConfig;
use homeboy_engine_primitives::artifact_ref_scheme::{
    is_metadata_only_ref, is_runner_artifact_ref,
};
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
    run_report_with_config(component_id, config, None).findings
}

pub(crate) fn run_report(
    component_id: &str,
    artifact_root: Option<&Path>,
) -> ArtifactPortabilityReport {
    run_report_with_config(
        component_id,
        &ArtifactPortabilityConfig::default(),
        artifact_root,
    )
}

/// Scan a component's recorded runs for non-portable artifact paths.
///
/// `artifact_root` is the caller's injected artifact root. Pass `Some` ONLY
/// when the recorded-artifact provider was registered against the same roots
/// (`audit_artifact_provider::register_in_roots`); an injected root paired with
/// an ambiently-registered provider compares one home's paths against another
/// home's root and flags every stored artifact as non-portable (#7505).
///
/// `None` — the current behavior at every call site — takes the root from the
/// provider that supplied the runs, so the two can never disagree. It is NOT
/// "resolve it ambiently": this detector no longer reads process-global path
/// state at all.
///
/// If neither the caller nor the provider can name a root, the scan DEGRADES:
/// root-anchored paths simply lose their exemption and the remaining
/// heuristics (relative, runner-artifact ref, runtime-temp markers) decide. It
/// never becomes an error.
pub(crate) fn run_report_with_config(
    component_id: &str,
    config: &ArtifactPortabilityConfig,
    artifact_root: Option<&Path>,
) -> ArtifactPortabilityReport {
    let run_window = config
        .observation_run_window
        .unwrap_or(DEFAULT_OBSERVATION_RUN_WINDOW)
        .clamp(1, 1000);
    let mut report = ArtifactPortabilityReport {
        run_window,
        ..Default::default()
    };
    let scan = crate::recorded_artifacts::recent_recorded_run_scan(component_id, run_window);
    let artifact_root: Option<PathBuf> =
        artifact_root.map(Path::to_path_buf).or(scan.artifact_root);
    let path_policy = config.with_generic_defaults();

    for run in scan.runs {
        report.runs_scanned += 1;
        let artifacts = &run.artifacts;
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
            report.findings.push(artifact_path_finding(&run, artifact));
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
    if is_runner_artifact_ref(path) || is_metadata_only_ref(path) {
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

fn artifact_path_finding(
    run: &crate::recorded_artifacts::AuditRecordedRun,
    artifact: &crate::recorded_artifacts::AuditRecordedArtifact,
) -> Finding {
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
    run: &crate::recorded_artifacts::AuditRecordedRun,
    artifact_paths: &[String],
    artifact_root: Option<&Path>,
    config: &ArtifactPortabilityConfig,
) -> MetadataPathScan {
    let cleanup_promised = metadata_promises_cleanup(&run.metadata_json);
    let mut scan = MetadataPathScan::default();
    for field in metadata_string_fields(&run.metadata_json) {
        if is_runner_artifact_ref(field.value)
            && !artifact_paths.iter().any(|path| path == field.value)
        {
            scan.fields_scanned += 1;
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

        if !field_expects_portable_artifact_ref(&field.path)
            || field_is_remote_artifact_manifest_path(&field.path)
        {
            continue;
        }
        scan.fields_scanned += 1;
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
    run: &crate::recorded_artifacts::AuditRecordedRun,
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
            line: None,
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
        "run_path",
        "patch_artifact_path",
    ]
    .iter()
    .any(|marker| path.contains(marker))
}

fn field_is_remote_artifact_manifest_path(path: &str) -> bool {
    path.starts_with("$.lab.remote_artifact_manifest[") && path.ends_with("].path")
}

fn looks_like_local_absolute_path(
    path: &str,
    artifact_root: Option<&Path>,
    _config: &ArtifactPortabilityConfig,
) -> bool {
    if is_runner_artifact_ref(path) || is_metadata_only_ref(path) {
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
    use crate::recorded_artifacts::{
        register_audit_recorded_artifact_provider, AuditRecordedArtifact,
        AuditRecordedArtifactProvider, AuditRecordedRun,
    };
    use homeboy_core::observation::{
        ArtifactRecord, NewRunRecord, ObservationStore, RunListFilter,
    };
    use homeboy_core::test_support::with_isolated_home;
    use std::path::{Path, PathBuf};

    /// One synthetic recorded run holding `artifact_path`, but only for
    /// component `demo`.
    ///
    /// The component filter is not decoration. A registered provider outlives
    /// the test that installed it — there is no unregister — so an unfiltered
    /// provider would feed a fabricated run to every later test in this binary
    /// that audits any component. Scoping to `demo` makes these providers
    /// indistinguishable from the no-op for everyone else.
    fn synthetic_runs(component_id: &str, artifact_path: &str) -> Vec<AuditRecordedRun> {
        if component_id != "demo" {
            return Vec::new();
        }
        vec![AuditRecordedRun {
            id: "run-1".to_string(),
            command: Some("homeboy test demo".to_string()),
            metadata_json: serde_json::json!({}),
            artifacts: vec![AuditRecordedArtifact {
                id: "artifact-1".to_string(),
                kind: "results".to_string(),
                artifact_type: "file".to_string(),
                path: artifact_path.to_string(),
            }],
        }]
    }

    /// Provider that reports a fixed artifact root and one recorded run whose
    /// artifact sits under it. No store, no ambient state — which is the point:
    /// it isolates "where does the detector get its root from?" (#7505).
    struct RootedProvider {
        artifact_root: PathBuf,
        artifact_path: String,
    }

    impl RootedProvider {
        fn under_root(artifact_root: PathBuf) -> Self {
            let artifact_path = artifact_root
                .join("run-1/results.json")
                .to_string_lossy()
                .into_owned();
            Self {
                artifact_root,
                artifact_path,
            }
        }
    }

    impl AuditRecordedArtifactProvider for RootedProvider {
        fn recent_runs(&self, component_id: &str, _limit: usize) -> Vec<AuditRecordedRun> {
            synthetic_runs(component_id, &self.artifact_path)
        }

        fn artifact_root(&self) -> Option<PathBuf> {
            Some(self.artifact_root.clone())
        }
    }

    /// Same runs, but the root is unresolvable. Exercises the degrade path.
    struct RootlessProvider {
        artifact_path: String,
    }

    impl AuditRecordedArtifactProvider for RootlessProvider {
        fn recent_runs(&self, component_id: &str, _limit: usize) -> Vec<AuditRecordedRun> {
            synthetic_runs(component_id, &self.artifact_path)
        }

        fn artifact_root(&self) -> Option<PathBuf> {
            None
        }
    }

    /// With no injected root the detector anchors on the root the PROVIDER
    /// reports, so an artifact under an injected home reads as portable even
    /// though it is nowhere near the ambient artifact root. Before #7505 this
    /// compared against `homeboy_paths::artifact_root()` and flagged it.
    ///
    /// Wrapped in `with_isolated_home` purely for the serialization it provides:
    /// the recorded-artifact registry is process-global, so two tests
    /// registering different providers concurrently would stomp each other.
    #[test]
    fn anchors_on_the_root_reported_by_the_provider() {
        with_isolated_home(|_| {
            let artifact_root = PathBuf::from("/injected-home/artifacts");
            register_audit_recorded_artifact_provider(Box::new(RootedProvider::under_root(
                artifact_root.clone(),
            )));

            assert!(super::run_report("demo", None).findings.is_empty());
        });
    }

    /// Injecting the same root the provider is rooted on agrees with it. This
    /// is the supported way to hand `artifact_portability` an injected root.
    #[test]
    fn injected_root_matching_the_provider_agrees_with_it() {
        with_isolated_home(|_| {
            let artifact_root = PathBuf::from("/injected-home/artifacts");
            register_audit_recorded_artifact_provider(Box::new(RootedProvider::under_root(
                artifact_root.clone(),
            )));

            assert!(super::run_report("demo", Some(artifact_root.as_path()))
                .findings
                .is_empty());
        });
    }

    /// Injecting a root the provider does not share is the misuse the docs warn
    /// about, and it still reads every stored artifact as non-portable. Pinned
    /// so the failure mode stays visible rather than becoming folklore: the fix
    /// is to register the provider `_in_roots`, not to change this detector.
    #[test]
    fn injected_root_disagreeing_with_the_provider_flags_everything() {
        with_isolated_home(|_| {
            register_audit_recorded_artifact_provider(Box::new(RootedProvider::under_root(
                PathBuf::from("/injected-home/artifacts"),
            )));

            let report = super::run_report("demo", Some(Path::new("/other-home/artifacts")));

            assert_eq!(report.findings.len(), 1);
            assert_eq!(
                report.findings[0].kind,
                AuditFinding::NonPortableArtifactPath
            );
        });
    }

    /// An unresolvable artifact root DEGRADES the scan: the root-anchored
    /// exemption is simply unavailable and the remaining heuristics decide. It
    /// is never an error, and the report is still produced.
    #[test]
    fn unresolvable_artifact_root_degrades_instead_of_failing() {
        with_isolated_home(|_| {
            register_audit_recorded_artifact_provider(Box::new(RootlessProvider {
                artifact_path: "/injected-home/artifacts/run-1/results.json".to_string(),
            }));
            let absolute = super::run_report("demo", None);
            assert_eq!(absolute.runs_scanned, 1);
            assert_eq!(absolute.findings.len(), 1);

            register_audit_recorded_artifact_provider(Box::new(RootlessProvider {
                artifact_path: "artifacts/run-1/results.json".to_string(),
            }));
            let relative = super::run_report("demo", None);
            assert_eq!(relative.runs_scanned, 1);
            assert!(relative.findings.is_empty());
        });
    }

    /// Store-backed recorded-artifact provider registered directly into the audit
    /// crate's own registry, so these in-crate tests read from the same static
    /// instance they register into. The dev-dependency cycle (this crate dev-deps
    /// homeboy-core, which deps this crate) makes Cargo compile the audit crate
    /// twice, so `homeboy_core::observation::audit_artifact_provider::register()`
    /// would write to a different instance than the detector reads. This mirrors
    /// that provider's `StoreArtifactProvider`.
    struct StoreArtifactProvider;

    impl AuditRecordedArtifactProvider for StoreArtifactProvider {
        fn recent_runs(&self, component_id: &str, limit: usize) -> Vec<AuditRecordedRun> {
            let Ok(store) = ObservationStore::open_initialized() else {
                return Vec::new();
            };
            let Ok(runs) = store.list_runs(RunListFilter {
                kind: None,
                component_id: Some(component_id.to_string()),
                status: None,
                rig_id: None,
                limit: Some(limit as i64),
                ..RunListFilter::default()
            }) else {
                return Vec::new();
            };
            runs.into_iter()
                .map(|run| {
                    let artifacts = store
                        .list_artifacts(&run.id)
                        .unwrap_or_default()
                        .into_iter()
                        .map(|artifact| AuditRecordedArtifact {
                            id: artifact.id,
                            kind: artifact.kind,
                            artifact_type: artifact.artifact_type,
                            path: artifact.path,
                        })
                        .collect();
                    AuditRecordedRun {
                        id: run.id,
                        command: run.command,
                        metadata_json: run.metadata_json,
                        artifacts,
                    }
                })
                .collect()
        }

        /// This provider opens the store ambiently, so its root is the ambient
        /// one — the same root the runs it just returned were written under.
        fn artifact_root(&self) -> Option<PathBuf> {
            homeboy_paths::artifact_root().ok()
        }
    }

    fn register_store_artifact_provider() {
        register_audit_recorded_artifact_provider(Box::new(StoreArtifactProvider));
    }

    #[test]
    fn flags_runtime_temp_artifact_paths() {
        with_isolated_home(|_| {
            let store = ObservationStore::open_initialized().expect("store");
            let cwd = std::env::current_dir().expect("cwd");
            let run_record = store
                .start_run(
                    NewRunRecord::builder("observe")
                        .component_id("demo")
                        .command("homeboy trace observe demo")
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
                    public_url: None,
                    viewer_url: None,
                    viewer_links: Vec::new(),
                    sha256: None,
                    size_bytes: None,
                    mime: None,
                    metadata_json: serde_json::json!({}),
                    created_at: chrono::Utc::now().to_rfc3339(),
                })
                .expect("artifact");

            register_store_artifact_provider();
            let findings = super::run("demo");

            assert_eq!(findings.len(), 1);
            assert_eq!(findings[0].kind, AuditFinding::NonPortableArtifactPath);
            assert!(findings[0]
                .description
                .contains("/tmp/homeboy-run-abc/trace.json"));
            assert!(findings[0]
                .description
                .contains("command `homeboy trace observe demo`"));
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
                    homeboy_core::observation::RunStatus::Pass,
                    None,
                )
                .expect("finish");

            register_store_artifact_provider();
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
                    homeboy_core::observation::RunStatus::Pass,
                    None,
                )
                .expect("finish");

            register_store_artifact_provider();
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
                    public_url: None,
                    viewer_url: None,
                    viewer_links: Vec::new(),
                    sha256: None,
                    size_bytes: None,
                    mime: None,
                    metadata_json: serde_json::json!({}),
                    created_at: chrono::Utc::now().to_rfc3339(),
                })
                .expect("artifact");
            store
                .finish_run(
                    &run_record.id,
                    homeboy_core::observation::RunStatus::Pass,
                    None,
                )
                .expect("finish");

            register_store_artifact_provider();
            let findings = super::run("demo");

            assert!(findings.is_empty());
        });
    }

    #[test]
    fn accepts_lab_remote_artifact_manifest_paths_as_remote_payload() {
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
                                "remote_artifact_manifest": [
                                    {
                                        "kind": "summary",
                                        "path": "/home/user/.local/share/homeboy/artifacts/run/summary.json"
                                    }
                                ]
                            }
                        }))
                        .build(),
                )
                .expect("run");
            store
                .finish_run(
                    &run_record.id,
                    homeboy_core::observation::RunStatus::Pass,
                    None,
                )
                .expect("finish");

            register_store_artifact_provider();
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
                        .command("homeboy trace observe demo")
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
                    homeboy_core::observation::RunStatus::Pass,
                    None,
                )
                .expect("finish");

            register_store_artifact_provider();
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
    fn accepts_bare_run_dir_as_internal_scratch_metadata() {
        with_isolated_home(|home| {
            let store = ObservationStore::open_initialized().expect("store");
            let cwd = std::env::current_dir().expect("cwd");
            let run_record = store
                .start_run(
                    NewRunRecord::builder("test")
                        .component_id("demo")
                        .command("homeboy test demo")
                        .cwd_path(&cwd)
                        .metadata(serde_json::json!({
                            "run_dir": home.path().join("homeboy-run-scratch").to_string_lossy()
                        }))
                        .build(),
                )
                .expect("run");
            store
                .finish_run(
                    &run_record.id,
                    homeboy_core::observation::RunStatus::Pass,
                    None,
                )
                .expect("finish");

            register_store_artifact_provider();
            let findings = super::run_with_config(
                "demo",
                &ArtifactPortabilityConfig {
                    non_portable_path_contains: vec!["homeboy-run-".to_string()],
                    ..Default::default()
                },
            );

            assert!(findings.is_empty());
        });
    }

    #[test]
    fn accepts_artifact_store_paths() {
        with_isolated_home(|home| {
            let artifact_root = home.path().join("artifacts");
            homeboy_paths::set_artifact_root_override(Some(artifact_root.clone()));

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
                            .command(format!("homeboy trace observe demo --run {index}"))
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
                        homeboy_core::observation::RunStatus::Pass,
                        None,
                    )
                    .expect("finish");
            }

            register_store_artifact_provider();
            let report = super::run_report("demo", None);

            assert_eq!(report.run_window, DEFAULT_OBSERVATION_RUN_WINDOW);
            assert_eq!(report.runs_scanned, 60);
            assert_eq!(report.metadata_fields_scanned, 60);
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
                            .command(format!("homeboy trace observe demo --run {index}"))
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
                        homeboy_core::observation::RunStatus::Pass,
                        None,
                    )
                    .expect("finish");
            }

            register_store_artifact_provider();
            let report = super::run_report_with_config(
                "demo",
                &ArtifactPortabilityConfig {
                    observation_run_window: Some(2),
                    ..Default::default()
                },
                None,
            );

            assert_eq!(report.run_window, 2);
            assert_eq!(report.runs_scanned, 2);
            assert_eq!(report.metadata_fields_scanned, 2);
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
        // homeboy.json lives at the repository root; this crate builds two levels
        // down (crates/homeboy-core), so resolve it relative to the manifest dir
        // rather than the (per-crate) test working directory.
        let config_path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../homeboy.json");
        let raw = std::fs::read_to_string(&config_path).expect("homeboy config");
        let component: homeboy_core::component::Component =
            serde_json::from_str(&raw).expect("component config");
        let audit = component.audit.expect("audit config");

        assert!(audit
            .artifact_portability
            .non_portable_path_contains
            .contains(&"/homeboy-run-".to_string()));
    }
}
