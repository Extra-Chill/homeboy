//! Read the durable Lab receipt without requiring an agent-task snapshot.
use homeboy::core::observation::ObservationStore;
use serde_json::Value;

pub(crate) fn report(store: &ObservationStore, run_id: &str) -> Option<Value> {
    let artifacts = store.list_artifacts(run_id).ok()?;
    artifacts.iter().rev().find_map(|artifact| {
        if artifact.kind != "admitted-lab-gate-receipt"
            || artifact
                .metadata_json
                .get("visibility")
                .and_then(Value::as_str)
                == Some("private")
        {
            return None;
        }
        let bytes = std::fs::read(&artifact.path).ok()?;
        let receipt: Value = serde_json::from_slice(&bytes).ok()?;
        let mut report = receipt.get("report")?.clone();
        report["lab_receipt"] = serde_json::json!({
            "artifact_ref": format!("homeboy://run/{run_id}/artifact/{}", artifact.id)
        });
        Some(homeboy::core::redaction::redact_json(&report))
    })
}

pub(crate) fn excerpt(report: &Value) -> Option<String> {
    homeboy::agents::agent_task_service::gate_failure_excerpt_from_value(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn real_lab_gate_receipt_reaches_logs_and_failure_only_evidence() {
        homeboy::test_support::with_isolated_home(|home| {
            let _xdg = homeboy_core::test_support::EnvVarGuard::unset("XDG_DATA_HOME");
            let store = ObservationStore::open_initialized().unwrap();
            let run = store
                .start_run(homeboy::core::observation::NewRunRecord::builder("runner-exec").build())
                .unwrap();
            let output = std::process::Command::new("sh").args([
                "-c", "printf 'FAIL missing fixture\\n'; printf 'Error: ENOENT fixture\\n' >&2; exit 7"
            ]).output().unwrap();
            let receipt = json!({"report": {
                "id": "fixture", "status": "failed", "exit_code": output.status.code(),
                "stdout": String::from_utf8_lossy(&output.stdout),
                "stderr": String::from_utf8_lossy(&output.stderr)
            }});
            let path = home.path().join("receipt.json");
            std::fs::write(&path, receipt.to_string()).unwrap();
            store
                .record_artifact_with_metadata(
                    &run.id,
                    "admitted-lab-gate-receipt",
                    &path,
                    json!({"visibility":"public"}),
                )
                .unwrap();
            let (logs, _) = super::super::logs(super::super::RunsLogsArgs {
                run_id: run.id.clone(),
                cursor: None,
            })
            .unwrap();
            assert!(logs["stdout"]
                .as_str()
                .unwrap()
                .contains("FAIL missing fixture"));
            assert!(logs["stderr"].as_str().unwrap().contains("ENOENT"));
            let (evidence, _) = super::super::evidence::evidence_from_args(
                &store,
                super::super::cook::EvidenceArgs {
                    run_id: run.id,
                    kind: None,
                    task: None,
                    failure_only: true,
                    full: false,
                },
            )
            .unwrap();
            let evidence = serde_json::to_value(evidence).unwrap().to_string();
            assert!(evidence.contains("FAIL missing fixture"), "{evidence}");
            assert!(evidence.contains("Error: ENOENT fixture"));
            assert!(evidence.contains("exit code: 7"));
        });
    }
}
