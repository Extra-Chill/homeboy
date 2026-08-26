//! Local-placement Cook detachment.
//!
//! Cook advertises a durable run id "persisted before materialization" as the
//! handle that makes a cook observable and recoverable independently of the
//! caller. On Lab placement a runner daemon owns the provider attempt, so that
//! promise holds. On local placement it did not: the cook ran in the calling
//! client's process group, so a client that hit its command timeout took the
//! cook down with it — `local_provider_ownership.state: "owner_dead"`, the run
//! cancelled, the durable record good only for reading a tombstone (#11476).
//!
//! The remedy is the one the operator was reaching for by hand with `setsid
//! nohup ... &`: the launcher re-executes the same Cook in its own session and
//! returns a bounded handoff. Nothing about the cook's own lifecycle changes —
//! it is the same controller-owned cook, executing the same argv, writing the
//! same durable records. Only its parentage does.
//!
//! This composes with, rather than contradicts, provider process-group
//! containment (#11477): containment makes a cook's children die *with* the
//! cook; detachment makes the cook survive *its launcher*. The detached cook is
//! the containment owner, so a cancelled or killed cook still reaps its
//! provider tree.
//!
//! # Daemon ownership
//!
//! Detachment alone left the cook PID-owned: durable enough to read, but with
//! no job record, no checkpoint, and no authority that outlived the launcher.
//! The launcher now also submits the cook to the daemon as a typed controller
//! job (`agent_task_service::CookJobDriver`), so the daemon owns the lifecycle
//! — durable record, checkpointing, cancellation, and HTTP inspection — while
//! this launcher still spawns the child.
//!
//! Who spawns the child is the load-bearing detail, not an implementation
//! accident. The ambient process environment is the first secret source a
//! provider invocation consults, and the daemon inherits its environment and
//! working directory from whichever caller first started it. Spawning the cook
//! from the daemon would silently change which credentials the provider gets.
//! Spawning it here preserves the operator's environment exactly, which is why
//! the daemon supervises a child it did not create.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use homeboy::agents::agent_tasks::lifecycle as agent_task_lifecycle;
use homeboy::cli_surface::{Cli, Commands};
use homeboy::core::Error;
use serde_json::{json, Value};

use crate::core::io::output_file::write_output_file;

const HANDOFF_SCHEMA: &str = "homeboy/agent-task-cook-local-detach-handoff/v1";

/// Bound on how long the launcher waits for the detached cook to materialize
/// its first executable attempt before reporting the handoff as pending.
///
/// The placeholder and controller job make the Cook addressable before this
/// boundary, but they do not prove the child has durable execution ownership.
/// `accepted` is reserved for that later proof. A zero wait consequently emits
/// a pending handoff, never an unproven acceptance.
const DEFAULT_HANDOFF_TIMEOUT_MS: u64 = 30_000;
const HANDOFF_POLL: Duration = Duration::from_millis(100);

/// Test and operator override for the bounded handoff wait.
const HANDOFF_TIMEOUT_ENV: &str = "HOMEBOY_COOK_DETACH_HANDOFF_TIMEOUT_MS";
const LOCAL_COOK_LAUNCH_TOKEN_ENV: &str = "HOMEBOY_LOCAL_COOK_LAUNCH_TOKEN";
const LOCAL_COOK_LAUNCH_TOKEN_PATH_ENV: &str = "HOMEBOY_LOCAL_COOK_LAUNCH_TOKEN_PATH";
const LOCAL_COOK_SUPERVISOR_JOB_ID_ENV: &str = "HOMEBOY_LOCAL_COOK_SUPERVISOR_JOB_ID";
// Hermetic E2E control: keep the launcher observable after ownership was
// accepted so the test can signal it. Normal invocations still return at handoff.
const TEST_LOCAL_COOK_RETRY_FOLLOW_ENV: &str = "HOMEBOY_TEST_LOCAL_COOK_RETRY_FOLLOW";
const TEST_LOCAL_COOK_RETRY_PAUSE_AFTER_RESERVATION_ENV: &str =
    "HOMEBOY_TEST_LOCAL_COOK_RETRY_PAUSE_AFTER_RESERVATION";
const TEST_LOCAL_COOK_RETRY_PAUSE_AFTER_SPAWN_ENV: &str =
    "HOMEBOY_TEST_LOCAL_COOK_RETRY_PAUSE_AFTER_SPAWN";
const TEST_LOCAL_COOK_RETRY_PAUSE_AFTER_SUBMIT_ENV: &str =
    "HOMEBOY_TEST_LOCAL_COOK_RETRY_PAUSE_AFTER_SUBMIT";

/// Serve the one local retry route that has an existing Cook owner. Generic
/// retries, Lab retries, runner-side commands, and a retry that only reserves
/// a successor retain their established routes.
pub(super) fn intercept_local_cook_retry(
    cli: &Cli,
    normalized_args: &[String],
    runner_side: bool,
) -> homeboy::core::Result<Option<i32>> {
    let Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
        command: crate::commands::agent_task::AgentTaskCommand::Retry(retry),
    }) = &cli.command
    else {
        return Ok(None);
    };
    if !retry.run || runner_side || cli.placement != homeboy::cli_surface::Placement::Local {
        return Ok(None);
    }
    // The child cannot route the retry until its parent has atomically published
    // this unique readiness token after recording the exact-run supervisor.
    if local_cook_launch_token_is_present() {
        return if await_local_cook_launch_token(handoff_timeout()) {
            Ok(None)
        } else {
            Err(Error::validation_invalid_argument(
                "retry",
                "local Cook retry readiness token was not published before the bounded handoff deadline",
                None,
                None,
            ))
        };
    }
    let source = match agent_task_lifecycle::status(&retry.run_id) {
        Ok(record) => record,
        Err(_) => return Ok(None),
    };
    let Some(cook_id) = source.metadata["cook_id"].as_str() else {
        return Ok(None);
    };
    if source.runner_id().is_some() || source.runner_job_id().is_some() {
        return Ok(None);
    }
    // This guard must precede retry reservation: unsupported hosts retain the
    // ordinary retry path without leaving an unowned queued successor behind.
    if !local_cook_supervision_supported() {
        return Err(Error::validation_invalid_argument(
            "placement",
            "local Cook retry supervision requires a platform with session detachment and exact process start identity support",
            Some(source.run_id),
            None,
        ));
    }

    // Every fallible resource needed to launch is prepared before reserving a
    // successor. A preflight error must not leave an ownerless queued retry.
    let controller_client =
        homeboy::core::daemon::LocalControllerJobClient::connect_current_build()?;
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    let session_root = detached_session_root(&format!(
        "{}-retry-launch-{}",
        source.run_id,
        uuid::Uuid::new_v4()
    ))?;
    let launch_token = new_local_cook_launch_token(&session_root);
    let _launch_token_cleanup = LocalCookLaunchTokenCleanup(launch_token.1.clone());
    let log_path = session_root.join("cook-retry.log");

    // A crashed launcher can leave its successor queued in `child_spawned`.
    // Recover that exact durable reservation before asking normal retry
    // admission, which correctly rejects a generic request while work is queued.
    let existing_retry = agent_task_lifecycle::list_records()?
        .into_iter()
        .find(|record| {
            record.metadata["retry_of"].as_str() == Some(retry.run_id.as_str())
                && record.state == agent_task_lifecycle::AgentTaskRunState::Queued
                && matches!(
                    record.metadata["local_cook_supervisor"]["state"].as_str(),
                    Some("pending") | Some("child_spawned")
                )
        });
    let (retry_runs, retry_record) = match existing_retry {
        Some(record) => (true, record),
        None => {
            let result = homeboy::agents::agent_task_service::retry(
                &retry.run_id,
                retry.new_run_id.as_deref(),
                true,
                retry.force,
            )?;
            (result.run, result.record)
        }
    };
    if !retry_runs || retry_record.state.is_terminal() {
        println!(
            "{}",
            serde_json::to_string(&retry_record).unwrap_or_default()
        );
        return Ok(Some(0));
    }
    let run_id = retry_record.run_id;
    let child = match agent_task_lifecycle::claim_local_cook_retry_launch_in_store(
        &lifecycle_store,
        &run_id,
        cook_id,
    )? {
        agent_task_lifecycle::LocalCookRetryLaunchClaim::Acquired => None,
        agent_task_lifecycle::LocalCookRetryLaunchClaim::ChildSpawned {
            pid,
            start_identity,
            launch_token,
            launch_token_path,
        } => Some((
            pid,
            start_identity,
            (launch_token, PathBuf::from(launch_token_path)),
        )),
        agent_task_lifecycle::LocalCookRetryLaunchClaim::ChildExited => {
            record_retry_launcher_failure_for_run(
                &run_id,
                "local Cook retry child exited before durable supervision",
            );
            return Ok(Some(0));
        }
        // A live owner has the same pinned reservation and will establish the
        // supervisor. Returning success makes concurrent retries converge.
        agent_task_lifecycle::LocalCookRetryLaunchClaim::OwnedElsewhere
        | agent_task_lifecycle::LocalCookRetryLaunchClaim::RecoveryPending
        | agent_task_lifecycle::LocalCookRetryLaunchClaim::NotPending => {
            println!(
                "{}",
                serde_json::to_string(
                    &agent_task_lifecycle::status_in_store(
                        &lifecycle_store,
                        &run_id,
                        agent_task_lifecycle::AgentTaskStatusOptions::default(),
                        false,
                    )?
                    .record
                )
                .unwrap_or_default()
            );
            return Ok(Some(0));
        }
    };
    // A takeover must continue through the recovered stage, not reproduce the
    // crash fixture that stopped its dead predecessor.
    let is_initial_launcher = lifecycle_store.read_record(&run_id)?.metadata
        ["local_cook_supervisor"]["launcher_reclaimed_at"]
        .is_null();
    if is_initial_launcher
        && std::env::var_os(TEST_LOCAL_COOK_RETRY_PAUSE_AFTER_RESERVATION_ENV).is_some()
    {
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    let plan = match agent_task_lifecycle::load_plan(&run_id) {
        Ok(plan) => plan,
        Err(error) => {
            record_retry_launcher_failure_for_run(
                &run_id,
                "local Cook retry plan could not be loaded",
            );
            return Err(error);
        }
    };
    let (pid, start_identity, launch_token) = match child {
        Some(child) => child,
        None => {
            let route = detached_route(cli);
            let child_args = retry_child_args(normalized_args, &run_id);
            let mut child =
                match spawn_detached_cook(&child_args, &log_path, route.as_ref(), &launch_token) {
                    Ok(child) => child,
                    Err(error) => {
                        record_retry_launcher_failure(
                            &lifecycle_store,
                            &run_id,
                            &plan,
                            "local Cook retry could not be spawned",
                        );
                        return Err(error);
                    }
                };
            let pid = child.id();
            let start_identity = match detached_child_start_identity(pid) {
                Ok(identity) => identity,
                Err(error) => {
                    terminate_and_reap_detached_child(&mut child);
                    record_retry_launcher_failure(
                        &lifecycle_store,
                        &run_id,
                        &plan,
                        "local Cook retry child start identity could not be captured",
                    );
                    return Err(error);
                }
            };
            if let Err(error) = agent_task_lifecycle::record_local_cook_retry_child_in_store(
                &lifecycle_store,
                &run_id,
                cook_id,
                pid,
                start_identity.clone(),
                &launch_token.0,
                &launch_token.1.display().to_string(),
            ) {
                terminate_and_reap_detached_child(&mut child);
                record_retry_launcher_failure(
                    &lifecycle_store,
                    &run_id,
                    &plan,
                    "local Cook retry child identity could not be persisted",
                );
                return Err(error);
            }
            if is_initial_launcher
                && std::env::var_os(TEST_LOCAL_COOK_RETRY_PAUSE_AFTER_SPAWN_ENV).is_some()
            {
                loop {
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
            (pid, start_identity, launch_token)
        }
    };
    let controller_job = match submit_cook_retry_controller_job(
        &controller_client,
        cook_id,
        &run_id,
        pid,
        &start_identity,
    ) {
        Ok(job) => job,
        Err(error) => {
            record_retry_launcher_failure(
                &lifecycle_store,
                &run_id,
                &plan,
                "durable local Cook retry supervision could not be established",
            );
            return Err(error);
        }
    };
    if is_initial_launcher
        && std::env::var_os(TEST_LOCAL_COOK_RETRY_PAUSE_AFTER_SUBMIT_ENV).is_some()
    {
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    if let Err(error) = agent_task_lifecycle::record_local_cook_retry_supervisor_in_store(
        &lifecycle_store,
        &run_id,
        cook_id,
        controller_job.job_id(),
    ) {
        let _ = controller_client.cancel(
            controller_job.job_id(),
            "local Cook retry supervisor projection failed",
        );
        record_retry_launcher_failure(
            &lifecycle_store,
            &run_id,
            &plan,
            "local Cook retry supervisor projection failed",
        );
        return Err(error);
    }
    if let Err(error) = publish_local_cook_launch_token(&launch_token) {
        let _ = controller_client.cancel(
            controller_job.job_id(),
            "local Cook retry readiness publication failed",
        );
        record_retry_launcher_failure(
            &lifecycle_store,
            &run_id,
            &plan,
            "local Cook retry readiness publication failed",
        );
        return Err(error);
    }
    if let Err(error) = await_consumed_retry_launch_token_by_identity(
        pid,
        &start_identity,
        &launch_token.1,
        handoff_timeout(),
    ) {
        let _ = controller_client.cancel(
            controller_job.job_id(),
            "local Cook retry did not consume its launch readiness token",
        );
        record_retry_launcher_failure(
            &lifecycle_store,
            &run_id,
            &plan,
            "local Cook retry did not consume its launch readiness token",
        );
        return Err(error);
    }
    println!(
        "{}",
        serde_json::to_string(&json!({
            "run_id": run_id,
            "controller_job": controller_job.projection(),
            "state": "accepted",
        }))
        .unwrap_or_default()
    );
    if std::env::var_os(TEST_LOCAL_COOK_RETRY_FOLLOW_ENV).as_deref()
        == Some(std::ffi::OsStr::new("1"))
    {
        // A recovered launcher no longer owns a `Child` handle. The durable
        // supervisor is the authority, so normal retry handoff returns here.
        return Ok(Some(0));
    }
    Ok(Some(0))
}

fn await_consumed_retry_launch_token_by_identity(
    pid: u32,
    start_identity: &homeboy::core::process::ProcessStartIdentity,
    token_path: &Path,
    timeout: Duration,
) -> homeboy::core::Result<()> {
    let deadline = Instant::now() + timeout;
    while token_path.exists() {
        if !matches!(
            homeboy::core::process::process_identity_state_with_start_identity(
                pid,
                None,
                Some(start_identity)
            ),
            homeboy::core::process::ProcessIdentityState::Live
        ) {
            return Err(Error::validation_invalid_argument(
                "retry",
                "local Cook retry child exited before consuming its readiness token",
                None,
                None,
            ));
        }
        if Instant::now() >= deadline {
            return Err(Error::validation_invalid_argument("retry", "local Cook retry launcher did not consume its readiness token before the bounded handoff deadline", None, None));
        }
        std::thread::sleep(HANDOFF_POLL);
    }
    Ok(())
}

fn record_retry_launcher_failure_for_run(run_id: &str, reason: &str) {
    let Ok(lifecycle_store) =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()
    else {
        return;
    };
    if let Ok(plan) = agent_task_lifecycle::load_plan(run_id) {
        record_retry_launcher_failure(&lifecycle_store, run_id, &plan, reason);
    } else {
        // A corrupt or missing retry plan cannot produce an aggregate, but the
        // reserved successor must still stop looking runnable.
        let _ = agent_task_lifecycle::cancel_run(run_id, Some(reason));
    }
}

fn record_retry_launcher_failure(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
    plan: &homeboy::agents::agent_tasks::scheduler::AgentTaskPlan,
    reason: &str,
) {
    let _ = agent_task_lifecycle::record_pre_execution_failure_in_store(
        lifecycle_store,
        run_id,
        plan,
        "local_retry_supervisor",
        &Error::internal_unexpected(reason),
    );
}

/// Pin the detached child to the reservation its parent won. Replaying an
/// unpinned retry can otherwise resolve a later Cook successor after a
/// concurrent caller advances the recipe.
fn retry_child_args(normalized_args: &[String], run_id: &str) -> Vec<String> {
    let mut args = Vec::with_capacity(normalized_args.len() + 2);
    let mut index = usize::from(
        normalized_args
            .first()
            .is_some_and(|arg| arg == "homeboy" || arg.ends_with("/homeboy")),
    );
    while index < normalized_args.len() {
        let arg = &normalized_args[index];
        if arg == "--new-run-id" {
            index += 2;
            continue;
        }
        if !arg.starts_with("--new-run-id=") {
            args.push(arg.clone());
        }
        index += 1;
    }
    args.push("--new-run-id".to_string());
    args.push(run_id.to_string());
    args
}

/// Whether this is an unsupervised Cook requesting detached supervision.
///
/// The one-use launch token prevents only that exact child from handing itself
/// off again; ambient environment variables cannot bypass supervision.
fn is_unsupervised_local_cook(cli: &Cli) -> bool {
    cli.detach_after_handoff
        && matches!(
            &cli.command,
            Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
                command: crate::commands::agent_task::AgentTaskCommand::Cook(cook),
            }) if !cook.preview
        )
        && !consume_local_cook_launch_token()
}

fn automatic_local_cook_needs_supervision(cli: &Cli, provider_placement: Option<&str>) -> bool {
    cli.placement == homeboy::cli_surface::Placement::Auto
        && provider_placement == Some("local")
        && matches!(
            &cli.command,
            Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
                command: crate::commands::agent_task::AgentTaskCommand::Cook(cook),
            }) if !cook.preview
        )
        && !consume_local_cook_launch_token()
}

