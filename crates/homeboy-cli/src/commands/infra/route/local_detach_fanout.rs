//! Local-placement fanout detachment.
//!
//! `--detach-after-handoff` is a global flag, and it is advertised on
//! `fanout cook-batch` and `fanout run-plan`. It did not work there. Every gate
//! that acted on it tested for Cook specifically — `detached_cook_can_queue`,
//! `is_local_detached_cook` — and locally-placed fanout returned from
//! `run_split_placement_fanout` before any detach handling. So
//! `fanout cook-batch --detach-after-handoff --placement local` accepted a flag
//! promising the caller could disconnect, then blocked that caller for hours and
//! died with its terminal, orphaning every in-flight child.
//!
//! This module makes the flag mean what it says, on the same terms as the Cook
//! launcher next door: re-execute the coordinator in its own session, hand
//! durable ownership of its lifecycle to the daemon as a typed controller job,
//! and return a bounded handoff naming the batch.
//!
//! # Why the launcher spawns and the daemon only supervises
//!
//! Identical to `local_detach`, and more load-bearing here. The ambient process
//! environment is the first secret provider a cook consults; the real provider
//! invocation never calls `env_clear`; the daemon inherits its environment and
//! working directory from whichever caller first started it. A daemon-hosted
//! wave would therefore run *every child* against credentials from an
//! environment no operator chose. Spawning the coordinator here preserves the
//! operator's environment exactly.
//!
//! # Where the flag still refuses
//!
//! Detachment needs a coordinator to detach *from*. `fanout plan`, `submit`,
//! `submit-batch`, `status`, `resume`, `artifacts`, and a `cook-batch` without
//! `--run-plan` all return promptly and own no long-running work, so there is
//! nothing to hand to the daemon. Those are rejected with an explicit error
//! rather than silently ignored: a flag that refuses is better than a flag that
//! lies, which is the whole defect being fixed.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use homeboy::agents::agent_task_service;
use homeboy::cli_surface::{Cli, Commands};
use homeboy::core::Error;
use serde_json::{json, Value};

use crate::commands::agent_task::{
    AgentTaskArgs, AgentTaskCommand, AgentTaskFanoutArgs, AgentTaskFanoutCommand,
};

const HANDOFF_SCHEMA: &str = "homeboy/agent-task-fanout-local-detach-handoff/v1";

/// Bound on how long the launcher waits for the detached coordinator to publish
/// its durable batch record before reporting the handoff as still pending.
///
/// What this wait buys is that `fanout status <fanout_id>` resolves the moment
/// the envelope is printed. Unlike a Cook — whose id is made resolvable before a
/// child exists by `record_detached_cook_handoff_parent` — a batch record cannot
/// be written by the launcher, because the launcher has not loaded the plan and
/// so does not know the children. Only the coordinator can write it.
///
/// The fanout id itself is addressable regardless: it is pinned onto the child's
/// argv, and it is the controller job's idempotency key.
const DEFAULT_HANDOFF_TIMEOUT_MS: u64 = 30_000;
const HANDOFF_POLL: Duration = Duration::from_millis(100);

/// Test and operator override for the bounded handoff wait.
const HANDOFF_TIMEOUT_ENV: &str = "HOMEBOY_FANOUT_DETACH_HANDOFF_TIMEOUT_MS";

/// A fanout invocation that owns a long-running coordinator, and the durable id
/// that coordinator will key its batch on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DetachableFanout {
    fanout_id: String,
    /// Whether the fanout id must be pinned onto the child's argv.
    ///
    /// False when the caller already pinned it, which matters because
    /// `--record-run-id` rekeys a plan *after* `--fanout-id` does; appending a
    /// second, contradictory id would silently change which batch the daemon
    /// was told to supervise.
    pin_fanout_id: bool,
}

