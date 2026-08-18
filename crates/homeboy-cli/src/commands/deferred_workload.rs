use clap::{Args, Subcommand};
use homeboy::deferred_workload;
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use super::CmdResult;

const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// How long a single `reconcile` pass may spend terminating orphans.
///
/// A host that accumulated hundreds of orphans must not turn one recovery
/// command into an unbounded stall. Whatever does not fit is reported as
/// remaining so the operator can run the command again.
const RECONCILE_BUDGET: Duration = Duration::from_secs(60);
/// SIGTERM grace before escalation for each orphan.
const RECONCILE_GRACE: Duration = Duration::from_millis(500);
const CAPABILITY_MISMATCH_EXIT_CODE: i32 = 75;
const CAPABILITY_MISMATCH_ERROR: &str = "deferred workload runner capability mismatch";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RunnerCapabilityInventory {
    pub runner_id: String,
    pub runtime_ids: BTreeSet<String>,
    pub capabilities: BTreeSet<String>,
}

#[derive(Args)]
pub struct DeferredWorkloadArgs {
    #[command(subcommand)]
    command: DeferredWorkloadCommand,
}

#[derive(Subcommand)]
enum DeferredWorkloadCommand {
    /// Run the singleton controller-owned deferred-workload worker
    Worker {
        #[arg(long, value_name = "TOKEN")]
        startup_token: String,
    },
    /// Inspect deferred workloads and the controller worker
    Status,
    /// Terminate worker processes that no live durable ownership backs
    Reconcile {
        /// Report what would be terminated without signaling anything
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Serialize)]
struct DeferredWorkloadStatusOutput {
    schema: &'static str,
    worker: Option<deferred_workload::DeferredWorkloadWorkerStatus>,
    records: Vec<serde_json::Value>,
    diagnostics: serde_json::Value,
}

pub fn run(args: DeferredWorkloadArgs) -> CmdResult<serde_json::Value> {
    match args.command {
        DeferredWorkloadCommand::Worker { startup_token } => {
            run_worker(&startup_token)?;
            Ok((
                serde_json::json!({ "schema": "homeboy/deferred-workload-worker-result/v1", "status": "stopped" }),
                0,
            ))
        }
        DeferredWorkloadCommand::Status => {
            // One resolution for the whole command: the worker status file and
            // the record store are two files under the same root, and reading
            // them from two independently resolved roots is how a status report
            // ends up describing two different Homeboy installations.
            let config_root = homeboy::core::paths::homeboy()?;
            let output = DeferredWorkloadStatusOutput {
                schema: "homeboy/deferred-workload-status/v1",
                worker: deferred_workload::worker_status_in_roots(&config_root)?,
                records: deferred_workload::records_in_roots(&config_root)?
                    .iter()
                    .map(redacted_record)
                    .collect(),
                diagnostics: serde_json::json!({
                    "worker_command": "homeboy deferred-workload worker",
                    "status_command": "homeboy deferred-workload status",
                    "ci_alternative": "Run the portable command in CI or configure a ready Homeboy runner."
                }),
            };
            Ok((
                serde_json::to_value(output).expect("deferred workload status serializes"),
                0,
            ))
        }
        DeferredWorkloadCommand::Reconcile { dry_run } => Ok((
            serde_json::to_value(reconcile_workers(dry_run)?)
                .expect("deferred workload reconciliation serializes"),
            0,
        )),
    }
}

#[derive(Serialize)]
struct DeferredWorkloadReconcileOutput {
    schema: &'static str,
    dry_run: bool,
    scanned: usize,
    retained: Vec<deferred_workload::DeferredWorkloadWorkerProcess>,
    orphaned: Vec<DeferredWorkloadOrphan>,
    /// Orphans the time budget did not reach. Re-run to continue.
    remaining: Vec<DeferredWorkloadOrphan>,
    diagnostics: serde_json::Value,
}

#[derive(Serialize)]
struct DeferredWorkloadOrphan {
    #[serde(flatten)]
    process: deferred_workload::DeferredWorkloadWorkerProcess,
    reason: String,
    /// Deferred workloads returned to the queue by terminating this worker.
    released_workload_ids: Vec<String>,
    terminated: bool,
    error: Option<String>,
}

/// Terminate every worker process that no live durable ownership backs.
///
/// The command line only nominates candidates. Whether one may keep running is
/// decided by the durable record store and the startup token the process can
/// prove from its own environment, so a foreign process that happens to share
/// the command name is never signaled on that basis (#12081).
fn reconcile_workers(dry_run: bool) -> homeboy::core::Result<DeferredWorkloadReconcileOutput> {
    reconcile_workers_in_roots(&homeboy::core::paths::homeboy()?, dry_run)
}

