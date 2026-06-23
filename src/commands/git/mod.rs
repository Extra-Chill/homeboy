use clap::{Args, Subcommand};

use homeboy::core::git::{self, CherryPickOptions, PushOptions, RebaseOptions};

use crate::commands::version;

use super::CmdResult;

mod args;
mod helpers;
mod issue;
mod output;
mod pr;

#[cfg(test)]
mod tests;

pub use args::{IssueArgs, PrArgs, PrPolicyArgs};
pub use output::GitCommandOutput;

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

        /// Component ID (non-JSON mode). When omitted, the component is
        /// auto-detected from CWD via the registry or a portable
        /// `homeboy.json`.
        component_id: Option<String>,

        /// Workspace path to operate on directly. Useful for unregistered
        /// checkouts (CI runners, ad-hoc clones, worktrees).
        #[arg(long, value_name = "PATH")]
        path: Option<String>,
    },
    /// Commit changes (by default stages all, use flags for granular control)
    Commit {
        /// Component ID (optional if provided in JSON body or auto-detected
        /// from CWD).
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

        /// Workspace path to operate on directly. Useful for unregistered
        /// checkouts (CI runners, ad-hoc clones, worktrees).
        #[arg(long, value_name = "PATH")]
        path: Option<String>,
    },
    /// Push local commits to remote
    Push {
        /// JSON input spec for bulk operations.
        /// Use "-" for stdin, "@file.json" for file, or inline JSON string.
        #[arg(long)]
        json: Option<String>,

        /// Component ID (non-JSON mode). When omitted, the component is
        /// auto-detected from CWD via the registry or a portable
        /// `homeboy.json`.
        component_id: Option<String>,

        /// Push tags as well
        #[arg(long)]
        tags: bool,

        /// Use `--force-with-lease` for safe force-pushes (e.g. after a
        /// rebase). Refuses to overwrite the remote if it has commits the
        /// local ref hasn't seen. Plain `--force` is intentionally not
        /// exposed.
        #[arg(long)]
        force_with_lease: bool,

        /// Push to this remote URL directly instead of the configured upstream.
        /// Prefer this without credentials plus --token; embedding tokens in
        /// the URL can expose them in process listings.
        #[arg(long, value_name = "URL")]
        remote_url: Option<String>,

        /// GitHub token to inject into --remote-url for this invocation.
        /// Requires a https://github.com/... remote URL.
        #[arg(long, value_name = "TOKEN", requires = "remote_url")]
        token: Option<String>,

        /// Explicit push refspec, e.g. HEAD:refs/heads/my-branch.
        #[arg(long, value_name = "REFSPEC")]
        refspec: Option<String>,

        /// Clear GitHub Actions checkout's auth extraheader so URL auth wins.
        #[arg(long)]
        strip_extraheader: bool,

        /// Workspace path to operate on directly. Useful for unregistered
        /// checkouts (CI runners, ad-hoc clones, worktrees).
        #[arg(long, value_name = "PATH")]
        path: Option<String>,
    },
    /// Rebase the current branch onto another ref.
    ///
    /// Default (no `--onto`) rebases onto the current branch's tracked
    /// upstream (`@{upstream}`), same semantics as `git pull --rebase`.
    /// Git's default rebase drops commits whose patch-id matches a commit
    /// already in upstream — squash-merged PRs are NOT dropped (different
    /// patch-id); that case will land in a follow-up.
    ///
    /// On conflict, the operation returns a failed result with git's
    /// stderr. Resolve manually, then re-run with `--continue` or
    /// `--abort`.
    Rebase {
        /// Component ID. When omitted, auto-detected from CWD.
        component_id: Option<String>,

        /// Target ref to rebase onto. Defaults to the current branch's
        /// tracked upstream (`@{upstream}`).
        #[arg(long, value_name = "REF")]
        onto: Option<String>,

        /// Continue an in-progress rebase after manual conflict resolution.
        /// Mutually exclusive with `--abort`.
        #[arg(long, conflicts_with = "abort")]
        r#continue: bool,

        /// Abort an in-progress rebase and return to the pre-rebase state.
        #[arg(long)]
        abort: bool,

        /// Workspace path to operate on directly.
        #[arg(long, value_name = "PATH")]
        path: Option<String>,
    },
    /// Cherry-pick one or more commits onto the current branch.
    ///
    /// Accepts SHAs, branch names, and ranges (`<a>..<b>`) as positional
    /// args. Use `--pr <n>` to pick all commits from a GitHub PR via `gh`.
    /// Both can be combined.
    ///
    /// On conflict, returns a failed result. Resolve manually, then
    /// re-run with `--continue` or `--abort`.
    CherryPick {
        /// Component ID. When omitted, auto-detected from CWD.
        #[arg(long, short)]
        component_id: Option<String>,

        /// Commit refs to pick: SHAs, branches, ranges (`<a>..<b>`).
        /// Multiple positional args allowed.
        #[arg(value_name = "REF")]
        refs: Vec<String>,

        /// Cherry-pick all commits from a GitHub PR (repeatable).
        /// Resolved via `gh pr view <n> --json commits`.
        #[arg(long, value_name = "NUMBER")]
        pr: Vec<u64>,

        /// Continue an in-progress cherry-pick after manual conflict
        /// resolution. Mutually exclusive with `--abort`.
        #[arg(long, conflicts_with = "abort")]
        r#continue: bool,

        /// Abort an in-progress cherry-pick.
        #[arg(long)]
        abort: bool,

        /// Workspace path to operate on directly.
        #[arg(long, value_name = "PATH")]
        path: Option<String>,
    },
    /// Pull remote changes
    Pull {
        /// JSON input spec for bulk operations.
        /// Use "-" for stdin, "@file.json" for file, or inline JSON string.
        #[arg(long)]
        json: Option<String>,

        /// Component ID (non-JSON mode). When omitted, the component is
        /// auto-detected from CWD via the registry or a portable
        /// `homeboy.json`.
        component_id: Option<String>,

        /// Workspace path to operate on directly. Useful for unregistered
        /// checkouts (CI runners, ad-hoc clones, worktrees).
        #[arg(long, value_name = "PATH")]
        path: Option<String>,
    },
    /// Create a git tag
    Tag {
        /// Component ID. When omitted, the component is auto-detected from
        /// CWD via the registry or a portable `homeboy.json`.
        component_id: Option<String>,

        /// Tag name (e.g., v0.1.2)
        ///
        /// Defaults to v<component version> if not provided.
        tag_name: Option<String>,

        /// Tag message (creates annotated tag)
        #[arg(short, long)]
        message: Option<String>,

        /// Workspace path to operate on directly. Useful for unregistered
        /// checkouts (CI runners, ad-hoc clones, worktrees).
        #[arg(long, value_name = "PATH")]
        path: Option<String>,
    },
    /// Manage GitHub issues for a component
    Issue(IssueArgs),
    /// Manage GitHub pull requests for a component
    Pr(PrArgs),
}

