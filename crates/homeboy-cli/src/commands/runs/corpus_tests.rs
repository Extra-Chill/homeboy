//! Integration tests for the schema-blind corpus primitives
//! (bundle export/import and the gh-actions branch of `runs import`).
//!
//! These exercise the primitives end-to-end against a real `ObservationStore`
//! with on-disk JSON artifact files, imitating the corpus an ingestor would
//! produce: each run has one JSON artifact attached.

#![cfg(test)]

use homeboy::core::observation::{NewRunRecord, ObservationStore, RunRecord, RunStatus};
use homeboy::test_support::with_isolated_home;
use serde_json::Value;

use super::bundle::{RunsExportArgs, RunsImportArgs};
use super::{bundle::import_runs, RunsOutput};

/// The observation store the enclosing isolated home installs.
///
/// A test is the entry point for its own unit of work, so opening once here
/// is a boundary open, not an ambient one inside production code (#7505).
fn test_store() -> homeboy::core::observation::ObservationStore {
    homeboy::core::observation::ObservationStore::open_initialized().expect("observation store")
}

/// Restore `XDG_DATA_HOME` for the test scope so the observation store
/// resolves under the temporary home created by `with_isolated_home`.

fn sample_run(kind: &str, component_id: &str, rig_id: &str, metadata: Value) -> NewRunRecord {
    NewRunRecord::builder(kind)
        .component_id(component_id)
        .rig_id(rig_id)
        .metadata(metadata)
        .build()
}

fn install_artifact(
    store: &ObservationStore,
    home: &std::path::Path,
    run_kind: &str,
    component: &str,
    body: Value,
    artifact_kind: &str,
) -> RunRecord {
    let run = store
        .start_run(sample_run(run_kind, component, "corpus", Value::Null))
        .expect("start run");
    store
        .finish_run(&run.id, RunStatus::Pass, None)
        .expect("finish run");
    let path = home.join(format!("{}.json", run.id));
    std::fs::write(&path, body.to_string()).expect("write artifact");
    store
        .record_artifact(&run.id, artifact_kind, &path)
        .expect("record artifact");
    run
}

#[test]
fn import_from_gh_actions_requires_gh_specific_arguments() {
    with_isolated_home(|_home| {
        let _xdg = homeboy_core::test_support::EnvVarGuard::unset("XDG_DATA_HOME");
        // Without --component, --repo, --workflow/--run-id, --artifact-glob, the
        // gh-actions branch must reject with a missing-argument error.
        let err = import_runs(
            &test_store(),
            RunsImportArgs {
                input: None,
                from_gh_actions: true,
                ..RunsImportArgs::default()
            },
        )
        .err()
        .expect("must fail without required gh-actions args");
        assert_eq!(err.code.as_str(), "validation.missing_argument");
    });
}

#[test]
fn import_from_gh_actions_requires_workflow_or_run_id() {
    with_isolated_home(|_home| {
        let _xdg = homeboy_core::test_support::EnvVarGuard::unset("XDG_DATA_HOME");
        let err = import_runs(
            &test_store(),
            RunsImportArgs {
                input: None,
                from_gh_actions: true,
                component_id: Some("wp-site-generator".into()),
                repo: Some("example-org/wp-site-generator".into()),
                artifact_glob: Some("php-transformer-iterator-transcript-*".into()),
                ..RunsImportArgs::default()
            },
        )
        .err()
        .expect("must fail without workflow or run id");
        assert_eq!(err.code.as_str(), "validation.missing_argument");
    });
}

#[test]
fn bundle_import_restores_file_artifacts() {
    let bundle_dir = tempfile::tempdir().expect("bundle dir");
    let mut run_id = String::new();
    let mut source_home_path = String::new();

    with_isolated_home(|home| {
        let _xdg = homeboy_core::test_support::EnvVarGuard::unset("XDG_DATA_HOME");
        source_home_path = home.path().display().to_string();
        let store = ObservationStore::open_initialized().expect("store");
        let run = install_artifact(
            &store,
            home.path(),
            "observe",
            "homeboy",
            serde_json::json!({ "component_id": "homeboy" }),
            "trace-results",
        );
        run_id = run.id.clone();

        super::bundle::export_runs(
            &test_store(),
            RunsExportArgs {
                run: Some(run.id),
                since: None,
                output: bundle_dir.path().to_path_buf(),
            },
        )
        .expect("export bundle");
    });

    with_isolated_home(|_home| {
        let _xdg = homeboy_core::test_support::EnvVarGuard::unset("XDG_DATA_HOME");
        let (output, _) = import_runs(
            &test_store(),
            RunsImportArgs {
                input: Some(bundle_dir.path().to_path_buf()),
                ..RunsImportArgs::default()
            },
        )
        .expect("import bundle");
        let RunsOutput::Import(output) = output else {
            panic!("expected import output");
        };
        assert_eq!(output.imported.artifacts, 1);
        assert_eq!(output.imported.artifact_metadata_only, 0);

        let store = ObservationStore::open_initialized().expect("store");
        let artifacts = store.list_artifacts(&run_id).expect("artifacts");
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].artifact_type, "file");
        assert!(!artifacts[0].path.contains(&source_home_path));
        assert!(std::path::Path::new(&artifacts[0].path).is_absolute());
    });
}