fn reconcile_workers_in_roots(
    config_root: &Path,
    dry_run: bool,
) -> homeboy::core::Result<DeferredWorkloadReconcileOutput> {
    let deadline = Instant::now() + RECONCILE_BUDGET;
    let status = deferred_workload::worker_status_in_roots(config_root)?;
    let owner_is_live = status
        .as_ref()
        .is_some_and(|status| deferred_workload::worker_is_live_in_roots(config_root, status));
    let pending_work = deferred_workload::has_pending_work_in_roots(config_root)?;
    let processes = deferred_workload::worker_processes()?;
    let scanned = processes.len();
    let mut retained = Vec::new();
    let mut orphaned = Vec::new();
    let mut remaining = Vec::new();

    for process in processes {
        // Reconciling from inside a worker would have the pass kill itself.
        if process.pid == std::process::id() {
            continue;
        }
        let deferred_workload::DeferredWorkloadWorkerDisposition::Orphaned { reason } =
            deferred_workload::classify_worker_process(
                &process,
                status.as_ref(),
                owner_is_live,
                pending_work,
            )
        else {
            retained.push(process);
            continue;
        };
        if dry_run || Instant::now() >= deadline {
            let orphan = DeferredWorkloadOrphan {
                process,
                reason,
                released_workload_ids: Vec::new(),
                terminated: false,
                error: None,
            };
            if dry_run {
                orphaned.push(orphan);
            } else {
                remaining.push(orphan);
            }
            continue;
        }
        // Release before signaling: a killed worker cannot hand its claim back,
        // and the record would otherwise sit out the full lease.
        let released_workload_ids = match process.startup_token.as_deref() {
            Some(token) => {
                deferred_workload::release_claims_for_owner_in_roots(config_root, token)?
            }
            None => Vec::new(),
        };
        let (terminated, error) = match homeboy::core::process::terminate_process_tree_with_grace(
            process.pid,
            RECONCILE_GRACE,
        ) {
            Ok(termination) => (
                termination.surviving_pids.is_empty(),
                (!termination.surviving_pids.is_empty()).then(|| {
                    format!(
                        "surviving pids after {}: {:?}",
                        termination.signal, termination.surviving_pids
                    )
                }),
            ),
            Err(error) => (false, Some(error.message)),
        };
        if terminated {
            let _ = deferred_workload::append_worker_log_in_roots(
                config_root,
                format!(
                    "reconcile terminated worker pid={} reason={reason}",
                    process.pid
                ),
            );
        }
        orphaned.push(DeferredWorkloadOrphan {
            process,
            reason,
            released_workload_ids,
            terminated,
            error,
        });
    }

    Ok(DeferredWorkloadReconcileOutput {
        schema: "homeboy/deferred-workload-reconcile/v1",
        dry_run,
        scanned,
        retained,
        orphaned,
        remaining,
        diagnostics: serde_json::json!({
            "owner_is_live": owner_is_live,
            "pending_work": pending_work,
            "status_command": "homeboy deferred-workload status",
            "rerun_command": "homeboy deferred-workload reconcile",
        }),
    })
}

pub fn ensure_worker() -> homeboy::core::Result<()> {
    ensure_worker_in_roots(&homeboy::core::paths::homeboy()?)
}

/// Spawn the singleton worker against an already-resolved config root.
///
/// The start lock, the liveness probe, the worker's working directory, the
/// readiness poll, and the pending-record check are five reads of the same
/// Homeboy installation. Resolving them independently let a five-second poll
/// re-resolve the root up to 250 times, and left the spawned worker free to
/// disagree with the probe that was waiting on it.
fn ensure_worker_in_roots(config_root: &Path) -> homeboy::core::Result<()> {
    let _start_lock = deferred_workload::acquire_worker_start_lock_in_roots(config_root)?;
    ensure_worker_with(
        config_root,
        |status: &deferred_workload::DeferredWorkloadWorkerStatus| {
            deferred_workload::worker_is_live_in_roots(config_root, status)
        },
        || {
            let executable = std::env::current_exe().map_err(|error| {
                homeboy::core::Error::internal_io(
                    error.to_string(),
                    Some("resolve deferred worker executable".to_string()),
                )
            })?;
            let mut command = Command::new(executable);
            let startup_token = uuid::Uuid::new_v4().to_string();
            command.args([
                "deferred-workload",
                "worker",
                "--startup-token",
                &startup_token,
            ]);
            // The ownership probe reads `/proc/<pid>/environ`, which the kernel
            // populates at execve. The marker must therefore be set on the child
            // command, not by the child once it is running (#12081).
            command.env(deferred_workload::WORKER_OWNER_ENV, &startup_token);
            // A singleton that outlives this command must not hold its working
            // directory open. Inheriting it left workers pinned to worktrees that
            // were finalized and deleted underneath them.
            command.current_dir(deferred_workload::worker_root_in_roots(config_root)?);
            // A detached worker must not keep an invoking client's capture pipes
            // open after the foreground command exits.
            command
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                unsafe {
                    command.pre_exec(|| {
                        if libc::setsid() == -1 {
                            return Err(std::io::Error::last_os_error());
                        }
                        Ok(())
                    });
                }
            }
            command.spawn().map_err(|error| {
                homeboy::core::Error::internal_io(
                    error.to_string(),
                    Some("spawn deferred workload worker".to_string()),
                )
            })?;
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while std::time::Instant::now() < deadline {
                if deferred_workload::worker_status_in_roots(config_root)?
                    .as_ref()
                    .is_some_and(|status| {
                        deferred_workload::worker_is_live_in_roots(config_root, status)
                    })
                {
                    return Ok(());
                }
                thread::sleep(Duration::from_millis(20));
            }
            if deferred_workload::records_in_roots(config_root)?
                .iter()
                .any(|record| {
                    matches!(
                        record.state,
                        deferred_workload::DeferredWorkloadState::Deferred
                            | deferred_workload::DeferredWorkloadState::Claimed
                    )
                })
            {
                return Err(homeboy::core::Error::internal_unexpected(
                    "deferred workload worker did not publish live ownership within 5 seconds",
                ));
            }
            Ok(())
        },
    )
}

pub fn restart_worker_if_pending() -> homeboy::core::Result<()> {
    restart_worker_if_pending_in_roots(&homeboy::core::paths::homeboy()?)
}

