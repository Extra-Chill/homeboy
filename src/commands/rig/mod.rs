//! `homeboy rig` command — CLI surface for the rig primitive.

mod output;
mod sources;

pub use output::RigCommandOutput;

use clap::{Args, Subcommand};

use homeboy::core::rig;

use self::output::{
    RigAppOutput, RigCheckOutput, RigDownOutput, RigInstallOutput, RigInstalledStackSummary,
    RigInstalledSummary, RigListOutput, RigReleaseLockOutput, RigRepairOutput, RigRunOutput,
    RigShowOutput, RigSourceSummary, RigStatusOutput, RigSummary, RigSyncOutput, RigUpOutput,
    RigUpPlanOutput, RigUpPlanStep, RigUpdateOutput,
};
use super::bench::RigRunBenchOptions;
use super::utils::args::SettingArgs;
use super::CmdResult;
use crate::command_contract::{
    CommandPortabilityContract, LabCommandContract, BENCH_LAB_LABEL, LAB_NO_EXTRA_TOOLS,
    RIG_CHECK_LAB_LABEL, RIG_UP_LAB_UNSUPPORTED_REASON,
};

#[derive(Args)]
pub struct RigArgs {
    #[command(subcommand)]
    command: RigCommand,
}

impl RigArgs {
    pub fn is_hot_resource_command(&self) -> bool {
        matches!(
            self.command,
            RigCommand::Up { .. } | RigCommand::Check { .. }
        )
    }

    pub fn is_check_command(&self) -> bool {
        matches!(self.command, RigCommand::Check { .. })
    }

    pub fn is_runner_source_management_command(&self) -> bool {
        matches!(
            self.command,
            RigCommand::Install { .. } | RigCommand::Sync { .. } | RigCommand::Sources { .. }
        )
    }

    pub(crate) fn up_dry_run_rig_id(&self) -> Option<&str> {
        match &self.command {
            RigCommand::Up {
                rig_id,
                dry_run: true,
            } => Some(rig_id),
            _ => None,
        }
    }

    pub(crate) fn portability_contract(&self) -> CommandPortabilityContract {
        if let RigCommand::Check { target, .. } = &self.command {
            let contract = if rig_check_uses_linked_local_source(target) {
                LabCommandContract::explicit_runner(
                    RIG_CHECK_LAB_LABEL,
                    None,
                    false,
                    LAB_NO_EXTRA_TOOLS,
                )
            } else {
                LabCommandContract::portable_workload(
                    RIG_CHECK_LAB_LABEL,
                    None,
                    false,
                    LAB_NO_EXTRA_TOOLS,
                )
            };
            return CommandPortabilityContract::lab(contract);
        }
        if matches!(self.command, RigCommand::Run(_)) {
            return CommandPortabilityContract::lab(LabCommandContract::portable(
                BENCH_LAB_LABEL,
                None,
                true,
                LAB_NO_EXTRA_TOOLS,
            ));
        }
        if self.is_hot_resource_command() {
            return CommandPortabilityContract::lab(LabCommandContract::local_only(
                "rig up",
                RIG_UP_LAB_UNSUPPORTED_REASON,
            ));
        }
        CommandPortabilityContract::none()
    }
}

fn rig_check_uses_linked_local_source(rig_id: &str) -> bool {
    if std::path::Path::new(rig_id).exists() {
        return true;
    }
    rig::read_source_metadata(rig_id).is_some_and(|source| source.linked)
}

