use homeboy::core::observation::evidence_report::{self, RunEvidenceReport};
use homeboy::core::observation::{runs_service, ObservationStore};

use super::{disk, reconcile, require_run, run_summary, CmdResult, RunSummary, RunsOutput};

/// `runs evidence` output. The report shaping lives in
/// [`homeboy::core::observation::evidence_report`]; this adapter only embeds
/// the command-local [`RunSummary`] as the report's `run` field.
pub type RunsEvidenceOutput = RunEvidenceReport<RunSummary>;

pub fn evidence(run_id: &str) -> CmdResult<RunsOutput> {
    let store = ObservationStore::open_initialized()?;
    let run = require_run(&store, run_id)?;
    let artifacts = runs_service::list_artifacts_for_run(&store, run_id)?;
    let artifact_root = homeboy::core::artifacts::root()?;
    let disk_budget = disk::disk_budget(
        &artifact_root,
        "artifact",
        "disk budget probing is not implemented for this platform",
    );
    let stale_reason = reconcile::running_status_note(&run);
    let metadata = evidence_report::evidence_metadata(&run.metadata_json);
    let artifact_index = evidence_report::evidence_artifact_index(&artifacts);
    let failure = evidence_report::evidence_failure_summary(&run);
    let retention = evidence_report::evidence_retention(&artifact_root, &run.id);
    let evidence_links = evidence_report::evidence_links(&artifacts);
    let matrix_summary = evidence_report::evidence_matrix_summary(&run, &artifacts);
    let (evidence_manifest, evidence_manifest_errors) =
        evidence_report::evidence_manifest(&run, &artifacts);

    Ok((
        RunsOutput::Evidence(RunsEvidenceOutput {
            command: "runs.evidence",
            run_id: run.id.clone(),
            run: run_summary(run.clone()),
            homeboy_version: run.homeboy_version.clone(),
            metadata,
            heartbeat: evidence_report::EvidenceHeartbeat {
                status: run.status.clone(),
                stale: stale_reason.is_some(),
                stale_reason,
                owner_pid: homeboy::core::observation::run_owner_pid(&run),
                updated_at: run
                    .finished_at
                    .clone()
                    .unwrap_or_else(|| run.started_at.clone()),
            },
            artifact_index,
            retention,
            failure,
            disk_budget,
            evidence_links,
            matrix_summary,
            evidence_manifest,
            evidence_manifest_errors,
        }),
        0,
    ))
}

#[cfg(test)]
mod tests {
    use homeboy::core::artifact_address::ArtifactAddressKind;
    use homeboy::core::artifact_links::PUBLIC_ARTIFACT_BASE_URL_ENV;
    use homeboy::core::observation::{NewRunRecord, ObservationStore, RunStatus};
    use homeboy::test_support::with_isolated_home;
    use serde_json::Value;
    use std::path::Path;

    use super::*;

    struct XdgGuard(Option<String>);

    struct EnvGuard {
        key: &'static str,
        prior: Option<String>,
    }