fn restart_worker_if_pending_in_roots(config_root: &Path) -> homeboy::core::Result<()> {
    restart_worker_if_pending_with_in_roots(
        config_root,
        |status: &deferred_workload::DeferredWorkloadWorkerStatus| {
            deferred_workload::worker_is_live_in_roots(config_root, status)
        },
        || ensure_worker_in_roots(config_root),
    )
}

fn ensure_worker_with(
    config_root: &Path,
    is_live: impl Fn(&deferred_workload::DeferredWorkloadWorkerStatus) -> bool,
    spawn: impl FnOnce() -> homeboy::core::Result<()>,
) -> homeboy::core::Result<()> {
    if deferred_workload::worker_status_in_roots(config_root)?
        .as_ref()
        .is_some_and(is_live)
    {
        return Ok(());
    }
    spawn()
}

fn restart_worker_if_pending_with(
    is_live: impl Fn(&deferred_workload::DeferredWorkloadWorkerStatus) -> bool,
    spawn: impl FnOnce() -> homeboy::core::Result<()>,
) -> homeboy::core::Result<()> {
    restart_worker_if_pending_with_in_roots(&homeboy::core::paths::homeboy()?, is_live, spawn)
}

fn restart_worker_if_pending_with_in_roots(
    config_root: &Path,
    is_live: impl Fn(&deferred_workload::DeferredWorkloadWorkerStatus) -> bool,
    spawn: impl FnOnce() -> homeboy::core::Result<()>,
) -> homeboy::core::Result<()> {
    if deferred_workload::records_in_roots(config_root)?
        .iter()
        .any(|record| {
            matches!(
                record.state,
                deferred_workload::DeferredWorkloadState::Deferred
                    | deferred_workload::DeferredWorkloadState::Claimed
            )
        })
    {
        ensure_worker_with(config_root, is_live, spawn)?;
    }
    Ok(())
}

fn run_worker(startup_token: &str) -> homeboy::core::Result<()> {
    if std::env::var(deferred_workload::WORKER_OWNER_ENV).as_deref() != Ok(startup_token) {
        return reexec_with_owner_marker(startup_token);
    }
    // The worker process resolves its installation once and then never again.
    // Everything below — the singleton lock, the status file, the log, the
    // claim/terminalize protocol, and the dispatch heartbeat — is the same
    // durable store, and an unbounded loop that re-resolved it per iteration
    // could drift onto a different one mid-lease.
    let config_root = homeboy::core::paths::homeboy()?;
    let Some(lock) = deferred_workload::try_acquire_worker_lock_in_roots(&config_root)? else {
        return Ok(());
    };
    let owner = startup_token.to_string();
    deferred_workload::write_worker_status_in_roots(
        &config_root,
        &owner,
        "starting",
        "probing runner readiness",
    )?;
    deferred_workload::append_worker_log_in_roots(
        &config_root,
        format!("worker started owner={owner}"),
    )?;
    run_worker_while_holding_lock(
        &config_root,
        &lock,
        &owner,
        || {
            let readiness = crate::runner::runners::lab_runner_readiness()?;
            let Some(runner_id) = (readiness.state
                == crate::runner::runners::LabRunnerReadinessState::ConnectedReady)
                .then_some(readiness.selected_runner_id)
                .flatten()
            else {
                return Ok(None);
            };
            crate::runner::runners::runner_capability_inventory(&runner_id).map(|inventory| {
                Some(RunnerCapabilityInventory {
                    runner_id,
                    runtime_ids: inventory.runtime_ids,
                    capabilities: inventory.capabilities,
                })
            })
        },
        // A bare `dispatch_record` reference would keep heartbeating against
        // its own ambient resolution while the loop around it used the injected
        // root — the split-home shape this campaign exists to remove.
        |record: &deferred_workload::DeferredWorkload, runner_id: &str, owner: &str| {
            dispatch_record(config_root.as_path(), record, runner_id, owner)
        },
        deferred_workload_now_ms,
        thread::sleep,
    )
}

/// Replace this process image with one whose execve environment carries the
/// ownership marker.
///
/// `std::env::set_var` cannot do this: `/proc/<pid>/environ` exposes the block
/// the kernel copied at exec, so a worker that set the variable on itself could
/// never prove ownership, `worker_is_live` always answered false, and every
/// mutating command spawned yet another worker. `ensure_worker` sets the marker
/// on the child, so this path exists only for a worker started by hand — and
/// `exec` keeps the pid, which the spawner is already waiting on.
fn reexec_with_owner_marker(startup_token: &str) -> homeboy::core::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        let executable = std::env::current_exe().map_err(|error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some("resolve deferred worker executable".to_string()),
            )
        })?;
        let error = Command::new(executable)
            .args(std::env::args_os().skip(1))
            .env(deferred_workload::WORKER_OWNER_ENV, startup_token)
            .exec();
        Err(homeboy::core::Error::internal_io(
            error.to_string(),
            Some("re-exec deferred workload worker with its ownership marker".to_string()),
        ))
    }

    #[cfg(not(unix))]
    {
        let _ = startup_token;
        Err(homeboy::core::Error::validation_invalid_argument(
            "platform",
            "the deferred workload worker requires a Unix process image replacement to publish its ownership marker",
            None,
            None,
        ))
    }
}