#[derive(Subcommand)]
enum RigCommand {
    /// List all declared rigs
    List,
    /// Show a rig spec
    Show {
        /// Rig ID
        rig_id: String,
    },
    /// Materialize a rig: run its `up` pipeline
    Up {
        /// Rig ID
        rig_id: String,
        /// Build an execution plan without running the rig.
        #[arg(long)]
        dry_run: bool,
    },
    /// Run a rig's `check` pipeline and report health
    Check {
        /// Rig ID, local package path, or direct rig.json path
        target: String,
        /// Select a rig from a local package path containing multiple rigs
        #[arg(long)]
        id: Option<String>,
        /// Override the rig's primary component checkout path for this check.
        /// Uses bench.default_component when present, otherwise the rig's only component.
        #[arg(long, value_name = "CHECKOUT")]
        path: Option<String>,
    },
    /// Lint a rig package without touching the environment.
    ///
    /// Runs ONLY the env-independent package lint (conflict markers, JSON
    /// validity, and `extends` template materialization) — no requirements and
    /// no live `check` probes. This is the entry point CI uses to validate a
    /// rig package where no component checkouts exist.
    Lint {
        /// Rig ID, local package path, or direct rig.json path
        target: String,
        /// Select a rig from a local package path containing multiple rigs
        #[arg(long)]
        id: Option<String>,
    },
    /// Tear down a rig: stop services and run its `down` pipeline
    Down {
        /// Rig ID
        rig_id: String,
    },
    /// Repair safe declared drift without running the full `up` pipeline
    Repair {
        /// Rig ID
        rig_id: String,
    },
    /// Sync every stack declared by this rig's components
    Sync {
        /// Rig ID
        rig_id: String,
        /// Print what WOULD happen without mutating stack specs or target branches.
        #[arg(long)]
        dry_run: bool,
    },
    /// Refresh, sync, check, and benchmark a rig profile end-to-end
    Run(RigRunArgs),
    /// Show current state of a rig: running services, last up/check
    Status {
        /// Rig ID
        rig_id: String,
    },
    /// Compatibility alias for `homeboy runs list --rig <rig-id>`
    Runs {
        /// Rig ID
        rig_id: String,
        /// Maximum runs to return
        #[arg(long, default_value_t = 20)]
        limit: i64,
    },
    /// Release a stuck active-run lock (rig lease) so a new run can proceed.
    ///
    /// By default the lock is only released when its holder is provably gone or
    /// past its TTL. Pass `--force` to reclaim a lock whose holder is still
    /// alive but wedged. Releasing the lock frees the local guardrail; it does
    /// not terminate the holder process.
    ReleaseLock {
        /// Rig ID
        rig_id: String,
        /// Reclaim the lock even if its holder process is still alive.
        #[arg(long)]
        force: bool,
    },
    /// Install rigs from a local package path or git URL
    Install {
        /// Git URL or local path containing rig.json or rigs/<id>/rig.json
        source: String,
        /// Install a specific rig from a multi-rig package
        #[arg(long)]
        id: Option<String>,
        /// Install every rig in the package
        #[arg(long)]
        all: bool,
        /// Explicitly refresh an existing matching rig install. Refuses user-owned conflicts.
        #[arg(long, alias = "force")]
        reinstall: bool,
    },
    /// Update rigs installed from git-backed rig packages
    Update {
        /// Rig ID to update. Updates the source package that owns this rig.
        rig_id: Option<String>,
        /// Update every installed git-backed rig source package
        #[arg(long)]
        all: bool,
    },
    /// Inspect or remove installed rig sources
    Sources {
        #[command(subcommand)]
        command: Option<sources::RigSourcesCommand>,
    },
    /// Install, update, or remove this rig's desktop app launcher.
    App {
        #[command(subcommand)]
        command: RigAppCommand,
    },
}

