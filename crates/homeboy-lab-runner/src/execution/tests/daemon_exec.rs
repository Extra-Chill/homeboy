//! Daemon `/exec` end-to-end tests that require the real runner exec driver.
//!
//! The daemon `/exec` endpoint routes command execution through the
//! `RunnerExecDriver` hook (Extra-Chill/homeboy#8632), whose real implementation
//! lives in this runner crate (extracted from core in #8698). These tests were
//! moved out of `homeboy-core`'s daemon tests because they assert on the result
//! of actually running the requested command — which a core-only stub driver
//! cannot produce. Here the real driver is registered, so the command runs for
//! real and the result events, exit codes, captured stdout/stderr, and patch
//! artifacts are exercised end to end.

use homeboy_core::api_jobs::{Job, JobEventKind, JobStatus, JobStore, RemoteRunnerJobRequest};
use homeboy_core::daemon::{route_with_body, DirectDaemonExecSubmitRequest};
use homeboy_core::extension::registry::ExtensionLifecycleValidation;
use homeboy_core::observation::ObservationStore;
use homeboy_core::test_support::HomeGuard;
use homeboy_runner_contract::{
    RunnerApiSubmitRequest, RUNNER_API_SUBMIT_REQUEST_SCHEMA, RUNNER_API_V1,
};

use crate::runner_staging_operation::SourceArtifactTransfer;
use crate::runner_staging_store::RemoteRunnerStagingRequest;

/// Register the runner-side exec driver the daemon `/exec` route drives.
/// Production wires this at CLI startup; the registration is an idempotent
/// process-global slot, so registering per test is safe.
fn register_driver() {
    crate::register_runner_daemon_exec_driver();
}

fn write_runner_config(id: &str, value: &serde_json::Value) {
    let dir = homeboy_core::paths::homeboy()
        .expect("homeboy dir")
        .join("runners");
    std::fs::create_dir_all(&dir).expect("create runners dir");
    std::fs::write(
        dir.join(format!("{id}.json")),
        serde_json::to_string_pretty(value).expect("serialize runner"),
    )
    .expect("write runner config");
}

fn create_lab_local_runner() -> HomeGuard {
    let home = HomeGuard::new();
    write_runner_config(
        "lab-local",
        &serde_json::json!({"id": "lab-local", "kind": "local"}),
    );
    home
}

