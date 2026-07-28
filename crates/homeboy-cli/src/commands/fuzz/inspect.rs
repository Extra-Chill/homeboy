use std::path::Path;

use homeboy::core::artifact_ref::{artifact_uri, EvidenceRef};
use homeboy::core::observation::{runs_service, ArtifactRecord, ObservationStore, RunRecord};
use homeboy::fuzz::inspect_fuzz_result_envelope_artifact;

use super::types::{FuzzInspectArgs, FuzzInspectCandidate, FuzzInspectOutput};
use super::types_extra::{FuzzDiagnosticSourceIdentity, FuzzFailureDiagnostic, FuzzGateEvaluation};
use homeboy::fuzz::fuzz_result_envelope_evidence_ref;
use homeboy::fuzz::{
    classify_fuzz_failure, FuzzEvidenceContract, FuzzFailureDomain, FuzzFailureSignals,
};

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
    // Read-time reconstruction: prefer the classified contract the producer
    // persisted, and fall back to the pre-taxonomy `missing_artifact_refs` /
    // `results_error` members for runs recorded before it existed.
    let evidence_contract = FuzzEvidenceContract::from_run_metadata(&diagnostic_run.metadata_json);
    let recorded_gates = recorded_gate_evaluations(&diagnostic_run.metadata_json);

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
            &evidence_contract,
            &recorded_gates,
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