    impl EnvGuard {
        fn unset(key: &'static str) -> Self {
            let prior = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, prior }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

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
    fn evidence_command_reports_registry_artifacts_retention_and_failure_summary() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let _public_artifact_base = EnvGuard::unset(PUBLIC_ARTIFACT_BASE_URL_ENV);
            let artifact_root = home.path().join("agent-readable-artifacts");
            homeboy::core::set_artifact_root_override(Some(artifact_root.clone()));
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(sample_run(
                    "bench",
                    "homeboy",
                    "studio",
                    serde_json::json!({
                        "exit_code": 1,
                        "error": "boom",
                        "gate_failures": ["p95_ms exceeded"],
                        "hints": ["inspect artifacts"],
                        "evidence_manifest": {
                            "schema": "homeboy/evidence-manifest/v1",
                            "status": { "state": "blocked" },
                            "interpretation": {
                                "summary": "Evidence is blocked on reviewer confirmation.",
                                "confidence": "medium"
                            },
                            "tracker_refs": [{
                                "kind": "github_issue",
                                "id": "Extra-Chill/homeboy#123"
                            }],
                            "blocking_conditions": [{
                                "kind": "review_needed",
                                "summary": "Maintainer review is required.",
                                "severity": "warning"
                            }]
                        },
                        "scenario_metrics": [{"scenario_id":"cold","metrics":{"p95_ms":42.0}}],
                        "resource_policy": {"hot_command":"bench"}
                    }),
                ))
                .expect("run");
            store
                .finish_run(&run.id, RunStatus::Fail, None)
                .expect("finish run");
            let artifact_path = home.path().join("bench-results.json");
            std::fs::write(&artifact_path, b"{}").expect("artifact");
            store
                .record_artifact(&run.id, "bench_results", &artifact_path)
                .expect("record artifact");
            store
                .record_url_artifact(&run.id, "review", "https://example.test/evidence")
                .expect("record url");

            let (output, _) = evidence(&run.id).expect("evidence");
            let RunsOutput::Evidence(output) = output else {
                panic!("expected evidence output");
            };