#[derive(Subcommand)]
enum RigAppCommand {
    /// Generate and install this rig's configured launcher.
    Install {
        /// Rig ID
        rig_id: String,
        /// Print generated paths without writing files.
        #[arg(long)]
        dry_run: bool,
    },
    /// Regenerate this rig's configured launcher.
    Update {
        /// Rig ID
        rig_id: String,
        /// Print generated paths without writing files.
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove this rig's configured launcher.
    Uninstall {
        /// Rig ID
        rig_id: String,
        /// Print generated paths without deleting files.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Args)]
struct RigRunArgs {
    /// Rig ID
    rig_id: String,

    /// Rig-defined bench profile to run
    #[arg(long, value_name = "PROFILE")]
    profile: String,

    /// Optional component ID override for rigs with multiple bench components
    #[arg(long, value_name = "COMPONENT")]
    component: Option<String>,

    /// Override the component checkout path for this invocation
    #[arg(long)]
    path: Option<String>,

    /// Iterations per scenario. Forwarded to the bench runner.
    #[arg(long, default_value_t = 10)]
    iterations: u64,

    /// Warmup iterations to run before measured iterations.
    #[arg(long, value_name = "N", allow_hyphen_values = true)]
    warmup: Option<u64>,

    /// Number of repetitions (independent substrate spawns).
    #[arg(long, default_value_t = 1, value_name = "COUNT", value_parser = crate::commands::parse_runs_count)]
    runs: u64,

    /// Caller-supplied stable proof label for this run.
    #[arg(long = "run-id", value_name = "ID")]
    run_id: Option<String>,

    /// Directory shared across bench runner instances.
    #[arg(long, value_name = "DIR")]
    shared_state: Option<std::path::PathBuf>,

    /// Number of concurrent bench runner instances.
    #[arg(long, default_value_t = 1)]
    concurrency: u32,

    #[command(flatten)]
    setting_args: SettingArgs,

    /// Print compact machine-readable bench summary.
    #[arg(long)]
    json_summary: bool,

    /// Additional arguments to pass to the bench runner (must follow --)
    #[arg(last = true)]
    args: Vec<String>,
}

pub fn run(args: RigArgs, _global: &super::GlobalArgs) -> CmdResult<RigCommandOutput> {
    match args.command {
        RigCommand::List => list(),
        RigCommand::Show { rig_id } => show(&rig_id),
        RigCommand::Up { rig_id, dry_run } => up(&rig_id, dry_run),
        RigCommand::Check { target, id, path } => check(&target, id.as_deref(), path.as_deref()),
        RigCommand::Lint { target, id } => lint(&target, id.as_deref()),
        RigCommand::Down { rig_id } => down(&rig_id),
        RigCommand::Repair { rig_id } => repair(&rig_id),
        RigCommand::Sync { rig_id, dry_run } => sync(&rig_id, dry_run),
        RigCommand::Run(args) => run_profile(args),
        RigCommand::Status { rig_id } => status(&rig_id),
        RigCommand::Runs { rig_id, limit } => runs(&rig_id, limit),
        RigCommand::ReleaseLock { rig_id, force } => release_lock(&rig_id, force),
        RigCommand::Install {
            source,
            id,
            all,
            reinstall,
        } => install(&source, id.as_deref(), all, reinstall),
        RigCommand::Update { rig_id, all } => update(rig_id.as_deref(), all),
        RigCommand::Sources { command } => sources::run(command),
        RigCommand::App { command } => app(command),
    }
}

fn release_lock(rig_id: &str, force: bool) -> CmdResult<RigCommandOutput> {
    let outcome = rig::release_active_run_lease(rig_id, force)?;
    Ok((
        RigCommandOutput::ReleaseLock(RigReleaseLockOutput {
            command: "rig.release_lock",
            outcome,
        }),
        0,
    ))
}

fn runs(rig_id: &str, limit: i64) -> CmdResult<RigCommandOutput> {
    let (output, exit_code) = super::runs::list_runs(
        super::runs::RunsListArgs {
            rig: Some(rig_id.to_string()),
            limit,
            ..Default::default()
        },
        "rig.runs",
    )?;
    Ok((RigCommandOutput::Runs(output), exit_code))
}

fn list() -> CmdResult<RigCommandOutput> {
    let rigs = rig::list()?;
    let summaries = rigs
        .into_iter()
        .map(|r| {
            let mut pipelines: Vec<String> = r.pipeline.keys().cloned().collect();
            pipelines.sort();
            let declared_id = rig::declared_id(&r.id)?;
            Ok(RigSummary {
                source: rig::read_source_metadata(&r.id).map(|source| RigSourceSummary {
                    source: source.source,
                    package_path: source.package_path,
                    rig_path: source.rig_path,
                    linked: source.linked,
                    source_revision: source.source_revision,
                }),
                id: r.id,
                declared_id,
                description: r.description,
                component_count: r.components.len(),
                service_count: r.services.len(),
                pipelines,
            })
        })
        .collect::<homeboy::core::Result<Vec<_>>>()?;

    Ok((
        RigCommandOutput::List(RigListOutput {
            command: "rig.list",
            rigs: summaries,
        }),
        0,
    ))
}

fn install(
    source: &str,
    id: Option<&str>,
    all: bool,
    _reinstall: bool,
) -> CmdResult<RigCommandOutput> {
    let result = rig::install(source, id, all)?;
    Ok((
        RigCommandOutput::Install(RigInstallOutput {
            command: "rig.install",
            source: result.source,
            package_path: result.package_path.to_string_lossy().to_string(),
            linked: result.linked,
            installed: result
                .installed
                .into_iter()
                .map(|rig| RigInstalledSummary {
                    id: rig.id,
                    description: rig.description,
                    path: rig.path.to_string_lossy().to_string(),
                    spec_path: rig.spec_path.to_string_lossy().to_string(),
                    source_revision: rig.source_revision,
                })
                .collect(),
            installed_stacks: result
                .installed_stacks
                .into_iter()
                .map(|stack| RigInstalledStackSummary {
                    id: stack.id,
                    description: stack.description,
                    path: stack.path.to_string_lossy().to_string(),
                    spec_path: stack.spec_path.to_string_lossy().to_string(),
                    source_revision: stack.source_revision,
                })
                .collect(),
        }),
        0,
    ))
}

fn update(rig_id: Option<&str>, all: bool) -> CmdResult<RigCommandOutput> {
    let report = match (rig_id, all) {
        (Some(_), true) => {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "rig_id",
                "Pass either a rig ID or --all, not both",
                rig_id.map(str::to_string),
                None,
            ))
        }
        (Some(id), false) => rig::update_source_for_rig(id)?,
        (None, true) => rig::update_all_sources()?,
        (None, false) => {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "rig_id",
                "Pass a rig ID or --all",
                None,
                None,
            ))
        }
    };

    Ok((
        RigCommandOutput::Update(RigUpdateOutput {
            command: "rig.update",
            report,
        }),
        0,
    ))
}

