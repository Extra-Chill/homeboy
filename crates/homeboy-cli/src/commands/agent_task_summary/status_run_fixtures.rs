//! Regression reads of portable real-run directories from #16322.
use super::*;
use homeboy::agents::agent_task_lifecycle::{AgentTaskLifecycleStore, AgentTaskRunRecord};
use homeboy::agents::agent_tasks::AgentTaskAggregate;
use homeboy::agents::orchestration::{LifecycleStoreLookup, OrchestrationService};
use homeboy_control_plane_contract::{ControlPlaneRunListRequest, RunId};
use serde_json::json;

#[test]
fn agent_task_status_run_directories_preserve_recorded_cause_and_next_action() {
    crate::test_support::with_isolated_home(|_| {
        let store = AgentTaskLifecycleStore::from_current_environment().unwrap();
        let service = OrchestrationService::new(LifecycleStoreLookup::new(store.clone()));
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../homeboy-agents/tests/fixtures/cook-status");
        for (class, reason, action) in [
            ("provider-timeout", "timeout: spawnSync pi ETIMEDOUT (task exceeded timeout_ms=3600000)", "raise the provider timeout or retry with a different route"),
            ("harvest-failure", "agent_task.committed_harvest_git_failed: committed-change harvest failed while running verify Lab snapshot harvest provenance", "inspect and recover the harvested candidate and its Lab snapshot provenance before retrying; do not rerun the provider blindly"),
            ("missing-credential", "execution_failed: Pi exited with status 1. No API key found for amazon-bedrock.", "configure credentials for the resolved provider or select a credentialed route before retrying"),
        ] {
            let directory = root.join(class);
            let record: AgentTaskRunRecord = serde_json::from_slice(&std::fs::read(directory.join("record.json")).unwrap()).unwrap();
            let aggregate: AgentTaskAggregate = serde_json::from_slice(&std::fs::read(directory.join("aggregate.json")).unwrap()).unwrap();
            store.write_aggregate_and_record(&record, &aggregate).unwrap();
            assert!(store.aggregate_path(&record.run_id).exists());
            let run_id = RunId::new(&record.run_id).unwrap();
            let run = service.run(&run_id).unwrap();
            let verdict = crate::commands::cook_verdict::cook_verdict(&run);
            let mut payload = serde_json::to_value(&run).unwrap();
            payload["reason"] = serde_json::to_value(&verdict.reason).unwrap();
            let summary = render_agent_task_summary(AgentTaskSummaryKind::Status, &payload).unwrap();
            assert!(summary.contains("Status: failed\n"), "{class}: {summary}");
            assert!(summary.contains(&format!("Blocker: {reason}")), "{class}: {summary}");
            assert_eq!(summary.matches("Blocker:").count(), 1, "{summary}");
            assert!(!summary.contains("Reason:"), "{summary}");
            assert!(!summary.contains("Cook ended"), "{summary}");
            assert!(summary.contains(&format!("Next: {action}\n")), "{summary}");
            assert_eq!(verdict.next_action, action);
            assert!(verdict.reason.unwrap().starts_with(reason));
            let listed = service.runs(&ControlPlaneRunListRequest {limit:100, ..Default::default()}).unwrap()
                .runs.into_iter().find(|listed| listed.run == run_id).unwrap();
            assert_eq!(listed.blocker, run.blocker);
            let review = service.review(&run_id, &Default::default()).unwrap();
            assert_eq!(review.resource.blocker, run.blocker);
            // A free-standing cache must not override the atomic stored pair.
            std::fs::write(store.aggregate_path(&record.run_id), b"{}").unwrap();
            assert_eq!(service.run(&run_id).unwrap().blocker, run.blocker);
        }
    });
}

#[test]
fn agent_task_status_published_receipt_wins_over_terminal_and_promotion_snapshots() {
    crate::test_support::with_isolated_home(|_| {
        let store = AgentTaskLifecycleStore::from_current_environment().unwrap();
        let service = OrchestrationService::new(LifecycleStoreLookup::new(store.clone()));
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../homeboy-agents/tests/fixtures/cook-status/contradictory-publication");
        let template: Value =
            serde_json::from_slice(&std::fs::read(root.join("record.json")).unwrap()).unwrap();
        let aggregate: AgentTaskAggregate =
            serde_json::from_slice(&std::fs::read(root.join("aggregate.json")).unwrap()).unwrap();
        for status in ["review_ready", "draft_published"] {
            for promotion in ["applied", "verification_pending"] {
                let mut value = template.clone();
                value["metadata"]["cook_delivery"]["delivery"]["publication"]["status"] =
                    json!(status);
                value["metadata"]["cook_delivery"]["delivery"]["publication"]["draft"] =
                    json!(status == "draft_published");
                value["metadata"]["cook_delivery"]["delivery"]["candidate"]["promotion"] =
                    json!(promotion);
                let record: AgentTaskRunRecord = serde_json::from_value(value).unwrap();
                assert!(
                    homeboy::agents::agent_task_service::CookDelivery::from_run_metadata(
                        &record.metadata
                    )
                    .is_some(),
                    "invalid delivery: {:?}",
                    record.metadata
                );
                store
                    .write_aggregate_and_record(&record, &aggregate)
                    .unwrap();
                let before =
                    serde_json::to_value(store.read_record_bounded(&record.run_id).unwrap())
                        .unwrap();
                let run = service.run(&RunId::new(&record.run_id).unwrap()).unwrap();
                let verdict = crate::commands::cook_verdict::cook_verdict(&run);
                let payload = serde_json::to_value(&run).unwrap();
                let summary =
                    render_agent_task_summary(AgentTaskSummaryKind::Status, &payload).unwrap();
                assert!(summary.contains("Status: succeeded\n"), "{summary}");
                assert!(
                    summary.contains(&format!("Publication: {status}\n")),
                    "{summary}"
                );
                assert!(
                    summary.contains(
                        "PR: https://github.com/Extra-Chill/data-liberation-agent/pull/938\n"
                    ),
                    "{summary}"
                );
                assert!(
                    summary.contains("Next: review the published PR:"),
                    "{summary}"
                );
                assert_eq!(verdict.state, "succeeded");
                assert!(verdict.reason.is_none());
                assert!(run.blocker.is_none());
                assert!(!summary.contains("recover"));
                assert_eq!(
                    before,
                    serde_json::to_value(store.read_record_bounded(&record.run_id).unwrap())
                        .unwrap()
                );
            }
        }
    });
}