fn wait_for_job(store: &JobStore, job_id: &str) -> Job {
    let id = uuid::Uuid::parse_str(job_id).expect("uuid");
    for _ in 0..100 {
        let job = store.get(id).expect("job");
        if matches!(
            job.status,
            JobStatus::Succeeded | JobStatus::Failed | JobStatus::Cancelled
        ) {
            return job;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    store.get(id).expect("job")
}

fn staged_envelope(
    source: &std::path::Path,
    command: Vec<String>,
    run_id: &str,
) -> crate::runner_staging_operation::RemoteRunnerStagingEnvelope {
    let mut envelope = crate::runner_staging_operation::tests_support::envelope();
    envelope.handoff.run_id = run_id.to_string();
    envelope.handoff.idempotency_key = run_id.to_string();
    envelope.handoff.runner_id = "lab-local".to_string();
    envelope.handoff.recipe.run_id = run_id.to_string();
    envelope.handoff.recipe.runner_id = "lab-local".to_string();
    envelope.handoff.recipe.placement_decision =
        homeboy_core::lab_routing::compatibility_placement_decision(
            homeboy_lab_runner_contract::Placement::Lab,
            Some("lab-local"),
            false,
        );
    envelope.handoff.recipe.normalized_args = command;
    envelope.materialization.authority_id = format!("authority-{run_id}");
    envelope.materialization.workspace_key = run_id.to_string();
    envelope.materialization.source_artifact = Some(
        SourceArtifactTransfer::from_directory(format!("source-{run_id}"), source)
            .expect("bounded source package"),
    );
    envelope.validate().expect("staged envelope");
    envelope
}

fn stage_request(
    store: &JobStore,
    path: &str,
    envelope: &crate::runner_staging_operation::RemoteRunnerStagingEnvelope,
) -> serde_json::Value {
    let response = route_with_body(
        "POST",
        path,
        Some(
            serde_json::to_value(RemoteRunnerStagingRequest::new(envelope.clone()))
                .expect("serialize staging request"),
        ),
        store,
    );
    assert_eq!(response.status_code, 200, "{}", response.body);
    response.body["body"].clone()
}

fn staged_job_id(response: &serde_json::Value) -> String {
    response["receipt"]["handoff"]["runner_job_id"]
        .as_str()
        .expect("staged runner job id")
        .to_string()
}

fn direct_submission(command: Vec<&str>, submission_key: &str) -> serde_json::Value {
    let request = RemoteRunnerJobRequest {
        runner_id: "lab-local".to_string(),
        project_id: None,
        operation: "runner.exec".to_string(),
        command: command.into_iter().map(str::to_string).collect(),
        cwd: Some(std::env::current_dir().expect("cwd").display().to_string()),
        env: Default::default(),
        secret_env_names: Vec::new(),
        secret_env_plan: Default::default(),
        env_materialization: None,
        capture_patch: false,
        source_snapshot: None,
        path_materialization_plan: None,
        require_paths: Vec::new(),
        extension_env_providers: Vec::new(),
        lab_runner_workload: None,
        lifecycle: None,
        workspace_claim_binding: None,
        workspace_owner_lease: None,
        metadata: None,
    };
    serde_json::to_value(DirectDaemonExecSubmitRequest {
        submission: RunnerApiSubmitRequest {
            schema: RUNNER_API_SUBMIT_REQUEST_SCHEMA.to_string(),
            api_version: RUNNER_API_V1,
            submission_key: submission_key.to_string(),
            envelope: request.execution_envelope(),
            workspace_claim_binding: None,
            workspace_owner_lease: None,
            credential_delivery: None,
        },
        runner: None,
        raw_exec: false,
        workspace_owner_request: None,
    })
    .expect("serialize direct submission")
}

#[test]
fn direct_staging_recovers_the_original_queue_entry_and_replays_one_execution() {
    register_driver();
    crate::runner_staging_store::register_runner_staging_provider();
    let _home = create_lab_local_runner();
    write_runner_config(
        "runner-1",
        &serde_json::json!({"id":"runner-1","kind":"local"}),
    );
    let marker = tempfile::tempdir().expect("marker root");
    let marker_path = marker.path().join("executions");
    let store = JobStore::default();
    let mut envelope = crate::runner_staging_operation::tests_support::envelope();
    envelope.handoff.recipe.normalized_args = vec![
        "sh".to_string(),
        "-c".to_string(),
        format!("cat source.bin; printf x >> '{}'", marker_path.display()),
    ];
    let payload = serde_json::to_value(
        crate::runner_staging_store::RemoteRunnerStagingRequest::new(envelope),
    )
    .expect("staging request");
    let queued = route_with_body("POST", "/runner/staging", Some(payload.clone()), &store);
    assert_eq!(queued.status_code, 200, "{}", queued.body);
    let receipt = queued.body["body"]["receipt"].clone();
    let id = receipt["handoff"]["runner_job_id"]
        .as_str()
        .expect("job ID");
    let job_id = uuid::Uuid::parse_str(id).unwrap();
    assert_eq!(store.get(job_id).unwrap().status, JobStatus::Queued);
    assert!(!marker_path.exists());

    let accepted = route_with_body(
        "POST",
        "/runner/staging/direct",
        Some(payload.clone()),
        &store,
    );
    assert_eq!(accepted.status_code, 200, "{}", accepted.body);
    assert_eq!(accepted.body["body"]["receipt"], receipt);
    let terminal = wait_for_job(&store, id);
    let events = store.events(job_id).unwrap();
    assert_eq!(terminal.status, JobStatus::Succeeded, "{events:?}");
    assert!(events.iter().any(|event| event.kind == JobEventKind::Result
        && event
            .data
            .as_ref()
            .is_some_and(|result| result["stdout"] == "source package")));
    assert_eq!(std::fs::read_to_string(&marker_path).unwrap(), "x");

    let replay = route_with_body("POST", "/runner/staging/direct", Some(payload), &store);
    assert_eq!(replay.status_code, 200, "{}", replay.body);
    assert_eq!(replay.body["body"]["receipt"], receipt);
    assert_eq!(store.get(job_id).unwrap().status, JobStatus::Succeeded);
    assert_eq!(std::fs::read_to_string(marker_path).unwrap(), "x");
    assert_eq!(store.events(job_id).unwrap().len(), events.len());
}

#[test]
fn typed_daemon_exec_uses_canonical_submission_without_scalar_execution_fields() {
    register_driver();
    let _home = create_lab_local_runner();
    let store = JobStore::default();
    let payload = direct_submission(
        vec!["sh", "-c", "sleep 1; printf typed"],
        "typed-daemon-run",
    );

    for field in [
        "runner_id",
        "command",
        "cwd",
        "env",
        "source_snapshot",
        "lifecycle",
        "runner_workload",
        "path_materialization_plan",
    ] {
        assert!(payload.get(field).is_none(), "adapter leaked {field}");
    }
    let response = route_with_body("POST", "/exec", Some(payload.clone()), &store);
    assert_eq!(response.status_code, 200);
    let job_id = response.body["body"]["job"]["id"]
        .as_str()
        .expect("job id")
        .to_string();
    let retry = route_with_body("POST", "/exec", Some(payload), &store);
    assert_eq!(retry.status_code, 200);
    assert_eq!(retry.body["body"]["job"]["id"], job_id);
    assert_eq!(retry.body["body"]["idempotent_resubmission"], true);
    assert_eq!(wait_for_job(&store, &job_id).status, JobStatus::Succeeded);
}

#[test]
fn typed_daemon_exec_rejects_submitted_authority_before_owner_registration() {
    let _home = create_lab_local_runner();
    let store = JobStore::default();
    let mut payload = direct_submission(vec!["sh", "-c", "printf no"], "typed-authority");
    payload["workspace_owner_request"] = serde_json::json!({
        "workspace": {
            "schema": homeboy_core::workspace_claim::WORKSPACE_IDENTITY_SCHEMA,
            "kind": "test",
            "locator": "typed-authority",
        },
        "owner_id": "owner",
        "ttl_ms": 1000,
    });
    // Keep the submitted authority well-formed so this reaches the admission
    // policy rather than failing earlier during JSON deserialization.
    payload["submission"]["workspace_claim_binding"] = serde_json::json!({
        "workspace": payload["workspace_owner_request"]["workspace"].clone(),
        "lifecycle_revision": 1,
    });

    let response = route_with_body("POST", "/exec", Some(payload), &store);
    assert_eq!(response.status_code, 400);
    assert!(response
        .body
        .to_string()
        .contains("not submitted workspace authority"));
}

#[test]
fn typed_daemon_exec_rejects_inline_secret_before_execution() {
    register_driver();
    let _home = create_lab_local_runner();
    let store = JobStore::default();
    let marker = std::path::PathBuf::from(std::env::var("HOME").expect("isolated HOME"))
        .join("inline-secret-executed");
    let mut payload = direct_submission(
        vec!["sh", "-c", &format!("printf ran > {}", marker.display())],
        "typed-inline-secret",
    );
    payload["submission"]["envelope"]["dispatch"]["env"]["TOKEN"] =
        serde_json::json!("inline-secret");
    payload["submission"]["envelope"]["secret_env"] = serde_json::to_value(
        homeboy_core::secret_env_plan::SecretEnvPlan::from_secret_env_names(["TOKEN".to_string()]),
    )
    .expect("serialize secret plan");

    let response = route_with_body("POST", "/exec", Some(payload), &store);
    assert_eq!(response.status_code, 400);
    assert!(response
        .body
        .to_string()
        .contains("cannot accept inline secret env values"));
    assert!(response.body["body"]["job"].is_null());
    assert!(!marker.exists());
}

#[test]
fn direct_staging_concurrent_replays_preserve_source_mutations_and_environment() {
    register_driver();
    crate::register_runner_staging_provider();
    let _home = create_lab_local_runner();
    let source = tempfile::tempdir().expect("source");
    let marker_root = tempfile::tempdir().expect("marker");
    let marker = marker_root.path().join("executed");
    std::fs::write(source.path().join("input.txt"), "sealed input\n").expect("source input");
    let run_id = format!("direct-staged-{}", uuid::Uuid::new_v4());
    let mut envelope = staged_envelope(
        source.path(),
        vec![
            "sh".to_string(),
            "-c".to_string(),
            format!(
                "set -e; test \"$STAGED_PUBLIC\" = preserved; test \"$(cat input.txt)\" = 'sealed input'; printf 'sealed stdout'; printf changed > input.txt; sleep 0.1; test \"$(cat input.txt)\" = changed; printf x >> '{}'",
                marker.display()
            ),
        ],
        &run_id,
    );
    envelope
        .handoff
        .recipe
        .job_override_env
        .insert("STAGED_PUBLIC".to_string(), "preserved".to_string());
    let store = JobStore::default();

    let queued = stage_request(&store, "/runner/staging", &envelope);
    let job_id = staged_job_id(&queued);
    assert_eq!(
        store
            .get(uuid::Uuid::parse_str(&job_id).expect("uuid"))
            .expect("job")
            .status,
        JobStatus::Queued
    );
    assert!(!marker.exists(), "reverse staging must remain queue-only");

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let submissions = (0..2)
        .map(|_| {
            let store = store.clone();
            let envelope = envelope.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                stage_request(&store, "/runner/staging/direct", &envelope)
            })
        })
        .collect::<Vec<_>>();
    let responses = submissions
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(responses[0]["receipt"], responses[1]["receipt"]);
    let direct = &responses[0];
    assert_eq!(
        staged_job_id(&direct),
        job_id,
        "direct adoption retains the staged UUID"
    );
    let terminal = wait_for_job(&store, &job_id);
    assert_eq!(terminal.status, JobStatus::Succeeded);
    let result = store
        .events(terminal.id)
        .expect("events")
        .into_iter()
        .find(|event| event.kind == JobEventKind::Result)
        .and_then(|event| event.data)
        .expect("execution result");
    assert_eq!(result["stdout"], "sealed stdout");
    assert_eq!(std::fs::read_to_string(&marker).expect("marker"), "x");

    let replay = stage_request(&store, "/runner/staging/direct", &envelope);
    assert_eq!(replay["receipt"], direct["receipt"]);
    assert_eq!(staged_job_id(&replay), job_id);
    assert_eq!(wait_for_job(&store, &job_id).status, JobStatus::Succeeded);
    assert_eq!(std::fs::read_to_string(&marker).expect("marker"), "x");
}

