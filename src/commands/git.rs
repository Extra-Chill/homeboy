use clap::{Args, Subcommand};
use serde::Serialize;

use homeboy::git::{
    self, GitOutput, GithubFindOutput, GithubIssueOutput, GithubPrOutput, IssueCreateOptions,
    IssueFindOptions, IssueState, PrCommentMode, PrCommentOptions, PrCreateOptions, PrEditOptions,
    PrFindOptions, PrState,
};
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
    /// Manage GitHub issues for a component
    Issue(IssueArgs),
    /// Manage GitHub pull requests for a component
    Pr(PrArgs),
}

// ---------------------------------------------------------------------------
// `git issue` subcommand tree
// ---------------------------------------------------------------------------

#[derive(Args)]
pub struct IssueArgs {
    #[command(subcommand)]
    command: IssueCommand,
}

#[derive(Subcommand)]
enum IssueCommand {
    /// Create a new issue
    Create {
        /// Component ID
        component_id: String,

        /// Issue title
        #[arg(short, long)]
        title: String,

        /// Issue body (markdown). Prefer --body-file for long content.
        #[arg(short, long, conflicts_with = "body_file")]
        body: Option<String>,

        /// Read body from a file ("-" for stdin)
        #[arg(long, value_name = "PATH")]
        body_file: Option<String>,

        /// Issue label (repeatable)
        #[arg(short, long)]
        label: Vec<String>,
    },
    /// Comment on an existing issue
    Comment {
        /// Component ID
        component_id: String,

        /// Issue number
        #[arg(short, long)]
        number: u64,

        /// Comment body (markdown). Prefer --body-file for long content.
        #[arg(short, long, conflicts_with = "body_file")]
        body: Option<String>,

        /// Read body from a file ("-" for stdin)
        #[arg(long, value_name = "PATH")]
        body_file: Option<String>,
    },
    /// Find issues matching filters (dedup primitive)
    Find {
        /// Component ID
        component_id: String,

        /// Exact title match
        #[arg(short, long)]
        title: Option<String>,

        /// Required label (repeatable — all labels must be present)
        #[arg(short, long)]
        label: Vec<String>,

        /// State filter: open (default), closed, all
        #[arg(short, long, default_value = "open")]
        state: String,

        /// Max results (default 30)
        #[arg(long, default_value_t = 30)]
        limit: usize,
    },
}

// ---------------------------------------------------------------------------
// `git pr` subcommand tree
// ---------------------------------------------------------------------------

#[derive(Args)]
pub struct PrArgs {
    #[command(subcommand)]
    command: PrCommand,
}

