//! Local-placement Cook detachment.
//!
//! Cook advertises a durable run id "persisted before materialization" as the
//! handle that makes a cook observable and recoverable independently of the
//! caller. A Cook running in the calling client's process group dies with
//! that client (#11476), so a local Cook runs as a daemon `step` instead: the
//! launcher admits its environment, submits the step, and returns a bounded
//! handoff (or, with `--wait`, follows the step's log to completion).
//!
//! The daemon spawns `homeboy agent-task cook --wait --cook-id <id> ...` with
//! `env_clear()` plus the environment admitted by
//! [`agent_task_service::admitted_cook_environment`], so the daemon's own
//! environment never decides which credentials a Cook gets. The step child
//! binds itself to the durable handoff with its exact PID and start identity
//! before it routes anything; the launcher waits for that proof.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use homeboy::agents::agent_task_service;
use homeboy::agents::agent_tasks::lifecycle as agent_task_lifecycle;
use homeboy::cli_surface::{Cli, Commands};
use homeboy::core::daemon::step_job;
use homeboy::core::Error;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::core::io::output_file::write_output_file;

const HANDOFF_SCHEMA: &str = "homeboy/agent-task-cook-local-detach-handoff/v1";

/// Bound on how long the launcher waits for the step child to bind itself and
/// then materialize its first executable attempt before reporting the handoff
/// as pending. `accepted` is reserved for that later proof, so a zero wait
/// emits a pending handoff, never an unproven acceptance.
const DEFAULT_HANDOFF_TIMEOUT_MS: u64 = 30_000;
const HANDOFF_POLL: Duration = Duration::from_millis(100);
const CHILD_DIAGNOSTIC_LOG_BYTES: u64 = 16 * 1024;
const CHILD_DIAGNOSTIC_TEXT_CHARS: usize = 2_048;

/// Test and operator override for the bounded handoff wait.
const HANDOFF_TIMEOUT_ENV: &str = "HOMEBOY_COOK_DETACH_HANDOFF_TIMEOUT_MS";
/// The step job that supervises this Cook; read when its attempt materializes.
const LOCAL_COOK_SUPERVISOR_JOB_ID_ENV: &str = "HOMEBOY_LOCAL_COOK_SUPERVISOR_JOB_ID";
const TEST_LOCAL_COOK_PAUSE_AFTER_CONTROLLER_START_PATH_ENV: &str =
    "HOMEBOY_TEST_LOCAL_COOK_PAUSE_AFTER_CONTROLLER_START_PATH";

/// Set once this process has bound itself to the step that runs it. Later
/// interception calls in the same process are execution, not a new launch.
static STEP_CHILD_BOUND: AtomicBool = AtomicBool::new(false);

/// A parent is persisted before the step exists. Until the step child binds
/// itself to it, an interrupted launcher must terminalize that parent instead
/// of leaving an unactionable pending handoff behind.
struct DetachedCookAdmission {
    store: agent_task_lifecycle::AgentTaskLifecycleStore,
    cook_id: String,
    launcher_id: String,
    pending: bool,
}

impl DetachedCookAdmission {
    fn establish(cook_id: &str) -> homeboy::core::Result<Self> {
        let store = agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
        let launcher_id = std::env::var(crate::cli_runtime::COOK_STARTUP_LAUNCHER_ID_ENV)
            .unwrap_or_else(|_| uuid::Uuid::new_v4().to_string());
        agent_task_lifecycle::claim_detached_cook_handoff_parent_in_store(
            &store,
            cook_id,
            &launcher_id,
        )?;
        Ok(Self {
            store,
            cook_id: cook_id.to_string(),
            launcher_id,
            pending: true,
        })
    }

    fn release(&mut self) {
        self.pending = false;
    }
}

impl Drop for DetachedCookAdmission {
    fn drop(&mut self) {
        if self.pending {
            let _ = agent_task_lifecycle::fail_claimed_detached_cook_handoff_parent_in_store(
                &self.store,
                &self.cook_id,
                &self.launcher_id,
                "detached Cook launcher stopped before its step bound the handoff",
            );
        }
    }
}

/// The step job this process runs as, when a daemon step launched it and it
/// has not yet bound itself.
fn unbound_step_job_id() -> Option<String> {
    if STEP_CHILD_BOUND.load(Ordering::SeqCst) {
        return None;
    }
    std::env::var(step_job::STEP_JOB_ID_ENV)
        .ok()
        .filter(|job_id| !job_id.trim().is_empty())
}

/// Mark this process as the bound step child. The job id leaves the
/// environment so a nested Cook (a gate running `homeboy agent-task cook`)
/// is an ordinary invocation rather than a second claim on this step.
fn mark_step_child_bound(job_id: &str) {
    STEP_CHILD_BOUND.store(true, Ordering::SeqCst);
    std::env::remove_var(step_job::STEP_JOB_ID_ENV);
    std::env::set_var(LOCAL_COOK_SUPERVISOR_JOB_ID_ENV, job_id);
}

fn own_process_identity(
) -> homeboy::core::Result<(u32, homeboy::core::process::ProcessStartIdentity)> {
    let pid = std::process::id();
    let identity = homeboy::core::process::process_start_identity(pid)
        .map_err(Error::internal_unexpected)?
        .ok_or_else(|| Error::internal_unexpected("Cook step has no kernel start identity"))?;
    Ok((pid, identity))
}

/// Whether this is a Cook requesting detached supervision.
fn is_unsupervised_local_cook(cli: &Cli) -> bool {
    cli.detach_after_handoff
        && matches!(
            &cli.command,
            Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
                command: crate::commands::agent_task::AgentTaskCommand::Cook(cook),
            }) if !cook.preview
        )
}

