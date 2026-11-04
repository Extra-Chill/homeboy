use super::*;

#[test]
fn queued_approval_is_owned_by_outbox_and_route_reaches_steps() {
    with_isolated_home(|home| {
        let mut manifest: homeboy_extension_contract::ExtensionManifest = serde_json::from_value(json!({
            "name": "Offline transport", "version": "1.0.0",
            "notification_transports": [{
                "schema": homeboy_extension_contract::notification_transport_config::NOTIFICATION_TRANSPORT_SCHEMA,
                "id": "offline.test", "command": ["false"]
            }]
        })).unwrap();
        manifest.id = "offline-test".into();
        crate::extension::catalog::save_manifest(&manifest).unwrap();
        let mut request = request(
            json!({
                "schema": "homeboy/workflow/v1", "id": "queued-approval",
                "steps": [
                    {"id": "prepare", "exec": sh("true")},
                    {"id": "hold", "needs": ["prepare"], "approval": {"prompt": "Review {{steps.prepare.output}}"}}
                ]
            }),
            home.path(),
        );
        request.notification_route = Some(
            crate::notification_route::NotificationRoute::new("offline.test", "thread").unwrap(),
        );
        let initial = request.initial_state();
        let (running, commands) = advance(&request.definition, &initial, &Event::Start, 0);
        let submission = commands
            .iter()
            .find_map(|c| match c {
                Command::SubmitStep(s) => Some(s),
                _ => None,
            })
            .unwrap();
        let step = step_request(&request, submission).unwrap();
        let encoded = serde_json::to_value(step).unwrap();
        assert!(encoded.to_string().contains("HOMEBOY_NOTIFICATION_ROUTE"));
        assert!(encoded.to_string().contains("offline.test"));
        let (waiting, _) = advance(
            &request.definition,
            &running,
            &Event::StepFinished(StepResult {
                key: submission.key.clone(),
                status: FinishedStatus::Succeeded,
                output: Some(json!({"report": "ready"})),
                outcome: None,
                run_id: None,
                error: None,
            }),
            1,
        );
        assert_eq!(waiting.steps["hold"].phase, StepPhase::AwaitingApproval);
        crate::daemon::workflow_notify::awaiting_approvals(&request, &waiting);
        crate::daemon::workflow_notify::awaiting_approvals(&request, &waiting);
        let pending = crate::notify_outbox::pending_entries();
        assert_eq!(pending.len(), 1);
        let encoded = serde_json::to_string(&pending[0]).unwrap();
        assert!(encoded.contains("prepare output"));
        assert!(encoded.contains("file://"));
        assert!(encoded.contains("workflow-controller"));
        assert!(encoded.contains("ready"));
    });
}