#[test]
fn direct_staging_preserves_nonzero_child_failure() {
    register_driver();
    crate::register_runner_staging_provider();
    let _home = create_lab_local_runner();
    let source = tempfile::tempdir().expect("source");
    std::fs::write(source.path().join("input.txt"), "failure input\n").expect("source input");
    let run_id = format!("direct-staged-failure-{}", uuid::Uuid::new_v4());
    let envelope = staged_envelope(
        source.path(),
        vec![
            "sh".to_string(),
            "-c".to_string(),
            "test \"$(cat input.txt)\" = 'failure input' && printf staged-out; printf staged-err >&2; exit 23".to_string(),
        ],
        &run_id,
    );
    let store = JobStore::default();
    let response = stage_request(&store, "/runner/staging/direct", &envelope);
    let job_id = staged_job_id(&response);
    let terminal = wait_for_job(&store, &job_id);
    assert_eq!(terminal.status, JobStatus::Failed);
    let result = store
        .events(terminal.id)
        .expect("events")
        .into_iter()
        .find(|event| event.kind == JobEventKind::Result)
        .and_then(|event| event.data)
        .expect("execution result");
    assert_eq!(result["exit_code"], 23);
    assert_eq!(result["stdout"], "staged-out");
    assert_eq!(result["stderr"], "staged-err");
}

