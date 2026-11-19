#[test]
fn subsystem_events_are_keyed_and_replay_safe() {
    for (event, phase) in [
        (CookRunEvent::LabHandoffAccepted, P::Running),
        (CookRunEvent::LabHandoffExpired, P::Cancelled),
        (CookRunEvent::LabHandoffRejected, P::Failed),
        (CookRunEvent::DeliveryRecorded { phase: P::Succeeded }, P::Succeeded),
        (CookRunEvent::DeliveryRecorded { phase: P::CandidateRecoverable }, P::CandidateRecoverable),
    ] {
        let next = state(P::Running, false, false).apply(event, "receipt").unwrap();
        assert_eq!(next.phase, phase);
        assert_eq!(next.apply(event, "receipt").unwrap(), next);
    }
}

#[test]
fn subsystem_classification_stays_in_owners() {
    fn scan(path: &std::path::Path) {
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy();
            if name.contains("test") || name.contains("fixture") { continue; }
            if path.is_dir() { scan(&path); continue; }
            if path.extension().is_none_or(|extension| extension != "rs") { continue; }
            // Wire validation and fact adapters are the subsystem owners.
            if matches!(name.as_ref(), "records.rs" | "cook_run_state.rs" | "lab_offload.rs" | "lab_handoff_reconciliation.rs" | "terminal_transition.rs" | "lifecycle_ops.rs" | "cook_delivery.rs") { continue; }
            let source = std::fs::read_to_string(&path).unwrap();
            let production = source.split("#[cfg(test)]").next().unwrap();
            assert!(!production.contains("AgentTaskLabHandoffState::"), "{}", path.display());
            assert!(!production.contains("CookDeliveryState::"), "{}", path.display());
        }
    }
    scan(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"));
    for source in [
        include_str!("../agent_task_service/cook.rs"),
        include_str!("../agent_task_service/cook_recipe.rs"),
        include_str!("../agent_task_submission_service.rs"),
        include_str!("lifecycle_candidate_adoption.rs"),
        include_str!("runner_exec.rs"),
        include_str!("private_attachment.rs"),
        include_str!("lifecycle_transport_proxy.rs"),
    ] {
        let production = source.split("#[cfg(test)]").next().unwrap();
        assert!(!production.contains("AgentTaskLabHandoffState::"));
        assert!(!production.contains("CookDeliveryState::"));
    }
}

#[test]
fn subsystem_receipts_drive_fenced_record_transitions() {
    use super::super::{AgentTaskLabHandoff, AgentTaskLabHandoffState, AgentTaskLabHandoffAuthority};
    for (wire_state, execution, phase, expected) in [
        (AgentTaskLabHandoffState::Accepted, RunExecutionState::Running, P::Running, CookRunEvent::LabHandoffAccepted),
        (AgentTaskLabHandoffState::Expired, RunExecutionState::Cancelled, P::Cancelled, CookRunEvent::LabHandoffExpired),
        (AgentTaskLabHandoffState::Pending, RunExecutionState::Failed, P::Failed, CookRunEvent::LabHandoffRejected),
    ] {
        let stored = record(RunExecutionState::Running, serde_json::json!({}));
        let mut next = stored.clone();
        next.set_state_for_test(execution);
        next.lab_handoff = Some(AgentTaskLabHandoff {
            state: wire_state,
            authority: if wire_state == AgentTaskLabHandoffState::Accepted { AgentTaskLabHandoffAuthority::RunnerDaemon } else { AgentTaskLabHandoffAuthority::Controller },
            runner_id: "lab".into(), runner_job_id: (wire_state == AgentTaskLabHandoffState::Accepted).then(|| "job".into()),
            ..serde_json::from_value(serde_json::json!({"state":"pending","authority":"controller","runner_id":"lab"})).unwrap()
        });
        // Use the wire fixture's real validity rules, including acceptance timestamps.
        next.lab_handoff.as_mut().unwrap().accepted_at = Some("2026-06-01T00:00:00Z".into());
        transition_record(&mut next, Some(&stored)).unwrap();
        assert_eq!(CookRunState::of(&next).phase, phase);
        assert_eq!(next.persisted_cook_state().unwrap().applied_events.last().unwrap().1, expected);
        let committed = next.clone();
        transition_record(&mut next, Some(&committed)).unwrap();
        assert_eq!(next.persisted_cook_state(), committed.persisted_cook_state());
    }
}

#[test]
fn delivery_receipts_persist_keyed_outcomes_without_caching_evidence() {
    for execution in [RunExecutionState::Succeeded, RunExecutionState::CandidateRecoverable] {
        let stored = record(RunExecutionState::Running, serde_json::json!({}));
        let mut next = stored.clone();
        next.set_state_for_test(execution);
        next.metadata["cook_delivery"] = serde_json::json!({"receipt": "delivery"});
        transition_record(&mut next, Some(&stored)).unwrap();
        let phase = P::of_execution(execution);
        assert_eq!(CookRunState::of(&next).phase, phase);
        let persisted = next.persisted_cook_state().unwrap();
        let (key, event) = persisted.applied_events.last().unwrap();
        assert_eq!(*event, CookRunEvent::DeliveryRecorded { phase });
        let state = CookRunState::of(&next);
        assert_eq!(state.apply(*event, key).unwrap(), state);
        let committed = next.clone();
        transition_record(&mut next, Some(&committed)).unwrap();
        assert_eq!(next.persisted_cook_state(), committed.persisted_cook_state());
    }
}

#[test]
fn pre_transition_record_loads_without_cook_state() {
    let legacy = record(RunExecutionState::Running, serde_json::json!({}));
    let mut fixture = serde_json::to_value(&legacy).unwrap();
    fixture["lab_handoff"] = serde_json::json!({
        "state": "accepted", "authority": "runner_daemon", "runner_id": "lab",
        "runner_job_id": "job", "accepted_at": "2026-06-01T00:00:00Z",
        "workspace_lifecycle_revision": 0
    });
    let loaded: AgentTaskRunRecord = serde_json::from_value(fixture.clone()).unwrap();
    assert!(loaded.persisted_cook_state().is_none());
    assert_eq!(serde_json::to_value(&loaded).unwrap()["lab_handoff"], fixture["lab_handoff"]);
    assert_eq!(CookRunState::of(&loaded).phase, P::Running);
}