fn show(rig_id: &str) -> CmdResult<RigCommandOutput> {
    let rig = rig::load(rig_id)?;
    let resources = rig::expand::expand_resources(&rig);
    Ok((
        RigCommandOutput::Show(RigShowOutput {
            command: "rig.show",
            rig,
            resources,
        }),
        0,
    ))
}

fn up(rig_id: &str, dry_run: bool) -> CmdResult<RigCommandOutput> {
    let rig = rig::load(rig_id)?;
    if dry_run {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "dry_run",
            "rig up --dry-run requires --runner <runner-id> so Homeboy can emit a portable runner exec plan",
            Some(rig_id.to_string()),
            None,
        ));
    }
    let report = rig::run_up(&rig)?;
    let exit_code = if report.success { 0 } else { 1 };
    Ok((
        RigCommandOutput::Up(RigUpOutput {
            command: "rig.up",
            report,
        }),
        exit_code,
    ))
}

pub(crate) fn up_runner_exec_plan(rig_id: &str, runner_id: &str) -> CmdResult<RigCommandOutput> {
    let rig = rig::load(rig_id)?;
    let steps = command_only_up_steps(&rig, runner_id)?;
    let commands = steps
        .iter()
        .map(|step| step.runner_exec_command.clone())
        .collect();
    Ok((
        RigCommandOutput::UpPlan(RigUpPlanOutput {
            command: "rig.up.plan",
            rig_id: rig.id,
            runner_id: runner_id.to_string(),
            portable: true,
            steps,
            commands,
        }),
        0,
    ))
}

fn command_only_up_steps(
    rig: &rig::RigSpec,
    runner_id: &str,
) -> homeboy::core::Result<Vec<RigUpPlanStep>> {
    if !rig.services.is_empty()
        || !rig.resources.is_empty()
        || !rig.symlinks.is_empty()
        || !rig.shared_paths.is_empty()
    {
        return Err(non_portable_up_error(&rig.id));
    }

    let Some(steps) = rig.pipeline.get("up") else {
        return Ok(Vec::new());
    };
    steps
        .iter()
        .enumerate()
        .map(|(index, step)| match step {
            rig::PipelineStep::Command {
                cmd,
                cwd,
                env,
                label,
                ..
            } => {
                let mut env_pairs = env
                    .iter()
                    .map(|(key, value)| format!("{key}={value}"))
                    .collect::<Vec<_>>();
                env_pairs.sort();
                let mut argv = vec![
                    "homeboy".to_string(),
                    "runner".to_string(),
                    "exec".to_string(),
                    runner_id.to_string(),
                ];
                if let Some(cwd) = cwd {
                    argv.push("--cwd".to_string());
                    argv.push(cwd.clone());
                }
                for pair in &env_pairs {
                    argv.push("--env".to_string());
                    argv.push(pair.clone());
                }
                argv.extend([
                    "--".to_string(),
                    "sh".to_string(),
                    "-c".to_string(),
                    cmd.clone(),
                ]);
                Ok(RigUpPlanStep {
                    label: label
                        .clone()
                        .unwrap_or_else(|| format!("command[{}]", index + 1)),
                    command: cmd.clone(),
                    cwd: cwd.clone(),
                    env: env_pairs,
                    runner_exec_command: shell_join(&argv),
                })
            }
            _ => Err(non_portable_up_error(&rig.id)),
        })
        .collect()
}

fn non_portable_up_error(rig_id: &str) -> homeboy::core::Error {
    homeboy::core::Error::validation_invalid_argument(
        "rig_id",
        "rig up --dry-run --runner only plans command-only rigs; service, resource, symlink, shared-path, and typed pipeline steps still run locally through rig up",
        Some(rig_id.to_string()),
        None,
    )
}

