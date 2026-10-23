use super::*;
use serde_json::json;

fn fixture(class: &str) -> RunSnapshot {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/cook-status")
        .join(class);
    RunSnapshot {
        record: serde_json::from_slice(&std::fs::read(directory.join("record.json")).unwrap())
            .unwrap(),
        aggregate: Some(
            serde_json::from_slice(&std::fs::read(directory.join("aggregate.json")).unwrap())
                .unwrap(),
        ),
        plan: None,
    }
}

#[test]
fn status_recorded_failure_preserves_first_diagnostic_and_classification() {
    for (class, code, message) in [
        (
            "provider-timeout",
            "outcome.timeout",
            "spawnSync pi ETIMEDOUT",
        ),
        (
            "harvest-failure",
            "outcome.agent_task.committed_harvest_git_failed",
            "verify Lab snapshot harvest provenance",
        ),
        (
            "missing-credential",
            "outcome.execution_failed",
            "No API key found for amazon-bedrock",
        ),
    ] {
        let snapshot = fixture(class);
        let resource = project_snapshot(&snapshot, false).unwrap();
        let blocker = resource.blocker.unwrap();
        assert_eq!(blocker.code.as_deref(), Some(code));
        assert!(blocker.message.contains(message), "{blocker:?}");
        assert!(blocker.reason.is_some());
    }
}

#[test]
fn status_recorded_failure_handles_stalled_cancelled_and_recoverable_without_losing_cause() {
    let mut snapshot = fixture("provider-timeout");
    for (status, classification, action) in [
        ("failed", "stalled", "different provider route"),
        ("failed", "provider", "different available route"),
        ("cancelled", "execution_failed", "before retrying"),
        (
            "candidate_recoverable",
            "timeout",
            "recover the harvested candidate",
        ),
    ] {
        let outcome = &mut snapshot.aggregate.as_mut().unwrap().outcomes[0];
        let mut value = serde_json::to_value(&outcome).unwrap();
        value["status"] = json!(status);
        value["failure_classification"] = json!(classification);
        *outcome = serde_json::from_value(value).unwrap();
        let blocker = project_snapshot(&snapshot, false).unwrap().blocker.unwrap();
        assert!(blocker.message.starts_with(classification), "{blocker:?}");
        assert!(blocker.message.contains("spawnSync pi ETIMEDOUT"));
        assert!(blocker.reason.unwrap().contains(action));
    }
}

#[test]
fn status_current_delivery_blocker_outranks_provider_diagnostic_and_success_has_none() {
    let mut snapshot = fixture("provider-timeout");
    crate::agent_task_service::edit_cook_delivery_for_test(
        &mut snapshot.record.metadata,
        |artifacts| {
            artifacts.promotion = Some(
                json!({"status":"gate_failed", "deterministic_gates":[{"id":"check", "status":"failed"}]}),
            );
        },
    );
    assert_eq!(
        project_snapshot(&snapshot, false)
            .unwrap()
            .blocker
            .unwrap()
            .code
            .as_deref(),
        Some("gate_failed")
    );
    let published = fixture("contradictory-publication");
    let resource = project_snapshot(&published, false).unwrap();
    assert_eq!(resource.state, ControlPlaneRunState::Succeeded);
    assert!(resource.blocker.is_none());
    assert_eq!(resource.publication.unwrap().state, "review_ready");
}

#[test]
fn status_recorded_failure_redacts_secrets_and_uses_summary_when_no_diagnostic_exists() {
    let mut snapshot = fixture("provider-timeout");
    let outcome = &mut snapshot.aggregate.as_mut().unwrap().outcomes[0];
    outcome.diagnostics.clear();
    outcome.summary = Some("provider failed token=super-secret".to_string());
    let blocker = project_snapshot(&snapshot, false).unwrap().blocker.unwrap();
    assert!(blocker.message.starts_with("timeout: provider failed"));
    assert!(!blocker.message.contains("super-secret"));
    snapshot.aggregate = None;
    assert!(project_snapshot(&snapshot, false)
        .unwrap()
        .blocker
        .is_none());
}
