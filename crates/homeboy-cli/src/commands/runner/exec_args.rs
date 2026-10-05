use clap::Args;

#[derive(Args)]
pub(super) struct RunnerExecArgs {
    /// Runner ID
    pub(super) id: String,

    /// Remote/current working directory. SSH runners require this to be
    /// inside the runner workspace root unless the runner has a default
    /// workspace_root.
    #[arg(long)]
    pub(super) cwd: Option<String>,

    /// Snapshot a local worktree to the runner first and execute from the materialized remote path.
    #[arg(long = "sync-workspace")]
    pub(super) sync_workspace: Option<String>,

    /// Opaque ref returned by runner workspace sync. Resolves the exact existing snapshot without rematerializing it.
    #[arg(long = "workspace-ref", conflicts_with_all = ["cwd", "sync_workspace"])]
    pub(super) workspace_ref: Option<String>,

    /// Hydrate detected dependencies from a matching runner cache or sealed controller package before execution. This offline-safe mode never invokes a runner package manager.
    #[arg(long)]
    pub(super) hydrate_deps: bool,

    /// Bound --sync-workspace snapshot preparation and transfer before command handoff. Defaults to 240s; does not limit dependency hydration or the runner command itself.
    #[arg(long = "workspace-sync-timeout", default_value = "240s", value_parser = crate::commands::utils::watch::parse_duration_arg, value_name = "DURATION")]
    pub(super) workspace_sync_timeout: std::time::Duration,

    /// Project ID used for runner trust policy checks
    #[arg(long)]
    pub(super) project: Option<String>,

    /// Allow diagnostic-only SSH command execution when the daemon is disconnected or non-fresh; it never uses or rotates daemon admission
    #[arg(long)]
    pub(super) ssh: bool,

    /// Capture the file delta produced by the remote command as a patch artifact
    #[arg(long)]
    pub(super) capture_patch: bool,

    /// Runner-side path that must exist before executing the command. Repeat for multiple paths.
    #[arg(long = "require-path")]
    pub(super) require_paths: Vec<String>,

    /// Read a shell script from this path and execute its materialized runner copy with bash.
    /// Use `-` to read stdin on the controller; it is captured with the same bounded semantics.
    /// Whitespace-only scripts are executed verbatim.
    #[arg(long = "script-file")]
    pub(super) script_file: Option<String>,

    /// Environment variable to inject into the runner process as KEY=VALUE.
    /// Set a value to `homeboy://controller-proxy` to explicitly project the
    /// controller proxy as a credential-free runner-loopback URL.
    /// Repeat for multiple values.
    #[arg(long = "env")]
    pub(super) env: Vec<String>,

    /// Secret environment variable name to resolve through the runner secret-env contract.
    /// Repeat for multiple names.
    #[arg(long = "secret-env", value_name = "NAME")]
    pub(super) secret_env: Vec<String>,

    /// Secret-env plan JSON to apply to the runner process.
    #[arg(long = "secret-env-plan", value_name = "JSON")]
    pub(super) secret_env_plan: Option<String>,

    /// Path to a secret-env plan JSON file to apply to the runner process.
    #[arg(long = "secret-env-plan-file", value_name = "PATH")]
    pub(super) secret_env_plan_file: Option<String>,

    /// Installed extension that contributes runtime environment on the selected runner. Repeat in contribution order.
    #[arg(long = "extension-env", value_name = "ID")]
    pub(super) extension_env_providers: Vec<String>,

    /// Build the runner exec plan without executing it.
    #[arg(long)]
    pub(super) dry_run: bool,

    /// Explicit persisted run id for ad hoc runner exec evidence.
    #[arg(long = "run-id")]
    pub(super) run_id: Option<String>,

    /// File or directory path produced by the runner command to persist as a run artifact.
    /// Relative paths are resolved from the runner exec cwd. Repeat for multiple artifacts.
    #[arg(long = "artifact", value_name = "PATH")]
    pub(super) artifact_outputs: Vec<String>,

    /// Directory whose immediate produced files/directories should each be persisted as run artifacts.
    /// Relative paths are resolved from the runner exec cwd. Repeat for multiple directories.
    #[arg(long = "artifact-dir", value_name = "PATH")]
    pub(super) artifact_dir_outputs: Vec<String>,

    /// Summary file or directory produced by the runner command to persist as typed run evidence.
    /// Relative paths are resolved from the runner exec cwd. Repeat for multiple summaries.
    #[arg(long = "summary", value_name = "PATH")]
    pub(super) summary_outputs: Vec<String>,

    /// Print the full structured runner execution envelope to stdout.
    #[arg(long)]
    pub(super) json: bool,

    /// Print remote stdout/stderr directly instead of the structured JSON envelope.
    /// Use global --output to still write the full structured envelope to a file.
    #[arg(long)]
    pub(super) raw: bool,

    /// Treat this exec as a read-only retrieval of evidence the runner
    /// already retains (for example, hydrating a completed run's artifact).
    /// Routes to the generation that owns the retained run/artifact and
    /// never rotates the shared tunnel, so a stale admission daemon does not
    /// block the read.
    #[arg(long = "read-only-artifact")]
    pub(super) read_only_artifact: bool,

    /// Command and arguments to execute on the runner
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub(super) command: Vec<String>,
}

impl From<RunnerExecArgs> for super::exec::RunnerExecInput {
    fn from(args: RunnerExecArgs) -> Self {
        Self {
            runner_id: args.id,
            command: args.command,
            cwd: args.cwd,
            sync_workspace: args.sync_workspace,
            workspace_ref: args.workspace_ref,
            hydrate_deps: args.hydrate_deps,
            workspace_sync_timeout: args.workspace_sync_timeout,
            project_id: args.project,
            allow_diagnostic_ssh: args.ssh,
            capture_patch: args.capture_patch,
            require_paths: args.require_paths,
            script_file: args.script_file,
            env: args.env,
            secret_env: args.secret_env,
            secret_env_plan: args.secret_env_plan,
            secret_env_plan_file: args.secret_env_plan_file,
            dry_run: args.dry_run,
            run_id: args.run_id,
            artifact_outputs: args.artifact_outputs,
            artifact_dir_outputs: args.artifact_dir_outputs,
            summary_outputs: args.summary_outputs,
            read_only_artifact: args.read_only_artifact,
            raw: args.raw,
            extension_env_providers: args.extension_env_providers,
        }
    }
}