fn shell_join(args: &[String]) -> String {
    args.iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | ':' | '='))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn check(
    target: &str,
    id: Option<&str>,
    path_override: Option<&str>,
) -> CmdResult<RigCommandOutput> {
    let mut rig = if std::path::Path::new(target).exists() {
        rig::load_local_source(target, id)?
    } else {
        if id.is_some() {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "id",
                "--id is only valid when checking a local package path",
                id.map(str::to_string),
                None,
            ));
        }
        rig::load(target)?
    };
    if let Some(path) = path_override {
        apply_check_path_override(&mut rig, path)?;
    }
    let report = rig::run_check(&rig)?;
    let exit_code = if report.success { 0 } else { 1 };
    Ok((
        RigCommandOutput::Check(RigCheckOutput {
            command: "rig.check",
            report,
        }),
        exit_code,
    ))
}

fn lint(target: &str, id: Option<&str>) -> CmdResult<RigCommandOutput> {
    let rig = if std::path::Path::new(target).exists() {
        rig::load_local_source(target, id)?
    } else {
        if id.is_some() {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "id",
                "--id is only valid when linting a local package path",
                id.map(str::to_string),
                None,
            ));
        }
        rig::load(target)?
    };
    let report = rig::run_lint(&rig)?;
    let exit_code = if report.success { 0 } else { 1 };
    Ok((
        RigCommandOutput::Check(RigCheckOutput {
            command: "rig.lint",
            report,
        }),
        exit_code,
    ))
}

fn apply_check_path_override(rig: &mut rig::RigSpec, path: &str) -> homeboy::core::Result<()> {
    if path.trim().is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "path",
            "--path requires a non-empty component checkout path",
            Some(path.to_string()),
            None,
        ));
    }

    let component_id = if let Some(component_id) = rig
        .bench
        .as_ref()
        .and_then(|bench| bench.default_component.as_deref())
        .filter(|component_id| !component_id.trim().is_empty())
    {
        component_id.to_string()
    } else if rig.components.len() == 1 {
        rig.components
            .keys()
            .next()
            .expect("one component key")
            .to_string()
    } else {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "path",
            "rig check --path can only infer a target component when the rig declares bench.default_component or exactly one component",
            Some(path.to_string()),
            Some(vec![
                "Add bench.default_component to the rig spec".to_string(),
                "Run homeboy bench --rig <rig> --path <checkout> for bench workflows".to_string(),
            ]),
        ));
    };

    let Some(component) = rig.components.get_mut(&component_id) else {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "bench.default_component",
            format!(
                "rig '{}' declares bench.default_component '{}' but no matching component exists",
                rig.id, component_id
            ),
            Some(component_id),
            None,
        ));
    };
    component.path = path.to_string();
    Ok(())
}

fn down(rig_id: &str) -> CmdResult<RigCommandOutput> {
    let rig = rig::load(rig_id)?;
    let report = rig::run_down(&rig)?;
    let exit_code = if report.success { 0 } else { 1 };
    Ok((
        RigCommandOutput::Down(RigDownOutput {
            command: "rig.down",
            report,
        }),
        exit_code,
    ))
}

fn repair(rig_id: &str) -> CmdResult<RigCommandOutput> {
    let rig = rig::load(rig_id)?;
    let report = rig::run_repair(&rig)?;
    let exit_code = if report.success { 0 } else { 1 };
    Ok((
        RigCommandOutput::Repair(RigRepairOutput {
            command: "rig.repair",
            report,
        }),
        exit_code,
    ))
}

fn sync(rig_id: &str, dry_run: bool) -> CmdResult<RigCommandOutput> {
    let rig = rig::load(rig_id)?;
    let report = rig::run_sync(&rig, dry_run)?;
    let exit_code = if report.success { 0 } else { 1 };
    Ok((
        RigCommandOutput::Sync(RigSyncOutput {
            command: "rig.sync",
            report,
        }),
        exit_code,
    ))
}

fn run_profile(args: RigRunArgs) -> CmdResult<RigCommandOutput> {
    let source_update = refresh_rig_source_if_materialized(&args.rig_id)?;
    let rig = rig::load(&args.rig_id)?;
    let sync_report = rig::run_sync(&rig, false)?;
    let bench_options = rig_run_bench_options(args);
    let profile = bench_options.profile.clone();
    let bench_invocation = bench_options.bench_plan();

    if !sync_report.success {
        return Ok((
            RigCommandOutput::Run(RigRunOutput {
                command: "rig.run",
                rig_id: rig.id,
                profile,
                source_update,
                sync: sync_report,
                bench_invocation,
                bench: None,
            }),
            1,
        ));
    }

    let (bench, bench_exit_code) = super::bench::run_rig_profile(bench_options)?;
    Ok((
        RigCommandOutput::Run(RigRunOutput {
            command: "rig.run",
            rig_id: rig.id,
            profile,
            source_update,
            sync: sync_report,
            bench_invocation,
            bench: Some(bench),
        }),
        bench_exit_code,
    ))
}

