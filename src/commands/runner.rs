use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;
use serde_json::Value;

use homeboy::core::api_jobs::{Job, JobEvent, JobStatus};
use homeboy::core::redaction::RedactionPolicy;
use homeboy::core::runners::{
    self as runner, runner_job_log_snapshot, ReverseRunnerConnectOptions,
    ReverseRunnerWorkerOptions, ReverseRunnerWorkerOutput, Runner, RunnerConnectReport,
    RunnerDisconnectReport, RunnerExecOutput, RunnerKind, RunnerStatusReport,
};
use homeboy::core::server::{RunnerPolicy, RunnerSecretEnvRef, RunnerSettings};
use homeboy::core::{EntityCrudOutput, MergeOutput};

use super::{CmdResult, DynamicSetArgs};

pub mod doctor;
mod policy;
mod workspace;

#[derive(Debug, Default, Serialize)]
pub struct RunnerExtra {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection: Option<RunnerConnectionOutput>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<RunnerStatusReport>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum RunnerConnectionOutput {
    Connect(RunnerConnectReport),
    Status(RunnerStatusReport),
    Disconnect(RunnerDisconnectReport),
}

pub type RunnerOutput = EntityCrudOutput<Runner, RunnerExtra>;

const REDACTED_ENV_VALUE: &str = "[redacted]";

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum RunnerCommandOutput {
    Registry(RunnerOutput),
    Doctor(doctor::RunnerDoctorOutput),
    Execution(RunnerExecOutput),
    Env(RunnerEnvOutput),
    Job(RunnerJobOutput),
    Worker(ReverseRunnerWorkerOutput),
    Workspace(workspace::RunnerWorkspaceOutput),
}

#[derive(Debug, Serialize)]
pub struct RunnerJobOutput {
    pub command: &'static str,
    pub runner_id: String,
    pub job_id: String,
    pub follow: bool,
    pub job: Job,
    pub events: Vec<JobEvent>,
}

#[derive(Debug, Serialize)]
pub struct RunnerEnvOutput {
    pub command: String,
    pub runner_id: String,
    pub source: String,
    pub values_redacted: bool,
    pub env: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub secret_env: BTreeMap<String, RunnerSecretEnvReferenceOutput>,
    pub diagnostics: RunnerEnvDiagnostics,
}

#[derive(Debug, Serialize)]
pub struct RunnerSecretEnvReferenceOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    pub values_redacted: bool,
}

#[derive(Debug, Serialize)]
pub struct RunnerEnvDiagnostics {
    pub server_shell_env: String,
    pub runner_job_env: String,
}

#[derive(Args)]
pub struct RunnerArgs {
    #[command(subcommand)]
    command: RunnerCommand,
}