fn local_cook_needs_supervision(cli: &Cli, provider_placement: Option<&str>) -> bool {
    matches!(
        cli.placement,
        homeboy::cli_surface::Placement::Auto
            | homeboy::cli_surface::Placement::Local
            | homeboy::cli_surface::Placement::LabOrLocal
    ) && (provider_placement == Some("local")
        || (cli.placement.allows_local_fallback() && provider_placement == Some("lab")))
        && matches!(
            &cli.command,
            Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
                command: crate::commands::agent_task::AgentTaskCommand::Cook(cook),
            }) if !cook.preview
        )
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

/// Run a local Cook as a daemon `step` once its durable handoff exists. By
/// default return the handoff acknowledgement; `--wait` follows that same
/// durable step until it completes.
///
/// `runner_side` is true when this process is a Lab offload subprocess, a
/// managed-runner placement, or a runner-resident execution. Those children
/// complete the attempt under their existing owner without another handoff.
pub(super) fn intercept_local_detached_cook(
    cli: &Cli,
    normalized_args: &[String],
    output_file: Option<&str>,
    runner_side: bool,
    provider_placement: Option<&str>,
    provider_runner_id: Option<&str>,
) -> homeboy::core::Result<Option<i32>> {
    let Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
        command: crate::commands::agent_task::AgentTaskCommand::Cook(cook),
    }) = &cli.command
    else {
        return Ok(None);
    };
    if STEP_CHILD_BOUND.load(Ordering::SeqCst) {
        return Ok(None);
    }
    // The step child binds its exact identity to the handoff it runs before
    // it can persist an attempt, so cancellation can signal it and the
    // launcher can prove ownership.
    if let Some(job_id) = unbound_step_job_id() {
        if cook.cook_id.is_none() {
            return Ok(None);
        }
        bind_cook_step_child(&job_id, cook)?;
        return Ok(None);
    }
    // Runner-owned attempts must complete in their existing owner's process.
    if runner_side {
        return Ok(None);
    }
    let durable_local_ownership = local_cook_needs_supervision(cli, provider_placement);
    if !is_unsupervised_local_cook(cli) && !durable_local_ownership {
        return Ok(None);
    }
    if !local_cook_supervision_supported() {
        return if cli.detach_after_handoff || durable_local_ownership {
            Err(Error::validation_invalid_argument(
                "placement",
                "local Cook detachment requires a platform with session detachment and exact process start identity support",
                None,
                None,
            ))
        } else {
            Ok(None)
        };
    }

    let requested_cook_id = cook.dispatch.run_id.clone();
    let cook_id = requested_cook_id
        .clone()
        .unwrap_or_else(|| format!("cook-detached-{}", uuid::Uuid::new_v4()));
    // Establish the user-requested handle before any filesystem setup, prompt
    // read, daemon connection, or output path can delay this launcher.
    let mut admission = DetachedCookAdmission::establish(&cook_id)?;
    let mut child_args =
        detached_cook_child_args(normalized_args, &cook_id, cli.detach_after_handoff);
    // A step child cannot answer a `--prompt -`: its stdin is closed and the
    // bytes live only in the launcher's pipe. Capture them here so the exact
    // prompt survives the handoff instead of the cook stalling on an empty read.
    let stdin_prompt_snapshot = cook
        .prompt_snapshot
        .as_ref()
        .filter(|snapshot| snapshot.source == "stdin")
        .map(|snapshot| snapshot.content.clone());
    match stdin_prompt_snapshot {
        Some(prompt) => materialize_stdin_prompt_content(&mut child_args, &cook_id, &prompt)?,
        None => materialize_stdin_prompt(&mut child_args, &cook_id)?,
    };

    // When authoritative daemon status authorizes a confirmation-free idle
    // restart, recover under the exact lease and continue this same request
    // instead of making the operator reconstruct it (#13513).
    let controller_client = if cli.detach_after_handoff {
        homeboy::core::daemon::LocalControllerJobClient::connect_current_build_recovering_idle()?
    } else {
        match homeboy::core::daemon::LocalControllerJobClient::connect_current_build() {
            Ok(client) => client,
            Err(error) if controller_job_daemon_build_mismatch(&error) => {
                if durable_local_ownership {
                    return Err(error);
                }
                return Ok(None);
            }
            Err(error) => return Err(error),
        }
    };
    let mut overrides = vec![(
        crate::cli_runtime::COOK_STARTUP_LAUNCHER_ID_ENV.to_string(),
        admission.launcher_id.clone(),
    )];
    overrides.extend(
        homeboy::core::notification_route::child_env(detached_route(cli).as_ref())
            .into_iter()
            .map(|(name, value)| (name.to_string(), value)),
    );
    let environment = agent_task_service::admitted_cook_environment(overrides)?;
    let (program, cwd) = step_program_and_cwd()?;
    let request = agent_task_service::cook_launch_step_request(
        &cook_id,
        &admission.launcher_id,
        &program,
        child_args,
        &cwd,
        environment,
    );
    let job_id = submit_step(&controller_client, &request)?;
    pause_after_controller_start_for_test()?;
    let store = &admission.store;
    let bound = await_step_binding(&controller_client, &job_id, handoff_timeout(), || {
        store
            .read_record_bounded(&cook_id)
            .map(|record| {
                let handoff = &record.metadata["detached_cook_handoff"];
                handoff["admission_state"] == "supervising"
                    && handoff["supervisor_job_id"] == job_id.as_str()
            })
            .unwrap_or(false)
    })?;
    if !bound {
        let _ = controller_client.cancel(&job_id, "detached Cook step did not bind its handoff");
        return Err(detached_child_pre_admission_error(
            &job_id,
            durable_run_id_for_cook(&cook_id, Some(&job_id)).as_deref(),
            &step_log_path(&job_id),
            "detached Cook step did not bind its handoff before the bounded admission deadline",
        ));
    }
    // The step child owns the handoff now. Later client-side observation
    // failures must not rewrite the authoritative parent.
    admission.release();
    if cli.detach_after_handoff {
        let handoff =
            await_durable_linked_handoff(&controller_client, &cook_id, &job_id, handoff_timeout())?;
        if let Some(reason) = detached_handoff_rejection_reason(handoff.state) {
            return Err(detached_child_pre_admission_error(
                &job_id,
                durable_run_id_for_cook(&cook_id, Some(&job_id)).as_deref(),
                &step_log_path(&job_id),
                reason,
            ));
        }
        if let Some(run_id) = handoff.run_id.as_deref() {
            crate::commands::agent_task::run::announce_durable_cook_identity(
                Some(&cook_id),
                run_id,
            );
            crate::commands::agent_task::run::announce_resolved_execution_placement();
        }
        let envelope = handoff_envelope(
            &cook_id,
            bound_child_pid(&cook_id),
            &step_log_path(&job_id),
            &handoff,
            &job_id,
            cli.placement,
            provider_placement.unwrap_or("local"),
            provider_runner_id,
        );
        let stdout = finalize_handoff_envelope(&envelope, output_file)?;
        // `--output` owns the serialized acknowledgement until this point. Emit
        // those exact bytes so both requested destinations carry one contract.
        print!("{stdout}");
        return Ok(Some(0));
    }

    // Attachment is observation only. The step owns the Cook, so losing this
    // client cannot cancel provider work.
    follow_step_log(&controller_client, &job_id)
}