fn consume_local_cook_launch_token() -> bool {
    let (Some(token), Some(path)) = (
        std::env::var_os(LOCAL_COOK_LAUNCH_TOKEN_ENV),
        std::env::var_os(LOCAL_COOK_LAUNCH_TOKEN_PATH_ENV),
    ) else {
        return false;
    };
    let consumed = consume_local_cook_launch_token_at(&token, &PathBuf::from(path));
    if consumed {
        std::env::remove_var(LOCAL_COOK_LAUNCH_TOKEN_ENV);
        std::env::remove_var(LOCAL_COOK_LAUNCH_TOKEN_PATH_ENV);
    }
    consumed
}

fn local_cook_launch_token_is_present() -> bool {
    std::env::var_os(LOCAL_COOK_LAUNCH_TOKEN_ENV).is_some()
        && std::env::var_os(LOCAL_COOK_LAUNCH_TOKEN_PATH_ENV).is_some()
}

fn await_local_cook_launch_token(timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if consume_local_cook_launch_token() {
            return true;
        }
        if Instant::now() >= deadline {
            if let Some(path) = std::env::var_os(LOCAL_COOK_LAUNCH_TOKEN_PATH_ENV) {
                let _ = std::fs::remove_file(PathBuf::from(path));
            }
            return false;
        }
        std::thread::sleep(HANDOFF_POLL);
    }
}

fn consume_local_cook_launch_token_at(token: &std::ffi::OsStr, path: &Path) -> bool {
    let claimed = path.with_extension(format!("consumed-{}", uuid::Uuid::new_v4()));
    if std::fs::rename(path, &claimed).is_err() {
        return false;
    }
    let stored = std::fs::read_to_string(&claimed).ok();
    let valid = stored.as_deref().is_some_and(|stored| {
        if stored.trim_end() == token {
            return true;
        }
        let Ok(publication) = serde_json::from_str::<Value>(stored) else {
            return false;
        };
        if publication["token"].as_str().map(std::ffi::OsStr::new) != Some(token) {
            return false;
        }
        if let Some(job_id) = publication["supervisor_job_id"].as_str() {
            std::env::set_var(LOCAL_COOK_SUPERVISOR_JOB_ID_ENV, job_id);
        }
        true
    });
    let _ = std::fs::remove_file(claimed);
    valid
}

/// Say so when this Cook's provider is about to run inside the caller's own
/// process tree.
///
/// Diagnostics only, and emitted here because this is the one point where the
/// resolved provider placement and the caller's detachment request are both
/// known. It is stated before the paths below can fall back to foreground
/// execution, so an operator hears it whether or not supervision is available. A
/// runner-owned execution is excluded: the runner, not this client, owns that
/// attempt.
fn announce_attached_local_cook_placement(
    cli: &Cli,
    runner_side: bool,
    provider_placement: Option<&str>,
) {
    if runner_side || attached_local_cook_progress_is_suppressed(cli) {
        return;
    }
    let disclosure = crate::commands::agent_task::run::cook_attached_local_placement_disclosure(
        provider_placement,
        cli.detach_after_handoff,
    );
    if let Some(warning) = disclosure {
        eprintln!("{warning}");
    }
}

/// Whether this Cook asked for a quiet submission.
///
/// `--no-progress` suppresses Cook's submission preamble lines, and the attached
/// local placement warning is one of them.
fn attached_local_cook_progress_is_suppressed(cli: &Cli) -> bool {
    let Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
        command: crate::commands::agent_task::AgentTaskCommand::Cook(cook),
    }) = &cli.command
    else {
        return false;
    };
    cook.no_progress
}

/// Durable local supervision needs both a separate child session and an exact
/// process identity for safe cancellation. Platforms without both retain the
/// normal foreground Cook path rather than making an ownership promise they
/// cannot enforce.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn local_cook_supervision_supported() -> bool {
    true
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn local_cook_supervision_supported() -> bool {
    false
}

