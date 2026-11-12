use clap::{Args, CommandFactory, Subcommand};
use serde::Serialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Instant;
use uuid::Uuid;

use homeboy::core::api_jobs::{DaemonJobInspection, JobEvent, JobStatus, JobStore};
use homeboy::core::daemon::{
    self, DaemonAttestAbsentResult, DaemonStartResult, DaemonStatus, DaemonStopResult,
};
use homeboy::core::http_api::{AnalysisJobRunOutput, AnalysisJobRunner};
use homeboy::core::Error;

use super::CmdResult;

#[derive(Args)]
pub struct DaemonArgs {
    #[command(subcommand)]
    command: DaemonCommand,
}

#[derive(Subcommand)]
enum DaemonCommand {
    /// Return the current live daemon or start one when no live daemon exists
    EnsureRunning {
        #[arg(long, default_value = daemon::DEFAULT_ADDR)]
        addr: String,
        /// Controller-generated idempotency key for a replacement operation.
        #[arg(long)]
        replacement_operation_id: Option<String>,
    },
    /// Resolve and run the right daemon recovery from the current status report
    ///
    /// Reads `homeboy daemon status` once to plan and fill every recovery
    /// argument, then re-reads it after execution to verify freshness. A dead
    /// or missing daemon is recovered by starting one: its startup recovery
    /// reconciles every dead generation's jobs in this state directory. Jobs
    /// it cannot prove gone (no recorded child identity) are reported, and
    /// only `--attest-absent` terminalizes them.
    Recover {
        /// Print the resolved plan without running it. This is the default.
        #[arg(long)]
        dry_run: bool,
        /// Stop new admissions and replace when protected jobs finish (applies unless --dry-run).
        #[arg(long, conflicts_with = "attest_absent")]
        drain: bool,
        /// Run the resolved plan.
        #[arg(long, conflicts_with_all = ["dry_run", "attest_absent"])]
        yes: bool,
        /// Attest that the workload of each named dead-generation job is gone,
        /// and terminalize exactly those jobs. Unverifiable by design, so never
        /// supplied automatically: the daemon died before recording any child
        /// identity for them. The whole set is refused if any job is not an
        /// active job of a dead generation in this state directory without a
        /// recorded child. Persisted as durable job provenance.
        #[arg(
            long = "attest-absent",
            value_name = "JOB_ID",
            num_args = 1..,
            conflicts_with = "dry_run"
        )]
        attest_absent: Vec<Uuid>,
        #[arg(long, default_value = daemon::DEFAULT_ADDR)]
        addr: String,
    },
    /// Run the local daemon in this process (what a service manager runs)
    Serve {
        /// Local bind address. Defaults to an OS-selected loopback port.
        #[arg(long, default_value = daemon::DEFAULT_ADDR)]
        addr: String,
        /// Exact startup admission identity, passed by the launching CLI.
        #[arg(long, hide = true)]
        startup_token: Option<String>,
        /// Durable store identity, passed by the launching CLI for process discovery.
        #[arg(long, hide = true)]
        state_dir: Option<PathBuf>,
    },
    /// Stop the background daemon recorded in the state file
    Stop {
        /// Require this exact live daemon lease before stopping
        #[arg(long)]
        lease_id: Option<String>,
        /// Directly SIGTERM a matching stale or unreachable daemon lease. Requires --lease-id.
        #[arg(long, requires = "lease_id")]
        force: bool,
    },
    /// Show the daemon's one next lifecycle action and its recovery plan (read-only).
    ///
    /// The same planner `daemon recover` follows, over the one daemon store.
    /// Runners are evaluated by running this in their remote daemon frame.
    Plan,
    /// Show daemon health and actionable recovery state
    Status {
        /// Include complete daemon process and recovery evidence.
        #[arg(long)]
        full: bool,
    },
    /// Fetch artifact bytes through the local daemon byte endpoint
    ArtifactGet(DaemonArtifactGetArgs),
    /// Inspect durable daemon jobs without reading the internal store directly
    Jobs(DaemonJobsArgs),
}

#[derive(Args)]
pub struct DaemonJobsArgs {
    #[command(subcommand)]
    command: DaemonJobsCommand,
}

#[derive(Subcommand)]
enum DaemonJobsCommand {
    /// List compact durable daemon-job projections
    List(DaemonJobsListArgs),
    /// Show one daemon job; --full includes retained events
    Show(DaemonJobsShowArgs),
    /// Wait for one daemon job to reach a terminal state
    Watch(DaemonJobsWatchArgs),
    /// Preview or remove exact terminal jobs of one controller driver type and version
    Prune(DaemonJobsPruneArgs),
}

#[derive(Args)]
pub struct DaemonJobsPruneArgs {
    /// Exact controller driver type (without the controller. operation prefix).
    #[arg(long)]
    job_type: String,
    #[arg(long)]
    version: u32,
    /// JSON array of job ids from the preview. Required with --apply.
    #[arg(long)]
    expected_ids: Option<String>,
    #[arg(long)]
    apply: bool,
}

#[derive(Args)]
pub struct DaemonJobsListArgs {
    #[arg(long)]
    status: Option<String>,
    #[arg(long)]
    operation: Option<String>,
    #[arg(long)]
    lease: Option<String>,
    #[arg(long)]
    run: Option<String>,
    #[arg(long, default_value_t = 20)]
    limit: usize,
}

#[derive(Args)]
pub struct DaemonJobsShowArgs {
    pub job_id: Uuid,
    /// Include retained event payloads and checkpoints; compact output omits them.
    #[arg(long)]
    full: bool,
}

#[derive(Args, Clone)]
pub struct DaemonJobsWatchArgs {
    pub job_id: Uuid,
    #[arg(long, default_value = "5m", conflicts_with = "forever")]
    timeout: String,
    #[arg(long, conflicts_with = "timeout")]
    forever: bool,
    #[arg(long, default_value = "2s")]
    interval: String,
}

#[derive(Args, Clone)]
pub struct DaemonArtifactGetArgs {
    /// Observation run id that owns the artifact
    pub run_id: String,
    /// Artifact id/path token from daemon artifact metadata
    pub artifact_id: String,
    /// Destination file path. Defaults to the artifact id basename.
    #[arg(long, short = 'o')]
    pub output: Option<PathBuf>,
    /// Daemon base URL. Defaults to the address from `homeboy daemon status`.
    #[arg(long = "daemon-url")]
    pub daemon_url: Option<String>,
}

/// Schema of `homeboy daemon plan` data, read by remote runner evaluation.
pub const DAEMON_PLAN_SCHEMA: &str = "homeboy/daemon-lifecycle-plan/v1";

#[derive(Debug, Serialize)]
pub struct DaemonPlanOutput {
    pub schema: &'static str,
    pub plan: daemon::lifecycle_plan::Plan,
    /// The recovery the same planner resolved, with its executable steps.
    pub recovery: daemon::recovery_actions::DaemonRecoveryPlan,
    /// The one daemon state file every reader and writer resolves.
    pub state_path: String,
}

#[derive(Debug, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum DaemonOutput {
    Plan(DaemonPlanOutput),
    EnsureRunning(DaemonStartResult),
    Recover(DaemonRecoverOutput),
    AttestAbsent(DaemonAttestAbsentResult),
    Serve(DaemonStartResult),
    Stop(DaemonStopResult),
    Status(Value),
    ArtifactGet(DaemonArtifactGetOutput),
    JobsList(DaemonJobsListOutput),
    JobsShow(DaemonJobsShowOutput),
    JobsWatch(DaemonJobsWatchOutput),
    JobsPrune(DaemonJobsPruneOutput),
}

#[derive(Debug, Serialize)]
pub struct DaemonJobsPruneOutput {
    pub command: &'static str,
    pub job_type: String,
    pub version: u32,
    pub applied: bool,
    pub job_ids: Vec<Uuid>,
}