#[test]
#[cfg(unix)]
fn direct_staging_retains_the_original_cancel_endpoint_and_reaps_the_child() {
    register_driver();
    crate::register_runner_staging_provider();
    let _home = create_lab_local_runner();
    let source = tempfile::tempdir().expect("source");
    std::fs::write(source.path().join("input"), "sealed").unwrap();
    let evidence = tempfile::tempdir().expect("child evidence");
    let pid_path = evidence.path().join("pid");
    let late_path = evidence.path().join("late");
    let envelope = staged_envelope(
        source.path(),
        vec![
            "sh".to_string(),
            "-c".to_string(),
            format!(
                "printf '%s' $$ > '{}'; sleep 30; printf late > '{}'",
                pid_path.display(),
                late_path.display()
            ),
        ],
        "direct-cancel",
    );
    let store = JobStore::default();
    let accepted = stage_request(&store, "/runner/staging/direct", &envelope);
    let id = staged_job_id(&accepted);
    for _ in 0..100 {
        if pid_path.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let pid = std::fs::read_to_string(&pid_path)
        .expect("child started")
        .parse::<i32>()
        .unwrap();
    let cancelled = route_with_body("POST", &format!("/runner/jobs/{id}/cancel"), None, &store);
    assert_eq!(cancelled.status_code, 200, "{}", cancelled.body);
    assert_eq!(wait_for_job(&store, &id).status, JobStatus::Cancelled);
    for _ in 0..100 {
        if unsafe { libc::kill(pid, 0) } != 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_ne!(
        unsafe { libc::kill(pid, 0) },
        0,
        "cancelled child still exists"
    );
    assert!(!late_path.exists());
    let replay = stage_request(&store, "/runner/staging/direct", &envelope);
    assert_eq!(replay["receipt"], accepted["receipt"]);
    assert_eq!(wait_for_job(&store, &id).status, JobStatus::Cancelled);
}

#[test]
fn daemon_exec_does_not_require_runner_config_on_daemon_host() {
    register_driver();
    let _home = HomeGuard::new();
    let store = JobStore::default();
    let response = route_with_body(
        "POST",
        "/exec",
        Some(serde_json::json!({
            "runner_id": "homeboy-lab",
            "cwd": std::env::current_dir().expect("cwd"),
            "command": ["sh", "-c", "printf lab"]
        })),
        &store,
    );

    assert_eq!(response.status_code, 200);
    let job_id = response.body["body"]["job"]["id"]
        .as_str()
        .expect("job id")
        .to_string();
    let job = wait_for_job(&store, &job_id);
    assert_eq!(job.status, JobStatus::Succeeded);

    let events = store.events(job.id).expect("events");
    assert!(events.iter().any(|event| {
        event
            .data
            .as_ref()
            .and_then(|data| data.get("execution_context"))
            .is_some_and(|evidence| {
                evidence["content_sha256"]
                    .as_str()
                    .is_some_and(|value| value.starts_with("sha256:"))
                    && evidence["context"]["runner_job_id"] == serde_json::json!(job.id.to_string())
            })
    }));
    let result = events
        .iter()
        .find(|event| event.kind == JobEventKind::Result)
        .and_then(|event| event.data.as_ref())
        .expect("result event");
    assert_eq!(result["runner_id"], "homeboy-lab");
    assert_eq!(result["stdout"], "lab");
    assert_eq!(result["source_snapshot"]["runner_id"], "homeboy-lab");
    assert_eq!(result["source_snapshot"]["sync_mode"], "existing_remote");
}

#[test]
fn exec_applies_request_env_to_daemon_command() {
    register_driver();
    let _home = create_lab_local_runner();
    let store = JobStore::default();
    let response = route_with_body(
        "POST",
        "/exec",
        Some(serde_json::json!({
            "runner_id": "lab-local",
            "cwd": std::env::current_dir().expect("cwd"),
            "command": ["sh", "-c", "printf '%s' \"$HOMEBOY_TEST_DAEMON_ENV\""],
            "env": {
                "HOMEBOY_TEST_DAEMON_ENV": "ok"
            }
        })),
        &store,
    );

    assert_eq!(response.status_code, 200);
    let job_id = response.body["body"]["job"]["id"]
        .as_str()
        .expect("job id")
        .to_string();
    let job = wait_for_job(&store, &job_id);
    assert_eq!(job.status, JobStatus::Succeeded);

    let events = store.events(job.id).expect("events");
    let result = events
        .iter()
        .find(|event| event.kind == JobEventKind::Result)
        .and_then(|event| event.data.as_ref())
        .expect("result event");
    assert_eq!(result["runner_id"], "lab-local");
    assert_eq!(
        result["cwd"],
        std::env::current_dir()
            .expect("cwd")
            .to_string_lossy()
            .to_string()
    );
    assert_eq!(
        result["command"],
        serde_json::json!(["sh", "-c", "printf '%s' \"$HOMEBOY_TEST_DAEMON_ENV\""])
    );
    assert_eq!(result["stdout"], "ok");
    assert_eq!(result["source_snapshot"]["runner_id"], "lab-local");
    assert_eq!(result["source_snapshot"]["sync_mode"], "existing_remote");
    assert!(result["metrics"]["duration_ms"].as_u64().is_some());
    if cfg!(target_os = "linux") {
        assert_eq!(result["metrics"]["source"], "linux_procfs_process_tree");
        if result["metrics"]["sample_count"].as_u64().unwrap_or(0) > 0 {
            assert!(result["metrics"].get("peak_rss_bytes").is_some());
        }
    }
}

#[test]
fn daemon_exec_injects_extension_env_and_redacts_provider_secret() {
    register_driver();
    let _home = create_lab_local_runner();
    let extension = tempfile::tempdir().expect("extension");
    let secret = extension.path().join("fixture-secret");
    std::fs::write(&secret, "runner-secret\n").expect("secret");
    std::fs::write(
        extension.path().join("fixture.json"),
        r#"{"id":"fixture","name":"Fixture","version":"1.2.3","env_provider":{"script":"env.sh","secret_env":["FIXTURE_SECRET"]}}"#,
    )
    .expect("manifest");
    std::fs::write(
        extension.path().join("env.sh"),
            "#!/bin/sh\ntest -n \"$HOMEBOY_ENV_PROVIDER_COMMAND_PAYLOAD\" || exit 23\nprintf '%s\\n' '{\"FIXTURE_RUNTIME\":\"runner-local\",\"HOMEBOY_ACTIVE_RUN_ID\":\"provider-active\",\"HOMEBOY_RUN_ID\":\"provider-homeboy\",\"HOMEBOY_BENCH_RUN_ID\":\"provider-bench\",\"WORKFLOW_BENCH_RUN_ID\":\"provider-workflow\"}'\n",
    )
    .expect("provider");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            extension.path().join("env.sh"),
            std::fs::Permissions::from_mode(0o755),
        )
        .expect("provider executable");
    }
    homeboy_core::extension::lifecycle::install(
        &extension.path().display().to_string(),
        Some("fixture"),
        ExtensionLifecycleValidation::declaration_only(),
    )
    .expect("install fixture extension");
    let store = JobStore::default();
    let response = route_with_body(
        "POST",
        "/exec",
        Some(serde_json::json!({
            "runner_id": "lab-local",
            "runner": {
                "id": "lab-local",
                "kind": "local",
                "secret_env": { "FIXTURE_SECRET": { "file": secret } }
            },
            "cwd": std::env::current_dir().expect("cwd"),
            "command": ["sh", "-c", "test \"$FIXTURE_RUNTIME\" = runner-local && test -n \"$FIXTURE_SECRET\" && printf '%s|%s|%s|%s' \"$HOMEBOY_ACTIVE_RUN_ID\" \"$HOMEBOY_RUN_ID\" \"$HOMEBOY_BENCH_RUN_ID\" \"${WORKFLOW_BENCH_RUN_ID-unset}\""],
            "extension_env_providers": ["fixture"],
            "idempotency_key": "daemon-explicit-run"
        })),
        &store,
    );

    assert_eq!(response.status_code, 200);
    assert_eq!(
        response.body["body"]["request"]["extension_env_providers"]["providers"],
        serde_json::json!(["fixture"]),
        "enqueue retains only provider declarations; resolved output is produced after authenticated execution"
    );
    let job_id = response.body["body"]["job"]["id"]
        .as_str()
        .expect("job id")
        .to_string();
    let job = wait_for_job(&store, &job_id);
    assert_eq!(job.status, JobStatus::Succeeded);

    let result = store
        .events(job.id)
        .expect("events")
        .into_iter()
        .find(|event| event.kind == JobEventKind::Result)
        .and_then(|event| event.data)
        .expect("result");
    assert!(!result.to_string().contains("runner-secret"));
    assert_eq!(
        result["stdout"],
        "daemon-explicit-run|daemon-explicit-run|daemon-explicit-run|unset"
    );
    let hints = result["diagnostic_hints"]
        .as_array()
        .expect("daemon result carries precedence diagnostics");
    let hint = hints
        .iter()
        .filter_map(serde_json::Value::as_str)
        .find(|hint| hint.contains("runner exec --run-id took precedence"))
        .expect("precedence hint");
    for name in [
        "HOMEBOY_ACTIVE_RUN_ID",
        "HOMEBOY_RUN_ID",
        "HOMEBOY_BENCH_RUN_ID",
        "WORKFLOW_BENCH_RUN_ID",
    ] {
        assert!(hint.contains(name), "{hint}");
    }
    assert!(!hint.contains("provider-active"), "{hint}");
    assert_eq!(
        result["extension_env_providers"][0]["extension_id"],
        "fixture"
    );
    assert_eq!(
        result["extension_env_providers"][0]["secret_env_names"],
        serde_json::json!(["FIXTURE_SECRET"])
    );
}