/// Serve `--detach-after-handoff` by re-executing this exact controller-owned
/// Cook in its own session and returning after durable daemon ownership exists.
///
/// `runner_side` is true when this process is a Lab offload subprocess, a
/// managed-runner placement, or a runner-resident execution. There the request
/// is genuinely unserveable: the process is already the runner's owned
/// execution of one attempt and has no controller lifecycle to hand off, so
/// detaching would orphan work the runner believes it owns.
pub(super) fn intercept_local_detached_cook(
    cli: &Cli,
    normalized_args: &[String],
    output_file: Option<&str>,
    runner_side: bool,
    provider_placement: Option<&str>,
    provider_runner_id: Option<&str>,
) -> homeboy::core::Result<Option<i32>> {
    if !matches!(
        &cli.command,
        Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
            command: crate::commands::agent_task::AgentTaskCommand::Cook(_),
        })
    ) {
        return Ok(None);
    }
    // The detached child cannot enter Cook admission until its parent publishes
    // the one-use token carrying the exact durable supervisor identity.
    if local_cook_launch_token_is_present() {
        return if await_local_cook_launch_token(handoff_timeout()) {
            Ok(None)
        } else {
            Err(empty_detached_plan_error(
                None,
                "detached Cook ownership was not published before the bounded admission deadline",
            ))
        };
    }
    if !is_unsupervised_local_cook(cli)
        && !automatic_local_cook_needs_supervision(cli, provider_placement)
    {
        return Ok(None);
    }
    // Diagnostics only: an attached local Cook shares this client's lifetime and
    // nothing said so (#12570). Placement itself is unchanged.
    announce_attached_local_cook_placement(cli, runner_side, provider_placement);
    if !local_cook_supervision_supported() {
        return if cli.detach_after_handoff {
            Err(Error::validation_invalid_argument(
                "detach-after-handoff",
                "local Cook detachment requires a platform with session detachment and exact process start identity support",
                None,
                None,
            ))
        } else {
            Ok(None)
        };
    }
    if runner_side {
        return if cli.detach_after_handoff {
            Err(runner_side_detach_error())
        } else {
            Ok(None)
        };
    }

    let Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
        command: crate::commands::agent_task::AgentTaskCommand::Cook(cook),
    }) = &cli.command
    else {
        return Ok(None);
    };

    let requested_cook_id = cook.dispatch.run_id.clone();
    let cook_id = requested_cook_id
        .clone()
        .unwrap_or_else(|| format!("cook-detached-{}", uuid::Uuid::new_v4()));
    let mut child_args = detached_cook_child_args(
        normalized_args,
        &cook_id,
        requested_cook_id.is_some(),
        cli.detach_after_handoff,
    );
    let session_root = detached_session_root(&cook_id)?;
    // A detached child cannot answer a `--prompt -`: its stdin is closed and the
    // bytes live only in the launcher's pipe. Capture them here so the exact
    // prompt survives the handoff instead of the cook stalling on an empty read.
    materialize_stdin_prompt(&mut child_args, &session_root)?;
    let log_path = session_root.join("cook.log");

    // This is still before the first durable Cook write, but the validated
    // request is already held in `child_args`. When authoritative daemon status
    // authorizes a confirmation-free idle restart, recover under the exact
    // lease and continue this same request instead of making the operator
    // reconstruct it after `homeboy daemon recover --yes` (#13513).
    let preflight_controller_client = cli
        .detach_after_handoff
        .then(
            homeboy::core::daemon::LocalControllerJobClient::connect_current_build_recovering_idle,
        )
        .transpose()?;

    // A daemon-owned job is the authority that outlives this launcher. Prove it
    // is reachable before a provider-capable child exists, so unsupported
    // detachment is rejected before dispatch.
    let controller_client = match preflight_controller_client {
        Some(client) => client,
        None => match homeboy::core::daemon::LocalControllerJobClient::connect_current_build() {
            Ok(client) => client,
            Err(error) if controller_job_daemon_build_mismatch(&error) => {
                // Attached callers retain foreground ownership when the resident
                // daemon is an older build; #12581 owns that wait-policy path.
                return Ok(None);
            }
            Err(error) => return Err(error),
        },
    };
    let route = detached_route(cli);
    // Keep the token unpublished while the child starts. The child blocks in
    // this interceptor and therefore cannot persist a run before its daemon job.
    let launch_token = new_local_cook_launch_token(&session_root);
    let mut child = match spawn_detached_cook(&child_args, &log_path, route.as_ref(), &launch_token)
    {
        Ok(child) => child,
        Err(error) => return Err(error),
    };
    let pid = child.id();
    let start_identity = match detached_child_start_identity(pid) {
        Ok(identity) => identity,
        Err(error) => {
            terminate_and_reap_detached_child(&mut child);
            return Err(error);
        }
    };
    let controller_job =
        match submit_cook_controller_job(&controller_client, &cook_id, pid, &start_identity) {
            Ok(job) => job,
            Err(error) => {
                terminate_and_reap_detached_child(&mut child);
                return Err(error);
            }
        };
    if let Err(error) =
        publish_local_cook_launch_token_with_supervisor(&launch_token, controller_job.job_id())
    {
        compensate_supervisor_projection_failure(
            &controller_client,
            controller_job.job_id(),
            &mut child,
            &cook_id,
        );
        return Err(error);
    }
    if cli.detach_after_handoff {
        let handoff = await_durable_linked_handoff(
            &cook_id,
            controller_job.job_id(),
            &mut child,
            handoff_timeout(),
        )?;
        if let Some(reason) = detached_handoff_rejection_reason(handoff.state) {
            let _ = controller_client.cancel(controller_job.job_id(), reason);
            terminate_and_reap_detached_child(&mut child);
            return Err(empty_detached_plan_error(
                Some(controller_job.job_id()),
                reason,
            ));
        }
        if let Some(run_id) = handoff.run_id.as_deref() {
            crate::commands::agent_task::run::announce_durable_cook_identity(
                Some(&cook_id),
                run_id,
            );
        }
        let envelope = handoff_envelope(
            &cook_id,
            pid,
            &log_path,
            &handoff,
            &controller_job,
            cli.placement,
            provider_placement.unwrap_or("local"),
            provider_runner_id,
        );
        let stdout = match finalize_handoff_envelope(&envelope, output_file) {
            Ok(stdout) => stdout,
            Err(error) => {
                terminate_and_reap_detached_child(&mut child);
                let _ = controller_client.cancel(
                    controller_job.job_id(),
                    "detached Cook handoff output could not be written",
                );
                return Err(error);
            }
        };
        // `--output` owns the serialized acknowledgement until this point. Emit
        // those exact bytes so both requested destinations carry one contract.
        print!("{stdout}");
        return Ok(Some(0));
    }

    // Attachment is observation only. The controller job and detached child have
    // already accepted ownership, so losing this client cannot cancel provider work.
    let status = stream_attached_cook_log(&mut child, &log_path)?;
    Ok(Some(status.code().unwrap_or(1)))
}

fn controller_job_daemon_build_mismatch(error: &Error) -> bool {
    error.details["classification"] == "controller_job_daemon_build_mismatch"
}

/// Serialize once, then write that exact acknowledgement wherever the caller
/// requested it. This keeps stdout and `--output` from becoming competing
/// handoff contracts.
fn finalize_handoff_envelope(
    envelope: &Value,
    output_file: Option<&str>,
) -> homeboy::core::Result<String> {
    let stdout = serde_json::to_string_pretty(envelope).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize detached local cook handoff".to_string()),
        )
    })?;
    if let Some(path) = output_file {
        write_output_file(path, &stdout)?;
    }
    Ok(stdout)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ControllerJobHandoff {
    Owned { job_id: String },
}

impl ControllerJobHandoff {
    fn job_id(&self) -> &str {
        match self {
            Self::Owned { job_id } => job_id,
        }
    }

    fn projection(&self) -> Value {
        match self {
            Self::Owned { job_id } => json!({ "state": "owned", "job_id": job_id }),
        }
    }
}

/// Offer the detached cook to the daemon as a durable controller job.
///
/// `submit` admits the job and returns its id synchronously; `start` releases
/// the daemon worker that supervises the child. The cook id is the job's
/// idempotency key, so a replayed submit converges on one supervisor rather
/// than creating a second one for the same child.
fn submit_cook_controller_job(
    client: &homeboy::core::daemon::LocalControllerJobClient,
    cook_id: &str,
    pid: u32,
    start_identity: &homeboy::core::process::ProcessStartIdentity,
) -> homeboy::core::Result<ControllerJobHandoff> {
    submit_cook_controller_job_inner(client, cook_id, pid, start_identity)
        .map(|job_id| ControllerJobHandoff::Owned { job_id })
}

fn submit_cook_controller_job_inner(
    client: &homeboy::core::daemon::LocalControllerJobClient,
    cook_id: &str,
    pid: u32,
    start_identity: &homeboy::core::process::ProcessStartIdentity,
) -> homeboy::core::Result<String> {
    let submission =
        homeboy::agents::agent_task_service::cook_job_submission(cook_id, pid, start_identity)?;
    let job = client.submit(submission)?;
    let job_id = job.id.to_string();
    client.start(&job_id)?;
    Ok(job_id)
}

fn submit_cook_retry_controller_job(
    client: &homeboy::core::daemon::LocalControllerJobClient,
    cook_id: &str,
    run_id: &str,
    pid: u32,
    start_identity: &homeboy::core::process::ProcessStartIdentity,
) -> homeboy::core::Result<ControllerJobHandoff> {
    let submission = homeboy::agents::agent_task_service::cook_retry_job_submission(
        cook_id,
        run_id,
        pid,
        start_identity,
    )?;
    let job = client.submit(submission)?;
    Ok(ControllerJobHandoff::Owned {
        job_id: job.id.to_string(),
    })
}

/// The one context where local detachment stays a rejection.
fn runner_side_detach_error() -> Error {
    Error::validation_invalid_argument(
        "detach-after-handoff",
        "agent-task cook cannot detach after handoff with --placement local inside a runner-owned execution because the runner already owns this attempt",
        None,
        Some(vec![
            "Detach from the controller that dispatches the attempt, not from the runner process executing it.".to_string(),
        ]),
    )
}

/// The argv the detached cook executes.
///
/// It is the caller's own argv with two edits: the detach request is consumed
/// by the launcher, and the cook id is pinned so the launcher can name the run
/// it just handed off. Dropping `--detach-after-handoff` is what makes the child
/// observe its own lifecycle to a terminal report, because observing is the
/// default. The parent is the only process authorized to detach; the child must
/// not infer a handoff policy from its noninteractive stdio. Everything else is
/// preserved byte for byte.
fn detached_cook_child_args(
    normalized_args: &[String],
    cook_id: &str,
    has_explicit_cook_id: bool,
    consume_output: bool,
) -> Vec<String> {
    let mut args = Vec::new();
    let mut values = normalized_args.iter().skip(1);
    while let Some(arg) = values.next() {
        if arg == "--detach-after-handoff" || arg.starts_with("--detach-after-handoff=") {
            continue;
        }
        // The launcher writes the handoff envelope to both destinations. The
        // child must not overwrite that durable acknowledgement at completion.
        if consume_output && (arg == "--output" || arg == "-o") {
            let _ = values.next();
            continue;
        }
        if consume_output && arg.starts_with("--output=") {
            continue;
        }
        args.push(arg.clone());
    }
    if !has_explicit_cook_id {
        args.push("--run-id".to_string());
        args.push(cook_id.to_string());
    }
    args
}

fn stream_attached_cook_log(
    child: &mut std::process::Child,
    log_path: &Path,
) -> homeboy::core::Result<std::process::ExitStatus> {
    let mut offset = 0;
    loop {
        if let Ok(log) = std::fs::read_to_string(log_path) {
            if log.len() > offset {
                print!("{}", &log[offset..]);
                offset = log.len();
            }
        }
        if let Some(status) = child.try_wait().map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("observe supervised local Cook".to_string()),
            )
        })? {
            return Ok(status);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The destination the detached cook's notifications belong to.
///
/// Resolved from the launcher's own arguments rather than from
/// `notification_route::current()`, because detachment is intercepted during
/// argument routing — before the runtime binds the thread-local route — so the
/// thread-local is still empty here.
///
/// A route this process could not resolve is `None` rather than an error. The
/// runtime already validated this exact pair before routing, so a failure here
/// is unreachable; and notification routing is observability, which must never
/// take a cook down with it.
fn detached_route(cli: &Cli) -> Option<homeboy::core::notification_route::NotificationRoute> {
    homeboy::core::notification_route::from_cli_or_env(
        cli.notification_transport.as_deref(),
        cli.notification_route.as_deref(),
    )
    .ok()
    .flatten()
}

/// Replace a `--prompt -` stdin request with a file the detached cook can read.
///
/// Returns the materialized path when a rewrite happened.
fn materialize_stdin_prompt(
    args: &mut [String],
    session_root: &Path,
) -> homeboy::core::Result<Option<PathBuf>> {
    materialize_prompt_from(args, session_root, &mut std::io::stdin().lock())
}

/// The reader is a parameter so the capture can be exercised without a test
/// reaching for the harness's own stdin, which may never reach EOF.
fn materialize_prompt_from(
    args: &mut [String],
    session_root: &Path,
    source: &mut impl Read,
) -> homeboy::core::Result<Option<PathBuf>> {
    let Some(index) = stdin_prompt_index(args) else {
        return Ok(None);
    };
    let mut prompt = Vec::new();
    source.read_to_end(&mut prompt).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("read stdin prompt for a detached local cook".to_string()),
        )
    })?;
    let path = session_root.join("prompt.txt");
    std::fs::write(&path, &prompt)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    args[index] = if args[index] == "-" {
        format!("@{}", path.display())
    } else {
        format!("--prompt=@{}", path.display())
    };
    Ok(Some(path))
}