/// Whether this invocation is a locally-placed fanout wave asking to detach,
/// and the coordinator it would detach.
///
/// `Ok(None)` means "not this interceptor's business". `Err` means the request
/// was addressed to this interceptor and cannot be honoured.
fn detachable_local_fanout(cli: &Cli) -> homeboy::core::Result<Option<DetachableFanout>> {
    if !cli.detach_after_handoff || cli.placement != homeboy::cli_surface::Placement::Local {
        return Ok(None);
    }
    let Commands::AgentTask(AgentTaskArgs {
        command: AgentTaskCommand::Fanout(AgentTaskFanoutArgs { command }),
    }) = &cli.command
    else {
        return Ok(None);
    };

    match command {
        AgentTaskFanoutCommand::RunPlan(args) => {
            // `--record-run-id` rekeys the plan after `--fanout-id`, so it is
            // the id the batch record will actually carry when both are given.
            if let Some(record_run_id) = &args.record_run_id {
                return Ok(Some(DetachableFanout {
                    fanout_id: record_run_id.clone(),
                    pin_fanout_id: false,
                }));
            }
            Ok(Some(match &args.input.fanout_id {
                Some(fanout_id) => DetachableFanout {
                    fanout_id: fanout_id.clone(),
                    pin_fanout_id: false,
                },
                None => DetachableFanout {
                    fanout_id: generated_fanout_id(),
                    pin_fanout_id: true,
                },
            }))
        }
        AgentTaskFanoutCommand::CookBatch(args) => {
            if !args.run_plan {
                return Err(no_coordinator_error(
                    "agent-task fanout cook-batch",
                    "Add `--run-plan` to execute the wave from this machine, which is the coordinator that can be detached.",
                ));
            }
            if args.dry_run {
                return Err(no_coordinator_error(
                    "agent-task fanout cook-batch --dry-run",
                    "A dry run plans without executing, so it owns no coordinator. Drop `--dry-run` to detach a real wave.",
                ));
            }
            Ok(Some(match &args.fanout_id {
                Some(fanout_id) => DetachableFanout {
                    fanout_id: fanout_id.clone(),
                    pin_fanout_id: false,
                },
                None => DetachableFanout {
                    fanout_id: generated_fanout_id(),
                    pin_fanout_id: true,
                },
            }))
        }
        // Long-running, and currently orphaned by detachment exactly as a wave
        // was: resume rebases, re-runs gates, pushes, and opens pull requests,
        // which can take hours. It is not daemon-owned yet, and silently
        // ignoring the flag here would reproduce the defect this module exists
        // to remove, so it refuses and names what to do instead.
        AgentTaskFanoutCommand::Resume(_) => Err(no_coordinator_error(
            "agent-task fanout resume",
            "Resume harvests terminal children through their existing gate and finalization contract, and is not yet daemon-owned. Run it attached, or detach the wave itself with `fanout run-plan --detach-after-handoff`.",
        )),
        // Deliberately not this interceptor's business, and deliberately not an
        // error either.
        //
        // The defect being fixed is a flag that *lies*: one that promises the
        // caller can disconnect while real work is in flight, then orphans that
        // work. These commands own no work. `plan` normalizes and prints,
        // `submit`/`submit-batch` hand work to another executor, `status` and
        // `artifacts` are read-only projections — every one of them returns
        // promptly and leaves nothing running. The flag is inert here, not
        // false.
        //
        // Rejecting it anyway would break the common shape of setting a global
        // flag once in a wrapper and running several subcommands under it, and
        // would buy no safety, so it falls through to normal routing.
        AgentTaskFanoutCommand::Plan(_)
        | AgentTaskFanoutCommand::Submit(_)
        | AgentTaskFanoutCommand::SubmitBatch(_)
        | AgentTaskFanoutCommand::Status(_)
        | AgentTaskFanoutCommand::Artifacts(_) => Ok(None),
    }
}

/// A fanout id for a wave the operator did not name.
///
/// Deliberately built from a uuid so it survives `sanitize_path_segment`
/// unchanged: the durable batch record is keyed by the sanitized id, and a
/// launcher that submitted one id while the coordinator wrote another would
/// leave the daemon supervising a batch that never appears.
fn generated_fanout_id() -> String {
    format!("fanout-detached-{}", uuid::Uuid::new_v4())
}

fn no_coordinator_error(subject: &str, remedy: &str) -> Error {
    Error::validation_invalid_argument(
        "detach-after-handoff",
        format!(
            "{subject} cannot detach after handoff with `--placement local` because it owns no long-running coordinator to hand to the daemon"
        ),
        None,
        Some(vec![remedy.to_string()]),
    )
}

/// The one context where fanout detachment stays a rejection outright.
fn runner_side_detach_error() -> Error {
    Error::validation_invalid_argument(
        "detach-after-handoff",
        "agent-task fanout cannot detach after handoff with --placement local inside a runner-owned execution because the runner already owns this work",
        None,
        Some(vec![
            "Detach from the controller that coordinates the wave, not from the runner process executing one of its attempts.".to_string(),
        ]),
    )
}