#[test]
fn exec_failed_command_marks_job_failed_after_result_event() {
    register_driver();
    let _home = create_lab_local_runner();
    let store = JobStore::default();
    let response = route_with_body(
        "POST",
        "/exec",
        Some(serde_json::json!({
            "runner_id": "lab-local",
            "cwd": std::env::current_dir().expect("cwd"),
            "command": ["sh", "-c", "printf out; printf err >&2; exit 7"]
        })),
        &store,
    );

    assert_eq!(response.status_code, 200);
    let job_id = response.body["body"]["job"]["id"]
        .as_str()
        .expect("job id")
        .to_string();
    let job = wait_for_job(&store, &job_id);
    assert_eq!(job.status, JobStatus::Failed);

    let events = store.events(job.id).expect("events");
    let result = events
        .iter()
        .find(|event| event.kind == JobEventKind::Result)
        .and_then(|event| event.data.as_ref())
        .expect("result event");
    assert_eq!(result["exit_code"], 7);
    assert_eq!(result["stdout"], "out");
    assert_eq!(result["stderr"], "err");

    let status_events: Vec<_> = events
        .iter()
        .filter(|event| event.kind == JobEventKind::Status)
        .collect();
    assert!(status_events.iter().all(|event| {
        event.data.as_ref().and_then(|data| data["status"].as_str()) != Some("succeeded")
    }));
    let final_status = status_events.last().expect("final status event");
    assert_eq!(final_status.data.as_ref().unwrap()["status"], "failed");
    assert_ne!(final_status.message.as_deref(), Some("job succeeded"));
}

