use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{self, Read};
use std::time::Duration;

use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;
use serde_json::Value;

use homeboy::core::api_jobs::{Job, JobEvent, JobStatus};
use homeboy::core::redaction::RedactionPolicy;
use homeboy::core::runners::{
    self as runner, runner_job_log_snapshot, ReverseRunnerConnectOptions,
    ReverseRunnerWorkerOptions, ReverseRunnerWorkerOutput, Runner, RunnerConnectReport,
    RunnerDisconnectReport, RunnerExecOutput, RunnerKind, RunnerSession, RunnerStatusReport,
    RunnerTunnelMode,
};
use homeboy::core::server::{RunnerPolicy, RunnerSecretEnvRef, RunnerSettings};
use homeboy::core::stream_capture::StreamCaptureMetadata;
use homeboy::core::{EntityCrudOutput, MergeOutput};

use super::output_runtime::{CommandPresentation, JsonCommandRun};
use super::{CmdResult, DynamicSetArgs};

pub mod doctor;
mod policy;
mod workspace;

#[derive(Debug, Serialize)]
pub struct RunnerExtra {
    pub variant: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection: Option<RunnerConnectionOutput>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<RunnerStatusReport>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub operator_hints: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub operator_commands: Vec<RunnerOperatorCommand>,
}