/// Index of the argv element holding a stdin prompt request, if any.
///
/// Both spellings clap accepts are covered: a separated `--prompt -` names the
/// following element, an attached `--prompt=-` names itself.
fn stdin_prompt_index(args: &[String]) -> Option<usize> {
    args.iter().enumerate().find_map(|(index, arg)| {
        if arg == "--prompt" && args.get(index + 1).is_some_and(|value| value.trim() == "-") {
            Some(index + 1)
        } else if arg == "--prompt=-" {
            Some(index)
        } else {
            None
        }
    })
}

/// Per-cook scratch directory for the launcher's captured stdio and prompt.
fn detached_session_root(cook_id: &str) -> homeboy::core::Result<PathBuf> {
    let root = homeboy::core::paths::homeboy_data()?
        .join("agent-task-detached")
        .join(homeboy::core::paths::sanitize_path_segment(cook_id));
    std::fs::create_dir_all(&root)
        .map_err(|error| Error::internal_io(error.to_string(), Some(root.display().to_string())))?;
    Ok(root)
}

/// Spawn the cook in its own session.
///
/// The route is set explicitly rather than left to environment inheritance so
/// the detached cook is bound to the destination the launcher resolved, whether
/// that came from argv or from the launcher's own environment. Setting both
/// variables together also normalizes a half-set pair inherited from the
/// launcher, which the child would otherwise reject as a validation error.
fn new_local_cook_launch_token(session_root: &Path) -> (String, PathBuf) {
    let token = uuid::Uuid::new_v4().to_string();
    let path = session_root.join(format!("launch-token-{token}"));
    (token, path)
}

fn publish_local_cook_launch_token(launch_token: &(String, PathBuf)) -> homeboy::core::Result<()> {
    publish_local_cook_launch_token_bytes(launch_token, launch_token.0.as_bytes())
}

fn publish_local_cook_launch_token_with_supervisor(
    launch_token: &(String, PathBuf),
    supervisor_job_id: &str,
) -> homeboy::core::Result<()> {
    let publication = serde_json::to_vec(&json!({
        "token": launch_token.0,
        "supervisor_job_id": supervisor_job_id,
    }))
    .map_err(|error| Error::internal_json(error.to_string(), None))?;
    publish_local_cook_launch_token_bytes(launch_token, &publication)
}

fn publish_local_cook_launch_token_bytes(
    launch_token: &(String, PathBuf),
    bytes: &[u8],
) -> homeboy::core::Result<()> {
    let pending = launch_token.1.with_extension("pending");
    std::fs::write(&pending, bytes).map_err(|error| {
        Error::internal_io(error.to_string(), Some(pending.display().to_string()))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&pending, std::fs::Permissions::from_mode(0o600)).map_err(
            |error| Error::internal_io(error.to_string(), Some(pending.display().to_string())),
        )?;
    }
    std::fs::rename(&pending, &launch_token.1).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(launch_token.1.display().to_string()),
        )
    })?;
    Ok(())
}

/// Retry readiness is valid only while its parent is still establishing the
/// handoff. Always remove it when that parent returns, including timeout paths.
struct LocalCookLaunchTokenCleanup(PathBuf);

impl Drop for LocalCookLaunchTokenCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_file(self.0.with_extension("pending"));
    }
}

#[cfg(test)]
fn create_local_cook_launch_token(session_root: &Path) -> homeboy::core::Result<(String, PathBuf)> {
    let launch_token = new_local_cook_launch_token(session_root);
    publish_local_cook_launch_token(&launch_token)?;
    Ok(launch_token)
}

fn spawn_detached_cook(
    args: &[String],
    log_path: &Path,
    route: Option<&homeboy::core::notification_route::NotificationRoute>,
    launch_token: &(String, PathBuf),
) -> homeboy::core::Result<std::process::Child> {
    let exe = std::env::current_exe().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("resolve current executable for a detached local cook".to_string()),
        )
    })?;
    let log = std::fs::File::create(log_path).map_err(|error| {
        Error::internal_io(error.to_string(), Some(log_path.display().to_string()))
    })?;
    let log_err = log.try_clone().map_err(|error| {
        Error::internal_io(error.to_string(), Some(log_path.display().to_string()))
    })?;

    let mut command = Command::new(exe);
    command
        .args(args)
        .env(LOCAL_COOK_LAUNCH_TOKEN_ENV, &launch_token.0)
        .env(LOCAL_COOK_LAUNCH_TOKEN_PATH_ENV, &launch_token.1)
        .envs(homeboy::core::notification_route::child_env(route))
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
    homeboy::core::process::detach_from_caller_session(&mut command);
    command.spawn().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("spawn detached local cook".to_string()),
        )
    })
}

fn compensate_supervisor_projection_failure(
    client: &homeboy::core::daemon::LocalControllerJobClient,
    job_id: &str,
    child: &mut std::process::Child,
    cook_id: &str,
) {
    let _ = client.cancel(
        job_id,
        "local Cook supervisor projection could not be persisted",
    );
    terminate_and_reap_detached_child(child);
    let _ = agent_task_lifecycle::fail_detached_cook_handoff_parent(
        cook_id,
        "local Cook supervisor projection could not be persisted",
    );
}

#[cfg(test)]
fn project_supervisor_or_compensate(
    projection: homeboy::core::Result<()>,
    compensate: impl FnOnce(),
) -> homeboy::core::Result<()> {
    if projection.is_err() {
        compensate();
    }
    projection
}

fn terminate_and_reap_detached_child(child: &mut std::process::Child) {
    let _ = homeboy::core::process::terminate_process_tree(child.id());
    let _ = child.wait();
}

fn detached_child_start_identity(
    pid: u32,
) -> homeboy::core::Result<homeboy::core::process::ProcessStartIdentity> {
    homeboy::core::process::process_start_identity(pid)
        .map_err(|error| {
            Error::internal_unexpected(format!("inspect detached Cook child: {error}"))
        })?
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "detached_cook_handoff.child_start_identity",
                "detached Cook child exited before its process identity could be captured",
                Some(pid.to_string()),
                None,
            )
        })
}

/// What the launcher could prove about the cook before returning.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DetachedCookHandoff {
    state: DetachedHandoffState,
    run_id: Option<String>,
    waited_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetachedHandoffState {
    /// The detached cook published its durable cook index; the returned ids are
    /// addressable now.
    Accepted,
    /// The process is alive but has not reached durable submission yet. The
    /// cook id is still the correct handle — it is pinned on the child's argv.
    Pending,
    /// The detached process exited before publishing durable identity. The log
    /// is the only evidence, and this is reported rather than dressed up as a
    /// successful handoff.
    ExitedBeforeHandoff,
}

impl DetachedHandoffState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Pending => "pending",
            Self::ExitedBeforeHandoff => "exited_before_handoff",
        }
    }
}

fn detached_handoff_rejection_reason(state: DetachedHandoffState) -> Option<&'static str> {
    (state == DetachedHandoffState::ExitedBeforeHandoff)
        .then_some("detached Cook exited before materializing an executable plan")
}

fn handoff_timeout() -> Duration {
    let millis = std::env::var(HANDOFF_TIMEOUT_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_HANDOFF_TIMEOUT_MS);
    Duration::from_millis(millis)
}

/// Poll durable state until the detached cook materializes an executable,
/// supervisor-linked attempt, it dies, or the bound elapses.
///
/// Liveness is read from the child handle rather than from the pid. The
/// detached cook is still this process's direct child until this process exits,
/// so an early exit leaves a zombie that a pid probe reports as running on
/// platforms without a `/proc` state check — which would turn a cook that died
/// on startup into an indefinite "pending" handoff.
fn await_durable_linked_handoff(
    cook_id: &str,
    supervisor_job_id: &str,
    child: &mut std::process::Child,
    timeout: Duration,
) -> homeboy::core::Result<DetachedCookHandoff> {
    await_durable_handoff_inner(cook_id, Some(supervisor_job_id), child, timeout)
}

/// Read the exact durable attempt an accepted handoff names. An index file on
/// its own is not sufficient: a caller must be able to resolve the attempt it
/// receives immediately.
#[cfg(test)]
fn await_durable_handoff(
    cook_id: &str,
    child: &mut std::process::Child,
    timeout: Duration,
) -> homeboy::core::Result<DetachedCookHandoff> {
    let handoff = await_durable_handoff_inner(cook_id, None, child, timeout)?;
    if handoff.state == DetachedHandoffState::ExitedBeforeHandoff {
        agent_task_lifecycle::fail_detached_cook_handoff_parent(
            cook_id,
            "detached Cook exited before materializing its first attempt",
        )?;
    }
    Ok(handoff)
}

fn await_durable_handoff_inner(
    cook_id: &str,
    supervisor_job_id: Option<&str>,
    child: &mut std::process::Child,
    timeout: Duration,
) -> homeboy::core::Result<DetachedCookHandoff> {
    let started = Instant::now();
    loop {
        if let Some(run_id) = durable_attempt_id(cook_id, supervisor_job_id) {
            return Ok(DetachedCookHandoff {
                state: DetachedHandoffState::Accepted,
                run_id: Some(run_id),
                waited_ms: started.elapsed().as_millis() as u64,
            });
        }
        match child.try_wait() {
            Ok(Some(_)) => {
                if let Some(run_id) = durable_attempt_id(cook_id, supervisor_job_id) {
                    return Ok(DetachedCookHandoff {
                        state: DetachedHandoffState::Accepted,
                        run_id: Some(run_id),
                        waited_ms: started.elapsed().as_millis() as u64,
                    });
                }
                return Ok(DetachedCookHandoff {
                    state: DetachedHandoffState::ExitedBeforeHandoff,
                    run_id: None,
                    waited_ms: started.elapsed().as_millis() as u64,
                });
            }
            Err(_) => {
                return Ok(DetachedCookHandoff {
                    state: DetachedHandoffState::Pending,
                    run_id: None,
                    waited_ms: started.elapsed().as_millis() as u64,
                });
            }
            Ok(None) => {}
        }
        if started.elapsed() >= timeout {
            return Ok(DetachedCookHandoff {
                state: DetachedHandoffState::Pending,
                run_id: None,
                waited_ms: started.elapsed().as_millis() as u64,
            });
        }
        std::thread::sleep(HANDOFF_POLL);
    }
}

fn durable_attempt_id(cook_id: &str, supervisor_job_id: Option<&str>) -> Option<String> {
    let run_id = agent_task_lifecycle::cook_index(cook_id)
        .ok()?
        .latest_run_id;
    let record = agent_task_lifecycle::exact_record(&run_id).ok()?;
    let plan = agent_task_lifecycle::load_plan(&run_id).ok()?;
    (!run_id.trim().is_empty()
        && !plan.tasks.is_empty()
        && supervisor_job_id
            .is_none_or(|job_id| record.metadata["local_cook_supervisor"]["job_id"] == job_id))
    .then_some(run_id)
}

