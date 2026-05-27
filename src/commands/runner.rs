use std::collections::HashMap;

use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;
use serde_json::Value;

use homeboy::core::runner::{
    self, ReverseRunnerConnectOptions, Runner, RunnerConnectReport, RunnerDisconnectReport,
    RunnerExecOutput, RunnerKind, RunnerStatusReport, RunnerWorkspaceApplyOutput,
    RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOutput,
};
use homeboy::core::server::RunnerSettings;
use homeboy::core::{EntityCrudOutput, MergeOutput};

use super::{CmdResult, DynamicSetArgs};

pub mod doctor;

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

#[derive(Debug, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum RunnerWorkspaceOutput {
    Sync(RunnerWorkspaceSyncOutput),
    Apply(RunnerWorkspaceApplyOutput),
}

pub type RunnerOutput = EntityCrudOutput<Runner, RunnerExtra>;

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum RunnerCommandOutput {
    Registry(RunnerOutput),
    Doctor(doctor::RunnerDoctorOutput),
    Execution(RunnerExecOutput),
    Workspace(RunnerWorkspaceOutput),
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
    /// Migrate a legacy standalone SSH runner onto its referenced server
    Migrate {
        /// Legacy standalone SSH runner ID, for example `lab`
        runner_id: String,

        /// Delete the old standalone runner after copying it to the server
        #[arg(long)]
        remove_legacy: bool,
    },
    /// List all configured runners
    List,
    /// Display runner configuration
    Show {
        /// Runner ID
        id: String,
    },
    /// Modify runner settings
    #[command(visible_aliases = ["edit", "merge"])]
    Set {
        #[command(flatten)]
        args: DynamicSetArgs,
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

        /// Allow explicit SSH command execution when no daemon session is connected
        #[arg(long)]
        ssh: bool,

        /// Capture the file delta produced by the remote command as a patch artifact
        #[arg(long)]
        capture_patch: bool,

        /// Command and arguments to execute on the runner
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Materialize local workspaces on a configured runner
    Workspace {
        #[command(subcommand)]
        command: RunnerWorkspaceCommand,
    },
}

#[derive(Subcommand)]
enum RunnerWorkspaceCommand {
    /// Sync a local worktree snapshot into the runner workspace root
    Sync {
        /// Runner ID
        runner_id: String,

        /// Local worktree path to materialize for Lab execution
        #[arg(long)]
        path: String,

        /// Sync mode. snapshot includes dirty local files; git requires a clean tree and clones/checks out HEAD remotely.
        #[arg(long, value_enum, default_value_t = RunnerWorkspaceSyncModeArg::Snapshot)]
        mode: RunnerWorkspaceSyncModeArg,
    },
    /// Apply a Lab-generated patch/delta back to its local source worktree
    Apply {
        /// Lab apply JSON artifact path
        input: String,

        /// Apply even when the local worktree snapshot no longer matches the Lab source snapshot
        #[arg(long)]
        force: bool,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum RunnerKindArg {
    Local,
    Ssh,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum RunnerWorkspaceSyncModeArg {
    #[default]
    Snapshot,
    Git,
}

impl From<RunnerWorkspaceSyncModeArg> for RunnerWorkspaceSyncMode {
    fn from(value: RunnerWorkspaceSyncModeArg) -> Self {
        match value {
            RunnerWorkspaceSyncModeArg::Snapshot => RunnerWorkspaceSyncMode::Snapshot,
            RunnerWorkspaceSyncModeArg::Git => RunnerWorkspaceSyncMode::Git,
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
        RunnerCommand::Migrate {
            runner_id,
            remove_legacy,
        } => map_registry({
            let runner = runner::migrate_standalone_ssh_runner(&runner_id, remove_legacy)?;
            let mut deleted = Vec::new();
            let hint = if remove_legacy {
                deleted.push(runner_id.to_string());
                Some(format!(
                    "Legacy runner '{runner_id}' was removed. Use runner ID '{}' for this Lab server.",
                    runner.id
                ))
            } else {
                Some(format!(
                    "Legacy runner '{runner_id}' was preserved. Re-run with --remove-legacy after verifying runner '{}' works.",
                    runner.id
                ))
            };

            Ok((
                RunnerOutput {
                    command: "runner.migrate".to_string(),
                    id: Some(runner.id.clone()),
                    entity: Some(runner),
                    updated_fields: vec!["server.runner".to_string()],
                    deleted,
                    hint,
                    ..Default::default()
                },
                0,
            ))
        }),
        RunnerCommand::List => map_registry(list()),
        RunnerCommand::Show { id } => map_registry(show(&id)),
        RunnerCommand::Set { args } => map_registry(set(args)),
        RunnerCommand::Remove { id } => map_registry(remove(&id)),
        RunnerCommand::Doctor {
            runner_id,
            path,
            required_extensions,
        } => map_doctor(doctor::run_with_options(
            &runner_id,
            doctor::RunnerDoctorOptions {
                path,
                extensions: required_extensions,
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
            ssh,
            capture_patch,
            command,
        } => map_execution(exec(&id, cwd, ssh, capture_patch, command)),
        RunnerCommand::Workspace { command } => match command {
            RunnerWorkspaceCommand::Sync {
                runner_id,
                path,
                mode,
            } => map_workspace(workspace_sync(&runner_id, path, mode)),
            RunnerWorkspaceCommand::Apply { input, force } => map_workspace_apply(
                runner::apply_workspace_patch(runner::RunnerWorkspaceApplyOptions { input, force }),
            ),
        },
    }
}

fn map_registry(result: CmdResult<RunnerOutput>) -> CmdResult<RunnerCommandOutput> {
    result.map(|(output, exit_code)| (RunnerCommandOutput::Registry(output), exit_code))
}

fn map_doctor(result: CmdResult<doctor::RunnerDoctorOutput>) -> CmdResult<RunnerCommandOutput> {
    result.map(|(output, exit_code)| (RunnerCommandOutput::Doctor(output), exit_code))
}

fn map_execution(result: CmdResult<RunnerExecOutput>) -> CmdResult<RunnerCommandOutput> {
    result.map(|(output, exit_code)| (RunnerCommandOutput::Execution(output), exit_code))
}

fn map_workspace(result: CmdResult<RunnerWorkspaceSyncOutput>) -> CmdResult<RunnerCommandOutput> {
    result.map(|(output, exit_code)| {
        (
            RunnerCommandOutput::Workspace(RunnerWorkspaceOutput::Sync(output)),
            exit_code,
        )
    })
}

fn map_workspace_apply(
    result: CmdResult<RunnerWorkspaceApplyOutput>,
) -> CmdResult<RunnerCommandOutput> {
    result.map(|(output, exit_code)| {
        (
            RunnerCommandOutput::Workspace(RunnerWorkspaceOutput::Apply(output)),
            exit_code,
        )
    })
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
            resources: HashMap::<String, Value>::new(),
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
            "Provide JSON spec, --json flag, --base64 flag, or --key value flags",
            None,
            None,
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
        return Ok((
            RunnerOutput {
                command: "runner.status".to_string(),
                id: Some(id.to_string()),
                extra: RunnerExtra {
                    connection: Some(RunnerConnectionOutput::Status(runner::status(id)?)),
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
                sessions: runner::statuses()?,
                ..Default::default()
            },
            ..Default::default()
        },
        0,
    ))
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
    allow_ssh: bool,
    capture_patch: bool,
    command: Vec<String>,
) -> CmdResult<RunnerExecOutput> {
    runner::exec(
        runner_id,
        runner::RunnerExecOptions {
            cwd,
            allow_ssh,
            command,
            env: Default::default(),
            capture_patch,
            source_snapshot: None,
            capability_preflight: None,
        },
    )
}

fn workspace_sync(
    runner_id: &str,
    path: String,
    mode: RunnerWorkspaceSyncModeArg,
) -> CmdResult<RunnerWorkspaceSyncOutput> {
    runner::sync_workspace(
        runner_id,
        runner::RunnerWorkspaceSyncOptions {
            path,
            mode: RunnerWorkspaceSyncMode::from(mode),
            changed_since_base: None,
        },
    )
}