#[derive(Subcommand)]
enum RunnerCommand {
    /// Register a local or SSH execution runner
    Add {
        /// JSON input spec for add/update (supports single or bulk)
        #[arg(long)]
        json: Option<String>,

        /// Skip items that already exist (JSON mode only)
        #[arg(long)]
        skip_existing: bool,

        /// Runner ID
        id: Option<String>,

        /// Runner kind. Defaults to ssh when --server is set, otherwise local.
        #[arg(long, value_enum)]
        kind: Option<RunnerKindArg>,

        /// Existing server ID for SSH runners
        #[arg(long)]
        server: Option<String>,

        /// Root directory where this runner checks out or owns workspaces
        #[arg(long)]
        workspace_root: Option<String>,

        /// Homeboy binary path on the runner machine
        #[arg(long)]
        homeboy_path: Option<String>,

        /// Prefer daemon-backed execution for future runner commands
        #[arg(long)]
        daemon: bool,

        /// Maximum concurrent workflows this runner should accept
        #[arg(long)]
        concurrency_limit: Option<usize>,

        /// Artifact retention/copying policy label for future execution commands
        #[arg(long)]
        artifact_policy: Option<String>,
    },
    /// Enable runner capability on an existing SSH server
    Enable {
        /// Server ID to make runner-capable
        server_id: String,

        /// Root directory where this server checks out or owns workspaces
        #[arg(long)]
        workspace_root: Option<String>,

        /// Homeboy binary path on the server machine
        #[arg(long)]
        homeboy_path: Option<String>,

        /// Prefer daemon-backed execution for future runner commands
        #[arg(long)]
        daemon: bool,

        /// Maximum concurrent workflows this server should accept
        #[arg(long)]
        concurrency_limit: Option<usize>,

        /// Artifact retention/copying policy label for future execution commands
        #[arg(long)]
        artifact_policy: Option<String>,
    },
    /// List all configured runners
    List,
    /// Display runner configuration
    Show {
        /// Runner ID
        id: String,
    },
    /// Modify runner settings
    Set {
        #[command(flatten)]
        args: DynamicSetArgs,
    },
    /// Trust a runner for constrained controller-side project execution
    Trust {
        /// Runner ID
        runner_id: String,

        /// Project ID allowed to use this runner. Repeat for multiple projects.
        #[arg(long = "project")]
        projects: Vec<String>,

        /// Allowed command family, for example test, bench, lint, audit, trace, cargo, or runner.exec. Repeat or pass comma-separated values.
        #[arg(long = "command", value_delimiter = ',')]
        commands: Vec<String>,

        /// Explicitly allow or deny raw runner exec shell commands
        #[arg(long)]
        allow_raw_exec: Option<bool>,

        /// Workspace root allowed by policy. Repeat for multiple roots.
        #[arg(long = "workspace-root")]
        workspace_roots: Vec<String>,

        /// Artifact behavior for runner jobs, for example copy, metadata, none, or deny
        #[arg(long)]
        artifact_policy: Option<String>,

        /// Expected peer/controller server ID. Repeat for multiple peers.
        #[arg(long = "peer")]
        peers: Vec<String>,

        /// Expected peer host key/fingerprint. Repeat for multiple fingerprints.
        #[arg(long = "fingerprint")]
        fingerprints: Vec<String>,
    },
    /// Pair a runner with a trusted peer/controller policy from the runner side
    Pair {
        /// Runner ID
        runner_id: String,

        /// Peer/controller server ID accepted by this runner. Repeat for multiple peers.
        #[arg(long = "peer")]
        peers: Vec<String>,

        /// Peer/controller host key/fingerprint. Repeat for multiple fingerprints.
        #[arg(long = "fingerprint")]
        fingerprints: Vec<String>,

        /// Project ID accepted from the peer. Repeat for multiple projects.
        #[arg(long = "accept-project")]
        projects: Vec<String>,

        /// Workspace root this runner accepts jobs under. Repeat for multiple roots.
        #[arg(long = "workspace-root")]
        workspace_roots: Vec<String>,

        /// Explicitly allow or deny raw runner exec shell commands
        #[arg(long)]
        allow_raw_exec: Option<bool>,
    },
    /// Remove a runner configuration
    Remove {
        /// Runner ID
        id: String,
    },
    /// Diagnose a local or configured SSH runner without mutating it
    Doctor {
        /// Runner ID. Use `local`, `localhost`, or `self` for this machine;
        /// other values resolve through `homeboy runner` configuration.
        runner_id: String,

        /// Component/workspace path to use as the extension parity probe cwd.
        #[arg(long)]
        path: Option<String>,

        /// Required extension ID to resolve on the runner. Repeat for multiple extensions.
        #[arg(long = "extension")]
        required_extensions: Vec<String>,

        /// Required command to resolve on the runner PATH. Repeat for provider/job-specific tools.
        #[arg(long = "require-tool")]
        required_tools: Vec<String>,

        /// Readiness scope. `lab-offload` adds Lab-specific binary, daemon, and provider readiness checks.
        #[arg(long, value_enum, default_value_t = RunnerDoctorScopeArg::General)]
        scope: RunnerDoctorScopeArg,

        /// Safely repair issues in the selected scope, such as reconnecting a stale Lab daemon.
        #[arg(long)]
        repair: bool,
    },
    /// Connect to a runner by starting a loopback-only remote daemon and SSH tunnel
    Connect {
        /// Runner ID for direct SSH connect, or controller/broker ID when --reverse is set
        id: String,

        /// Record a runner-initiated reverse tunnel session substrate
        #[arg(long)]
        reverse: bool,

        /// Runner ID initiating the reverse connection
        #[arg(long = "reverse-runner")]
        reverse_runner: Option<String>,

        /// Broker/controller URL observed by the reverse runner
        #[arg(long)]
        broker_url: Option<String>,
    },
    /// Show persisted runner tunnel status
    Status {
        /// Runner ID. Omit to show all runner session states.
        id: Option<String>,
    },
    /// Close a runner tunnel and remove its persisted session state
    Disconnect {
        /// Runner ID
        id: String,
    },
    /// Execute a command on a configured runner
    Exec {
        /// Runner ID
        id: String,

        /// Remote/current working directory. SSH runners require this to be
        /// inside the runner workspace root unless the runner has a default
        /// workspace_root.
        #[arg(long)]
        cwd: Option<String>,

        /// Project ID used for runner trust policy checks
        #[arg(long)]
        project: Option<String>,

        /// Allow diagnostic-only SSH command execution when no daemon session is connected
        #[arg(long)]
        ssh: bool,

        /// Capture the file delta produced by the remote command as a patch artifact
        #[arg(long)]
        capture_patch: bool,

        /// Runner-side path that must exist before executing the command. Repeat for multiple paths.
        #[arg(long = "require-path")]
        require_paths: Vec<String>,

        /// Command and arguments to execute on the runner
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Show the effective environment injected into runner jobs
    Env {
        /// Runner ID
        id: String,

        /// Print actual values instead of redacting them
        #[arg(long)]
        show_values: bool,
    },
    /// Inspect or follow a runner daemon job stream
    Job {
        #[command(subcommand)]
        command: RunnerJobCommand,
    },
    /// Claim and execute one brokered reverse-runner job from this machine
    Work {
        /// Runner ID on this machine
        runner_id: String,

        /// Controller/broker daemon URL
        #[arg(long)]
        broker_url: String,

        /// Optional project filter for claimed jobs
        #[arg(long)]
        project: Option<String>,

        /// Claim lease duration in milliseconds
        #[arg(long, default_value_t = 30_000)]
        lease_ms: u64,

        /// Keep claiming jobs until SIGINT/SIGTERM instead of exiting after one claim
        #[arg(long)]
        r#loop: bool,

        /// Initial sleep after an empty claim in loop mode
        #[arg(long, default_value_t = 1_000)]
        idle_backoff_ms: u64,

        /// Maximum sleep after repeated empty claims in loop mode
        #[arg(long, default_value_t = 30_000)]
        max_idle_backoff_ms: u64,

        /// Sleep after transient broker failures in loop mode
        #[arg(long, default_value_t = 5_000)]
        broker_failure_backoff_ms: u64,

        /// Consecutive broker failures allowed before the worker exits non-zero
        #[arg(long, default_value_t = 5)]
        broker_retry_limit: u32,
    },
    /// Materialize local workspaces on a configured runner
    Workspace {
        #[command(subcommand)]
        command: workspace::RunnerWorkspaceCommand,
    },
}

#[derive(Subcommand)]
enum RunnerJobCommand {
    /// Show or follow durable runner daemon job events
    Logs {
        /// Runner ID with an active daemon connection
        runner_id: String,

        /// Runner daemon job ID from runner exec/Lab output or error details
        job_id: String,

        /// Poll until the remote job reaches a terminal state, printing new events to stderr
        #[arg(long)]
        follow: bool,

        /// Poll interval in milliseconds when --follow is set
        #[arg(long = "poll-ms", default_value_t = 1000)]
        poll_ms: u64,
    },
    /// Cancel a queued or running durable runner daemon job
    Cancel {
        /// Runner ID with an active daemon connection
        runner_id: String,

        /// Runner daemon job ID from runner exec/Lab output or error details
        job_id: String,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum RunnerKindArg {
    Local,
    Ssh,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum RunnerDoctorScopeArg {
    General,
    LabOffload,
}

impl From<RunnerDoctorScopeArg> for doctor::RunnerDoctorScope {
    fn from(value: RunnerDoctorScopeArg) -> Self {
        match value {
            RunnerDoctorScopeArg::General => doctor::RunnerDoctorScope::General,
            RunnerDoctorScopeArg::LabOffload => doctor::RunnerDoctorScope::LabOffload,
        }
    }
}

impl From<RunnerKindArg> for RunnerKind {
    fn from(value: RunnerKindArg) -> Self {
        match value {
            RunnerKindArg::Local => RunnerKind::Local,
            RunnerKindArg::Ssh => RunnerKind::Ssh,
        }
    }
}

pub fn run(
    args: RunnerArgs,
    _global: &crate::commands::GlobalArgs,
) -> CmdResult<RunnerCommandOutput> {
    match args.command {
        RunnerCommand::Add {
            json,
            skip_existing,
            id,
            kind,
            server,
            workspace_root,
            homeboy_path,
            daemon,
            concurrency_limit,
            artifact_policy,
        } => map_registry(add(RunnerAddInput {
            json,
            skip_existing,
            id,
            kind,
            server,
            workspace_root,
            settings: RunnerSettings {
                homeboy_path,
                daemon,
                concurrency_limit,
                artifact_policy,
            },
        })),
        RunnerCommand::Enable {
            server_id,
            workspace_root,
            homeboy_path,
            daemon,
            concurrency_limit,
            artifact_policy,
        } => map_registry(enable(
            &server_id,
            workspace_root,
            RunnerSettings {
                homeboy_path,
                daemon,
                concurrency_limit,
                artifact_policy,
            },
        )),
        RunnerCommand::List => map_registry(list()),
        RunnerCommand::Show { id } => map_registry(show(&id)),
        RunnerCommand::Set { args } => map_registry(set(args)),
        RunnerCommand::Trust {
            runner_id,
            projects,
            commands,
            allow_raw_exec,
            workspace_roots,
            artifact_policy,
            peers,
            fingerprints,
        } => map_registry(policy::update(
            &runner_id,
            policy::RunnerPolicyPatch::trust(
                peers,
                fingerprints,
                projects,
                commands,
                allow_raw_exec,
                workspace_roots,
                artifact_policy,
            ),
            "runner.trust",
        )),
        RunnerCommand::Pair {
            runner_id,
            peers,
            fingerprints,
            projects,
            workspace_roots,
            allow_raw_exec,
        } => map_registry(policy::update(
            &runner_id,
            policy::RunnerPolicyPatch::pair(
                peers,
                fingerprints,
                projects,
                allow_raw_exec,
                workspace_roots,
            ),
            "runner.pair",
        )),
        RunnerCommand::Remove { id } => map_registry(remove(&id)),
        RunnerCommand::Doctor {
            runner_id,
            path,
            required_extensions,
            required_tools,
            scope,
            repair,
        } => map_doctor(doctor::run_with_options(
            &runner_id,
            doctor::RunnerDoctorOptions {
                path,
                extensions: required_extensions,
                required_tools,
                scope: scope.into(),
                repair,
            },
        )),
        RunnerCommand::Connect {
            id,
            reverse,
            reverse_runner,
            broker_url,
        } => map_registry(connect(&id, reverse, reverse_runner, broker_url)),
        RunnerCommand::Status { id } => map_registry(status(id.as_deref())),
        RunnerCommand::Disconnect { id } => map_registry(disconnect(&id)),
        RunnerCommand::Exec {
            id,
            cwd,
            project,
            ssh,
            capture_patch,
            require_paths,
            command,
        } => map_execution(exec(
            &id,
            cwd,
            project,
            ssh,
            capture_patch,
            require_paths,
            command,
        )),
        RunnerCommand::Env { id, show_values } => map_env(env(&id, show_values)),
        RunnerCommand::Job { command } => map_job(job(command)),
        RunnerCommand::Work {
            runner_id,
            broker_url,
            project,
            lease_ms,
            r#loop,
            idle_backoff_ms,
            max_idle_backoff_ms,
            broker_failure_backoff_ms,
            broker_retry_limit,
        } => map_worker(runner::run_reverse_worker(ReverseRunnerWorkerOptions {
            runner_id,
            broker_url,
            project_id: project,
            lease_ms,
            loop_mode: r#loop,
            idle_backoff_ms,
            max_idle_backoff_ms,
            broker_failure_backoff_ms,
            broker_retry_limit,
        })),
        RunnerCommand::Workspace { command } => workspace::run(command)
            .map(|(output, exit_code)| (RunnerCommandOutput::Workspace(output), exit_code)),
    }
}

fn map_registry(result: CmdResult<RunnerOutput>) -> CmdResult<RunnerCommandOutput> {
    result.map(|(mut output, exit_code)| {
        redact_runner_output_env(&mut output);
        (RunnerCommandOutput::Registry(output), exit_code)
    })
}

fn redact_runner_output_env(output: &mut RunnerOutput) {
    if let Some(runner) = output.entity.as_mut() {
        redact_runner_env(runner);
    }

    for runner in &mut output.entities {
        redact_runner_env(runner);
    }
}

fn redact_runner_env(runner: &mut Runner) {
    let policy = RedactionPolicy::default();
    for (key, value) in runner.env.iter_mut() {
        if policy.is_sensitive_key(key) {
            *value = REDACTED_ENV_VALUE.to_string();
        } else {
            *value = policy.redact_string(value);
        }
    }
}

fn map_doctor(result: CmdResult<doctor::RunnerDoctorOutput>) -> CmdResult<RunnerCommandOutput> {
    result.map(|(output, exit_code)| (RunnerCommandOutput::Doctor(output), exit_code))
}

fn map_execution(result: CmdResult<RunnerExecOutput>) -> CmdResult<RunnerCommandOutput> {
    result.map(|(output, exit_code)| (RunnerCommandOutput::Execution(output), exit_code))
}

fn map_env(result: CmdResult<RunnerEnvOutput>) -> CmdResult<RunnerCommandOutput> {
    result.map(|(output, exit_code)| (RunnerCommandOutput::Env(output), exit_code))
}

fn map_job(result: CmdResult<RunnerJobOutput>) -> CmdResult<RunnerCommandOutput> {
    result.map(|(output, exit_code)| (RunnerCommandOutput::Job(output), exit_code))
}

fn map_worker(result: CmdResult<ReverseRunnerWorkerOutput>) -> CmdResult<RunnerCommandOutput> {
    result.map(|(output, exit_code)| (RunnerCommandOutput::Worker(output), exit_code))
}

struct RunnerAddInput {
    json: Option<String>,
    skip_existing: bool,
    id: Option<String>,
    kind: Option<RunnerKindArg>,
    server: Option<String>,
    workspace_root: Option<String>,
    settings: RunnerSettings,
}

fn add(input: RunnerAddInput) -> CmdResult<RunnerOutput> {
    let json_spec = if let Some(spec) = input.json {
        spec
    } else {
        let id = input.id.ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "id",
                "Missing required argument: id",
                None,
                None,
            )
        })?;
        let kind = input.kind.map(RunnerKind::from).unwrap_or_else(|| {
            if input.server.is_some() {
                RunnerKind::Ssh
            } else {
                RunnerKind::Local
            }
        });
        let new_runner = Runner {
            id,
            kind,
            server_id: input.server,
            workspace_root: input.workspace_root,
            settings: input.settings,
            env: HashMap::new(),
            secret_env: HashMap::new(),
            resources: HashMap::<String, Value>::new(),
            policy: RunnerPolicy::default(),
        };

        homeboy::core::config::to_json_string(&new_runner)?
    };