fn empty_detached_plan_error(job_id: Option<&str>, problem: &str) -> Error {
    let mut error = Error::validation_invalid_argument(
        "detach-after-handoff",
        problem,
        None,
        Some(vec![
            "Re-run the original detached Cook command; do not resume a zero-task run.".to_string(),
        ]),
    );
    error.details = json!({
        "field": "detach-after-handoff",
        "problem": problem,
        "classification": "empty_detached_plan",
        "controller_job_id": job_id,
        "replay": "re-run the original detached Cook command",
    });
    error
}

/// The launcher's only output: a bounded, machine-readable handoff naming the
/// durable handle and the evidence needed to follow or stop the cook.
fn handoff_envelope(
    cook_id: &str,
    pid: u32,
    log_path: &Path,
    handoff: &DetachedCookHandoff,
    controller_job: &ControllerJobHandoff,
    requested_placement: homeboy::cli_surface::Placement,
    provider_placement: &str,
    provider_runner_id: Option<&str>,
) -> Value {
    json!({
        "schema": HANDOFF_SCHEMA,
        "placement": "local",
        "effective_placement": "controller_local",
        "requested_placement": placement_name(requested_placement),
        "provider_placement": provider_placement,
        "provider_runner_id": provider_runner_id,
        "detached": true,
        "cook_id": cook_id,
        "run_id": handoff.run_id,
        "pid": pid,
        "launcher_log": log_path.display().to_string(),
        "handoff": {
            "state": handoff.state.as_str(),
            "waited_ms": handoff.waited_ms,
        },
        // Additive: the daemon-owned durable job that now supervises this cook,
        // making the run inspectable through the job API rather than only
        // through its durable cook record.
        "controller_job": controller_job.projection(),
        "status_command": format!("homeboy agent-task status {cook_id}"),
        "logs_command": format!("homeboy agent-task logs {cook_id}"),
        "cancel_command": format!("homeboy agent-task cancel {cook_id}"),
        "output_file_owner": "handoff_launcher",
    })
}

fn placement_name(placement: homeboy::cli_surface::Placement) -> &'static str {
    match placement {
        homeboy::cli_surface::Placement::Auto => "auto",
        homeboy::cli_surface::Placement::Local => "local",
        homeboy::cli_surface::Placement::Lab => "lab",
        homeboy::cli_surface::Placement::LabOrLocal => "lab-or-local",
    }
}

#[cfg(test)]
mod tests {

    /// Tests are the entry point for their own unit of work, so the store
    /// resolves once here (#7505).
    fn test_lifecycle_store() -> homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore {
        homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()
            .expect("lifecycle store")
    }
    use super::*;
    use clap::Parser;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    fn cook_cli(extra: &[&str]) -> (Cli, Vec<String>) {
        let mut argv = vec!["homeboy"];
        argv.extend_from_slice(extra);
        argv.extend_from_slice(&[
            "agent-task",
            "cook",
            "--prompt",
            "implement the fix",
            "--to-worktree",
            "repo@branch",
            "--verify",
            "true",
        ]);
        let cli = Cli::try_parse_from(argv.clone()).expect("parse cook invocation");
        (cli, args(&argv))
    }

    /// A detached cook is a different process, so the launcher's route reaches
    /// it only if something writes it onto the child environment.
    #[test]
    fn a_detached_cook_child_carries_an_explicit_route() {
        let (cli, _) = cook_cli(&[
            "--placement",
            "local",
            "--detach-after-handoff",
            "--notification-transport",
            "extension.run-completion",
            "--notification-route",
            "opaque-destination",
        ]);

        let route = detached_route(&cli).expect("an explicit route resolves");

        assert_eq!(
            homeboy::core::notification_route::child_env(Some(&route)),
            vec![
                (
                    homeboy::core::notification_route::NOTIFICATION_TRANSPORT_ENV,
                    "extension.run-completion".to_string()
                ),
                (
                    homeboy::core::notification_route::NOTIFICATION_ROUTE_ENV,
                    "opaque-destination".to_string()
                ),
            ]
        );
    }

    /// Half a pair is a hard validation error in the child, so propagation is
    /// all-or-nothing.
    ///
    /// A cook launched without CLI values can still legitimately inherit a
    /// route from this test process's own environment, so the deterministic
    /// assertion is the all-or-nothing rule rather than a fixed count.
    #[test]
    fn a_detached_cook_never_propagates_half_a_route() {
        assert!(homeboy::core::notification_route::child_env(None).is_empty());

        let (cli, _) = cook_cli(&["--placement", "local", "--detach-after-handoff"]);

        let propagated =
            homeboy::core::notification_route::child_env(detached_route(&cli).as_ref());
        assert!(
            propagated.is_empty() || propagated.len() == 2,
            "propagated {propagated:?}"
        );
    }

    /// The rejection this replaces was unconditional. Preserving it inside a
    /// runner-owned execution is the whole reason `runner_side` exists.
    #[test]
    fn a_runner_owned_execution_still_refuses_to_detach() {
        let (cli, normalized) = cook_cli(&["--placement", "local", "--detach-after-handoff"]);

        let error = intercept_local_detached_cook(&cli, &normalized, None, true, None, None)
            .expect_err("a runner-owned execution cannot detach");

        assert!(
            error
                .message
                .contains("cannot detach after handoff with --placement local"),
            "{}",
            error.message
        );
        assert!(
            error.message.contains("runner-owned execution"),
            "{}",
            error.message
        );
    }

