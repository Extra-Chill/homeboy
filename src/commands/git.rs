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
        /// JSON input spec for bulk operations.
        /// Use "-" for stdin, "@file.json" for file, or inline JSON string.
        #[arg(long)]
        json: Option<String>,

        /// Component ID (non-JSON mode)
        component_id: Option<String>,
    },
    /// Commit changes (by default stages all, use flags for granular control)
    Commit {
        /// Component ID (optional if provided in JSON body)
        component_id: Option<String>,

        /// Commit message or JSON spec (auto-detected).
        /// Plain text: treated as commit message.
        /// JSON (starts with { or [): parsed as commit spec.
        /// @file.json: reads JSON from file.
        /// "-": reads JSON from stdin.
        spec: Option<String>,

        /// Explicit JSON spec (takes precedence over positional)
        #[arg(long, value_name = "JSON")]
        json: Option<String>,

        /// Commit message (CLI mode)
        #[arg(short, long)]
        message: Option<String>,

        /// Commit only staged changes (skip automatic git add)
        #[arg(long)]
        staged_only: bool,

        /// Stage and commit only these specific files
        #[arg(long, num_args = 1.., conflicts_with = "exclude")]
        files: Option<Vec<String>>,

        /// Stage all files except these (mutually exclusive with --files)
        #[arg(long, num_args = 1.., conflicts_with = "files")]
        exclude: Option<Vec<String>>,

        /// Explicit include list (repeatable)
        #[arg(long, num_args = 1.., conflicts_with = "exclude", conflicts_with = "files")]
        include: Option<Vec<String>>,
    },
    /// Push local commits to remote
    Push {
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
        /// JSON input spec for bulk operations.
        /// Use "-" for stdin, "@file.json" for file, or inline JSON string.
        #[arg(long)]
        json: Option<String>,

        /// Component ID (non-JSON mode)
        component_id: Option<String>,
    },
    /// Create a git tag
    Tag {
        /// Component ID
        component_id: Option<String>,

        /// Tag name (e.g., v0.1.2)
        ///
        /// Defaults to v<component version> if not provided.
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
        GitCommand::Status { json, component_id } => {
            if let Some(spec) = json {
                let output = git::status_bulk(&spec)?;
                let exit_code = if output.summary.failed > 0 { 1 } else { 0 };
                return Ok((GitCommandOutput::Bulk(output), exit_code));
            }

            let output = git::status(component_id.as_deref())?;
            let exit_code = output.exit_code;
            Ok((GitCommandOutput::Single(output), exit_code))
        }
        GitCommand::Commit {
            component_id,
            spec,
            json,
            message,
            staged_only,
            files,
            exclude,
            include,
        } => {
            // Explicit --json flag always uses JSON mode
            if let Some(json_spec) = json {
                let output = git::commit_from_json(component_id.as_deref(), &json_spec)?;
                return match output {
                    git::CommitJsonOutput::Single(o) => {
                        let exit_code = o.exit_code;
                        Ok((GitCommandOutput::Single(o), exit_code))
                    }
                    git::CommitJsonOutput::Bulk(b) => {
                        let exit_code = if b.summary.failed > 0 { 1 } else { 0 };
                        Ok((GitCommandOutput::Bulk(b), exit_code))
                    }
                };
            }

            // Auto-detect: check if positional spec looks like JSON or is a plain message
            let (inferred_message, json_spec) = match &spec {
                Some(s) => {
                    let trimmed = s.trim();
                    // JSON indicators: starts with { or [, uses @file, or - for stdin
                    let is_json = trimmed.starts_with('{')
                        || trimmed.starts_with('[')
                        || trimmed.starts_with('@')
                        || trimmed == "-";
                    if is_json {
                        (None, Some(s.clone()))
                    } else {
                        // Treat as plain commit message
                        (Some(s.clone()), None)
                    }
                }
                None => (None, None),
            };

            // JSON mode if auto-detected
            if let Some(json_str) = json_spec {
                let output = git::commit_from_json(component_id.as_deref(), &json_str)?;
                return match output {
                    git::CommitJsonOutput::Single(o) => {
                        let exit_code = o.exit_code;
                        Ok((GitCommandOutput::Single(o), exit_code))
                    }
                    git::CommitJsonOutput::Bulk(b) => {
                        let exit_code = if b.summary.failed > 0 { 1 } else { 0 };
                        Ok((GitCommandOutput::Bulk(b), exit_code))
                    }
                };
            }

            // CLI flag mode - use inferred message or explicit -m flag
            let final_message = inferred_message.or(message);
            let mut resolved_files = files;
            if resolved_files.is_none() {
                resolved_files = include;
            }

            let options = git::CommitOptions {
                staged_only,
                files: resolved_files,
                exclude,
            };
            let output = git::commit(component_id.as_deref(), final_message.as_deref(), options)?;
            let exit_code = output.exit_code;
            Ok((GitCommandOutput::Single(output), exit_code))
        }
        GitCommand::Push {
            json,
            component_id,
            tags,
        } => {
            if let Some(spec) = json {
                let output = git::push_bulk(&spec)?;
                let exit_code = if output.summary.failed > 0 { 1 } else { 0 };
                return Ok((GitCommandOutput::Bulk(output), exit_code));
            }

            let output = git::push(component_id.as_deref(), tags)?;
            let exit_code = output.exit_code;
            Ok((GitCommandOutput::Single(output), exit_code))
        }
        GitCommand::Pull { json, component_id } => {
            if let Some(spec) = json {
                let output = git::pull_bulk(&spec)?;
                let exit_code = if output.summary.failed > 0 { 1 } else { 0 };
                return Ok((GitCommandOutput::Bulk(output), exit_code));
            }

            let output = git::pull(component_id.as_deref())?;
            let exit_code = output.exit_code;
            Ok((GitCommandOutput::Single(output), exit_code))
        }
        GitCommand::Tag {
            component_id,
            tag_name,
            message,
        } => {
            // Derive tag from version if not provided
            let final_tag = match tag_name {
                Some(name) => name,
                None => {
                    // Need component_id to look up version
                    let id = component_id.as_ref().ok_or_else(|| {
                        homeboy::Error::validation_invalid_argument(
                            "componentId",
                            "Missing componentId",
                            None,
                            Some(vec![
                                "Provide a component ID: homeboy git tag <component-id>".to_string(),
                                "Or specify a tag name: homeboy git tag <component-id> <tag-name>"
                                    .to_string(),
                            ]),
                        )
                    })?;
                    let (out, _) = version::show_version_output(id)?;
                    format!("v{}", out.version)
                }
            };

            let output = git::tag(
                component_id.as_deref(),
                Some(&final_tag),
                message.as_deref(),
            )?;
            let exit_code = output.exit_code;
            Ok((GitCommandOutput::Single(output), exit_code))
        }
    }
}