fn run_worker_while_holding_lock(
    config_root: &Path,
    _lock: &deferred_workload::DeferredWorkloadWorkerLock,
    owner: &str,
    readiness: impl FnMut() -> homeboy::core::Result<Option<RunnerCapabilityInventory>>,
    dispatch: impl FnMut(
        &deferred_workload::DeferredWorkload,
        &str,
        &str,
    ) -> homeboy::core::Result<bool>,
    now: impl Fn() -> u64,
    sleep: impl FnMut(Duration),
) -> homeboy::core::Result<()> {
    run_worker_with_in_roots(config_root, owner, readiness, dispatch, now, sleep)
}

/// Ambient entry point for the worker loop, retained for callers that own no
/// resolved root (the hermetic tests). Production runs
/// [`run_worker_with_in_roots`] from the single resolution `run_worker` makes.
pub(crate) fn run_worker_with(
    owner: &str,
    readiness: impl FnMut() -> homeboy::core::Result<Option<RunnerCapabilityInventory>>,
    dispatch: impl FnMut(
        &deferred_workload::DeferredWorkload,
        &str,
        &str,
    ) -> homeboy::core::Result<bool>,
    now: impl Fn() -> u64,
    sleep: impl FnMut(Duration),
) -> homeboy::core::Result<()> {
    run_worker_with_in_roots(
        &homeboy::core::paths::homeboy()?,
        owner,
        readiness,
        dispatch,
        now,
        sleep,
    )
}

pub(crate) fn run_worker_with_in_roots(
    config_root: &Path,
    owner: &str,
    mut readiness: impl FnMut() -> homeboy::core::Result<Option<RunnerCapabilityInventory>>,
    mut dispatch: impl FnMut(
        &deferred_workload::DeferredWorkload,
        &str,
        &str,
    ) -> homeboy::core::Result<bool>,
    now: impl Fn() -> u64,
    mut sleep: impl FnMut(Duration),
) -> homeboy::core::Result<()> {
    loop {
        let pending = deferred_workload::records_in_roots(config_root)?
            .into_iter()
            .any(|record| {
                matches!(
                    record.state,
                    deferred_workload::DeferredWorkloadState::Deferred
                        | deferred_workload::DeferredWorkloadState::Claimed
                )
            });
        if !pending {
            deferred_workload::write_worker_status_in_roots(
                config_root,
                owner,
                "idle",
                "no deferred workloads",
            )?;
            return Ok(());
        }
        let inventory = match readiness() {
            Ok(Some(inventory)) => inventory,
            Ok(None) => {
                deferred_workload::write_worker_status_in_roots(
                    config_root,
                    owner,
                    "waiting_for_runner",
                    "no ready runner",
                )?;
                sleep(POLL_INTERVAL);
                continue;
            }
            Err(error) => {
                deferred_workload::write_worker_status_in_roots(
                    config_root,
                    owner,
                    "waiting_for_runner",
                    error.message,
                )?;
                sleep(POLL_INTERVAL);
                continue;
            }
        };
        let Some(record) = deferred_workload::claim_next_matching_at_in_roots(
            config_root,
            &inventory.runner_id,
            owner,
            now(),
            |candidate| runner_satisfies_requirements(candidate, &inventory),
        )?
        else {
            deferred_workload::write_worker_status_in_roots(
                config_root,
                owner,
                "waiting_for_runner",
                "no claimable workload for selected runner",
            )?;
            sleep(POLL_INTERVAL);
            continue;
        };
        // A worktree can be finalized and deleted while its workload waits.
        // Replaying it from wherever the worker happens to stand would sync the
        // wrong source tree, so the record fails here instead (#12081).
        if let Some(missing) = missing_source_directory(&record) {
            deferred_workload::terminalize_in_roots(config_root, &record.id, false)?;
            deferred_workload::append_worker_log_in_roots(
                config_root,
                format!(
                    "failed {} source worktree {missing} no longer exists",
                    record.id
                ),
            )?;
            continue;
        }
        let runner_id = &inventory.runner_id;
        deferred_workload::write_worker_status_in_roots(
            config_root,
            owner,
            "dispatching",
            format!("{} via {runner_id}", record.id),
        )?;
        deferred_workload::append_worker_log_in_roots(
            config_root,
            format!("claimed {} via {runner_id}", record.id),
        )?;
        let success = match dispatch(&record, runner_id, owner) {
            Ok(success) => success,
            Err(error) if error.message == CAPABILITY_MISMATCH_ERROR => {
                deferred_workload::defer_claim_in_roots(config_root, &record.id, owner)?;
                deferred_workload::append_worker_log_in_roots(
                    config_root,
                    format!(
                        "deferred {} after runner capability preflight mismatch",
                        record.id
                    ),
                )?;
                continue;
            }
            Err(error) => return Err(error),
        };
        deferred_workload::terminalize_in_roots(config_root, &record.id, success)?;
        deferred_workload::append_worker_log_in_roots(
            config_root,
            format!("terminalized {} success={success}", record.id),
        )?;
    }
}

/// The recorded source worktree, when it no longer exists.
fn missing_source_directory(record: &deferred_workload::DeferredWorkload) -> Option<&str> {
    let source = record.source_directory.as_deref()?;
    (!Path::new(source).is_dir()).then_some(source)
}

fn runner_satisfies_requirements(
    record: &deferred_workload::DeferredWorkload,
    inventory: &RunnerCapabilityInventory,
) -> bool {
    record
        .test_requirements
        .is_satisfied_by(&inventory.runtime_ids, &inventory.capabilities)
}

