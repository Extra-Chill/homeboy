use homeboy::core::observation::evidence_report::{self, RunEvidenceReport};
use homeboy::core::observation::{
    disk_budget::disk_budget, evidence_report::evidence_failure_summary, runs_service,
    ArtifactRecord, BoundedArtifactProjection, ObservationStore, RunRecord,
};
use serde_json::{json, Value};

use crate::commands::utils::response::{self as response, CommandIdentity};

use super::types::{
    RunsEvidenceArtifactIndexSummary, RunsEvidenceLinksSummary, RunsEvidenceSummaryOutput,
};
use super::{require_run, run_summary, CmdResult, RunSummary, RunsOutput};

const DEFAULT_ARTIFACT_LIMIT: usize = 8;
/// Maximum bytes in the pretty-serialized public command-result envelope.
///
/// This is deliberately measured after the standard response wrapper has
/// lifted command metadata, rather than against the inner `RunsOutput` value.
/// The measurement includes the newline emitted by both stdout and `--output`.
const MAX_PUBLIC_ENVELOPE_BYTES: usize = 16 * 1024;
// Eight compact artifacts plus a failure summary fit below the public envelope
// contract even when every retained byte serializes as a six-byte JSON escape.
// Keep ordinary opaque identifiers (including 36-byte UUID run IDs) intact.
// Stable locators are either emitted verbatim or omitted by the envelope pass;
// they must never be shortened into invalid commands.
const MAX_STRING_BYTES: usize = 64;

/// `runs evidence` output. The report shaping lives in
/// [`homeboy::core::observation::evidence_report`]; this adapter only embeds
/// the command-local [`RunSummary`] as the report's `run` field.
pub type RunsEvidenceOutput = RunEvidenceReport<RunSummary>;

pub fn evidence(run_id: &str) -> CmdResult<RunsOutput> {
    let store = ObservationStore::open_initialized()?;
    let run = require_run(&store, run_id)?;
    let mut artifacts = runs_service::list_artifacts_for_run(&store, &run.id)?;
    artifacts.extend(runs_service::related_lab_artifacts_for_runner_job(
        &store, &run,
    )?);
    let descendant_evidence = run
        .metadata_json
        .get("descendant_run_evidence")
        .cloned()
        .and_then(|value| {
            serde_json::from_value::<Vec<evidence_report::DescendantRunEvidenceRef>>(value).ok()
        })
        .unwrap_or_default()
        .into_iter()
        .filter(|reference| reference.is_valid() && reference.run_id != run.id)
        .filter_map(|reference| {
            let child = store.get_run(&reference.run_id).ok().flatten()?;
            if child.kind != reference.kind {
                return None;
            }
            let child_artifacts = runs_service::list_artifacts_for_run(&store, &child.id).ok()?;
            Some(evidence_report::DescendantRunEvidence {
                evidence_command: format!("homeboy runs evidence {}", child.id),
                primary_diagnostic: evidence_report::failure_diagnostic_artifact(&child_artifacts),
                status: child.status,
                reference,
            })
        })
        .collect();
    let artifact_root = homeboy::core::artifacts::root()?;
    let disk_budget = disk_budget(
        &artifact_root,
        "artifact",
        "disk budget probing is not implemented for this platform",
    );
    // The full report assembly lives in core so non-CLI consumers (HTTP API,
    // MCP, automation) can reuse it; this adapter only supplies the CLI-owned
    // `RunSummary`, disk budget, and command label.
    let run_summary = run_summary(run.clone());
    let report = evidence_report::build_run_evidence_report_with_descendant_evidence(
        evidence_report::RunEvidenceReportInputs {
            command: "runs.evidence",
            run,
            run_summary,
            artifacts,
            artifact_root,
            disk_budget,
        },
        descendant_evidence,
    );

    Ok((RunsOutput::Evidence(Box::new(report)), 0))
}

/// Render the full core report or a bounded CLI projection without changing the
/// reusable report schema used by non-CLI consumers.
pub fn evidence_projection(run_id: &str, full: bool) -> CmdResult<RunsOutput> {
    if full {
        return evidence(run_id);
    }
    let store = ObservationStore::open_initialized()?;
    let run = require_run(&store, run_id)?;
    let mut run_ids = vec![run.id.clone()];
    run_ids.extend(runs_service::related_lab_run_ids(&store, &run)?);
    let projection =
        store.bounded_artifact_projection_for_runs(&run_ids, DEFAULT_ARTIFACT_LIMIT)?;
    let summary = compact_projection(&run, projection);
    let summary = compact_to_output_limit(summary);
    Ok((RunsOutput::EvidenceSummary(summary), 0))
}

