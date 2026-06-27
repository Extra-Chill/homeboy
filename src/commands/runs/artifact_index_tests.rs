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
        assert_eq!(
            output.path_guide.listing_source,
            "operator_local_persisted_store"
        );
        assert!(output
            .path_guide
            .operator_local_path_fields
            .iter()
            .any(|field| field.contains("artifacts[].path")));
        assert!(output
            .path_guide
            .runner_path_scope
            .contains("not operator-local filesystem paths"));
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
fn runs_artifacts_classifies_persisted_bench_artifact_from_metadata() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let run = store
            .start_run(sample_run(
                "bench",
                "homeboy",
                "matrix-rig",
                serde_json::json!({ "scenario_id": "fixture-matrix" }),
            ))
            .expect("matrix run");
        store
            .finish_run(&run.id, RunStatus::Pass, None)
            .expect("finish matrix run");
        let packets = home.path().join("finding-packets.json");
        std::fs::write(
            &packets,
            serde_json::to_vec(&serde_json::json!({
                "finding_packets": [
                    { "diagnostic_kind": "missing_block", "fixture_id": "home" }
                ]
            }))
            .expect("packet json"),
        )
        .expect("write packets");
        store
            .record_artifact_with_metadata(
                &run.id,
                "bench_artifact",
                &packets,
                serde_json::json!({
                    "source": "bench",
                    "name": "finding_packets",
                    "url": "https://homeboy-artifacts.example.test/finding-packets.json"
                }),
            )
            .expect("record packets");

        let (output, _) = handlers::artifacts(&run.id).expect("artifacts");
        let RunsOutput::Artifacts(output) = output else {
            panic!("expected artifacts output");
        };
        let summary = output.matrix_summary.expect("matrix summary");

        assert_eq!(summary.finding_count, 1);
        assert_eq!(summary.finding_packet_refs.len(), 1);
        assert!(summary.parse_diagnostics.is_empty());
    });
}

#[test]
fn runs_artifacts_recognizes_canonical_fuzz_result_envelope() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let run = store
            .start_run(sample_run(
                "fuzz",
                "homeboy",
                "studio",
                serde_json::json!({ "schema": "example/fuzz-run/v1" }),
            ))
            .expect("fuzz run");
        store
            .finish_run(&run.id, RunStatus::Pass, None)
            .expect("finish fuzz run");
        let envelope = home.path().join("runner-output.json");
        std::fs::write(
            &envelope,
            serde_json::to_vec(&serde_json::json!({
                "schema": "homeboy/fuzz-result-envelope/v1",
                "version": 1,
                "id": "envelope-1",
                "status": "passed",
                "request": { "id": "request-1", "component": "homeboy" },
                "campaign": { "id": "campaign-1", "safety_class": "read_only" },
                "artifacts": [{ "id": "case-log", "kind": "case_log" }],
                "required_artifacts": [{ "id": "case-log", "kind": "case_log", "required": true }],
                "gates": [{ "id": "open-findings", "kind": "threshold", "metric": "open_findings", "operator": "equal", "value": 0 }]
            }))
            .expect("envelope json"),
        )
        .expect("write envelope");
        store
            .record_artifact(&run.id, "runner-output", &envelope)
            .expect("record envelope");

        let (output, _) = handlers::artifacts(&run.id).expect("artifacts");
        let RunsOutput::Artifacts(output) = output else {
            panic!("expected artifacts output");
        };

        assert_eq!(output.fuzz_result_envelopes.len(), 1);
        let inspection = &output.fuzz_result_envelopes[0];
        assert!(inspection.valid);
        assert!(inspection
            .recognized_by
            .contains(&"content.schema".to_string()));
        let summary = inspection.summary.as_ref().expect("summary");
        assert_eq!(summary.envelope_id, "envelope-1");
        assert_eq!(summary.campaign_id, "campaign-1");
        assert_eq!(summary.gate_status, "passed");
        assert_eq!(summary.gate_count, 1);
        assert_eq!(summary.required_artifact_count, 1);
        assert_eq!(summary.artifact_ref_count, 1);
    });
}

