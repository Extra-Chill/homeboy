//! Integration tests for the schema-blind corpus primitives
//! (`runs query`, `runs drift`, and the gh-actions branch of `runs import`).
//!
//! These exercise the primitives end-to-end against a real `ObservationStore`
//! with on-disk JSON artifact files, imitating the corpus an ingestor would
//! produce: each run has one JSON artifact attached.

#![cfg(test)]

use homeboy::core::observation::{NewRunRecord, ObservationStore, RunRecord, RunStatus};
use homeboy::test_support::with_isolated_home;

use super::bundle::{RunsExportArgs, RunsImportArgs};
use super::{bundle::import_runs, drift, query, RunsOutput};
use serde_json::Value;

/// Restore `XDG_DATA_HOME` for the test scope so the observation store
/// resolves under the temporary home created by `with_isolated_home`.
struct XdgGuard(Option<String>);

impl XdgGuard {
    fn unset() -> Self {
        let prior = std::env::var("XDG_DATA_HOME").ok();
        std::env::remove_var("XDG_DATA_HOME");
        Self(prior)
    }
}

impl Drop for XdgGuard {
    fn drop(&mut self) {
        match &self.0 {
            Some(value) => std::env::set_var("XDG_DATA_HOME", value),
            None => std::env::remove_var("XDG_DATA_HOME"),
        }
    }
}

fn install_artifact(
    store: &ObservationStore,
    home: &std::path::Path,
    run_kind: &str,
    component: &str,
    body: serde_json::Value,
    artifact_kind: &str,
) -> RunRecord {
    let run = store
        .start_run(
            NewRunRecord::builder(run_kind)
                .component_id(component)
                .build(),
        )
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
fn runs_query_projects_select_jsonpath_over_artifact_corpus() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        install_artifact(
            &store,
            home.path(),
            "gh-actions",
            "wc-site-generator",
            serde_json::json!({ "theme": "noir", "fonts": ["serif", "mono"] }),
            "design-distribution",
        );
        install_artifact(
            &store,
            home.path(),
            "gh-actions",
            "wc-site-generator",
            serde_json::json!({ "theme": "vivid", "fonts": ["sans"] }),
            "design-distribution",
        );

        let (output, _) = query::runs_query(query::RunsQueryArgs {
            component_id: Some("wc-site-generator".into()),
            kind: Some("gh-actions".into()),
            since: None,
            select: vec!["$.theme".into()],
            group_by: None,
            count: false,
            format: query::QueryFormat::Json,
            limit: 200,
        })
        .expect("query");

        let RunsOutput::Query(output) = output else {
            panic!("expected query output");
        };
        assert_eq!(output.matched_artifact_count, 2);
        assert_eq!(output.rows.len(), 2);
        let themes: std::collections::HashSet<_> = output
            .rows
            .iter()
            .map(|row| row.values[0].as_str().unwrap().to_string())
            .collect();
        assert!(themes.contains("noir"));
        assert!(themes.contains("vivid"));
    });
}

#[test]
fn runs_query_groups_by_jsonpath_with_count() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        for theme in ["noir", "noir", "vivid"] {
            install_artifact(
                &store,
                home.path(),
                "gh-actions",
                "wc-site-generator",
                serde_json::json!({ "theme": theme }),
                "design-distribution",
            );
        }

        let (output, _) = query::runs_query(query::RunsQueryArgs {
            component_id: Some("wc-site-generator".into()),
            kind: Some("gh-actions".into()),
            since: None,
            select: vec!["$.theme".into()],
            group_by: Some("$.theme".into()),
            count: true,
            format: query::QueryFormat::Json,
            limit: 200,
        })
        .expect("query");

        let RunsOutput::Query(output) = output else {
            panic!("expected query output");
        };
        assert_eq!(output.matched_artifact_count, 3);
        assert_eq!(output.groups.len(), 2);
        assert_eq!(output.groups[0].group, "noir");
        assert_eq!(output.groups[0].count, 2);
        assert_eq!(output.groups[1].group, "vivid");
        assert_eq!(output.groups[1].count, 1);
    });
}

#[test]
fn runs_drift_reports_dominant_value_above_threshold() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        for theme in ["noir", "noir", "noir", "vivid"] {
            install_artifact(
                &store,
                home.path(),
                "gh-actions",
                "wc-site-generator",
                serde_json::json!({ "theme": theme }),
                "design-distribution",
            );
        }

        let (output, _) = drift::runs_drift(drift::RunsDriftArgs {
            component_id: Some("wc-site-generator".into()),
            kind: Some("gh-actions".into()),
            metric: "$.theme".into(),
            window: "30d".into(),
            threshold: 0.6,
            baseline: None,
            format: drift::DriftFormat::Json,
        })
        .expect("drift");

        let RunsOutput::Drift(output) = output else {
            panic!("expected drift output");
        };
        assert_eq!(output.window_observations, 4);
        let noir = output
            .values
            .iter()
            .find(|v| v.value == "noir")
            .expect("noir present");
        assert!(noir.dominant);
        let vivid = output
            .values
            .iter()
            .find(|v| v.value == "vivid")
            .expect("vivid present");
        assert!(!vivid.dominant);
    });
}