/// Bind this step child to the detached handoff it runs, with its exact
/// process identity. Re-entry by the same child is a no-op.
fn bind_cook_step_child(
    job_id: &str,
    cook: &crate::commands::agent_task::AgentTaskCookArgs,
) -> homeboy::core::Result<()> {
    let cook_id = cook.dispatch.run_id.clone().ok_or_else(|| {
        Error::validation_invalid_argument(
            "cook_id",
            "a Cook step must name its durable Cook id",
            None,
            None,
        )
    })?;
    let store = agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    let launcher_id = match std::env::var(crate::cli_runtime::COOK_STARTUP_LAUNCHER_ID_ENV) {
        Ok(launcher_id) => launcher_id,
        // A workflow `cook` step has no launcher process: the step itself
        // launches the Cook, so it claims the handoff as its own launcher.
        Err(_) => {
            agent_task_lifecycle::claim_detached_cook_handoff_parent_in_store(
                &store, &cook_id, job_id,
            )?;
            std::env::set_var(crate::cli_runtime::COOK_STARTUP_LAUNCHER_ID_ENV, job_id);
            job_id.to_string()
        }
    };
    let (pid, identity) = own_process_identity()?;
    agent_task_lifecycle::record_claimed_detached_cook_handoff_supervision_in_store(
        &store,
        &cook_id,
        &launcher_id,
        pid,
        identity,
        job_id,
    )?;
    mark_step_child_bound(job_id);
    Ok(())
}

/// The program a step runs is this exact (pinned) runtime, from this exact
/// working directory, so the step executes what the operator invoked.
fn step_program_and_cwd() -> homeboy::core::Result<(PathBuf, PathBuf)> {
    let program = std::env::current_exe().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("resolve current executable for a Cook step".to_string()),
        )
    })?;
    let cwd = std::env::current_dir().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("resolve working directory for a Cook step".to_string()),
        )
    })?;
    Ok((program, cwd))
}

/// Submit and start one step, returning its job id. The key makes a replayed
/// submission converge on the same step instead of spawning a second child.
pub(super) fn submit_step(
    client: &homeboy::core::daemon::LocalControllerJobClient,
    request: &step_job::StepJobRequest,
) -> homeboy::core::Result<String> {
    let job = client.submit(request.submission()?)?;
    let job_id = job.id.to_string();
    client.start(&job_id)?;
    Ok(job_id)
}

/// Wait until `bound` observes the step child's binding, the step ends, or the
/// bound elapses. Returns whether the binding was observed.
fn await_step_binding(
    client: &homeboy::core::daemon::LocalControllerJobClient,
    job_id: &str,
    timeout: Duration,
    bound: impl Fn() -> bool,
) -> homeboy::core::Result<bool> {
    let started = Instant::now();
    loop {
        if bound() {
            return Ok(true);
        }
        if client.status(job_id)?.status.is_terminal() {
            return Ok(bound());
        }
        if started.elapsed() >= timeout {
            return Ok(false);
        }
        std::thread::sleep(HANDOFF_POLL);
    }
}

fn step_log_path(job_id: &str) -> PathBuf {
    step_job::step_job_dir(job_id)
        .map(|dir| dir.join(step_job::STEP_LOG_FILE))
        .unwrap_or_default()
}

fn bound_child_pid(cook_id: &str) -> Option<u32> {
    agent_task_lifecycle::exact_record(cook_id).ok()?.metadata["detached_cook_handoff"]["child_pid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
}

/// Stream the step's log until the step ends, returning the child's exit code.
fn follow_step_log(
    client: &homeboy::core::daemon::LocalControllerJobClient,
    job_id: &str,
) -> homeboy::core::Result<Option<i32>> {
    let log_path = step_log_path(job_id);
    let mut offset = 0_u64;
    loop {
        let terminal = client.status(job_id)?;
        offset = print_log_from(&log_path, offset);
        if terminal.status.is_terminal() {
            print_log_from(&log_path, offset);
            let exit_code = step_job::recorded_step_exit_code(job_id).unwrap_or(
                if terminal.status == homeboy::core::api_jobs::JobStatus::Succeeded {
                    0
                } else {
                    1
                },
            );
            return Ok(Some(exit_code));
        }
        std::thread::sleep(HANDOFF_POLL);
    }
}

fn print_log_from(log_path: &Path, offset: u64) -> u64 {
    use std::io::{Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(log_path) else {
        return offset;
    };
    if file.seek(SeekFrom::Start(offset)).is_err() {
        return offset;
    }
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return offset;
    }
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(&bytes);
    let _ = stdout.flush();
    offset + bytes.len() as u64
}

/// Pause the real launcher at the only boundary that has already started the
/// daemon step. The marker makes interruption coverage deterministic without
/// changing the production handoff sequence.
fn pause_after_controller_start_for_test() -> homeboy::core::Result<()> {
    let Some(path) = std::env::var_os(TEST_LOCAL_COOK_PAUSE_AFTER_CONTROLLER_START_PATH_ENV) else {
        return Ok(());
    };
    std::fs::write(&path, b"controller_started").map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(PathBuf::from(path).display().to_string()),
        )
    })?;
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
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

