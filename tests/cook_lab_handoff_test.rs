use std::path::Path;
use std::process::Output;
use std::time::{Duration, Instant};

use homeboy_core::observation::{NewRunRecord, ObservationStore, RunListFilter, RunStatus};
use homeboy_core::test_support::{bounded_output, HermeticTestContext, TestBinary};

/// Run the fixture binary through the shared hermetic harness.
///
/// Every case in this file asserts what an invocation settles *before* any
/// worktree or provider resolution: a contradictory combination is rejected,
/// and a locally-detached Cook is handed off (#11476).
///
/// `HermeticTestContext::command` owns the complete isolation contract — HOME,
/// XDG config and data roots, artifact root, runtime and temp dirs — and strips
/// the Lab transport variables. Overriding only HOME on a hand-rolled `Command`
/// leaves that leak open (#10717, #10718). These cases run with a default
/// operator context; the transport variables are injected explicitly only by
/// the case that asserts they cannot change a validation outcome (#10917).
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

/// `--placement local --detach-after-handoff` is served, not rejected (#11476).
///
/// The launcher hands the Cook to a process in its own session and returns a
/// bounded handoff naming the durable handle. It performs no worktree or
/// provider resolution itself — that belongs to the detached cook — so this
/// still asserts the fast, work-free return the old rejection guaranteed.
#[test]
fn cook_detaches_local_placement_instead_of_rejecting_it() {
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
            "--prompt",
            "implement the fix",
            "--to-worktree",
            "missing@worktree",
            "--verify",
            "true",
        ]);
    let output = bounded_output(command);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert!(
        !stdout.contains("cannot detach after handoff with --placement local"),
        "{stdout}"
    );
    assert!(!stdout.contains("worktree provider"), "{stdout}");

    // The envelope is the last thing the launcher writes, so parse from the
    // first object brace rather than assuming stdout holds nothing else.
    let envelope_start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("handoff envelope is present\n{stdout}"));
    let envelope: serde_json::Value = serde_json::from_str(stdout[envelope_start..].trim())
        .unwrap_or_else(|error| panic!("handoff envelope is JSON: {error}\n{stdout}"));
    assert_eq!(
        envelope["schema"], "homeboy/agent-task-cook-local-detach-handoff/v1",
        "{stdout}"
    );
    assert_eq!(envelope["placement"], "local", "{stdout}");
    assert_eq!(envelope["detached"], true, "{stdout}");
    let cook_id = envelope["cook_id"]
        .as_str()
        .unwrap_or_else(|| panic!("handoff names a cook id\n{stdout}"));
    assert!(!cook_id.is_empty(), "{stdout}");
    assert_eq!(
        envelope["status_command"],
        format!("homeboy agent-task status {cook_id}"),
        "{stdout}"
    );
    let pid = envelope["pid"]
        .as_u64()
        .unwrap_or_else(|| panic!("handoff names the detached pid\n{stdout}"));
    assert!(pid > 0, "{stdout}");

    // Never leave a detached process behind a test. An unresolvable destination
    // normally kills it well before this, so this is belt-and-braces cleanup.
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
}

/// Historical runner recovery is a distinct owner, never an admission gate for
/// an accepted Cook. This invokes the actual detached Cook command path rather
/// than a scheduler helper so its output and durable handoff remain observable.
#[test]
fn detached_cook_admission_is_bounded_with_a_hundred_unavailable_recovery_records() {
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
            "--prompt",
            "admit despite stale runner records",
            "--to-worktree",
            "missing@worktree",
            "--verify",
            "true",
        ]);
    let started = Instant::now();
    let output = bounded_output(command);
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "Cook admission must not wait for stale daemon recovery"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    let handoff_start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("Cook handoff is durable output\n{stdout}"));
    let handoff: serde_json::Value = serde_json::from_str(stdout[handoff_start..].trim())
        .unwrap_or_else(|error| panic!("Cook handoff is JSON: {error}\n{stdout}"));
    assert_eq!(handoff["detached"], true, "{stdout}");
    assert!(handoff["cook_id"].as_str().is_some_and(|id| !id.is_empty()));

    let store = ObservationStore::open_initialized_at(&database).expect("reopen fixture store");
    let owners = store
        .list_runs(RunListFilter {
            kind: Some("runner_exec_recovery".to_string()),
            ..RunListFilter::default()
        })
        .expect("list recovery owners");
    assert_eq!(owners.len(), 1, "recovery has its own durable owner");
    assert_eq!(owners[0].status, RunStatus::Pass.as_str());
    assert_eq!(owners[0].metadata_json["deferred_count"], 100);

    #[cfg(unix)]
    if let Some(pid) = handoff["pid"].as_u64() {
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGKILL);
        }
    }
}

#[test]
fn non_tty_client_must_choose_one_lab_observation_mode() {
    // Command::output supplies pipes, matching an interruptible bridge client
    // rather than an interactive terminal. Reject ambiguity before any worktree
    // or provider work can begin.
    let output = homeboy(&[
        "--wait",
        "--detach-after-handoff",
        "agent-task",
        "cook",
        "--prompt",
        "implement the fix",
        "--to-worktree",
        "missing@worktree",
        "--verify",
        "true",
    ]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cannot be used with"), "{stderr}");
    assert!(!stderr.contains("worktree provider"), "{stderr}");
}

#[test]
fn cook_rejects_queue_only_before_worktree_resolution() {
    let output = homeboy(&[
        "agent-task",
        "cook",
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
    let output = homeboy(&["--placement", "lab", "review", "lint"]);

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
    assert!(help.contains("--detach-after-handoff"), "{help}");
    assert!(help.contains("--wait"), "{help}");
}