    match runner::create(&json_spec, input.skip_existing)? {
        homeboy::core::CreateOutput::Single(result) => Ok((
            RunnerOutput {
                command: "runner.add".to_string(),
                id: Some(result.id),
                entity: Some(result.entity),
                updated_fields: vec!["created".to_string()],
                ..Default::default()
            },
            0,
        )),
        homeboy::core::CreateOutput::Bulk(summary) => {
            let exit_code = summary.exit_code();
            Ok((
                RunnerOutput {
                    command: "runner.add".to_string(),
                    import: Some(summary),
                    ..Default::default()
                },
                exit_code,
            ))
        }
    }
}

fn list() -> CmdResult<RunnerOutput> {
    Ok((
        RunnerOutput {
            command: "runner.list".to_string(),
            entities: runner::list()?,
            extra: RunnerExtra {
                sessions: runner::statuses()?,
                ..Default::default()
            },
            ..Default::default()
        },
        0,
    ))
}

fn enable(
    server_id: &str,
    workspace_root: Option<String>,
    settings: RunnerSettings,
) -> CmdResult<RunnerOutput> {
    let mut spec = serde_json::Map::new();
    if let Some(workspace_root) = workspace_root {
        spec.insert("workspace_root".to_string(), workspace_root.into());
    }
    if let Some(homeboy_path) = settings.homeboy_path {
        spec.insert("homeboy_path".to_string(), homeboy_path.into());
    }
    if settings.daemon {
        spec.insert("daemon".to_string(), true.into());
    }
    if let Some(concurrency_limit) = settings.concurrency_limit {
        spec.insert("concurrency_limit".to_string(), concurrency_limit.into());
    }
    if let Some(artifact_policy) = settings.artifact_policy {
        spec.insert("artifact_policy".to_string(), artifact_policy.into());
    }
    let runner = runner::enable_server_runner(server_id, Value::Object(spec))?;
    Ok((
        RunnerOutput {
            command: "runner.enable".to_string(),
            id: Some(runner.id.clone()),
            entity: Some(runner),
            updated_fields: vec!["runner".to_string()],
            ..Default::default()
        },
        0,
    ))
}

fn show(id: &str) -> CmdResult<RunnerOutput> {
    let runner = runner::load(id)?;
    Ok((
        RunnerOutput {
            command: "runner.show".to_string(),
            id: Some(runner.id.clone()),
            entity: Some(runner),
            ..Default::default()
        },
        0,
    ))
}

fn set(args: DynamicSetArgs) -> CmdResult<RunnerOutput> {
    let merged = super::merge_dynamic_args(&args)?.ok_or_else(|| {
        homeboy::core::Error::validation_invalid_argument(
            "spec",
            "Provide --json '<object>' or --base64 <encoded-json>",
            None,
            Some(vec![
                "Arbitrary runner updates must use explicit JSON input.".to_string(),
                "Example: homeboy runner set <id> --json '{\"workspace_root\":\"/srv/homeboy\"}'"
                    .to_string(),
            ]),
        )
    })?;
    let (json_string, replace_fields) = super::finalize_set_spec(&merged, &args.replace)?;

    match runner::merge(args.id.as_deref(), &json_string, &replace_fields)? {
        MergeOutput::Single(result) => {
            let entity = runner::load(&result.id)?;
            Ok((
                RunnerOutput {
                    command: "runner.set".to_string(),
                    id: Some(result.id),
                    entity: Some(entity),
                    updated_fields: result.updated_fields,
                    ..Default::default()
                },
                0,
            ))
        }
        MergeOutput::Bulk(summary) => {
            let exit_code = summary.exit_code();
            Ok((
                RunnerOutput {
                    command: "runner.set".to_string(),
                    batch: Some(summary),
                    ..Default::default()
                },
                exit_code,
            ))
        }
    }
}

fn remove(id: &str) -> CmdResult<RunnerOutput> {
    runner::delete_safe(id)?;
    Ok((
        RunnerOutput {
            command: "runner.remove".to_string(),
            id: Some(id.to_string()),
            deleted: vec![id.to_string()],
            ..Default::default()
        },
        0,
    ))
}

fn connect(
    id: &str,
    reverse: bool,
    runner_id: Option<String>,
    broker_url: Option<String>,
) -> CmdResult<RunnerOutput> {
    let (report, exit_code) = if reverse {
        let runner_id = runner_id.ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "runner",
                "Provide --reverse-runner <runner-id> when using --reverse",
                None,
                None,
            )
        })?;
        runner::connect_reverse(ReverseRunnerConnectOptions {
            controller_id: id.to_string(),
            runner_id,
            broker_url,
        })?
    } else {
        runner::connect(id)?
    };
    Ok((
        RunnerOutput {
            command: "runner.connect".to_string(),
            id: Some(report.runner_id.clone()),
            extra: RunnerExtra {
                connection: Some(RunnerConnectionOutput::Connect(report)),
                ..Default::default()
            },
            ..Default::default()
        },
        exit_code,
    ))
}