/// The argv the Cook step executes.
///
/// Explicit `--wait` makes the child execute to terminal completion, and
/// `--cook-id` names the durable Cook it runs. Forwarded arguments stay
/// untouched; the launcher alone owns the handoff acknowledgement and its
/// output file.
fn detached_cook_child_args(
    normalized_args: &[String],
    cook_id: &str,
    consume_output: bool,
) -> Vec<String> {
    let mut args = vec!["--wait".to_string()];
    let owned_args = crate::command_capability::homeboy_owned_args(normalized_args);
    let mut values = owned_args.iter().skip(1);
    while let Some(arg) = values.next() {
        if arg == "--wait" {
            continue;
        }
        if arg == "--run-id" {
            let _ = values.next();
            continue;
        }
        if arg.starts_with("--run-id=") {
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
    args.push("--cook-id".to_string());
    args.push(cook_id.to_string());
    args.extend_from_slice(&normalized_args[owned_args.len()..]);
    args
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
pub(super) fn detached_route(
    cli: &Cli,
) -> Option<homeboy::core::notification_route::NotificationRoute> {
    homeboy::core::notification_route::from_cli_or_env(
        cli.notification_transport.as_deref(),
        cli.notification_route.as_deref(),
    )
    .ok()
    .and_then(homeboy::core::notification_route::NotificationRouteContext::into_route)
}

/// Replace a `--prompt -` stdin request with a private file the step can read.
fn materialize_stdin_prompt(args: &mut [String], cook_id: &str) -> homeboy::core::Result<()> {
    if stdin_prompt_index(args).is_none() {
        return Ok(());
    }
    let prompt =
        homeboy::agents::agent_task_prompts::read_prompt_input_bounded("-", handoff_timeout())?;
    materialize_stdin_prompt_content(args, cook_id, &prompt)
}

fn materialize_stdin_prompt_content(
    args: &mut [String],
    cook_id: &str,
    prompt: &str,
) -> homeboy::core::Result<()> {
    let Some(index) = stdin_prompt_index(args) else {
        return Ok(());
    };
    if prompt.is_empty() {
        return Err(Error::validation_invalid_argument(
            "prompt",
            "agent-task cook --prompt - received empty stdin",
            None,
            None,
        ));
    }
    let path = crate::cli_runtime::persist_cook_stdin_replay_prompt(
        &homeboy::core::paths::homeboy_data()?,
        cook_id,
        prompt,
    )?;
    rewrite_stdin_prompt_arg(args, index, &path);
    Ok(())
}

fn rewrite_stdin_prompt_arg(args: &mut [String], index: usize, path: &Path) {
    args[index] = if args[index] == "-" {
        format!("@{}", path.display())
    } else {
        format!("--prompt=@{}", path.display())
    };
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

/// What the launcher could prove about the cook before returning.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DetachedCookHandoff {
    state: DetachedHandoffState,
    run_id: Option<String>,
    waited_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetachedHandoffState {
    /// The Cook step published its durable cook index; the returned ids are
    /// addressable now.
    Accepted,
    /// The step is alive but has not reached durable submission yet. The
    /// cook id is still the correct handle — it is pinned on the child's argv.
    Pending,
    /// The step ended before publishing durable identity. Its log is the only
    /// evidence, and this is reported rather than dressed up as a successful
    /// handoff.
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

/// Poll durable state until the Cook step materializes an executable,
/// supervisor-linked attempt, the step ends, or the bound elapses.
fn await_durable_linked_handoff(
    client: &homeboy::core::daemon::LocalControllerJobClient,
    cook_id: &str,
    job_id: &str,
    timeout: Duration,
) -> homeboy::core::Result<DetachedCookHandoff> {
    let started = Instant::now();
    loop {
        let waited_ms = started.elapsed().as_millis() as u64;
        if let Some(run_id) = durable_attempt_id(cook_id, Some(job_id)) {
            return Ok(DetachedCookHandoff {
                state: DetachedHandoffState::Accepted,
                run_id: Some(run_id),
                waited_ms,
            });
        }
        if client.status(job_id)?.status.is_terminal() {
            let run_id = durable_attempt_id(cook_id, Some(job_id));
            return Ok(DetachedCookHandoff {
                state: if run_id.is_some() {
                    DetachedHandoffState::Accepted
                } else {
                    DetachedHandoffState::ExitedBeforeHandoff
                },
                run_id,
                waited_ms,
            });
        }
        if started.elapsed() >= timeout {
            return Ok(DetachedCookHandoff {
                state: DetachedHandoffState::Pending,
                run_id: None,
                waited_ms,
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

/// Return a child error only when the captured stream proves it is Homeboy's
/// typed command result. Arbitrary child output is evidence for the session log,
/// not safe structured data for an operator-facing error.
fn detached_child_pre_admission_error(
    job_id: &str,
    durable_run_id: Option<&str>,
    log_path: &Path,
    problem: &str,
) -> Error {
    let diagnostic = detached_child_diagnostic(log_path);
    let (message, child_diagnostic, diagnostic_unavailable_reason) = match diagnostic {
        Ok(diagnostic) => (
            format!(
                "detached Cook failed before durable admission: {}",
                diagnostic["message"].as_str().unwrap_or(problem)
            ),
            Some(diagnostic),
            None,
        ),
        Err(reason) => (problem.to_string(), None, Some(reason)),
    };
    let mut error = Error::validation_invalid_argument(
        "handoff",
        message,
        None,
        Some({
            let mut hints = vec![format!(
                "Inspect the controller job with: homeboy daemon status (controller job: {job_id})."
            )];
            hints.push(match durable_run_id {
                Some(run_id) => format!("Inspect the durable agent-task run with: homeboy agent-task status {run_id}."),
                None => "No durable agent-task run was created; re-run the original detached Cook command after addressing the failure.".to_string(),
            });
            hints
        }),
    );
    error.details = json!({
        "field": "handoff",
        "problem": problem,
        "classification": "detached_cook_pre_admission_failure",
        "controller_job": {
            "id": job_id,
            "reference_type": "controller_job",
            "agent_task_status_compatible": false,
            "follow_up": "homeboy daemon status",
        },
        "durable_agent_task_run": {
            "exists": durable_run_id.is_some(),
            "run_id": durable_run_id,
            "status_command": durable_run_id.map(|run_id| format!("homeboy agent-task status {run_id}")),
        },
        "child_diagnostic": child_diagnostic,
        "child_diagnostic_unavailable_reason": diagnostic_unavailable_reason,
        "launcher_log": log_path.display().to_string(),
        "replay": "re-run the original detached Cook command",
    });
    error
}

/// Read the last typed `homeboy/command-result/v3` failure out of a bounded
/// prefix of a redirected child log. Reused by the exact Cook execution-step
/// worker path (#15009) as well as the detached-launcher pre-admission path
/// this was written for, since both redirect the same JSON envelope to a log
/// file and need the same bounded, redacted extraction.
pub(super) fn detached_child_diagnostic(log_path: &Path) -> Result<Value, &'static str> {
    let file =
        std::fs::File::open(log_path).map_err(|_| "child diagnostic log could not be read")?;
    let mut bytes = Vec::new();
    file.take(CHILD_DIAGNOSTIC_LOG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "child diagnostic log could not be read")?;
    if bytes.is_empty() {
        return Err("child diagnostic log was empty");
    }
    if bytes.len() as u64 > CHILD_DIAGNOSTIC_LOG_BYTES {
        return Err("child diagnostic log exceeded the bounded diagnostic capture");
    }
    let error = bytes
        .iter()
        .enumerate()
        .filter(|(_, byte)| **byte == b'{')
        .find_map(|(start, _)| {
            let mut stream = serde_json::Deserializer::from_slice(&bytes[start..]);
            let value = Value::deserialize(&mut stream).ok()?;
            value
                .as_object()
                .filter(|result| {
                    result.get("schema").and_then(Value::as_str)
                        == Some("homeboy/command-result/v3")
                        && result.get("success").and_then(Value::as_bool) == Some(false)
                })
                // The real v3 envelope carries its typed failure under
                // `diagnostics`, not `error` (`CommandResultEnvelope::diagnostics`,
                // `homeboy-cli/src/commands/utils/response.rs`). This previously
                // read a key production output never emits, so no detached-Cook
                // launcher failure was ever actually classified — every call
                // fell through to the narrow fallback below.
                .and_then(|result| result.get("diagnostics"))
                .filter(|error| {
                    error.get("code").and_then(Value::as_str).is_some()
                        && error.get("message").and_then(Value::as_str).is_some()
                })
                .cloned()
        })
        .ok_or("child diagnostic log did not contain a typed command error")?;
    let mut error = homeboy::core::redaction::redact_json(&error);
    bound_child_diagnostic(&mut error, 0);
    Ok(error)
}

fn durable_run_id_for_cook(cook_id: &str, supervisor_job_id: Option<&str>) -> Option<String> {
    let parent = agent_task_lifecycle::exact_record(cook_id).ok();
    let handoff = parent
        .as_ref()
        .map(|record| &record.metadata["detached_cook_handoff"]);
    let run_id = agent_task_lifecycle::cook_index(cook_id)
        .ok()
        .map(|index| index.latest_run_id)
        .or_else(|| handoff.and_then(|value| value["attempt_run_id"].as_str().map(str::to_string)))
        .or_else(|| {
            handoff.and_then(|value| {
                value["materializing_attempt_run_id"]
                    .as_str()
                    .map(str::to_string)
            })
        })?;
    if run_id.trim().is_empty() {
        return None;
    }
    let record = agent_task_lifecycle::exact_record(&run_id).ok()?;
    supervisor_job_id
        .is_none_or(|job_id| {
            record.metadata["local_cook_supervisor"]["job_id"] == job_id
                || handoff.is_some_and(|value| value["supervisor_job_id"] == job_id)
        })
        .then_some(run_id)
}

fn bound_child_diagnostic(value: &mut Value, depth: usize) {
    if depth >= 4 {
        *value = Value::String("[omitted: diagnostic depth limit]".to_string());
        return;
    }
    match value {
        Value::String(text) => {
            if text.chars().count() > CHILD_DIAGNOSTIC_TEXT_CHARS {
                *text = format!(
                    "{}...[truncated]",
                    text.chars()
                        .take(CHILD_DIAGNOSTIC_TEXT_CHARS)
                        .collect::<String>()
                );
            }
        }
        Value::Array(items) => {
            items.truncate(8);
            for item in items {
                bound_child_diagnostic(item, depth + 1);
            }
        }
        Value::Object(entries) => {
            for item in entries.values_mut() {
                bound_child_diagnostic(item, depth + 1);
            }
        }
        _ => {}
    }
}

/// The launcher's only output: a bounded, machine-readable handoff naming the
/// durable handle and the evidence needed to follow or stop the cook.
#[allow(clippy::too_many_arguments)]
fn handoff_envelope(
    cook_id: &str,
    pid: Option<u32>,
    log_path: &Path,
    handoff: &DetachedCookHandoff,
    job_id: &str,
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
        // The daemon `step` that runs this cook, inspectable through the job
        // API as well as through its durable cook record.
        "controller_job": { "state": "owned", "job_id": job_id },
        "status_command": format!("homeboy agent-task status {cook_id}"),
        "watch_command": format!("homeboy agent-task status {cook_id} --watch"),
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
    use std::process::Command;

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

        let (cli, _) = cook_cli(&["--placement", "local"]);

        let propagated =
            homeboy::core::notification_route::child_env(detached_route(&cli).as_ref());
        assert!(
            propagated.is_empty() || propagated.len() == 2,
            "propagated {propagated:?}"
        );
    }

    #[test]
    fn a_runner_owned_execution_completes_without_recursive_detachment() {
        let (cli, normalized) = cook_cli(&["--placement", "local"]);

        assert_eq!(
            intercept_local_detached_cook(&cli, &normalized, None, true, Some("local"), None)
                .expect("runner retains execution ownership"),
            None
        );
    }

    /// Local placement transfers durable ownership before provider submission;
    /// the initiating client is only an observer. Non-local and preview Cooks
    /// must not enter this handoff.
    #[test]
    fn local_cook_ownership_is_not_optional() {
        let normalized = args(&[
            "homeboy",
            "--wait",
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
        let local = Cli::try_parse_from(&normalized).expect("parse local Cook");
        assert!(!is_unsupervised_local_cook(&local));
        assert!(local_cook_needs_supervision(&local, Some("local")));
        assert!(!local_cook_needs_supervision(&local, Some("lab")));

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
        assert!(!is_unsupervised_local_cook(&preview));
        assert!(!local_cook_needs_supervision(&preview, Some("local")));

        let (fallback, _) = cook_cli(&["--placement", "lab-or-local", "--wait"]);
        assert!(local_cook_needs_supervision(&fallback, Some("local")));
        assert!(local_cook_needs_supervision(&fallback, Some("lab")));

        let (auto, normalized) = cook_cli(&["--placement", "auto", "--wait"]);
        assert!(!is_unsupervised_local_cook(&auto));
        assert!(local_cook_needs_supervision(&auto, Some("local")));
        assert_eq!(
            intercept_local_detached_cook(&auto, &normalized, None, false, Some("lab"), None,)
                .expect("non-local route falls through"),
            None,
        );
    }

    #[test]
    fn detachment_owns_the_controller_for_auto_lab_and_local_placement() {
        for placement in ["auto", "lab", "local", "lab-or-local"] {
            let (cli, _) = cook_cli(&["--placement", placement]);
            assert!(is_unsupervised_local_cook(&cli), "{placement}");
        }
    }

    #[test]
    fn a_non_cook_command_is_never_intercepted() {
        let cli = Cli::try_parse_from(["homeboy", "--placement", "local", "status"])
            .expect("parse status invocation");

        assert!(!is_unsupervised_local_cook(&cli));
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
    fn child_args_drop_detach_and_pin_a_generated_cook_id() {
        let normalized = args(&[
            "homeboy",
            "--placement",
            "local",
            "agent-task",
            "cook",
            "--prompt",
            "fix it",
        ]);

        let child = detached_cook_child_args(&normalized, "cook-generated", true);

        assert_eq!(
            child,
            args(&[
                "--wait",
                "--placement",
                "local",
                "agent-task",
                "cook",
                "--prompt",
                "fix it",
                "--cook-id",
                "cook-generated",
            ])
        );
    }

    #[test]
    fn child_args_preserve_an_explicit_cook_id_without_duplicating_it() {
        let normalized = args(&[
            "homeboy",
            "--placement",
            "local",
            "agent-task",
            "cook",
            "--run-id",
            "cook-explicit",
            "--prompt",
            "fix it",
        ]);

        let child = detached_cook_child_args(&normalized, "cook-explicit", true);

        assert!(!child.iter().any(|arg| arg == "--run-id"), "{child:?}");
        assert_eq!(
            child.iter().filter(|arg| *arg == "--cook-id").count(),
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
                "--wait",
                "--placement",
                "local",
                "agent-task",
                "cook",
                "--prompt",
                "fix it",
                "--cook-id",
                "cook-explicit",
            ])
        );
    }

    #[test]
    fn child_args_leave_the_handoff_output_owned_by_the_launcher() {
        let child = detached_cook_child_args(
            &args(&[
                "homeboy",
                "--output",
                "/tmp/handoff.json",
                "agent-task",
                "cook",
                "--prompt",
                "fix it",
            ]),
            "cook-output",
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

        let child = detached_cook_child_args(&normalized, "cook-generated", true);

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
    fn an_accepted_handoff_reports_the_addressable_ids_and_follow_up_commands() {
        let handoff = DetachedCookHandoff {
            state: DetachedHandoffState::Accepted,
            run_id: Some("cook-11476-attempt-1-ab12cd34".to_string()),
            waited_ms: 240,
        };

        let envelope = handoff_envelope(
            "cook-11476",
            Some(4242),
            Path::new("/data/artifacts/jobs/3f2b1c00/step.log"),
            &handoff,
            "3f2b1c00-0000-4000-8000-000000000001",
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

    #[test]
    fn an_exited_child_preserves_a_bounded_redacted_typed_diagnostic() {
        let directory = tempfile::tempdir().expect("diagnostic directory");
        let log_path = directory.path().join("cook.log");
        let result = serde_json::to_string_pretty(&json!({
                "schema": "homeboy/command-result/v3",
                "success": false,
                "diagnostics": {
                    "code": "validation.invalid_argument",
                    "message": format!("provider token=super-secret was rejected {}", "x".repeat(CHILD_DIAGNOSTIC_TEXT_CHARS)),
                    "details": { "field": "provider", "nested": { "value": "x" } },
                    "hints": ["supply a valid provider"]
                }
            }))
            .expect("serialize typed child diagnostic");
        std::fs::write(&log_path, format!("progress: starting Cook\n{result}\n"))
            .expect("write child diagnostic");

        let error = detached_child_pre_admission_error(
            "controller-job-14376",
            Some("cook-14376-attempt-1"),
            &log_path,
            "detached Cook exited before materializing an executable plan",
        );

        assert_eq!(
            error.details["classification"],
            "detached_cook_pre_admission_failure"
        );
        assert_eq!(
            error.details["child_diagnostic"]["code"],
            "validation.invalid_argument"
        );
        assert_eq!(
            error.details["child_diagnostic"]["details"]["field"],
            "provider"
        );
        let child_message = error.details["child_diagnostic"]["message"]
            .as_str()
            .expect("typed message");
        assert!(child_message.contains("[REDACTED]"));
        assert!(child_message.ends_with("...[truncated]"));
        assert_eq!(error.details["durable_agent_task_run"]["exists"], true);
        assert_eq!(
            error.details["durable_agent_task_run"]["run_id"],
            "cook-14376-attempt-1"
        );
        assert_eq!(
            error.details["durable_agent_task_run"]["status_command"],
            "homeboy agent-task status cook-14376-attempt-1"
        );
        assert_eq!(
            error.details["controller_job"]["agent_task_status_compatible"],
            false
        );
        assert_eq!(
            error.details["controller_job"]["follow_up"],
            "homeboy daemon status"
        );
    }

    #[test]
    fn missing_child_diagnostic_uses_a_narrow_explained_fallback() {
        let directory = tempfile::tempdir().expect("diagnostic directory");
        let log_path = directory.path().join("absent-cook.log");

        let error = detached_child_pre_admission_error(
            "controller-job-absent",
            None,
            &log_path,
            "detached Cook exited before materializing an executable plan",
        );

        assert_eq!(error.details["child_diagnostic"], Value::Null);
        assert_eq!(
            error.details["child_diagnostic_unavailable_reason"],
            "child diagnostic log could not be read"
        );
        assert_eq!(error.details["durable_agent_task_run"]["exists"], false);
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
                None,
                Path::new("/tmp/cook.log"),
                &handoff,
                "job-unproven",
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
            Some(4242),
            Path::new("/tmp/cook.log"),
            &handoff,
            "job-owned",
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

    /// A corrupt, unparseable on-disk index for an unrelated Cook must not
    /// block admission of a new one: the index is a derived projection and
    /// SQLite is the lifecycle authority (#14962).
    #[test]
    fn detached_cook_admission_survives_a_corrupt_historical_cook_index() {
        crate::test_support::with_isolated_home(|_| {
            let store = test_lifecycle_store();
            let corrupt_index_path = store.cook_index_path("unrelated-historical-cook");
            std::fs::create_dir_all(
                corrupt_index_path
                    .parent()
                    .expect("corrupt historical index parent"),
            )
            .expect("create corrupt historical index parent");
            std::fs::write(&corrupt_index_path, b"{ this is not valid json")
                .expect("write corrupt historical Cook index");

            let cook_id = "cook-admits-despite-corrupt-history";
            let mut admission = DetachedCookAdmission::establish(cook_id).expect(
                "a corrupt unrelated historical Cook index must not block admission (#14962)",
            );

            let parent = store
                .read_record_bounded(cook_id)
                .expect("newly admitted parent is durable despite the corrupt sibling index");
            assert_eq!(parent.metadata["detached_cook_handoff"]["cook_id"], cook_id);
            admission.release();
        });
    }

    #[test]
    fn interrupted_pre_supervisor_admission_is_terminal_and_discoverable() {
        crate::test_support::with_isolated_home(|_| {
            let cook_id = "cook-interrupted-before-stdin";
            let admission = DetachedCookAdmission::establish(cook_id)
                .expect("persist parent before any launcher input work");
            drop(admission);

            let parent = agent_task_lifecycle::exact_record(cook_id)
                .expect("interrupted parent remains addressable");
            assert_eq!(
                parent.state,
                agent_task_lifecycle::AgentTaskRunState::Failed
            );
            assert_eq!(
                parent.metadata["detached_cook_handoff"]["admission_state"],
                "failed"
            );
        });
    }

    #[test]
    fn second_launcher_cannot_adopt_or_terminalize_pending_handoff_parent() {
        crate::test_support::with_isolated_home(|_| {
            let cook_id = "cook-exclusive-launcher";
            let first = DetachedCookAdmission::establish(cook_id)
                .expect("first launcher owns pending parent");
            let second = DetachedCookAdmission::establish(cook_id);
            assert!(second.is_err(), "second launcher must be fenced out");

            let parent = agent_task_lifecycle::exact_record(cook_id)
                .expect("first parent remains discoverable");
            assert_eq!(
                parent.state,
                agent_task_lifecycle::AgentTaskRunState::Queued
            );
            assert_eq!(parent.metadata["detached_cook_handoff"]["state"], "pending");
            assert_eq!(
                parent.metadata["detached_cook_handoff"]["admission_state"],
                "pre_supervisor"
            );
            assert!(parent.metadata["detached_cook_handoff"]["launcher_id"]
                .as_str()
                .is_some_and(|id| !id.is_empty()));

            drop(first);
            assert_eq!(
                agent_task_lifecycle::exact_record(cook_id)
                    .expect("owner cleanup terminalizes parent")
                    .state,
                agent_task_lifecycle::AgentTaskRunState::Failed
            );
        });
    }

    #[test]
    fn a_pending_handoff_cook_id_resolves_to_its_lifecycle_parent() {
        crate::test_support::with_isolated_home(|_| {
            agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
                &test_lifecycle_store(),
                "cook-pending",
            )
            .expect("persist handoff parent");

            let status = agent_task_lifecycle::reconcile_status("cook-pending")
                .expect("pending handoff status resolves");
            let logs =
                agent_task_lifecycle::logs("cook-pending").expect("pending handoff logs resolve");

            assert_eq!(status.run_id, "cook-pending");
            assert_eq!(status.metadata["detached_cook_handoff"]["state"], "pending");
            assert_eq!(logs.run.as_str(), "cook-pending");
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
                homeboy::core::process::process_start_identity(child.id())
                    .expect("inspect child")
                    .expect("capture child identity"),
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
    fn cancellation_during_pre_materialization_signals_the_supervised_child() {
        crate::test_support::with_isolated_home(|_| {
            let cook_id = "cook-cancel-during-pre-materialization";
            let mut admission =
                DetachedCookAdmission::establish(cook_id).expect("claim handoff parent");
            let mut child = Command::new("sh")
                .args(["-c", "sleep 30"])
                .spawn()
                .expect("spawn pre-materialization child");
            let identity = homeboy::core::process::process_start_identity(child.id())
                .expect("inspect child")
                .expect("capture child identity");
            agent_task_lifecycle::record_claimed_detached_cook_handoff_supervision_in_store(
                &admission.store,
                cook_id,
                &admission.launcher_id,
                child.id(),
                identity,
                "supervisor-pre-materialization",
            )
            .expect("persist child and supervisor before guard release");

            let cancelled = agent_task_lifecycle::cancel_run(cook_id, None)
                .expect("cancel pending supervised handoff");
            assert_eq!(
                cancelled.state,
                agent_task_lifecycle::AgentTaskRunState::Cancelled
            );
            assert_eq!(
                cancelled.metadata["detached_cook_handoff"]["supervisor_job_id"],
                "supervisor-pre-materialization"
            );
            assert!(
                cancelled
                    .metadata
                    .get("detached_cook_handoff_cancellation")
                    .is_some(),
                "cancellation must use the durable child identity"
            );
            let _ = child.wait();
            assert!(
                matches!(
                    homeboy::core::process::process_identity_state(child.id(), None),
                    homeboy::core::process::ProcessIdentityState::Dead
                ),
                "cancelled child must be dead"
            );
            admission.release();
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
    fn stdin_prompt_is_captured_verbatim_for_the_cook_step() {
        homeboy::core::test_support::with_isolated_home(|_| {
            let mut child = args(&["agent-task", "cook", "--prompt", "-"]);
            let piped = "fix `$THING`\n\nwith a trailing newline\n";

            materialize_stdin_prompt_content(&mut child, "cook-stdin", piped)
                .expect("materialize stdin prompt");

            let path = child
                .last()
                .and_then(|arg| arg.strip_prefix('@'))
                .expect("prompt rewritten to a file reference");
            assert_eq!(std::fs::read_to_string(path).expect("prompt file"), piped);
        });
    }

    #[test]
    fn a_prompt_that_is_not_stdin_is_left_alone() {
        homeboy::core::test_support::with_isolated_home(|_| {
            let mut child = args(&["agent-task", "cook", "--prompt", "@task.md"]);
            materialize_stdin_prompt_content(&mut child, "cook-file-prompt", "unused")
                .expect("inspect prompt");
            assert_eq!(child.last().map(String::as_str), Some("@task.md"));
        });
    }

    #[test]
    fn an_attached_stdin_prompt_keeps_its_option_when_rewritten() {
        homeboy::core::test_support::with_isolated_home(|_| {
            let mut child = args(&["agent-task", "cook", "--prompt=-", "--no-finalize"]);
            materialize_stdin_prompt_content(&mut child, "cook-attached-prompt", "piped")
                .expect("materialize stdin prompt");
            assert!(child[2].starts_with("--prompt=@"), "{child:?}");
        });
    }

    #[test]
    fn launch_steps_run_this_runtime_with_a_pinned_cook_id_and_linked_run() {
        let child = detached_cook_child_args(
            &args(&[
                "homeboy",
                "--placement",
                "local",
                "agent-task",
                "cook",
                "--run-id=cook-step",
                "--prompt",
                "fix it",
            ]),
            "cook-step",
            true,
        );
        let request = agent_task_service::cook_launch_step_request(
            "cook-step",
            "launcher-a",
            Path::new("/opt/homeboy/bin/homeboy"),
            child.clone(),
            Path::new("/work"),
            homeboy::core::env_materialization_plan::EnvMaterializationPlan::default(),
        );
        assert_eq!(request.args, child);
        assert_eq!(request.args.first().map(String::as_str), Some("--wait"));
        assert!(request
            .args
            .windows(2)
            .any(|pair| pair == ["--cook-id", "cook-step"]));
        assert!(!request.args.iter().any(|arg| arg.starts_with("--run-id")));
        assert_eq!(request.linked_durable_run_id.as_deref(), Some("cook-step"));
        let submission = request.submission().expect("valid step submission");
        assert_eq!(submission["type"], step_job::STEP_JOB_TYPE);
        assert_eq!(
            submission["idempotency_key"],
            "agent-task-cook-launch:cook-step:launcher-a"
        );
    }
}