fn refresh_rig_source_if_materialized(
    rig_id: &str,
) -> homeboy::core::Result<Option<rig::RigSourceUpdateResult>> {
    let Some(metadata) = rig::read_source_metadata(rig_id) else {
        return Ok(None);
    };
    if metadata.linked {
        return Ok(None);
    }
    Ok(Some(rig::update_source_for_rig(rig_id)?))
}

fn rig_run_bench_options(args: RigRunArgs) -> RigRunBenchOptions {
    RigRunBenchOptions {
        rig_id: args.rig_id,
        profile: args.profile,
        component: args.component,
        path: args.path,
        iterations: args.iterations,
        warmup: args.warmup,
        runs: args.runs,
        run_id: args.run_id,
        shared_state: args.shared_state,
        concurrency: args.concurrency,
        settings: args.setting_args,
        json_summary: args.json_summary,
        passthrough_args: args.args,
    }
}

fn status(rig_id: &str) -> CmdResult<RigCommandOutput> {
    let rig = rig::load(rig_id)?;
    let report = rig::run_status(&rig)?;
    Ok((
        RigCommandOutput::Status(RigStatusOutput {
            command: "rig.status",
            report,
        }),
        0,
    ))
}

fn app(command: RigAppCommand) -> CmdResult<RigCommandOutput> {
    match command {
        RigAppCommand::Install { rig_id, dry_run } => app_install(&rig_id, dry_run),
        RigAppCommand::Update { rig_id, dry_run } => app_update(&rig_id, dry_run),
        RigAppCommand::Uninstall { rig_id, dry_run } => app_uninstall(&rig_id, dry_run),
    }
}

fn app_install(rig_id: &str, dry_run: bool) -> CmdResult<RigCommandOutput> {
    let rig = rig::load(rig_id)?;
    let report = rig::app::install(&rig, rig::AppLauncherOptions { dry_run })?;
    Ok((
        RigCommandOutput::App(RigAppOutput {
            command: "rig.app.install",
            report,
        }),
        0,
    ))
}

fn app_update(rig_id: &str, dry_run: bool) -> CmdResult<RigCommandOutput> {
    let rig = rig::load(rig_id)?;
    let report = rig::app::update(&rig, rig::AppLauncherOptions { dry_run })?;
    Ok((
        RigCommandOutput::App(RigAppOutput {
            command: "rig.app.update",
            report,
        }),
        0,
    ))
}