fn status(id: Option<&str>) -> CmdResult<RunnerOutput> {
    if let Some(id) = id {
        let mut report = runner::status(id)?;
        report.active_jobs = active_runner_jobs(id);
        return Ok((
            RunnerOutput {
                command: "runner.status".to_string(),
                id: Some(id.to_string()),
                extra: RunnerExtra {
                    connection: Some(RunnerConnectionOutput::Status(report)),
                    ..Default::default()
                },
                ..Default::default()
            },
            0,
        ));
    }

    Ok((
        RunnerOutput {
            command: "runner.status".to_string(),
            extra: RunnerExtra {
                sessions: runner::statuses()?
                    .into_iter()
                    .map(|mut report| {
                        report.active_jobs = active_runner_jobs(&report.runner_id);
                        report
                    })
                    .collect(),
                ..Default::default()
            },
            ..Default::default()
        },
        0,
    ))
}

fn active_runner_jobs(runner_id: &str) -> Vec<homeboy::core::api_jobs::ActiveRunnerJobSummary> {
    runner::daemon_api_get(runner_id, "/jobs")
        .ok()
        .and_then(|data| data.get("body").cloned())
        .and_then(|body| body.get("active_runner_jobs").cloned())
        .and_then(|jobs| serde_json::from_value(jobs).ok())
        .unwrap_or_default()
}