/// Build the compact fuzz failure diagnosis.
///
/// The `evidence` argument is what keeps a producer defect from being reported
/// as a discovery about the code under test. Before it existed, the only
/// inputs were the campaign and the runner's stdout/stderr, so a campaign
/// whose 19 cases all passed but whose declared `results.json` was absent got
/// diagnosed as `workload_execution_failure` against an unrelated passing case
/// id, with successful runner stdout at the head of the root-cause chain.
pub(super) fn fuzz_failure_diagnostic(
    run: &RunRecord,
    source_run_id: Option<&str>,
    result: &serde_json::Value,
    output: &[&str],
    mut evidence_refs: Vec<String>,
    evidence: &FuzzEvidenceContract,
    gates: &[FuzzGateEvaluation],
) -> FuzzFailureDiagnostic {
    let campaign = result.get("campaign").unwrap_or(result);
    let cases = campaign
        .get("cases")
        .and_then(serde_json::Value::as_array)
        .map(|cases| cases.as_slice())
        .unwrap_or_default();
    let failed_cases = cases
        .iter()
        .filter(|case| {
            matches!(
                case.get("status").and_then(serde_json::Value::as_str),
                Some("failed" | "error")
            )
        })
        .collect::<Vec<_>>();
    let failed_case_ids = failed_cases
        .iter()
        .filter_map(|case| {
            case.get("id")
                .or_else(|| case.get("case_id"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .collect::<Vec<_>>();
    let failed_gate_ids = gates
        .iter()
        .filter(|gate| gate.status != "passed")
        .map(|gate| gate.gate_id.clone())
        .collect::<Vec<_>>();
    let open_findings = campaign
        .get("findings")
        .and_then(serde_json::Value::as_array)
        .map(|findings| {
            findings
                .iter()
                .filter(|finding| {
                    finding.get("status").and_then(serde_json::Value::as_str) == Some("open")
                })
                .count() as u64
        })
        .unwrap_or(0);
    let classification_result = classify_fuzz_failure(&FuzzFailureSignals {
        evidence,
        failed_case_ids: &failed_case_ids,
        open_findings,
        failed_gate_ids: &failed_gate_ids,
        campaign_present: !campaign.is_null()
            && campaign
                .get("id")
                .and_then(serde_json::Value::as_str)
                .is_some(),
    });
    let is_evidence_failure =
        classification_result.domain == FuzzFailureDomain::EvidenceContractFailure;

    // A campaign-level evidence failure has no owning case. Naming a passing
    // one is how #10513 pointed at `sqlite-artifact-mask-1` for a run in which
    // every case passed.
    let case_id = if is_evidence_failure {
        None
    } else {
        failed_cases
            .first()
            .copied()
            .or_else(|| cases.first())
            .and_then(|case| case.get("id").or_else(|| case.get("case_id")))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .or_else(|| find_string(result, &["case_id"]))
    };
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
    let classification = if is_evidence_failure {
        FuzzFailureDomain::EvidenceContractFailure
            .as_str()
            .to_string()
    } else if joined.contains("ELOOP") {
        "pre_execution_assembly_failure".to_string()
    } else if joined.contains("PHP")
        && (joined.contains("Missing ") || joined.contains("exit code"))
    {
        "php_bootstrap_fatal".to_string()
    } else if causes.is_empty() {
        "failed_campaign".to_string()
    } else {
        "workload_execution_failure".to_string()
    };
    causes.truncate(4);
    // Evidence-contract root causes lead: they name the declared reference,
    // the base it was resolved against, and the producer that owed it. Runner
    // output stays available behind them instead of masquerading as the cause.
    let mut root_cause_chain = evidence.root_cause_lines();
    root_cause_chain.truncate(4);
    root_cause_chain.extend(causes);
    root_cause_chain.truncate(8);

    let executions = if classification == "pre_execution_assembly_failure" {
        0
    } else {
        find_u64(result, &["executions", "execution_count", "executionCount"]).unwrap_or_else(
            || {
                // For a campaign-level evidence failure there is no owning
                // case, but the campaign still says how much ran. Reporting
                // `1` there (the old `case_id.is_some()` fallback) understated
                // a 19-case campaign.
                if is_evidence_failure {
                    cases.len() as u64
                } else {
                    u64::from(case_id.is_some())
                }
            },
        )
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
        failure_domain: classification_result.domain.as_str().to_string(),
        workload_verdict: classification_result.workload_verdict.as_str().to_string(),
        evidence_verdict: classification_result.evidence_verdict.as_str().to_string(),
        evidence_violations: evidence.violations.clone(),
        root_cause_chain,
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

/// Recover the gate evaluations the producing run persisted.
///
/// `fuzz inspect` reads a run back off storage, so it cannot re-evaluate
/// gates; a gate failure has to come from the record. An entry without a
/// resolvable `gate_id` is dropped rather than counted as an anonymous
/// failure.
fn recorded_gate_evaluations(metadata: &serde_json::Value) -> Vec<FuzzGateEvaluation> {
    metadata
        .get("gates")
        .and_then(serde_json::Value::as_array)
        .map(|gates| {
            gates
                .iter()
                .filter_map(|gate| {
                    Some(FuzzGateEvaluation {
                        gate_id: gate
                            .get("gate_id")
                            .and_then(serde_json::Value::as_str)?
                            .to_string(),
                        status: gate
                            .get("status")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("unknown")
                            .to_string(),
                        metric: gate
                            .get("metric")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        observed: gate
                            .get("observed")
                            .and_then(serde_json::Value::as_f64)
                            .unwrap_or(0.0),
                        expected: gate
                            .get("expected")
                            .and_then(serde_json::Value::as_f64)
                            .unwrap_or(0.0),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
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

    /// #10513: a strict campaign whose cases all passed, whose gates all
    /// passed, and one of whose declared artifacts was never written.
    #[test]
    fn inspect_blames_the_evidence_contract_not_a_passing_case() {
        with_isolated_home(|home| {
            let _artifact_root =
                ArtifactRootOverrideGuard::new(home.path().join("agent-readable-artifacts"));
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(sample_run(
                    "fuzz",
                    serde_json::json!({
                        "exit_code": 1,
                        "success": false,
                        "campaign_id": "sqlite-artifact-mask",
                        "missing_artifact_refs": ["results.json"],
                        "results_error": "fuzz campaign references artifact path(s) missing from HOMEBOY_FUZZ_ARTIFACTS_DIR: results.json",
                        "gates": [
                            { "gate_id": "no-open-findings", "status": "passed", "metric": "open_findings", "observed": 0.0, "expected": 0.0 },
                            { "gate_id": "has-case-evidence", "status": "passed", "metric": "case_evidence", "observed": 1.0, "expected": 1.0 },
                            { "gate_id": "target-coverage-complete", "status": "passed", "metric": "target_coverage", "observed": 1.0, "expected": 1.0 },
                            { "gate_id": "operation-coverage-complete", "status": "passed", "metric": "operation_coverage", "observed": 1.0, "expected": 1.0 }
                        ]
                    }),
                ))
                .expect("run");
            store
                .finish_run(&run.id, RunStatus::Fail, None)
                .expect("finish");
            let result = serde_json::json!({
                "campaign": {
                    "id": "sqlite-artifact-mask",
                    "status": "passed",
                    "cases": [
                        { "id": "sqlite-artifact-mask-1", "status": "passed",
                          "observed": { "stdout": "imported 512 rows without error" } },
                        { "id": "sqlite-artifact-mask-2", "status": "passed" }
                    ],
                    "findings": []
                }
            });
            let path = home.path().join("passing-results.json");
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

            let diagnostic = output.diagnostic.expect("diagnostic");
            assert_eq!(diagnostic.classification, "evidence_contract_failure");
            assert_eq!(diagnostic.failure_domain, "evidence_contract_failure");
            // The workload verdict survives: every case passed.
            assert_eq!(diagnostic.workload_verdict, "passed");
            assert_eq!(diagnostic.evidence_verdict, "incomplete");
            // No unrelated case id is assigned to a campaign-level failure.
            assert!(diagnostic.case_id.is_none());
            // Root cause leads with the missing artifact, its resolution base,
            // and the owning producer contract — not with successful stdout.
            let leading = diagnostic
                .root_cause_chain
                .first()
                .expect("a root cause")
                .clone();
            assert!(leading.contains("artifact_ref_missing"), "{leading}");
            assert!(leading.contains("results.json"), "{leading}");
            assert!(
                !leading.contains("imported 512 rows"),
                "successful runner stdout must not lead: {leading}"
            );
            assert_eq!(diagnostic.evidence_violations.len(), 1);
            assert_eq!(
                diagnostic.evidence_violations[0].declared_ref.as_deref(),
                Some("results.json")
            );
        });
    }

    #[test]
    fn inspect_still_reports_a_workload_failure_when_a_case_actually_failed() {
        with_isolated_home(|home| {
            let _artifact_root =
                ArtifactRootOverrideGuard::new(home.path().join("agent-readable-artifacts"));
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(sample_run(
                    "fuzz",
                    serde_json::json!({ "exit_code": 1, "campaign_id": "mixed" }),
                ))
                .expect("run");
            store
                .finish_run(&run.id, RunStatus::Fail, None)
                .expect("finish");
            let result = serde_json::json!({
                "campaign": {
                    "id": "mixed",
                    "status": "failed",
                    "cases": [
                        { "id": "case-1", "status": "passed" },
                        { "id": "case-2", "status": "failed",
                          "observed": { "stderr": "assertion failed" } }
                    ]
                }
            });
            let path = home.path().join("mixed-results.json");
            std::fs::write(&path, serde_json::to_vec(&result).unwrap()).expect("write");
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
            assert_eq!(diagnostic.classification, "workload_execution_failure");
            assert_eq!(diagnostic.failure_domain, "workload_failure");
            assert_eq!(diagnostic.workload_verdict, "failed");
            assert_eq!(diagnostic.evidence_verdict, "complete");
            // The failing case is named, not the first case in the list.
            assert_eq!(diagnostic.case_id.as_deref(), Some("case-2"));
        });
    }

    #[test]
    fn inspect_reports_a_gate_failure_separately_from_a_workload_failure() {
        with_isolated_home(|home| {
            let _artifact_root =
                ArtifactRootOverrideGuard::new(home.path().join("agent-readable-artifacts"));
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(sample_run(
                    "fuzz",
                    serde_json::json!({
                        "exit_code": 1,
                        "campaign_id": "gated",
                        "gates": [
                            { "gate_id": "p95-budget", "status": "failed", "metric": "p95_ms", "observed": 900.0, "expected": 500.0 }
                        ]
                    }),
                ))
                .expect("run");
            store
                .finish_run(&run.id, RunStatus::Fail, None)
                .expect("finish");
            let path = home.path().join("gated-results.json");
            std::fs::write(
                &path,
                br#"{"campaign":{"id":"gated","status":"passed","cases":[{"id":"c1","status":"passed"}]}}"#,
            )
            .expect("write");
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
            assert_eq!(diagnostic.failure_domain, "gate_failure");
            assert_eq!(diagnostic.workload_verdict, "passed");
            assert_eq!(diagnostic.evidence_verdict, "complete");
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
