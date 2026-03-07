use clap::{Args, Subcommand};
use serde::Serialize;

use homeboy::component;
use homeboy::release::{self, ReleasePlan, ReleaseRun};
use homeboy::version::{
    read_component_version, read_version, undo_version_bump, UndoResult, VersionTargetInfo,
};

use super::release::BumpType;

use super::CmdResult;

#[derive(Serialize)]
#[serde(untagged)]
#[allow(clippy::large_enum_variant)]
pub enum VersionOutput {
    Show(VersionShowOutput),
    Bump(VersionBumpOutput),
    Undo(VersionUndoOutput),
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
    /// [DEPRECATED] Use 'homeboy version bump' or 'homeboy release' instead. See issue #259.
    #[command(visible_aliases = ["edit", "merge"])]
    Set {
        /// Component ID
        component_id: Option<String>,

        /// New version (e.g., 1.2.3)
        new_version: String,

        /// Override local_path for version file lookup
        #[arg(long)]
        path: Option<String>,
    },
    /// Bump version with semantic versioning (alias for `release`)
    Bump {
        /// Component ID
        component_id: String,

        /// Version bump type (patch, minor, major)
        bump_type: BumpType,

        /// Preview what will happen without making changes
        #[arg(long)]
        dry_run: bool,

        /// Override local_path for version operations
        #[arg(long)]
        path: Option<String>,

        /// Allow bump type lower than commit-derived semver recommendation
        #[arg(long)]
        allow_underbump: bool,
    },
    /// Undo the last version bump (reset or revert the release commit)
    Undo {
        /// Component ID
        component_id: String,

        /// Preview what will happen without making changes
        #[arg(long)]
        dry_run: bool,
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
pub struct VersionBumpOutput {
    command: String,
    component_id: String,
    bump_type: String,
    dry_run: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    plan: Option<ReleasePlan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    run: Option<ReleaseRun>,
}

#[derive(Serialize)]
pub struct VersionUndoOutput {
    command: String,
    component_id: String,
    #[serde(flatten)]
    result: UndoResult,
}

pub fn run(args: VersionArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<VersionOutput> {
    match args.command {
        VersionCommand::Show { component_id, path } => {
            let info = if let Some(ref p) = path {
                // With --path: resolve component, override local_path
                let mut comp = component::resolve(component_id.as_deref())?;
                comp.local_path = p.clone();
                read_component_version(&comp)?
            } else if component_id.is_some() {
                // Explicit component ID or CWD discovery
                let comp = component::resolve(component_id.as_deref())?;
                read_component_version(&comp)?
            } else {
                // No component ID and no --path: try CWD discovery first,
                // fall back to showing homeboy binary version
                match component::resolve(None) {
                    Ok(comp) => read_component_version(&comp)?,
                    Err(_) => read_version(None)?,
                }
            };

            let display_id = component_id.or_else(|| {
                // Include discovered component ID in output
                if info.targets.is_empty() {
                    None
                } else {
                    component::resolve(None).ok().map(|c| c.id)
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
        VersionCommand::Set {
            component_id: _,
            new_version: _,
            path: _,
        } => Err(homeboy::Error::validation_invalid_argument(
            "version set",
            "'version set' has been deprecated. It skips changelog finalization, hooks, \
                 and push — producing incomplete releases. Use 'homeboy version bump' or \
                 'homeboy release' instead, which handle the full release pipeline atomically.",
            None,
            None,
        )
        .with_hint("homeboy version bump <component> patch".to_string())
        .with_hint("homeboy release <component> patch".to_string())
        .with_hint("See: https://github.com/Extra-Chill/homeboy/issues/259".to_string())),
        VersionCommand::Bump {
            component_id,
            bump_type,
            dry_run,
            path,
            allow_underbump,
        } => {
            let options = release::ReleaseOptions {
                bump_type: bump_type.as_str().to_string(),
                dry_run,
                path_override: path,
                skip_checks: false,
                allow_underbump,
                skip_publish: false,
            };

            if dry_run {
                let plan = release::plan(&component_id, &options)?;
                Ok((
                    VersionOutput::Bump(VersionBumpOutput {
                        command: "version.bump".to_string(),
                        component_id,
                        bump_type: options.bump_type,
                        dry_run: true,
                        plan: Some(plan),
                        run: None,
                    }),
                    0,
                ))
            } else {
                let run_result = release::run(&component_id, &options)?;
                super::release::display_release_summary(&run_result);

                // Exit code 3 when post-release hooks failed (matches `release` command behavior)
                let exit_code = if super::release::has_post_release_warnings(&run_result) {
                    3
                } else {
                    0
                };

                Ok((
                    VersionOutput::Bump(VersionBumpOutput {
                        command: "version.bump".to_string(),
                        component_id,
                        bump_type: options.bump_type,
                        dry_run: false,
                        plan: None,
                        run: Some(run_result),
                    }),
                    exit_code,
                ))
            }
        }
        VersionCommand::Undo {
            component_id,
            dry_run,
        } => {
            let result = undo_version_bump(&component_id, dry_run)?;
            Ok((
                VersionOutput::Undo(VersionUndoOutput {
                    command: "version.undo".to_string(),
                    component_id,
                    result,
                }),
                0,
            ))
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