fn disconnect(id: &str) -> CmdResult<RunnerOutput> {
    Ok((
        RunnerOutput {
            command: "runner.disconnect".to_string(),
            id: Some(id.to_string()),
            extra: RunnerExtra {
                connection: Some(RunnerConnectionOutput::Disconnect(runner::disconnect(id)?)),
                ..Default::default()
            },
            ..Default::default()
        },
        0,
    ))
}

fn exec(
    runner_id: &str,
    cwd: Option<String>,
    project_id: Option<String>,
    allow_diagnostic_ssh: bool,
    capture_patch: bool,
    require_paths: Vec<String>,
    command: Vec<String>,
) -> CmdResult<RunnerExecOutput> {
    let required_commands = command.first().cloned().into_iter().collect();
    runner::exec(
        runner_id,
        runner::RunnerExecOptions {
            cwd,
            project_id,
            allow_diagnostic_ssh,
            command,
            env: Default::default(),
            capture_patch,
            raw_exec: true,
            source_snapshot: None,
            capability_preflight: Some(runner::RunnerCapabilityPreflight {
                command: "runner.exec".to_string(),
                required_commands,
                ..Default::default()
            }),
            required_extensions: Vec::new(),
            require_paths,
        },
    )
}

fn env(runner_id: &str, show_values: bool) -> CmdResult<RunnerEnvOutput> {
    let runner = runner::load(runner_id)?;
    let effective_env = runner::effective_env(runner_id)?;
    let env = effective_env
        .into_iter()
        .map(|(key, value)| {
            (
                key,
                if show_values {
                    value
                } else {
                    REDACTED_ENV_VALUE.to_string()
                },
            )
        })
        .collect();
    let secret_env = runner
        .secret_env
        .into_iter()
        .map(|(key, reference)| (key, secret_env_reference_output(reference)))
        .collect();

    Ok((
        RunnerEnvOutput {
            command: "runner.env".to_string(),
            runner_id: runner_id.to_string(),
            source: "runner_job_env".to_string(),
            values_redacted: !show_values,
            env,
            secret_env,
            diagnostics: RunnerEnvDiagnostics {
                server_shell_env: "Use `homeboy ssh <server> -- printenv NAME` to inspect the server login shell environment; it does not include runner job env by default.".to_string(),
                runner_job_env: "This output shows configured public env Homeboy injects into runner jobs. secret_env entries are shown as refs only; their values resolve on the runner at execution time and are never printed here.".to_string(),
            },
        },
        0,
    ))
}