/// Serve `--placement local --detach-after-handoff` for a fanout wave by
/// re-executing its coordinator in its own session and returning a bounded
/// handoff.
///
/// `runner_side` is true when this process is a Lab offload subprocess, a
/// managed-runner placement, or a runner-resident execution. There the request
/// is genuinely unserveable: the process is already the runner's owned
/// execution and has no controller lifecycle to hand off.
pub(super) fn intercept_local_detached_fanout(
    cli: &Cli,
    normalized_args: &[String],
    runner_side: bool,
) -> homeboy::core::Result<Option<i32>> {
    let Some(target) = detachable_local_fanout(cli)? else {
        return Ok(None);
    };
    if runner_side {
        return Err(runner_side_detach_error());
    }

    let DetachableFanout {
        fanout_id,
        pin_fanout_id,
    } = target;
    let session_root = detached_session_root(&fanout_id)?;
    let mut child_args = detached_fanout_child_args(normalized_args, &fanout_id, pin_fanout_id);
    // A detached coordinator cannot answer an `--input -`: its stdin is closed
    // and the plan lives only in the launcher's pipe. Capture it here so the
    // exact plan survives the handoff instead of the wave stalling on an empty
    // read.
    materialize_stdin_plan(&mut child_args, &session_root)?;
    let log_path = session_root.join("fanout.log");

    let route = detached_route(cli);
    let mut child = spawn_detached_fanout(&child_args, &fanout_id, &log_path, route.as_ref())?;
    let pid = child.id();
    let start_identity = match detached_child_start_identity(pid) {
        Ok(identity) => identity,
        Err(error) => {
            terminate_and_reap_detached_child(&mut child);
            return Err(error);
        }
    };

    // A detached handoff promises daemon ownership, not merely a session-
    // detached PID. If admission fails, stop the coordinator before returning
    // so the caller cannot mistake an orphaned wave for durable work.
    let controller_job = hand_off_to_controller(
        &mut child,
        submit_batch_controller_job(&fanout_id, pid, &start_identity),
    )?;
    let handoff = await_durable_handoff(&fanout_id, &mut child, handoff_timeout());

    let envelope = handoff_envelope(&fanout_id, pid, &log_path, &handoff, &controller_job);
    let stdout = serde_json::to_string_pretty(&envelope).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize detached local fanout handoff".to_string()),
        )
    })?;
    println!("{stdout}");
    Ok(Some(0))
}

/// Durable daemon ownership of a detached wave.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ControllerJobHandoff {
    Owned { job_id: String },
}

impl ControllerJobHandoff {
    fn projection(&self) -> Value {
        match self {
            Self::Owned { job_id } => json!({ "state": "owned", "job_id": job_id }),
        }
    }
}

/// Offer the detached wave to the daemon as a durable controller job.
///
/// `submit` admits the job and returns its id synchronously; `start` releases
/// the daemon worker that supervises the coordinator. The batch id is the job's
/// idempotency key, so a replayed submit converges on one supervisor rather than
/// creating a second one for the same wave.
fn submit_batch_controller_job(
    batch_id: &str,
    pid: u32,
    start_identity: &homeboy::core::process::ProcessStartIdentity,
) -> homeboy::core::Result<String> {
    submit_batch_controller_job_inner(batch_id, pid, start_identity)
}

/// Convert a spawned coordinator into a durable handoff, or leave no work
/// behind. The child exists before daemon admission because its PID and kernel
/// start identity are the daemon's liveness proof.
fn hand_off_to_controller(
    child: &mut std::process::Child,
    submission: homeboy::core::Result<String>,
) -> homeboy::core::Result<ControllerJobHandoff> {
    match submission {
        Ok(job_id) => Ok(ControllerJobHandoff::Owned { job_id }),
        Err(error) => {
            terminate_and_reap_detached_child(child);
            Err(error)
        }
    }
}

fn submit_batch_controller_job_inner(
    batch_id: &str,
    pid: u32,
    start_identity: &homeboy::core::process::ProcessStartIdentity,
) -> homeboy::core::Result<String> {
    let submission = agent_task_service::cook_batch_job_submission(batch_id, pid, start_identity)?;
    let client = homeboy::core::daemon::LocalControllerJobClient::connect()?;
    let job = client.submit(submission)?;
    let job_id = job.id.to_string();
    client.start(&job_id)?;
    Ok(job_id)
}

