use clap::{Args, Subcommand, ValueEnum};

use homeboy_runner_contract::RunnerKind;

use super::super::DynamicSetArgs;
use super::doctor;
use super::exec_args::RunnerExecArgs;
use super::lifecycle;
use super::refresh_plan;
use super::workspace;
use crate::commands::utils::args::MutationArgs;

#[derive(Args)]
pub struct RunnerArgs {
    #[command(subcommand)]
    pub(super) command: RunnerCommand,
}

impl RunnerArgs {
    pub(crate) fn compact_doctor_stdout(&self) -> bool {
        matches!(&self.command, RunnerCommand::Doctor { full: false, .. })
    }
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

        /// Seconds the controller waits for an accepted runner job; zero detaches immediately
        #[arg(long)]
        runner_exec_wait_timeout_secs: Option<u64>,

        /// Cancel an in-flight remote job when the controller wait expires
        #[arg(long)]
        cancel_on_wait_timeout: Option<bool>,

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

        /// Seconds the controller waits for an accepted runner job; zero detaches immediately
        #[arg(long)]
        runner_exec_wait_timeout_secs: Option<u64>,

        /// Cancel an in-flight remote job when the controller wait expires
        #[arg(long)]
        cancel_on_wait_timeout: Option<bool>,

        /// Artifact retention/copying policy label for future execution commands
        #[arg(long)]
        artifact_policy: Option<String>,
    },
    /// List all configured runners
    List {
        /// Include full runner configuration, sessions, and environment.
        ///
        /// The default is a compact row per runner. A long-lived controller's
        /// full listing carries every configured environment, serialized
        /// settings, and dev-sync extension metadata — which truncates in agent
        /// and terminal output and buries the answer (#9487).
        #[arg(long)]
        full: bool,
    },
    /// Display runner configuration
    Show {
        /// Runner ID
        id: String,
    },
    /// Modify runner settings
    #[command(
        after_help = r#"Objects merge by key; arrays supplied at the top level of a single patch
replace by default. Use --replace <FIELD> for nested arrays. For env only,
--replace env replaces the configured map with the supplied env object.

Delete an environment key by setting its value to null:
  homeboy runner set <id> --json '{"env":{"OLD_MODE":null}}'

