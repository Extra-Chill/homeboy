use std::path::Path;

use homeboy::core::artifact_ref::{artifact_uri, EvidenceRef};
use homeboy::core::fuzz::inspect_fuzz_result_envelope_artifact;
use homeboy::core::observation::{runs_service, ArtifactRecord, ObservationStore};

use super::report::fuzz_result_envelope_evidence_ref;
use super::types::{FuzzInspectArgs, FuzzInspectCandidate, FuzzInspectOutput};

/// Artifact kinds that hold the raw fuzz runner input/result pair, ordered by
/// inspection preference. `fuzz_results` is the verbatim file a runner wrote to
/// `HOMEBOY_FUZZ_RESULTS_FILE`; `fuzz_result_envelope` is the normalized report
/// envelope persisted by `homeboy fuzz report`.
const RAW_FUZZ_RESULT_KINDS: &[&str] = &["fuzz_results", "fuzz_result_envelope"];

/// Implement `homeboy fuzz inspect <run-id>`.
///
/// Resolves the raw fuzz runner result for a run and prints it directly so an
/// operator debugging a remote Lab fuzz failure does not have to chase a remote
/// temp path or read large runner job logs. Works against either the `fuzz` run
/// id or the Lab `runner-exec` run id that offloaded it, because
/// [`runs_service::list_artifacts_for_run`] already folds in downstream Lab job
/// artifacts that share the same `remote_job_id`.
pub(super) fn run_inspect(args: FuzzInspectArgs) -> homeboy::core::Result<FuzzInspectOutput> {
    let store = ObservationStore::open_initialized()?;
    let artifacts = runs_service::list_artifacts_for_run(&store, &args.run_id)?;

    let mut candidates: Vec<&ArtifactRecord> = RAW_FUZZ_RESULT_KINDS
        .iter()
        .flat_map(|kind| {
            artifacts
                .iter()
                .filter(move |artifact| &artifact.kind == kind && artifact.artifact_type == "file")
        })
        .collect();
    for artifact in &artifacts {
        if !candidates
            .iter()
            .any(|candidate| candidate.id == artifact.id)
            && inspect_fuzz_result_envelope_artifact(artifact).is_some()
        {
            candidates.push(artifact);
        }
    }

    let candidate_index = candidates
        .iter()
        .map(|artifact| FuzzInspectCandidate {
            run_id: artifact.run_id.clone(),
            artifact_id: artifact.id.clone(),
            kind: artifact.kind.clone(),
            artifact_type: artifact.artifact_type.clone(),
            path: artifact.path.clone(),
            canonical_ref: artifact_uri(&artifact.run_id, &artifact.id),
            exists: Path::new(&artifact.path).is_file(),
        })
        .collect::<Vec<_>>();

    let Some(selected) = candidates
        .iter()
        .copied()
        .find(|artifact| Path::new(&artifact.path).is_file())
        .or_else(|| candidates.first().copied())
    else {
        return Ok(FuzzInspectOutput {
            command: "fuzz.inspect".to_string(),
            status: "not_found".to_string(),
            run_id: args.run_id.clone(),
            source_run_id: args.run_id.clone(),
            artifact_id: String::new(),
            artifact_kind: String::new(),
            artifact_path: String::new(),
            canonical_ref: None,
            evidence_ref: None,
            fetch_command: None,
            result: None,
            raw: None,
            envelope_summary: None,
            candidates: candidate_index,
            next_steps: vec![
                format!(
                    "No raw fuzz result artifact ({}) is recorded for `{}`. Confirm the runner wrote HOMEBOY_FUZZ_RESULTS_FILE and that Lab evidence was mirrored before cleanup.",
                    RAW_FUZZ_RESULT_KINDS.join(" / "),
                    args.run_id
                ),
                format!("Inspect run evidence with `homeboy runs evidence {}`.", args.run_id),
                format!("List recorded artifacts with `homeboy runs artifacts {}`.", args.run_id),
            ],
        });
    };

    let fetch_command = Some(format!(
        "homeboy runs artifact get {} {} -o <path>",
        selected.run_id, selected.id
    ));
    let canonical_ref = Some(artifact_uri(&selected.run_id, &selected.id));
    let evidence_ref = fuzz_inspect_evidence_ref(selected);

    let path = Path::new(&selected.path);
    if !path.is_file() {
        return Ok(FuzzInspectOutput {
            command: "fuzz.inspect".to_string(),
            status: "unavailable".to_string(),
            run_id: args.run_id.clone(),
            source_run_id: selected.run_id.clone(),
            artifact_id: selected.id.clone(),
            artifact_kind: selected.kind.clone(),
            artifact_path: selected.path.clone(),
            canonical_ref,
            evidence_ref,
            fetch_command: fetch_command.clone(),
            result: None,
            raw: None,
            envelope_summary: None,
            candidates: candidate_index,
            next_steps: vec![
                format!(
                    "Raw fuzz result artifact {} is recorded but its bytes are not present locally at {}.",
                    selected.id, selected.path
                ),
                format!(
                    "Fetch the bytes with `{}`.",
                    fetch_command.as_deref().unwrap_or_default()
                ),
            ],
        });
    }

    let bytes = std::fs::read(path).map_err(|error| {
        homeboy::core::Error::internal_io(error.to_string(), Some(selected.path.clone()))
    })?;
    let text = String::from_utf8_lossy(&bytes).to_string();

    let (result, raw) = if args.raw {
        (None, Some(text))
    } else {
        match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(value) => (Some(value), None),
            Err(_) => (None, Some(text)),
        }
    };

    let envelope_summary = inspect_fuzz_result_envelope_artifact(selected)
        .filter(|inspection| inspection.valid)
        .and_then(|inspection| inspection.summary);

    Ok(FuzzInspectOutput {
        command: "fuzz.inspect".to_string(),
        status: "ok".to_string(),
        run_id: args.run_id.clone(),
        source_run_id: selected.run_id.clone(),
        artifact_id: selected.id.clone(),
        artifact_kind: selected.kind.clone(),
        artifact_path: selected.path.clone(),
        canonical_ref,
        evidence_ref,
        fetch_command,
        result,
        raw,
        envelope_summary,
        candidates: candidate_index,
        next_steps: vec![
            format!(
                "Replay a failing case with `homeboy fuzz replay --run-id {} --case-id <id>`.",
                selected.run_id
            ),
            format!(
                "Review full run evidence with `homeboy runs evidence {}`.",
                args.run_id
            ),
        ],
    })
}