/// The argv the detached coordinator executes.
///
/// It is the caller's own argv with at most two edits: the detach request is
/// consumed by the launcher, and the fanout id is pinned when the caller did
/// not name one. Dropping `--detach-after-handoff` is what makes the child
/// coordinate its own wave to a terminal report, because coordinating is the
/// default. The parent is the only process authorized to detach. Everything
/// else is preserved byte for byte.
fn detached_fanout_child_args(
    normalized_args: &[String],
    fanout_id: &str,
    pin_fanout_id: bool,
) -> Vec<String> {
    let owned_args = crate::command_capability::homeboy_owned_args(normalized_args);
    let mut args: Vec<String> = owned_args
        .iter()
        .skip(1)
        .filter(|arg| {
            arg.as_str() != "--detach-after-handoff" && !arg.starts_with("--detach-after-handoff=")
        })
        .cloned()
        .collect();
    if pin_fanout_id {
        args.push("--fanout-id".to_string());
        args.push(fanout_id.to_string());
    }
    args.extend(normalized_args.iter().skip(owned_args.len()).cloned());
    args
}

/// The destination the detached wave's notifications belong to.
///
/// Resolved from the launcher's own arguments rather than from
/// `notification_route::current()`, because detachment is intercepted during
/// argument routing — before the runtime binds the thread-local route — so the
/// thread-local is still empty here.
///
/// A route this process could not resolve is `None` rather than an error.
/// Notification routing is observability, which must never take a wave down.
fn detached_route(cli: &Cli) -> Option<homeboy::core::notification_route::NotificationRoute> {
    homeboy::core::notification_route::from_cli_or_env(
        cli.notification_transport.as_deref(),
        cli.notification_route.as_deref(),
    )
    .ok()
    .flatten()
}

/// Replace an `--input -` stdin request with a file the detached coordinator
/// can read. Returns the materialized path when a rewrite happened.
fn materialize_stdin_plan(
    args: &mut [String],
    session_root: &Path,
) -> homeboy::core::Result<Option<PathBuf>> {
    materialize_plan_from(args, session_root, &mut std::io::stdin().lock())
}

/// The reader is a parameter so the capture can be exercised without a test
/// reaching for the harness's own stdin, which may never reach EOF.
fn materialize_plan_from(
    args: &mut [String],
    session_root: &Path,
    source: &mut impl Read,
) -> homeboy::core::Result<Option<PathBuf>> {
    let Some(index) = stdin_input_index(args) else {
        return Ok(None);
    };
    let mut plan = Vec::new();
    source.read_to_end(&mut plan).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("read stdin plan for a detached local fanout".to_string()),
        )
    })?;
    let path = session_root.join("plan.json");
    std::fs::write(&path, &plan)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    args[index] = if args[index] == "-" {
        format!("@{}", path.display())
    } else {
        format!("--input=@{}", path.display())
    };
    Ok(Some(path))
}

/// Index of the argv element holding a stdin plan request, if any.
///
/// Both spellings clap accepts are covered: a separated `--input -` names the
/// following element, an attached `--input=-` names itself.
fn stdin_input_index(args: &[String]) -> Option<usize> {
    let args = crate::command_capability::homeboy_owned_args(args);
    args.iter().enumerate().find_map(|(index, arg)| {
        if arg == "--input" && args.get(index + 1).is_some_and(|value| value == "-") {
            Some(index + 1)
        } else if arg == "--input=-" {
            Some(index)
        } else {
            None
        }
    })
}

/// Per-wave scratch directory for the launcher's captured stdio and plan.
fn detached_session_root(fanout_id: &str) -> homeboy::core::Result<PathBuf> {
    let root = homeboy::core::paths::homeboy_data()?
        .join("agent-task-detached-fanout")
        .join(homeboy::core::paths::sanitize_path_segment(fanout_id));
    std::fs::create_dir_all(&root)
        .map_err(|error| Error::internal_io(error.to_string(), Some(root.display().to_string())))?;
    Ok(root)
}

