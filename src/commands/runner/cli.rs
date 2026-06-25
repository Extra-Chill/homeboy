use clap::{Args, Subcommand, ValueEnum};

use homeboy::core::runners::RunnerKind;

use super::super::DynamicSetArgs;
use super::doctor;
use super::workspace;

#[derive(Args)]
pub struct RunnerArgs {
    #[command(subcommand)]
    pub(super) command: RunnerCommand,
}

#[derive(Subcommand)]
pub(super) enum RunnerCommand {
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

        /// Explicit persisted run id for ad hoc runner exec evidence.
        #[arg(long = "run-id")]
        run_id: Option<String>,

        /// File or directory path produced by the runner command to persist as a run artifact.
        /// Relative paths are resolved from the runner exec cwd. Repeat for multiple artifacts.
        #[arg(long = "artifact", value_name = "PATH")]
        artifact_outputs: Vec<String>,

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
pub(super) enum RunnerBrokerCommand {
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
pub(super) enum RunnerJobCommand {
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
    /// Reconcile expired reverse-runner broker claims
    Reconcile {
        /// Reverse-connected runner ID
        runner_id: String,
    },
    /// Inspect broker-held reverse-runner artifact metadata
    Artifacts {
        /// Reverse-connected runner ID
        runner_id: String,

        /// Reverse broker job ID
        job_id: String,

        /// Artifact ID reported by the finished broker job
        artifact_id: String,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(super) enum RunnerKindArg {
    Local,
    Ssh,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(super) enum RunnerDoctorScopeArg {
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
