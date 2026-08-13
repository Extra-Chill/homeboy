use clap::{Args, Subcommand, ValueEnum};

use homeboy::runner::runners::RunnerKind;

use super::super::DynamicSetArgs;
use super::doctor;
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
    pub fn compact_exec_stdout(&self) -> bool {
        matches!(
            &self.command,
            RunnerCommand::Exec {
                json: false,
                raw: false,
                ..
            }
        )
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
    List {
        /// Include full runner configuration, sessions, and environment.
        ///
        /// The default is a compact row per runner. A long-lived controller's
        /// full listing carries every configured environment, serialized
        /// settings, dev-sync extension metadata, and every historical draining
        /// generation — which truncates in agent and terminal output and buries
        /// the answer (#9487).
        #[arg(long)]
        full: bool,
    },
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
    /// Evaluate workload placement without creating a run, rig lease, runner job, or connection
    Preflight {
        /// Complete typed PlacementReadinessRequest JSON. It accepts only
        /// compiler-recognised invocations and never executable probe text.
        #[arg(long)]
        request: String,
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

        /// Explicitly adopt this exact remote daemon lease after confirming its PID is dead
        #[arg(long)]
        adopt_orphan_lease: Option<String>,

        /// Deprecated no-op retained for one release; the runner proves the recorded PID dead itself
        #[arg(long)]
        confirm_pid_dead: bool,

        /// Operator-confirm a live lease/PID/build adoption within the trusted remote SSH UID boundary; never stops or replaces a daemon
        #[arg(long)]
        adopt_live_lease: Option<String>,

        /// Current remote daemon PID paired with --adopt-live-lease
        #[arg(long)]
        expected_live_pid: Option<u32>,

        /// Confirm one exact unresolved job has no live untracked child; repeat for each job
        #[arg(long = "confirm-untracked-child-dead")]
        confirm_untracked_child_dead: Vec<uuid::Uuid>,

        /// Explicitly reconcile active jobs after proving the missing-lease remote store has no daemon owner
        #[arg(long)]
        reconcile_leaseless_orphans: bool,

        /// Deprecated no-op retained for one release; the runner fails closed on owner-lock, process, and listener probes
        #[arg(long)]
        confirm_no_daemon_owner: bool,

        /// Recover this exact lease after the remote daemon state record was lost
        #[arg(long)]
        recover_missing_lease_state: Option<String>,

        /// Recorded remote daemon PID paired with --recover-missing-lease-state
        #[arg(long)]
        recorded_pid: Option<u32>,

        /// Recorded concrete remote daemon endpoint paired with --recover-missing-lease-state
        #[arg(long)]
        recorded_endpoint: Option<String>,

        /// Deprecated no-op retained for one release; the runner probes its own state record and endpoint
        #[arg(long)]
        confirm_control_plane_lost: bool,
    },
    /// Show persisted runner tunnel status
    Status {
        /// Runner ID. Omit to show all runner session states.
        id: Option<String>,

        /// Include the full historical draining-generation inventory. By
        /// default status leads with the compact authoritative admission
        /// summary and omits the expanded per-generation ledger, which can run
        /// to thousands of lines on a long-lived runner.
        #[arg(long)]
        generations: bool,

        /// Return complete status, runtime diagnostics, followups, and generation detail.
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
    /// Reconcile one runner's persisted daemon generations and retire verified
    /// drained daemons. Success means that runner accepts jobs with no
    /// unresolved generation projection; durable agent-task records and
    /// observation runs have separate reconcilers.
    Reconcile {
        /// Runner ID
        id: String,
    },
    /// Close a runner tunnel and remove its persisted session state
    Disconnect {
        /// Runner ID
        id: String,

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

        /// Git remote URL to clone/fetch when materializing a managed Homeboy binary
        #[arg(long)]
        source: Option<String>,

        /// Git ref to materialize from the source remote
        #[arg(long = "ref")]
        git_ref: Option<String>,

        /// Runner-side checkout directory for the managed Homeboy source
        #[arg(long)]
        target_dir: Option<String>,

        /// Disconnect and reconnect the runner daemon after updating homeboy_path
        #[arg(long)]
        reconnect: bool,

        /// Interrupt active daemon jobs when reconnecting
        #[arg(long)]
        force: bool,

        /// Permit replacing a newer managed runner build with an older Git revision
        #[arg(long)]
        allow_downgrade: bool,

        /// Print the plan without executing it or changing runner config
        #[arg(long)]
        dry_run: bool,
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
    Exec {
        /// Runner ID
        id: String,

        /// Remote/current working directory. SSH runners require this to be
        /// inside the runner workspace root unless the runner has a default
        /// workspace_root.
        #[arg(long)]
        cwd: Option<String>,

        /// Snapshot a local worktree to the runner first and execute from the materialized remote path.
        #[arg(long = "sync-workspace")]
        sync_workspace: Option<String>,

        /// Hydrate detected dependencies from a matching runner cache or sealed controller package before execution. This offline-safe mode never invokes a runner package manager.
        #[arg(long)]
        hydrate_deps: bool,

        /// Project ID used for runner trust policy checks
        #[arg(long)]
        project: Option<String>,

        /// Allow diagnostic-only SSH command execution when the daemon is disconnected or non-fresh; it never uses or rotates daemon admission
        #[arg(long)]
        ssh: bool,

        /// Capture the file delta produced by the remote command as a patch artifact
        #[arg(long)]
        capture_patch: bool,

        /// Runner-side path that must exist before executing the command. Repeat for multiple paths.
        #[arg(long = "require-path")]
        require_paths: Vec<String>,

        /// Read a shell script from this path and execute its materialized runner copy with bash.
        /// Use `-` to read stdin on the controller; it is captured with the same bounded semantics.
        /// Whitespace-only scripts are executed verbatim.
        #[arg(long = "script-file")]
        script_file: Option<String>,

        /// Environment variable to inject into the runner process as KEY=VALUE.
        /// Set a value to `homeboy://controller-proxy` to explicitly project the
        /// controller proxy as a credential-free runner-loopback URL.
        /// Repeat for multiple values.
        #[arg(long = "env")]
        env: Vec<String>,

        /// Secret environment variable name to resolve through the runner secret-env contract.
        /// Repeat for multiple names.
        #[arg(long = "secret-env", value_name = "NAME")]
        secret_env: Vec<String>,

        /// Secret-env plan JSON to apply to the runner process.
        #[arg(long = "secret-env-plan", value_name = "JSON")]
        secret_env_plan: Option<String>,

        /// Path to a secret-env plan JSON file to apply to the runner process.
        #[arg(long = "secret-env-plan-file", value_name = "PATH")]
        secret_env_plan_file: Option<String>,

        /// Installed extension that contributes runtime environment on the selected runner. Repeat in contribution order.
        #[arg(long = "extension-env", value_name = "ID")]
        extension_env_providers: Vec<String>,

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

        /// Directory whose immediate produced files/directories should each be persisted as run artifacts.
        /// Relative paths are resolved from the runner exec cwd. Repeat for multiple directories.
        #[arg(long = "artifact-dir", value_name = "PATH")]
        artifact_dir_outputs: Vec<String>,

        /// Summary file or directory produced by the runner command to persist as typed run evidence.
        /// Relative paths are resolved from the runner exec cwd. Repeat for multiple summaries.
        #[arg(long = "summary", value_name = "PATH")]
        summary_outputs: Vec<String>,

        /// Print the full structured runner execution envelope to stdout.
        #[arg(long)]
        json: bool,

        /// Print remote stdout/stderr directly instead of the structured JSON envelope.
        /// Use global --output to still write the full structured envelope to a file.
        #[arg(long)]
        raw: bool,

        /// Treat this exec as a read-only retrieval of evidence the runner
        /// already retains (for example, hydrating a completed run's artifact).
        /// Routes to the generation that owns the retained run/artifact and
        /// never rotates the shared tunnel, so a stale admission daemon does not
        /// block the read.
        #[arg(long = "read-only-artifact")]
        read_only_artifact: bool,

        /// Command and arguments to execute on the runner
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
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
    /// Plan a runner-backed refresh loop before dispatching matrix-style work
    #[command(name = "refresh-plan")]
    RefreshPlan(refresh_plan::RefreshPlanArgs),
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
    /// List live daemon jobs and retained durable job projections
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

        /// Include only jobs owned by this daemon generation
        #[arg(long)]
        generation: Option<String>,

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
    use super::*;
    use crate::cli_surface::{Cli, Commands};
    use clap::Parser;

    #[test]
    fn preflight_accepts_only_the_request_json_flag() {
        let cli = Cli::try_parse_from([
            "homeboy",
            "runner",
            "preflight",
            "--request",
            r#"{"schema":"homeboy/placement-readiness/v2","runner_id":"lab","allow_queue":false,"durable_workload":false,"invocation":{"kind":"capability_audit","source_path":"/workspace","capability_id":"capability.alpha"}}"#,
        ])
        .expect("parse typed preflight request");
        assert!(matches!(
            cli.command,
            Commands::Runner(RunnerArgs {
                command: RunnerCommand::Preflight { .. }
            })
        ));
        assert!(
            Cli::try_parse_from(["homeboy", "runner", "preflight", "--required-tool", "node"])
                .is_err()
        );
        assert!(Cli::try_parse_from([
            "homeboy",
            "runner",
            "preflight",
            "--request",
            "{}",
            "--unknown"
        ])
        .is_err());
    }

    #[test]
    fn runner_job_list_accepts_recovery_filters_and_rejects_conflicting_states() {
        let cli = Cli::try_parse_from([
            "homeboy",
            "runner",
            "job",
            "list",
            "lab",
            "--active",
            "--generation",
            "generation-a",
            "--correlation",
            "run-11770",
        ])
        .expect("parse runner job list filters");
        assert!(matches!(
            cli.command,
            Commands::Runner(RunnerArgs {
                command: RunnerCommand::Job {
                    command: RunnerJobCommand::List {
                        active: true,
                        queued: false,
                        terminal: false,
                        ..
                    }
                }
            })
        ));
        assert!(Cli::try_parse_from([
            "homeboy", "runner", "job", "list", "lab", "--active", "--queued"
        ])
        .is_err());
    }

    #[test]
    fn recipe_run_preserves_required_execution_arguments_and_recipe_providers_is_a_sibling() {
        let cli = Cli::try_parse_from([
            "homeboy",
            "runner",
            "recipe-run",
            "local",
            "--provider",
            "fixture.run",
            "--sync-workspace",
            "/workspace",
            "--recipe",
            "recipe.json",
            "--artifacts",
            "artifacts",
            "--run-id",
            "run-12157",
        ])
        .expect("existing recipe-run invocation parses");
        assert!(matches!(
            cli.command,
            Commands::Runner(RunnerArgs {
                command: RunnerCommand::RecipeRun {
                    runner_id,
                    provider,
                    ..
                }
            }) if runner_id == "local" && provider == "fixture.run"
        ));

        let cli = Cli::try_parse_from(["homeboy", "runner", "recipe-providers"])
            .expect("provider inventory parses");
        assert!(matches!(
            cli.command,
            Commands::Runner(RunnerArgs {
                command: RunnerCommand::RecipeProviders
            })
        ));

        let error = match Cli::try_parse_from(["homeboy", "runner", "recipe-run", "local"]) {
            Ok(_) => panic!("normal recipe-run remains required"),
            Err(error) => error,
        };
        let rendered = error.to_string();
        assert!(rendered.contains("--provider"));
        assert!(!rendered.contains("--runner_id"));
    }
}
