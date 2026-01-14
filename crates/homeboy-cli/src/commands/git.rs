use clap::{Args, Subcommand};
use serde::Serialize;

use homeboy::git::{self, GitOutput};
use homeboy::BulkResult;

use crate::commands::version;

use super::CmdResult;

#[derive(Args)]
pub struct GitArgs {
    #[command(subcommand)]
    command: GitCommand,
}

#[derive(Subcommand)]
enum GitCommand {
    /// Show git status for a component
    Status {
        /// Use current working directory (ad-hoc mode)
        #[arg(long)]
        cwd: bool,

        /// JSON input spec for bulk operations.
        /// Use "-" for stdin, "@file.json" for file, or inline JSON string.
        #[arg(long)]
        json: Option<String>,

        /// Component ID (non-JSON mode)
        component_id: Option<String>,
    },
    /// Stage all changes and commit
    Commit {
        /// Use current working directory (ad-hoc mode)
        #[arg(long)]
        cwd: bool,

        /// JSON input spec for bulk operations.
        /// Use "-" for stdin, "@file.json" for file, or inline JSON string.
        #[arg(long)]
        json: Option<String>,

        /// Component ID (non-JSON mode)
        component_id: Option<String>,

        /// Commit message
        message: Option<String>,
    },
    /// Push local commits to remote
    Push {
        /// Use current working directory (ad-hoc mode)
        #[arg(long)]
        cwd: bool,

        /// JSON input spec for bulk operations.
        /// Use "-" for stdin, "@file.json" for file, or inline JSON string.
        #[arg(long)]
        json: Option<String>,

        /// Component ID (non-JSON mode)
        component_id: Option<String>,

        /// Push tags as well
        #[arg(long)]
        tags: bool,
    },
    /// Pull remote changes
    Pull {
        /// Use current working directory (ad-hoc mode)
        #[arg(long)]
        cwd: bool,

        /// JSON input spec for bulk operations.
        /// Use "-" for stdin, "@file.json" for file, or inline JSON string.
        #[arg(long)]
        json: Option<String>,

        /// Component ID (non-JSON mode)
        component_id: Option<String>,
    },
    /// Create a git tag
    Tag {
        /// Use current working directory (ad-hoc mode)
        #[arg(long)]
        cwd: bool,

        /// Component ID
        component_id: Option<String>,

        /// Tag name (e.g., v0.1.2)
        ///
        /// Required when using --cwd. Otherwise defaults to v<component version>.
        tag_name: Option<String>,

        /// Tag message (creates annotated tag)
        #[arg(short, long)]
        message: Option<String>,
    },
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum GitCommandOutput {
    Single(GitOutput),
    Bulk(BulkResult<GitOutput>),
}

pub fn run(args: GitArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<GitCommandOutput> {
    match args.command {
        GitCommand::Status {
            cwd,
            json,
            component_id,
        } => {
            // Priority: --cwd > --json > component_id
            if cwd {
                let output = git::status_cwd()?;
                let exit_code = output.exit_code;
                return Ok((GitCommandOutput::Single(output), exit_code));
            }

            if let Some(spec) = json {
                let output = git::status_bulk(&spec)?;
                let exit_code = if output.summary.failed > 0 { 1 } else { 0 };
                return Ok((GitCommandOutput::Bulk(output), exit_code));
            }

            let id = component_id.ok_or_else(|| {
                homeboy::Error::validation_invalid_argument(
                    "componentId",
                    "Missing componentId (or use --cwd or --json)",
                    None,
                    None,
                )
            })?;
            let output = git::status(&id)?;
            let exit_code = output.exit_code;
            Ok((GitCommandOutput::Single(output), exit_code))
        }
        GitCommand::Commit {
            cwd,
            json,
            component_id,
            message,
        } => {
            // Priority: --cwd > --json > component_id
            if cwd {
                let msg = message.ok_or_else(|| {
                    homeboy::Error::validation_invalid_argument(
                        "message",
                        "Missing commit message",
                        None,
                        None,
                    )
                })?;
                let output = git::commit_cwd(&msg)?;
                let exit_code = output.exit_code;
                return Ok((GitCommandOutput::Single(output), exit_code));
            }

            if let Some(spec) = json {
                let output = git::commit_bulk(&spec)?;
                let exit_code = if output.summary.failed > 0 { 1 } else { 0 };
                return Ok((GitCommandOutput::Bulk(output), exit_code));
            }

            let id = component_id.ok_or_else(|| {
                homeboy::Error::validation_invalid_argument(
                    "componentId",
                    "Missing componentId (or use --cwd or --json)",
                    None,
                    None,
                )
            })?;
            let msg = message.ok_or_else(|| {
                homeboy::Error::validation_invalid_argument(
                    "message",
                    "Missing commit message",
                    None,
                    None,
                )
            })?;
            let output = git::commit(&id, &msg)?;
            let exit_code = output.exit_code;
            Ok((GitCommandOutput::Single(output), exit_code))
        }
        GitCommand::Push {
            cwd,
            json,
            component_id,
            tags,
        } => {
            // Priority: --cwd > --json > component_id
            if cwd {
                let output = git::push_cwd(tags)?;
                let exit_code = output.exit_code;
                return Ok((GitCommandOutput::Single(output), exit_code));
            }

            if let Some(spec) = json {
                let output = git::push_bulk(&spec)?;
                let exit_code = if output.summary.failed > 0 { 1 } else { 0 };
                return Ok((GitCommandOutput::Bulk(output), exit_code));
            }

            let id = component_id.ok_or_else(|| {
                homeboy::Error::validation_invalid_argument(
                    "componentId",
                    "Missing componentId (or use --cwd or --json)",
                    None,
                    None,
                )
            })?;
            let output = git::push(&id, tags)?;
            let exit_code = output.exit_code;
            Ok((GitCommandOutput::Single(output), exit_code))
        }
        GitCommand::Pull {
            cwd,
            json,
            component_id,
        } => {
            // Priority: --cwd > --json > component_id
            if cwd {
                let output = git::pull_cwd()?;
                let exit_code = output.exit_code;
                return Ok((GitCommandOutput::Single(output), exit_code));
            }

            if let Some(spec) = json {
                let output = git::pull_bulk(&spec)?;
                let exit_code = if output.summary.failed > 0 { 1 } else { 0 };
                return Ok((GitCommandOutput::Bulk(output), exit_code));
            }

            let id = component_id.ok_or_else(|| {
                homeboy::Error::validation_invalid_argument(
                    "componentId",
                    "Missing componentId (or use --cwd or --json)",
                    None,
                    None,
                )
            })?;
            let output = git::pull(&id)?;
            let exit_code = output.exit_code;
            Ok((GitCommandOutput::Single(output), exit_code))
        }
        GitCommand::Tag {
            cwd,
            component_id,
            tag_name,
            message,
        } => {
            // Priority: --cwd > component_id
            if cwd {
                let tag = tag_name.ok_or_else(|| {
                    homeboy::Error::validation_invalid_argument(
                        "tagName",
                        "Tag name is required when using --cwd",
                        None,
                        None,
                    )
                })?;
                let output = git::tag_cwd(&tag, message.as_deref())?;
                let exit_code = output.exit_code;
                return Ok((GitCommandOutput::Single(output), exit_code));
            }

            let id = component_id.ok_or_else(|| {
                homeboy::Error::validation_invalid_argument(
                    "componentId",
                    "Missing componentId (or use --cwd)",
                    None,
                    None,
                )
            })?;

            let derived_tag_name = match tag_name {
                Some(name) => name,
                None => {
                    let (out, _) = version::show_version_output(&id)?;
                    format!("v{}", out.version)
                }
            };

            let output = git::tag(&id, &derived_tag_name, message.as_deref())?;
            let exit_code = output.exit_code;
            Ok((GitCommandOutput::Single(output), exit_code))
        }
    }
}