#[test]
fn runs_artifacts_summarizes_static_site_fixture_matrix_artifacts() {
    with_isolated_home(|home| {
        let _xdg = XdgGuard::unset();
        let store = ObservationStore::open_initialized().expect("store");
        let run = store
            .start_run(sample_run(
                "bench",
                "static-site-importer",
                "static-site-importer-fixture-matrix",
                serde_json::json!({
                    "scenario_id": "static-site-fixture-matrix",
                    "result_summary": {
                        "fixture_count": 71,
                        "succeeded": 6,
                        "failed": 65,
                        "not_run": 0,
                        "finding_count": 1055
                    }
                }),
            ))
            .expect("matrix run");
        store
            .finish_run(&run.id, RunStatus::Fail, None)
            .expect("finish matrix run");

        let result = home.path().join("static-site-fixture-matrix-result.json");
        std::fs::write(
            &result,
            serde_json::to_vec(&serde_json::json!({
                "schema": "homeboy-rigs/static-site-fixture-matrix-result/v1",
                "matrix_id": "static-site-importer-fixture-matrix-final",
                "summary": {
                    "fixture_count": 71,
                    "succeeded": 6,
                    "failed": 65,
                    "not_run": 0,
                    "finding_count": 1055,
                    "groups": {
                        "fallback_block": 700,
                        "visual_parity": 250,
                        "unresolved_asset": 105
                    }
                },
                "fixtures": [
                    { "fixture_id": "fixture-passed", "status": "passed" },
                    { "fixture_id": "fixture-failed", "status": "failed" }
                ],
                "findings": [
                    {
                        "fixture_id": "fixture-failed",
                        "kind": "unsupported_html_fallback",
                        "group_key": "fallback_block",
                        "candidate_repo": "Automattic/blocks-engine"
                    }
                ],
                "fanout_groups": [{
                    "group_key": "fallback_block",
                    "findings": []
                }]
            }))
            .expect("result json"),
        )
        .expect("write result");
        let summary = home.path().join("summary.json");
        std::fs::write(
            &summary,
            serde_json::to_vec(&serde_json::json!({
                "fixture_count": 71,
                "succeeded": 6,
                "failed": 65,
                "not_run": 0,
                "finding_count": 1055,
                "groups": {
                    "fallback_block": 700,
                    "visual_parity": 250,
                    "unresolved_asset": 105
                }
            }))
            .expect("summary json"),
        )
        .expect("write summary");
        let packets = home.path().join("finding-packets.json");
        std::fs::write(
            &packets,
            serde_json::to_vec(&serde_json::json!([
                {
                    "diagnostic_id": "diag-001",
                    "fixture_id": "fixture-failed",
                    "category": "fallback_block",
                    "kind": "unsupported_html_fallback",
                    "candidate_repo": "Automattic/blocks-engine"
                },
                {
                    "diagnostic_id": "diag-002",
                    "fixture_id": "fixture-failed",
                    "category": "visual_parity",
                    "kind": "visual_parity_mismatch",
                    "candidate_repo": "chubes4/wp-site-generator"
                },
                {
                    "diagnostic_id": "diag-003",
                    "fixture_id": "fixture-failed",
                    "category": "unresolved_asset",
                    "kind": "asset_map",
                    "candidate_repo": "chubes4/static-site-importer"
                }
            ]))
            .expect("packet json"),
        )
        .expect("write packets");
        store
            .record_artifact(&run.id, "result", &result)
            .expect("record result");
        store
            .record_artifact(&run.id, "summary", &summary)
            .expect("record summary");
        store
            .record_artifact(&run.id, "finding_packets", &packets)
            .expect("record packets");

        let (output, _) = handlers::artifacts(&run.id).expect("artifacts");
        let RunsOutput::Artifacts(output) = output else {
            panic!("expected artifacts output");
        };
        let summary = output.matrix_summary.expect("matrix summary");

        assert_eq!(summary.fixture_count, 71);
        assert_eq!(summary.passed_count, 6);
        assert_eq!(summary.failed_count, 65);
        assert_eq!(summary.not_run_count, 0);
        assert_eq!(summary.finding_count, 1055);
        assert_eq!(summary.group_counts[0].key, "fallback_block");
        assert_eq!(
            summary.top_diagnostic_kinds[0].key,
            "unsupported_html_fallback"
        );
        assert!(summary
            .candidate_repo_counts
            .iter()
            .any(|count| count.key == "Automattic/blocks-engine"));
        assert_eq!(summary.result_refs.len(), 2);
        assert_eq!(summary.finding_packet_refs.len(), 1);
        assert!(summary.parse_diagnostics.is_empty());
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