#[test]
fn import_from_gh_actions_requires_gh_specific_arguments() {
    with_isolated_home(|_home| {
        let _xdg = XdgGuard::unset();
        // Without --component, --repo, --workflow/--run-id, --artifact-glob, the
        // gh-actions branch must reject with a missing-argument error.
        let err = import_runs(RunsImportArgs {
            input: None,
            from_gh_actions: true,
            ..RunsImportArgs::default()
        })
        .err()
        .expect("must fail without required gh-actions args");
        assert_eq!(err.code.as_str(), "validation.missing_argument");
    });
}

#[test]
fn import_from_gh_actions_requires_workflow_or_run_id() {
    with_isolated_home(|_home| {
        let _xdg = XdgGuard::unset();
        let err = import_runs(RunsImportArgs {
            input: None,
            from_gh_actions: true,
            component_id: Some("wp-site-generator".into()),
            repo: Some("chubes4/wp-site-generator".into()),
            artifact_glob: Some("php-transformer-iterator-transcript-*".into()),
            ..RunsImportArgs::default()
        })
        .err()
        .expect("must fail without workflow or run id");
        assert_eq!(err.code.as_str(), "validation.missing_argument");
    });
}

#[test]
fn bundle_import_marks_file_artifacts_metadata_only_and_query_reports_skip() {
    let bundle_dir = tempfile::tempdir().expect("bundle dir");
    let mut run_id = String::new();
    let mut source_home_path = String::new();

    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
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

        super::bundle::export_runs(RunsExportArgs {
            run: Some(run.id),
            since: None,
            output: bundle_dir.path().to_path_buf(),
        })
        .expect("export bundle");
    });

    with_isolated_home(|_home| {
        let _xdg = XdgGuard::unset();
        let (output, _) = import_runs(RunsImportArgs {
            input: Some(bundle_dir.path().to_path_buf()),
            ..RunsImportArgs::default()
        })
        .expect("import bundle");
        let RunsOutput::Import(output) = output else {
            panic!("expected import output");
        };
        assert_eq!(output.imported.artifacts, 0);
        assert_eq!(output.imported.artifact_metadata_only, 1);

        let store = ObservationStore::open_initialized().expect("store");
        let artifacts = store.list_artifacts(&run_id).expect("artifacts");
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].artifact_type, "metadata-only");
        assert!(!artifacts[0].path.contains(&source_home_path));
        assert!(!std::path::Path::new(&artifacts[0].path).is_absolute());

        let (output, _) = query::runs_query(query::RunsQueryArgs {
            component_id: Some("homeboy".into()),
            kind: Some("observe".into()),
            since: None,
            select: vec!["$.component_id".into()],
            group_by: None,
            count: false,
            format: query::QueryFormat::Json,
            limit: 200,
        })
        .expect("query");
        let RunsOutput::Query(output) = output else {
            panic!("expected query output");
        };
        assert_eq!(output.matched_artifact_count, 0);
        assert_eq!(output.skipped_artifact_count, 1);
        assert_eq!(output.skipped_artifacts[0].artifact_type, "metadata-only");
        assert!(output.skipped_artifacts[0]
            .reason
            .contains("artifact bytes are not available"));

        let err = super::artifact_get(super::RunsArtifactGetArgs {
            run_id,
            artifact_id: artifacts[0].id.clone(),
            output: None,
        })
        .err()
        .expect("metadata-only artifact get must fail");
        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("metadata only"));
    });
}

#[test]
fn runs_query_rejects_invalid_jsonpath() {
    with_isolated_home(|_home| {
        let _xdg = XdgGuard::unset();
        let err = query::runs_query(query::RunsQueryArgs {
            component_id: None,
            kind: None,
            since: None,
            select: vec!["definitely not a jsonpath".into()],
            group_by: None,
            count: false,
            format: query::QueryFormat::Json,
            limit: 10,
        })
        .err()
        .expect("must reject invalid jsonpath");
        assert_eq!(err.code.as_str(), "validation.invalid_argument");
    });
}

#[test]
fn runs_drift_rejects_threshold_outside_unit_interval() {
    with_isolated_home(|_home| {
        let _xdg = XdgGuard::unset();
        let err = drift::runs_drift(drift::RunsDriftArgs {
            component_id: None,
            kind: None,
            metric: "$.theme".into(),
            window: "7d".into(),
            threshold: 1.5,
            baseline: None,
            format: drift::DriftFormat::Json,
        })
        .err()
        .expect("must reject threshold > 1");
        assert_eq!(err.code.as_str(), "validation.invalid_argument");
    });
}