/// Spawn the coordinator in its own session.
///
/// Two environment edits, and only two. The route is set explicitly so the
/// detached wave is bound to the destination the launcher resolved, and so a
/// half-set pair inherited from the launcher is normalized rather than rejected
/// by the child. The controller-job signal tells the coordinator it now has a
/// durable owner, which is what arms its cancellation and resume behaviour;
/// without it the coordinator would run as the unowned thread pool it has
/// always been, with nothing able to stop it.
fn spawn_detached_fanout(
    args: &[String],
    fanout_id: &str,
    log_path: &Path,
    route: Option<&homeboy::core::notification_route::NotificationRoute>,
) -> homeboy::core::Result<std::process::Child> {
    let exe = std::env::current_exe().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("resolve current executable for a detached local fanout".to_string()),
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
        .envs(homeboy::core::notification_route::child_env(route))
        .env(
            agent_task_service::DETACHED_BATCH_COORDINATOR_ENV,
            fanout_id,
        )
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
    homeboy::core::process::detach_from_caller_session(&mut command);
    command.spawn().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("spawn detached local fanout".to_string()),
        )
    })
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
            Error::internal_unexpected(format!("inspect detached fanout coordinator: {error}"))
        })?
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "detached_fanout.child_start_identity",
                "detached fanout coordinator exited before its process identity could be captured",
                Some(pid.to_string()),
                None,
            )
        })
}

