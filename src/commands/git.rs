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
    /// Commit changes (by default stages all, use flags for granular control)
    Commit {
        /// Use current working directory (ad-hoc mode)
        #[arg(long)]
        cwd: bool,

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
            if let Some(spec) = json {
                let output = git::status_bulk(&spec)?;
                let exit_code = if output.summary.failed > 0 { 1 } else { 0 };
                return Ok((GitCommandOutput::Bulk(output), exit_code));
            }

            // --cwd or component_id (None = CWD)
            let target = if cwd { None } else { component_id.as_deref() };
            let output = git::status(target)?;
            let exit_code = output.exit_code;
            Ok((GitCommandOutput::Single(output), exit_code))
        }
        GitCommand::Commit {
            cwd,
            component_id,
            spec,
            json,
            message,
            staged_only,
            files,
            exclude,
            include,
        } => {
            // When --cwd is set, component_id is ignored. If user passed a positional
            // argument it was likely intended as the message/spec. Shift it.
            let effective_spec = if cwd && component_id.is_some() && spec.is_none() {
                component_id.clone()
            } else {
                spec.clone()
            };

            // Explicit --json flag always uses JSON mode
            if let Some(json_spec) = json {
                let target = if cwd { None } else { component_id.as_deref() };
                let output = git::commit_from_json(target, &json_spec)?;
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
            let (inferred_message, json_spec) = match &effective_spec {
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
                let target = if cwd { None } else { component_id.as_deref() };
                let output = git::commit_from_json(target, &json_str)?;
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
            let target = if cwd { None } else { component_id.as_deref() };
            let mut resolved_files = files;
            if resolved_files.is_none() {
                resolved_files = include;
            }

            let options = git::CommitOptions {
                staged_only,
                files: resolved_files,
                exclude,
            };
            let output = git::commit(target, final_message.as_deref(), options)?;
            let exit_code = output.exit_code;
            Ok((GitCommandOutput::Single(output), exit_code))
        }
        GitCommand::Push {
            cwd,
            json,
            component_id,
            tags,
        } => {
            if let Some(spec) = json {
                let output = git::push_bulk(&spec)?;
                let exit_code = if output.summary.failed > 0 { 1 } else { 0 };
                return Ok((GitCommandOutput::Bulk(output), exit_code));
            }

            // --cwd or component_id (None = CWD)
            let target = if cwd { None } else { component_id.as_deref() };
            let output = git::push(target, tags)?;
            let exit_code = output.exit_code;
            Ok((GitCommandOutput::Single(output), exit_code))
        }
        GitCommand::Pull {
            cwd,
            json,
            component_id,
        } => {
            if let Some(spec) = json {
                let output = git::pull_bulk(&spec)?;
                let exit_code = if output.summary.failed > 0 { 1 } else { 0 };
                return Ok((GitCommandOutput::Bulk(output), exit_code));
            }

            // --cwd or component_id (None = CWD)
            let target = if cwd { None } else { component_id.as_deref() };
            let output = git::pull(target)?;
            let exit_code = output.exit_code;
            Ok((GitCommandOutput::Single(output), exit_code))
        }
        GitCommand::Tag {
            cwd,
            component_id,
            tag_name,
            message,
        } => {
            if cwd {
                // CWD mode: core validates tag_name
                let output = git::tag(None, tag_name.as_deref(), message.as_deref())?;
                let exit_code = output.exit_code;
                return Ok((GitCommandOutput::Single(output), exit_code));
            }

            // Component mode: derive tag from version if not provided
            let final_tag = match tag_name {
                Some(name) => name,
                None => {
                    // Need component_id to look up version
                    let id = component_id.as_ref().ok_or_else(|| {
                        homeboy::Error::validation_invalid_argument(
                            "componentId",
                            "Missing componentId (required to derive tag from version)",
                            None,
                            None,
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