fn job(command: RunnerJobCommand) -> CmdResult<RunnerJobOutput> {
    match command {
        RunnerJobCommand::Logs {
            runner_id,
            job_id,
            follow,
            poll_ms,
        } => job_logs(&runner_id, &job_id, follow, poll_ms),
        RunnerJobCommand::Cancel { runner_id, job_id } => job_cancel(&runner_id, &job_id),
    }
}

fn job_cancel(runner_id: &str, job_id: &str) -> CmdResult<RunnerJobOutput> {
    let body = runner::daemon_api_post(runner_id, &format!("/jobs/{job_id}/cancel"))?;
    let canonical = body.get("body").unwrap_or(&body);
    let job: Job = serde_json::from_value(canonical["job"].clone()).map_err(|err| {
        homeboy::core::Error::internal_json(
            err.to_string(),
            Some("parse runner job cancel response".to_string()),
        )
    })?;
    let events = canonical
        .get("events")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|err| {
            homeboy::core::Error::internal_json(
                err.to_string(),
                Some("parse runner job cancel events".to_string()),
            )
        })?
        .unwrap_or_default();

    Ok((
        RunnerJobOutput {
            command: "runner.job.cancel",
            runner_id: runner_id.to_string(),
            job_id: job_id.to_string(),
            follow: false,
            job,
            events,
        },
        0,
    ))
}