/// What the launcher could prove about the wave before returning.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DetachedFanoutHandoff {
    state: DetachedHandoffState,
    children: Option<usize>,
    waited_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetachedHandoffState {
    /// The coordinator published its durable batch record; `fanout status` and
    /// `fanout resume` resolve now.
    Accepted,
    /// The coordinator is alive but has not written its batch record yet. The
    /// fanout id is still the correct handle — it is pinned on the child's argv
    /// and is the controller job's idempotency key.
    Pending,
    /// The coordinator exited before publishing durable batch state. The log is
    /// the only evidence, and this is reported rather than dressed up as a
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

fn handoff_timeout() -> Duration {
    let millis = std::env::var(HANDOFF_TIMEOUT_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_HANDOFF_TIMEOUT_MS);
    Duration::from_millis(millis)
}

/// Poll durable state until the wave is addressable, its coordinator dies, or
/// the bound elapses.
///
/// Liveness is read from the child handle rather than from the pid: the detached
/// coordinator is still this process's direct child until this process exits, so
/// an early exit leaves a zombie that a pid probe reports as running — which
/// would turn a wave that died on startup into an indefinite "pending" handoff.
fn await_durable_handoff(
    fanout_id: &str,
    child: &mut std::process::Child,
    timeout: Duration,
) -> DetachedFanoutHandoff {
    let started = Instant::now();
    loop {
        if let Ok(record) = homeboy::agents::agent_tasks::batch::read_batch_record(fanout_id) {
            return DetachedFanoutHandoff {
                state: DetachedHandoffState::Accepted,
                children: Some(record.child_runs.len()),
                waited_ms: started.elapsed().as_millis() as u64,
            };
        }
        if matches!(child.try_wait(), Ok(Some(_)) | Err(_)) {
            return DetachedFanoutHandoff {
                state: DetachedHandoffState::ExitedBeforeHandoff,
                children: None,
                waited_ms: started.elapsed().as_millis() as u64,
            };
        }
        if started.elapsed() >= timeout {
            return DetachedFanoutHandoff {
                state: DetachedHandoffState::Pending,
                children: None,
                waited_ms: started.elapsed().as_millis() as u64,
            };
        }
        std::thread::sleep(HANDOFF_POLL);
    }
}

/// The launcher's only output: a bounded, machine-readable handoff naming the
/// durable handle and the evidence needed to follow or stop the wave.
fn handoff_envelope(
    fanout_id: &str,
    pid: u32,
    log_path: &Path,
    handoff: &DetachedFanoutHandoff,
    controller_job: &ControllerJobHandoff,
) -> Value {
    json!({
        "schema": HANDOFF_SCHEMA,
        "placement": "local",
        "detached": true,
        "fanout_id": fanout_id,
        "pid": pid,
        "launcher_log": log_path.display().to_string(),
        "handoff": {
            "state": handoff.state.as_str(),
            "children": handoff.children,
            "waited_ms": handoff.waited_ms,
        },
        // The daemon-owned durable job that now supervises this wave, making it
        // inspectable and cancellable through the job API rather than only
        // through its durable batch record.
        "controller_job": controller_job.projection(),
        "commands": {
            "status": format!("homeboy agent-task fanout status {fanout_id}"),
            "artifacts": format!("homeboy agent-task fanout artifacts {fanout_id}"),
            "resume": format!("homeboy agent-task fanout resume {fanout_id}"),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn cli(extra: &[&str]) -> Cli {
        let mut argv = vec!["homeboy"];
        argv.extend_from_slice(extra);
        Cli::try_parse_from(argv).expect("parse cli")
    }

    /// The defect, pinned. A locally-placed wave asking to detach must be
    /// served, not silently ignored.
    #[test]
    fn a_local_run_plan_asking_to_detach_is_detachable() {
        let cli = cli(&[
            "--placement",
            "local",
            "--detach-after-handoff",
            "agent-task",
            "fanout",
            "run-plan",
            "--input",
            "@plan.json",
        ]);

        let target = detachable_local_fanout(&cli)
            .expect("a run-plan wave is detachable")
            .expect("detachment is served");
        assert!(
            target.pin_fanout_id,
            "an unnamed wave must have its id pinned onto the child argv, or the \
             daemon would supervise a batch id the coordinator never uses"
        );
        assert!(target.fanout_id.starts_with("fanout-detached-"));
    }

    #[test]
    fn a_local_cook_batch_run_plan_asking_to_detach_is_detachable() {
        let cli = cli(&[
            "--placement",
            "local",
            "--detach-after-handoff",
            "agent-task",
            "fanout",
            "cook-batch",
            "--repo",
            "acme/widget",
            "--run-plan",
            "--fanout-id",
            "wave-7",
            "https://github.com/acme/widget/issues/1",
        ]);

        let target = detachable_local_fanout(&cli)
            .expect("a cook-batch wave is detachable")
            .expect("detachment is served");
        assert_eq!(target.fanout_id, "wave-7");
        assert!(
            !target.pin_fanout_id,
            "an operator-named wave must not have a second id appended"
        );
    }

    /// `--record-run-id` rekeys a plan after `--fanout-id` does, so it is the id
    /// the batch record actually carries. Submitting the other one would leave
    /// the daemon supervising a batch that never appears.
    #[test]
    fn record_run_id_is_the_durable_batch_id_when_both_are_given() {
        let cli = cli(&[
            "--placement",
            "local",
            "--detach-after-handoff",
            "agent-task",
            "fanout",
            "run-plan",
            "--input",
            "@plan.json",
            "--fanout-id",
            "planned",
            "--record-run-id",
            "recorded",
        ]);

        let target = detachable_local_fanout(&cli)
            .expect("detachable")
            .expect("served");
        assert_eq!(target.fanout_id, "recorded");
        assert!(!target.pin_fanout_id);
    }

    /// The heart of the fix: where the operator plausibly believes they are
    /// detaching a wave and are not, the flag must refuse rather than pretend.
    ///
    /// `resume` is in this set because it genuinely runs for hours — gates,
    /// pushes, pull requests — and is not daemon-owned, so ignoring the flag
    /// there would reproduce the exact defect being fixed.
    #[test]
    fn a_fanout_that_cannot_detach_refuses_rather_than_lying() {
        for (label, extra) in [
            (
                "cook-batch without --run-plan",
                vec![
                    "agent-task",
                    "fanout",
                    "cook-batch",
                    "--repo",
                    "acme/widget",
                    "https://github.com/acme/widget/issues/1",
                ],
            ),
            ("resume", vec!["agent-task", "fanout", "resume", "wave-7"]),
        ] {
            let mut argv = vec!["--placement", "local", "--detach-after-handoff"];
            argv.extend_from_slice(&extra);
            let error = detachable_local_fanout(&cli(&argv))
                .expect_err(&format!("{label} must refuse to detach"));
            assert!(
                error.message.contains("owns no long-running coordinator"),
                "{label}: {}",
                error.message
            );
        }
    }

    /// The other side of that line. These own no work at all, so the flag is
    /// inert rather than false. Rejecting it would break setting a global flag
    /// once in a wrapper and buy no safety, so they fall through to routing.
    #[test]
    fn a_fanout_that_owns_no_work_is_left_alone() {
        for (label, extra) in [
            (
                "plan",
                vec!["agent-task", "fanout", "plan", "--input", "@plan.json"],
            ),
            (
                "submit",
                vec!["agent-task", "fanout", "submit", "--input", "@plan.json"],
            ),
            ("status", vec!["agent-task", "fanout", "status", "wave-7"]),
            (
                "artifacts",
                vec!["agent-task", "fanout", "artifacts", "wave-7"],
            ),
        ] {
            let mut argv = vec!["--placement", "local", "--detach-after-handoff"];
            argv.extend_from_slice(&extra);
            assert_eq!(
                detachable_local_fanout(&cli(&argv))
                    .unwrap_or_else(|error| panic!("{label} must not be rejected: {error:?}")),
                None,
                "{label}"
            );
        }
    }

    /// A dry run plans without executing. Detaching it would hand the daemon a
    /// coordinator that exits immediately.
    #[test]
    fn a_dry_run_cook_batch_refuses_to_detach() {
        let cli = cli(&[
            "--placement",
            "local",
            "--detach-after-handoff",
            "agent-task",
            "fanout",
            "cook-batch",
            "--repo",
            "acme/widget",
            "--run-plan",
            "--dry-run",
            "https://github.com/acme/widget/issues/1",
        ]);

        let error = detachable_local_fanout(&cli).expect_err("a dry run must refuse to detach");
        assert!(error.message.contains("owns no long-running coordinator"));
    }

    /// Non-local placement already threads detachment into each child attempt
    /// dispatcher, and a wave that did not ask to detach is nobody's business
    /// here. Neither must be captured by this interceptor.
    #[test]
    fn only_a_local_wave_that_asked_to_detach_is_intercepted() {
        let lab = cli(&[
            "--placement",
            "lab",
            "--detach-after-handoff",
            "agent-task",
            "fanout",
            "run-plan",
            "--input",
            "@plan.json",
        ]);
        assert_eq!(
            detachable_local_fanout(&lab).expect("lab is not ours"),
            None
        );

        let attached = cli(&[
            "--placement",
            "local",
            "agent-task",
            "fanout",
            "run-plan",
            "--input",
            "@plan.json",
        ]);
        assert_eq!(
            detachable_local_fanout(&attached).expect("attached is not ours"),
            None
        );

        let not_fanout = cli(&[
            "--placement",
            "local",
            "--detach-after-handoff",
            "agent-task",
            "status",
            "cook-1",
        ]);
        assert_eq!(
            detachable_local_fanout(&not_fanout).expect("non-fanout is not ours"),
            None
        );
    }

    /// The child must coordinate rather than re-detach, and must key its batch
    /// on the id the launcher submitted to the daemon.
    #[test]
    fn the_child_argv_drops_the_detach_request_and_pins_the_wave() {
        let normalized = [
            "homeboy",
            "--placement",
            "local",
            "--detach-after-handoff",
            "agent-task",
            "fanout",
            "run-plan",
            "--input",
            "@plan.json",
        ]
        .map(str::to_string);
        let args = detached_fanout_child_args(&normalized, "wave-9", true);

        assert!(!args.iter().any(|arg| arg == "homeboy"));
        assert!(!args.iter().any(|arg| arg.contains("detach-after-handoff")));
        assert_eq!(args[args.len() - 2], "--fanout-id");
        assert_eq!(args[args.len() - 1], "wave-9");
    }

    #[test]
    fn the_child_argv_preserves_forwarded_detach_named_arguments() {
        let normalized = [
            "homeboy",
            "--detach-after-handoff",
            "agent-task",
            "fanout",
            "run-plan",
            "--input",
            "@plan.json",
            "--",
            "--detach-after-handoff",
        ]
        .map(str::to_string);
        let args = detached_fanout_child_args(&normalized, "wave-9", false);

        assert_eq!(
            args,
            [
                "agent-task",
                "fanout",
                "run-plan",
                "--input",
                "@plan.json",
                "--",
                "--detach-after-handoff",
            ]
        );
    }

    /// An operator who named the wave must not get a second, contradictory id.
    #[test]
    fn an_already_named_wave_is_not_re_pinned() {
        let normalized = [
            "homeboy",
            "--detach-after-handoff",
            "agent-task",
            "fanout",
            "run-plan",
            "--input",
            "@plan.json",
            "--fanout-id",
            "wave-7",
        ]
        .map(str::to_string);
        let args = detached_fanout_child_args(&normalized, "wave-7", false);

        assert_eq!(
            args.iter().filter(|arg| *arg == "--fanout-id").count(),
            1,
            "{args:?}"
        );
    }

    /// A detached coordinator has no stdin, so a piped plan must be captured by
    /// the launcher or the wave stalls on an empty read.
    #[test]
    fn a_piped_plan_is_materialized_for_the_detached_coordinator() {
        let session = tempfile::tempdir().expect("temp session root");
        for mut args in [
            vec!["fanout".to_string(), "--input".to_string(), "-".to_string()],
            vec!["fanout".to_string(), "--input=-".to_string()],
        ] {
            let mut source = std::io::Cursor::new(br#"{"schema":"x"}"#.to_vec());
            let path = materialize_plan_from(&mut args, session.path(), &mut source)
                .expect("materialize plan")
                .expect("a stdin plan is rewritten");

            assert_eq!(
                std::fs::read_to_string(&path).expect("plan file"),
                r#"{"schema":"x"}"#
            );
            assert!(
                args.iter()
                    .any(|arg| arg.contains(&path.display().to_string())),
                "{args:?}"
            );
            assert!(!args.iter().any(|arg| arg == "-"), "{args:?}");
        }
    }

    #[test]
    fn a_forwarded_input_flag_is_not_materialized() {
        let session = tempfile::tempdir().expect("temp session root");
        let mut args = vec![
            "fanout".to_string(),
            "--input".to_string(),
            "@plan.json".to_string(),
            "--".to_string(),
            "--input".to_string(),
            "-".to_string(),
        ];
        let mut source = std::io::Cursor::new(br#"{"schema":"x"}"#.to_vec());

        assert_eq!(
            materialize_plan_from(&mut args, session.path(), &mut source)
                .expect("forwarded input does not need materialization"),
            None
        );
        assert_eq!(args[4..], ["--input", "-"]);
    }

    /// An argv with no stdin plan must be left exactly alone.
    #[test]
    fn a_file_plan_is_left_untouched() {
        let session = tempfile::tempdir().expect("temp session root");
        let mut args = vec![
            "fanout".to_string(),
            "--input".to_string(),
            "@plan.json".to_string(),
        ];
        let before = args.clone();

        let materialized = materialize_plan_from(&mut args, session.path(), &mut &b""[..])
            .expect("no stdin plan to materialize");

        assert_eq!(materialized, None);
        assert_eq!(args, before);
    }

    /// Daemon admission is required for a detached wave. A failed admission
    /// must stop the just-spawned coordinator rather than claiming success for
    /// PID-owned work that dies with no durable owner.
    #[test]
    fn a_failed_daemon_admission_terminates_the_detached_coordinator() {
        let mut child = Command::new("sh")
            .args(["-c", "sleep 30"])
            .spawn()
            .expect("spawn detached coordinator fixture");

        let error = hand_off_to_controller(
            &mut child,
            Err(Error::validation_invalid_argument(
                "controller_job",
                "daemon admission failed",
                None,
                None,
            )),
        )
        .expect_err("a wave without daemon ownership must fail handoff");

        assert!(error.message.contains("daemon admission failed"));
        assert!(
            matches!(
                homeboy::core::process::process_identity_state(child.id(), None),
                homeboy::core::process::ProcessIdentityState::Dead
            ),
            "failed admission must leave no coordinator running"
        );
    }

    /// The envelope is the operator's only output, so every handle it names has
    /// to be one they can actually use.
    #[test]
    fn the_envelope_names_the_wave_and_how_to_follow_it() {
        let envelope = handoff_envelope(
            "wave-7",
            4242,
            Path::new("/tmp/fanout.log"),
            &DetachedFanoutHandoff {
                state: DetachedHandoffState::Accepted,
                children: Some(3),
                waited_ms: 12,
            },
            &ControllerJobHandoff::Owned {
                job_id: "job-1".to_string(),
            },
        );

        assert_eq!(envelope["schema"], HANDOFF_SCHEMA);
        assert_eq!(envelope["fanout_id"], "wave-7");
        assert_eq!(envelope["detached"], true);
        assert_eq!(envelope["handoff"]["state"], "accepted");
        assert_eq!(envelope["handoff"]["children"], 3);
        assert_eq!(envelope["controller_job"]["state"], "owned");
        assert_eq!(
            envelope["commands"]["status"],
            "homeboy agent-task fanout status wave-7"
        );
        assert_eq!(
            envelope["commands"]["resume"],
            "homeboy agent-task fanout resume wave-7"
        );
    }
}
