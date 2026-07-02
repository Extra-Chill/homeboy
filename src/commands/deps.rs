use clap::{Args, Subcommand};

use homeboy::core::deps;

use super::CmdResult;

#[derive(Args)]
pub struct DepsArgs {
    #[command(subcommand)]
    command: DepsCommand,
}

#[derive(Subcommand)]
enum DepsCommand {
    /// Inspect dependency constraints and locked package versions
    Status {
        /// Component ID. When omitted, auto-detected from CWD.
        component: Option<String>,

        /// Limit output to one package.
        #[arg(long, value_name = "PACKAGE")]
        package: Option<String>,

        /// Workspace path to operate on directly.
        #[arg(long, value_name = "PATH")]
        path: Option<String>,
    },
    /// Install a component's dependencies through its detected providers
    ///
    /// Package manager (composer/npm/component script/extension) is chosen by
    /// workspace detection and manifest config — not hardcoded. CI uses this
    /// (or `component setup`) instead of shell-level composer/npm/pnpm/yarn.
    Install {
        /// Component ID. When omitted, auto-detected from CWD.
        component: Option<String>,

        /// Workspace path to operate on directly.
        #[arg(long, value_name = "PATH")]
        path: Option<String>,
    },
    /// Update one package through its dependency provider
    Update {
        /// Package name, e.g. example-org/block-format-bridge.
        package: String,

        /// Component ID. When omitted, auto-detected from CWD.
        component: Option<String>,

        /// New manifest constraint, e.g. ^0.4.
        #[arg(long, value_name = "CONSTRAINT")]
        to: Option<String>,

        /// Workspace path to operate on directly.
        #[arg(long, value_name = "PATH")]
        path: Option<String>,

        /// Skip provider-owned install/lockfile refresh after the manifest update.
        #[arg(long)]
        no_install: bool,

        /// Rebuild the component through its generic build capability after updating.
        #[arg(long)]
        rebuild: bool,
    },
    /// Work with declared downstream dependency stacks
    Stack {
        #[command(subcommand)]
        command: DepsStackCommand,
    },
}

#[derive(Subcommand)]
enum DepsStackCommand {
    /// List declared dependency stack edges
    Status,
    /// Plan downstream updates for an upstream component/repo
    Plan {
        /// Upstream component or repository identifier from dependency_stack[].upstream.
        upstream: String,
    },
    /// Run downstream update commands for an upstream component/repo
    Apply {
        /// Upstream component or repository identifier from dependency_stack[].upstream.
        upstream: String,

        /// New manifest constraint to pass to provider-backed default update steps.
        #[arg(long, value_name = "CONSTRAINT")]
        to: Option<String>,

        /// Print the command plan without running commands.
        #[arg(long)]
        dry_run: bool,

        /// Skip provider-owned install/lockfile refresh after each manifest update.
        #[arg(long)]
        no_install: bool,

        /// Rebuild each downstream component through its generic build capability.
        #[arg(long)]
        rebuild: bool,
    },
}

pub fn run(args: DepsArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<serde_json::Value> {
    match args.command {
        DepsCommand::Status {
            component,
            package,
            path,
        } => {
            let output =
                deps::status_value(component.as_deref(), path.as_deref(), package.as_deref())?;
            Ok((output, 0))
        }
        DepsCommand::Install { component, path } => {
            let output = deps::install_value(component.as_deref(), path.as_deref())?;
            Ok((output, 0))
        }
        DepsCommand::Update {
            package,
            component,
            to,
            path,
            no_install,
            rebuild,
        } => {
            let output = deps::update_value(
                component.as_deref(),
                path.as_deref(),
                &package,
                to.as_deref(),
                !no_install,
                rebuild,
            )?;
            Ok((output, 0))
        }
        DepsCommand::Stack { command } => match command {
            DepsStackCommand::Status => {
                let output = deps::stack_status_value()?;
                Ok((output, 0))
            }
            DepsStackCommand::Plan { upstream } => {
                let output = deps::stack_plan_value(&upstream)?;
                Ok((output, 0))
            }
            DepsStackCommand::Apply {
                upstream,
                to,
                dry_run,
                no_install,
                rebuild,
            } => {
                let output = deps::stack_apply_value(
                    &upstream,
                    to.as_deref(),
                    dry_run,
                    !no_install,
                    rebuild,
                )?;
                Ok((output, 0))
            }
        },
    }
}