fn job_logs(
    runner_id: &str,
    job_id: &str,
    follow: bool,
    poll_ms: u64,
) -> CmdResult<RunnerJobOutput> {
    let poll_interval = Duration::from_millis(poll_ms.max(100));
    let mut emitted_sequence = 0;
    let mut snapshot = runner_job_log_snapshot(runner_id, job_id)?;

    emit_new_job_events(&snapshot.events, &mut emitted_sequence);
    while follow && !runner_job_terminal(snapshot.job.status) {
        std::thread::sleep(poll_interval);
        snapshot = runner_job_log_snapshot(runner_id, job_id)?;
        emit_new_job_events(&snapshot.events, &mut emitted_sequence);
    }

    Ok((
        RunnerJobOutput {
            command: "runner.job.logs",
            runner_id: runner_id.to_string(),
            job_id: job_id.to_string(),
            follow,
            job: snapshot.job,
            events: snapshot.events,
        },
        0,
    ))
}

fn emit_new_job_events(events: &[JobEvent], emitted_sequence: &mut u64) {
    for event in events {
        if event.sequence <= *emitted_sequence {
            continue;
        }
        eprintln!("{}", format_job_event(event));
        *emitted_sequence = event.sequence;
    }
}

fn format_job_event(event: &JobEvent) -> String {
    let kind = format!("{:?}", event.kind).to_ascii_lowercase();
    let message = event.message.as_deref().unwrap_or("");
    let data = event
        .data
        .as_ref()
        .map(|data| serde_json::to_string(data).unwrap_or_else(|_| "null".to_string()))
        .unwrap_or_default();
    match (message.is_empty(), data.is_empty()) {
        (true, true) => format!("#{:04} {}", event.sequence, kind),
        (false, true) => format!("#{:04} {} {}", event.sequence, kind, message),
        (true, false) => format!("#{:04} {} {}", event.sequence, kind, data),
        (false, false) => format!("#{:04} {} {} {}", event.sequence, kind, message, data),
    }
}

fn runner_job_terminal(status: JobStatus) -> bool {
    matches!(
        status,
        JobStatus::Succeeded | JobStatus::Failed | JobStatus::Cancelled
    )
}