impl Default for RunnerExtra {
    fn default() -> Self {
        Self {
            variant: "registry",
            connection: None,
            sessions: Vec::new(),
            operator_hints: Vec::new(),
            operator_commands: Vec::new(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct RunnerOperatorCommand {
    pub scope: &'static str,
    pub runner_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    pub command: String,
    pub description: String,
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
const RUNNER_EXEC_SCRIPT_ENV: &str = "HOMEBOY_RUNNER_EXEC_SCRIPT";

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
    Broker(RunnerBrokerOutput),
}

/// Result of a broker auth/pairing management command. The plaintext `token` is
/// present only on a successful `pair` and is the single time it is ever shown.
#[derive(Debug, Serialize)]
pub struct RunnerBrokerOutput {
    pub command: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_id: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    /// One-time plaintext bearer token (only on `pair`). Never re-displayed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revoked: Option<bool>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub credentials: Vec<RunnerBrokerCredentialSummary>,
    pub store_path: String,
}

/// Non-secret summary of a stored broker credential. Token hashes are never
/// surfaced.
#[derive(Debug, Serialize)]
pub struct RunnerBrokerCredentialSummary {
    pub id: String,
    pub runner_id: String,
    pub scopes: Vec<String>,
    pub revoked: bool,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
pub struct RunnerJobOutput {
    pub variant: &'static str,
    pub command: &'static str,
    pub runner_id: String,
    pub job_id: String,
    pub follow: bool,
    pub job: Job,
    pub events: Vec<JobEvent>,
}

#[derive(Debug, Serialize)]
pub struct RunnerEnvOutput {
    pub variant: &'static str,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
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

        /// Read a shell script from this path and execute it on the runner with bash.
        /// Use `-` to read the script from stdin.
        #[arg(long = "script-file")]
        script_file: Option<String>,

        /// Environment variable to inject into the runner process as KEY=VALUE.
        /// Repeat for multiple values.
        #[arg(long = "env")]
        env: Vec<String>,

        /// Build the runner exec plan without executing it.
        #[arg(long)]
        dry_run: bool,

        /// Print remote stdout/stderr directly instead of the structured JSON envelope.
        /// Use global --output to still write the full structured envelope to a file.
        #[arg(long)]
        raw: bool,

        /// Command and arguments to execute on the runner
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Show the effective environment injected into runner jobs
    Env {
        /// Runner ID
        id: String,
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

        /// Paired broker bearer token. Falls back to the HOMEBOY_BROKER_TOKEN
        /// environment variable when omitted. Required when the broker enforces
        /// auth; omit only for loopback-open smoke setups.
        #[arg(long)]
        broker_token: Option<String>,

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
    /// Manage reverse runner broker authentication and pairing
    Broker {
        #[command(subcommand)]
        command: RunnerBrokerCommand,
    },
}

#[derive(Subcommand)]
enum RunnerBrokerCommand {
    /// Pair a runner with the broker, minting a one-time scoped bearer token
    Pair {
        /// Stable credential id used for later revocation
        id: String,

        /// Runner id this credential authorizes (worker routes must match it)
        #[arg(long)]
        runner_id: String,

        /// Grant the controller submit scope (POST /runner/jobs)
        #[arg(long)]
        submit: bool,

        /// Grant the worker scope (register/claim/event/finish/heartbeat)
        #[arg(long)]
        work: bool,
    },
    /// Revoke a paired credential by id
    Revoke {
        /// Credential id to revoke
        id: String,
    },
    /// List paired broker credentials (never prints tokens)
    List,
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
            script_file,
            env,
            dry_run,
            raw: _,
            command,
        } => map_execution(exec(
            &id,
            cwd,
            project,
            ssh,
            capture_patch,
            require_paths,
            script_file,
            env,
            dry_run,
            command,
        )),
        RunnerCommand::Env { id } => map_env(env(&id)),
        RunnerCommand::Job { command } => map_job(job(command)),
        RunnerCommand::Work {
            runner_id,
            broker_url,
            broker_token,
            project,
            lease_ms,
            r#loop,
            idle_backoff_ms,
            max_idle_backoff_ms,
            broker_failure_backoff_ms,
            broker_retry_limit,
        } => {
            let concurrency_limit = runner::load(&runner_id)
                .ok()
                .and_then(|runner| runner.settings.concurrency_limit);
            let broker_token = broker_token.or_else(runner::broker_token_from_env);
            map_worker(runner::run_reverse_worker(ReverseRunnerWorkerOptions {
                runner_id,
                broker_url,
                broker_token,
                project_id: project,
                lease_ms,
                concurrency_limit,
                loop_mode: r#loop,
                idle_backoff_ms,
                max_idle_backoff_ms,
                broker_failure_backoff_ms,
                broker_retry_limit,
            }))
        }
        RunnerCommand::Workspace { command } => workspace::run(command)
            .map(|(output, exit_code)| (RunnerCommandOutput::Workspace(output), exit_code)),
        RunnerCommand::Broker { command } => {
            run_broker(command).map(|output| (RunnerCommandOutput::Broker(output), 0))
        }
    }
}

fn run_broker(command: RunnerBrokerCommand) -> Result<RunnerBrokerOutput, homeboy::core::Error> {
    use std::collections::BTreeSet;

    let mut store = runner::BrokerAuthStore::load()?;
    match command {
        RunnerBrokerCommand::Pair {
            id,
            runner_id,
            submit,
            work,
        } => {
            let mut scopes: BTreeSet<runner::BrokerScope> = BTreeSet::new();
            if submit {
                scopes.insert(runner::BrokerScope::Submit);
            }
            if work {
                scopes.insert(runner::BrokerScope::Work);
            }
            if scopes.is_empty() {
                // Default to a worker credential, the most common pairing.
                scopes.insert(runner::BrokerScope::Work);
            }
            let minted = store.pair(id, runner_id, scopes)?;
            let store_path = store.save()?;
            let scope_labels = scope_labels(&store, &minted.id);
            Ok(RunnerBrokerOutput {
                command: "runner.broker.pair",
                credential_id: Some(minted.id),
                runner_id: Some(minted.runner_id),
                scopes: scope_labels,
                token: Some(minted.token),
                revoked: None,
                credentials: Vec::new(),
                store_path: store_path.display().to_string(),
            })
        }
        RunnerBrokerCommand::Revoke { id } => {
            let revoked = store.revoke(&id);
            let store_path = store.save()?;
            Ok(RunnerBrokerOutput {
                command: "runner.broker.revoke",
                credential_id: Some(id),
                runner_id: None,
                scopes: Vec::new(),
                token: None,
                revoked: Some(revoked),
                credentials: Vec::new(),
                store_path: store_path.display().to_string(),
            })
        }
        RunnerBrokerCommand::List => {
            let credentials = store
                .credentials
                .iter()
                .map(|cred| RunnerBrokerCredentialSummary {
                    id: cred.id.clone(),
                    runner_id: cred.runner_id.clone(),
                    scopes: cred.scopes.iter().map(scope_label).collect(),
                    revoked: cred.revoked_at.is_some(),
                    created_at: cred.created_at.clone(),
                })
                .collect();
            // Listing does not mutate; resolve the path without rewriting.
            let path = runner::broker_auth_store_path()?;
            Ok(RunnerBrokerOutput {
                command: "runner.broker.list",
                credential_id: None,
                runner_id: None,
                scopes: Vec::new(),
                token: None,
                revoked: None,
                credentials,
                store_path: path.display().to_string(),
            })
        }
    }
}

fn scope_label(scope: &runner::BrokerScope) -> String {
    match scope {
        runner::BrokerScope::Submit => "submit".to_string(),
        runner::BrokerScope::Work => "work".to_string(),
    }
}

fn scope_labels(store: &runner::BrokerAuthStore, id: &str) -> Vec<String> {
    store
        .credentials
        .iter()
        .find(|cred| cred.id == id)
        .map(|cred| cred.scopes.iter().map(scope_label).collect())
        .unwrap_or_default()
}

pub fn run_command_output(args: RunnerArgs, _global: &super::GlobalArgs) -> JsonCommandRun {
    crate::commands::utils::tty::status("homeboy is working...");

    match args.command {
        RunnerCommand::Exec {
            id,
            cwd,
            project,
            ssh,
            capture_patch,
            require_paths,
            script_file,
            env,
            dry_run,
            raw: true,
            command,
        } => run_raw_exec(
            id,
            cwd,
            project,
            ssh,
            capture_patch,
            require_paths,
            script_file,
            env,
            dry_run,
            command,
        ),
        command => {
            let (stdout_result, exit_code) =
                crate::commands::utils::response::map_cmd_result_to_json(run(
                    RunnerArgs { command },
                    _global,
                ));
            JsonCommandRun::from_stdout_result(stdout_result, exit_code)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_raw_exec(
    id: String,
    cwd: Option<String>,
    project: Option<String>,
    ssh: bool,
    capture_patch: bool,
    require_paths: Vec<String>,
    script_file: Option<String>,
    env: Vec<String>,
    dry_run: bool,
    command: Vec<String>,
) -> JsonCommandRun {
    match exec(
        &id,
        cwd,
        project,
        ssh,
        capture_patch,
        require_paths,
        script_file,
        env,
        dry_run,
        command,
    ) {
        Ok((output, exit_code)) => raw_exec_command_run(output, exit_code),
        Err(err) => {
            let (stdout_result, exit_code) =
                crate::commands::utils::response::map_cmd_result_to_json::<RunnerCommandOutput>(
                    Err(err),
                );
            JsonCommandRun::from_stdout_result(stdout_result, exit_code)
        }
    }
}

fn raw_exec_command_run(output: RunnerExecOutput, exit_code: i32) -> JsonCommandRun {
    let presentation_stdout = output.stdout.clone();
    let presentation_stderr = output.stderr.clone();
    let (stdout_result, _) = crate::commands::utils::response::map_cmd_result_to_json(Ok((
        RunnerCommandOutput::Execution(output),
        exit_code,
    )));

    JsonCommandRun::from_stdout_result(stdout_result, exit_code).with_presentation(
        CommandPresentation {
            stdout: Some(presentation_stdout),
            stderr: Some(presentation_stderr),
        },
    )
}

fn map_registry(result: CmdResult<RunnerOutput>) -> CmdResult<RunnerCommandOutput> {
    result.map(|(mut output, exit_code)| {
        redact_runner_output_env(&mut output);
        output.extra.variant = runner_variant_from_command(&output.command);
        (RunnerCommandOutput::Registry(output), exit_code)
    })
}

fn runner_variant_from_command(command: &str) -> &'static str {
    match command {
        "runner.add" => "add",
        "runner.enable" => "enable",
        "runner.list" => "list",
        "runner.show" => "show",
        "runner.set" => "set",
        "runner.trust" => "trust",
        "runner.pair" => "pair",
        "runner.remove" => "remove",
        "runner.connect" => "connect",
        "runner.status" => "status",
        "runner.disconnect" => "disconnect",
        _ => "registry",
    }
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
        let report = runner::status(id)?;
        let operator_hints = runner_status_operator_hints(&report);
        let operator_commands = runner_status_operator_commands(&report);
        return Ok((
            RunnerOutput {
                command: "runner.status".to_string(),
                id: Some(id.to_string()),
                extra: RunnerExtra {
                    connection: Some(RunnerConnectionOutput::Status(report)),
                    operator_hints,
                    operator_commands,
                    ..Default::default()
                },
                ..Default::default()
            },
            0,
        ));
    }

    let sessions = runner::statuses()?;
    let operator_hints = sessions
        .iter()
        .flat_map(runner_status_operator_hints)
        .collect();
    let operator_commands = sessions
        .iter()
        .flat_map(runner_status_operator_commands)
        .collect();
    Ok((
        RunnerOutput {
            command: "runner.status".to_string(),
            extra: RunnerExtra {
                sessions,
                operator_hints,
                operator_commands,
                ..Default::default()
            },
            ..Default::default()
        },
        0,
    ))
}

fn runner_status_operator_hints(report: &RunnerStatusReport) -> Vec<String> {
    let Some(session) = report.session.as_ref().filter(|_| report.connected) else {
        return Vec::new();
    };
    let mut hints = Vec::new();
    match session.mode {
        RunnerTunnelMode::DirectSsh => {
            if report.active_job_count > 0 {
                hints.push(format!(
                    "Active daemon jobs for `{}` are listed from the direct daemon; inspect with `homeboy runner job logs {} <job-id> --follow` and cancel known jobs with `homeboy runner job cancel {} <job-id>`.",
                    report.runner_id, report.runner_id, report.runner_id
                ));
            }
        }
        RunnerTunnelMode::Reverse => reverse_runner_status_hints(report, session, &mut hints),
    }
    hints
}

fn reverse_runner_status_hints(
    report: &RunnerStatusReport,
    session: &RunnerSession,
    hints: &mut Vec<String>,
) {
    if session.broker_url.is_none() {
        hints.push(format!(
            "Reverse runner `{}` has no broker URL; active-job listing, logs, and cancel require reconnecting with `homeboy runner connect <controller-id> --reverse --reverse-runner {} --broker-url <url>`.",
            report.runner_id, report.runner_id
        ));
        return;
    }
    hints.push(format!(
        "Reverse runner `{}` active jobs are listed through the broker; inspect with `homeboy runner job logs {} <job-id> --follow`.",
        report.runner_id, report.runner_id
    ));
    if report.active_job_count > 0 {
        hints.push(format!(
            "Cancel known reverse broker jobs with `homeboy runner job cancel {} <job-id>`; if a claim lease expires, reconcile broker state with POST /runner/jobs/reconcile instead of mutating the job store manually.",
            report.runner_id
        ));
    }
}

fn runner_status_operator_commands(report: &RunnerStatusReport) -> Vec<RunnerOperatorCommand> {
    let Some(session) = report.session.as_ref().filter(|_| report.connected) else {
        return Vec::new();
    };

    let mut commands = Vec::new();
    for job in &report.active_jobs {
        commands.push(RunnerOperatorCommand {
            scope: "job_logs",
            runner_id: report.runner_id.clone(),
            job_id: Some(job.job_id.clone()),
            command: format!(
                "homeboy runner job logs {} {} --follow",
                report.runner_id, job.job_id
            ),
            description: "Follow the active runner job event stream.".to_string(),
        });
        commands.push(RunnerOperatorCommand {
            scope: "job_cancel",
            runner_id: report.runner_id.clone(),
            job_id: Some(job.job_id.clone()),
            command: format!(
                "homeboy runner job cancel {} {}",
                report.runner_id, job.job_id
            ),
            description: "Request cancellation for a queued or running runner job.".to_string(),
        });
        if let Some(run_id) = job.durable_run_id.as_deref() {
            commands.push(RunnerOperatorCommand {
                scope: "artifact_get",
                runner_id: report.runner_id.clone(),
                job_id: Some(job.job_id.clone()),
                command: format!("homeboy runs artifact get {run_id} <artifact-id> -o <path>"),
                description: "Fetch a mirrored observation artifact after the run records one."
                    .to_string(),
            });
        }
    }

    if session.mode == RunnerTunnelMode::Reverse {
        if let Some(broker_url) = session.broker_url.as_deref() {
            commands.push(RunnerOperatorCommand {
                scope: "broker_reconcile",
                runner_id: report.runner_id.clone(),
                job_id: None,
                command: format!(
                    "curl -fsS -X POST {}/runner/jobs/reconcile",
                    broker_url.trim_end_matches('/')
                ),
                description:
                    "Fail expired reverse-runner claims through the broker-owned lifecycle path."
                        .to_string(),
            });
            for job in &report.active_jobs {
                commands.push(RunnerOperatorCommand {
                    scope: "broker_artifact_lookup",
                    runner_id: report.runner_id.clone(),
                    job_id: Some(job.job_id.clone()),
                    command: format!(
                        "curl -fsS {}/runner/jobs/{}/artifacts/<artifact-id>",
                        broker_url.trim_end_matches('/'),
                        job.job_id
                    ),
                    description: "Inspect broker-held reverse-runner artifact metadata."
                        .to_string(),
                });
            }
        }
    }

    commands
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

#[allow(clippy::too_many_arguments)]
fn exec(
    runner_id: &str,
    cwd: Option<String>,
    project_id: Option<String>,
    allow_diagnostic_ssh: bool,
    capture_patch: bool,
    require_paths: Vec<String>,
    script_file: Option<String>,
    env: Vec<String>,
    dry_run: bool,
    command: Vec<String>,
) -> CmdResult<RunnerExecOutput> {
    let script = script_file
        .as_deref()
        .map(read_runner_exec_script)
        .transpose()?;
    let prepared_command = prepare_runner_exec_command(script.as_ref(), command)?;
    let env = prepare_runner_exec_env(env, script.as_deref())?;
    let required_commands = prepared_command.first().cloned().into_iter().collect();

    if dry_run {
        return runner_exec_dry_run(
            runner_id,
            cwd,
            allow_diagnostic_ssh,
            require_paths,
            prepared_command,
            script.unwrap_or_default(),
        );
    }

    runner::exec(
        runner_id,
        runner::RunnerExecOptions {
            cwd,
            project_id,
            allow_diagnostic_ssh,
            command: prepared_command,
            env,
            secret_env_names: script_file
                .is_some()
                .then(|| RUNNER_EXEC_SCRIPT_ENV.to_string())
                .into_iter()
                .collect(),
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
            detach_after_handoff: false,
        },
    )
}

/// Maximum number of bytes retained when reading a runner exec script into
/// memory. The script is executed verbatim, so an oversized script is rejected
/// rather than silently truncated; the cap bounds the retained bytes and the
/// truncation metadata records when the source exceeded the limit (#5238).
const RUNNER_EXEC_SCRIPT_LIMIT_BYTES: usize = 1024 * 1024;

/// Read a stream into memory with an explicit retained-byte bound, returning the
/// retained bytes plus truncation metadata. Reads one byte past the limit so an
/// overflow is detectable without retaining the entire (potentially unbounded)
/// source.
fn read_bounded(
    mut reader: impl Read,
    limit_bytes: usize,
) -> io::Result<(Vec<u8>, StreamCaptureMetadata)> {
    let mut retained = Vec::new();
    let read = reader
        .by_ref()
        .take((limit_bytes as u64).saturating_add(1))
        .read_to_end(&mut retained)?;
    let truncated = read > limit_bytes;
    if truncated {
        retained.truncate(limit_bytes);
    }
    let metadata = StreamCaptureMetadata {
        limit_bytes,
        seen_bytes: read,
        retained_bytes: retained.len(),
        truncated,
    };
    Ok((retained, metadata))
}

fn read_runner_exec_script(path: &str) -> homeboy::core::Result<String> {
    let (bytes, capture) = if path == "-" {
        read_bounded(io::stdin().lock(), RUNNER_EXEC_SCRIPT_LIMIT_BYTES).map_err(|err| {
            homeboy::core::Error::internal_io(
                err.to_string(),
                Some("read runner exec script from stdin".to_string()),
            )
        })?
    } else {
        let file = fs::File::open(path).map_err(|err| {
            homeboy::core::Error::internal_io(
                err.to_string(),
                Some(format!("read runner exec script {path}")),
            )
        })?;
        read_bounded(file, RUNNER_EXEC_SCRIPT_LIMIT_BYTES).map_err(|err| {
            homeboy::core::Error::internal_io(
                err.to_string(),
                Some(format!("read runner exec script {path}")),
            )
        })?
    };

    if capture.truncated {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "script_file",
            format!(
                "runner exec script exceeds the {} byte limit (retained {} of {}+ bytes); refusing to execute a truncated script",
                capture.limit_bytes, capture.retained_bytes, capture.seen_bytes
            ),
            Some(path.to_string()),
            None,
        ));
    }

    String::from_utf8(bytes).map_err(|err| {
        homeboy::core::Error::internal_io(
            err.to_string(),
            Some(format!("decode runner exec script {path}")),
        )
    })
}

fn prepare_runner_exec_command(
    script: Option<&String>,
    command: Vec<String>,
) -> homeboy::core::Result<Vec<String>> {
    match (script.is_some(), command.is_empty()) {
        (true, false) => Err(homeboy::core::Error::validation_invalid_argument(
            "command",
            "runner exec accepts either --script-file or a command argv, not both",
            None,
            None,
        )),
        (false, true) => Err(homeboy::core::Error::validation_invalid_argument(
            "command",
            "runner exec requires a command after -- or --script-file <path>",
            None,
            None,
        )),
        (true, true) => Ok(vec![
            "bash".to_string(),
            "-c".to_string(),
            "printf '%s' \"$HOMEBOY_RUNNER_EXEC_SCRIPT\" | bash -s".to_string(),
        ]),
        (false, false) => Ok(command),
    }
}

fn prepare_runner_exec_env(
    env: Vec<String>,
    script: Option<&str>,
) -> homeboy::core::Result<HashMap<String, String>> {
    let mut values = HashMap::new();
    for assignment in env {
        let Some((key, value)) = assignment.split_once('=') else {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "env",
                "runner exec --env expects KEY=VALUE",
                Some(assignment),
                None,
            ));
        };
        if key.is_empty() || key.contains('=') || key.chars().any(|c| c.is_whitespace()) {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "env",
                "runner exec --env key must be a non-empty shell environment name",
                Some(key.to_string()),
                None,
            ));
        }
        values.insert(key.to_string(), value.to_string());
    }
    if let Some(script) = script {
        values.insert(RUNNER_EXEC_SCRIPT_ENV.to_string(), script.to_string());
    }
    Ok(values)
}