fn deferred_workload_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Dispatch a claimed record and heartbeat its lease under the caller's root.
///
/// The heartbeat writes the same store the claim came from, so the root is
/// injected rather than re-resolved: a dispatch that renewed a lease in one
/// installation while the loop had claimed it in another would silently let the
/// claim expire under the worker.
fn dispatch_record(
    config_root: &Path,
    record: &deferred_workload::DeferredWorkload,
    runner_id: &str,
    owner: &str,
) -> homeboy::core::Result<bool> {
    let executable = std::env::current_exe().map_err(|error| {
        homeboy::core::Error::internal_io(
            error.to_string(),
            Some("resolve deferred workload executable".to_string()),
        )
    })?;
    let args = child_args(record, runner_id);
    let route = record_notification_route(record);
    let mut command = Command::new(executable);
    command
        .args(&args[1..])
        .env("HOMEBOY_DEFERRED_WORKLOAD_REPLAY", "1")
        .envs(homeboy::core::notification_route::child_env(route.as_ref()));
    // The replay resolves its source worktree from its working directory when
    // argv does not name one. The worker runs from a stable root, so the
    // directory the workload was deferred from has to be restored explicitly.
    if let Some(source_directory) = record.source_directory.as_deref() {
        command.current_dir(source_directory);
    }
    let mut child = command.spawn().map_err(|error| {
        homeboy::core::Error::internal_io(
            error.to_string(),
            Some("dispatch deferred workload".to_string()),
        )
    })?;
    loop {
        if let Some(status) = child.try_wait().map_err(|error| {
            homeboy::core::Error::internal_io(
                error.to_string(),
                Some("wait for deferred workload".to_string()),
            )
        })? {
            if status.code() == Some(CAPABILITY_MISMATCH_EXIT_CODE) {
                return Err(homeboy::core::Error::validation_invalid_argument(
                    "runner_capabilities",
                    CAPABILITY_MISMATCH_ERROR,
                    Some(runner_id.to_string()),
                    None,
                ));
            }
            return Ok(status.success());
        }
        if !deferred_workload::heartbeat_in_roots(config_root, &record.id, owner)? {
            return Ok(false);
        }
        thread::sleep(Duration::from_secs(1));
    }
}

/// The destination a deferred workload's notifications belong to.
///
/// Read from the record's own persisted argv, never from the worker's ambient
/// route. The deferred-workload worker is a long-lived singleton that claims
/// records deferred by unrelated callers; propagating the worker's own route
/// would deliver every deferred workload's notifications to whoever happened to
/// start the worker, which is a mis-attribution rather than a fix.
///
/// A record deferred by a caller who supplied the route through the environment
/// rather than argv still cannot be recovered here, because the route is not
/// persisted on the record. That gap belongs to the deferral producer.
fn record_notification_route(
    record: &deferred_workload::DeferredWorkload,
) -> Option<homeboy::core::notification_route::NotificationRoute> {
    let transport = argv_flag_value(&record.args, "--notification-transport")?;
    let route = argv_flag_value(&record.args, "--notification-route")?;
    // Observability must never fail a dispatch, so a malformed persisted pair
    // is dropped rather than propagated or raised.
    homeboy::core::notification_route::NotificationRoute::new(transport, route).ok()
}

/// The value of `flag` in an argv, covering both spellings clap accepts:
/// a separated `--flag value` and an attached `--flag=value`.
fn argv_flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let attached = format!("{flag}=");
    args.iter().enumerate().find_map(|(index, arg)| {
        if arg == flag {
            args.get(index + 1).map(String::as_str)
        } else {
            arg.strip_prefix(attached.as_str())
        }
    })
}

fn child_args(record: &deferred_workload::DeferredWorkload, runner_id: &str) -> Vec<String> {
    let mut args = record.args.clone();
    let mut overrides = vec!["--runner".to_string(), runner_id.to_string()];
    for (name, value) in &record.job_overrides.env {
        overrides.extend(["--runner-env".to_string(), format!("{name}={value}")]);
    }
    for name in &record.job_overrides.secret_env_names {
        overrides.extend(["--runner-secret-env".to_string(), name.clone()]);
    }
    args.splice(1..1, overrides);
    args
}

pub(crate) fn redacted_record(record: &deferred_workload::DeferredWorkload) -> serde_json::Value {
    let mut value = serde_json::to_value(record).expect("deferred workload serializes");
    let secret_names = record
        .job_overrides
        .secret_env_names
        .iter()
        .collect::<std::collections::BTreeSet<_>>();
    if let Some(env) = value
        .pointer_mut("/job_overrides/env")
        .and_then(serde_json::Value::as_object_mut)
    {
        for name in secret_names {
            if env.contains_key(name) {
                env.insert(
                    name.to_string(),
                    serde_json::Value::String("[REDACTED]".to_string()),
                );
            }
        }
    }
    if let Some(args) = value
        .get_mut("args")
        .and_then(serde_json::Value::as_array_mut)
    {
        redact_settings_args(args);
    }
    value
}

