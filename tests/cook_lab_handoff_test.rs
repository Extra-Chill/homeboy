use std::path::Path;
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

use homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore;
use homeboy_core::api_jobs::{JobStatus, JobStore};
use homeboy_core::observation::{NewRunRecord, ObservationStore, RunListFilter};
use homeboy_core::test_support::{bounded_output, HermeticTestContext, TestBinary};

/// Run the fixture binary through the shared hermetic harness.
///
/// Every case in this file asserts what an invocation settles *before* any
/// worktree or provider resolution: a contradictory combination is rejected,
/// and a locally-detached Cook either materializes executable ownership or
/// reports an honest rejection (#11476, #12290).
///
/// `HermeticTestContext::command` owns the complete isolation contract — HOME,
/// XDG config and data roots, artifact root, runtime and temp dirs — and strips
/// the Lab transport variables. Overriding only HOME on a hand-rolled `Command`
/// leaves that leak open (#10717, #10718). The transport variables are injected
/// explicitly only by the case that asserts they cannot change a validation
/// outcome (#10917); the Lab-route case likewise opts into CI resource admission
/// so live host pressure cannot preempt the routing diagnostic.
/// Cook cases name the test-support `fixture` backend explicitly: test HOME is
/// intentionally empty, so backend selection must never depend on an operator
/// default provider policy.
///
/// `controller_runtime_command` adds the controller-runtime pins on top of that
/// contract. Every `agent-task cook` invocation is sealed into an immutable
/// controller runtime by `delegate_agent_task_cook_to_pinned_runtime` *before*
/// routing reaches validation, so a case that survives clap pays a cold pin —
/// hash, 666 MB copy, re-exec — against a fresh `HOME`. The pins are immutable
/// and content-addressed, so sharing them costs no isolation (#10687, #11185).
///
/// `bounded_output` replaces `Command::output`, which waits on pipe EOF with no
/// ceiling and cannot be interrupted by libtest (#10687).
fn homeboy(args: &[&str]) -> Output {
    let context = HermeticTestContext::new();
    let mut command = context.controller_runtime_command(TestBinary::HomeboyFixture);
    command.args(args);
    bounded_output(command)
}

#[test]
fn cook_handoff_subprocesses_do_not_inherit_controller_transport() {
    // Pins the isolation the rejections below depend on. If these fixtures ever
    // stop stripping the controller transport variables, the routing bail-out
    // above `run_split_placement_cook` swallows every rejection this file
    // asserts, and the suite hangs instead of failing (#10917).
    let context = HermeticTestContext::new();
    let command = context.controller_runtime_command(TestBinary::HomeboyFixture);
    let removed_env = command
        .get_envs()
        .filter_map(|(key, value)| value.is_none().then(|| key.to_string_lossy().into_owned()))
        .collect::<std::collections::BTreeSet<_>>();

    assert!(
        removed_env.contains(homeboy_core::observation::LAB_OFFLOAD_METADATA_ENV),
        "cook handoff fixtures must not inherit Lab offload transport"
    );
    assert!(
        removed_env.contains(homeboy_core::observation::SOURCE_SNAPSHOT_METADATA_ENV),
        "cook handoff fixtures must not inherit the controller source snapshot"
    );
}

#[test]
fn contradictory_cook_arguments_survive_controller_transport_context() {
    // The routing bail-outs skip *routing*, not validation. Injecting the exact
    // transport context that used to suppress these rejections must not change
    // the outcome: a contradictory flag combination is contradictory wherever
    // the process runs.
    //
    // Before #10917 this context sent both invocations past validation into
    // real worktree and provider work, where they blocked indefinitely instead
    // of failing fast.
    // #11185 moved this case onto the shared controller-runtime pins; #11191
    // reverted it while rebasing an unrelated broker fixture, restoring a cold
    // 666 MB pin per invocation on a gate that was already at its ceiling.
    let cook = |args: &[&str]| {
        let context = HermeticTestContext::new();
        let mut command = context.controller_runtime_command(TestBinary::HomeboyFixture);
        command
            .env(
                homeboy_core::observation::LAB_OFFLOAD_METADATA_ENV,
                r#"{"runner_id":"homeboy-lab"}"#,
            )
            .args(args);
        bounded_output(command)
    };

    // Local detach is served on a controller (#11476), but not here: this
    // process IS the runner's owned execution of one attempt. It has no
    // controller lifecycle to hand off, so detaching would orphan work the
    // runner believes it owns. The rejection must still land before any
    // worktree or provider resolution.
    let detach = cook(&[
        "--placement",
        "local",
        "--detach-after-handoff",
        "agent-task",
        "cook",
        "--backend",
        "fixture",
        "--prompt",
        "implement the fix",
        "--to-worktree",
        "missing@worktree",
        "--verify",
        "true",
    ]);
    assert!(!detach.status.success());
    let detach_stdout = String::from_utf8_lossy(&detach.stdout);
    assert!(
        detach_stdout.contains(
            "cannot detach after handoff with --placement local inside a runner-owned execution"
        ),
        "{detach_stdout}"
    );
    assert!(
        !detach_stdout.contains("worktree provider"),
        "{detach_stdout}"
    );

    let queue_only = cook(&[
        "agent-task",
        "cook",
        "--backend",
        "fixture",
        "--prompt",
        "implement the fix",
        "--to-worktree",
        "missing@worktree",
        "--verify",
        "true",
        "--queue-only",
    ]);
    assert!(!queue_only.status.success());
    let queue_only_stdout = String::from_utf8_lossy(&queue_only.stdout);
    assert!(
        queue_only_stdout.contains("cannot queue its controller-owned lifecycle"),
        "{queue_only_stdout}"
    );
    assert!(
        !queue_only_stdout.contains("worktree provider"),
        "{queue_only_stdout}"
    );
}