#[derive(Subcommand)]
enum PrCommand {
    /// Create a new pull request
    Create {
        /// Component ID
        component_id: String,

        /// Base branch (target of the PR)
        #[arg(short, long)]
        base: String,

        /// Head branch (source of the PR)
        #[arg(short = 'H', long)]
        head: String,

        /// PR title
        #[arg(short, long)]
        title: String,

        /// PR body (markdown). Prefer --body-file for long content.
        #[arg(short = 'B', long, conflicts_with = "body_file")]
        body: Option<String>,

        /// Read body from a file ("-" for stdin)
        #[arg(long, value_name = "PATH")]
        body_file: Option<String>,

        /// Open as draft
        #[arg(long)]
        draft: bool,
    },
    /// Edit an existing PR's title or body
    Edit {
        /// Component ID
        component_id: String,

        /// PR number
        #[arg(short, long)]
        number: u64,

        /// New title
        #[arg(short, long)]
        title: Option<String>,

        /// New body (markdown)
        #[arg(short = 'B', long, conflicts_with = "body_file")]
        body: Option<String>,

        /// Read body from a file ("-" for stdin)
        #[arg(long, value_name = "PATH")]
        body_file: Option<String>,
    },
    /// Find PRs matching filters
    Find {
        /// Component ID
        component_id: String,

        /// Base branch filter
        #[arg(short, long)]
        base: Option<String>,

        /// Head branch filter
        #[arg(short = 'H', long)]
        head: Option<String>,

        /// State filter: open (default), closed, merged, all
        #[arg(short, long, default_value = "open")]
        state: String,

        /// Max results (default 30)
        #[arg(long, default_value_t = 30)]
        limit: usize,
    },
    /// Post a comment on a PR. Three modes:
    ///
    /// 1. Plain: no marker flags — a fresh comment is appended.
    /// 2. Sticky single-section (#1334): `--key <k>` finds-or-updates the one
    ///    comment tagged `<!-- homeboy:key=<k> -->`. The whole `--body` becomes
    ///    the comment body.
    /// 3. Sectioned (#1348): `--comment-key <outer> --section-key <inner>`
    ///    merges `--body` into section `<inner>` of the shared comment tagged
    ///    `<!-- homeboy:comment-key=<outer> -->`. Other sections are preserved.
    ///    `--header` sets the line printed after the outer marker on new
    ///    comments. `--section-order` pins section ordering (CSV of keys);
    ///    default is alphabetical.
    ///
    /// Modes 2 and 3 are mutually exclusive. `--key` with `--comment-key` or
    /// `--section-key` is an error.
    Comment {
        /// Component ID
        component_id: String,

        /// PR number
        #[arg(short, long)]
        number: u64,

        /// Comment body (markdown). Prefer --body-file for long content.
        #[arg(short = 'B', long, conflicts_with = "body_file")]
        body: Option<String>,

        /// Read body from a file ("-" for stdin)
        #[arg(long, value_name = "PATH")]
        body_file: Option<String>,

        /// Sticky whole-body key (mode 2, PR #1334).
        /// Mutually exclusive with --comment-key / --section-key.
        #[arg(short, long, conflicts_with_all = ["comment_key", "section_key"])]
        key: Option<String>,

        /// Sectioned mode: outer shared-comment key (mode 3, #1348).
        /// Must be combined with --section-key.
        #[arg(long, requires = "section_key")]
        comment_key: Option<String>,

        /// Sectioned mode: inner per-section key (mode 3, #1348).
        /// Must be combined with --comment-key.
        #[arg(long, requires = "comment_key")]
        section_key: Option<String>,

        /// Sectioned mode: optional header line written after the outer
        /// marker on freshly-created shared comments (e.g.
        /// "## Homeboy Results — `<component>`"). Existing comment headers
        /// are preserved on merge.
        #[arg(long, requires = "comment_key")]
        header: Option<String>,

        /// Sectioned mode: CSV of section keys in desired order. Sections
        /// listed here come first in the given order; others are appended
        /// alphabetically. Example: `--section-order lint,test,audit`.
        #[arg(long, requires = "comment_key", value_delimiter = ',')]
        section_order: Option<Vec<String>>,
    },
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum GitCommandOutput {
    Single(GitOutput),
    Bulk(BulkResult<GitOutput>),
    Issue(GithubIssueOutput),
    Pr(GithubPrOutput),
    Find(GithubFindOutput),
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
                amend: false,
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
                                "Provide a component ID: homeboy git tag <component-id>"
                                    .to_string(),
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
        GitCommand::Issue(args) => run_issue(args),
        GitCommand::Pr(args) => run_pr(args),
    }
}

// ---------------------------------------------------------------------------
// `git issue` dispatch
// ---------------------------------------------------------------------------

fn run_issue(args: IssueArgs) -> CmdResult<GitCommandOutput> {
    match args.command {
        IssueCommand::Create {
            component_id,
            title,
            body,
            body_file,
            label,
        } => {
            let body = resolve_body(body, body_file)?.unwrap_or_default();
            let output = git::issue_create(
                Some(&component_id),
                IssueCreateOptions {
                    title,
                    body,
                    labels: label,
                },
            )?;
            Ok((GitCommandOutput::Issue(output), 0))
        }
        IssueCommand::Comment {
            component_id,
            number,
            body,
            body_file,
        } => {
            let body = resolve_body(body, body_file)?.ok_or_else(|| {
                homeboy::Error::validation_invalid_argument(
                    "body",
                    "Comment body is required (--body or --body-file)",
                    None,
                    None,
                )
            })?;
            let output = git::issue_comment(Some(&component_id), number, &body)?;
            Ok((GitCommandOutput::Issue(output), 0))
        }
        IssueCommand::Find {
            component_id,
            title,
            label,
            state,
            limit,
        } => {
            let state = parse_issue_state(&state)?;
            let output = git::issue_find(
                Some(&component_id),
                IssueFindOptions {
                    title,
                    labels: label,
                    state,
                    limit,
                },
            )?;
            Ok((GitCommandOutput::Find(output), 0))
        }
    }
}

// ---------------------------------------------------------------------------
// `git pr` dispatch
// ---------------------------------------------------------------------------