Replace or clear the whole environment map:
  homeboy runner set <id> --replace env --json '{"env":{"APP_MODE":"production"}}'
  homeboy runner set <id> --replace env --json '{"env":{}}'"#
    )]
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

        /// Explicitly allow controller-driven Homeboy binary convergence
        #[arg(long)]
        allow_homeboy_convergence: Option<bool>,

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

        /// Explicitly allow controller-driven Homeboy binary convergence
        #[arg(long)]
        allow_homeboy_convergence: Option<bool>,
    },
    /// Remove a runner configuration
    Remove {
        /// Runner ID
        id: String,
    },
    /// Diagnose a local or configured SSH runner without mutating it
    Doctor {
        #[arg(long)]
        reason: Option<String>,
        /// Runner ID. Use `local`, `localhost`, or `self` for this machine;
        /// other values resolve through `homeboy runner` configuration.
        /// Omit to select the only configured non-local runner.
        runner_id: Option<String>,

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

        /// Safely repair issues in the selected scope, such as reconnecting a stale Lab daemon. Without --scope, repairs use lab-offload.
        #[arg(long)]
        repair: bool,

        /// Return complete, redacted probe evidence and resource maps.
        #[arg(long)]
        full: bool,
    },
    /// Evaluate workload placement without creating a run, rig lease, runner job, or connection
    Preflight {
        /// Complete typed PlacementReadinessRequest JSON. It accepts only
        /// compiler-recognised invocations and never executable probe text.
        #[arg(long)]
        request: String,
    },
    /// Connect to a runner by starting a loopback-only remote daemon and SSH tunnel
    Connect {
        #[arg(long)]
        reason: Option<String>,
        /// Runner ID. Omit to select the only configured non-local runner.
        id: Option<String>,

        /// Operator-confirm a live lease/PID/build adoption within the trusted remote SSH UID boundary; never stops or replaces a daemon
        #[arg(long)]
        adopt_live_lease: Option<String>,

        /// Current remote daemon PID paired with --adopt-live-lease
        #[arg(long)]
        expected_live_pid: Option<u32>,
    },
    /// Disconnect and reconnect a runner using the lease-safe lifecycle
    Restart {
        id: Option<String>,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Bounded disk usage inventory of Homeboy-owned roots (two-second scan).
    Inventory { id: String },
    /// Show persisted runner tunnel status
    Status {
        /// Runner ID. Omit to select the only non-local runner, or show all session states otherwise.
        id: Option<String>,

        /// Return complete status, runtime diagnostics, and followups.
        #[arg(long)]
        full: bool,
    },
    /// Inspect and safely remove persisted peer sessions whose local tunnels are proven dead
    PeerSessions {
        /// Runner ID
        id: String,

        /// Continue after this cursor returned by a prior peer-session command
        #[arg(long)]
        cursor: Option<String>,

        /// Remove only peer-session snapshots proven dead during this inspection
        #[arg(long)]
        apply: bool,
    },
    /// Reconcile one runner's current daemon session and settle its terminal
    /// jobs. Success means that runner accepts jobs; durable agent-task
    /// records and observation runs have separate reconcilers.
    Reconcile {
        /// Runner ID
        id: String,

        /// Attest that the workload processes of the runner's dead daemon
        /// generation were inspected and are absent. When the reconcile is
        /// blocked by jobs whose dead daemon recorded no child identity, this
        /// runs `daemon recover --attest-absent <job-id>...` on the runner —
        /// bound to the state directory and exact jobs its lifecycle plan
        /// names — and then re-runs the reconcile. A recorded child, a live
        /// generation, or an out-of-scope job is still refused, with the
        /// runner-side refusal surfaced verbatim (#15556, #17429).
        #[arg(long)]
        attest_absent: bool,
    },
    /// Close a runner tunnel and remove its persisted session state
    Disconnect {
        #[arg(long)]
        reason: Option<String>,
        /// Runner ID. Omit to select the only configured non-local runner.
        id: Option<String>,

        /// Retire only this controller's matching local tunnel/session state after a read-only SSH probe proves zero active jobs; it never stops the remote daemon
        #[arg(long)]
        local_recovery: bool,
    },
    /// Build or select the Homeboy binary used for runner/Lab jobs
    RefreshHomeboy {
        /// Runner ID
        runner_id: String,

        /// Existing runner-side Homeboy binary to select instead of building one
        #[arg(long)]
        select: Option<String>,

        /// Official Homeboy release tag to install (defaults to this controller's release)
        #[arg(long = "ref")]
        git_ref: Option<String>,

        /// Permit replacing a newer managed runner build with an older Git revision
        #[arg(long)]
        allow_downgrade: bool,

        /// Print the plan without executing it or changing runner config
        #[arg(long)]
        dry_run: bool,

        /// Emit the complete refresh result. The default is a bounded summary;
        /// durable plan and build-log artifacts remain available from its run ref.
        #[arg(long)]
        full: bool,
    },
    /// Sync a controller-local Homeboy dev binary to the runner and select it for Lab jobs
    DevSync {
        /// Runner ID
        runner_id: String,

        /// Controller-local Homeboy source checkout to build before upload. Defaults to current directory.
        #[arg(long)]
        homeboy_source: Option<String>,

        /// Controller-local prebuilt Homeboy binary to upload instead of building from source
        #[arg(long)]
        homeboy_binary: Option<String>,

        /// Dev extension source to sync later, in id=path form. Accepted and recorded; extension relink is deferred.
        #[arg(long = "extensions")]
        extensions: Vec<String>,

        /// Disconnect and reconnect the runner daemon after selecting the dev binary
        #[arg(long)]
        reconnect: bool,

        /// Print the plan without executing it or changing runner config
        #[arg(long)]
        dry_run: bool,
    },
    /// Inventory or remove stale managed Homeboy binary slots on a runner
    CachePrune {
        /// Runner ID
        runner_id: String,

        // Delete eligible slots. Omit for inventory only.
        // Shared plan-default mutation group (#11139).
        #[command(flatten)]
        mutation: MutationArgs,

        /// Minimum slot age before an unselected slot is eligible.
        /// Defaults to the shared runner age floor
        /// (`cleanup::RUNNER_MIN_AGE_HOURS`).
        #[arg(long)]
        min_age_hours: Option<u64>,
    },
    /// Execute a command on a configured runner. Use `homeboy runner exec [HOMEBOY_OPTIONS] <RUNNER> -- <COMMAND>...`.
    #[command(
        after_help = "Use `homeboy runner exec [HOMEBOY_OPTIONS] <RUNNER> -- <COMMAND>...` to make the Homeboy/remote-command boundary explicit."
    )]
    Exec(RunnerExecArgs),
    /// Hold new admission on the whole runner host until five-minute load per
    /// logical CPU is below the threshold.
    Reserve {
        id: String,
        /// Quiet threshold: five-minute load average per logical CPU (1.0 = full).
        #[arg(long, required_unless_present = "release")]
        quiet_below: Option<f64>,
        /// Measurement window length, in minutes.
        #[arg(long, required_unless_present = "release")]
        window: Option<u64>,
        /// Minutes to wait for quiet before giving up (default 30).
        #[arg(long)]
        wait_timeout: Option<u64>,
        /// Free-text reason shown in runner status.
        #[arg(long)]
        reason: Option<String>,
        /// Release the reservation with this id.
        #[arg(long, conflicts_with_all = ["quiet_below", "window", "wait_timeout", "reason"])]
        release: Option<String>,
    },
    /// Execute an extension-owned recipe provider in one materialized workspace.
    RecipeRun {
        /// Runner ID
        runner_id: String,
        /// Stable extension-owned recipe execution provider ID
        #[arg(long)]
        provider: String,
        /// Controller-local workspace to snapshot once before execution
        #[arg(long = "sync-workspace")]
        sync_workspace: String,
        /// Recipe path relative to the materialized workspace
        #[arg(long)]
        recipe: String,
        /// Artifact directory relative to the materialized workspace
        #[arg(long)]
        artifacts: String,
        /// Durable run identity that receives execution evidence
        #[arg(long = "run-id")]
        run_id: String,
    },
    /// List installed extension-owned recipe-run providers.
    RecipeProviders,
    /// Show the runner daemon's own lifecycle plan (read-only).
    ///
    /// Runs `homeboy daemon plan` in the runner's daemon frame: the same
    /// planner `daemon recover` follows locally (#15557).
    Plan {
        /// Runner ID
        id: String,
    },
    /// Show the effective environment injected into runner jobs
    Env {
        /// Runner ID
        id: String,
    },
    /// Evaluate runner workspace lifecycle and finalization readiness without mutating state
    Lifecycle {
        /// Runner ID that owns the workspace
        runner_id: String,

        /// Absolute runner-side workspace path
        #[arg(long)]
        workspace: String,

        /// Runner daemon or broker job ID associated with this workspace
        #[arg(long)]
        job_id: Option<String>,

        /// Durable run ID associated with this workspace
        #[arg(long)]
        run_id: Option<String>,

        /// Canonical lifecycle status. When omitted, --exit-code maps 0 to succeeded and non-zero to failed.
        #[arg(long, value_enum)]
        status: Option<lifecycle::RunnerLifecycleStatusArg>,

        /// Process exit code to project into lifecycle status and RunOutcomeEnvelope fields
        #[arg(long)]
        exit_code: Option<i32>,
    },
    /// Inspect or follow a runner daemon job stream
    Job {
        #[command(subcommand)]
        command: RunnerJobCommand,
    },
    /// Materialize local workspaces on a configured runner
    Workspace {
        #[command(subcommand)]
        command: workspace::RunnerWorkspaceCommand,
    },
    /// Plan a runner-backed refresh loop before dispatching matrix-style work
    #[command(name = "refresh-plan")]
    RefreshPlan(refresh_plan::RefreshPlanArgs),
    /// Manage runner broker authentication and pairing
    Broker {
        #[command(subcommand)]
        command: RunnerBrokerCommand,
    },
    /// Manage the runner-owned daemon service (a systemd user unit)
    Service {
        #[command(subcommand)]
        command: RunnerServiceCommand,
    },
}

