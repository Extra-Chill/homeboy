use homeboy::core::observation::{NewRunRecord, ObservationStore, RunStatus};
use homeboy::test_support::with_isolated_home;
use serde_json::Value;

use super::{handlers, list_runs, RunsListArgs, RunsOutput};

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

fn sample_run(kind: &str, component_id: &str, rig_id: &str, metadata: Value) -> NewRunRecord {
    NewRunRecord::builder(kind)
        .component_id(component_id)
        .command(format!("homeboy {kind} {component_id}"))
        .cwd_path(std::path::Path::new("/tmp/homeboy-fixture"))
        .homeboy_version("test-version")
        .git_sha(Some("abc123".to_string()))
        .rig_id(rig_id)
        .metadata(metadata)
        .build()
}

#[test]
fn rig_runs_list_surfaces_compact_artifact_index() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let run = store
            .start_run(sample_run(
                "rig",
                "homeboy",
                "proof-rig",
                serde_json::json!({
                    "pipeline": {
                        "name": "check",
                        "steps": [{
                            "kind": "command",
                            "label": "produce proof report",
                            "status": "fail",
                            "error": "report failed"
                        }],
                        "passed": 0,
                        "failed": 1
                    }
                }),
            ))
            .expect("rig run");
        store
            .finish_run(&run.id, RunStatus::Fail, None)
            .expect("finish rig run");
        let report = home.path().join("proof-report.json");
        std::fs::write(&report, "{}").expect("report artifact");
        store
            .record_artifact(&run.id, "proof_report", &report)
            .expect("record report");

        let (output, _) = list_runs(
            RunsListArgs {
                runner: None,
                kind: None,
                component_id: None,
                rig: Some("proof-rig".to_string()),
                scenario_id: None,
                status: None,
                limit: 20,
                include_active_runner_jobs: false,
            },
            "rig.runs",
        )
        .expect("rig runs");

        let RunsOutput::List(output) = output else {
            panic!("expected list output");
        };
        assert_eq!(output.command, "rig.runs");
        assert_eq!(output.runs.len(), 1);
        let artifact_index = output.runs[0]
            .artifact_index
            .as_ref()
            .expect("artifact index");
        assert_eq!(artifact_index.run_id, run.id);
        assert_eq!(artifact_index.rig_id, "proof-rig");
        assert_eq!(artifact_index.status, "fail");
        assert!(artifact_index
            .artifact_index_path
            .ends_with("rig-artifact-index.json"));
        assert_eq!(
            artifact_index.evidence_commands.artifacts_command,
            format!("homeboy runs artifacts {}", run.id)
        );
        assert!(artifact_index
            .key_report_refs
            .iter()
            .any(|artifact| artifact.kind == "proof_report"));
        assert_eq!(artifact_index.failed_step_refs.len(), 1);
        assert_eq!(
            artifact_index.failed_step_refs[0].label,
            "produce proof report"
        );
    });
}

#[test]
fn runs_artifacts_surfaces_matrix_summary_from_typed_packets() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let run = store
            .start_run(sample_run(
                "fixture.matrix",
                "static-site-importer",
                "ssi-fixtures",
                serde_json::json!({ "schema": "example/matrix-run/v1" }),
            ))
            .expect("matrix run");
        store
            .finish_run(&run.id, RunStatus::Fail, None)
            .expect("finish matrix run");
        let summary = home.path().join("summary.json");
        std::fs::write(
            &summary,
            serde_json::to_vec(&serde_json::json!({
                "schema": "example/matrix-summary/v1",
                "fixture_count": 2,
                "finding_count": 3,
                "group_counts": { "markup": 2, "links": 1 },
                "findings": [
                    { "kind": "missing_block", "fixture": "home" },
                    { "kind": "missing_block", "fixture": "about" }
                ]
            }))
            .expect("summary json"),
        )
        .expect("write summary");
        let packets = home.path().join("finding-packets.json");
        std::fs::write(
            &packets,
            serde_json::to_vec(&serde_json::json!({
                "finding_packets": [
                    { "diagnostic_kind": "broken_link", "fixture_id": "home" }
                ]
            }))
            .expect("packet json"),
        )
        .expect("write packets");
        store
            .record_artifact(&run.id, "matrix_summary", &summary)
            .expect("record summary");
        store
            .record_artifact(&run.id, "finding_packets", &packets)
            .expect("record packets");

        let (output, _) = handlers::artifacts(&run.id).expect("artifacts");
        let RunsOutput::Artifacts(output) = output else {
            panic!("expected artifacts output");
        };
        let summary = output.matrix_summary.expect("matrix summary");

        assert_eq!(summary.fixture_count, 2);
        assert_eq!(summary.finding_count, 3);
        assert_eq!(summary.group_counts[0].key, "markup");
        assert_eq!(summary.top_diagnostic_kinds[0].key, "missing_block");
        assert_eq!(summary.top_fixtures[0].key, "home");
        assert_eq!(summary.result_refs[0].kind, "matrix_summary");
        assert_eq!(summary.finding_packet_refs[0].kind, "finding_packets");
    });
}

#[test]
fn runs_artifacts_surfaces_static_html_preview_entrypoints() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let run = store
            .start_run(sample_run(
                "runner-exec",
                "generic-site-generator",
                "html-artifacts",
                serde_json::json!({ "schema": "example/run/v1" }),
            ))
            .expect("run");
        store
            .finish_run(&run.id, RunStatus::Pass, None)
            .expect("finish run");
        let site = home.path().join("site-output");
        std::fs::create_dir_all(site.join("case-a")).expect("site dirs");
        std::fs::write(site.join("index.html"), b"<html>Home</html>").expect("index");
        std::fs::write(site.join("case-a/index.html"), b"<html>Case</html>").expect("case");
        store
            .record_directory_artifact_with_metadata(
                &run.id,
                "generated_site",
                &site,
                serde_json::json!({
                    "role": "static_site_artifact",
                    "entrypoints": [{
                        "path": "index.html",
                        "label": "Open generated homepage",
                        "mime_type": "text/html"
                    }]
                }),
            )
            .expect("record directory");

        let (output, _) = handlers::artifacts(&run.id).expect("artifacts");
        let RunsOutput::Artifacts(output) = output else {
            panic!("expected artifacts output");
        };

        assert_eq!(output.preview_entrypoints.len(), 2);
        assert_eq!(output.preview_entrypoints[0].path, "index.html");
        assert_eq!(
            output.preview_entrypoints[0].label,
            "Open generated homepage"
        );
        assert_eq!(output.preview_entrypoints[0].mime_type, "text/html");
        assert!(output
            .preview_entrypoints
            .iter()
            .any(|entrypoint| entrypoint.path == "case-a/index.html"));
        assert_eq!(output.preview_entrypoints[0].public_url, None);
    });
}