#[test]
fn cook_rejects_invalid_controller_transport_before_worktree_resolution() {
    // `--runner` implies Lab placement, so combining it with an explicit
    // `--placement` is contradictory and rejected at argument parsing (see
    // `runner_and_placement_are_mutually_exclusive`). That rejection happens
    // before any worktree provider resolution.
    let output = homeboy(&[
        "--placement",
        "local",
        "--runner",
        "homeboy-lab",
        "agent-task",
        "cook",
        "--backend",
        "fixture",
        "--prompt",
        "implement the fix",
        "--to-worktree",
        "missing@worktree",
        "--verify",
        "true",
    ]);

    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("'--placement <PLACEMENT>' cannot be used with '--runner <RUNNER_ID>'"));
    assert!(!stdout.contains("worktree provider"));
    assert!(!stderr.contains("worktree provider"));
}

/// A local detached Cook with pending provider resolution is accepted only after
/// its durable attempt is externally visible (#12290, #13512).
///
/// The launcher hands the Cook to a process in its own session, then observes
/// whether it materializes the durable attempt. It performs no worktree or
/// provider resolution itself, preserving the bounded handoff boundary.
#[test]
fn cook_accepts_local_detachment_after_pending_attempt_materialization() {
    let context = HermeticTestContext::new();
    let mut command = context.controller_runtime_command(TestBinary::HomeboyFixture);
    command
        // Bound the launcher's wait so an unresolvable destination reports an
        // honest handoff state promptly instead of holding the default budget.
        .env("HOMEBOY_COOK_DETACH_HANDOFF_TIMEOUT_MS", "5000")
        .args([
            "--placement",
            "local",
            "--detach-after-handoff",
            "agent-task",
            "cook",
            "--run-id",
            "local-detach-exits-before-attempt",
            "--backend",
            "fixture",
            "--prompt",
            "implement the fix",
            "--to-worktree",
            "missing@worktree",
            "--verify",
            "true",
        ]);
    let stdout_path = context.root().join("detached-cook.stdout");
    let stderr_path = context.root().join("detached-cook.stderr");
    command
        .stdout(Stdio::from(
            std::fs::File::create(&stdout_path).expect("create detached Cook stdout"),
        ))
        .stderr(Stdio::from(
            std::fs::File::create(&stderr_path).expect("create detached Cook stderr"),
        ));
    let mut child = command.spawn().expect("start detached Cook launcher");
    let status = child.wait().expect("wait for detached Cook launcher");
    let stdout = std::fs::read_to_string(&stdout_path).expect("read detached Cook stdout");
    let stderr = std::fs::read_to_string(&stderr_path).expect("read detached Cook stderr");
    assert!(status.success(), "{stdout}\n{stderr}");
    assert!(
        !stdout.contains("cannot detach after handoff with --placement local"),
        "{stdout}"
    );
    assert!(stdout.contains("\"state\": \"accepted\""), "{stdout}");
    assert!(
        stdout.contains("local-detach-exits-before-attempt-attempt-1-"),
        "{stdout}"
    );
    let mut status = context.controller_runtime_command(TestBinary::HomeboyFixture);
    status.args([
        "agent-task",
        "status",
        "local-detach-exits-before-attempt",
        "--full",
    ]);
    let status = bounded_output(status);
    let status_stdout = String::from_utf8_lossy(&status.stdout);
    assert!(
        status.status.success(),
        "durable attempt must be visible: {status_stdout}"
    );
    assert!(status_stdout.contains("local-detach-exits-before-attempt"));
}

