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

/// A stopped chain announces itself once through the run-completion notifier,
/// naming the workflow, failed step, attempts, first error line and `runs show`
/// command; a succeeding chain stays silent (#18540).
#[test]
fn a_stopped_chain_notifies_once_and_a_successful_one_never() {
    for (exit, expected) in [(3, 1), (0, 0)] {
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
                "name": "Chain transport", "version": "1.0.0",
                "notification_transports": [{
                    "schema": homeboy_extension_contract::notification_transport_config::NOTIFICATION_TRANSPORT_SCHEMA,
                    "id": "chain.test", "command": [script.display().to_string()]
                }]
            })).unwrap();
            manifest.id = "chain-test".into();
            crate::extension::catalog::save_manifest(&manifest).unwrap();
            let store = JobStore::open_without_reconciliation(
                home.path().join(crate::paths::OBSERVATION_DB_FILENAME),
            )
            .unwrap()
            .with_daemon_lease("chain-notify-test".into());
            let release = home.path().join("release");
            let mut request = request(
                json!({
                    "schema": "homeboy/workflow/v1", "id": "chain-stop",
                    "steps": [{"id": "import", "exec": sh(&format!(
                        "while [ ! -f '{}' ]; do sleep 0.05; done; echo 'import broke\nsecond line' >&2; exit {exit}",
                        release.display()
                    ))}]
                }),
                home.path(),
            );
            request.notification_route = Some(
                crate::notification_route::NotificationRoute::new("chain.test", "thread").unwrap(),
            );
            let job_id = admit(&store, &request);
            crate::daemon::start_controller_job(job_id, &store).unwrap();

            // The notifier announces runs it observed in flight.
            let mut tracker = crate::daemon::completion_tracker::CompletionTracker::default();
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                let observation = crate::observation::ObservationStore::open_initialized().unwrap();
                if observation.get_run(&request.instance).unwrap().is_some() {
                    // Seed only after the run exists, while the release barrier
                    // still guarantees it is in flight.
                    crate::daemon::completion_notify_pass(&mut tracker, || false);
                    break;
                }
                assert!(Instant::now() < deadline, "instance run never recorded");
                std::thread::sleep(Duration::from_millis(50));
            }
            std::fs::write(&release, "").unwrap();
            wait_terminal(&store, job_id, 15);
            // Job terminality precedes the observation projection. Poll the
            // notifier until it has seen that projection (and delivered a stop),
            // rather than assuming a fixed number of passes is enough.
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                let observation = crate::observation::ObservationStore::open_initialized().unwrap();
                let terminal = observation
                    .get_run(&request.instance)
                    .unwrap()
                    .and_then(|run| crate::observation::RunStatus::from_label(&run.status))
                    .is_some_and(|status| status.is_terminal());
                crate::daemon::completion_notify_pass(&mut tracker, || false);
                let deliveries = std::fs::read_to_string(&delivered).unwrap_or_default();
                if terminal && (expected == 0 || !deliveries.is_empty()) {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "exit {exit}: completion never observed"
                );
                std::thread::sleep(Duration::from_millis(50));
            }
            // Later passes must neither duplicate a stop nor announce success.
            crate::daemon::completion_notify_pass(&mut tracker, || false);
            crate::daemon::completion_notify_pass(&mut tracker, || false);

            let text = std::fs::read_to_string(&delivered).unwrap_or_default();
            assert_eq!(text.lines().count(), expected, "exit {exit}: {text}");
            if expected == 0 {
                return;
            }
            let payload: Value = serde_json::from_str(text.trim()).unwrap();
            let fact = |label: &str| {
                payload["facts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|fact| fact["label"] == label)
                    .map(|fact| fact["value"].as_str().unwrap().to_string())
            };
            assert_eq!(payload["subject"]["id"], request.instance);
            assert_eq!(fact("Workflow").as_deref(), Some("chain-stop"));
            assert_eq!(fact("Failed step").as_deref(), Some("import"));
            assert_eq!(fact("Attempts").as_deref(), Some("1"));
            let error = fact("Error").unwrap();
            assert!(
                error.contains("exited with status 3: import broke") && !error.contains('\n'),
                "{error}"
            );
            assert!(
                payload["actions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|action| action["command"]
                        == format!("homeboy runs show {}", request.instance)),
                "{payload}"
            );
        });
    }
}
