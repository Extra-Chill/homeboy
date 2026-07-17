use std::process::Command;

use homeboy::core::api_jobs::{JobEventKind, JobStatus};
use homeboy_core::test_support::{HermeticTestContext, ReverseBrokerFixture, TestBinary};

fn output(command: &mut Command) -> std::process::Output {
    let output = command.output().expect("run homeboy fixture command");
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    output
}

#[cfg(unix)]
#[test]
fn detached_cook_accepts_reverse_capacity_queue_and_worker_completes_once() {
    use std::os::unix::fs::PermissionsExt;

    let context = HermeticTestContext::new();
    let broker = ReverseBrokerFixture::start("lab");
    let (_checkout_guard, checkout) =
        homeboy_core::test_support::shared_committed_git_repo_fixture("cook-source");
    let provider = context.root().join("provider.sh");
    std::fs::write(
        &provider,
        "#!/bin/sh\nset -eu\ncat >/dev/null\nprintf '%s\\n' '{\"schema\":\"homeboy/agent-task-outcome/v1\",\"status\":\"succeeded\",\"summary\":\"fixture provider completed\"}'\n",
    )
    .expect("write provider");
    std::fs::set_permissions(&provider, std::fs::Permissions::from_mode(0o755))
        .expect("make provider executable");

    output(context.command(TestBinary::HomeboyFixture).args([
        "server",
        "create",
        "lab",
        "--host",
        "reverse-fixture.invalid",
        "--user",
        "fixture",
    ]));
    output(context.command(TestBinary::HomeboyFixture).args([
        "runner",
        "enable",
        "lab",
        "--workspace-root",
        checkout.to_str().expect("checkout path"),
        "--concurrency-limit",
        "1",
        "--homeboy-path",
        context
            .binary_path(TestBinary::HomeboyFixture)
            .to_str()
            .expect("homeboy path"),
    ]));

    let session_path = context
        .config_dir()
        .join("runner-sessions/lab/fixture-controller.json");
    std::fs::create_dir_all(session_path.parent().expect("session parent"))
        .expect("create session directory");
    std::fs::write(
        session_path,
        serde_json::json!({
            "runner_id": "lab",
            "mode": "reverse",
            "role": "controller",
            "controller_id": "fixture-controller",
            "broker_url": broker.url(),
            "homeboy_version": env!("CARGO_PKG_VERSION"),
            "homeboy_build_identity": null,
            "connected_at": "2026-01-01T00:00:00Z",
            "worker_identity": "fixture-worker",
            "worker_pid": 1,
            "last_seen_at": chrono::Utc::now().to_rfc3339()
        })
        .to_string(),
    )
    .expect("write reverse controller session");

    let mut cook_command = context.command(TestBinary::HomeboyFixture);
    cook_command.env("HOMEBOY_CONTROLLER_ID", "fixture-controller").args([
        "--runner",
        "lab",
        "--placement",
        "lab",
        "--detach-after-handoff",
        "agent-task",
        "cook",
        "--prompt",
        "Run the deterministic fixture provider.",
        "--backend",
        "fixture",
        "--cwd",
        checkout.to_str().expect("checkout path"),
        "--to-worktree",
        checkout.to_str().expect("checkout path"),
        "--provider-command",
        provider.to_str().expect("provider path"),
        "--verify",
        "true",
        "--max-attempts",
        "1",
        "--no-finalize",
    ]);
    let cook = output(&mut cook_command);
    let accepted: serde_json::Value = serde_json::from_slice(&cook.stdout).expect("cook JSON");
    assert_eq!(accepted["success"], true);
    assert!(accepted.to_string().contains("queued") || accepted.to_string().contains("running"));

    let queued = broker.jobs();
    assert_eq!(queued.len(), 1, "detached Cook submits one durable job");
    assert_eq!(queued[0].status, JobStatus::Queued);

    // The worker uses the same broker URL and store as the CLI subprocess.
    let (worker, code) = homeboy::runner::run_reverse_worker(
        homeboy::runner::ReverseRunnerWorkerOptions {
            runner_id: "lab".to_string(),
            broker_url: broker.url().to_string(),
            broker_token: None,
            project_id: None,
            lease_ms: 30_000,
            concurrency_limit: Some(1),
            loop_mode: false,
            idle_backoff_ms: 1,
            max_idle_backoff_ms: 10,
            broker_failure_backoff_ms: 1,
            broker_retry_limit: 1,
        },
    )
    .expect("run reverse worker");
    assert_eq!(code, 0);
    assert!(worker.claimed);
    let completed = broker.store.get(queued[0].id).expect("completed broker job");
    assert_eq!(completed.status, JobStatus::Succeeded);
    assert_eq!(
        broker
            .store
            .events(completed.id)
            .expect("broker events")
            .iter()
            .filter(|event| event.kind == JobEventKind::Result)
            .count(),
        1,
    );

    let (_, duplicate_code) = homeboy::runner::run_reverse_worker(
        homeboy::runner::ReverseRunnerWorkerOptions {
            runner_id: "lab".to_string(), broker_url: broker.url().to_string(), broker_token: None,
            project_id: None, lease_ms: 30_000, concurrency_limit: Some(1), loop_mode: false,
            idle_backoff_ms: 1, max_idle_backoff_ms: 10, broker_failure_backoff_ms: 1, broker_retry_limit: 1,
        },
    ).expect("duplicate worker wake");
    assert_eq!(duplicate_code, 0);
}