fn compact_projection(
    run: &RunRecord,
    projection: BoundedArtifactProjection,
) -> RunsEvidenceSummaryOutput {
    let failure = evidence_failure_summary(run);
    let diagnostic = projection
        .diagnostic
        .as_ref()
        .map(|artifact| compact_artifact(artifact, true));
    let diagnostic_retrieval_command = projection.diagnostic.as_ref().map(artifact_continuation);
    let artifacts = projection
        .artifacts
        .iter()
        .map(|artifact| compact_artifact(artifact, true))
        .collect::<Vec<_>>();
    let mut failure_summary = json!({
        "failed": failure.failed,
        "status": bounded_string(&failure.status),
        "exit_code": failure.exit_code,
        "error": failure.error.as_deref().map(bounded_string),
        "gate_failures": failure.gate_failures.iter().take(4).map(|value| bounded_string(value)).collect::<Vec<_>>(),
        "hints": failure.hints.iter().take(4).map(|value| bounded_string(value)).collect::<Vec<_>>(),
    });
    if let Some(diagnostic) = &diagnostic {
        failure_summary["diagnostic"] = json!({ "artifact": diagnostic });
    }
    RunsEvidenceSummaryOutput {
        schema: "homeboy/runs-evidence-summary/v1",
        byte_limit: MAX_PUBLIC_ENVELOPE_BYTES,
        command: "runs.evidence",
        run_id: Some(run.id.clone()),
        run: json!({ "id": run.id, "kind": bounded_string(&run.kind), "status": bounded_string(&run.status) }),
        failure: failure_summary,
        diagnostic: None,
        diagnostic_retrieval_command,
        artifact_index: RunsEvidenceArtifactIndexSummary {
            count: projection.count,
            file_count: projection.file_count,
            directory_count: projection.directory_count,
            url_count: projection.url_count,
            missing_count: 0,
            missing_count_known: false,
            total_size_bytes: projection.total_size_bytes,
            returned_count: artifacts.len(),
            omitted_count: projection.count.saturating_sub(artifacts.len()),
            artifacts,
            complete_command: Some(format!("homeboy runs artifacts {}", run.id)),
        },
        evidence_links: RunsEvidenceLinksSummary {
            count: 0,
            returned_count: 0,
            omitted_count: 0,
            links: Vec::new(),
        },
        full_report_command: Some(format!("homeboy runs evidence {} --full", run.id)),
    }
}

fn compact_artifact(artifact: &ArtifactRecord, include_continuation: bool) -> Value {
    // The bounded projection is safe to send beyond the controller: opaque
    // handles are its only artifact locators. `runs evidence --full` retains
    // the complete artifact records, including their paths.
    json!({ "handle": artifact.id, "kind": bounded_string(&artifact.kind), "type": bounded_string(&artifact.artifact_type), "continuation": include_continuation.then(|| artifact_continuation(artifact)) })
}

fn artifact_continuation(artifact: &ArtifactRecord) -> String {
    if artifact.artifact_type == "directory" {
        format!("homeboy runs artifact preview-handle {}", artifact.id)
    } else {
        format!("homeboy runs artifact get-handle {} -o <path>", artifact.id)
    }
}

/// Release builds must never turn a large but valid evidence inventory into an
/// internal error. Remove optional samples deterministically until the final
/// pretty-serialized public envelope is within the command's byte contract.
fn compact_to_output_limit(mut summary: RunsEvidenceSummaryOutput) -> RunsEvidenceSummaryOutput {
    loop {
        let output = RunsOutput::EvidenceSummary(summary);
        if public_envelope_bytes(&output) <= MAX_PUBLIC_ENVELOPE_BYTES {
            let RunsOutput::EvidenceSummary(summary) = output else {
                unreachable!()
            };
            return summary;
        }
        let RunsOutput::EvidenceSummary(mut next) = output else {
            unreachable!()
        };
        if next.artifact_index.artifacts.pop().is_some() {
            next.artifact_index.returned_count = next.artifact_index.artifacts.len();
            next.artifact_index.omitted_count = next
                .artifact_index
                .count
                .saturating_sub(next.artifact_index.returned_count);
            summary = next;
            continue;
        }
        if next.diagnostic_retrieval_command.take().is_some() {
            summary = next;
            continue;
        }
        if next.full_report_command.take().is_some() {
            summary = next;
            continue;
        }
        if next.artifact_index.complete_command.take().is_some() {
            summary = next;
            continue;
        }
        if next
            .run
            .as_object_mut()
            .and_then(|run| run.remove("id"))
            .is_some()
        {
            summary = next;
            continue;
        }
        if next.run_id.take().is_some() {
            summary = next;
            continue;
        }
        // The remaining schema, counts, status, and selected opaque diagnostic
        // handle are fixed-size. Artifact handles are validated 67-byte digests.
        return next;
    }
}

/// This is the exact public serializer used by `homeboy runs evidence`: the
/// runtime turns `RunsOutput` into a JSON value, wraps it as a v3 command
/// result, then pretty-serializes it for both stdout and `--output`.
fn public_envelope_bytes(output: &RunsOutput) -> usize {
    // `print_response` uses `writeln!` and `OutputWriteOptions::json_output`
    // appends the same newline to `--output` files.
    public_envelope_json(output).len() + 1
}