            assert_eq!(output.command, "runs.evidence");
            assert_eq!(output.run_id, run.id);
            assert_eq!(output.run.kind, "bench");
            assert_eq!(output.artifact_index.count, 2);
            assert_eq!(output.artifact_index.file_count, 1);
            assert_eq!(output.artifact_index.url_count, 1);
            assert_eq!(output.artifact_index.missing_count, 0);
            let bench_results = output
                .artifact_index
                .artifacts
                .iter()
                .find(|artifact| artifact.kind == "bench_results")
                .expect("bench results artifact");
            assert!(!bench_results.public);
            assert_eq!(
                bench_results.path,
                format!("homeboy://run/{}/artifact/{}", run.id, bench_results.id)
            );
            assert!(!Path::new(&bench_results.path).is_absolute());
            assert_eq!(
                bench_results.address.kind,
                ArtifactAddressKind::LocalOperatorPath
            );
            assert!(!bench_results.address.reviewer_visible);
            assert_eq!(
                bench_results.relative_to.as_deref(),
                Some("homeboy observation artifact store")
            );
            let expected_fetch_command = format!(
                "homeboy runs artifact get {} {} -o <path>",
                run.id, bench_results.id
            );
            assert_eq!(
                bench_results.fetch_command.as_deref(),
                Some(expected_fetch_command.as_str())
            );
            assert_eq!(bench_results.reference.schema, "homeboy/artifact-ref/v1");
            assert_eq!(bench_results.reference.id, bench_results.id);
            let review = output
                .artifact_index
                .artifacts
                .iter()
                .find(|artifact| artifact.kind == "review")
                .expect("review artifact");
            assert!(review.public);
            assert_eq!(
                review.public_url.as_deref(),
                Some("https://example.test/evidence")
            );
            assert_eq!(review.address.kind, ArtifactAddressKind::PublicUrl);
            assert!(review.address.reviewer_visible);
            assert_eq!(output.evidence_links.len(), 1);
            assert_eq!(
                output.evidence_links[0].reference.schema,
                "homeboy/evidence-ref/v1"
            );
            assert_eq!(
                output.evidence_links[0].target,
                "https://example.test/evidence"
            );
            assert_eq!(output.retention.default_retention_days, 30);
            assert!(output
                .retention
                .artifact_root
                .contains("agent-readable-artifacts"));
            assert!(output
                .retention
                .cleanup_command
                .contains("cleanup-persisted --run-id"));
            assert!(output.failure.failed);
            assert_eq!(output.failure.exit_code, Some(1));
            assert_eq!(output.failure.gate_failures, vec!["p95_ms exceeded"]);
            assert_eq!(output.failure.hints, vec!["inspect artifacts"]);
            let manifest = output.evidence_manifest.expect("evidence manifest");
            assert_eq!(manifest.schema, "homeboy/evidence-manifest/v1");
            assert_eq!(manifest.tracker_refs[0].id, "Extra-Chill/homeboy#123");
            assert_eq!(manifest.blocking_conditions[0].kind, "review_needed");
            assert!(output.evidence_manifest_errors.is_empty());
            assert!(
                output.disk_budget.available_bytes.is_some()
                    || output.disk_budget.warning.is_some()
            );
            homeboy::core::set_artifact_root_override(None);
        });
    }

    #[test]
    fn evidence_command_surfaces_static_html_preview_entrypoints() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let artifact_root = home.path().join("agent-readable-artifacts");
            homeboy::core::set_artifact_root_override(Some(artifact_root));
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
            std::fs::create_dir_all(&site).expect("site dir");
            std::fs::write(site.join("index.html"), b"<html>Home</html>").expect("index");
            store
                .record_directory_artifact(&run.id, "generated_site", &site)
                .expect("record directory");

            let (output, _) = evidence(&run.id).expect("evidence");
            let RunsOutput::Evidence(output) = output else {
                panic!("expected evidence output");
            };

            let artifact = output
                .artifact_index
                .artifacts
                .iter()
                .find(|artifact| artifact.kind == "generated_site")
                .expect("generated site artifact");
            assert_eq!(artifact.artifact_type, "directory");
            assert_eq!(artifact.preview_entrypoints.len(), 1);
            assert_eq!(artifact.preview_entrypoints[0].path, "index.html");
            assert_eq!(artifact.preview_entrypoints[0].label, "Open generated site");
            assert_eq!(artifact.preview_entrypoints[0].public_url, None);
        });
    }

    #[test]
    fn evidence_links_reject_unvalidated_local_urls() {
        with_isolated_home(|_home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(sample_run(
                    "trace",
                    "homeboy",
                    "studio",
                    serde_json::json!({}),
                ))
                .expect("run");
            store
                .record_url_artifact(&run.id, "review", "http://localhost:8888/evidence")
                .expect("record url");

            let (output, _) = evidence(&run.id).expect("evidence");
            let RunsOutput::Evidence(output) = output else {
                panic!("expected evidence output");
            };

            let review = output
                .artifact_index
                .artifacts
                .iter()
                .find(|artifact| artifact.kind == "review")
                .expect("review artifact");
            assert!(!review.public);
            assert_eq!(review.url, None);
            assert_eq!(review.public_url, None);
            assert_eq!(review.address.kind, ArtifactAddressKind::MetadataOnly);
            assert!(output.evidence_links.is_empty());
        });
    }

    #[test]
    fn evidence_surfaces_generic_matrix_summary_artifact() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(sample_run(
                    "matrix",
                    "homeboy",
                    "generic",
                    serde_json::json!({}),
                ))
                .expect("run");
            store
                .finish_run(&run.id, RunStatus::Fail, None)
                .expect("finish run");
            let summary_path = home.path().join("matrix-summary.json");
            std::fs::write(
                &summary_path,
                serde_json::to_vec(&serde_json::json!({
                    "schema": "homeboy/matrix-summary/v1",
                    "status": "needs_review",
                    "case_count": 4,
                    "failed_count": 1,
                    "needs_review_count": 2,
                    "artifact_refs": [
                        "homeboy://run/example/artifact/matrix-log",
                        { "kind": "report", "ref": "runner-artifact://runner/run/report", "label": "runner report" }
                    ],
                    "preview_refs": [
                        { "kind": "preview", "url": "https://example.test/preview", "label": "preview" }
                    ],
                    "cases": [
                        { "opaque": "domain data stays unread" }
                    ]
                }))
                .expect("summary json"),
            )
            .expect("write summary");
            store
                .record_artifact(&run.id, "matrix_summary", &summary_path)
                .expect("record summary");

            let (output, _) = evidence(&run.id).expect("evidence");
            let RunsOutput::Evidence(output) = output else {
                panic!("expected evidence output");
            };
            let summary = output.matrix_summary.expect("matrix summary");

            assert_eq!(summary.schema, "homeboy/matrix-summary/v1");
            assert_eq!(summary.run_id, run.id);
            assert_eq!(summary.status, "needs_review");
            assert_eq!(summary.case_count, 4);
            assert_eq!(summary.failed_count, 1);
            assert_eq!(summary.needs_review_count, 2);
            assert_eq!(summary.source_artifact.kind, "matrix_summary");
            assert_eq!(summary.artifact_refs.len(), 2);
            assert_eq!(
                summary.artifact_refs[0].target,
                "homeboy://run/example/artifact/matrix-log"
            );
            assert_eq!(summary.artifact_refs[1].kind, "report");
            assert_eq!(
                summary.preview_refs[0].target,
                "https://example.test/preview"
            );
        });
    }

    #[test]
    fn evidence_failure_summary_does_not_mark_running_run_failed() {
        with_isolated_home(|_home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(sample_run(
                    "trace",
                    "homeboy",
                    "studio",
                    serde_json::json!({
                        "status": "running",
                        "phase": "waiting-for-child"
                    }),
                ))
                .expect("run");

            let (output, _) = evidence(&run.id).expect("evidence");
            let RunsOutput::Evidence(output) = output else {
                panic!("expected evidence output");
            };

            assert_eq!(output.run.status, "running");
            assert_eq!(output.failure.status, "running");
            assert!(!output.failure.failed);
        });
    }

    #[test]
    fn evidence_includes_related_lab_fuzz_results_for_runner_failure() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let artifact_root = home.path().join("agent-readable-artifacts");
            homeboy::core::set_artifact_root_override(Some(artifact_root));
            let store = ObservationStore::open_initialized().expect("store");
            let remote_job_id = "remote-job-5997";
            let runner_run = store
                .start_run(sample_run(
                    "runner-exec",
                    "homeboy",
                    "studio",
                    serde_json::json!({
                        "exit_code": 1,
                        "lab": {
                            "runner": { "id": "lab-runner" },
                            "remote_job_id": remote_job_id
                        }
                    }),
                ))
                .expect("runner run");
            store
                .finish_run(&runner_run.id, RunStatus::Fail, None)
                .expect("finish runner run");
            let fuzz_run = store
                .start_run(sample_run(
                    "fuzz",
                    "homeboy",
                    "studio",
                    serde_json::json!({
                        "exit_code": 1,
                        "lab": { "remote_job_id": remote_job_id }
                    }),
                ))
                .expect("fuzz run");
            store
                .finish_run(&fuzz_run.id, RunStatus::Fail, None)
                .expect("finish fuzz run");
            let results_path = home.path().join("fuzz-results.json");
            std::fs::write(
                &results_path,
                br#"{"schema":"homeboy/fuzz-result-envelope/v1","campaign":{"id":"raw"}}"#,
            )
            .expect("write fuzz results");
            store
                .record_artifact(&fuzz_run.id, "fuzz_results", &results_path)
                .expect("record fuzz results");

            let (output, _) = evidence(&runner_run.id).expect("evidence");
            let RunsOutput::Evidence(output) = output else {
                panic!("expected evidence output");
            };

            let raw_results = output
                .artifact_index
                .artifacts
                .iter()
                .find(|artifact| artifact.kind == "fuzz_results")
                .expect("raw fuzz results artifact is discoverable");
            assert_eq!(raw_results.artifact_type, "file");
            assert_eq!(
                raw_results.address.kind,
                ArtifactAddressKind::LocalOperatorPath
            );
            let expected_fetch_command = format!(
                "homeboy runs artifact get {} {} -o <path>",
                fuzz_run.id, raw_results.id
            );
            assert_eq!(
                raw_results.fetch_command.as_deref(),
                Some(expected_fetch_command.as_str())
            );
            assert!(raw_results.exists);
            homeboy::core::set_artifact_root_override(None);
        });
    }
}