fn redact_settings_args(args: &mut [serde_json::Value]) {
    let mut redact_next = false;
    for arg in args {
        let Some(value) = arg.as_str() else { continue };
        if redact_next {
            *arg = serde_json::Value::String("[REDACTED]".to_string());
            redact_next = false;
        } else if matches!(
            value,
            "--setting" | "--setting-json" | "--settings-json-file" | "--settings-profile"
        ) {
            redact_next = true;
        } else if value.starts_with("--setting=")
            || value.starts_with("--setting-json=")
            || value.starts_with("--settings-json-file=")
            || value.starts_with("--settings-profile=")
        {
            *arg = serde_json::Value::String(
                value.split_once('=').expect("checked").0.to_string() + "=[REDACTED]",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    fn inventory(id: &str) -> RunnerCapabilityInventory {
        RunnerCapabilityInventory {
            runner_id: id.to_string(),
            runtime_ids: ["homeboy".to_string()].into(),
            capabilities: ["test-runner".to_string()].into(),
        }
    }

    fn input() -> deferred_workload::DeferredWorkloadInput {
        deferred_workload::DeferredWorkloadInput {
            command_label: "review test".to_string(),
            args: vec![
                "homeboy".to_string(),
                "review".to_string(),
                "test".to_string(),
            ],
            placement: "auto".to_string(),
            resource_requirement: "eligible_lab_runner".to_string(),
            portability: "portable_lab_route".to_string(),
            reason: "no ready runner".to_string(),
            ci_alternative: "run in CI".to_string(),
            resolved_contract: serde_json::json!({}),
            resolved_resources: serde_json::json!({}),
            test_requirements: deferred_workload::DeferredWorkloadRequirements {
                required_runtimes: ["homeboy".to_string()].into(),
                required_capabilities: ["test-runner".to_string()].into(),
            },
            source_directory: None,
            job_overrides: Default::default(),
        }
    }

    fn input_with_args(extra: &[&str]) -> deferred_workload::DeferredWorkloadInput {
        let mut input = input();
        input
            .args
            .extend(extra.iter().map(|value| value.to_string()));
        input
    }

    /// A deferred workload is replayed by a separate worker process, so its
    /// route reaches the dispatched child only through the child environment.
    #[test]
    fn a_deferred_workload_child_carries_the_recorded_route() {
        crate::test_support::with_isolated_home(|_| {
            let record = deferred_workload::defer(input_with_args(&[
                "--notification-transport",
                "extension.run-completion",
                "--notification-route",
                "opaque-destination",
            ]))
            .expect("defer routed workload");

            let route = record_notification_route(&record).expect("recorded route resolves");

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
        });
    }

    /// Half a pair is a hard validation error in the child, so a workload
    /// deferred without a route must leave both variables untouched.
    #[test]
    fn a_deferred_workload_without_a_route_sets_neither_variable() {
        crate::test_support::with_isolated_home(|_| {
            let record = deferred_workload::defer(input()).expect("defer unrouted workload");

            assert!(record_notification_route(&record).is_none());
            assert!(homeboy::core::notification_route::child_env(None).is_empty());
        });
    }

    /// An incomplete recorded pair cannot be completed, and a half-set child
    /// environment would make the dispatched child fail to start.
    #[test]
    fn a_partially_recorded_route_propagates_nothing() {
        crate::test_support::with_isolated_home(|_| {
            let record = deferred_workload::defer(input_with_args(&[
                "--notification-transport",
                "extension.run-completion",
            ]))
            .expect("defer half-routed workload");

            assert!(record_notification_route(&record).is_none());
        });
    }

    #[test]
    fn argv_flag_value_reads_both_spellings_clap_accepts() {
        let separated = vec!["--notification-route".to_string(), "opaque".to_string()];
        let attached = vec!["--notification-route=opaque".to_string()];

        assert_eq!(
            argv_flag_value(&separated, "--notification-route"),
            Some("opaque")
        );
        assert_eq!(
            argv_flag_value(&attached, "--notification-route"),
            Some("opaque")
        );
        assert_eq!(argv_flag_value(&[], "--notification-route"), None);
    }

    #[test]
    fn warm_deferred_workload_waits_then_dispatches_once_when_runner_appears() {
        crate::test_support::with_isolated_home(|_| {
            let deferred = deferred_workload::defer(input()).expect("defer warm workload");
            let ready = Rc::new(Cell::new(false));
            let ready_after_wait = ready.clone();
            let dispatched = Rc::new(RefCell::new(Vec::new()));
            let dispatched_by_worker = dispatched.clone();

            run_worker_with(
                "worker-a",
                || Ok(ready.get().then(|| inventory("compatible-runner"))),
                move |record, runner_id, _| {
                    dispatched_by_worker
                        .borrow_mut()
                        .push((record.id.clone(), runner_id.to_string()));
                    Ok(true)
                },
                || 10,
                |_| ready_after_wait.set(true),
            )
            .expect("worker completes deferred workload");

            assert_eq!(
                dispatched.borrow().as_slice(),
                &[(deferred.id, "compatible-runner".to_string())]
            );
            assert_eq!(
                deferred_workload::records().expect("records")[0].state,
                deferred_workload::DeferredWorkloadState::Dispatched
            );
        });
    }

    #[test]
    fn warm_defer_dispatches_public_db_service_values_and_secret_reference_without_plaintext() {
        crate::test_support::with_isolated_home(|_| {
            let mut input = input();
            input.job_overrides = homeboy::core::lab_offload::LabJobOverrides {
                env: [
                    ("DB_SERVICE_HOST".to_string(), "db.fixture".to_string()),
                    ("DB_SERVICE_PORT".to_string(), "3306".to_string()),
                ]
                .into(),
                secret_env_names: vec!["DB_SERVICE_PASSWORD".to_string()],
                workspace_root: None,
            };
            let deferred = deferred_workload::defer(input).expect("defer warm workload");
            let ready = Rc::new(Cell::new(false));
            let ready_after_wait = ready.clone();

            run_worker_with(
                "worker-a",
                || Ok(ready.get().then(|| inventory("compatible-runner"))),
                move |record, runner_id, _| {
                    assert_eq!(runner_id, "compatible-runner");
                    assert_eq!(record.job_overrides.env["DB_SERVICE_HOST"], "db.fixture");
                    assert_eq!(record.job_overrides.env["DB_SERVICE_PORT"], "3306");
                    assert!(!record.job_overrides.env.contains_key("DB_SERVICE_PASSWORD"));
                    assert_eq!(
                        record.job_overrides.secret_env_names,
                        ["DB_SERVICE_PASSWORD"]
                    );
                    let durable_json = serde_json::to_string(record).expect("deferred JSON");
                    assert!(!durable_json.contains("fixture-password"));
                    Ok(true)
                },
                || 10,
                |_| ready_after_wait.set(true),
            )
            .expect("worker dispatches after compatible runner appears");

            assert_eq!(
                deferred_workload::records().expect("records")[0].state,
                deferred_workload::DeferredWorkloadState::Dispatched
            );
            assert_eq!(
                deferred.id,
                deferred_workload::records().expect("records")[0].id
            );
        });
    }

    #[test]
    fn incompatible_runner_waits_without_dispatching_until_compatible_runner_arrives() {
        crate::test_support::with_isolated_home(|_| {
            deferred_workload::defer(input()).expect("defer workload");
            let compatible = Rc::new(Cell::new(false));
            let compatible_after_wait = compatible.clone();
            let readiness_calls = Rc::new(Cell::new(0));
            let calls = readiness_calls.clone();
            let dispatches = Rc::new(Cell::new(0));
            let dispatch_count = dispatches.clone();

            run_worker_with(
                "worker-a",
                move || {
                    calls.set(calls.get() + 1);
                    if compatible.get() {
                        Ok(Some(inventory("compatible-runner")))
                    } else {
                        Ok(Some(RunnerCapabilityInventory {
                            runner_id: "incompatible-runner".to_string(),
                            runtime_ids: ["other-runtime".to_string()].into(),
                            capabilities: BTreeSet::new(),
                        }))
                    }
                },
                move |_, _, _| {
                    dispatch_count.set(dispatch_count.get() + 1);
                    Ok(true)
                },
                || 10,
                |_| compatible_after_wait.set(true),
            )
            .expect("worker dispatches after compatibility changes");

            assert!(
                readiness_calls.get() >= 2,
                "incompatible readiness must wait"
            );
            assert_eq!(dispatches.get(), 1);
        });
    }

    #[test]
    fn incompatible_work_waits_without_busy_spinning() {
        crate::test_support::with_isolated_home(|_| {
            let deferred = deferred_workload::defer(input()).expect("defer workload");
            let sleeps = Cell::new(0);
            let dispatches = Cell::new(0);

            run_worker_with(
                "worker-a",
                || {
                    Ok(Some(RunnerCapabilityInventory {
                        runner_id: "incompatible-runner".to_string(),
                        runtime_ids: ["other-runtime".to_string()].into(),
                        capabilities: BTreeSet::new(),
                    }))
                },
                |_, _, _| {
                    dispatches.set(dispatches.get() + 1);
                    Ok(true)
                },
                || 10,
                |_| {
                    sleeps.set(sleeps.get() + 1);
                    deferred_workload::terminalize(&deferred.id, false)
                        .expect("remove incompatible fixture after wait");
                },
            )
            .expect("worker waits and exits when no work remains");

            assert_eq!(sleeps.get(), 1);
            assert_eq!(dispatches.get(), 0);
        });
    }

    #[test]
    fn child_argv_reconstructs_persisted_secret_references() {
        let mut record = deferred_workload::DeferredWorkload {
            id: "deferred-fixture".to_string(),
            fingerprint: "fixture".to_string(),
            command_label: "review test".to_string(),
            args: vec![
                "homeboy".to_string(),
                "review".to_string(),
                "test".to_string(),
            ],
            placement: "auto".to_string(),
            resource_requirement: "eligible_lab_runner".to_string(),
            portability: "portable_lab_route".to_string(),
            reason: "fixture".to_string(),
            ci_alternative: "CI".to_string(),
            resolved_contract: serde_json::json!({}),
            resolved_resources: serde_json::json!({}),
            test_requirements: deferred_workload::DeferredWorkloadRequirements {
                required_runtimes: ["homeboy".to_string()].into(),
                required_capabilities: BTreeSet::new(),
            },
            source_directory: None,
            job_overrides: Default::default(),
            state: deferred_workload::DeferredWorkloadState::Deferred,
            created_at_ms: 0,
            updated_at_ms: 0,
            runner_id: None,
            claim_owner: None,
            claim_expires_at_ms: None,
        };
        record.job_overrides.secret_env_names = vec!["DB_SERVICE_PASSWORD".to_string()];
        let args = child_args(&record, "compatible-runner");

        assert!(args
            .windows(2)
            .any(|pair| { pair == ["--runner-secret-env", "DB_SERVICE_PASSWORD"] }));
    }

    #[test]
    fn restarted_worker_reclaims_an_expired_claim() {
        crate::test_support::with_isolated_home(|_| {
            let deferred = deferred_workload::defer(input()).expect("defer workload");
            let first = deferred_workload::claim_next_at("first-runner", "dead-worker", 1)
                .expect("claim workload")
                .expect("deferred workload");
            let dispatched = Rc::new(Cell::new(0));
            let dispatch_count = dispatched.clone();

            run_worker_with(
                "restarted-worker",
                || Ok(Some(inventory("recovery-runner"))),
                move |record, runner_id, owner| {
                    assert_eq!(record.id, deferred.id);
                    assert_eq!(runner_id, "recovery-runner");
                    assert_eq!(owner, "restarted-worker");
                    dispatch_count.set(dispatch_count.get() + 1);
                    Ok(true)
                },
                || first.claim_expires_at_ms.expect("claim expiry"),
                |_| panic!("recovered worker should not wait"),
            )
            .expect("restarted worker reclaims expired work");

            assert_eq!(dispatched.get(), 1);
            assert_eq!(
                deferred_workload::records().expect("records")[0].state,
                deferred_workload::DeferredWorkloadState::Dispatched
            );
        });
    }

    #[test]
    fn pending_workload_restarts_a_dead_worker_but_not_a_live_one() {
        crate::test_support::with_isolated_home(|_| {
            deferred_workload::defer(input()).expect("defer workload");
            let spawned = Cell::new(0);
            restart_worker_if_pending_with(
                |_| false,
                || {
                    spawned.set(spawned.get() + 1);
                    Ok(())
                },
            )
            .expect("restart dead worker");
            assert_eq!(spawned.get(), 1);
        });
    }

    /// A worktree can be finalized while its workload waits for a runner.
    /// Replaying it from the worker's own directory would sync the wrong source
    /// tree, so the record fails instead.
    #[test]
    fn a_workload_whose_source_worktree_was_deleted_fails_instead_of_dispatching() {
        crate::test_support::with_isolated_home(|_| {
            let mut input = input();
            input.source_directory = Some("/nonexistent/workspace/repo@finalized".to_string());
            let deferred = deferred_workload::defer(input).expect("defer workload");

            run_worker_with(
                "worker-a",
                || Ok(Some(inventory("compatible-runner"))),
                |_, _, _| panic!("a deleted source worktree must not be dispatched"),
                || 10,
                |_| panic!("a failed record leaves no pending work to wait for"),
            )
            .expect("worker fails the record and exits");

            let records = deferred_workload::records().expect("records");
            assert_eq!(records[0].id, deferred.id);
            assert_eq!(
                records[0].state,
                deferred_workload::DeferredWorkloadState::Failed
            );
        });
    }

    /// A surviving worktree still dispatches, and the record carries the
    /// directory the replay must run from.
    #[test]
    fn a_workload_dispatches_from_its_recorded_source_worktree() {
        crate::test_support::with_isolated_home(|home| {
            let source = home.path().join("workspace/repo@live");
            std::fs::create_dir_all(&source).expect("create source worktree");
            let mut input = input();
            input.source_directory = Some(source.display().to_string());
            deferred_workload::defer(input).expect("defer workload");
            let dispatched = Rc::new(RefCell::new(Vec::new()));
            let dispatched_by_worker = dispatched.clone();

            run_worker_with(
                "worker-a",
                || Ok(Some(inventory("compatible-runner"))),
                move |record, _, _| {
                    dispatched_by_worker
                        .borrow_mut()
                        .push(record.source_directory.clone());
                    Ok(true)
                },
                || 10,
                |_| panic!("a dispatchable workload should not wait"),
            )
            .expect("worker dispatches the surviving worktree");

            assert_eq!(
                dispatched.borrow().as_slice(),
                &[Some(source.display().to_string())]
            );
        });
    }

    #[test]
    fn a_record_without_a_source_directory_is_never_treated_as_missing() {
        let mut record = deferred_workload::DeferredWorkload {
            id: "deferred-fixture".to_string(),
            fingerprint: "fixture".to_string(),
            command_label: "review test".to_string(),
            args: vec!["homeboy".to_string()],
            placement: "auto".to_string(),
            resource_requirement: "eligible_lab_runner".to_string(),
            portability: "portable_lab_route".to_string(),
            reason: "fixture".to_string(),
            ci_alternative: "CI".to_string(),
            resolved_contract: serde_json::json!({}),
            resolved_resources: serde_json::json!({}),
            test_requirements: Default::default(),
            source_directory: None,
            job_overrides: Default::default(),
            state: deferred_workload::DeferredWorkloadState::Deferred,
            created_at_ms: 0,
            updated_at_ms: 0,
            runner_id: None,
            claim_owner: None,
            claim_expires_at_ms: None,
        };

        assert_eq!(missing_source_directory(&record), None);

        record.source_directory = Some("/nonexistent/workspace/repo@gone".to_string());
        assert_eq!(
            missing_source_directory(&record),
            Some("/nonexistent/workspace/repo@gone")
        );
    }

    /// A dry run signals nothing, and with no live durable owner in this
    /// isolated home no candidate on the host may be retained either.
    #[test]
    fn a_dry_run_reconciliation_signals_nothing_and_retains_nothing() {
        crate::test_support::with_isolated_home(|_| {
            let outcome = reconcile_workers(true).expect("reconcile");

            assert_eq!(outcome.schema, "homeboy/deferred-workload-reconcile/v1");
            assert!(outcome.dry_run);
            assert!(
                outcome.retained.is_empty(),
                "no process can be retained without a live durable owner"
            );
            assert!(outcome.remaining.is_empty(), "a dry run consumes no budget");
            assert!(
                outcome
                    .orphaned
                    .iter()
                    .all(|orphan| !orphan.terminated && orphan.released_workload_ids.is_empty()),
                "a dry run must not signal or mutate durable state"
            );
        });
    }
}
