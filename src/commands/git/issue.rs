use homeboy::core::git::{
    self, IssueCloseOptions, IssueCommentOptions, IssueCreateOptions, IssueEditOptions,
    IssueFindOptions,
};

use super::args::{IssueArgs, IssueCommand};
use super::helpers::{parse_issue_close_reason, parse_issue_state, resolve_body};
use super::output::GitCommandOutput;
use crate::commands::CmdResult;

// ---------------------------------------------------------------------------
// `git issue` dispatch
// ---------------------------------------------------------------------------

pub(super) fn run_issue(args: IssueArgs) -> CmdResult<GitCommandOutput> {
    match args.command {
        IssueCommand::Create {
            component_id,
            title,
            body,
            body_file,
            label,
            path,
        } => {
            let body = resolve_body(body, body_file)?.unwrap_or_default();
            let output = git::issue_create(
                Some(&component_id),
                IssueCreateOptions {
                    title,
                    body,
                    labels: label,
                    path,
                },
            )?;
            Ok((GitCommandOutput::Issue(output), 0))
        }
        IssueCommand::Comment {
            component_id,
            number,
            body,
            body_file,
            path,
        } => {
            let body = resolve_body(body, body_file)?.ok_or_else(|| {
                homeboy::core::Error::validation_invalid_argument(
                    "body",
                    "Comment body is required (--body or --body-file)",
                    None,
                    None,
                )
            })?;
            let output = git::issue_comment(
                Some(&component_id),
                IssueCommentOptions { number, body, path },
            )?;
            Ok((GitCommandOutput::Issue(output), 0))
        }
        IssueCommand::Find {
            component_id,
            title,
            label,
            state,
            limit,
            path,
        } => {
            let state = parse_issue_state(&state)?;
            let output = git::issue_find(
                Some(&component_id),
                IssueFindOptions {
                    title,
                    labels: label,
                    state,
                    limit,
                    path,
                },
            )?;
            Ok((GitCommandOutput::Find(output), 0))
        }
        IssueCommand::Close {
            component_id,
            number,
            reason,
            comment,
            comment_file,
            path,
        } => {
            let reason = parse_issue_close_reason(&reason)?;
            let comment = resolve_body(comment, comment_file)?;
            let output = git::issue_close(
                Some(&component_id),
                IssueCloseOptions {
                    number,
                    reason,
                    comment,
                    path,
                },
            )?;
            Ok((GitCommandOutput::Issue(output), 0))
        }
        IssueCommand::Edit {
            component_id,
            number,
            title,
            body,
            body_file,
            add_labels,
            remove_labels,
            path,
        } => {
            let body = resolve_body(body, body_file)?;
            let output = git::issue_edit(
                Some(&component_id),
                IssueEditOptions {
                    number,
                    title,
                    body,
                    add_labels,
                    remove_labels,
                    path,
                },
            )?;
            Ok((GitCommandOutput::Issue(output), 0))
        }
    }
}