fn fuzz_inspect_evidence_ref(artifact: &ArtifactRecord) -> Option<EvidenceRef> {
    inspect_fuzz_result_envelope_artifact(artifact)
        .is_some()
        .then(|| fuzz_result_envelope_evidence_ref(artifact))
}

#[cfg(test)]
mod tests {
    use homeboy::core::observation::{NewRunRecord, ObservationStore, RunStatus};
    use homeboy::test_support::with_isolated_home;

    use super::super::types::FuzzInspectArgs;
    use super::run_inspect;

    fn sample_run(kind: &str, metadata: serde_json::Value) -> NewRunRecord {
        NewRunRecord::builder(kind)
            .component_id("homeboy")
            .command(format!("homeboy {kind} homeboy"))
            .cwd_path(std::path::Path::new("/tmp/homeboy-fixture"))
            .homeboy_version("test-version")
            .rig_id("studio")
            .metadata(metadata)
            .build()
    }

    #[test]
    fn inspect_prints_raw_fuzz_results_for_fuzz_run() {
        with_isolated_home(|home| {
            let artifact_root = home.path().join("agent-readable-artifacts");
            homeboy::core::set_artifact_root_override(Some(artifact_root));
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(sample_run("fuzz", serde_json::json!({ "exit_code": 1 })))
                .expect("run");
            store
                .finish_run(&run.id, RunStatus::Fail, None)
                .expect("finish");
            let results_path = home.path().join("fuzz-results.json");
            std::fs::write(
                &results_path,
                br#"{"schema":"homeboy/fuzz-result-envelope/v1","campaign":{"id":"raw"}}"#,
            )
            .expect("write results");
            store
                .record_artifact(&run.id, "fuzz_results", &results_path)
                .expect("record");

            let output = run_inspect(FuzzInspectArgs {
                run_id: run.id.clone(),
                raw: false,
            })
            .expect("inspect");

            assert_eq!(output.status, "ok");
            assert_eq!(output.artifact_kind, "fuzz_results");
            assert_eq!(output.source_run_id, run.id);
            let result = output.result.expect("parsed json result");
            assert_eq!(
                result.pointer("/campaign/id").and_then(|v| v.as_str()),
                Some("raw")
            );
            assert!(output.raw.is_none());
            assert!(output
                .fetch_command
                .as_deref()
                .unwrap()
                .contains("runs artifact get"));
            homeboy::core::set_artifact_root_override(None);
        });
    }