fn secret_env_reference_output(reference: RunnerSecretEnvRef) -> RunnerSecretEnvReferenceOutput {
    RunnerSecretEnvReferenceOutput {
        env: reference.env,
        file: reference.file,
        values_redacted: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runner_with_env(id: &str) -> Runner {
        Runner {
            id: id.to_string(),
            kind: RunnerKind::Local,
            server_id: None,
            workspace_root: None,
            settings: RunnerSettings::default(),
            env: HashMap::from([
                ("OPENCODE_API_KEY".to_string(), "secret-token".to_string()),
                (
                    "HOMEBOY_PUBLIC_ARTIFACT_BASE_URL".to_string(),
                    "https://artifacts.example.test".to_string(),
                ),
            ]),
            secret_env: HashMap::new(),
            resources: HashMap::new(),
            policy: RunnerPolicy::default(),
        }
    }

    #[test]
    fn runner_job_event_format_includes_sequence_kind_message_and_data() {
        let event = JobEvent {
            sequence: 7,
            job_id: uuid::Uuid::nil(),
            kind: homeboy::core::api_jobs::JobEventKind::Progress,
            timestamp_ms: 123,
            message: Some("cell started".to_string()),
            data: Some(serde_json::json!({ "cell": "audit" })),
        };

        assert_eq!(
            format_job_event(&event),
            "#0007 progress cell started {\"cell\":\"audit\"}"
        );
    }

    #[test]
    fn registry_entity_output_redacts_runner_env_values() {
        let (output, exit_code) = map_registry(Ok((
            RunnerOutput {
                command: "runner.show".to_string(),
                entity: Some(runner_with_env("lab")),
                ..Default::default()
            },
            0,
        )))
        .expect("map output");

        assert_eq!(exit_code, 0);
        let value = serde_json::to_value(output).expect("serialize output");
        assert_eq!(
            value["entity"]["env"]["OPENCODE_API_KEY"],
            REDACTED_ENV_VALUE
        );
        assert_eq!(
            value["entity"]["env"]["HOMEBOY_PUBLIC_ARTIFACT_BASE_URL"],
            "https://artifacts.example.test"
        );
        assert!(!value.to_string().contains("secret-token"));
    }

    #[test]
    fn registry_list_output_redacts_runner_env_values() {
        let (output, _) = map_registry(Ok((
            RunnerOutput {
                command: "runner.list".to_string(),
                entities: vec![runner_with_env("lab")],
                ..Default::default()
            },
            0,
        )))
        .expect("map output");

        let value = serde_json::to_value(output).expect("serialize output");
        assert_eq!(
            value["entities"][0]["env"]["OPENCODE_API_KEY"],
            REDACTED_ENV_VALUE
        );
        assert_eq!(
            value["entities"][0]["env"]["HOMEBOY_PUBLIC_ARTIFACT_BASE_URL"],
            "https://artifacts.example.test"
        );
        assert!(!value.to_string().contains("secret-token"));
    }

    #[test]
    fn runner_env_output_redacts_values_by_default() {
        let output = RunnerEnvOutput {
            command: "runner.env".to_string(),
            runner_id: "lab".to_string(),
            source: "runner_job_env".to_string(),
            values_redacted: true,
            env: BTreeMap::from([("TOKEN".to_string(), REDACTED_ENV_VALUE.to_string())]),
            secret_env: BTreeMap::new(),
            diagnostics: RunnerEnvDiagnostics {
                server_shell_env: "shell".to_string(),
                runner_job_env: "runner".to_string(),
            },
        };

        let value = serde_json::to_value(output).expect("serialize output");

        assert_eq!(value["command"], "runner.env");
        assert_eq!(value["source"], "runner_job_env");
        assert_eq!(value["values_redacted"], true);
        assert_eq!(value["env"]["TOKEN"], REDACTED_ENV_VALUE);
    }

    #[test]
    fn runner_env_output_reports_secret_env_refs_without_values() {
        let output = RunnerEnvOutput {
            command: "runner.env".to_string(),
            runner_id: "lab".to_string(),
            source: "runner_job_env".to_string(),
            values_redacted: false,
            env: BTreeMap::from([(
                "HOMEBOY_PUBLIC_ARTIFACT_BASE_URL".to_string(),
                "https://artifacts.example.test".to_string(),
            )]),
            secret_env: BTreeMap::from([(
                "OPENAI_API_KEY".to_string(),
                RunnerSecretEnvReferenceOutput {
                    env: Some("OPENAI_API_KEY".to_string()),
                    file: None,
                    values_redacted: true,
                },
            )]),
            diagnostics: RunnerEnvDiagnostics {
                server_shell_env: "shell".to_string(),
                runner_job_env: "runner".to_string(),
            },
        };

        let value = serde_json::to_value(output).expect("serialize output");

        assert_eq!(
            value["env"]["HOMEBOY_PUBLIC_ARTIFACT_BASE_URL"],
            "https://artifacts.example.test"
        );
        assert_eq!(
            value["secret_env"]["OPENAI_API_KEY"]["env"],
            "OPENAI_API_KEY"
        );
        assert_eq!(
            value["secret_env"]["OPENAI_API_KEY"]["values_redacted"],
            true
        );
        assert!(!value.to_string().contains("dummy-secret"));
    }
}