#[test]
fn local_retry_launch_token_is_not_reinterpreted_as_a_detached_cook() {
    let context = HermeticTestContext::new();
    let token_path = context.root().join("retry-launch-token");
    std::fs::write(&token_path, "consumed-token").expect("publish retry launch token");
    let mut command = context.controller_runtime_command(TestBinary::HomeboyFixture);
    command
        .env("HOMEBOY_COOK_DETACH_HANDOFF_TIMEOUT_MS", "0")
        .env("HOMEBOY_LOCAL_COOK_LAUNCH_TOKEN", "consumed-token")
        .env("HOMEBOY_LOCAL_COOK_LAUNCH_TOKEN_PATH", token_path)
        .args([
            "--placement",
            "local",
            "agent-task",
            "retry",
            "missing-cook-run",
            "--run",
        ]);

    let output = bounded_output(command);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!output.status.success(), "{stdout}");
    assert!(!stdout.contains("empty_detached_plan"), "{stdout}");
    assert!(stdout.contains("missing-cook-run"), "{stdout}");
}

/// A successful local detach exposes accepted only after the child has written
/// an attempt that status can resolve, rather than merely its zero-task parent.
#[test]
fn cook_accepts_local_detachment_after_materializing_an_executable_attempt() {
    let context = HermeticTestContext::new();
    let (_checkout_guard, checkout) =
        homeboy_core::test_support::shared_committed_git_repo_fixture("local-detach-acceptance");
    let mut register = context.command(TestBinary::HomeboyFixture);
    register.args([
        "component",
        "create",
        "--local-path",
        checkout.to_str().expect("checkout path"),
    ]);
    let registered = bounded_output(register);
    assert!(
        registered.status.success(),
        "register component: {}",
        String::from_utf8_lossy(&registered.stdout)
    );
    let task_worktree = context.root().join("local-detach-acceptance-worktree");
    homeboy_core::test_support::run_git_fixture_command(
        &checkout,
        &[
            "worktree",
            "add",
            "-b",
            "local-detach-acceptance",
            task_worktree.to_str().expect("task worktree path"),
        ],
    );
    std::fs::write(
        context.config_dir().join("homeboy.json"),
        r#"{"retention":{"reconstructable_artifact_reserve_bytes":0}}"#,
    )
    .expect("disable host-capacity admission for fixture worktree");
    let cook_id = "local-detach-materializes-attempt";
    let output_path = context.root().join("local-detach-handoff.json");
    let mut command = context.controller_runtime_command(TestBinary::HomeboyFixture);
    command.args([
        "--placement",
        "local",
        "--detach-after-handoff",
        "agent-task",
        "cook",
        "--run-id",
        cook_id,
        "--repo",
        "local-detach-acceptance",
        "--backend",
        "fixture",
        "--prompt",
        "materialize a deterministic local attempt",
        "--cwd",
        task_worktree.to_str().expect("task worktree path"),
        "--to-worktree",
        task_worktree.to_str().expect("task worktree path"),
        "--verify",
        "true",
        "--max-attempts",
        "1",
        "--no-finalize",
        "--output",
        output_path.to_str().expect("handoff output path"),
    ]);
    let output = bounded_output(command);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert_eq!(
        std::fs::read_to_string(&output_path).expect("read atomic handoff output"),
        stdout,
        "stdout and --output must receive the identical terminal handoff envelope"
    );
    let handoff: serde_json::Value = serde_json::from_str(
        &stdout[stdout
            .find('{')
            .unwrap_or_else(|| panic!("handoff envelope is present\n{stdout}"))..],
    )
    .unwrap_or_else(|error| panic!("handoff envelope is JSON: {error}\n{stdout}"));
    assert_eq!(handoff["handoff"]["state"], "accepted", "{stdout}");
    let attempt_id = handoff["run_id"]
        .as_str()
        .unwrap_or_else(|| panic!("accepted handoff names an attempt\n{stdout}"))
        .to_string();
    assert_ne!(attempt_id, cook_id, "{stdout}");
    let supervisor_job_id = handoff["controller_job"]["job_id"]
        .as_str()
        .and_then(|job_id| uuid::Uuid::parse_str(job_id).ok())
        .unwrap_or_else(|| panic!("accepted handoff names its controller job\n{stdout}"));
    let mut status = context.controller_runtime_command(TestBinary::HomeboyFixture);
    status.args(["agent-task", "status", &attempt_id, "--full"]);
    let status = bounded_output(status);
    let status_stdout = String::from_utf8_lossy(&status.stdout);
    assert!(status.status.success(), "{status_stdout}");
    assert!(status_stdout.contains(&attempt_id), "{status_stdout}");
    assert!(!status_stdout.contains("\"tasks\": []"), "{status_stdout}");
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
    let attempt_record = lifecycle_store
        .read_record(&attempt_id)
        .expect("read accepted attempt record");
    assert_eq!(
        attempt_record.metadata["local_cook_supervisor"]["job_id"],
        supervisor_job_id.to_string(),
        "the executable run and controller job are linked before acceptance"
    );
    assert!(
        lifecycle_store.read_record(cook_id).is_err(),
        "local detach must not persist a zero-task handoff parent"
    );
    let job_store = JobStore::open_without_reconciliation(context.daemon_dir().join("jobs.json"))
        .expect("open controller job store");
    assert!(
        job_store
            .events(supervisor_job_id)
            .expect("read controller job events")
            .iter()
            .any(|event| event
                .data
                .as_ref()
                .is_some_and(|data| { data["durable_run_id"].as_str() == Some(cook_id) })),
        "controller job admission must carry its durable Cook identity"
    );

    // Cancel through the durable Cook alias, then wait on the returned attempt.
    // The controller job owns process-tree termination and reaping; this keeps
    // the fixture from leaking its detached child across hermetic teardown.
    let mut cancel = context.controller_runtime_command(TestBinary::HomeboyFixture);
    cancel.args(["agent-task", "cancel", cook_id]);
    let cancelled = bounded_output(cancel);
    let cancelled_stdout = String::from_utf8_lossy(&cancelled.stdout);
    assert!(
        cancelled.status.success() || cancelled_stdout.contains("already terminal"),
        "cancel detached Cook: {cancelled_stdout}"
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let mut status = context.controller_runtime_command(TestBinary::HomeboyFixture);
        status.args(["agent-task", "status", &attempt_id, "--full"]);
        let status = bounded_output(status);
        let status_stdout = String::from_utf8_lossy(&status.stdout);
        assert!(status.status.success(), "{status_stdout}");
        let terminal = serde_json::from_str::<serde_json::Value>(&status_stdout)
            .ok()
            .is_some_and(|status| {
                [
                    "/data/state",
                    "/data/child_run_state",
                    "/data/lifecycle/execution/state",
                    "/data/status_scope/queried_attempt/state",
                ]
                .iter()
                .filter_map(|pointer| status.pointer(pointer).and_then(serde_json::Value::as_str))
                .any(|state| matches!(state, "succeeded" | "failed" | "cancelled"))
            });
        if terminal {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "detached Cook did not terminalize after cancellation: {status_stdout}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Piped stdio is not permission to turn a default local wait into a durable
/// handoff. This target fails during foreground resolution, which makes the
/// assertion bounded while proving the launcher did not emit a detach envelope.
#[test]
fn non_tty_local_wait_stays_foreground() {
    let output = homeboy(&[
        "--placement",
        "local",
        "agent-task",
        "cook",
        "--backend",
        "fixture",
        "--prompt",
        "implement the fix",
        "--to-worktree",
        "missing@worktree",
        "--verify",
        "true",
    ]);

    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("homeboy/agent-task-cook-local-detach-handoff/v1"),
        "a default local wait must not return a detach handoff: {stdout}"
    );
    assert!(
        !stdout.contains("\"status\": \"in_flight\""),
        "a default local wait must not return an in-flight Cook report: {stdout}"
    );
}

/// A foreground client is an observer after acceptance, not the owner of local
/// provider work. Killing it must leave the daemon-supervised Cook to retain its
/// terminal artifacts (#12248).
#[test]
fn foreground_local_cook_survives_client_termination_with_artifacts() {
    let context = HermeticTestContext::new();
    let (_checkout_guard, checkout) =
        homeboy_core::test_support::shared_committed_git_repo_fixture("local-cook-durability");
    let component_id = "local-cook-durability";
    let mut register = context.command(TestBinary::HomeboyFixture);
    register.args([
        "component",
        "create",
        "--local-path",
        checkout.to_str().expect("checkout path"),
    ]);
    let registered = bounded_output(register);
    assert!(
        registered.status.success(),
        "register Cook component: stdout={} stderr={}",
        String::from_utf8_lossy(&registered.stdout),
        String::from_utf8_lossy(&registered.stderr),
    );
    let task_worktree = context
        .root()
        .join("foreground-client-termination-worktree");
    homeboy_core::test_support::run_git_fixture_command(
        &checkout,
        &[
            "worktree",
            "add",
            "-b",
            "foreground-client-termination",
            task_worktree.to_str().expect("task worktree path"),
        ],
    );
    std::fs::write(
        context.config_dir().join("homeboy.json"),
        r#"{"retention":{"reconstructable_artifact_reserve_bytes":0}}"#,
    )
    .expect("disable host-capacity admission for fixture worktree");
    let cook_id = "foreground-client-termination";
    let client_stdout = context.root().join("foreground-client.stdout");
    let client_stderr = context.root().join("foreground-client.stderr");
    let provider_started = context.root().join("fixture-provider-started");
    let mut client = context.controller_runtime_command(TestBinary::HomeboyFixture);
    client
        // This test exercises automatic placement, not live host pressure.
        .env("GITHUB_ACTIONS", "true")
        .env("HOMEBOY_FIXTURE_PROVIDER_DELAY_MS", "5000")
        .env("HOMEBOY_FIXTURE_PROVIDER_STARTED_FILE", &provider_started)
        .args([
            "agent-task",
            "cook",
            "--run-id",
            cook_id,
            "--repo",
            component_id,
            "--backend",
            "fixture",
            "--prompt",
            "complete after the observing client exits",
            "--cwd",
            task_worktree.to_str().expect("task worktree path"),
            "--to-worktree",
            task_worktree.to_str().expect("task worktree path"),
            "--verify",
            "true",
            "--max-attempts",
            "1",
            "--no-finalize",
        ])
        .stdout(Stdio::from(
            std::fs::File::create(&client_stdout).expect("create client stdout"),
        ))
        .stderr(Stdio::from(
            std::fs::File::create(&client_stderr).expect("create client stderr"),
        ));
    let mut client = client.spawn().expect("start foreground Cook client");

    // The fixture writes this marker only after the scheduler has reserved its
    // provider execution. This avoids repeatedly cold-starting status CLIs and
    // distinguishes the pre-dispatch `provider_start` progress event from the
    // durable provider boundary we must kill the client during.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if provider_started.exists() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Cook did not start provider work: child log={} client stdout={} stderr={}",
            std::fs::read_to_string(
                context
                    .data_dir()
                    .join("agent-task-detached")
                    .join(cook_id)
                    .join("cook.log"),
            )
            .unwrap_or_default(),
            std::fs::read_to_string(&client_stdout).unwrap_or_default(),
            std::fs::read_to_string(&client_stderr).unwrap_or_default(),
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    let mut status = context.controller_runtime_command(TestBinary::HomeboyFixture);
    status.args(["agent-task", "status", cook_id, "--full"]);
    let provider_status = bounded_output(status);
    let provider_status_json = serde_json::from_slice::<serde_json::Value>(&provider_status.stdout)
        .expect("provider status JSON");
    assert!(
        provider_status.status.success()
            && provider_status_json
                .pointer("/data/status_scope/queried_attempt/state")
                .and_then(serde_json::Value::as_str)
                == Some("running"),
        "Cook did not durably record running provider work: {provider_status_json}",
    );

    client.kill().expect("terminate observing client");
    client.wait().expect("reap observing client");

    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if std::fs::read_to_string(
            context
                .data_dir()
                .join("agent-task-detached")
                .join(cook_id)
                .join("cook.log"),
        )
        .unwrap_or_default()
        .contains("Cook terminal")
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Cook did not terminalize after client termination"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let mut status = context.controller_runtime_command(TestBinary::HomeboyFixture);
    status.args(["agent-task", "status", cook_id, "--full"]);
    let output = bounded_output(status);
    let completed = String::from_utf8_lossy(&output.stdout).into_owned();
    let completed_json =
        serde_json::from_str::<serde_json::Value>(&completed).expect("completed status JSON");
    let terminal_provider_success = completed_json
        .pointer("/data/action_eligibility/state")
        .and_then(serde_json::Value::as_str)
        == Some("succeeded")
        && completed_json
            .pointer("/data/status_scope/queried_attempt/child_run_state")
            .and_then(serde_json::Value::as_str)
            == Some("succeeded")
        && completed_json
            .pointer("/data/status_scope/queried_attempt/artifacts/count")
            .and_then(serde_json::Value::as_u64)
            .is_some_and(|count| count > 0);
    assert!(
        output.status.success() && terminal_provider_success,
        "Cook did not complete after client termination: {completed}"
    );
    let completed_run_id = completed_json
        .pointer("/data/action_eligibility/run_id")
        .and_then(serde_json::Value::as_str)
        .expect("completed run id");
    let mut artifacts = context.controller_runtime_command(TestBinary::HomeboyFixture);
    artifacts.args(["agent-task", "artifacts", completed_run_id, "--full"]);
    let artifacts = bounded_output(artifacts);
    assert!(
        artifacts.status.success()
            && String::from_utf8_lossy(&artifacts.stdout).contains("changes.patch"),
        "terminal patch retained: {}",
        String::from_utf8_lossy(&artifacts.stdout)
    );
}

/// A retry can die after reserving its successor but before it spawns the
/// detached child. A later retry must reclaim that dead launcher, rather than
/// stranding the queued reservation or launching duplicate provider work.
fn local_retry_reclaims_a_dead_launcher_at_handoff_stage(crash_env: &str) {
    let context = HermeticTestContext::new();
    let (_checkout_guard, checkout) =
        homeboy_core::test_support::shared_committed_git_repo_fixture("local-retry-durability");
    let component_id = "local-retry-durability";
    let mut register = context.command(TestBinary::HomeboyFixture);
    register.args([
        "component",
        "create",
        "--local-path",
        checkout.to_str().expect("checkout path"),
    ]);
    assert!(
        bounded_output(register).status.success(),
        "register component"
    );
    let task_worktree = context.root().join("local-retry-durability-worktree");
    homeboy_core::test_support::run_git_fixture_command(
        &checkout,
        &[
            "worktree",
            "add",
            "-b",
            "local-retry-durability",
            task_worktree.to_str().expect("task worktree path"),
        ],
    );
    std::fs::write(
        context.config_dir().join("homeboy.json"),
        r#"{"retention":{"reconstructable_artifact_reserve_bytes":0}}"#,
    )
    .expect("disable host-capacity admission for fixture worktree");

    let cook_id = format!(
        "local-retry-durability-{}",
        crash_env
            .strip_prefix("HOMEBOY_TEST_LOCAL_COOK_RETRY_PAUSE_AFTER_")
            .unwrap_or("handoff")
            .to_ascii_lowercase()
    );
    let source_started = context.root().join("source-provider-started");
    let mut source = context.controller_runtime_command(TestBinary::HomeboyFixture);
    source
        .env("GITHUB_ACTIONS", "true")
        .env("HOMEBOY_FIXTURE_PROVIDER_DELAY_MS", "20000")
        .env("HOMEBOY_FIXTURE_PROVIDER_STARTED_FILE", &source_started)
        .args([
            "agent-task",
            "cook",
            "--run-id",
            &cook_id,
            "--repo",
            component_id,
            "--backend",
            "fixture",
            "--prompt",
            "make the retry durable",
            "--cwd",
            task_worktree.to_str().expect("task worktree path"),
            "--to-worktree",
            task_worktree.to_str().expect("task worktree path"),
            "--verify",
            "true",
            "--max-attempts",
            "2",
            "--no-finalize",
        ]);
    let mut source = source.spawn().expect("start source Cook");
    let deadline = Instant::now() + Duration::from_secs(60);
    while !source_started.exists() {
        assert!(
            Instant::now() < deadline,
            "source Cook did not reserve provider work"
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    let mut source_status = context.controller_runtime_command(TestBinary::HomeboyFixture);
    source_status.args(["agent-task", "status", &cook_id, "--full"]);
    let source_status = bounded_output(source_status);
    let source_status_json = serde_json::from_slice::<serde_json::Value>(&source_status.stdout)
        .expect("source status JSON");
    let source_run_id = source_status_json
        .pointer("/data/action_eligibility/run_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("source durable run id: {source_status_json}"))
        .to_string();
    let mut cancel = context.controller_runtime_command(TestBinary::HomeboyFixture);
    cancel.args([
        "agent-task",
        "cancel",
        &source_run_id,
        "--reason",
        "prepare retry fixture",
    ]);
    assert!(
        bounded_output(cancel).status.success(),
        "cancel source Cook attempt"
    );
    source.wait().expect("reap source client");
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
    assert!(
        lifecycle_store
            .read_record(&source_run_id)
            .expect("read cancelled source")
            .state
            .is_terminal(),
        "source must terminalize before its retry is reserved"
    );

    let retry_started = context.root().join("retry-provider-started");
    let retry_stdout = context.root().join("retry-client.stdout");
    let mut retry = context.controller_runtime_command(TestBinary::HomeboyFixture);
    retry
        .env("GITHUB_ACTIONS", "true")
        .env(crash_env, "1")
        .args([
            "--placement",
            "local",
            "agent-task",
            "retry",
            &source_run_id,
            "--run",
        ])
        .stdout(Stdio::from(
            std::fs::File::create(&retry_stdout).expect("create retry stdout"),
        ));
    let mut retry = retry.spawn().expect("start local retry client");
    let deadline = Instant::now() + Duration::from_secs(60);
    let retry_run_id = format!("{source_run_id}-retry");
    let expected_handoff_state =
        if crash_env == "HOMEBOY_TEST_LOCAL_COOK_RETRY_PAUSE_AFTER_RESERVATION" {
            "pending"
        } else {
            "child_spawned"
        };
    while lifecycle_store
        .read_record(&retry_run_id)
        .ok()
        .is_none_or(|record| {
            record.metadata["local_cook_supervisor"]["state"].as_str()
                != Some(expected_handoff_state)
                || record.metadata["local_cook_supervisor"]["launcher_pid"]
                    .as_u64()
                    .is_none()
                || record.metadata["local_cook_supervisor"]["launcher_process_start_identity"]
                    .is_null()
        })
    {
        assert!(
            Instant::now() < deadline,
            "local retry did not persist a verifiable pending launcher: {}",
            std::fs::read_to_string(&retry_stdout).unwrap_or_default(),
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let launcher_pid = lifecycle_store
        .read_record(&retry_run_id)
        .expect("read persisted retry launcher")
        .metadata["local_cook_supervisor"]["launcher_pid"]
        .as_u64()
        .and_then(|pid| i32::try_from(pid).ok())
        .expect("persisted retry launcher pid");

    // The launcher is deliberately paused after the durable reservation, before
    // it can spawn a child or project a supervisor.
    assert!(
        retry.try_wait().expect("observe retry client").is_none(),
        "retry client returned before the reservation could be interrupted"
    );
    assert_eq!(
        unsafe { libc::kill(launcher_pid, libc::SIGKILL) },
        0,
        "terminate only the persisted retry launcher",
    );
    let _ = retry.wait();

    let mut takeover = context.controller_runtime_command(TestBinary::HomeboyFixture);
    takeover
        .env("GITHUB_ACTIONS", "true")
        .env("HOMEBOY_FIXTURE_PROVIDER_DELAY_MS", "10000")
        .env("HOMEBOY_FIXTURE_PROVIDER_STARTED_FILE", &retry_started)
        .args([
            "--placement",
            "local",
            "agent-task",
            "retry",
            &source_run_id,
            "--run",
        ]);
    let takeover = bounded_output(takeover);
    assert!(
        takeover.status.success(),
        "retry takeover failed: {}",
        String::from_utf8_lossy(&takeover.stdout)
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    let running_record = loop {
        let record = lifecycle_store
            .read_record(&retry_run_id)
            .expect("read retry record at provider boundary");
        if record.metadata["local_cook_supervisor"]["state"].as_str() == Some("supervising") {
            break record;
        }
        assert!(
            Instant::now() < deadline,
            "retry did not project its pinned supervisor: {record:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(
        running_record.metadata["local_cook_supervisor"]["state"].as_str(),
        Some("supervising"),
        "retry must replace its queued pending owner with the pinned supervisor",
    );
    assert!(
        running_record.metadata["local_cook_supervisor"]["job_id"]
            .as_str()
            .is_some(),
        "retry supervisor must have a durable job id"
    );
    let supervisor_job_id = running_record.metadata["local_cook_supervisor"]["job_id"]
        .as_str()
        .expect("retry supervisor job id")
        .parse()
        .expect("retry supervisor UUID");

    let mut reconcile = context.controller_runtime_command(TestBinary::HomeboyFixture);
    reconcile.args(["agent-task", "reconcile", &retry_run_id, "--apply"]);
    let reconciled = bounded_output(reconcile);
    assert!(
        !String::from_utf8_lossy(&reconciled.stdout).contains("missing_runner_pid"),
        "pinned supervisor must prevent ownerless cancellation: {}",
        String::from_utf8_lossy(&reconciled.stdout),
    );

    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let mut status = context.controller_runtime_command(TestBinary::HomeboyFixture);
        status.args(["agent-task", "status", &retry_run_id, "--full"]);
        let output = bounded_output(status);
        let record = lifecycle_store
            .read_record(&retry_run_id)
            .expect("read retry record");
        if record.state == homeboy::agents::agent_task_lifecycle::AgentTaskRunState::Succeeded
            && !record.artifact_refs.is_empty()
        {
            assert!(output.status.success());
            assert!(
                !record.artifact_refs.is_empty(),
                "terminal artifacts retained"
            );
            assert_eq!(
                record.metadata["provider_executions_consumed"].as_u64(),
                Some(1),
                "only the reclaimed launcher may execute the provider"
            );
            assert!(
                !String::from_utf8_lossy(&output.stdout).contains("missing_runner_pid"),
                "terminal retry must retain its supervisor-owned finalization"
            );
            assert_eq!(
                record.metadata["local_cook_supervisor"]["pinned_run_id"].as_str(),
                Some(retry_run_id.as_str()),
                "terminal retry must retain its exact supervisor identity"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "local retry did not complete: {record:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let store = JobStore::open_without_reconciliation(context.daemon_dir().join("jobs.json"))
            .expect("open supervisor job store");
        if store
            .get(supervisor_job_id)
            .expect("read retry supervisor job")
            .status
            == JobStatus::Succeeded
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "retry supervisor did not complete"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Historical runner recovery belongs to `runner exec`, never an unrelated
/// Cook admission. This invokes the actual detached Cook command path so the
/// ownership boundary remains observable.
#[test]
fn detached_cook_admission_does_not_schedule_unrelated_recovery_records() {
    let context = HermeticTestContext::new();
    let database = context.data_dir().join("homeboy.sqlite");
    let store = ObservationStore::open_initialized_at(&database).expect("open fixture store");
    for index in 0..100 {
        store
            .start_run_with_id(
                NewRunRecord::builder("runner_execution")
                    .cwd_path(Path::new("/workspace"))
                    .metadata(serde_json::json!({
                        "kind": "runner_exec",
                        "runner_id": "unavailable-runner",
                        "runner_job_id": format!("stale-job-{index}"),
                    }))
                    .build(),
                format!("stale-runner-exec-{index}"),
            )
            .expect("seed stale runner execution");
    }
    drop(store);

    let mut command = context.controller_runtime_command(TestBinary::HomeboyFixture);
    command
        .env("HOMEBOY_COOK_DETACH_HANDOFF_TIMEOUT_MS", "5000")
        .args([
            "--placement",
            "local",
            "--detach-after-handoff",
            "agent-task",
            "cook",
            "--backend",
            "fixture",
            "--prompt",
            "admit despite stale runner records",
            "--to-worktree",
            "missing@worktree",
            "--verify",
            "true",
        ]);
    let started = Instant::now();
    let output = bounded_output(command);
    // The handoff itself is capped at five seconds above. Leave enough room for
    // concurrent controller-runtime pins in this integration binary; this
    // bound detects recovery blocking rather than host-local fixture startup.
    assert!(
        started.elapsed() < Duration::from_secs(60),
        "Cook admission must not wait for stale daemon recovery"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert!(stdout.contains("\"state\": \"accepted\""), "{stdout}");

    let store = ObservationStore::open_initialized_at(&database).expect("reopen fixture store");
    assert!(store
        .list_runs(RunListFilter {
            kind: Some("runner_exec_recovery".to_string()),
            ..RunListFilter::default()
        })
        .expect("list recovery owners")
        .is_empty());
    assert!(store
        .list_runs(RunListFilter {
            kind: Some("runner_exec_recovery_child".to_string()),
            ..RunListFilter::default()
        })
        .expect("list recovery children")
        .is_empty());
}

#[test]
fn cook_rejects_queue_only_before_worktree_resolution() {
    let output = homeboy(&[
        "agent-task",
        "cook",
        "--backend",
        "fixture",
        "--prompt",
        "implement the fix",
        "--to-worktree",
        "missing@worktree",
        "--verify",
        "true",
        "--queue-only",
    ]);

    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("cannot queue its controller-owned lifecycle"));
    assert!(!stdout.contains("worktree provider"));
}

#[test]
fn required_lab_route_without_a_selected_runner_fails_deterministically() {
    let context = HermeticTestContext::new();
    let mut command = context.controller_runtime_command(TestBinary::HomeboyFixture);
    command
        // Resource admission is tested independently. This routing test needs
        // the no-runner diagnostic even when the host is deliberately warm.
        .env("GITHUB_ACTIONS", "true")
        .args(["--placement", "lab", "review", "lint"]);
    let output = bounded_output(command);

    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("required Lab placement has no selected ready runner")
            || stderr.contains("required Lab placement has no selected ready runner"),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(!stdout.contains("worktree provider"));
    assert!(!stderr.contains("worktree provider"));
}

#[test]
fn cook_help_does_not_advertise_queue_only() {
    let output = homeboy(&["agent-task", "cook", "--help"]);

    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(!help.contains("\n      --queue-only\n"));
    assert!(help.contains("--help-full"), "{help}");

    let output = homeboy(&["agent-task", "cook", "--help-full"]);
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(!help.contains("\n      --queue-only\n"));
    assert!(help.contains("--detach-after-handoff"), "{help}");
}