#[derive(Subcommand)]
pub(super) enum RunnerServiceCommand {
    /// Install the runner daemon as a systemd user unit and hand the daemon to it.
    /// The runner must be idle. Afterwards `runner connect` only attaches.
    Install {
        /// Runner ID
        runner_id: String,
    },
    /// Show the runner service and the daemon lease it holds
    Status {
        /// Runner ID
        runner_id: String,
    },
    /// Run on the runner host by a controller's repoint: this binary writes
    /// its own units, migrates an older layout, and restarts the service.
    #[command(hide = true)]
    Repoint {
        /// Binary the unit executes
        #[arg(long)]
        binary: String,
        /// Daemon startup token the controller expects
        #[arg(long)]
        startup_token: String,
        /// Allowlisted daemon policy env, as KEY=VALUE
        #[arg(long = "env")]
        env: Vec<String>,
    },
}

#[derive(Subcommand)]
pub(super) enum RunnerBrokerCommand {
    /// Pair a controller with a runner, minting a one-time submit-scoped bearer token
    Pair {
        /// Stable credential id used for later revocation
        id: String,

        /// Runner id this credential authorizes (requests must match it)
        #[arg(long)]
        runner_id: String,

        /// Store only on this controller; skip installing broker_auth.json on an SSH runner host
        #[arg(long)]
        no_install: bool,
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
    /// List live daemon jobs
    List {
        /// Runner ID
        runner_id: String,

        /// Include only running jobs
        #[arg(long, conflicts_with_all = ["queued", "terminal"])]
        active: bool,

        /// Include only queued jobs
        #[arg(long, conflicts_with_all = ["active", "terminal"])]
        queued: bool,

        /// Include only observed terminal jobs
        #[arg(long, conflicts_with_all = ["active", "queued"])]
        terminal: bool,

        /// Include observed terminal jobs
        #[arg(long)]
        all: bool,

        /// Emit the complete structured JSON response instead of the compact table
        #[arg(long)]
        json: bool,

        /// Match a job ID, durable run ID, or command summary
        #[arg(long)]
        correlation: Option<String>,
    },
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

        /// Resume after this previously displayed event sequence
        #[arg(long)]
        cursor: Option<u64>,

        /// Emit one structured document without human event rendering
        #[arg(long)]
        json: bool,

        /// Return only lifecycle events, exit code, and a bounded stdout/stderr tail
        #[arg(long)]
        compact: bool,

        /// Bound embedded stdout/stderr to the last N kilobytes, surfaced as a tail
        #[arg(long = "tail", value_name = "KB")]
        tail_kb: Option<usize>,
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
pub(super) enum RunnerKindArg {
    Local,
    Ssh,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(super) enum RunnerDoctorScopeArg {
    General,
    LabOffload,
    SecretEnv,
}

impl From<RunnerDoctorScopeArg> for doctor::RunnerDoctorScope {
    fn from(value: RunnerDoctorScopeArg) -> Self {
        match value {
            RunnerDoctorScopeArg::General => doctor::RunnerDoctorScope::General,
            RunnerDoctorScopeArg::LabOffload => doctor::RunnerDoctorScope::LabOffload,
            RunnerDoctorScopeArg::SecretEnv => doctor::RunnerDoctorScope::SecretEnv,
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

#[cfg(test)]
mod tests {
    use crate::cli_surface::Cli;
    use clap::Parser;

    #[test]
    fn runner_cli_rejects_conflicting_selectors() {
        let workspace_ref = "workspace:00000000-0000-0000-0000-000000000001";
        assert!(Cli::try_parse_from([
            "homeboy",
            "runner",
            "exec",
            "--workspace-ref",
            workspace_ref,
            "--hydrate-deps",
            "lab",
            "--",
            "true",
        ])
        .is_ok());
        assert!(
            Cli::try_parse_from(["homeboy", "runner", "job", "list", "lab", "--active"]).is_ok()
        );

        for conflicting in [
            vec![
                "exec",
                "--workspace-ref",
                workspace_ref,
                "--cwd",
                "/runner/path",
                "lab",
                "--",
                "true",
            ],
            vec![
                "exec",
                "--workspace-ref",
                workspace_ref,
                "--sync-workspace",
                "/local/path",
                "lab",
                "--",
                "true",
            ],
            vec!["job", "list", "lab", "--active", "--queued"],
        ] {
            let argv = ["homeboy", "runner"].into_iter().chain(conflicting.clone());
            assert!(
                Cli::try_parse_from(argv).is_err(),
                "{conflicting:?} must be rejected"
            );
        }
    }
}