    #[test]
    fn inspect_resolves_raw_results_through_lab_runner_job() {
        with_isolated_home(|home| {
            let artifact_root = home.path().join("agent-readable-artifacts");
            homeboy::core::set_artifact_root_override(Some(artifact_root));
            let store = ObservationStore::open_initialized().expect("store");
            let remote_job_id = "remote-job-inspect-5997";
            let runner_run = store
                .start_run(sample_run(
                    "runner-exec",
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
                .expect("finish runner");
            let fuzz_run = store
                .start_run(sample_run(
                    "fuzz",
                    serde_json::json!({
                        "exit_code": 1,
                        "lab": { "remote_job_id": remote_job_id }
                    }),
                ))
                .expect("fuzz run");
            store
                .finish_run(&fuzz_run.id, RunStatus::Fail, None)
                .expect("finish fuzz");
            let results_path = home.path().join("fuzz-results.json");
            std::fs::write(&results_path, br#"{"campaign":{"id":"lab-raw"}}"#).expect("write");
            store
                .record_artifact(&fuzz_run.id, "fuzz_results", &results_path)
                .expect("record");

            // Inspecting the runner-exec run resolves the downstream fuzz raw result.
            let output = run_inspect(FuzzInspectArgs {
                run_id: runner_run.id.clone(),
                raw: false,
            })
            .expect("inspect");

            assert_eq!(output.status, "ok");
            assert_eq!(output.source_run_id, fuzz_run.id);
            assert_eq!(
                output
                    .result
                    .as_ref()
                    .and_then(|v| v.pointer("/campaign/id"))
                    .and_then(|v| v.as_str()),
                Some("lab-raw")
            );
            homeboy::core::set_artifact_root_override(None);
        });
    }

    #[test]
    fn inspect_raw_flag_returns_text_body() {
        with_isolated_home(|home| {
            let artifact_root = home.path().join("agent-readable-artifacts");
            homeboy::core::set_artifact_root_override(Some(artifact_root));
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(sample_run("fuzz", serde_json::json!({})))
                .expect("run");
            let results_path = home.path().join("fuzz-results.json");
            std::fs::write(&results_path, b"{\"ok\":true}").expect("write");
            store
                .record_artifact(&run.id, "fuzz_results", &results_path)
                .expect("record");

            let output = run_inspect(FuzzInspectArgs {
                run_id: run.id.clone(),
                raw: true,
            })
            .expect("inspect");

            assert_eq!(output.status, "ok");
            assert!(output.result.is_none());
            assert_eq!(output.raw.as_deref(), Some("{\"ok\":true}"));
            homeboy::core::set_artifact_root_override(None);
        });
    }

    #[test]
    fn inspect_discovers_canonical_envelope_with_generic_artifact_kind() {
        with_isolated_home(|home| {
            let artifact_root = home.path().join("agent-readable-artifacts");
            homeboy::core::set_artifact_root_override(Some(artifact_root));
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(sample_run("fuzz", serde_json::json!({})))
                .expect("run");
            let envelope_path = home.path().join("runner-output.json");
            std::fs::write(
                &envelope_path,
                br#"{
                    "schema":"homeboy/fuzz-result-envelope/v1",
                    "version":1,
                    "id":"envelope-1",
                    "status":"passed",
                    "request":{"id":"request-1","component":"homeboy"},
                    "campaign":{"id":"campaign-1","safety_class":"read_only"},
                    "required_artifacts":[{"id":"case-log","kind":"case_log","required":true}],
                    "gates":[{"id":"open-findings","kind":"threshold","metric":"open_findings","operator":"equal","value":0}]
                }"#,
            )
            .expect("write envelope");
            store
                .record_artifact(&run.id, "runner-output", &envelope_path)
                .expect("record");

            let output = run_inspect(FuzzInspectArgs {
                run_id: run.id.clone(),
                raw: false,
            })
            .expect("inspect");

            assert_eq!(output.status, "ok");
            assert_eq!(output.artifact_kind, "runner-output");
            assert!(output
                .canonical_ref
                .as_deref()
                .expect("canonical ref")
                .starts_with("homeboy://run/"));
            let evidence_ref = output.evidence_ref.as_ref().expect("evidence ref");
            assert_eq!(evidence_ref.role.as_deref(), Some("result"));
            assert_eq!(
                evidence_ref.semantic_key.as_deref(),
                Some("fuzz.result_envelope")
            );
            assert_eq!(
                Some(evidence_ref.canonical_uri()),
                output.canonical_ref.as_deref()
            );
            assert_eq!(
                output
                    .result
                    .as_ref()
                    .and_then(|v| v.pointer("/campaign/id"))
                    .and_then(|v| v.as_str()),
                Some("campaign-1")
            );
            let summary = output.envelope_summary.expect("envelope summary");
            assert_eq!(summary.gate_status, "passed");
            assert_eq!(summary.campaign_id, "campaign-1");
            homeboy::core::set_artifact_root_override(None);
        });
    }

    #[test]
    fn inspect_reports_not_found_without_raw_artifact() {
        with_isolated_home(|home| {
            let artifact_root = home.path().join("agent-readable-artifacts");
            homeboy::core::set_artifact_root_override(Some(artifact_root));
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(sample_run("fuzz", serde_json::json!({})))
                .expect("run");

            let output = run_inspect(FuzzInspectArgs {
                run_id: run.id.clone(),
                raw: false,
            })
            .expect("inspect");

            assert_eq!(output.status, "not_found");
            assert!(output.result.is_none());
            assert!(output.candidates.is_empty());
            assert!(output
                .next_steps
                .iter()
                .any(|step| step.contains("runs evidence")));
            homeboy::core::set_artifact_root_override(None);
        });
    }
}
