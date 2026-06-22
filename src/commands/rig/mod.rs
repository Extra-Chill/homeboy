//! `homeboy rig` command — CLI surface for the rig primitive.

mod output;
mod sources;

pub use output::RigCommandOutput;

use clap::{Args, Subcommand};

use homeboy::core::rig;

use self::output::{
    RigAppOutput, RigCheckOutput, RigDownOutput, RigInstallOutput, RigInstalledStackSummary,
    RigInstalledSummary, RigListOutput, RigRepairOutput, RigShowOutput, RigSourceSummary,
    RigStatusOutput, RigSummary, RigSyncOutput, RigUpOutput, RigUpdateOutput,
};
use super::CmdResult;
use crate::command_contract::{
    CommandPortabilityContract, LabCommandContract, LAB_NO_EXTRA_TOOLS, RIG_CHECK_LAB_LABEL,
    RIG_UP_LAB_UNSUPPORTED_REASON,
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
            RigCommand::Install { .. } | RigCommand::Sources { .. }
        )
    }

    pub(crate) fn portability_contract(&self) -> CommandPortabilityContract {
        if self.is_check_command() {
            return CommandPortabilityContract::lab(LabCommandContract::portable_workload(
                RIG_CHECK_LAB_LABEL,
                None,
                false,
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
    },
    /// Run a rig's `check` pipeline and report health
    Check {
        /// Rig ID
        rig_id: String,
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
    /// Show current state of a rig: running services, last up/check
    Status {
        /// Rig ID
        rig_id: String,
    },
    /// List persisted observation runs for this rig
    Runs {
        /// Rig ID
        rig_id: String,
        /// Maximum runs to return
        #[arg(long, default_value_t = 20)]
        limit: i64,
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
        command: sources::RigSourcesCommand,
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

pub fn run(args: RigArgs, _global: &super::GlobalArgs) -> CmdResult<RigCommandOutput> {
    match args.command {
        RigCommand::List => list(),
        RigCommand::Show { rig_id } => show(&rig_id),
        RigCommand::Up { rig_id } => up(&rig_id),
        RigCommand::Check { rig_id } => check(&rig_id),
        RigCommand::Down { rig_id } => down(&rig_id),
        RigCommand::Repair { rig_id } => repair(&rig_id),
        RigCommand::Sync { rig_id, dry_run } => sync(&rig_id, dry_run),
        RigCommand::Status { rig_id } => status(&rig_id),
        RigCommand::Runs { rig_id, limit } => runs(&rig_id, limit),
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

fn up(rig_id: &str) -> CmdResult<RigCommandOutput> {
    let rig = rig::load(rig_id)?;
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

fn check(rig_id: &str) -> CmdResult<RigCommandOutput> {
    let rig = rig::load(rig_id)?;
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
