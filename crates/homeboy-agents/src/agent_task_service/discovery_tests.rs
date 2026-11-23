use super::*;
use homeboy_core::test_support::with_isolated_home;
use serde_json::json;

fn record(id: &str, state: &str, submitted_at: &str, metadata: Value) -> AgentTaskRunRecord {
    let mut record: AgentTaskRunRecord = serde_json::from_value(json!({
        "schema": "homeboy/agent-task-run/v1",
        "run_id": id,
        "plan_id": "plan",
        "state": state,
        "submitted_at": submitted_at,
        "plan_path": "plan.json",
        "metadata": metadata,
    }))
    .expect("record");
    let state = match state {
        "queued" => homeboy_lifecycle_contract::RunExecutionState::Queued,
        "running" => homeboy_lifecycle_contract::RunExecutionState::Running,
        "succeeded" => homeboy_lifecycle_contract::RunExecutionState::Succeeded,
        "cancelled" => homeboy_lifecycle_contract::RunExecutionState::Cancelled,
        _ => unreachable!(),
    };
    agent_task_lifecycle::set_run_state(&mut record, state);
    record
}

#[test]
fn mission_discovery_uses_status_attempt_outcome() {
    with_isolated_home(|_| {
        let store = agent_task_lifecycle::AgentTaskLifecycleStore::from_environment().unwrap();
        for state in ["failed", "running", "succeeded"] {
            let mission = format!("mission-{state}");
            let attempt = format!("{mission}-attempt-1");
            store
                .write_record(&record(
                    &mission,
                    "succeeded",
                    "2026-01-01T00:00:00Z",
                    json!({}),
                ))
                .unwrap();
            let mut selected = record(&attempt, "running", "2026-01-01T00:00:00Z", json!({}));
            let expected = match state {
                "failed" => homeboy_lifecycle_contract::RunExecutionState::Failed,
                "running" => homeboy_lifecycle_contract::RunExecutionState::Running,
                _ => homeboy_lifecycle_contract::RunExecutionState::Succeeded,
            };
            agent_task_lifecycle::set_run_state(&mut selected, expected);
            store.write_record(&selected).unwrap();
            if state == "succeeded" {
                agent_task_lifecycle::record_cook_finalization_in_store(
                    &store,
                    &attempt,
                    json!({"status":"review_ready", "pr_url":"https://github.com/example/repo/pull/1"}),
                )
                .unwrap();
            }
            store.record_cook_attempt(&mission, 1, &attempt).unwrap();
            let page = discover_runs_page(
                AgentTaskDiscoveryFilter::All,
                AgentTaskDiscoveryPageOptions {
                    limit: 100,
                    ..Default::default()
                },
            )
            .unwrap();
            let root = page.runs.iter().find(|row| row.run_id == mission).unwrap();
            assert_eq!(
                root.state,
                agent_task_lifecycle::status_in_store(&store, &mission)
                    .unwrap()
                    .state()
            );
            assert_eq!(root.state, expected);
        }
    });
}

#[test]
fn terminal_transition_settles_cook_observations_and_span() {
    with_isolated_home(|_| {
        let store = agent_task_lifecycle::AgentTaskLifecycleStore::from_environment().unwrap();
        // Runs settle to their own Cook-observed lifecycle outcome, never to
        // the report's exit code (#18366). Each case seeds the lifecycle state
        // its Cook actually ends in.
        for (status, lifecycle, success, row_status) in [
            // A verified intentional no-change keeps its provider success.
            ("intentional_no_change", "succeeded", true, "pass"),
            // Provider success without a verified, finalized candidate.
            ("candidate_recoverable", "succeeded", false, "fail"),
            // Publication is the end-to-end receipt.
            ("published", "running", true, "pass"),
            // A cancelled run reads `skipped` on every row (#18868).
            ("cancelled", "cancelled", false, "skipped"),
        ] {
            let cook = format!("terminal-{status}");
            let attempt = format!("{cook}-attempt-1");
            // A Cook-owned run: its driver, not record projection, settles it.
            std::fs::create_dir_all(store.cook_dir(&cook)).unwrap();
            std::fs::write(store.cook_dir(&cook).join("recipe.json"), "{}").unwrap();
            for id in [&cook, &attempt] {
                store
                    .write_record(&record(
                        id,
                        lifecycle,
                        "2026-01-01T00:00:00Z",
                        json!({"cook_id":cook, "cook_progress":{"phase":"terminal"}}),
                    ))
                    .unwrap();
            }
            store.record_cook_attempt(&cook, 1, &attempt).unwrap();
            if status == "published" {
                for id in [&cook, &attempt] {
                    agent_task_lifecycle::record_cook_finalization_in_store(
                        &store,
                        id,
                        json!({"status":"review_ready", "pr_url":"https://github.com/example/repo/pull/1"}),
                    )
                    .unwrap();
                }
            }
            let observation = store.open_observation_initialized().unwrap();
            for id in [&cook, &attempt] {
                // Cook-owned rows wait for the Cook terminal.
                assert_eq!(observation.get_run(id).unwrap().unwrap().status, "running");
            }
            agent_task_lifecycle::record_cook_terminal_result_in_store(
                &store,
                &attempt,
                status,
                i32::from(!success),
            )
            .unwrap();
            let observation = store.open_observation_initialized().unwrap();
            for id in [&cook, &attempt] {
                assert_eq!(
                    observation.get_run(id).unwrap().unwrap().status,
                    row_status,
                    "{id}"
                );
            }
            let spans = observation.phase_spans(&cook).unwrap();
            let spans: Vec<_> = spans
                .spans
                .iter()
                .filter(|span| span.id == "cook")
                .collect();
            assert_eq!(spans.len(), 1);
            assert_eq!(
                spans[0].status == homeboy_engine_primitives::phase_timing::PhaseStatus::Failed,
                !success
            );
        }
    });
}