fn run_pr(args: PrArgs) -> CmdResult<GitCommandOutput> {
    match args.command {
        PrCommand::Create {
            component_id,
            base,
            head,
            title,
            body,
            body_file,
            draft,
        } => {
            let body = resolve_body(body, body_file)?.unwrap_or_default();
            let output = git::pr_create(
                Some(&component_id),
                PrCreateOptions {
                    base,
                    head,
                    title,
                    body,
                    draft,
                },
            )?;
            Ok((GitCommandOutput::Pr(output), 0))
        }
        PrCommand::Edit {
            component_id,
            number,
            title,
            body,
            body_file,
        } => {
            let body = resolve_body(body, body_file)?;
            let output = git::pr_edit(
                Some(&component_id),
                PrEditOptions {
                    number,
                    title,
                    body,
                },
            )?;
            Ok((GitCommandOutput::Pr(output), 0))
        }
        PrCommand::Find {
            component_id,
            base,
            head,
            state,
            limit,
        } => {
            let state = parse_pr_state(&state)?;
            let output = git::pr_find(
                Some(&component_id),
                PrFindOptions {
                    base,
                    head,
                    state,
                    limit,
                },
            )?;
            Ok((GitCommandOutput::Find(output), 0))
        }
        PrCommand::Comment {
            component_id,
            number,
            body,
            body_file,
            key,
            comment_key,
            section_key,
            header,
            section_order,
        } => {
            let body = resolve_body(body, body_file)?.ok_or_else(|| {
                homeboy::Error::validation_invalid_argument(
                    "body",
                    "Comment body is required (--body or --body-file)",
                    None,
                    None,
                )
            })?;

            let mode = match (key, comment_key, section_key) {
                (Some(k), None, None) => PrCommentMode::StickyWholeBody { key: k },
                (None, Some(ck), Some(sk)) => PrCommentMode::Sectioned {
                    comment_key: ck,
                    section_key: sk,
                    header,
                    section_order,
                },
                (None, None, None) => {
                    // Header / section_order without the pair — clap already
                    // caught this via `requires = "comment_key"`, but double-check.
                    PrCommentMode::Fresh
                }
                // Remaining cases are impossible due to clap `requires` /
                // `conflicts_with_all`, but keep the match exhaustive.
                _ => unreachable!(
                    "clap argument parsing should have rejected incompatible --key / --comment-key / --section-key combos"
                ),
            };

            let output =
                git::pr_comment(Some(&component_id), PrCommentOptions { number, body, mode })?;
            Ok((GitCommandOutput::Pr(output), 0))
        }
    }
}

// ---------------------------------------------------------------------------
// Small input helpers
// ---------------------------------------------------------------------------

/// Resolve a body argument from either inline `--body` or a file path.
/// Returns `Ok(None)` if neither is set. Supports `-` for stdin.
fn resolve_body(inline: Option<String>, file: Option<String>) -> homeboy::Result<Option<String>> {
    if let Some(body) = inline {
        return Ok(Some(body));
    }
    let Some(path) = file else {
        return Ok(None);
    };

    if path == "-" {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf).map_err(|e| {
            homeboy::Error::internal_io(
                format!("Failed to read body from stdin: {}", e),
                Some("stdin".into()),
            )
        })?;
        return Ok(Some(buf));
    }

    let content = std::fs::read_to_string(&path).map_err(|e| {
        homeboy::Error::internal_io(
            format!("Failed to read body file: {}", e),
            Some(path.clone()),
        )
    })?;
    Ok(Some(content))
}

fn parse_issue_state(s: &str) -> homeboy::Result<IssueState> {
    match s {
        "open" => Ok(IssueState::Open),
        "closed" => Ok(IssueState::Closed),
        "all" => Ok(IssueState::All),
        other => Err(homeboy::Error::validation_invalid_argument(
            "state",
            format!("Unknown issue state '{}'", other),
            None,
            Some(vec!["Use one of: open, closed, all".into()]),
        )),
    }
}

fn parse_pr_state(s: &str) -> homeboy::Result<PrState> {
    match s {
        "open" => Ok(PrState::Open),
        "closed" => Ok(PrState::Closed),
        "merged" => Ok(PrState::Merged),
        "all" => Ok(PrState::All),
        other => Err(homeboy::Error::validation_invalid_argument(
            "state",
            format!("Unknown PR state '{}'", other),
            None,
            Some(vec!["Use one of: open, closed, merged, all".into()]),
        )),
    }
}
