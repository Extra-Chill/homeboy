use clap::{Args, Subcommand};
use serde::Serialize;

use homeboy::component;
use homeboy::release::{BatchReleaseResult, ReleaseCommandResult};
use homeboy::version::{read_component_version, read_version, VersionTargetInfo};

use super::utils::args::{DryRunArgs, HiddenJsonArgs};
use super::CmdResult;

#[derive(Serialize)]
#[serde(untagged)]
pub enum VersionOutput {
    Show(VersionShowOutput),
    Bump(VersionBumpOutput),
    BatchBump(VersionBatchBumpOutput),
}

#[derive(Args)]
pub struct VersionArgs {
    #[command(subcommand)]
    command: VersionCommand,
}

#[derive(Subcommand)]
enum VersionCommand {
    /// Show current version (default: homeboy binary)
    Show {
        /// Component ID (optional - shows homeboy binary version when omitted)
        component_id: Option<String>,

        /// Override local_path for version file lookup
        #[arg(long)]
        path: Option<String>,
    },

    /// Bump version and release (alias for `homeboy release`)
    Bump {
        /// Component ID(s) to release
        components: Vec<String>,

        /// Release all components in a project that need a version bump
        #[arg(long, short = 'p')]
        project: Option<String>,

        /// Only release components with unreleased code commits (use with --project)
        #[arg(long)]
        outdated: bool,

        /// Override local_path for version file lookup (single component only)
        #[arg(long)]
        path: Option<String>,

        #[command(flatten)]
        dry_run_args: DryRunArgs,

        #[command(flatten)]
        _json: HiddenJsonArgs,

        /// Deploy to all projects using this component after release
        #[arg(long)]
        deploy: bool,

        /// Recover from an interrupted release (tag + push current version)
        #[arg(long)]
        recover: bool,

        /// Skip pre-release lint and test checks
        #[arg(long)]
        skip_checks: bool,

        /// Force a specific version bump: major, minor, patch, or an explicit version (e.g. 2.0.0).
        /// Overrides auto-detection from commit history.
        #[arg(long)]
        bump: Option<String>,

        /// Deprecated: use --bump major instead.
        #[arg(long, hide = true)]
        major: bool,

        /// Skip publish/package steps (version bump + tag + push only).
        /// Use when CI handles publishing after the tag is pushed.
        #[arg(long)]
        skip_publish: bool,
    },
}

#[derive(Serialize)]
pub struct VersionShowOutput {
    command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    component_id: Option<String>,
    pub version: String,
    targets: Vec<VersionTargetInfo>,
}

#[derive(Serialize)]
#[serde(tag = "command", rename = "release")]
pub struct VersionBumpOutput {
    pub result: ReleaseCommandResult,
}

#[derive(Serialize)]
#[serde(tag = "command", rename = "release.batch")]
pub struct VersionBatchBumpOutput {
    pub result: BatchReleaseResult,
}

pub fn run(args: VersionArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<VersionOutput> {
    match args.command {
        VersionCommand::Show { component_id, path } => {
            let info = if let Some(ref p) = path {
                let comp = component::resolve_effective(component_id.as_deref(), Some(p), None)?;
                read_component_version(&comp)?
            } else if component_id.is_some() {
                // Explicit component ID or CWD discovery
                let comp = component::resolve_effective(component_id.as_deref(), None, None)?;
                read_component_version(&comp)?
            } else {
                // No component ID and no --path: try CWD discovery first,
                // fall back to showing homeboy binary version
                match component::resolve_effective(None, None, None) {
                    Ok(comp) => read_component_version(&comp)?,
                    Err(_) => read_version(None)?,
                }
            };

            let display_id = component_id.or_else(|| {
                // Include discovered component ID in output
                if info.targets.is_empty() {
                    None
                } else {
                    component::resolve_effective(None, None, None)
                        .ok()
                        .map(|c| c.id)
                }
            });

            Ok((
                VersionOutput::Show(VersionShowOutput {
                    command: "version.show".to_string(),
                    component_id: display_id,
                    version: info.version,
                    targets: info.targets,
                }),
                0,
            ))
        }
        VersionCommand::Bump {
            components,
            project,
            outdated,
            path,
            dry_run_args,
            _json: _,
            deploy,
            recover,
            skip_checks,
            bump,
            major,
            skip_publish,
        } => {
            // Delegate to the release command's batch infrastructure
            let release_args = super::release::ReleaseArgs::from_parts(
                components,
                project,
                outdated,
                path,
                dry_run_args.dry_run,
                deploy,
                recover,
                skip_checks,
                major,
                skip_publish,
                bump,
            );

            match super::release::run(release_args, _global)? {
                (super::release::ReleaseCommandOutput::Single(output), exit_code) => Ok((
                    VersionOutput::Bump(VersionBumpOutput {
                        result: output.result,
                    }),
                    exit_code,
                )),
                (super::release::ReleaseCommandOutput::Batch(output), exit_code) => Ok((
                    VersionOutput::BatchBump(VersionBatchBumpOutput {
                        result: output.result,
                    }),
                    exit_code,
                )),
            }
        }
    }
}

pub fn show_version_output(component_id: &str) -> CmdResult<VersionShowOutput> {
    let info = read_version(Some(component_id))?;

    Ok((
        VersionShowOutput {
            command: "version.show".to_string(),
            component_id: Some(component_id.to_string()),
            version: info.version,
            targets: info.targets,
        },
        0,
    ))
}