#[test]
fn relative_since_cursor_keeps_the_original_bound_and_scope() {
    with_isolated_home(|_| {
        let store = agent_task_lifecycle::AgentTaskLifecycleStore::from_environment().unwrap();
        let now = chrono::Utc::now();
        for (id, offset) in [("newer", 1), ("older", 2)] {
            store
                .write_record(&record(
                    id,
                    "running",
                    &(now - chrono::Duration::seconds(offset)).to_rfc3339(),
                    json!({}),
                ))
                .unwrap();
        }
        let options = AgentTaskDiscoveryPageOptions {
            limit: 1,
            submitted_after: Some("10d".into()),
            ..Default::default()
        };
        let first = discover_runs_page(AgentTaskDiscoveryFilter::All, options.clone()).unwrap();
        assert_eq!(first.runs[0].run_id, "newer");
        std::thread::sleep(std::time::Duration::from_millis(2));
        let continued = AgentTaskDiscoveryPageOptions {
            cursor: first.next_cursor,
            ..options
        };
        let second = discover_runs_page(AgentTaskDiscoveryFilter::All, continued.clone()).unwrap();
        assert_eq!(second.runs[0].run_id, "older");
        for changed in ["9d", "2020-01-01T00:00:00Z"] {
            assert!(discover_runs_page(
                AgentTaskDiscoveryFilter::All,
                AgentTaskDiscoveryPageOptions {
                    submitted_after: Some(changed.into()),
                    ..continued.clone()
                }
            )
            .is_err());
        }
    });
}

#[test]
fn admission_loads_terminal_cook_parent_outside_active_set() {
    with_isolated_home(|_| {
        let store = agent_task_lifecycle::AgentTaskLifecycleStore::from_environment().unwrap();
        store
            .write_record(&record(
                "parent",
                "succeeded",
                "2000-01-01T00:00:00Z",
                json!({}),
            ))
            .unwrap();
        store
            .write_record(&record(
                "attempt",
                "running",
                "2000-01-01T00:00:00Z",
                json!({
                    "cook_id": "parent", "runner_id": "lab", "runner_job_id": "old-job"
                }),
            ))
            .unwrap();
        let (active, _) = store.read_active_records().unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].run_id, "attempt");

        let admission = AgentTaskControllerUpgradeAdmissionProvider
            .controller_upgrade_admission()
            .unwrap();
        assert!(admission.blockers.is_empty(), "{:?}", admission.blockers);
    });
}

#[test]
fn admission_blocks_nonterminal_record_older_than_display_window() {
    with_isolated_home(|_| {
        let store = agent_task_lifecycle::AgentTaskLifecycleStore::from_environment().unwrap();
        store
            .write_record(&record(
                "old-active",
                "running",
                "2000-01-01T00:00:00Z",
                json!({"runner_id": "lab", "runner_job_id": "old-job"}),
            ))
            .unwrap();
        for index in 0..1001 {
            store
                .write_record(&record(
                    &format!("terminal-{index}"),
                    "succeeded",
                    "2026-01-01T00:00:00Z",
                    json!({}),
                ))
                .unwrap();
        }
        let (bounded, _) = store.read_records_with_health_bounded(1000).unwrap();
        assert!(!bounded.iter().any(|record| record.run_id == "old-active"));

        let admission = AgentTaskControllerUpgradeAdmissionProvider
            .controller_upgrade_admission()
            .unwrap();
        assert_eq!(admission.blockers.len(), 1);
        assert_eq!(admission.blockers[0].run_id, "old-active");
    });
}