    /// Only a Cook explicitly requesting detachment is intercepted; attached
    /// local callers continue through normal routing untouched.
    /// These cases must not spawn anything.
    #[test]
    fn only_a_detaching_cook_is_intercepted() {
        let normalized = args(&[
            "homeboy",
            "--placement",
            "local",
            "agent-task",
            "cook",
            "--run-id",
            "attached",
            "--prompt",
            "implement the fix",
            "--to-worktree",
            "repo@branch",
            "--verify",
            "true",
        ]);
        let local = Cli::try_parse_from(&normalized).expect("parse attached local Cook");
        assert!(!is_unsupervised_local_cook(&local));
        crate::test_support::with_isolated_home(|_| {
            assert_eq!(
                intercept_local_detached_cook(
                    &local,
                    &normalized,
                    None,
                    false,
                    Some("local"),
                    None,
                )
                .expect("attached caller falls through"),
                None,
            );
            assert!(
                agent_task_lifecycle::exact_record("attached").is_err(),
                "an attached caller must not create the detached parent, supervisor, or attempt reservation"
            );
        });

        let preview = Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "cook",
            "--prompt",
            "implement the fix",
            "--to-worktree",
            "repo@branch",
            "--verify",
            "true",
            "--preview",
        ])
        .expect("parse preview cook invocation");
        assert!(
            !is_unsupervised_local_cook(&preview),
            "preview must bypass detached Cook interception"
        );

        let (auto, normalized) = cook_cli(&["--placement", "auto"]);
        assert!(!is_unsupervised_local_cook(&auto));
        assert_eq!(
            intercept_local_detached_cook(&auto, &normalized, None, false, Some("lab"), None,)
                .expect("non-local route falls through"),
            None,
        );
    }

    #[test]
    fn detachment_owns_the_controller_for_auto_lab_and_local_placement() {
        for placement in ["auto", "lab", "local", "lab-or-local"] {
            let (cli, _) = cook_cli(&["--placement", placement, "--detach-after-handoff"]);
            assert!(is_unsupervised_local_cook(&cli), "{placement}");
        }
    }

    /// The attached local placement warning is a submission preamble line, so it
    /// obeys the same `--no-progress` suppression as the rest of them.
    #[test]
    fn no_progress_suppresses_the_attached_local_placement_warning() {
        let (loud, _) = cook_cli(&["--placement", "local"]);
        assert!(!attached_local_cook_progress_is_suppressed(&loud));

        let quiet = Cli::try_parse_from([
            "homeboy",
            "--placement",
            "local",
            "agent-task",
            "cook",
            "--prompt",
            "implement the fix",
            "--to-worktree",
            "repo@branch",
            "--verify",
            "true",
            "--no-progress",
        ])
        .expect("parse quiet cook invocation");
        assert!(attached_local_cook_progress_is_suppressed(&quiet));
    }

    #[test]
    fn a_non_cook_command_is_never_intercepted() {
        let cli = Cli::try_parse_from(["homeboy", "--placement", "local", "status"])
            .expect("parse status invocation");

        assert!(!is_unsupervised_local_cook(&cli));
    }

    #[test]
    fn ambient_launch_token_values_do_not_bypass_supervision() {
        let (cli, _) = cook_cli(&["--placement", "local", "--detach-after-handoff"]);

        assert!(is_unsupervised_local_cook(&cli));
        assert!(!consume_local_cook_launch_token_at(
            std::ffi::OsStr::new("forged-token"),
            Path::new("/tmp/does-not-exist"),
        ));
    }

    #[test]
    fn launch_token_is_single_use() {
        let directory = tempfile::tempdir().expect("temporary token directory");
        let (token, path) = create_local_cook_launch_token(directory.path()).expect("launch token");
        assert!(consume_local_cook_launch_token_at(token.as_ref(), &path));
        assert!(!consume_local_cook_launch_token_at(token.as_ref(), &path));
    }

    #[test]
    fn consumed_launch_token_is_not_inherited_by_nested_cook() {
        let directory = tempfile::tempdir().expect("temporary token directory");
        let (token, path) = create_local_cook_launch_token(directory.path()).expect("launch token");
        let _env = super::super::tests::EnvGuard::set_many(&[
            (LOCAL_COOK_LAUNCH_TOKEN_ENV, Some(token.as_str())),
            (
                LOCAL_COOK_LAUNCH_TOKEN_PATH_ENV,
                Some(path.to_str().expect("UTF-8 token path")),
            ),
        ]);

        assert!(consume_local_cook_launch_token());
        assert!(!local_cook_launch_token_is_present());
    }

    #[test]
    fn retry_launch_tokens_are_unique_and_unpublished_until_supervisor_ready() {
        let directory = tempfile::tempdir().expect("temporary token directory");
        let first = new_local_cook_launch_token(directory.path());
        let second = new_local_cook_launch_token(directory.path());
        assert_ne!(first.1, second.1);
        assert!(
            !first.1.exists(),
            "preflight reserves no readable readiness token"
        );

        publish_local_cook_launch_token(&first).expect("atomically publish readiness");
        assert!(consume_local_cook_launch_token_at(
            first.0.as_ref(),
            &first.1
        ));
        assert!(
            !second.1.exists(),
            "one launch cannot release another child"
        );
    }

    #[test]
    fn retry_child_args_pin_the_parent_reservation() {
        let rewritten = retry_child_args(
            &args(&[
                "homeboy",
                "--placement",
                "local",
                "agent-task",
                "retry",
                "source",
                "--run",
                "--new-run-id=untrusted",
            ]),
            "reserved-retry",
        );

        assert_eq!(
            rewritten
                .iter()
                .filter(|arg| arg.as_str() == "--new-run-id")
                .count(),
            1
        );
        assert_eq!(rewritten.last(), Some(&"reserved-retry".to_string()));
        assert!(!rewritten.iter().any(|arg| arg == "--new-run-id=untrusted"));
    }

    #[test]
    fn retry_interceptor_leaves_no_reservation_when_controller_connect_fails() {
        homeboy::core::test_support::with_isolated_home(|_| {
            let source = "retry-interceptor-connect-failure";
            agent_task_lifecycle::submit_plan(
                &homeboy::agents::agent_tasks::scheduler::AgentTaskPlan::new("retry-plan", vec![]),
                Some(source),
            )
            .expect("persist source record");
            agent_task_lifecycle::record_cook_attempt_in_store(
                &test_lifecycle_store(),
                "retry-interceptor-cook",
                1,
                source,
            )
            .expect("bind source to Cook");
            let normalized = args(&[
                "homeboy",
                "--placement",
                "local",
                "agent-task",
                "retry",
                source,
                "--run",
            ]);
            let cli = Cli::try_parse_from(&normalized).expect("parse retry invocation");

            intercept_local_cook_retry(&cli, &normalized, false)
                .expect_err("an isolated home has no controller daemon");

            assert_eq!(
                agent_task_lifecycle::list_records()
                    .expect("list lifecycle records")
                    .len(),
                1,
                "controller preflight must fail before reserving a retry"
            );
        });
    }

    #[test]
    fn daemon_build_mismatch_is_an_explicit_foreground_fallback_signal() {
        let mut mismatch = Error::validation_invalid_argument(
            "daemon_build_identity",
            "resident daemon differs",
            None,
            None,
        );
        mismatch.details = serde_json::json!({
            "classification": "controller_job_daemon_build_mismatch"
        });
        assert!(controller_job_daemon_build_mismatch(&mismatch));

        let unrelated = Error::internal_unexpected("daemon unavailable");
        assert!(!controller_job_daemon_build_mismatch(&unrelated));
    }

    #[test]
    fn supervisor_projection_failure_compensates_before_returning_error() {
        let compensated = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = std::sync::Arc::clone(&compensated);
        let error = Error::internal_unexpected("injected supervisor projection write failure");

        let result = project_supervisor_or_compensate(Err(error), move || {
            observed.store(true, std::sync::atomic::Ordering::SeqCst);
        });

        assert!(result.is_err());
        assert!(compensated.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn child_args_drop_detach_and_pin_a_generated_cook_id() {
        let normalized = args(&[
            "homeboy",
            "--placement",
            "local",
            "--detach-after-handoff",
            "agent-task",
            "cook",
            "--prompt",
            "fix it",
        ]);

        let child = detached_cook_child_args(&normalized, "cook-generated", false, true);

        assert_eq!(
            child,
            args(&[
                "--placement",
                "local",
                "agent-task",
                "cook",
                "--prompt",
                "fix it",
                "--run-id",
                "cook-generated",
            ])
        );
    }

    #[test]
    fn child_args_preserve_an_explicit_cook_id_without_duplicating_it() {
        let normalized = args(&[
            "homeboy",
            "--detach-after-handoff",
            "--placement",
            "local",
            "agent-task",
            "cook",
            "--run-id",
            "cook-explicit",
            "--prompt",
            "fix it",
        ]);

        let child = detached_cook_child_args(&normalized, "cook-explicit", true, true);

        assert_eq!(
            child.iter().filter(|arg| *arg == "--run-id").count(),
            1,
            "{child:?}"
        );
        assert!(
            !child.iter().any(|arg| arg == "--detach-after-handoff"),
            "{child:?}"
        );
        assert_eq!(
            child,
            args(&[
                "--placement",
                "local",
                "agent-task",
                "cook",
                "--run-id",
                "cook-explicit",
                "--prompt",
                "fix it",
            ])
        );
    }

    #[test]
    fn child_args_leave_the_handoff_output_owned_by_the_launcher() {
        let child = detached_cook_child_args(
            &args(&[
                "homeboy",
                "--detach-after-handoff",
                "--output",
                "/tmp/handoff.json",
                "agent-task",
                "cook",
                "--prompt",
                "fix it",
            ]),
            "cook-output",
            false,
            true,
        );

        assert!(!child
            .iter()
            .any(|arg| arg == "--output" || arg == "/tmp/handoff.json"));
    }

    #[test]
    fn attached_child_preserves_output_for_its_terminal_report() {
        let child = detached_cook_child_args(
            &args(&[
                "homeboy",
                "--output",
                "/tmp/cook.json",
                "agent-task",
                "cook",
                "--prompt",
                "fix it",
            ]),
            "cook-output",
            false,
            false,
        );

        assert!(child
            .windows(2)
            .any(|args| args == ["--output", "/tmp/cook.json"]));
    }

    /// The re-executed cook must be the requested cook. Anything the launcher
    /// silently drops is work the operator asked for and did not get.
    #[test]
    fn child_args_preserve_every_other_flag_verbatim() {
        let normalized = args(&[
            "homeboy",
            "--placement",
            "local",
            "--detach-after-handoff",
            "--allow-dirty-lab-workspace",
            "agent-task",
            "cook",
            "--to-worktree",
            "repo@branch",
            "--verify",
            "true",
            "--max-attempts",
            "5",
            "--no-finalize",
        ]);

        let child = detached_cook_child_args(&normalized, "cook-generated", false, true);

        for expected in [
            "--allow-dirty-lab-workspace",
            "--to-worktree",
            "repo@branch",
            "--verify",
            "--max-attempts",
            "5",
            "--no-finalize",
        ] {
            assert!(child.iter().any(|arg| arg == expected), "{child:?}");
        }
    }

    /// A detached child's stdin is closed, and the piped bytes live only in the
    /// launcher's pipe. Losing them would strand the cook on an empty prompt.
    #[test]
    fn stdin_prompt_is_captured_verbatim_for_the_detached_cook() {
        let session = tempfile::tempdir().expect("session root");
        let mut child = args(&["agent-task", "cook", "--prompt", "-"]);
        let piped = "fix `$THING`\n\nwith a trailing newline\n";

        let path = materialize_prompt_from(
            &mut child,
            session.path(),
            &mut std::io::Cursor::new(piped.as_bytes()),
        )
        .expect("materialize stdin prompt")
        .expect("a stdin prompt was present");

        assert_eq!(std::fs::read_to_string(&path).expect("prompt file"), piped);
        assert_eq!(child.last(), Some(&format!("@{}", path.display())));
    }

    #[test]
    fn a_prompt_that_is_not_stdin_is_left_alone() {
        let session = tempfile::tempdir().expect("session root");
        let mut child = args(&["agent-task", "cook", "--prompt", "@task.md"]);

        let rewritten = materialize_prompt_from(
            &mut child,
            session.path(),
            &mut std::io::Cursor::new(b"unused".as_slice()),
        )
        .expect("inspect prompt");

        assert_eq!(rewritten, None);
        assert_eq!(child.last().map(String::as_str), Some("@task.md"));
    }

    /// A literal `-` that is the value of some other option is not a prompt.
    #[test]
    fn only_the_prompt_option_claims_a_stdin_marker() {
        assert_eq!(
            stdin_prompt_index(&args(&["--base", "-", "--prompt", "@task.md"])),
            None
        );
        assert_eq!(
            stdin_prompt_index(&args(&["--prompt", "-", "--no-finalize"])),
            Some(1)
        );
        assert_eq!(
            stdin_prompt_index(&args(&["--prompt=-", "--no-finalize"])),
            Some(0)
        );
    }

    #[test]
    fn an_attached_stdin_prompt_keeps_its_option_when_rewritten() {
        let session = tempfile::tempdir().expect("session root");
        let mut child = args(&["agent-task", "cook", "--prompt=-", "--no-finalize"]);

        let path = materialize_prompt_from(
            &mut child,
            session.path(),
            &mut std::io::Cursor::new(b"piped".as_slice()),
        )
        .expect("materialize stdin prompt")
        .expect("a stdin prompt was present");

        assert_eq!(child[2], format!("--prompt=@{}", path.display()));
    }

    #[test]
    fn an_accepted_handoff_reports_the_addressable_ids_and_follow_up_commands() {
        let handoff = DetachedCookHandoff {
            state: DetachedHandoffState::Accepted,
            run_id: Some("cook-11476-attempt-1-ab12cd34".to_string()),
            waited_ms: 240,
        };

        let envelope = handoff_envelope(
            "cook-11476",
            4242,
            Path::new("/data/agent-task-detached/cook-11476/cook.log"),
            &handoff,
            &ControllerJobHandoff::Owned {
                job_id: "3f2b1c00-0000-4000-8000-000000000001".to_string(),
            },
            homeboy::cli_surface::Placement::Local,
            "local",
            None,
        );

        assert_eq!(envelope["schema"], HANDOFF_SCHEMA);
        assert_eq!(envelope["placement"], "local");
        assert_eq!(envelope["effective_placement"], "controller_local");
        assert_eq!(envelope["requested_placement"], "local");
        assert_eq!(envelope["provider_placement"], "local");
        assert_eq!(envelope["detached"], true);
        assert_eq!(envelope["cook_id"], "cook-11476");
        assert_eq!(envelope["run_id"], "cook-11476-attempt-1-ab12cd34");
        assert_eq!(envelope["pid"], 4242);
        assert_eq!(envelope["handoff"]["state"], "accepted");
        assert_eq!(
            envelope["status_command"],
            "homeboy agent-task status cook-11476"
        );
        assert_eq!(
            envelope["cancel_command"],
            "homeboy agent-task cancel cook-11476"
        );
        assert_eq!(envelope["controller_job"]["state"], "owned");
        assert_eq!(
            envelope["controller_job"]["job_id"],
            "3f2b1c00-0000-4000-8000-000000000001"
        );
    }

    /// A launcher that dressed an unproven handoff as an accepted one would
    /// reproduce the exact dishonesty this change exists to remove.
    #[test]
    fn an_unproven_handoff_is_reported_as_such() {
        for (state, expected) in [
            (DetachedHandoffState::Pending, "pending"),
            (
                DetachedHandoffState::ExitedBeforeHandoff,
                "exited_before_handoff",
            ),
        ] {
            let handoff = DetachedCookHandoff {
                state,
                run_id: None,
                waited_ms: 30_000,
            };

            let envelope = handoff_envelope(
                "cook-11476",
                4242,
                Path::new("/tmp/cook.log"),
                &handoff,
                &ControllerJobHandoff::Owned {
                    job_id: "job-unproven".to_string(),
                },
                homeboy::cli_surface::Placement::Auto,
                "lab",
                Some("runner-a"),
            );

            assert_eq!(envelope["handoff"]["state"], expected);
            assert_eq!(envelope["run_id"], Value::Null);
            assert_eq!(envelope["launcher_log"], "/tmp/cook.log");
        }
    }

    /// The admitted controller job is included in every successful handoff.
    #[test]
    fn an_owned_daemon_returns_the_full_handoff_contract() {
        let handoff = DetachedCookHandoff {
            state: DetachedHandoffState::Accepted,
            run_id: Some("cook-degraded-attempt-1".to_string()),
            waited_ms: 12,
        };

        let envelope = handoff_envelope(
            "cook-degraded",
            4242,
            Path::new("/tmp/cook.log"),
            &handoff,
            &ControllerJobHandoff::Owned {
                job_id: "job-owned".to_string(),
            },
            homeboy::cli_surface::Placement::Lab,
            "lab",
            Some("runner-a"),
        );

        // Every pre-existing field survives unchanged.
        assert_eq!(envelope["schema"], HANDOFF_SCHEMA);
        assert_eq!(envelope["cook_id"], "cook-degraded");
        assert_eq!(envelope["run_id"], "cook-degraded-attempt-1");
        assert_eq!(envelope["pid"], 4242);
        assert_eq!(envelope["handoff"]["state"], "accepted");
        assert_eq!(
            envelope["status_command"],
            "homeboy agent-task status cook-degraded"
        );
        assert_eq!(
            envelope["cancel_command"],
            "homeboy agent-task cancel cook-degraded"
        );
        assert_eq!(envelope["output_file_owner"], "handoff_launcher");
        assert_eq!(envelope["controller_job"]["state"], "owned");
        assert_eq!(envelope["controller_job"]["job_id"], "job-owned");
        assert_eq!(envelope["provider_runner_id"], "runner-a");
    }

    #[test]
    fn output_file_receives_the_exact_stdout_handoff_envelope() {
        let directory = tempfile::tempdir().expect("output directory");
        let path = directory.path().join("handoff.json");
        let envelope = serde_json::json!({ "schema": HANDOFF_SCHEMA, "cook_id": "cook-output" });

        let stdout = finalize_handoff_envelope(&envelope, Some(path.to_str().expect("utf-8 path")))
            .expect("finalize handoff");

        assert_eq!(std::fs::read_to_string(path).expect("read output"), stdout);
    }

    #[test]
    fn a_pending_handoff_cook_id_resolves_to_its_lifecycle_parent() {
        crate::test_support::with_isolated_home(|_| {
            agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
                &test_lifecycle_store(),
                "cook-pending",
            )
            .expect("persist handoff parent");

            let status = agent_task_lifecycle::status("cook-pending")
                .expect("pending handoff status resolves");
            let logs =
                agent_task_lifecycle::logs("cook-pending").expect("pending handoff logs resolve");

            assert_eq!(status.run_id, "cook-pending");
            assert_eq!(status.metadata["detached_cook_handoff"]["state"], "pending");
            assert_eq!(logs.run_id, "cook-pending");
            assert_eq!(
                agent_task_lifecycle::cancel_run("cook-pending", None)
                    .expect("pending handoff cancel command resolves")
                    .run_id,
                "cook-pending"
            );
        });
    }

    #[test]
    fn handoff_admission_transitions_from_parent_to_child_to_supervisor() {
        crate::test_support::with_isolated_home(|_| {
            let cook_id = "cook-admission-transitions";
            let parent = agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
                &test_lifecycle_store(),
                cook_id,
            )
            .expect("persist pre-supervisor parent");
            assert_eq!(
                parent.metadata["detached_cook_handoff"]["admission_state"],
                "pre_supervisor"
            );
            let child = agent_task_lifecycle::record_detached_cook_handoff_child_in_store(
                &test_lifecycle_store(),
                cook_id,
                1,
                homeboy::core::process::ProcessStartIdentity::Macos {
                    start_seconds: 1,
                    start_microseconds: 1,
                },
            )
            .expect("attach child identity");
            assert_eq!(
                child.metadata["detached_cook_handoff"]["admission_state"],
                "child_attached"
            );
            agent_task_lifecycle::record_detached_cook_supervisor_in_store(
                &test_lifecycle_store(),
                cook_id,
                "supervisor-1",
            )
            .expect("attach supervisor");
            let supervised =
                agent_task_lifecycle::exact_record(cook_id).expect("read supervised handoff");
            assert_eq!(
                supervised.metadata["detached_cook_handoff"]["admission_state"],
                "supervising"
            );
            assert_eq!(
                supervised.metadata["detached_cook_handoff"]["supervisor_job_id"],
                "supervisor-1"
            );
        });
    }

    #[test]
    fn a_materialized_handoff_cook_id_cancels_its_attempt() {
        crate::test_support::with_isolated_home(|_| {
            let cook_id = "cook-materialized";
            agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
                &test_lifecycle_store(),
                cook_id,
            )
            .expect("persist handoff parent");
            let attempt_id = "cook-materialized-attempt-1";
            let plan = homeboy::agents::agent_tasks::scheduler::AgentTaskPlan::new(
                "materialized-handoff-attempt",
                Vec::new(),
            );
            agent_task_lifecycle::submit_plan(&plan, Some(attempt_id))
                .expect("persist materialized attempt");
            agent_task_lifecycle::record_cook_attempt_in_store(
                &test_lifecycle_store(),
                cook_id,
                1,
                attempt_id,
            )
            .expect("redirect cook alias to materialized attempt");

            let parent = agent_task_lifecycle::exact_record(cook_id)
                .expect("read redirected handoff parent");
            assert_eq!(
                parent.state,
                agent_task_lifecycle::AgentTaskRunState::Succeeded
            );
            assert_eq!(
                parent.metadata["detached_cook_handoff"]["state"],
                "redirected"
            );
            assert_eq!(
                parent.metadata["detached_cook_handoff"]["attempt_run_id"],
                attempt_id
            );

            assert_eq!(
                agent_task_lifecycle::cancel_run(cook_id, None)
                    .expect("materialized handoff cancel command resolves")
                    .run_id,
                attempt_id
            );
        });
    }

    #[test]
    fn cancelling_before_attempt_index_terminates_the_detached_child() {
        crate::test_support::with_isolated_home(|_| {
            let cook_id = "cook-cancel-before-index";
            agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
                &test_lifecycle_store(),
                cook_id,
            )
            .expect("persist handoff parent");
            let child = Command::new("sh")
                .args(["-c", "sleep 30"])
                .spawn()
                .expect("spawn detached preparation fixture");
            agent_task_lifecycle::record_detached_cook_handoff_child_in_store(
                &test_lifecycle_store(),
                cook_id,
                child.id(),
                detached_child_start_identity(child.id()).expect("capture child identity"),
            )
            .expect("persist detached child identity");

            let cancelled =
                agent_task_lifecycle::cancel_run(cook_id, None).expect("cancel pending handoff");
            assert_eq!(
                cancelled.state,
                agent_task_lifecycle::AgentTaskRunState::Cancelled
            );
            assert!(
                cancelled
                    .metadata
                    .get("detached_cook_handoff_cancellation")
                    .is_some(),
                "cancellation must record detached child termination"
            );
            assert!(
                matches!(
                    homeboy::core::process::process_identity_state(child.id(), None),
                    homeboy::core::process::ProcessIdentityState::Dead
                ),
                "cancelled child must be dead"
            );
        });
    }

    #[test]
    fn cancellation_between_spawn_and_child_attachment_stops_the_child() {
        crate::test_support::with_isolated_home(|_| {
            let cook_id = "cook-cancel-before-child-attachment";
            agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
                &test_lifecycle_store(),
                cook_id,
            )
            .expect("persist parent before spawn");
            agent_task_lifecycle::cancel_run(cook_id, None).expect("cancel before child attach");
            let mut child = Command::new("sh")
                .args(["-c", "sleep 30"])
                .spawn()
                .expect("spawn child after cancellation");

            let parent = agent_task_lifecycle::record_detached_cook_handoff_child_in_store(
                &test_lifecycle_store(),
                cook_id,
                child.id(),
                detached_child_start_identity(child.id()).expect("capture child identity"),
            )
            .expect("persist child identity on cancelled parent");
            assert_eq!(
                parent.state,
                agent_task_lifecycle::AgentTaskRunState::Cancelled
            );
            terminate_and_reap_detached_child(&mut child);
            assert!(
                matches!(
                    homeboy::core::process::process_identity_state(child.id(), None),
                    homeboy::core::process::ProcessIdentityState::Dead
                ),
                "launcher must stop a child spawned after cancellation"
            );
        });
    }

    #[test]
    fn a_spawn_failure_terminalizes_the_precreated_handoff_parent() {
        crate::test_support::with_isolated_home(|_| {
            let cook_id = "cook-spawn-failure";
            agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
                &test_lifecycle_store(),
                cook_id,
            )
            .expect("persist parent before spawn");
            let directory = tempfile::tempdir().expect("log directory");

            let token = (
                "test-token".to_string(),
                directory.path().join("launch-token"),
            );
            assert!(spawn_detached_cook(&[], directory.path(), None, &token).is_err());
            agent_task_lifecycle::fail_detached_cook_handoff_parent(
                cook_id,
                "detached Cook could not be spawned",
            )
            .expect("terminalize failed handoff");

            let parent =
                agent_task_lifecycle::exact_record(cook_id).expect("read failed handoff parent");
            assert_eq!(
                parent.state,
                agent_task_lifecycle::AgentTaskRunState::Failed
            );
            assert_eq!(
                parent.metadata["detached_cook_handoff"]["state"],
                "exited_before_handoff"
            );
        });
    }

    #[test]
    fn attaching_a_child_preserves_an_exited_before_handoff_terminal_parent() {
        crate::test_support::with_isolated_home(|_| {
            let cook_id = "cook-exited-before-child-attachment";
            agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
                &test_lifecycle_store(),
                cook_id,
            )
            .expect("persist handoff parent");
            agent_task_lifecycle::fail_detached_cook_handoff_parent(
                cook_id,
                "detached Cook exited before materializing its first attempt",
            )
            .expect("terminalize handoff before delayed child attachment");

            let mut child = Command::new("sh")
                .args(["-c", "sleep 30"])
                .spawn()
                .expect("spawn child for delayed attachment");
            let parent = agent_task_lifecycle::record_detached_cook_handoff_child_in_store(
                &test_lifecycle_store(),
                cook_id,
                child.id(),
                detached_child_start_identity(child.id()).expect("capture child identity"),
            )
            .expect("read terminal handoff parent");
            terminate_and_reap_detached_child(&mut child);

            assert_eq!(
                parent.state,
                agent_task_lifecycle::AgentTaskRunState::Failed
            );
            assert_eq!(
                parent.metadata["detached_cook_handoff"]["state"],
                "exited_before_handoff"
            );
            assert_eq!(
                parent.metadata["detached_cook_handoff"]["admission_state"],
                "failed"
            );
        });
    }

    #[test]
    fn an_explicit_cook_id_cannot_overwrite_a_non_handoff_run() {
        crate::test_support::with_isolated_home(|_| {
            let cook_id = "existing-non-handoff-run";
            let plan = homeboy::agents::agent_tasks::scheduler::AgentTaskPlan::new(
                "unrelated-run",
                Vec::new(),
            );
            agent_task_lifecycle::submit_plan(&plan, Some(cook_id)).expect("persist unrelated run");

            assert!(
                agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
                    &test_lifecycle_store(),
                    cook_id
                )
                .is_err()
            );
            let record = agent_task_lifecycle::exact_record(cook_id)
                .expect("unrelated run remains readable");
            assert!(
                record.metadata.get("detached_cook_handoff").is_none(),
                "collision must not overwrite unrelated metadata"
            );
        });
    }

    #[test]
    fn a_missing_child_start_identity_is_rejected_before_attachment() {
        assert!(detached_child_start_identity(u32::MAX).is_err());
    }

    #[test]
    fn child_identity_persistence_failure_terminates_and_reaps_the_child() {
        crate::test_support::with_isolated_home(|_| {
            let mut child = Command::new("sh")
                .args(["-c", "sleep 30"])
                .spawn()
                .expect("spawn child before persistence failure");

            assert!(
                agent_task_lifecycle::record_detached_cook_handoff_child_in_store(
                    &test_lifecycle_store(),
                    "missing-handoff-parent",
                    child.id(),
                    detached_child_start_identity(child.id()).expect("capture child identity"),
                )
                .is_err(),
                "missing durable parent must reject child attachment"
            );
            terminate_and_reap_detached_child(&mut child);
            assert!(
                matches!(
                    homeboy::core::process::process_identity_state(child.id(), None),
                    homeboy::core::process::ProcessIdentityState::Dead
                ),
                "persistence failure must leave no child process"
            );
        });
    }

    #[test]
    fn preparation_past_the_handoff_window_keeps_follow_commands_resolvable() {
        crate::test_support::with_isolated_home(|_| {
            let cook_id = "cook-slow-preparation";
            agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
                &test_lifecycle_store(),
                cook_id,
            )
            .expect("persist handoff parent before preparation");
            let mut child = Command::new("sh")
                .args(["-c", "sleep 1"])
                .spawn()
                .expect("spawn slow preparation fixture");

            let handoff = await_durable_handoff(cook_id, &mut child, Duration::from_millis(5))
                .expect("observe bounded pending handoff");

            assert_eq!(handoff.state, DetachedHandoffState::Pending);
            assert_eq!(detached_handoff_rejection_reason(handoff.state), None);
            assert_eq!(
                agent_task_lifecycle::status(cook_id)
                    .expect("pending status command resolves")
                    .run_id,
                cook_id
            );
            assert_eq!(
                agent_task_lifecycle::logs(cook_id)
                    .expect("pending logs command resolves")
                    .run_id,
                cook_id
            );
            assert_eq!(
                agent_task_lifecycle::cancel_run(cook_id, None)
                    .expect("pending cancel command resolves")
                    .run_id,
                cook_id
            );
            child.kill().expect("stop slow preparation fixture");
            child.wait().expect("reap slow preparation fixture");
        });
    }

    #[test]
    fn an_empty_materialized_attempt_is_not_an_accepted_handoff() {
        crate::test_support::with_isolated_home(|_| {
            let cook_id = "cook-materialized-acceptance";
            let attempt_id = "cook-materialized-acceptance-attempt-1";
            agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
                &test_lifecycle_store(),
                cook_id,
            )
            .expect("persist pending handoff parent");
            let plan = homeboy::agents::agent_tasks::scheduler::AgentTaskPlan::new(
                "materialized-acceptance-attempt",
                Vec::new(),
            );
            agent_task_lifecycle::submit_plan(&plan, Some(attempt_id))
                .expect("persist executable attempt");
            agent_task_lifecycle::record_cook_attempt_in_store(
                &test_lifecycle_store(),
                cook_id,
                1,
                attempt_id,
            )
            .expect("publish durable Cook attempt ownership");
            let mut child = Command::new("sh")
                .args(["-c", "sleep 30"])
                .spawn()
                .expect("spawn live detached child");

            let handoff = await_durable_handoff(cook_id, &mut child, Duration::from_millis(0))
                .expect("observe empty materialized handoff");

            assert_eq!(handoff.state, DetachedHandoffState::Pending);
            assert_eq!(handoff.run_id, None);
            terminate_and_reap_detached_child(&mut child);
        });
    }

    #[test]
    fn materialization_lock_first_makes_exit_failure_a_non_panicking_no_op() {
        crate::test_support::with_isolated_home(|_| {
            let cook_id = "cook-materialized-parent-preserved";
            let attempt_id = "cook-materialized-parent-preserved-attempt-1";
            agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
                &test_lifecycle_store(),
                cook_id,
            )
            .expect("persist pending handoff parent");
            let plan = homeboy::agents::agent_tasks::scheduler::AgentTaskPlan::new(
                "materialized-parent-preserved-attempt",
                Vec::new(),
            );
            agent_task_lifecycle::submit_plan(&plan, Some(attempt_id))
                .expect("persist executable attempt");
            agent_task_lifecycle::record_cook_attempt_in_store(
                &test_lifecycle_store(),
                cook_id,
                1,
                attempt_id,
            )
            .expect("publish durable Cook attempt ownership");

            agent_task_lifecycle::fail_detached_cook_handoff_parent(
                cook_id,
                "stale detached child exit observation",
            )
            .expect("protected materialized parent makes exit failure a no-op");

            let parent = agent_task_lifecycle::exact_record(cook_id)
                .expect("read materialized handoff parent");
            assert_eq!(
                parent.metadata["detached_cook_handoff"]["state"],
                "redirected"
            );
            assert_eq!(
                parent.metadata["detached_cook_handoff"]["attempt_run_id"],
                attempt_id
            );
            assert_ne!(
                parent.metadata["detached_cook_handoff"]["reason"],
                "stale detached child exit observation"
            );
        });
    }

    #[test]
    fn exit_after_attempt_record_preserves_materialization_handoff() {
        crate::test_support::with_isolated_home(|_| {
            let cook_id = "cook-exit-after-attempt-record";
            let attempt_id = "cook-exit-after-attempt-record-attempt-1";
            agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
                &test_lifecycle_store(),
                cook_id,
            )
            .expect("persist pending handoff parent");
            agent_task_lifecycle::reserve_detached_cook_handoff_materialization_in_store(
                &test_lifecycle_store(),
                cook_id,
                attempt_id,
            )
            .expect("reserve materializing attempt identity");
            let plan = homeboy::agents::agent_tasks::scheduler::AgentTaskPlan::new(
                "exit-after-attempt-record",
                Vec::new(),
            );
            agent_task_lifecycle::submit_plan(&plan, Some(attempt_id))
                .expect("persist attempt before ownership arbitration");

            agent_task_lifecycle::fail_detached_cook_handoff_parent(
                cook_id,
                "detached Cook exited before materializing its first attempt",
            )
            .expect("durable attempt protects the handoff arbitration");

            agent_task_lifecycle::record_cook_attempt_in_store(
                &test_lifecycle_store(),
                cook_id,
                1,
                attempt_id,
            )
            .expect("durable attempt publishes Cook ownership after child exit");
            let parent = agent_task_lifecycle::exact_record(cook_id)
                .expect("read redirected handoff parent");
            assert_eq!(
                parent.state,
                agent_task_lifecycle::AgentTaskRunState::Succeeded
            );
            assert_eq!(
                parent.metadata["detached_cook_handoff"]["state"],
                "redirected"
            );
            assert!(
                agent_task_lifecycle::cook_index_exists(cook_id)
                    .expect("materialization published the Cook index"),
                "the queued attempt remains continuable through its Cook alias"
            );
        });
    }

    #[test]
    fn detached_retry_after_first_handoff_does_not_rereserve_the_parent() {
        crate::test_support::with_isolated_home(|_| {
            let cook_id = "cook-retry-after-handoff";
            let first_attempt = "cook-retry-after-handoff-attempt-1";
            let retry_attempt = "cook-retry-after-handoff-attempt-2";
            agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
                &test_lifecycle_store(),
                cook_id,
            )
            .expect("persist pending handoff parent");
            agent_task_lifecycle::reserve_detached_cook_handoff_materialization_in_store(
                &test_lifecycle_store(),
                cook_id,
                first_attempt,
            )
            .expect("reserve first materialization");
            let plan = homeboy::agents::agent_tasks::scheduler::AgentTaskPlan::new(
                "first-handoff-attempt",
                Vec::new(),
            );
            agent_task_lifecycle::submit_plan(&plan, Some(first_attempt))
                .expect("persist first attempt");
            agent_task_lifecycle::record_cook_attempt_in_store(
                &test_lifecycle_store(),
                cook_id,
                1,
                first_attempt,
            )
            .expect("redirect parent to first attempt");

            agent_task_lifecycle::reserve_detached_cook_handoff_materialization_in_store(
                &test_lifecycle_store(),
                cook_id,
                retry_attempt,
            )
            .expect("a later detached retry bypasses the completed first-handoff reservation");
            agent_task_lifecycle::submit_plan(&plan, Some(retry_attempt))
                .expect("persist detached retry");
            agent_task_lifecycle::record_cook_attempt_in_store(
                &test_lifecycle_store(),
                cook_id,
                2,
                retry_attempt,
            )
            .expect("redirected first handoff accepts later retry registration");
        });
    }

    #[test]
    fn cancellation_lock_first_blocks_later_attempt_materialization() {
        crate::test_support::with_isolated_home(|_| {
            let cook_id = "cook-cancel-before-materialization";
            let attempt_id = "cook-cancel-before-materialization-attempt-1";
            agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
                &test_lifecycle_store(),
                cook_id,
            )
            .expect("persist pending handoff parent");
            let plan = homeboy::agents::agent_tasks::scheduler::AgentTaskPlan::new(
                "cancel-before-materialization-attempt",
                Vec::new(),
            );
            agent_task_lifecycle::submit_plan(&plan, Some(attempt_id))
                .expect("persist attempt before ownership arbitration");

            let cancelled = agent_task_lifecycle::cancel_run(cook_id, None)
                .expect("cancellation wins the handoff arbitration");
            assert_eq!(
                cancelled.state,
                agent_task_lifecycle::AgentTaskRunState::Cancelled
            );

            assert!(
                agent_task_lifecycle::record_cook_attempt_in_store(
                    &test_lifecycle_store(),
                    cook_id,
                    1,
                    attempt_id
                )
                .is_err(),
                "a cancelled parent must prevent attempt and index publication"
            );
            let parent = agent_task_lifecycle::exact_record(cook_id)
                .expect("read cancellation-winning handoff parent");
            assert_eq!(
                parent.state,
                agent_task_lifecycle::AgentTaskRunState::Cancelled
            );
            assert_eq!(
                parent.metadata["detached_cook_handoff"]["cancellation_fence"]["state"],
                "cancelled"
            );
            assert!(
                !agent_task_lifecycle::cook_index_exists(cook_id)
                    .expect("cancel winner left no Cook index"),
                "a cancelled placeholder cannot resurrect a Cook alias"
            );
        });
    }

    #[test]
    fn materialization_lock_first_remains_cancel_resolvable() {
        crate::test_support::with_isolated_home(|_| {
            let cook_id = "cook-materialized-before-cancel";
            let attempt_id = "cook-materialized-before-cancel-attempt-1";
            agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
                &test_lifecycle_store(),
                cook_id,
            )
            .expect("persist pending handoff parent");
            let plan = homeboy::agents::agent_tasks::scheduler::AgentTaskPlan::new(
                "materialized-before-cancel-attempt",
                Vec::new(),
            );
            agent_task_lifecycle::submit_plan(&plan, Some(attempt_id))
                .expect("persist executable attempt");
            agent_task_lifecycle::record_cook_attempt_in_store(
                &test_lifecycle_store(),
                cook_id,
                1,
                attempt_id,
            )
            .expect("materialization wins the handoff arbitration");

            let cancelled = agent_task_lifecycle::cancel_run(cook_id, None)
                .expect("Cook alias resolves to its materialized attempt");
            assert_eq!(cancelled.run_id, attempt_id);
            assert_eq!(
                cancelled.state,
                agent_task_lifecycle::AgentTaskRunState::Cancelled
            );
            assert_eq!(
                agent_task_lifecycle::exact_record(attempt_id)
                    .expect("read materialized cancelled attempt")
                    .state,
                agent_task_lifecycle::AgentTaskRunState::Cancelled
            );
        });
    }

    #[test]
    fn a_dead_detached_process_is_never_reported_as_an_accepted_handoff() {
        crate::test_support::with_isolated_home(|_| {
            let cook_id = "cook-never-submitted";
            agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
                &test_lifecycle_store(),
                cook_id,
            )
            .expect("persist handoff parent");
            let mut child = Command::new("sh")
                .args(["-c", "exit 0"])
                .spawn()
                .expect("spawn a child that exits immediately");

            let handoff =
                await_durable_handoff(cook_id, &mut child, std::time::Duration::from_millis(5_000))
                    .expect("observe exited handoff");

            assert_eq!(handoff.state, DetachedHandoffState::ExitedBeforeHandoff);
            assert_eq!(
                detached_handoff_rejection_reason(handoff.state),
                Some("detached Cook exited before materializing an executable plan")
            );
            assert_eq!(handoff.run_id, None);
            let parent = agent_task_lifecycle::exact_record(cook_id)
                .expect("the observer terminalizes the exited handoff parent");
            assert_eq!(
                parent.state,
                agent_task_lifecycle::AgentTaskRunState::Failed
            );
            assert_eq!(
                parent.metadata["detached_cook_handoff"]["state"],
                "exited_before_handoff"
            );
            assert_eq!(
                parent.metadata["detached_cook_handoff"]["reason"],
                "detached Cook exited before materializing its first attempt"
            );
        });
    }
}