pub fn run(args: GitArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<GitCommandOutput> {
    match args.command {
        GitCommand::Status {
            json,
            component_id,
            path,
        } => {
            if let Some(spec) = json {
                let output = git::status_bulk(&spec)?;
                let exit_code = if output.summary.failed > 0 { 1 } else { 0 };
                return Ok((GitCommandOutput::Bulk(output), exit_code));
            }

            let output = git::status_at(component_id.as_deref(), path.as_deref())?;
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
            path,
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
            let output = git::commit_at(
                component_id.as_deref(),
                final_message.as_deref(),
                options,
                path.as_deref(),
            )?;
            let exit_code = output.exit_code;
            Ok((GitCommandOutput::Single(output), exit_code))
        }
        GitCommand::Push {
            json,
            component_id,
            tags,
            force_with_lease,
            remote_url,
            token,
            refspec,
            strip_extraheader,
            path,
        } => {
            if let Some(spec) = json {
                let output = git::push_bulk(&spec)?;
                let exit_code = if output.summary.failed > 0 { 1 } else { 0 };
                return Ok((GitCommandOutput::Bulk(output), exit_code));
            }

            let output = git::push_at(
                component_id.as_deref(),
                PushOptions {
                    tags,
                    force_with_lease,
                    remote_url,
                    token,
                    refspec,
                    strip_extraheader,
                },
                path.as_deref(),
            )?;
            let exit_code = output.exit_code;
            Ok((GitCommandOutput::Single(output), exit_code))
        }
        GitCommand::Pull {
            json,
            component_id,
            path,
        } => {
            if let Some(spec) = json {
                let output = git::pull_bulk(&spec)?;
                let exit_code = if output.summary.failed > 0 { 1 } else { 0 };
                return Ok((GitCommandOutput::Bulk(output), exit_code));
            }

            let output = git::pull_at(component_id.as_deref(), path.as_deref())?;
            let exit_code = output.exit_code;
            Ok((GitCommandOutput::Single(output), exit_code))
        }
        GitCommand::Tag {
            component_id,
            tag_name,
            message,
            path,
        } => {
            // Derive tag from version if not provided
            let final_tag = match tag_name {
                Some(name) => name,
                None => {
                    // Need component_id to look up version
                    let id = component_id.as_ref().ok_or_else(|| {
                        homeboy::core::Error::validation_invalid_argument(
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

            let output = git::tag_at(
                component_id.as_deref(),
                Some(&final_tag),
                message.as_deref(),
                path.as_deref(),
            )?;
            let exit_code = output.exit_code;
            Ok((GitCommandOutput::Single(output), exit_code))
        }
        GitCommand::Rebase {
            component_id,
            onto,
            r#continue,
            abort,
            path,
        } => {
            let output = git::rebase_at(
                component_id.as_deref(),
                RebaseOptions {
                    onto,
                    continue_: r#continue,
                    abort,
                },
                path.as_deref(),
            )?;
            let exit_code = output.exit_code;
            Ok((GitCommandOutput::Single(output), exit_code))
        }
        GitCommand::CherryPick {
            component_id,
            refs,
            pr,
            r#continue,
            abort,
            path,
        } => {
            let output = git::cherry_pick_at(
                component_id.as_deref(),
                CherryPickOptions {
                    refs,
                    prs: pr,
                    continue_: r#continue,
                    abort,
                },
                path.as_deref(),
            )?;
            let exit_code = output.exit_code;
            Ok((GitCommandOutput::Single(output), exit_code))
        }
        GitCommand::Issue(args) => issue::run_issue(args),
        GitCommand::Pr(args) => pr::run_pr(args),
    }
}