fn runner_exec_dry_run(
    runner_id: &str,
    cwd: Option<String>,
    allow_diagnostic_ssh: bool,
    require_paths: Vec<String>,
    command: Vec<String>,
    script: String,
) -> CmdResult<RunnerExecOutput> {
    let runner = runner::load(runner_id)?;
    let remote_cwd = cwd
        .or_else(|| runner.workspace_root.clone())
        .unwrap_or_else(|| {
            std::env::current_dir()
                .map(display_path)
                .unwrap_or_else(|_| ".".to_string())
        });
    let mode = if runner.kind == RunnerKind::Local {
        runner::RunnerExecMode::Local
    } else if allow_diagnostic_ssh {
        runner::RunnerExecMode::DiagnosticSsh
    } else {
        runner::RunnerExecMode::Daemon
    };

    Ok((
        RunnerExecOutput {
            variant: "exec",
            command: "runner.exec",
            runner_id: runner.id,
            dry_run: true,
            mode,
            argv: command,
            remote_cwd,
            exit_code: 0,
            stdout: script,
            stderr: String::new(),
            source_snapshot: None,
            job: None,
            job_id: None,
            job_events: None,
            mirror_run_id: None,
            patch: None,
            artifacts: Vec::new(),
            metrics: None,
            capture: None,
            diagnostics: Some(runner::RunnerExecDiagnostics {
                runner_workspace_root: runner.workspace_root,
                source_snapshot_remote_path: None,
                required_paths: require_paths,
                hints: vec!["dry run only; no runner command was executed".to_string()],
            }),
        },
        0,
    ))
}