#[derive(Debug, Serialize)]
pub struct DaemonJobSummary {
    pub id: Uuid,
    pub status: JobStatus,
    pub operation: String,
    pub lease_id: Option<String>,
    pub linked_durable_run_id: Option<String>,
    pub child_identity: Option<homeboy::core::api_jobs::DaemonJobChildIdentity>,
    pub age_ms: u64,
    pub terminal_disposition: Option<String>,
    pub next_actions: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct DaemonJobsListOutput {
    pub command: &'static str,
    pub jobs: Vec<DaemonJobSummary>,
}

#[derive(Debug, Serialize)]
pub struct DaemonJobsShowOutput {
    pub command: &'static str,
    #[serde(flatten)]
    pub job: DaemonJobSummary,
    pub retained_event_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub events: Option<Vec<JobEvent>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct DaemonJobsWatchOutput {
    pub command: &'static str,
    pub job_id: Uuid,
    pub status: JobStatus,
    pub terminal: bool,
    pub timed_out: bool,
    pub waited_secs: u64,
    pub poll_count: u64,
    pub job: DaemonJobSummary,
}

/// The resolved recovery, and what was done about it.
///
/// `plan` is the whole point: every argument in it was filled from the status
/// read in this same invocation, so an operator never transcribes a lease id, a
/// PID, an endpoint, or a `/proc` start-tick value between two commands.
#[derive(Debug, Serialize)]
pub struct DaemonRecoverOutput {
    pub command: &'static str,
    /// `false` for a dry run, which is the default.
    pub executed: bool,
    pub fresh: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_reason_code: Option<daemon::DaemonStaleReasonCode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lease_id: Option<String>,
    /// Durable store whose lease and protected jobs informed this plan.
    pub store_path: String,
    pub active_jobs: usize,
    pub plan: daemon::recovery_actions::DaemonRecoveryPlan,
    /// The steps actually run, in order. Empty on a dry run.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub applied_steps: Vec<String>,
    /// Why execution stopped short, when it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_on: Option<String>,
    pub next_command: String,
    /// The planner's single next lifecycle action, from the same decision as
    /// `plan`: a `blocked` or `wait` verdict never executes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle_plan: Option<daemon::lifecycle_plan::Plan>,
}

#[derive(Debug, Serialize)]
pub struct DaemonArtifactGetOutput {
    pub command: &'static str,
    pub run_id: String,
    pub artifact_id: String,
    pub daemon_url: String,
    pub content_url: String,
    pub output_path: String,
    pub content_type: Option<String>,
    pub size_bytes: Option<u64>,
    pub sha256: Option<String>,
}

pub fn run(args: DaemonArgs) -> CmdResult<DaemonOutput> {
    match args.command {
        DaemonCommand::EnsureRunning {
            addr,
            replacement_operation_id,
        } => Ok((
            DaemonOutput::EnsureRunning(daemon::ensure_running_with_replacement_operation(
                &addr,
                replacement_operation_id.as_deref(),
            )?),
            0,
        )),
        DaemonCommand::Recover { attest_absent, .. } if !attest_absent.is_empty() => Ok((
            DaemonOutput::AttestAbsent(daemon::attest_absent(&attest_absent)?),
            0,
        )),
        DaemonCommand::Recover {
            dry_run,
            yes,
            addr,
            drain,
            ..
        } => recover_with_drain(
            // Dry run is the default: an unasked-for recovery is a mutation of
            // the daemon that owns the caller's durable jobs.
            (!yes && !drain) || dry_run,
            &addr,
            drain,
        ),
        DaemonCommand::Serve {
            addr,
            startup_token,
            state_dir,
        } => {
            // The launcher supplies the token through the environment; the
            // hidden arguments retain portable process ownership evidence.
            let _ = startup_token;
            if let Some(state_dir) = state_dir {
                std::env::set_var(homeboy::core::paths::DAEMON_STATE_DIR_ENV, state_dir);
            }
            serve(&addr)
        }
        DaemonCommand::Stop { lease_id, force } => {
            let lease_bound = lease_id.is_some();
            let result = match (force, lease_id) {
                (true, Some(lease_id)) => daemon::force_stop_for_lease(&lease_id)?,
                (true, None) => unreachable!("clap requires --lease-id with --force"),
                (false, Some(lease_id)) => daemon::stop_for_lease(&lease_id)?,
                (false, None) => daemon::stop()?,
            };
            if lease_bound && !result.stopped && !result.already_absent {
                return Err(Error::validation_invalid_argument(
                    "daemon_stop",
                    "lease-bound daemon stop did not stop the exact owner or prove it already absent",
                    None,
                    None,
                ));
            }
            Ok((DaemonOutput::Stop(result), 0))
        }
        DaemonCommand::Plan => {
            let status = daemon::read_status()?;
            let recovery = daemon::recovery_actions::plan_recovery(&status);
            Ok((
                DaemonOutput::Plan(DaemonPlanOutput {
                    schema: DAEMON_PLAN_SCHEMA,
                    plan: recovery.lifecycle.clone(),
                    recovery,
                    state_path: status.state_path,
                }),
                0,
            ))
        }
        DaemonCommand::Status { full } => {
            let status = daemon::read_status()?;
            Ok((DaemonOutput::Status(status_output(status, full)), 0))
        }
        DaemonCommand::ArtifactGet(args) => artifact_get(args),
        DaemonCommand::Jobs(args) => jobs(args),
    }
}

fn jobs(args: DaemonJobsArgs) -> CmdResult<DaemonOutput> {
    match args.command {
        DaemonJobsCommand::List(args) => jobs_list(args),
        DaemonJobsCommand::Show(args) => jobs_show(args),
        DaemonJobsCommand::Watch(args) => jobs_watch(args),
        DaemonJobsCommand::Prune(args) => jobs_prune(args),
    }
}

fn jobs_prune(args: DaemonJobsPruneArgs) -> CmdResult<DaemonOutput> {
    let store = job_store()?;
    let job_ids = if args.apply {
        let expected = args.expected_ids.as_deref().ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "expected_ids",
                "--apply requires the exact JSON job id list from a fresh preview",
                None,
                None,
            )
        })?;
        let expected: Vec<Uuid> = serde_json::from_str(expected).map_err(|error| {
            homeboy::core::Error::validation_invalid_argument(
                "expected_ids",
                error.to_string(),
                None,
                None,
            )
        })?;
        store.prune_terminal_controller_jobs(&args.job_type, args.version, &expected)?
    } else {
        store.terminal_controller_job_ids(&args.job_type, args.version)?
    };
    Ok((
        DaemonOutput::JobsPrune(DaemonJobsPruneOutput {
            command: "daemon.jobs.prune",
            job_type: args.job_type,
            version: args.version,
            applied: args.apply,
            job_ids,
        }),
        0,
    ))
}

fn job_store() -> homeboy::core::Result<JobStore> {
    JobStore::open_without_reconciliation(homeboy::core::paths::observation_db()?)
}

fn jobs_list(args: DaemonJobsListArgs) -> CmdResult<DaemonOutput> {
    let status = args.status.as_deref().map(parse_job_status).transpose()?;
    let store = job_store()?;
    let mut jobs = store
        .list()
        .into_iter()
        .rev()
        .filter_map(|job| {
            if status.is_some_and(|status| job.status != status)
                || args
                    .operation
                    .as_ref()
                    .is_some_and(|operation| &job.operation != operation)
                || args
                    .lease
                    .as_ref()
                    .is_some_and(|lease| job.daemon_lease_id.as_ref() != Some(lease))
            {
                return None;
            }
            let inspection = store.inspection(job.id).ok()?;
            args.run
                .as_ref()
                .is_none_or(|run| inspection.linked_durable_run_id.as_ref() == Some(run))
                .then(|| job_summary(inspection))
        })
        .collect::<Vec<_>>();
    jobs.truncate(args.limit);
    Ok((
        DaemonOutput::JobsList(DaemonJobsListOutput {
            command: "daemon.jobs.list",
            jobs,
        }),
        0,
    ))
}

fn jobs_show(args: DaemonJobsShowArgs) -> CmdResult<DaemonOutput> {
    let store = job_store()?;
    let inspection = require_job_inspection(&store, args.job_id)?;
    let events = store.events(args.job_id)?;
    let checkpoint = args.full.then(|| inspection.checkpoint.clone()).flatten();
    Ok((
        DaemonOutput::JobsShow(DaemonJobsShowOutput {
            command: "daemon.jobs.show",
            job: job_summary(inspection),
            retained_event_count: events.len(),
            events: args.full.then_some(events),
            checkpoint,
        }),
        0,
    ))
}

fn jobs_watch(args: DaemonJobsWatchArgs) -> CmdResult<DaemonOutput> {
    use crate::commands::utils::watch::{watch_loop, WatchConfig, WatchPoller, TIMEOUT_EXIT_CODE};
    struct Poller;
    impl WatchPoller for Poller {
        type Item = DaemonJobInspection;
        fn poll(&self, id: &str) -> homeboy::core::Result<Self::Item> {
            let job_id = Uuid::parse_str(id).map_err(|_| missing_or_pruned_job(Uuid::nil()))?;
            require_job_inspection(&job_store()?, job_id)
        }
        fn is_terminal(&self, item: &Self::Item) -> bool {
            item.job.status.is_terminal()
        }
    }
    let interval = crate::commands::utils::watch::parse_duration("--interval", &args.interval)?;
    let timeout = (!args.forever)
        .then(|| crate::commands::utils::watch::parse_duration("--timeout", &args.timeout))
        .transpose()?;
    let started = Instant::now();
    let result = watch_loop(
        &Poller,
        &args.job_id.to_string(),
        &WatchConfig { interval, timeout },
        std::thread::sleep,
        || started.elapsed(),
        |job, poll| {
            eprintln!(
                "homeboy daemon jobs watch {}: poll {poll} status={}",
                job.job.id,
                job.job.status.as_str()
            )
        },
    )?;
    let timed_out = result.timed_out();
    let exit_code = if timed_out {
        TIMEOUT_EXIT_CODE
    } else {
        exit_code_for_job_status(result.item.job.status)
    };
    let status = result.item.job.status;
    Ok((
        DaemonOutput::JobsWatch(DaemonJobsWatchOutput {
            command: "daemon.jobs.watch",
            job_id: args.job_id,
            status,
            terminal: !timed_out,
            timed_out,
            waited_secs: result.waited.as_secs(),
            poll_count: result.poll_count,
            job: job_summary(result.item),
        }),
        exit_code,
    ))
}

