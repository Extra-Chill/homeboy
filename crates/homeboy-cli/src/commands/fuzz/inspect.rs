use std::path::Path;

use homeboy::core::artifact_ref::{artifact_uri, EvidenceRef};
use homeboy::core::observation::{runs_service, ArtifactRecord, ObservationStore, RunRecord};
use homeboy::fuzz::inspect_fuzz_result_envelope_artifact;

use super::types::{FuzzInspectArgs, FuzzInspectCandidate, FuzzInspectOutput};
use super::types_extra::{FuzzDiagnosticSourceIdentity, FuzzFailureDiagnostic};
use homeboy::fuzz::fuzz_result_envelope_evidence_ref;

/// Artifact kinds that hold the raw fuzz runner input/result pair, ordered by
/// inspection preference. `fuzz_results` is the verbatim file a runner wrote to
/// `HOMEBOY_FUZZ_RESULTS_FILE`; `fuzz_result_envelope` is the normalized report
/// envelope persisted by `homeboy fuzz report`.
const RAW_FUZZ_RESULT_KINDS: &[&str] = &["fuzz_results", "fuzz_result_envelope"];

/// Implement `homeboy fuzz inspect <run-id>`.
///
/// Resolves the raw fuzz runner result for a run and emits a bounded diagnosis.
/// `--raw` and `--full` retain exact artifact access. Works against either the
/// `fuzz` run id or the Lab `runner-exec` run id that offloaded it, because
/// [`runs_service::list_artifacts_for_run`] already folds in downstream Lab job
/// artifacts that share the same `remote_job_id`.
pub(super) fn run_inspect(args: FuzzInspectArgs) -> homeboy::core::Result<FuzzInspectOutput> {
    let store = ObservationStore::open_initialized()?;
    let run = runs_service::require_run(&store, &args.run_id)?;
    let mut artifacts = runs_service::list_artifacts_for_run(&store, &run.id)?;
    artifacts.extend(runs_service::related_lab_artifacts_for_runner_job(
        &store, &run,
    )?);

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
            inspection_status: "not_found".to_string(),
            campaign_status: None,
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
            diagnostic: None,
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
            inspection_status: "unavailable".to_string(),
            campaign_status: None,
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
            diagnostic: None,
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

    let parsed = serde_json::from_slice::<serde_json::Value>(&bytes).ok();
    let (result, raw) = if args.raw {
        (None, Some(text.clone()))
    } else if args.full {
        (parsed.clone(), parsed.is_none().then_some(text.clone()))
    } else {
        (None, None)
    };
    let diagnostic_input = parsed
        .clone()
        .unwrap_or_else(|| serde_json::json!({ "error": bounded(&text, 600) }));

    let envelope_summary = inspect_fuzz_result_envelope_artifact(selected)
        .filter(|inspection| inspection.valid)
        .and_then(|inspection| inspection.summary);
    // The selected artifact can belong to a downstream Lab fuzz run rather than
    // the runner-exec run used for lookup. Its record is the diagnostic source.
    let diagnostic_run = runs_service::require_run(&store, &selected.run_id)?;

    Ok(FuzzInspectOutput {
        command: "fuzz.inspect".to_string(),
        inspection_status: "ok".to_string(),
        campaign_status: parsed.as_ref().and_then(campaign_status),
        run_id: args.run_id.clone(),
        source_run_id: selected.run_id.clone(),
        artifact_id: selected.id.clone(),
        artifact_kind: selected.kind.clone(),
        artifact_path: selected.path.clone(),
        canonical_ref: canonical_ref.clone(),
        evidence_ref: evidence_ref.clone(),
        fetch_command,
        result,
        raw,
        diagnostic: Some(fuzz_failure_diagnostic(
            &diagnostic_run,
            Some(&selected.run_id),
            &diagnostic_input,
            &[],
            canonical_ref
                .iter()
                .cloned()
                .chain(
                    evidence_ref
                        .iter()
                        .map(|reference| reference.canonical_uri().to_string()),
                )
                .collect(),
        )),
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

pub(super) fn fuzz_failure_diagnostic(
    run: &RunRecord,
    source_run_id: Option<&str>,
    result: &serde_json::Value,
    output: &[&str],
    mut evidence_refs: Vec<String>,
) -> FuzzFailureDiagnostic {
    let campaign = result.get("campaign").unwrap_or(result);
    let failed_case = campaign
        .get("cases")
        .and_then(serde_json::Value::as_array)
        .and_then(|cases| {
            cases
                .iter()
                .find(|case| {
                    matches!(
                        case.get("status").and_then(serde_json::Value::as_str),
                        Some("failed" | "error")
                    )
                })
                .or_else(|| cases.first())
        });
    let case_id = failed_case
        .and_then(|case| case.get("id").or_else(|| case.get("case_id")))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| find_string(result, &["case_id"]));
    let phase = find_string(result, &["phase", "phase_id"]);
    let mut causes = diagnostic_strings(result);
    causes.extend(
        output
            .iter()
            .filter(|value| !value.trim().is_empty())
            .map(|value| bounded(value, 600)),
    );
    causes.sort();
    causes.dedup();
    let joined = causes.join("\n");
    let classification = if joined.contains("ELOOP") {
        "pre_execution_assembly_failure"
    } else if joined.contains("PHP")
        && (joined.contains("Missing ") || joined.contains("exit code"))
    {
        "php_bootstrap_fatal"
    } else if causes.is_empty() {
        "failed_campaign"
    } else {
        "workload_execution_failure"
    }
    .to_string();
    causes.truncate(4);
    let executions = if classification == "pre_execution_assembly_failure" {
        0
    } else {
        find_u64(result, &["executions", "execution_count", "executionCount"])
            .unwrap_or_else(|| u64::from(case_id.is_some()))
    };
    if let Some(source_run_id) = source_run_id {
        evidence_refs.push(format!("homeboy://run/{source_run_id}"));
    }
    evidence_refs.sort();
    evidence_refs.dedup();
    let runtime = find_string(result, &["runtime", "runtime_id", "runtime_kind"]);
    FuzzFailureDiagnostic {
        run_id: run.id.clone(),
        rig_id: run
            .rig_id
            .clone()
            .or_else(|| find_string(result, &["rig_id"])),
        workload_id: find_string(result, &["workload_id"]),
        case_id,
        phase,
        classification,
        root_cause_chain: causes,
        executions,
        source_identity: FuzzDiagnosticSourceIdentity {
            component: run
                .component_id
                .clone()
                .or_else(|| find_string(result, &["component"])),
            homeboy_version: run.homeboy_version.clone(),
            git_sha: run.git_sha.clone(),
            runtime,
        },
        evidence_refs,
        inspect_command: format!("homeboy fuzz inspect {}", run.id),
    }
}

fn campaign_status(value: &serde_json::Value) -> Option<String> {
    value
        .get("campaign")
        .unwrap_or(value)
        .get("status")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

fn find_string(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
    match value {
        serde_json::Value::Object(object) => object.iter().find_map(|(key, value)| {
            (keys.contains(&key.as_str()))
                .then(|| value.as_str().map(str::to_string))
                .flatten()
                .or_else(|| find_string(value, keys))
        }),
        serde_json::Value::Array(values) => {
            values.iter().find_map(|value| find_string(value, keys))
        }
        _ => None,
    }
}

fn find_u64(value: &serde_json::Value, keys: &[&str]) -> Option<u64> {
    match value {
        serde_json::Value::Object(object) => object.iter().find_map(|(key, value)| {
            (keys.contains(&key.as_str()))
                .then(|| value.as_u64())
                .flatten()
                .or_else(|| find_u64(value, keys))
        }),
        serde_json::Value::Array(values) => values.iter().find_map(|value| find_u64(value, keys)),
        _ => None,
    }
}

fn diagnostic_strings(value: &serde_json::Value) -> Vec<String> {
    const KEYS: &[&str] = &[
        "error",
        "message",
        "failure_reason",
        "stderr",
        "stdout",
        "reason",
    ];
    fn visit(value: &serde_json::Value, key: Option<&str>, values: &mut Vec<String>) {
        if values.len() >= 12 {
            return;
        }
        match value {
            serde_json::Value::Object(object) => {
                for (key, value) in object {
                    visit(value, Some(key), values);
                }
            }
            serde_json::Value::Array(items) => {
                for value in items {
                    visit(value, key, values);
                }
            }
            serde_json::Value::String(value) if key.is_some_and(|key| KEYS.contains(&key)) => {
                values.push(bounded(value, 600))
            }
            _ => {}
        }
    }
    let mut values = Vec::new();
    visit(value, None, &mut values);
    values
}

fn bounded(value: &str, limit: usize) -> String {
    let mut result = value.chars().take(limit).collect::<String>();
    if value.chars().count() > limit {
        result.push_str("...[truncated]");
    }
    result
}

fn fuzz_inspect_evidence_ref(artifact: &ArtifactRecord) -> Option<EvidenceRef> {
    inspect_fuzz_result_envelope_artifact(artifact)
        .is_some()
        .then(|| fuzz_result_envelope_evidence_ref(artifact))
}

#[cfg(test)]
mod tests {
    use homeboy::core::observation::{NewRunRecord, ObservationStore, RunStatus};
    use homeboy::test_support::{with_isolated_home, ArtifactRootOverrideGuard};

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
            let _artifact_root =
                ArtifactRootOverrideGuard::new(home.path().join("agent-readable-artifacts"));
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
                full: true,
            })
            .expect("inspect");

            assert_eq!(output.inspection_status, "ok");
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
        });
    }

    #[test]
    fn inspect_resolves_raw_results_through_lab_runner_job() {
        with_isolated_home(|home| {
            let _artifact_root =
                ArtifactRootOverrideGuard::new(home.path().join("agent-readable-artifacts"));
            let store = ObservationStore::open_initialized().expect("store");
            let remote_job_id = "remote-job-inspect-5997";
            let runner_run = store
                .start_run(sample_run(
                    "runner-exec",
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
                full: true,
            })
            .expect("inspect");

            assert_eq!(output.inspection_status, "ok");
            assert_eq!(output.source_run_id, fuzz_run.id);
            assert_eq!(
                output
                    .diagnostic
                    .as_ref()
                    .map(|diagnostic| diagnostic.run_id.as_str()),
                Some(fuzz_run.id.as_str())
            );
            assert_eq!(
                output
                    .result
                    .as_ref()
                    .and_then(|v| v.pointer("/campaign/id"))
                    .and_then(|v| v.as_str()),
                Some("lab-raw")
            );
        });
    }

    #[test]
    fn inspect_raw_flag_returns_text_body() {
        with_isolated_home(|home| {
            let _artifact_root =
                ArtifactRootOverrideGuard::new(home.path().join("agent-readable-artifacts"));
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
                full: false,
            })
            .expect("inspect");

            assert_eq!(output.inspection_status, "ok");
            assert!(output.result.is_none());
            assert_eq!(output.raw.as_deref(), Some("{\"ok\":true}"));
        });
    }

    #[test]
    fn inspect_discovers_canonical_envelope_with_generic_artifact_kind() {
        with_isolated_home(|home| {
            let _artifact_root =
                ArtifactRootOverrideGuard::new(home.path().join("agent-readable-artifacts"));
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
                full: true,
            })
            .expect("inspect");

            assert_eq!(output.inspection_status, "ok");
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
        });
    }

    #[test]
    fn inspect_reports_not_found_without_raw_artifact() {
        with_isolated_home(|home| {
            let _artifact_root =
                ArtifactRootOverrideGuard::new(home.path().join("agent-readable-artifacts"));
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(sample_run("fuzz", serde_json::json!({})))
                .expect("run");

            let output = run_inspect(FuzzInspectArgs {
                run_id: run.id.clone(),
                raw: false,
                full: false,
            })
            .expect("inspect");

            assert_eq!(output.inspection_status, "not_found");
            assert!(output.result.is_none());
            assert!(output.candidates.is_empty());
            assert!(output
                .next_steps
                .iter()
                .any(|step| step.contains("runs evidence")));
        });
    }

    #[test]
    fn inspect_bounds_nested_codebox_failure_and_projects_statuses() {
        with_isolated_home(|home| {
            let _artifact_root =
                ArtifactRootOverrideGuard::new(home.path().join("agent-readable-artifacts"));
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(sample_run("fuzz", serde_json::json!({ "exit_code": 255 })))
                .expect("run");
            store
                .finish_run(&run.id, RunStatus::Fail, None)
                .expect("finish");
            let result = serde_json::json!({
                "campaign": {
                    "id": "codebox-campaign",
                    "status": "failed",
                    "cases": [{
                        "id": "case-17",
                        "status": "failed",
                        "workload_id": "wordpress.run-php",
                        "observed": {
                            "phase": "bootstrap",
                            "runtime_kind": "wp-codebox",
                            "stderr": "PHP.run() failed with exit code 255\\nMissing /wordpress/wp-content/plugins/jetpack/jetpack_vendor/automattic/jetpack-assets/actions.php",
                            "stdout": "x".repeat(700_000),
                            "generated": { "base64_payload": "y".repeat(700_000) }
                        }
                    }]
                }
            });
            let path = home.path().join("codebox-results.json");
            std::fs::write(&path, serde_json::to_vec(&result).unwrap()).expect("write");
            store
                .record_artifact(&run.id, "fuzz_results", &path)
                .expect("record");

            let output = run_inspect(FuzzInspectArgs {
                run_id: run.id.clone(),
                raw: false,
                full: false,
            })
            .expect("inspect");

            assert_eq!(output.inspection_status, "ok");
            assert_eq!(output.campaign_status.as_deref(), Some("failed"));
            assert!(output.result.is_none());
            assert!(output.raw.is_none());
            let diagnostic = output.diagnostic.expect("diagnostic");
            assert_eq!(diagnostic.classification, "php_bootstrap_fatal");
            assert_eq!(diagnostic.case_id.as_deref(), Some("case-17"));
            assert_eq!(diagnostic.phase.as_deref(), Some("bootstrap"));
            assert_eq!(diagnostic.executions, 1);
            assert!(diagnostic
                .root_cause_chain
                .join(" ")
                .contains("actions.php"));
            assert!(serde_json::to_vec(&diagnostic).unwrap().len() < 10_000);
        });
    }

    #[test]
    fn inspect_marks_eloop_as_pre_execution_with_zero_executions() {
        with_isolated_home(|home| {
            let _artifact_root =
                ArtifactRootOverrideGuard::new(home.path().join("agent-readable-artifacts"));
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(sample_run("fuzz", serde_json::json!({})))
                .expect("run");
            let path = home.path().join("eloop-results.json");
            std::fs::write(&path, br#"{"campaign":{"id":"assembly","status":"failed","metadata":{"error":"ELOOP: too many symbolic links while staging WP Codebox workload"}}}"#).expect("write");
            store
                .record_artifact(&run.id, "fuzz_results", &path)
                .expect("record");

            let output = run_inspect(FuzzInspectArgs {
                run_id: run.id,
                raw: false,
                full: false,
            })
            .expect("inspect");
            let diagnostic = output.diagnostic.expect("diagnostic");
            assert_eq!(diagnostic.classification, "pre_execution_assembly_failure");
            assert_eq!(diagnostic.executions, 0);
        });
    }
}