fn public_envelope_json(output: &RunsOutput) -> String {
    let data = serde_json::to_value(output).expect("runs evidence output serializes");
    let response = response::cli_response_for_json_result_for_identity(
        &Ok(data),
        0,
        &CommandIdentity::with_operation("runs", "evidence"),
        None,
    );
    serde_json::to_string_pretty(&response).expect("runs evidence public envelope serializes")
}

fn bounded_string(value: &str) -> String {
    if value.len() <= MAX_STRING_BYTES {
        return value.to_string();
    }
    let mut end = MAX_STRING_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...[{} bytes omitted]", &value[..end], value.len() - end)
}

#[cfg(test)]
mod tests {
    use homeboy::core::artifact_address::ArtifactAddressKind;
    use homeboy::core::artifact_links::PUBLIC_ARTIFACT_BASE_URL_ENV;
    use homeboy::core::observation::{NewRunRecord, ObservationStore, RunStatus};
    use homeboy::test_support::with_isolated_home;
    use serde_json::Value;
    use std::path::Path;

    use super::super::handlers::artifact_command;
    use super::super::types::{
        RunsArtifactArgs, RunsArtifactCommand, RunsArtifactPreviewHandleArgs,
    };
    use super::*;

    struct XdgGuard(Option<String>);

    struct EnvGuard {
        key: &'static str,
        prior: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let prior = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, prior }
        }

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
                        "remote_command": ["node", "review.mjs"],
                        "runner_execution_record": {
                            "schema": "homeboy/runner-execution-record/v1",
                            "execution_id": "job-1",
                            "runner_id": "lab-default",
                            "transport": "daemon",
                            "status": "failed",
                            "job_id": "job-1",
                            "orchestration_provenance": {
                                "schema": "homeboy/orchestration-target-provenance/v1",
                                "selected_runner_id": "lab-default",
                                "controller_binary": {
                                    "owner": "controller",
                                    "path": "/usr/local/bin/homeboy",
                                    "version": "0.334.0",
                                    "build_identity": "homeboy 0.334.0+controller"
                                },
                                "runner_daemon_binary": {
                                    "owner": "daemon",
                                    "path": "http://127.0.0.1:3000",
                                    "version": "0.334.0",
                                    "build_identity": "homeboy 0.334.0+daemon"
                                },
                                "runner_command_binary": {
                                    "owner": "configured binary",
                                    "path": "/opt/homeboy"
                                }
                            }
                        },
                        "gate_failures": ["p95_ms exceeded"],
                        "hints": ["inspect artifacts"],
                        "child_command_failures": [{
                            "argv": ["generic-child", "run", "--json"],
                            "exit_status": 9,
                            "stdout_tail": "child stdout tail",
                            "stderr_tail": "child stderr tail",
                            "scenario_id": "cold",
                            "iteration": "5/10",
                            "artifact_refs": [{
                                "kind": "log",
                                "ref": "runner-artifact://run/child-log"
                            }]
                        }],
                        "tracker_refs": [{
                            "kind": "linear",
                            "id": "HB-42"
                        }],
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
                        "resource_policy": {"hot_command":"bench"},
                        "lab": {
                            "failure": {
                                "schema": "homeboy/runner-exec-failure-projection/v1",
                                "failure_code": "validation.invalid_argument",
                                "phase": "preflight",
                                "exit_code": 1,
                                "runner_id": "lab-default",
                                "runner_job_id": "job-1",
                                "stderr_tail": "invalid input",
                                "stderr_sha256": "0000000000000000000000000000000000000000000000000000000000000000",
                                "runner_job_logs_command": "homeboy runner job logs lab-default job-1",
                                "remote_command_result_command": "homeboy runner job logs lab-default job-1 --json",
                                "artifact_refs": []
                            },
                            "remote_events": [{
                                "data": {
                                    "data": {
                                        "agent_task_lifecycle_event": {
                                            "schema": "homeboy/agent-task-run-plan-lifecycle-event/v1",
                                            "identity": {
                                                "runner_id": "lab-default",
                                                "runner_job_id": "job-1",
                                                "run_id": "run-typed"
                                            },
                                            "aggregate": {
                                                "schema": "homeboy/agent-task-aggregate/v1",
                                                "plan_id": "plan-from-event",
                                                "status": "succeeded",
                                                "totals": {"skipped": 0, "succeeded": 1, "failed": 0},
                                                "outcomes": []
                                            }
                                        }
                                    }
                                }
                            }]
                        }
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
            let serialized = serde_json::to_value(&output).expect("serialize evidence output");
            assert_eq!(serialized["variant"], "evidence");
            assert_eq!(serialized["payload"]["command"], "runs.evidence");
            let RunsOutput::Evidence(output) = output else {
                panic!("expected evidence output");
            };

            assert_eq!(output.command, "runs.evidence");
            assert_eq!(output.run_id, run.id);
            assert_eq!(output.run.kind, "bench");
            assert_eq!(
                output.homeboy_provenance.schema,
                "homeboy/homeboy-provenance/v1"
            );
            assert_eq!(output.homeboy_provenance.identities.len(), 6);
            assert_eq!(
                output.homeboy_provenance.identities[0].role,
                "observation_run_binary"
            );
            assert_eq!(
                output.homeboy_provenance.identities[0].version.as_deref(),
                Some("test-version")
            );
            assert_eq!(
                output.homeboy_provenance.identities[1].role,
                "controller_cli"
            );
            assert_eq!(
                output.homeboy_provenance.identities[2].role,
                "active_daemon"
            );
            assert_eq!(
                output.homeboy_provenance.identities[2]
                    .build_identity
                    .as_deref(),
                Some("homeboy 0.334.0+daemon")
            );
            assert_eq!(
                output.homeboy_provenance.identities[3].role,
                "configured_job_binary"
            );
            assert_eq!(
                output.homeboy_provenance.identities[3].unavailable_reason,
                Some("the binary path was recorded, but its Homeboy version was not observed")
            );
            assert_eq!(
                output.homeboy_provenance.identities[4].role,
                "runner_job_handoff"
            );
            assert_eq!(
                output.homeboy_provenance.identities[4].runner_id.as_deref(),
                Some("lab-default")
            );
            assert_eq!(
                output.homeboy_provenance.identities[4]
                    .runner_job_id
                    .as_deref(),
                Some("job-1")
            );
            assert_eq!(
                output.homeboy_provenance.identities[4].evidence_commands[0],
                "homeboy runner job logs lab-default job-1"
            );
            assert_eq!(
                output.homeboy_provenance.identities[5].role,
                "executed_child_homeboy"
            );
            assert_eq!(
                output.homeboy_provenance.identities[5].state,
                "inapplicable"
            );
            assert!(output.homeboy_provenance.warnings[0].contains(
                "controller_cli, active_daemon, configured_job_binary, and observation_run_binary"
            ));
            assert_eq!(output.tracker_refs.len(), 2);
            assert_eq!(output.tracker_refs[0].kind, "linear");
            assert_eq!(output.tracker_refs[0].id, "HB-42");
            assert_eq!(output.tracker_refs[1].kind, "github_issue");
            assert_eq!(output.tracker_refs[1].id, "Extra-Chill/homeboy#123");
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
            let runner_failure = output.failure.runner_failure.expect("runner failure");
            assert_eq!(
                runner_failure.failure_code.as_deref(),
                Some("validation.invalid_argument")
            );
            assert_eq!(runner_failure.phase.as_deref(), Some("preflight"));
            assert_eq!(runner_failure.runner_job_id, "job-1");
            assert_eq!(
                runner_failure.runner_job_logs_command,
                "homeboy runner job logs lab-default job-1"
            );
            assert_eq!(output.failure.gate_failures, vec!["p95_ms exceeded"]);
            assert_eq!(output.failure.hints, vec!["inspect artifacts"]);
            assert_eq!(
                output.failure.child_command_failures[0]["argv"][0],
                "generic-child"
            );
            assert_eq!(
                output.failure.child_command_failures[0]["stdout_tail"],
                "child stdout tail"
            );
            assert_eq!(
                output.failure.child_command_failures[0]["stderr_tail"],
                "child stderr tail"
            );
            assert_eq!(
                output.failure.child_command_failures[0]["artifact_refs"][0]["ref"],
                "runner-artifact://run/child-log"
            );
            let manifest = output.evidence_manifest.expect("evidence manifest");
            assert_eq!(manifest.schema, "homeboy/evidence-manifest/v1");
            assert_eq!(manifest.tracker_refs[0].id, "Extra-Chill/homeboy#123");
            assert_eq!(manifest.blocking_conditions[0].kind, "review_needed");
            // An attached manifest is surfaced verbatim, stamped with where it
            // was found rather than replaced by a derived reading.
            assert_eq!(
                manifest.source,
                Some(homeboy::core::evidence_manifest::EvidenceManifestSource::RunMetadata)
            );
            assert_eq!(
                manifest.interpretation.summary,
                "Evidence is blocked on reviewer confirmation."
            );
            assert!(output.evidence_manifest_errors.is_empty());
            let lifecycle_event = output
                .agent_task_lifecycle_event
                .expect("agent task lifecycle event");
            assert_eq!(
                lifecycle_event["aggregate"]["plan_id"].as_str(),
                Some("plan-from-event")
            );
            assert!(
                output.disk_budget.available_bytes.is_some()
                    || output.disk_budget.warning.is_some()
            );
            homeboy::core::set_artifact_root_override(None);
        });
    }

    #[test]
    fn evidence_command_surfaces_local_pre_handoff_failure_without_runner_connectivity() {
        with_isolated_home(|_| {
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(sample_run(
                    "runner_execution",
                    "homeboy-lab",
                    "",
                    serde_json::json!({
                        "runner_pre_handoff_failure": {
                            "phase": "connection",
                            "code": "runner.lab_transport_failure",
                            "message": "tunnel setup failed: token=<redacted>",
                            "details": { "token": "<redacted>" },
                            "recovery": {
                                "evidence": "homeboy runs evidence local-pre-handoff",
                                "status": "homeboy runs show local-pre-handoff",
                                "retry": "homeboy runner exec homeboy-lab --run-id <new-run-id> -- <command>"
                            }
                        }
                    }),
                ))
                .expect("run");
            store
                .finish_run(&run.id, homeboy::core::observation::RunStatus::Fail, None)
                .expect("finish");

            let (output, _) = evidence(&run.id).expect("local evidence does not need a runner");
            let RunsOutput::Evidence(report) = output else {
                panic!("full evidence output");
            };
            let failure = report
                .failure
                .runner_pre_handoff_failure
                .expect("pre-handoff evidence");
            assert_eq!(failure.phase, "connection");
            assert_eq!(failure.code, "runner.lab_transport_failure");
            assert!(failure.message.contains("<redacted>"));
            assert_eq!(
                failure.recovery["evidence"],
                "homeboy runs evidence local-pre-handoff"
            );
            assert!(failure.recovery["retry"]
                .as_str()
                .expect("retry command")
                .contains("--run-id <new-run-id>"));
        });
    }

    #[test]
    fn evidence_projection_bounds_large_inventories_and_keeps_the_top_diagnostic() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(sample_run(
                    "bench",
                    "homeboy",
                    "bounded-evidence",
                    serde_json::json!({ "exit_code": 1, "error": "\u{0001}".repeat(100_000) }),
                ))
                .expect("run");
            store
                .finish_run(&run.id, RunStatus::Fail, None)
                .expect("finish run");
            let artifact_path = home.path().join("artifact.json");
            std::fs::write(&artifact_path, b"{}").expect("artifact");
            let mut diagnostic_id = None;
            for index in 0..1022 {
                let metadata = if index == 1021 {
                    serde_json::json!({
                        "failure_diagnostic": true,
                        "failure_diagnostic_rank": 99
                    })
                } else if index == 1020 {
                    serde_json::json!({
                        "failure_diagnostic": true,
                        "failure_diagnostic_rank": 1
                    })
                } else {
                    serde_json::json!({ "failure_diagnostic_rank": index })
                };
                let kind = if index == 1021 {
                    format!("diagnostic-{}", "\u{0002}".repeat(100_000))
                } else {
                    format!("artifact-{index}")
                };
                let artifact = store
                    .record_artifact_with_id(
                        &run.id,
                        &kind,
                        &artifact_path,
                        &format!("artifact-id-{index:04}"),
                        metadata,
                    )
                    .expect("record artifact");
                if index == 1021 {
                    diagnostic_id = Some(artifact.id);
                }
            }
            let diagnostic_id = diagnostic_id.expect("diagnostic id");
            let omitted = store
                .get_artifact("artifact-id-0500")
                .expect("read omitted artifact")
                .expect("omitted artifact");
            std::fs::remove_file(&omitted.path).expect("remove omitted artifact bytes");

            let (output, _) = evidence_projection(&run.id, false).expect("bounded evidence");
            let RunsOutput::EvidenceSummary(summary) = output else {
                panic!("expected bounded evidence summary");
            };
            assert_eq!(summary.artifact_index.count, 1022);
            assert_eq!(
                summary.artifact_index.returned_count,
                DEFAULT_ARTIFACT_LIMIT
            );
            assert_eq!(summary.artifact_index.omitted_count, 1014);
            assert!(!summary.artifact_index.missing_count_known);
            assert_eq!(summary.run_id.as_deref(), Some(run.id.as_str()));
            assert_eq!(summary.run["id"], run.id);
            assert_eq!(
                summary.artifact_index.complete_command.as_deref(),
                Some(format!("homeboy runs artifacts {}", run.id).as_str())
            );
            assert_eq!(
                summary.full_report_command.as_deref(),
                Some(format!("homeboy runs evidence {} --full", run.id).as_str())
            );
            let summary_output = RunsOutput::EvidenceSummary(summary);
            assert!(
                public_envelope_bytes(&summary_output) <= MAX_PUBLIC_ENVELOPE_BYTES,
                "the pretty command-result envelope must satisfy the public byte contract"
            );
            let expected_envelope = public_envelope_json(&summary_output);
            let summary_json = serde_json::to_string(&summary_output).expect("serialize summary");
            assert!(
                !summary_json.contains(home.path().to_str().expect("UTF-8 controller path")),
                "the bounded output must not leak a controller-local path"
            );
            assert!(
                !expected_envelope.contains(artifact_path.to_str().expect("UTF-8 artifact path")),
                "the public command envelope must not leak a local artifact path"
            );
            let output_path = home.path().join("evidence-envelope.json");
            let data = serde_json::to_value(&summary_output).expect("serialize evidence output");
            response::write_json_to_file_for_identity(
                &Ok(data),
                output_path.to_str().expect("UTF-8 output path"),
                0,
                &CommandIdentity::with_operation("runs", "evidence"),
                None,
            );
            let output_file = std::fs::read_to_string(output_path).expect("read --output file");
            assert_eq!(output_file, format!("{expected_envelope}\n"));
            assert!(output_file.len() <= MAX_PUBLIC_ENVELOPE_BYTES);
            let envelope: Value = serde_json::from_str(&expected_envelope).expect("parse envelope");
            assert_eq!(envelope["schema"], "homeboy/command-result/v3");
            assert_eq!(envelope["command"], "runs");
            assert_eq!(envelope["operation"], "evidence");
            assert_eq!(envelope["data"]["variant"], "evidence_summary");
            let RunsOutput::EvidenceSummary(summary) = summary_output else {
                unreachable!()
            };
            let handle = summary.artifact_index.artifacts[0]["handle"]
                .as_str()
                .expect("bounded diagnostic handle")
                .to_string();
            assert!(handle.starts_with("ah_"));
            assert_eq!(
                summary.diagnostic_retrieval_command.as_deref(),
                Some(format!("homeboy runs artifact get-handle {handle} -o <path>").as_str())
            );
            let returned_ids = summary
                .artifact_index
                .artifacts
                .iter()
                .filter_map(|artifact| artifact["handle"].as_str())
                .collect::<Vec<_>>();
            assert_eq!(returned_ids.len(), DEFAULT_ARTIFACT_LIMIT);
            assert!(returned_ids.iter().all(|id| !id.is_empty()));
            let omitted_id = (0..1022)
                .map(|index| format!("artifact-id-{index:04}"))
                .find(|id| !returned_ids.contains(&id.as_str()))
                .expect("an omitted artifact id");
            let summary_json = serde_json::to_string(&summary).expect("serialize summary");
            assert!(
                !summary_json.contains(&format!("\"{omitted_id}\"")),
                "the bounded output must not leak omitted artifact IDs"
            );

            let (full, _) = evidence_projection(&run.id, true).expect("full evidence");
            let RunsOutput::Evidence(full) = full else {
                panic!("expected full evidence report");
            };
            assert_eq!(full.artifact_index.count, 1022);
            assert_eq!(full.artifact_index.artifacts.len(), 1022);
            assert_eq!(
                full.failure
                    .diagnostic
                    .as_ref()
                    .and_then(|diagnostic| diagnostic.artifact.as_ref())
                    .map(|artifact| artifact.id.as_str()),
                Some(diagnostic_id.as_str())
            );

            let (selection, _) = super::super::handlers::apply_field_selection(
                RunsOutput::EvidenceSummary(summary),
                &["$.failure.diagnostic.artifact.handle".to_string()],
            )
            .expect("field projection");
            let RunsOutput::FieldSelection(selection) = selection else {
                panic!("expected field selection");
            };
            assert_eq!(selection.fields[0].value, Value::String(handle));
        });
    }

    #[test]
    fn evidence_projection_continues_selected_directory_diagnostics_by_handle() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(sample_run(
                    "bench",
                    "homeboy",
                    "directory-diagnostic",
                    Value::Null,
                ))
                .expect("run");
            let directory = home.path().join("diagnostic-site");
            std::fs::create_dir_all(&directory).expect("directory");
            std::fs::write(directory.join("index.html"), "<html>diagnostic</html>").expect("index");
            let artifact = store
                .record_directory_artifact_with_metadata(
                    &run.id,
                    "diagnostic-site",
                    &directory,
                    json!({ "failure_diagnostic": true, "failure_diagnostic_rank": 1 }),
                )
                .expect("directory artifact");

            let (output, _) = evidence_projection(&run.id, false).expect("bounded evidence");
            let RunsOutput::EvidenceSummary(summary) = output else {
                panic!("expected bounded evidence summary");
            };
            let handle = summary.artifact_index.artifacts[0]["handle"]
                .as_str()
                .expect("directory handle");
            let continuation = format!("homeboy runs artifact preview-handle {handle}");
            assert_eq!(
                summary.artifact_index.artifacts[0]["continuation"],
                continuation
            );
            assert_eq!(summary.failure["diagnostic"]["artifact"]["handle"], handle);
            assert_eq!(
                summary.diagnostic_retrieval_command.as_deref(),
                Some(continuation.as_str())
            );

            assert_eq!(artifact.artifact_type, "directory");

            let (preview, _) = artifact_command(RunsArtifactArgs {
                command: RunsArtifactCommand::PreviewHandle(RunsArtifactPreviewHandleArgs {
                    handle: handle.to_string(),
                    port: None,
                }),
            })
            .expect("preview selected directory diagnostic by handle");
            let RunsOutput::ArtifactPreview(preview) = preview else {
                panic!("expected directory preview output");
            };
            unsafe {
                libc::kill(preview.process_id as i32, libc::SIGTERM);
            }
            assert_eq!(preview.run_id, run.id);
            assert_eq!(preview.artifact_id, artifact.id);
            assert!(preview.base_url.starts_with("http://127.0.0.1:"));
        });
    }

    #[test]
    fn bounded_strings_keep_ordinary_uuid_run_ids_exact() {
        let run_id = "123e4567-e89b-12d3-a456-426614174000";
        assert_eq!(bounded_string(run_id), run_id);
    }

    #[test]
    fn pathological_run_ids_omit_optional_locators_instead_of_truncating_them() {
        let run_id = "run-".to_string() + &"x".repeat(MAX_PUBLIC_ENVELOPE_BYTES);
        let summary = RunsEvidenceSummaryOutput {
            schema: "homeboy/runs-evidence-summary/v1",
            byte_limit: MAX_PUBLIC_ENVELOPE_BYTES,
            command: "runs.evidence",
            run_id: Some(run_id.clone()),
            run: json!({ "id": run_id, "kind": "bench", "status": "fail" }),
            failure: json!({ "failed": true, "status": "fail", "exit_code": 1 }),
            diagnostic: None,
            diagnostic_retrieval_command: None,
            artifact_index: RunsEvidenceArtifactIndexSummary {
                count: 0,
                file_count: 0,
                directory_count: 0,
                url_count: 0,
                missing_count: 0,
                missing_count_known: false,
                total_size_bytes: 0,
                artifacts: Vec::new(),
                returned_count: 0,
                omitted_count: 0,
                complete_command: Some(format!("homeboy runs artifacts {run_id}")),
            },
            evidence_links: RunsEvidenceLinksSummary {
                count: 0,
                links: Vec::new(),
                returned_count: 0,
                omitted_count: 0,
            },
            full_report_command: Some(format!("homeboy runs evidence {run_id} --full")),
        };

        let summary = compact_to_output_limit(summary);
        assert!(summary.run_id.is_none());
        assert!(summary.run.get("id").is_none());
        assert!(summary.artifact_index.complete_command.is_none());
        assert!(summary.full_report_command.is_none());
        let output = RunsOutput::EvidenceSummary(summary);
        assert!(public_envelope_bytes(&output) <= MAX_PUBLIC_ENVELOPE_BYTES);
        assert!(!public_envelope_json(&output).contains("bytes omitted"));
    }

    #[test]
    fn evidence_command_resolves_descendant_run_without_copying_its_artifacts() {
        with_isolated_home(|home| {
            let store = ObservationStore::open_initialized().expect("store");
            let child = store
                .start_run(NewRunRecord::builder("bench").build())
                .expect("child run");
            let diagnostic = home.path().join("child-diagnostic.txt");
            std::fs::write(&diagnostic, "child failed").expect("diagnostic");
            store
                .record_artifact_with_metadata(
                    &child.id,
                    "failure_diagnostic",
                    &diagnostic,
                    serde_json::json!({ "failure_diagnostic": true, "failure_diagnostic_rank": 1 }),
                )
                .expect("child artifact");
            store
                .finish_run(&child.id, RunStatus::Fail, None)
                .expect("finish child");

            let parent = store
                .start_run(
                    NewRunRecord::builder("runner_execution")
                        .metadata(serde_json::json!({
                            "descendant_run_evidence": [{
                                "schema": "homeboy/descendant-run-evidence-ref/v1",
                                "run_id": child.id,
                                "kind": "bench",
                                "source": "controller.terminal_command_result.refs.runs"
                            }]
                        }))
                        .build(),
                )
                .expect("parent run");
            store
                .finish_run(&parent.id, RunStatus::Pass, None)
                .expect("finish parent");

            let (output, _) = evidence(&parent.id).expect("parent evidence");
            let RunsOutput::Evidence(report) = output else {
                panic!("expected evidence report");
            };
            assert_eq!(report.heartbeat.status, "pass");
            assert_eq!(report.artifact_index.count, 0);
            assert_eq!(report.descendant_evidence.len(), 1);
            assert_eq!(report.descendant_evidence[0].reference.run_id, child.id);
            assert_eq!(report.descendant_evidence[0].status, "fail");
            assert_eq!(
                report.descendant_evidence[0].evidence_command,
                format!("homeboy runs evidence {}", child.id)
            );
            assert!(report.descendant_evidence[0].primary_diagnostic.is_some());

            store
                .update_run_metadata(
                    &parent.id,
                    serde_json::json!({
                        "descendant_run_evidence": [{
                            "schema": "homeboy/descendant-run-evidence-ref/v1",
                            "run_id": child.id,
                            "kind": "stale-kind",
                            "source": "controller.terminal_command_result.refs.runs"
                        }]
                    }),
                )
                .expect("stale descendant kind");
            let (output, _) = evidence(&parent.id).expect("stale parent evidence");
            let RunsOutput::Evidence(report) = output else {
                panic!("expected evidence report");
            };
            assert!(report.descendant_evidence.is_empty());
        });
    }

    /// Before this wiring the manifest member was structurally always absent:
    /// nothing in the repository produced one, so `runs evidence` advertised an
    /// interpretation layer it never populated. A run nobody attached a manifest
    /// to must still carry one, marked as Homeboy's own reading.
    #[test]
    fn evidence_command_derives_a_manifest_when_no_producer_attached_one() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let _public_artifact_base = EnvGuard::unset(PUBLIC_ARTIFACT_BASE_URL_ENV);
            let artifact_root = home.path().join("agent-readable-artifacts");
            homeboy::core::set_artifact_root_override(Some(artifact_root));
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(sample_run(
                    "bench",
                    "homeboy",
                    "studio",
                    serde_json::json!({ "gate_failures": ["p95_ms exceeded"] }),
                ))
                .expect("run");
            store
                .finish_run(&run.id, RunStatus::Fail, None)
                .expect("finish run");

            let (output, _) = evidence(&run.id).expect("evidence");
            let RunsOutput::Evidence(output) = output else {
                panic!("expected evidence output");
            };

            let manifest = output.evidence_manifest.expect("derived evidence manifest");
            manifest
                .validate()
                .expect("derived manifest is contract-valid");
            assert_eq!(
                manifest.source,
                Some(homeboy::core::evidence_manifest::EvidenceManifestSource::Derived)
            );
            assert_eq!(
                manifest.status.state,
                homeboy::core::evidence_manifest::EvidenceManifestState::Failed
            );
            assert_eq!(manifest.id.as_deref(), Some(run.id.as_str()));
            assert_eq!(manifest.run_refs[0].id, run.id);
            assert_eq!(manifest.run_refs[0].kind.as_deref(), Some("bench"));
            assert_eq!(
                manifest.run_refs[0].component_id.as_deref(),
                Some("homeboy")
            );
            let gate = manifest
                .blocking_conditions
                .iter()
                .find(|condition| condition.kind == "gate_failure")
                .expect("gate failure blocker");
            assert_eq!(gate.summary, "p95_ms exceeded");
            assert!(output.evidence_manifest_errors.is_empty());
            homeboy::core::set_artifact_root_override(None);
        });
    }

    #[test]
    fn evidence_command_surfaces_static_html_preview_entrypoints() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let _public_artifact_base = EnvGuard::unset(PUBLIC_ARTIFACT_BASE_URL_ENV);
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
    fn evidence_command_marks_directory_artifacts_as_published_without_local_paths() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let artifact_root = home.path().join("agent-readable-artifacts");
            homeboy::core::set_artifact_root_override(Some(artifact_root));
            let _public_artifact_base = EnvGuard::set(
                PUBLIC_ARTIFACT_BASE_URL_ENV,
                "https://artifacts.example.test/homeboy",
            );
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(sample_run("bench", "homeboy", "studio", Value::Null))
                .expect("run");
            let directory = home.path().join("fuzz-artifacts");
            std::fs::create_dir_all(&directory).expect("directory");
            std::fs::write(directory.join("summary.json"), b"{}").expect("summary");
            store
                .record_directory_artifact(&run.id, "fuzz_artifacts", &directory)
                .expect("directory artifact");

            let (output, _) = evidence(&run.id).expect("evidence");

            let RunsOutput::Evidence(output) = output else {
                panic!("expected evidence output");
            };
            let artifact = output
                .artifact_index
                .artifacts
                .iter()
                .find(|artifact| artifact.kind == "fuzz_artifacts")
                .expect("fuzz directory artifact");
            assert!(artifact.public);
            assert!(artifact
                .path
                .starts_with("https://artifacts.example.test/homeboy/"));
            assert!(!Path::new(&artifact.path).is_absolute());
            let publication = artifact
                .directory_publication
                .as_ref()
                .expect("directory publication guidance");
            assert_eq!(publication.status, "published");
            assert!(publication.command.is_none());
            homeboy::core::set_artifact_root_override(None);
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

            let (bounded, _) = evidence_projection(&run.id, false).expect("bounded evidence");
            let RunsOutput::EvidenceSummary(bounded) = bounded else {
                panic!("expected bounded evidence");
            };
            assert_eq!(bounded.artifact_index.count, 0);
            assert_eq!(bounded.artifact_index.total_size_bytes, 0);
        });
    }

    #[test]
    fn diagnostic_continuations_keep_long_ids_and_urls_exact() {
        let long_id = "id".repeat(2_000);
        let long_url = format!("https://example.test/{}", "path/".repeat(1_000));
        let file = ArtifactRecord {
            id: long_id.clone(),
            run_id: "run".to_string(),
            kind: "diagnostic".to_string(),
            artifact_type: "file".to_string(),
            path: "ignored".to_string(),
            url: None,
            public_url: None,
            viewer_url: None,
            viewer_links: Vec::new(),
            sha256: None,
            size_bytes: None,
            mime: None,
            metadata_json: Value::Null,
            created_at: "now".to_string(),
        };
        assert_eq!(
            artifact_continuation(&file),
            format!("homeboy runs artifact get-handle {long_id} -o <path>")
        );
        let url = ArtifactRecord {
            artifact_type: "url".to_string(),
            url: Some(long_url.clone()),
            ..file
        };
        assert_eq!(
            artifact_continuation(&url),
            format!("homeboy runs artifact get-handle {long_id} -o <path>")
        );
    }

    #[test]
    fn evidence_includes_related_lab_fuzz_results_for_runner_failure() {
        with_isolated_home(|home| {
            let _xdg = XdgGuard::unset();
            let _public_artifact_base = EnvGuard::unset(PUBLIC_ARTIFACT_BASE_URL_ENV);
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