fn parse_job_status(value: &str) -> homeboy::core::Result<JobStatus> {
    match value {
        "queued" => Ok(JobStatus::Queued),
        "running" => Ok(JobStatus::Running),
        "succeeded" => Ok(JobStatus::Succeeded),
        "failed" => Ok(JobStatus::Failed),
        "cancelled" => Ok(JobStatus::Cancelled),
        _ => Err(Error::validation_invalid_argument(
            "status",
            "expected queued, running, succeeded, failed, or cancelled",
            Some(value.to_string()),
            None,
        )),
    }
}

fn exit_code_for_job_status(status: JobStatus) -> i32 {
    if status == JobStatus::Succeeded {
        0
    } else {
        1
    }
}

fn require_job_inspection(
    store: &JobStore,
    job_id: Uuid,
) -> homeboy::core::Result<DaemonJobInspection> {
    store
        .inspection(job_id)
        .map_err(|_| missing_or_pruned_job(job_id))
}

fn missing_or_pruned_job(job_id: Uuid) -> Error {
    let mut error = Error::validation_invalid_argument("job_id", "daemon job is missing or its retained terminal record was pruned", Some(job_id.to_string()), Some(vec!["Inspect linked durable evidence with `homeboy runs show <run-id>` or `homeboy agent-task status <run-id>` when a prior job response named one.".to_string()]));
    error.details["classification"] = serde_json::json!("daemon_job_missing_or_pruned");
    error.details["retention"] = serde_json::json!(
        "terminal daemon jobs and events are retained within bounded store limits"
    );
    error
}

fn job_summary(inspection: DaemonJobInspection) -> DaemonJobSummary {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let id = inspection.job.id;
    let mut next_actions = vec![format!("homeboy daemon jobs show {id}")];
    if !inspection.job.status.is_terminal() {
        next_actions.push(format!("homeboy daemon jobs watch {id}"));
    }
    if let Some(run_id) = &inspection.linked_durable_run_id {
        next_actions.push(format!("homeboy runs show {run_id}"));
    }
    DaemonJobSummary {
        id,
        status: inspection.job.status,
        operation: inspection.job.operation,
        lease_id: inspection.job.daemon_lease_id,
        linked_durable_run_id: inspection.linked_durable_run_id,
        child_identity: inspection.child_identity,
        age_ms: now_ms.saturating_sub(inspection.job.created_at_ms),
        terminal_disposition: inspection.terminal_disposition,
        next_actions,
    }
}

const COMPACT_STATUS_BYTES: usize = 8 * 1024;
const COMPACT_STATUS_TEXT_LIMIT: usize = 256;
const COMPACT_STATUS_ACTION_LIMIT: usize = 4;
const COMPACT_STATUS_JOB_LIMIT: usize = 8;

/// Status collection remains lossless in core. The CLI's default is deliberately
/// a bounded decision surface; `--full` is the existing complete evidence view.
fn status_output(status: DaemonStatus, full: bool) -> Value {
    if full {
        let mut value = serde_json::to_value(&status).expect("daemon status serializes");
        value["admission"] = status.admission_report();
        return value;
    }

    bounded_status_projection(&status)
}

fn bounded_status_projection(status: &DaemonStatus) -> Value {
    let active_jobs = status
        .active_job_recovery_evidence
        .iter()
        .take(COMPACT_STATUS_JOB_LIMIT)
        .map(|job| {
            json!({
                "job_id": job.job_id,
                "operation": bounded_text(&job.operation),
                "status": job.status,
                "disposition": job.disposition,
                "child_pid": job.child_pid,
                "linked_durable_run_id": job.linked_durable_run_id.as_deref().map(bounded_text),
            })
        })
        .collect::<Vec<_>>();
    let next_actions = status
        .freshness
        .repair_plan
        .iter()
        .take(COMPACT_STATUS_ACTION_LIMIT)
        .map(|step| json!({ "code": step.code, "command": step.command }))
        .collect::<Vec<_>>();
    let projection = json!({
        "schema": "homeboy/daemon-status/v1",
        "admission": status.admission_report(),
        "running": status.running,
        "fresh": status.fresh,
        "reachable": status.reachable,
        "admits_work": status.admits_work(),
        "summary": bounded_text(&status.summary),
        "daemon": {
            "lease_id": status.freshness.lease_id,
            "pid": status.freshness.pid,
            "address": status.state.as_ref().map(|state| bounded_text(&state.address)),
            "active_build": status.freshness.daemon_build_identity.as_deref().map(bounded_text),
            "active_version": status.freshness.daemon_version.as_deref().map(bounded_text),
            "desired_build": bounded_text(&homeboy::core::build_identity::current().display),
            "service_manager": status.service_manager(),
        },
        "owner_lock": status.owner_lock,
        "competing_serve": status.competing_serve,
        "recovery": {
            "stale_reason_code": status.freshness.stale_reason_code,
            "restartable": status.freshness.restartable,
            "replacement_blocked": status.replacement_blocked,
            "blocker": status.replacement_blocked_reason.as_deref().map(bounded_text),
            "next_actions": next_actions,
        },
        "active_jobs": active_jobs,
        "truncation": {
            "active_jobs": { "shown": active_jobs.len(), "omitted": status.active_job_recovery_evidence.len().saturating_sub(active_jobs.len()) },
            "omitted_sections": ["state", "runtime_paths", "ownership_evidence", "termination_evidence", "complete_job_recovery_evidence"],
            "full_command": "homeboy daemon status --full",
        },
    });
    if status_projection_bytes(&projection) <= COMPACT_STATUS_BYTES {
        projection
    } else {
        json!({
            "schema": "homeboy/daemon-status/v1",
            "running": status.running,
            "fresh": status.fresh,
            "reachable": status.reachable,
            "admits_work": status.admits_work(),
            "summary": "daemon status details exceed the default response budget",
            "recovery": { "replacement_blocked": status.replacement_blocked },
            "truncation": { "full_command": "homeboy daemon status --full", "omitted": "see_full_output" },
        })
    }
}

fn status_projection_bytes(projection: &Value) -> usize {
    serde_json::to_vec(&DaemonOutput::Status(projection.clone()))
        .expect("daemon status output serializes")
        .len()
}

fn bounded_text(value: &str) -> String {
    if value.len() <= COMPACT_STATUS_TEXT_LIMIT {
        return value.to_string();
    }
    let end = value
        .char_indices()
        .find_map(|(index, _)| (index >= COMPACT_STATUS_TEXT_LIMIT).then_some(index))
        .unwrap_or(value.len());
    format!("{}...", &value[..end])
}

/// Resolve the recovery for the local daemon and either print it or run it.
///
/// The planning `read_status()` supplies every argument. A successful execution
/// requires a second authoritative read that proves freshness. Dispatch is on
/// the typed step code, never on the rendered command string (#11105). There is
/// one planner, so the reported lifecycle verdict is the same decision the
/// executor follows.
fn recover_with_drain(dry_run: bool, addr: &str, drain: bool) -> CmdResult<DaemonOutput> {
    let status = daemon::read_status()?;
    recover_direct_with_drain(status, dry_run, addr, drain)
}

fn recover_direct_with_drain(
    status: DaemonStatus,
    dry_run: bool,
    addr: &str,
    drain: bool,
) -> CmdResult<DaemonOutput> {
    let planned_status = status.clone();
    recover_from_status_with_drain(
        status,
        drain,
        dry_run,
        |plan, lease_id, job_ids| {
            let current = daemon::read_status()?;
            if !recovery_authority_matches(&planned_status, &current, lease_id.as_deref(), job_ids)
            {
                return Err(stale_recovery_plan_error());
            }
            execute_recovery_plan(
                plan,
                lease_id.as_deref(),
                addr,
                current.service_manager().is_some(),
            )
        },
        daemon::read_status,
    )
}

fn recovery_authority_matches(
    planned: &DaemonStatus,
    current: &DaemonStatus,
    lease_id: Option<&str>,
    job_ids: &[Uuid],
) -> bool {
    planned.state_identity == current.state_identity
        && current.freshness.lease_id.as_deref() == lease_id
        && current
            .active_job_recovery_evidence
            .iter()
            .map(|evidence| evidence.job_id)
            .eq(job_ids.iter().copied())
}