#[test]
fn exec_capture_patch_records_remote_delta_artifact() {
    register_driver();
    let _home = create_lab_local_runner();
    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::write(workspace.path().join("file.txt"), "before\n").expect("seed file");
    let source_snapshot = homeboy_core::source_snapshot::existing_remote(
        "lab-local",
        &workspace.path().display().to_string(),
        Some(workspace.path().display().to_string().as_str()),
    );
    let store = JobStore::default();
    let response = route_with_body(
        "POST",
        "/exec",
        Some(serde_json::json!({
            "runner_id": "lab-local",
            "cwd": workspace.path(),
            "command": ["sh", "-c", "printf 'after\n' > file.txt"],
            "capture_patch": true,
            "source_snapshot": source_snapshot,
        })),
        &store,
    );

    assert_eq!(response.status_code, 200);
    let job_id = response.body["body"]["job"]["id"]
        .as_str()
        .expect("job id")
        .to_string();
    let job = wait_for_job(&store, &job_id);
    assert_eq!(format!("{:?}", job.status), "Succeeded");

    let events = store.events(job.id).expect("events");
    let result = events
        .iter()
        .rev()
        .filter_map(|event| event.data.as_ref())
        .find(|data| data.get("patch").is_some())
        .expect("patch result");
    let patch = &result["patch"];
    assert_eq!(patch["runner_id"], "lab-local");
    assert_eq!(patch["remote_path"], workspace.path().display().to_string());
    assert_eq!(patch["modified_files"], serde_json::json!(["file.txt"]));
    assert_eq!(patch["dirty_snapshot"], false);
    assert_eq!(patch["baseline_missing"], false);
    assert!(patch["patch_artifact_id"].as_str().is_some());

    let observation_store = ObservationStore::open_initialized().expect("observation store");
    let run_id = format!("runner-exec-{job_id}");
    let artifacts = observation_store
        .list_artifacts(&run_id)
        .expect("patch artifacts");
    assert_eq!(artifacts.len(), 1);
    let patch_body = std::fs::read_to_string(&artifacts[0].path).expect("patch file");
    assert!(patch_body.contains("-before"));
    assert!(patch_body.contains("+after"));
}