fn display_path(path: std::path::PathBuf) -> String {
    path.display().to_string()
}

fn env(runner_id: &str) -> CmdResult<RunnerEnvOutput> {
    let runner = runner::load(runner_id)?;
    let effective_env = runner::effective_env(runner_id)?;
    let env = effective_env
        .into_keys()
        .map(|key| (key, REDACTED_ENV_VALUE.to_string()))
        .collect();
    let secret_env = runner
        .secret_env
        .into_iter()
        .map(|(key, reference)| (key, secret_env_reference_output(reference)))
        .collect();

    Ok((
        RunnerEnvOutput {
            variant: "env",
            command: "runner.env".to_string(),
            runner_id: runner_id.to_string(),
            source: "runner_job_env".to_string(),
            values_redacted: true,
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
    let (job, events) = homeboy::core::runners::runner_job_cancel(runner_id, job_id)?;

    Ok((
        RunnerJobOutput {
            variant: "job_cancel",
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
            variant: "job_logs",
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
        secret: reference.secret,
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
        assert_eq!(value["variant"], "show");
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
    fn raw_exec_command_run_keeps_structured_output_and_presentation_streams() {
        let run = raw_exec_command_run(
            RunnerExecOutput {
                variant: "exec",
                command: "runner.exec",
                runner_id: "lab".to_string(),
                dry_run: false,
                mode: runner::RunnerExecMode::Daemon,
                argv: vec!["printf".to_string(), "hello".to_string()],
                remote_cwd: "/workspace".to_string(),
                exit_code: 7,
                stdout: "hello\n".to_string(),
                stderr: "warn\n".to_string(),
                source_snapshot: None,
                job: None,
                job_id: Some("job-123".to_string()),
                job_events: None,
                mirror_run_id: None,
                patch: None,
                artifacts: Vec::new(),
                metrics: None,
                capture: None,
                diagnostics: None,
            },
            7,
        );

        assert_eq!(run.exit_code, 7);
        assert_eq!(run.presentation.stdout.as_deref(), Some("hello\n"));
        assert_eq!(run.presentation.stderr.as_deref(), Some("warn\n"));

        let value = run.stdout_result.expect("structured output");
        assert_eq!(value["command"], "runner.exec");
        assert_eq!(value["variant"], "exec");
        assert_eq!(value["stdout"], "hello\n");
        assert_eq!(value["stderr"], "warn\n");
        assert_eq!(value["job_id"], "job-123");
    }

    #[test]
    fn reverse_runner_status_commands_include_lifecycle_operations() {
        let report = RunnerStatusReport {
            runner_id: "homeboy-lab".to_string(),
            connected: true,
            state: runner::RunnerSessionState::Connected,
            session: Some(RunnerSession {
                runner_id: "homeboy-lab".to_string(),
                mode: RunnerTunnelMode::Reverse,
                role: runner::RunnerSessionRole::Controller,
                server_id: None,
                controller_id: Some("controller".to_string()),
                broker_url: Some("https://broker.example.test/".to_string()),
                remote_daemon_address: None,
                local_port: None,
                local_url: None,
                tunnel_pid: None,
                remote_daemon_pid: None,
                homeboy_version: "test".to_string(),
                homeboy_build_identity: None,
                connected_at: "2026-06-19T00:00:00Z".to_string(),
                worker_identity: None,
                worker_pid: None,
                last_seen_at: Some("2026-06-19T00:00:01Z".to_string()),
            }),
            stale_daemon: None,
            active_jobs: vec![homeboy::core::api_jobs::ActiveRunnerJobSummary {
                runner_id: "homeboy-lab".to_string(),
                job_id: "job-123".to_string(),
                operation: "runner.exec".to_string(),
                source: "broker".to_string(),
                kind: "runner.exec".to_string(),
                status: JobStatus::Running,
                command: "true".to_string(),
                cwd: None,
                started_at_ms: 1000,
                updated_at_ms: 1500,
                elapsed_ms: 500,
                heartbeat_age_ms: 0,
                claim_id: Some("claim-123".to_string()),
                claimed_by_runner_id: Some("homeboy-lab".to_string()),
                claimed_at_ms: Some(1000),
                claim_expires_at_ms: Some(31_000),
                claim_expires_in_ms: Some(29_500),
                durable_run_id: Some("run-123".to_string()),
                active_child_count: None,
                active_cell_count: None,
            }],
            active_job_count: 1,
            session_path: "/tmp/session.json".to_string(),
        };

        let commands = runner_status_operator_commands(&report);
        let serialized = serde_json::to_string(&commands).expect("serialize commands");

        assert!(serialized.contains("homeboy runner job logs homeboy-lab job-123 --follow"));
        assert!(serialized.contains("homeboy runner job cancel homeboy-lab job-123"));
        assert!(serialized.contains("homeboy runs artifact get run-123 <artifact-id> -o <path>"));
        assert!(serialized
            .contains("curl -fsS -X POST https://broker.example.test/runner/jobs/reconcile"));
        assert!(serialized.contains(
            "curl -fsS https://broker.example.test/runner/jobs/job-123/artifacts/<artifact-id>"
        ));
    }

    #[test]
    fn read_bounded_retains_full_source_within_limit() {
        let (bytes, capture) = read_bounded(&b"echo hi"[..], 1024).expect("read bounded");

        assert_eq!(bytes, b"echo hi");
        assert_eq!(capture.limit_bytes, 1024);
        assert_eq!(capture.seen_bytes, 7);
        assert_eq!(capture.retained_bytes, 7);
        assert!(!capture.truncated);
    }

    #[test]
    fn read_bounded_marks_truncated_when_source_exceeds_limit() {
        let source = [b'x'; 16];
        let (bytes, capture) = read_bounded(&source[..], 4).expect("read bounded");

        assert_eq!(bytes.len(), 4);
        assert_eq!(capture.limit_bytes, 4);
        assert_eq!(capture.retained_bytes, 4);
        assert!(capture.seen_bytes > capture.retained_bytes);
        assert!(capture.truncated);
    }

    #[test]
    fn read_runner_exec_script_rejects_oversized_script() {
        use std::io::Write;

        let mut file = tempfile::NamedTempFile::new().expect("temp script");
        let oversized = vec![b'a'; RUNNER_EXEC_SCRIPT_LIMIT_BYTES + 1];
        file.write_all(&oversized).expect("write script");
        let path = file.path().to_string_lossy().to_string();

        let err = read_runner_exec_script(&path).expect_err("oversized script rejected");
        assert!(err.to_string().contains("byte limit"));
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
        assert_eq!(value["variant"], "list");
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
            variant: "env",
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
        assert_eq!(value["variant"], "env");
        assert_eq!(value["source"], "runner_job_env");
        assert_eq!(value["values_redacted"], true);
        assert_eq!(value["env"]["TOKEN"], REDACTED_ENV_VALUE);
    }

    #[test]
    fn runner_env_output_reports_secret_env_refs_without_values() {
        let output = RunnerEnvOutput {
            variant: "env",
            command: "runner.env".to_string(),
            runner_id: "lab".to_string(),
            source: "runner_job_env".to_string(),
            values_redacted: true,
            env: BTreeMap::from([(
                "HOMEBOY_PUBLIC_ARTIFACT_BASE_URL".to_string(),
                REDACTED_ENV_VALUE.to_string(),
            )]),
            secret_env: BTreeMap::from([(
                "OPENAI_API_KEY".to_string(),
                RunnerSecretEnvReferenceOutput {
                    env: Some("OPENAI_API_KEY".to_string()),
                    file: None,
                    secret: None,
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
            REDACTED_ENV_VALUE
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

    #[test]
    fn runner_env_output_reports_secret_store_refs_without_values() {
        let output = RunnerEnvOutput {
            variant: "env",
            command: "runner.env".to_string(),
            runner_id: "lab".to_string(),
            source: "runner_job_env".to_string(),
            values_redacted: true,
            env: BTreeMap::new(),
            secret_env: BTreeMap::from([(
                "HOMEBOY_PREVIEW_TUNNEL_TOKEN".to_string(),
                RunnerSecretEnvReferenceOutput {
                    env: None,
                    file: None,
                    secret: Some("HOMEBOY_PREVIEW_TUNNEL_TOKEN".to_string()),
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
            value["secret_env"]["HOMEBOY_PREVIEW_TUNNEL_TOKEN"]["secret"],
            "HOMEBOY_PREVIEW_TUNNEL_TOKEN"
        );
        assert_eq!(
            value["secret_env"]["HOMEBOY_PREVIEW_TUNNEL_TOKEN"]["values_redacted"],
            true
        );
        assert!(!value.to_string().contains("dummy-secret"));
    }

    #[test]
    fn script_file_prepares_bash_stdin_command() {
        let command = prepare_runner_exec_command(Some(&"echo hi".to_string()), Vec::new())
            .expect("script command");

        assert_eq!(command[0], "bash");
        assert_eq!(command[1], "-c");
        assert!(command[2].contains(RUNNER_EXEC_SCRIPT_ENV));
    }

    #[test]
    fn script_file_rejects_extra_argv() {
        let err =
            prepare_runner_exec_command(Some(&"echo hi".to_string()), vec!["printf".to_string()])
                .expect_err("script plus argv should fail");

        assert!(err
            .to_string()
            .contains("either --script-file or a command"));
    }

    #[test]
    fn env_parser_injects_script_body_without_shell_quoting() {
        let env = prepare_runner_exec_env(
            vec!["GREETING=hello world".to_string()],
            Some("echo \"$GREETING\""),
        )
        .expect("env");

        assert_eq!(env["GREETING"], "hello world");
        assert_eq!(env[RUNNER_EXEC_SCRIPT_ENV], "echo \"$GREETING\"");
    }
}