fn stale_recovery_plan_error() -> Error {
    let mut error = Error::validation_invalid_argument(
        "recovery_plan",
        "daemon authority or protected jobs changed after planning; no recovery steps were applied",
        Some("stale_daemon_recovery_plan".to_string()),
        Some(vec![
            "Re-run `homeboy daemon recover --dry-run` and review the current authority and protected jobs before applying.".to_string(),
        ]),
    );
    error.details["classification"] = json!("stale_daemon_recovery_plan");
    error
}

/// Resolve a recovery from one authoritative status report, execute it when
/// requested, and prove the daemon is fresh before reporting success.
#[cfg(test)]
fn recover_from_status<Execute, ReadPostcondition>(
    status: DaemonStatus,
    dry_run: bool,
    execute: Execute,
    read_postcondition: ReadPostcondition,
) -> CmdResult<DaemonOutput>
where
    Execute: FnOnce(
        &daemon::recovery_actions::DaemonRecoveryPlan,
        &Option<String>,
        &[Uuid],
    ) -> homeboy::core::Result<Vec<String>>,
    ReadPostcondition: FnOnce() -> homeboy::core::Result<DaemonStatus>,
{
    recover_from_status_with_drain(status, false, dry_run, execute, read_postcondition)
}

fn recover_from_status_with_drain<Execute, ReadPostcondition>(
    status: DaemonStatus,
    drain: bool,
    dry_run: bool,
    execute: Execute,
    read_postcondition: ReadPostcondition,
) -> CmdResult<DaemonOutput>
where
    Execute: FnOnce(
        &daemon::recovery_actions::DaemonRecoveryPlan,
        &Option<String>,
        &[Uuid],
    ) -> homeboy::core::Result<Vec<String>>,
    ReadPostcondition: FnOnce() -> homeboy::core::Result<DaemonStatus>,
{
    use daemon::recovery_actions as actions;

    let plan = actions::plan_recovery_with_drain(&status, drain);
    let lease_id = status.freshness.lease_id.clone();
    let job_ids: Vec<Uuid> = status
        .active_job_recovery_evidence
        .iter()
        .map(|evidence| evidence.job_id)
        .collect();

    let mut output = DaemonRecoverOutput {
        command: "daemon.recover",
        executed: false,
        fresh: status.fresh,
        stale_reason_code: status.freshness.stale_reason_code,
        lease_id: lease_id.clone(),
        store_path: status.state_path.clone(),
        active_jobs: status.freshness.active_jobs,
        applied_steps: Vec::new(),
        blocked_on: None,
        next_command: String::new(),
        lifecycle_plan: Some(plan.lifecycle.clone()),
        plan,
    };

    if output.plan.steps.is_empty() {
        output.blocked_on = Some(output.plan.reason.clone());
        output.next_command = "homeboy daemon status".to_string();
        return Ok((DaemonOutput::Recover(output), 1));
    }
    if !output.plan.executable {
        // A read-only diagnosis, or evidence that authorizes nothing. The plan
        // and its reason are the answer; running it would be a guess.
        output.blocked_on = Some(output.plan.reason.clone());
        output.next_command = rendered_plan(&output.plan);
        return Ok((DaemonOutput::Recover(output), 1));
    }

    // Confirmations an operator has to make are surfaced, never synthesized:
    // `--attest-absent` is its own invocation, never a step this executes.
    let missing = &output.plan.required_confirmations;
    if !missing.is_empty() {
        output.blocked_on = Some(format!(
            "this recovery requires operator attestation that no report can supply: {}",
            missing
                .iter()
                .map(|confirmation| format!("--{confirmation}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        // The resolved step argv already carries the attestation flag and its
        // job ids exactly once, so the handoff renders the plan verbatim and
        // `next_command` agrees with `plan.steps[].command` (#13618).
        output.next_command = rendered_plan(&output.plan);
        return Ok((DaemonOutput::Recover(output), 1));
    }

    if dry_run {
        // A preview is a successful read-only operation. The executable plan
        // remains available for the operator to apply; it is not a blocker.
        output.blocked_on = None;
        output.next_command = "homeboy daemon recover --yes".to_string();
        return Ok((DaemonOutput::Recover(output), 0));
    }

    output.applied_steps = match execute(&output.plan, &lease_id, &job_ids) {
        Ok(applied_steps) => applied_steps,
        Err(error) if error.details["classification"] == "stale_daemon_recovery_plan" => {
            output.blocked_on = Some(error.message);
            output.next_command = "homeboy daemon recover --dry-run".to_string();
            return Ok((DaemonOutput::Recover(output), 1));
        }
        Err(error) => return Err(error),
    };
    output.executed = true;

    let postcondition = match read_postcondition() {
        Ok(status) => status,
        Err(error) => {
            output.blocked_on = Some(format!(
                "recovery executed but authoritative freshness verification failed: {}",
                bounded_error_message(&error.message)
            ));
            output.next_command = "homeboy daemon status".to_string();
            return Ok((DaemonOutput::Recover(output), 1));
        }
    };
    let fresh = daemon_is_fresh(&postcondition);
    output.fresh = postcondition.fresh;
    output.stale_reason_code = postcondition.freshness.stale_reason_code;
    output.lease_id = postcondition.freshness.lease_id;
    output.store_path = postcondition.state_path;
    output.active_jobs = postcondition.freshness.active_jobs;
    output.next_command = if output
        .applied_steps
        .iter()
        .any(|step| step == daemon::recovery_actions::DRAIN_THEN_REPLACE)
    {
        "homeboy daemon ensure-running".to_string()
    } else {
        "homeboy daemon status".to_string()
    };
    if !fresh
        && !output
            .applied_steps
            .iter()
            .any(|step| step == daemon::recovery_actions::DRAIN_THEN_REPLACE)
    {
        output.blocked_on = Some(format!(
            "recovery executed but the authoritative status remains stale{}",
            postcondition
                .stale_reason
                .as_deref()
                .map(|reason| format!(": {reason}"))
                .unwrap_or_default()
        ));
        return Ok((DaemonOutput::Recover(output), 1));
    }

    Ok((DaemonOutput::Recover(output), 0))
}

fn execute_recovery_plan(
    plan: &daemon::recovery_actions::DaemonRecoveryPlan,
    lease_id: Option<&str>,
    addr: &str,
    service_managed: bool,
) -> homeboy::core::Result<Vec<String>> {
    execute_recovery_plan_with(
        plan,
        lease_id,
        service_managed,
        RecoveryOperations {
            stop: &mut |lease_id| daemon::stop_for_lease(lease_id).map(|_| ()),
            restart_service_managed: &mut |lease_id| {
                daemon::restart_service_managed(lease_id, DaemonStatus::admits_work).map(|_| ())
            },
            drain_then_replace: &mut |lease_id| daemon::drain_then_replace(lease_id).map(|_| ()),
            ensure_running: &mut || daemon::ensure_running(addr).map(|_| ()),
        },
    )
}

type RecoveryOperation<'a, Arg> = &'a mut dyn FnMut(Arg) -> homeboy::core::Result<()>;

struct RecoveryOperations<'a> {
    stop: RecoveryOperation<'a, &'a str>,
    restart_service_managed: RecoveryOperation<'a, &'a str>,
    drain_then_replace: RecoveryOperation<'a, &'a str>,
    ensure_running: &'a mut dyn FnMut() -> homeboy::core::Result<()>,
}

fn execute_recovery_plan_with<'a>(
    plan: &daemon::recovery_actions::DaemonRecoveryPlan,
    lease_id: Option<&'a str>,
    service_managed: bool,
    operations: RecoveryOperations<'a>,
) -> homeboy::core::Result<Vec<String>> {
    use daemon::recovery_actions as actions;

    let mut applied_steps = Vec::new();
    // Set once a service manager's replacement is proven live, so the plan's
    // `ensure-running` step observes it instead of spawning beside it.
    let mut manager_replaced = false;
    for step in &plan.steps {
        match step.code.as_str() {
            // A service manager owns its daemon's replacement: signal the
            // exact lease and wait for the manager's fresh daemon. A bare stop
            // would let `ensure-running` race a Homeboy-owned spawn into the
            // manager's restart window (#17891).
            code if code == actions::DAEMON_STOP && service_managed => {
                let lease_id = lease_id.ok_or_else(|| {
                    Error::internal_unexpected("a service-managed stop requires a lease id")
                })?;
                (operations.restart_service_managed)(lease_id)?;
                manager_replaced = true;
            }
            code if code == actions::DAEMON_ENSURE_RUNNING && manager_replaced => {}
            code if code == actions::DAEMON_STOP => {
                let lease_id = lease_id.ok_or_else(|| {
                    Error::validation_invalid_argument(
                        "lease_id",
                        "the status report named a lease-bound stop but carried no lease id",
                        None,
                        None,
                    )
                })?;
                (operations.stop)(lease_id)?;
            }
            code if code == actions::DRAIN_THEN_REPLACE => {
                (operations.drain_then_replace)(
                    lease_id
                        .ok_or_else(|| Error::internal_unexpected("drain requires a lease id"))?,
                )?;
            }
            code if code == actions::DAEMON_ENSURE_RUNNING => {
                (operations.ensure_running)()?;
            }
            code => {
                return Err(Error::validation_invalid_argument(
                    "recovery_step",
                    format!("resolved recovery step `{code}` has no dispatcher"),
                    Some(code.to_string()),
                    Some(vec![
                        "Run `homeboy daemon recover --dry-run` and follow its next command."
                            .to_string(),
                    ]),
                ));
            }
        }
        applied_steps.push(step.code.clone());
    }

    Ok(applied_steps)
}

fn daemon_is_fresh(status: &DaemonStatus) -> bool {
    status.fresh && status.freshness.fresh && status.freshness.stale_reason_code.is_none()
}

fn bounded_error_message(message: &str) -> String {
    const MAX_CHARS: usize = 500;
    if message.chars().count() <= MAX_CHARS {
        return message.to_string();
    }
    format!("{}...", message.chars().take(MAX_CHARS).collect::<String>())
}

fn rendered_plan(plan: &daemon::recovery_actions::DaemonRecoveryPlan) -> String {
    plan.steps
        .iter()
        .map(|step| step.command.as_str())
        .collect::<Vec<_>>()
        .join(" && ")
}

fn artifact_get(args: DaemonArtifactGetArgs) -> CmdResult<DaemonOutput> {
    let outcome = daemon::fetch_artifact_to_path(
        &args.run_id,
        &args.artifact_id,
        args.daemon_url,
        args.output,
    )?;

    Ok((
        DaemonOutput::ArtifactGet(DaemonArtifactGetOutput {
            command: "daemon.artifact.get",
            run_id: args.run_id,
            artifact_id: args.artifact_id,
            daemon_url: outcome.daemon_url,
            content_url: outcome.content_url,
            output_path: outcome.output_path.display().to_string(),
            content_type: outcome.content_type,
            size_bytes: Some(outcome.size_bytes),
            sha256: outcome.sha256,
        }),
        0,
    ))
}

fn serve(addr: &str) -> CmdResult<DaemonOutput> {
    register_daemon_controller_job_providers();
    let parsed = daemon::parse_bind_addr(addr)?;
    if !is_runner_hosted_daemon() {
        if let Err(error) = std::thread::Builder::new()
            .name("release-prefetch".into())
            .spawn(|| loop {
                if let Err(error) = homeboy_upgrade::update_check::prefetch_release() {
                    homeboy::core::log_status!("update", "Release prefetch failed: {}", error);
                }
                std::thread::sleep(std::time::Duration::from_secs(
                    homeboy_upgrade::update_check::PREFETCH_INTERVAL_SECS,
                ));
            })
        {
            homeboy::core::log_status!("update", "Could not start release prefetch: {}", error);
        }
    }
    let shutdown = install_drain_handlers()?;
    let state = daemon::serve_with_analysis_runner_and_shutdown(
        parsed,
        CommandAnalysisJobRunner,
        shutdown,
    )?;
    Ok((
        DaemonOutput::Serve(DaemonStartResult {
            pid: state.pid,
            address: state.address,
            state_path: state.state_path,
            lease_id: state.lease_id,
        }),
        0,
    ))
}

/// Every `daemon serve`, CLI-launched or run by a service manager, drains on
/// SIGINT (Ctrl-C) and SIGTERM (`daemon stop` or a service-manager stop): it
/// finishes owned responses and releases its lease (#15436, #17387). A drain
/// that cannot finish is cut off at the shutdown deadline, so a caught SIGTERM
/// always ends the process (#17891).
fn install_drain_handlers() -> homeboy::core::Result<std::sync::Arc<std::sync::atomic::AtomicBool>>
{
    let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    homeboy::core::process::install_shutdown_handler(shutdown.clone(), "daemon")?;
    homeboy::core::process::install_terminate_handler(shutdown.clone())?;
    daemon::arm_shutdown_deadline(shutdown.clone())?;
    Ok(shutdown)
}

fn is_runner_hosted_daemon() -> bool {
    // Lab runner service units and remote daemon startup both use this namespace.
    // They install from runner workspace slots, not the controller binaries dir.
    std::env::var_os(homeboy::core::paths::DAEMON_STATE_DIR_ENV).is_some_and(|dir| {
        std::path::Path::new(&dir)
            .components()
            .any(|part| part.as_os_str() == "runner-daemons")
    })
}

fn register_daemon_controller_job_providers() {
    // A daemon executes controller jobs after the submitting CLI has exited.
    // Register the runner continuation here, at the process boundary that owns
    // those jobs, rather than relying on a later status command to populate the
    // process-global projection provider.
    crate::runner::register_runner_continuation_provider();
    crate::runner::register_runner_upgrade();
}

#[derive(Debug, Clone, Copy)]
struct CommandAnalysisJobRunner;

impl AnalysisJobRunner for CommandAnalysisJobRunner {
    fn run_analysis_job(&self, argv: Vec<String>) -> homeboy::core::Result<AnalysisJobRunOutput> {
        let matches = crate::cli_surface::Cli::command()
            .try_get_matches_from(argv)
            .map_err(|error| {
                homeboy::core::Error::validation_invalid_argument(
                    "body",
                    error.to_string(),
                    None,
                    Some(vec![
                        "Use the documented JSON request body contract for this endpoint"
                            .to_string(),
                    ]),
                )
            })?;
        let (cli, spec) = crate::cli_surface::Cli::from_registered_arg_matches(&matches)
            .expect("validated arguments should produce a typed CLI");
        let (result, exit_code) =
            crate::commands::json_output::run(cli.command, spec, cli.placement);
        Ok(AnalysisJobRunOutput {
            exit_code,
            output: result?,
        })
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use homeboy::test_support::with_isolated_home;

    use super::*;

    use crate::cli_surface::{Cli, Commands};

    #[test]
    fn daemon_prefetch_skips_runner_state_namespace() {
        with_isolated_home(|_| {
            for (dir, expected) in [
                (
                    "/home/lab/.config/homeboy/runner-daemons/lab/controller",
                    true,
                ),
                ("/home/controller/.config/homeboy/daemon", false),
                ("/tmp/not-runner-daemons/controller", false),
            ] {
                let _state = homeboy::core::test_support::EnvVarGuard::set(
                    homeboy::core::paths::DAEMON_STATE_DIR_ENV,
                    dir,
                );
                assert_eq!(is_runner_hosted_daemon(), expected, "{dir}");
            }
            let _state = homeboy::core::test_support::EnvVarGuard::unset(
                homeboy::core::paths::DAEMON_STATE_DIR_ENV,
            );
            assert!(!is_runner_hosted_daemon());
        });
    }

    #[test]
    fn daemon_job_status_filters_and_watch_exit_semantics_are_stable() {
        assert!(Cli::try_parse_from([
            "homeboy", "daemon", "jobs", "list", "--status", "running", "--limit", "1"
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "homeboy",
            "daemon",
            "jobs",
            "show",
            "00000000-0000-0000-0000-000000000001",
            "--full"
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "homeboy",
            "daemon",
            "jobs",
            "watch",
            "00000000-0000-0000-0000-000000000001",
            "--timeout",
            "1s"
        ])
        .is_ok());
        assert_eq!(
            parse_job_status("running").expect("status"),
            JobStatus::Running
        );
        assert!(parse_job_status("pass").is_err());
        assert_eq!(exit_code_for_job_status(JobStatus::Succeeded), 0);
        for status in [
            JobStatus::Queued,
            JobStatus::Running,
            JobStatus::Failed,
            JobStatus::Cancelled,
        ] {
            assert_eq!(
                exit_code_for_job_status(status),
                1,
                "{status:?} must not report success"
            );
        }
    }

    #[test]
    fn pruned_or_unknown_daemon_job_has_a_typed_retention_diagnostic() {
        let error = missing_or_pruned_job(Uuid::nil());
        assert_eq!(
            error.details["classification"],
            "daemon_job_missing_or_pruned"
        );
        assert!(error.details["retention"].as_str().is_some());
        assert!(error
            .message
            .contains("missing or its retained terminal record was pruned"));
    }

    #[test]
    fn daemon_status_parser_exposes_the_explicit_full_surface() {
        assert!(Cli::try_parse_from(["homeboy", "daemon", "status"]).is_ok());
        assert!(Cli::try_parse_from(["homeboy", "daemon", "status", "--full"]).is_ok());
    }

    /// `--attest-absent` is the one confirmation on this surface that is not
    /// ceremony: the daemon died before recording any child identity for the
    /// named jobs, so nothing in process can observe whether their workloads
    /// still run. It names the exact jobs and is its own invocation.
    #[test]
    fn attest_absent_names_exact_jobs_and_is_not_a_dry_run() {
        let job = "00000000-0000-0000-0000-000000000001";
        let other = "00000000-0000-0000-0000-000000000002";
        assert!(Cli::try_parse_from([
            "homeboy",
            "daemon",
            "recover",
            "--attest-absent",
            job,
            other
        ])
        .is_ok());
        assert!(
            Cli::try_parse_from(["homeboy", "daemon", "recover", "--attest-absent"]).is_err(),
            "an attestation names at least one job"
        );
        assert!(Cli::try_parse_from([
            "homeboy",
            "daemon",
            "recover",
            "--attest-absent",
            "not-a-uuid"
        ])
        .is_err());
        assert!(Cli::try_parse_from([
            "homeboy",
            "daemon",
            "recover",
            "--dry-run",
            "--attest-absent",
            job
        ])
        .is_err());
    }

    /// The deleted per-case verbs do not linger as aliases (#17429).
    #[test]
    fn recover_is_the_only_recovery_verb() {
        for verb in [
            "adopt-orphan",
            "reconcile-dead-lease-orphans",
            "reconcile-leaseless-orphans",
            "recover-missing-lease-state",
        ] {
            assert!(
                Cli::try_parse_from(["homeboy", "daemon", verb]).is_err(),
                "{verb} must not parse"
            );
        }
        assert!(Cli::try_parse_from([
            "homeboy",
            "daemon",
            "recover",
            "--confirm-workload-processes-absent"
        ])
        .is_err());
    }

    /// #11105: five recovery subcommands existed and nothing chose between
    /// them. The dispatcher parses with no arguments at all — every value it
    /// needs comes from the status report it reads.
    #[test]
    fn the_recovery_dispatcher_needs_no_transcribed_arguments() {
        assert!(Cli::try_parse_from(["homeboy", "daemon", "recover"]).is_ok());
        assert!(Cli::try_parse_from(["homeboy", "daemon", "recover", "--dry-run"]).is_ok());
        assert!(Cli::try_parse_from(["homeboy", "daemon", "recover", "--yes"]).is_ok());
        assert!(
            Cli::try_parse_from(["homeboy", "daemon", "recover", "--yes", "--dry-run"]).is_err(),
            "a dry run and an execution are not the same request"
        );
    }

    #[test]
    fn deferred_staging_follow_up_commands_are_registered_cli_commands() {
        for args in [
            ["homeboy", "agent-task", "status", "run-1"].as_slice(),
            ["homeboy", "runs", "logs", "run-1"].as_slice(),
            ["homeboy", "agent-task", "cancel", "run-1"].as_slice(),
            ["homeboy", "runs", "evidence", "run-1", "--full"].as_slice(),
        ] {
            assert!(
                Cli::try_parse_from(args).is_ok(),
                "deferred staging emitted an invalid command: {args:?}"
            );
        }
    }

    /// Dry run is the default. Recovery mutates the daemon that owns the
    /// caller's durable jobs, so it may not happen because someone ran the
    /// obvious command to find out what it would do.
    #[test]
    fn recovery_is_a_dry_run_unless_execution_is_asked_for() {
        with_isolated_home(|_| {
            let cli = Cli::try_parse_from(["homeboy", "daemon", "recover"])
                .expect("the dispatcher parses bare");
            let Commands::Daemon(args) = cli.command else {
                panic!("expected daemon command");
            };

            let (output, exit_code) = run(args).expect("an isolated home still resolves a plan");

            assert_eq!(exit_code, 0);
            let DaemonOutput::Recover(output) = output else {
                panic!("expected recovery output");
            };
            assert!(!output.executed, "a bare recover must not mutate anything");
            assert_eq!(output.command, "daemon.recover");
            assert_eq!(output.blocked_on.as_deref(), None);
            assert_eq!(output.next_command, "homeboy daemon recover --yes");
            assert_recovery_json_agrees_with_command_outcome(&output, exit_code);
        });
    }

    fn assert_recovery_json_agrees_with_command_outcome(
        output: &DaemonRecoverOutput,
        exit_code: i32,
    ) {
        let data = serde_json::to_value(output).expect("recovery JSON");
        let envelope = crate::commands::utils::response::cli_response_for_json_result_for_command(
            &Ok(data),
            exit_code,
            "daemon",
            None,
        );

        assert_eq!(envelope.exit_code, exit_code);
        assert_eq!(envelope.success, exit_code == 0);
        assert_eq!(
            envelope.status,
            if exit_code == 0 {
                "succeeded"
            } else {
                "failed"
            }
        );
    }

    fn recovery_status(
        fresh: bool,
        stale_reason_code: Option<daemon::DaemonStaleReasonCode>,
        repair_plan: Vec<daemon::DaemonRepairStep>,
    ) -> DaemonStatus {
        // A version-mismatched daemon is live and reachable but stale; an
        // unreachable transport is neither. Fresh is always live.
        let live = matches!(
            stale_reason_code,
            None | Some(daemon::DaemonStaleReasonCode::VersionMismatch)
        );
        let replacement_blocked = !fresh && repair_plan.is_empty();
        let mut status = DaemonStatus {
            running: live,
            fresh,
            reachable: live,
            freshness: daemon::DaemonFreshnessReport {
                fresh,
                stale_reason_code,
                restartable: !fresh,
                lease_id: Some("lease-test".to_string()),
                pid: Some(4242),
                recovery_evidence: None,
                ownership_evidence: Some("test recovery evidence".to_string()),
                daemon_version: None,
                daemon_build_identity: None,
                runtime_paths: None,
                active_jobs: 0,
                termination_evidence: None,
                repair_plan,
            },
            stale_reason: (!fresh).then(|| "test daemon remains stale".to_string()),
            replacement_blocked,
            replacement_blocked_reason: replacement_blocked
                .then(|| "test daemon remains stale".to_string()),
            summary: String::new(),
            state: None,
            state_path: "test".to_string(),
            state_identity: "test".to_string(),
            owner_lock: daemon::DaemonOwnerLockProbe::Free,
            competing_serve: None,
            active_job_recovery_evidence: Vec::new(),
            termination_evidence: None,
        };
        status.summary = status.render_summary();
        status
    }

    /// A dead generation's durable job with no recorded child identity: the
    /// evidence that routes `plan_recovery` to the attestation step.
    fn pidless_job_evidence(
        job_id: Uuid,
    ) -> homeboy::core::api_jobs::DaemonActiveJobRecoveryEvidence {
        homeboy::core::api_jobs::DaemonActiveJobRecoveryEvidence {
            job_id,
            operation: "exec".to_string(),
            status: homeboy::core::api_jobs::JobStatus::Running,
            daemon_lease_id: Some("lease-dead".to_string()),
            created_at_ms: 0,
            updated_at_ms: 0,
            started_at_ms: None,
            terminal_evidence: None,
            child_pid: None,
            child_started_at: None,
            controller_owned: false,
            linked_durable_run_id: None,
            linked_durable_run_state: None,
            linked_durable_run_terminal_status: None,
            disposition:
                homeboy::core::api_jobs::DaemonActiveJobRecoveryDisposition::NeedsAttestation,
        }
    }

    #[test]
    fn recovery_fence_requires_same_authority_store_lease_and_protected_jobs() {
        let job_id = Uuid::new_v4();
        let planned = recovery_status(
            false,
            Some(daemon::DaemonStaleReasonCode::PidDead),
            Vec::new(),
        );
        let mut current = recovery_status(
            false,
            Some(daemon::DaemonStaleReasonCode::PidDead),
            Vec::new(),
        );
        current.active_job_recovery_evidence = vec![pidless_job_evidence(job_id)];
        assert!(!recovery_authority_matches(
            &planned,
            &current,
            Some("lease-test"),
            &[]
        ));

        current.active_job_recovery_evidence.clear();
        current.state_identity = "different-daemon-store".to_string();
        assert!(!recovery_authority_matches(
            &planned,
            &current,
            Some("lease-test"),
            &[]
        ));

        current.state_identity = planned.state_identity.clone();
        assert!(recovery_authority_matches(
            &planned,
            &current,
            Some("lease-test"),
            &[]
        ));
        assert!(!recovery_authority_matches(
            &planned,
            &current,
            Some("other-lease"),
            &[]
        ));
    }

    #[test]
    fn changed_recovery_authority_returns_a_truthful_replan_without_mutation() {
        use daemon::recovery_actions as actions;

        let initial = recovery_status(
            false,
            Some(daemon::DaemonStaleReasonCode::VersionMismatch),
            vec![daemon::DaemonRepairStep::executable(
                actions::DAEMON_ENSURE_RUNNING,
                actions::ensure_running(),
            )],
        );
        let (result, exit_code) = recover_from_status(
            initial,
            false,
            |_, _, _| Err(stale_recovery_plan_error()),
            || panic!("stale authority cannot run a postcondition read"),
        )
        .expect("changed authority returns the stale-plan verdict");

        assert_eq!(exit_code, 1);
        let DaemonOutput::Recover(output) = result else {
            panic!("expected recovery output");
        };
        assert!(!output.executed);
        assert!(output.applied_steps.is_empty());
        assert_eq!(output.next_command, "homeboy daemon recover --dry-run");
        assert!(output
            .blocked_on
            .as_deref()
            .is_some_and(|message| message.contains("changed after planning")));
        assert_recovery_json_agrees_with_command_outcome(&output, exit_code);
    }

    #[test]
    fn blocked_recovery_returns_structured_nonzero_outcome() {
        use daemon::recovery_actions as actions;

        let initial = recovery_status(
            false,
            Some(daemon::DaemonStaleReasonCode::TransportUnreachable),
            Vec::new(),
        );
        let (output, exit_code) = recover_from_status(
            initial,
            false,
            |_, _, _| panic!("a blocked recovery must not execute"),
            || panic!("a blocked recovery must not read a postcondition"),
        )
        .expect("blocked recovery returns its typed report");

        assert_eq!(exit_code, 1);
        let DaemonOutput::Recover(output) = output else {
            panic!("expected recovery output");
        };
        assert!(!output.executed);
        assert!(!output.fresh);
        assert!(output.blocked_on.is_some());
        assert_eq!(output.plan.steps[0].code, actions::DAEMON_DIAGNOSE);
        assert_eq!(output.next_command, "homeboy daemon status");

        assert_recovery_json_agrees_with_command_outcome(&output, exit_code);
    }

    #[test]
    fn fresh_noop_recovery_is_a_typed_nonzero_refusal() {
        let (output, exit_code) = recover_from_status(
            recovery_status(true, None, Vec::new()),
            false,
            |_, _, _| panic!("a fresh daemon has no recovery to execute"),
            || panic!("a fresh no-op recovery must not read a postcondition"),
        )
        .expect("fresh no-op recovery returns its typed report");

        assert_eq!(exit_code, 1);
        let DaemonOutput::Recover(output) = output else {
            panic!("expected recovery output");
        };
        assert!(!output.executed);
        assert!(output.fresh);
        assert_eq!(
            output.blocked_on.as_deref(),
            Some("daemon lease is fresh; there is nothing to recover")
        );
        assert_eq!(output.next_command, "homeboy daemon status");
        assert_recovery_json_agrees_with_command_outcome(&output, exit_code);
    }

    #[test]
    fn executed_recovery_succeeds_only_after_a_fresh_postcondition() {
        use daemon::recovery_actions as actions;

        let initial = recovery_status(
            false,
            Some(daemon::DaemonStaleReasonCode::VersionMismatch),
            vec![daemon::DaemonRepairStep::executable(
                actions::DAEMON_ENSURE_RUNNING,
                actions::ensure_running(),
            )],
        );
        let (output, exit_code) = recover_from_status(
            initial,
            false,
            |plan, _, _| {
                assert_eq!(plan.steps[0].code, actions::DAEMON_ENSURE_RUNNING);
                Ok(vec![actions::DAEMON_ENSURE_RUNNING.to_string()])
            },
            || Ok(recovery_status(true, None, Vec::new())),
        )
        .expect("successful recovery returns its report");

        assert_eq!(exit_code, 0);
        let DaemonOutput::Recover(output) = output else {
            panic!("expected recovery output");
        };
        assert!(output.executed);
        assert!(output.fresh);
        assert!(output.blocked_on.is_none());
        assert_eq!(output.applied_steps, vec![actions::DAEMON_ENSURE_RUNNING]);
        assert_recovery_json_agrees_with_command_outcome(&output, exit_code);
    }

    /// A recovery may replace a dead recorded owner and publish a new lease
    /// before the final status read. The postcondition is the new reachable
    /// lease, not equality with the obsolete planning lease.
    #[test]
    fn dead_old_pid_with_new_fresh_lease_is_successful_and_lease_bound() {
        use daemon::recovery_actions as actions;

        let old_lease = "dead-old-lease";
        let new_lease = "fresh-replacement-lease";
        let mut initial = recovery_status(
            false,
            Some(daemon::DaemonStaleReasonCode::PidDead),
            vec![
                daemon::DaemonRepairStep::executable(
                    actions::DAEMON_STOP,
                    actions::stop_for_lease(old_lease),
                ),
                daemon::DaemonRepairStep::executable(
                    actions::DAEMON_ENSURE_RUNNING,
                    actions::ensure_running(),
                ),
            ],
        );
        initial.freshness.lease_id = Some(old_lease.to_string());
        initial.freshness.pid = Some(4242); // fixture PID is dead; never signal by PID guesswork

        let mut postcondition = recovery_status(true, None, Vec::new());
        postcondition.freshness.lease_id = Some(new_lease.to_string());
        postcondition.freshness.pid = Some(4243);

        let (output, exit_code) = recover_from_status(
            initial,
            false,
            |plan, lease_id, _| {
                assert_eq!(lease_id.as_deref(), Some(old_lease));
                assert_eq!(plan.steps[0].code, actions::DAEMON_STOP);
                assert!(plan.steps[0].command.contains(old_lease));
                Ok(vec![
                    actions::DAEMON_STOP.to_string(),
                    actions::DAEMON_ENSURE_RUNNING.to_string(),
                ])
            },
            || Ok(postcondition),
        )
        .expect("fresh replacement lease satisfies recovery");

        assert_eq!(exit_code, 0);
        let DaemonOutput::Recover(output) = output else {
            panic!("expected recovery output");
        };
        assert!(output.executed);
        assert!(output.fresh);
        assert_eq!(output.lease_id.as_deref(), Some(new_lease));
        assert!(output.blocked_on.is_none());
    }

    fn stop_then_ensure_plan(lease_id: &str) -> daemon::recovery_actions::DaemonRecoveryPlan {
        use daemon::recovery_actions as actions;
        let status = recovery_status(
            false,
            Some(daemon::DaemonStaleReasonCode::VersionMismatch),
            vec![
                daemon::DaemonRepairStep::executable(
                    actions::DAEMON_STOP,
                    actions::stop_for_lease(lease_id),
                ),
                daemon::DaemonRepairStep::executable(
                    actions::DAEMON_ENSURE_RUNNING,
                    actions::ensure_running(),
                ),
            ],
        );
        let plan = daemon::recovery_actions::plan_recovery(&status);
        assert_eq!(
            plan.steps
                .iter()
                .map(|step| step.code.as_str())
                .collect::<Vec<_>>(),
            vec![actions::DAEMON_STOP, actions::DAEMON_ENSURE_RUNNING],
            "a stale live daemon plans a stop then a start"
        );
        plan
    }

    /// #17891: recovering a stale service-managed daemon hands the replacement
    /// to its manager and waits for it; it never stops the daemon and then
    /// spawns a Homeboy-owned one into the manager's restart window.
    #[test]
    fn recover_routes_a_service_managed_stop_through_the_managers_restart() {
        use daemon::recovery_actions as actions;
        let plan = stop_then_ensure_plan("lease-managed");
        let calls = std::cell::RefCell::new(Vec::new());
        let applied = execute_recovery_plan_with(
            &plan,
            Some("lease-managed"),
            true,
            RecoveryOperations {
                stop: &mut |_| panic!("a managed daemon is not stopped bare"),
                restart_service_managed: &mut |lease| {
                    calls.borrow_mut().push(format!("restart {lease}"));
                    Ok(())
                },
                drain_then_replace: &mut |_| panic!("no drain was planned"),
                ensure_running: &mut || panic!("ensure-running would spawn beside the manager"),
            },
        )
        .expect("the manager's replacement completes the plan");
        assert_eq!(calls.into_inner(), vec!["restart lease-managed"]);
        assert_eq!(
            applied,
            vec![actions::DAEMON_STOP, actions::DAEMON_ENSURE_RUNNING]
        );

        let calls = std::cell::RefCell::new(Vec::new());
        execute_recovery_plan_with(
            &plan,
            Some("lease-managed"),
            false,
            RecoveryOperations {
                stop: &mut |lease| {
                    calls.borrow_mut().push(format!("stop {lease}"));
                    Ok(())
                },
                restart_service_managed: &mut |_| panic!("an unmanaged daemon has no manager"),
                drain_then_replace: &mut |_| panic!("no drain was planned"),
                ensure_running: &mut || {
                    calls.borrow_mut().push("ensure".to_string());
                    Ok(())
                },
            },
        )
        .expect("an unmanaged plan stops then starts");
        assert_eq!(calls.into_inner(), vec!["stop lease-managed", "ensure"]);
    }

    #[test]
    fn dry_run_returns_success_with_the_executable_lease_bound_plan() {
        use daemon::recovery_actions as actions;

        let initial = recovery_status(
            false,
            Some(daemon::DaemonStaleReasonCode::VersionMismatch),
            vec![
                daemon::DaemonRepairStep::executable(
                    actions::DAEMON_STOP,
                    actions::stop_for_lease("lease-test"),
                ),
                daemon::DaemonRepairStep::executable(
                    actions::DAEMON_ENSURE_RUNNING,
                    actions::ensure_running(),
                ),
            ],
        );
        let (output, exit_code) = recover_from_status(
            initial,
            true,
            |_, _, _| panic!("dry-run must never execute"),
            || panic!("dry-run must not read an apply postcondition"),
        )
        .expect("dry-run returns its executable plan");

        assert_eq!(exit_code, 0);
        let DaemonOutput::Recover(output) = output else {
            panic!("expected recovery output");
        };
        assert!(!output.executed);
        assert!(output.plan.executable);
        assert_eq!(output.plan.steps[0].code, actions::DAEMON_STOP);
        assert!(output.plan.steps[0]
            .command
            .contains("--lease-id lease-test"));
        assert!(output.blocked_on.is_none());
    }

    #[test]
    fn executed_recovery_fails_when_authoritative_postcondition_is_stale() {
        use daemon::recovery_actions as actions;

        let initial = recovery_status(
            false,
            Some(daemon::DaemonStaleReasonCode::VersionMismatch),
            vec![daemon::DaemonRepairStep::executable(
                actions::DAEMON_ENSURE_RUNNING,
                actions::ensure_running(),
            )],
        );
        let (output, exit_code) = recover_from_status(
            initial,
            false,
            |_, _, _| Ok(vec![actions::DAEMON_ENSURE_RUNNING.to_string()]),
            || {
                Ok(recovery_status(
                    false,
                    Some(daemon::DaemonStaleReasonCode::VersionMismatch),
                    Vec::new(),
                ))
            },
        )
        .expect("failed postcondition returns its typed report");

        assert_eq!(exit_code, 1);
        let DaemonOutput::Recover(output) = output else {
            panic!("expected recovery output");
        };
        assert!(output.executed);
        assert!(!output.fresh);
        assert!(output
            .blocked_on
            .as_deref()
            .is_some_and(|message| message.contains("authoritative status remains stale")));
        assert_recovery_json_agrees_with_command_outcome(&output, exit_code);
    }

    #[test]
    fn executed_recovery_preserves_evidence_when_postcondition_read_fails() {
        use daemon::recovery_actions as actions;

        let initial = recovery_status(
            false,
            Some(daemon::DaemonStaleReasonCode::VersionMismatch),
            vec![daemon::DaemonRepairStep::executable(
                actions::DAEMON_ENSURE_RUNNING,
                actions::ensure_running(),
            )],
        );
        let (output, exit_code) = recover_from_status(
            initial,
            false,
            |_, _, _| Ok(vec![actions::DAEMON_ENSURE_RUNNING.to_string()]),
            || {
                Err(Error::internal_unexpected(
                    "postcondition daemon unavailable",
                ))
            },
        )
        .expect("verification failure returns its typed recovery report");

        assert_eq!(exit_code, 1);
        let DaemonOutput::Recover(output) = output else {
            panic!("expected recovery output");
        };
        assert!(output.executed);
        assert!(
            !output.fresh,
            "the planning status remains the only evidence"
        );
        assert_eq!(output.applied_steps, vec![actions::DAEMON_ENSURE_RUNNING]);
        assert_eq!(output.next_command, "homeboy daemon status");
        assert!(output
            .blocked_on
            .as_deref()
            .is_some_and(|message| message.contains("freshness verification failed")));

        assert_recovery_json_agrees_with_command_outcome(&output, exit_code);
    }

    #[test]
    fn postcondition_verification_blocker_is_bounded() {
        let message = "x".repeat(501);

        let bounded = bounded_error_message(&message);

        assert_eq!(bounded.chars().count(), 503);
        assert!(bounded.ends_with("..."));
    }

    /// #13618: the attestation handoff is rendered from the resolved step
    /// exactly once, with every job id filled from the report. Plain
    /// `recover` never executes it: the attestation is the operator's own
    /// `recover --attest-absent` invocation, never synthesized.
    #[test]
    fn the_workload_attestation_renders_once_in_the_handoff_and_the_step() {
        use daemon::recovery_actions as actions;

        let mut status = recovery_status(true, None, Vec::new());
        status.active_job_recovery_evidence = vec![pidless_job_evidence(Uuid::nil())];

        let (output, exit_code) = recover_from_status(
            status,
            false,
            |_, _, _| panic!("an unattested recovery must not execute"),
            || panic!("a blocked recovery must not read a postcondition"),
        )
        .expect("the blocked recovery returns its typed report");

        assert_eq!(exit_code, 1);
        let DaemonOutput::Recover(output) = output else {
            panic!("expected recovery output");
        };
        assert!(!output.executed, "the handoff mutates nothing");
        assert_eq!(output.plan.steps.len(), 1);
        let step = &output.plan.steps[0];
        assert_eq!(step.code, actions::DAEMON_ATTEST_ABSENT);

        let flag = format!("--{}", actions::ATTEST_ABSENT);
        assert_eq!(
            step.command,
            format!("homeboy daemon recover {flag} {}", Uuid::nil()),
        );
        assert_eq!(
            output.next_command, step.command,
            "next_command is the resolved step, verbatim"
        );
        assert!(output
            .blocked_on
            .as_deref()
            .is_some_and(|reason| reason.contains(&flag)));
        assert_recovery_json_agrees_with_command_outcome(&output, exit_code);
    }

    /// Every value the dispatcher fills comes from the report. A restart plan
    /// needs no attestation, so it must not be gated behind one.
    #[test]
    fn a_plan_needing_no_attestation_is_not_gated_on_one() {
        use daemon::recovery_actions as actions;

        let plan = actions::DaemonRecoveryPlan {
            steps: vec![
                daemon::DaemonRepairStep::executable(actions::DAEMON_STOP, actions::stop()),
                daemon::DaemonRepairStep::executable(
                    actions::DAEMON_ENSURE_RUNNING,
                    actions::ensure_running(),
                ),
            ],
            reason: "version mismatch".to_string(),
            required_confirmations: Vec::new(),
            executable: true,
            lifecycle: daemon::lifecycle_plan::Plan::Converged,
        };

        assert!(plan.required_confirmations.is_empty());
        assert_eq!(
            rendered_plan(&plan),
            "homeboy daemon stop && homeboy daemon ensure-running"
        );
    }

    #[test]
    fn stop_accepts_an_exact_lease_selector() {
        assert!(
            Cli::try_parse_from(["homeboy", "daemon", "stop", "--lease-id", "lease-live",]).is_ok()
        );
    }

    #[test]
    fn force_stop_requires_lease_id() {
        assert!(Cli::try_parse_from(["homeboy", "daemon", "stop", "--force"]).is_err());
        assert!(Cli::try_parse_from([
            "homeboy",
            "daemon",
            "stop",
            "--force",
            "--lease-id",
            "lease-live",
        ])
        .is_ok());
    }

    #[test]
    fn artifact_get_downloads_daemon_byte_alias() {
        with_isolated_home(|home| {
            let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
            let addr = listener.local_addr().expect("addr");
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut request = [0; 1024];
                let bytes = stream.read(&mut request).expect("request");
                let request = String::from_utf8_lossy(&request[..bytes]);
                assert!(request.starts_with(
                    "GET /runs/run-1/artifacts/report%2Fsummary.json/content HTTP/1.1"
                ));
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\nX-Homeboy-Artifact-Sha256: abc123\r\nConnection: close\r\n\r\n{\"ok\":true}",
                    )
                    .expect("response");
            });
            let output = home.path().join("summary.json");

            let (result, exit_code) = artifact_get(DaemonArtifactGetArgs {
                run_id: "run-1".to_string(),
                artifact_id: "report/summary.json".to_string(),
                output: Some(output.clone()),
                daemon_url: Some(format!("http://{addr}")),
            })
            .expect("artifact get");

            server.join().expect("server");
            assert_eq!(exit_code, 0);
            let DaemonOutput::ArtifactGet(result) = result else {
                panic!("expected artifact get output");
            };
            assert_eq!(result.command, "daemon.artifact.get");
            assert_eq!(result.content_type.as_deref(), Some("application/json"));
            assert_eq!(result.size_bytes, Some(11));
            assert_eq!(result.sha256.as_deref(), Some("abc123"));
            assert_eq!(std::fs::read(&output).expect("output"), br#"{"ok":true}"#);
        });
    }
}