fn app_uninstall(rig_id: &str, dry_run: bool) -> CmdResult<RigCommandOutput> {
    let rig = rig::load(rig_id)?;
    let report = rig::app::uninstall(&rig, rig::AppLauncherOptions { dry_run })?;
    Ok((
        RigCommandOutput::App(RigAppOutput {
            command: "rig.app.uninstall",
            report,
        }),
        0,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::fs;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        rig: RigArgs,
    }

    #[test]
    fn parses_sources_without_nested_subcommand() {
        let cli = TestCli::try_parse_from(["homeboy", "sources"])
            .expect("rig sources should parse without nested subcommand");

        let RigCommand::Sources { command } = cli.rig.command else {
            panic!("expected rig sources command");
        };
        assert!(command.is_none());
    }

    #[test]
    fn sync_is_runner_source_management() {
        let cli =
            TestCli::try_parse_from(["homeboy", "sync", "static-site-importer-fixture-matrix"])
                .expect("rig sync should parse");

        assert!(cli.rig.is_runner_source_management_command());
    }

    #[test]
    fn check_accepts_path_override() {
        let cli =
            TestCli::try_parse_from(["homeboy", "check", "studio-bfb", "--path", "/tmp/studio"])
                .expect("rig check --path should parse");

        let RigCommand::Check { target, path, .. } = cli.rig.command else {
            panic!("expected rig check command");
        };
        assert_eq!(target, "studio-bfb");
        assert_eq!(path.as_deref(), Some("/tmp/studio"));
    }

    #[test]
    fn parses_rig_run_profile_and_bench_settings() {
        let cli = TestCli::try_parse_from([
            "homeboy",
            "run",
            "static-site-importer-fixture-matrix",
            "--profile",
            "fixture-matrix",
            "--iterations",
            "1",
            "--setting",
            "bench_env.CORPUS_SIZE=44",
            "--setting-json",
            "artifact_env={\"KEEP\":\"1\"}",
            "--json-summary",
            "--",
            "--full",
        ])
        .expect("rig run should parse profile and bench settings");

        let RigCommand::Run(args) = cli.rig.command else {
            panic!("expected rig run command");
        };
        assert_eq!(args.rig_id, "static-site-importer-fixture-matrix");
        assert_eq!(args.profile, "fixture-matrix");
        assert_eq!(args.iterations, 1);
        assert_eq!(
            args.setting_args.setting,
            vec![("bench_env.CORPUS_SIZE".to_string(), "44".to_string())]
        );
        assert_eq!(args.args, vec!["--full".to_string()]);
    }

    #[test]
    fn rig_run_accepts_global_runner_after_subcommand() {
        let cli = crate::cli_surface::Cli::try_parse_from([
            "homeboy",
            "rig",
            "run",
            "static-site-importer-fixture-matrix",
            "--profile",
            "fixture-matrix",
            "--runner",
            "homeboy-lab",
        ])
        .expect("global --runner should parse for rig run");

        assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));
        let crate::cli_surface::Commands::Rig(args) = cli.command else {
            panic!("expected rig command");
        };
        assert!(matches!(args.command, RigCommand::Run(_)));
    }

    #[test]
    fn rig_run_builds_canonical_bench_invocation() {
        let args = RigRunArgs {
            rig_id: "static-site-importer-fixture-matrix".to_string(),
            profile: "fixture-matrix".to_string(),
            component: None,
            path: Some("/tmp/static-site-importer".to_string()),
            iterations: 3,
            warmup: Some(1),
            runs: 2,
            run_id: Some("proof-7118".to_string()),
            shared_state: Some(std::path::PathBuf::from("/tmp/homeboy-shared")),
            concurrency: 4,
            setting_args: SettingArgs {
                setting: vec![("bench_env.CORPUS_SIZE".to_string(), "44".to_string())],
                setting_json: vec![(
                    "artifact_env".to_string(),
                    serde_json::json!({ "KEEP": "1" }),
                )],
            },
            json_summary: true,
            args: vec!["--full".to_string()],
        };

        let plan = rig_run_bench_options(args).bench_plan();

        assert_eq!(
            plan.command,
            vec![
                "homeboy",
                "bench",
                "--rig",
                "static-site-importer-fixture-matrix",
                "--profile",
                "fixture-matrix",
                "--iterations",
                "3",
                "--path",
                "/tmp/static-site-importer",
                "--warmup",
                "1",
                "--runs",
                "2",
                "--run-id",
                "proof-7118",
                "--shared-state",
                "/tmp/homeboy-shared",
                "--concurrency",
                "4",
                "--setting",
                "bench_env.CORPUS_SIZE=44",
                "--setting-json",
                "artifact_env={\"KEEP\":\"1\"}",
                "--json-summary",
                "--",
                "--full",
            ]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn check_path_override_uses_bench_default_component() {
        crate::test_support::with_isolated_home(|home| {
            let declared_component = tempfile::TempDir::new().expect("declared component");
            let override_component = tempfile::TempDir::new().expect("override component");
            fs::write(override_component.path().join("ready.txt"), "ready").expect("marker");
            write_rig(
                home.path(),
                "studio-bfb",
                serde_json::json!({
                    "id": "studio-bfb",
                    "components": {
                        "studio": { "path": declared_component.path() },
                        "helper": { "path": declared_component.path() }
                    },
                    "bench": { "default_component": "studio" },
                    "requirements": {
                        "filesystem_assertions": [
                            {
                                "kind": "file",
                                "path": "${components.studio.path}/ready.txt"
                            }
                        ]
                    }
                }),
            );

            let (_output, exit_code) = check(
                "studio-bfb",
                None,
                Some(&override_component.path().to_string_lossy()),
            )
            .expect("rig check should use --path override");

            assert_eq!(exit_code, 0);
        });
    }

    #[test]
    fn check_path_override_requires_unambiguous_component() {
        crate::test_support::with_isolated_home(|home| {
            let component = tempfile::TempDir::new().expect("component");
            write_rig(
                home.path(),
                "multi-component",
                serde_json::json!({
                    "id": "multi-component",
                    "components": {
                        "app": { "path": component.path() },
                        "helper": { "path": component.path() }
                    }
                }),
            );

            let err = match check(
                "multi-component",
                None,
                Some(&component.path().to_string_lossy()),
            ) {
                Ok(_) => panic!("ambiguous --path should fail"),
                Err(err) => err,
            };

            assert_eq!(err.code.as_str(), "validation.invalid_argument");
            assert!(err.message.contains("bench.default_component"));
        });
    }

    #[test]
    fn lint_command_parses_target_and_id() {
        let cli = TestCli::try_parse_from(["homeboy", "lint", "./pkg", "--id", "alpha"])
            .expect("rig lint should parse");

        let RigCommand::Lint { target, id } = cli.rig.command else {
            panic!("expected rig lint command");
        };
        assert_eq!(target, "./pkg");
        assert_eq!(id.as_deref(), Some("alpha"));
    }

    #[test]
    fn lint_runs_package_lint_only_ignoring_requirements() {
        crate::test_support::with_isolated_home(|home| {
            write_rig(
                home.path(),
                "lint-cmd",
                serde_json::json!({
                    "id": "lint-cmd",
                    "requirements": {
                        "filesystem_assertions": [
                            { "kind": "file", "path": "/definitely/missing/lint-cmd/marker.txt" }
                        ]
                    }
                }),
            );

            // Full check fails on the missing requirement file.
            let (_check, check_exit) = check("lint-cmd", None, None).expect("rig check runs");
            assert_eq!(check_exit, 1, "missing requirement should fail rig check");

            // The lint command skips requirements entirely and succeeds.
            let (output, exit_code) = lint("lint-cmd", None).expect("rig lint runs");
            assert_eq!(exit_code, 0);
            let json = serde_json::to_value(output).expect("serialize lint output");
            assert_eq!(json["payload"]["command"], "rig.lint");
            assert_eq!(json["payload"]["success"], true);
        });
    }

    #[test]
    fn lint_rejects_id_for_installed_rig_id() {
        crate::test_support::with_isolated_home(|home| {
            write_rig(
                home.path(),
                "lint-id-guard",
                serde_json::json!({ "id": "lint-id-guard" }),
            );

            let err = match lint("lint-id-guard", Some("alpha")) {
                Ok(_) => panic!("--id against an installed rig id should fail"),
                Err(err) => err,
            };
            assert_eq!(err.code.as_str(), "validation.invalid_argument");
            assert!(err.message.contains("local package path"));
        });
    }

    #[test]
    fn up_dry_run_runner_plan_emits_command_only_runner_exec_steps() {
        crate::test_support::with_isolated_home(|home| {
            write_rig(
                home.path(),
                "script-matrix",
                serde_json::json!({
                    "id": "script-matrix",
                    "description": "script matrix",
                    "pipeline": {
                        "up": [
                            {
                                "kind": "command",
                                "command": "./scripts/run-one.sh --axis a",
                                "cwd": "packages/tooling",
                                "env": { "MATRIX_AXIS": "a" },
                                "label": "axis a"
                            },
                            {
                                "kind": "command",
                                "command": "./scripts/run-one.sh --axis b",
                                "label": "axis b"
                            }
                        ]
                    }
                }),
            );

            let (output, exit_code) = up_runner_exec_plan("script-matrix", "homeboy-lab")
                .expect("command-only rig should plan");

            assert_eq!(exit_code, 0);
            let json = serde_json::to_value(output).expect("serialize plan");
            assert_eq!(json["variant"], "up_plan");
            assert_eq!(json["payload"]["portable"], true);
            assert_eq!(json["payload"]["steps"].as_array().unwrap().len(), 2);
            assert_eq!(
                json["payload"]["steps"][0]["runner_exec_command"],
                "homeboy runner exec homeboy-lab --cwd packages/tooling --env MATRIX_AXIS=a -- sh -c './scripts/run-one.sh --axis a'"
            );
            assert_eq!(
                json["payload"]["commands"][1],
                "homeboy runner exec homeboy-lab -- sh -c './scripts/run-one.sh --axis b'"
            );
        });
    }

    #[test]
    fn up_dry_run_runner_plan_refuses_resource_rigs() {
        crate::test_support::with_isolated_home(|home| {
            write_rig(
                home.path(),
                "resource-rig",
                serde_json::json!({
                    "id": "resource-rig",
                    "description": "resource rig",
                    "resources": {
                        "ports": [8888]
                    },
                    "pipeline": {
                        "up": [
                            { "kind": "command", "command": "npm run matrix" }
                        ]
                    }
                }),
            );

            let err = match up_runner_exec_plan("resource-rig", "homeboy-lab") {
                Ok(_) => panic!("resource rig should not plan"),
                Err(err) => err,
            };

            assert_eq!(err.code.as_str(), "validation.invalid_argument");
            assert!(err.message.contains("command-only rigs"));
        });
    }

    fn write_rig(home: &std::path::Path, rig_id: &str, value: serde_json::Value) {
        let dir = home.join(".config/homeboy/rigs");
        fs::create_dir_all(&dir).expect("create rigs dir");
        fs::write(
            dir.join(format!("{rig_id}.json")),
            serde_json::to_string_pretty(&value).expect("serialize rig"),
        )
        .expect("write rig");
    }
}
