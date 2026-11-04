use super::*;

#[test]
fn approval_notification_survives_replay_and_approval() {
    with_isolated_home(|home| {
        let delivered = home.path().join("delivered.jsonl");
        let script = home.path().join("transport.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$HOMEBOY_NOTIFY_PAYLOAD\" >> '{}'\n",
                delivered.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut manifest: homeboy_extension_contract::ExtensionManifest = serde_json::from_value(json!({
            "name": "Approval transport", "version": "1.0.0",
            "notification_transports": [{
                "schema": homeboy_extension_contract::notification_transport_config::NOTIFICATION_TRANSPORT_SCHEMA,
                "id": "approval.test", "command": [script.display().to_string()]
            }]
        })).unwrap();
        manifest.id = "approval-test".into();
        crate::extension::catalog::save_manifest(&manifest).unwrap();
        let store = JobStore::open_without_reconciliation(
            home.path().join(crate::paths::OBSERVATION_DB_FILENAME),
        )
        .unwrap()
        .with_daemon_lease("approval-notify-test".into());
        let mut request = request(
            json!({
                "schema": "homeboy/workflow/v1", "id": "approval-notify",
                "inputs": {"message": {"default": "Ship this change?"}},
                "steps": [{"id": "approve", "approval": {"prompt": "{{inputs.message}}"}}]
            }),
            home.path(),
        );
        request.notification_route = Some(
            crate::notification_route::NotificationRoute::new("approval.test", "thread").unwrap(),
        );
        let job_id = admit(&store, &request);
        crate::daemon::start_controller_job(job_id, &store).unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while !delivered.exists()
            || std::fs::read_to_string(&delivered)
                .unwrap_or_default()
                .is_empty()
        {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(50));
        }
        let checkpoint = store
            .controller_job_state(job_id)
            .unwrap()
            .checkpoint
            .unwrap();
        let recovered: InstanceState = serde_json::from_value(checkpoint).unwrap();
        // Recovery reconciles the durable checkpoint; status remains read-only.
        crate::daemon::workflow_notify::awaiting_approvals(&request, &recovered);
        workflow_status(&store, &request.instance).unwrap();
        workflow_approve(&store, &request.instance, Some(json!({"step": "approve"}))).unwrap();
        assert_eq!(
            wait_terminal(&store, job_id, 15).status,
            JobStatus::Succeeded
        );
        crate::daemon::workflow_notify::awaiting_approvals(&request, &recovered);
        let text = std::fs::read_to_string(delivered).unwrap();
        assert_eq!(text.lines().count(), 1);
        let payload: Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(payload["subject"]["phase"], "awaiting_approval");
        assert!(text.contains("Ship this change?"));
        assert_eq!(
            payload["actions"][0]["command"],
            format!("homeboy workflow approve {} approve", request.instance)
        );
    });
}
