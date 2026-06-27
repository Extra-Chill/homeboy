use homeboy::core::git::{
    self, PrCommentMode, PrCommentOptions, PrCreateOptions, PrEditOptions, PrFindOptions,
    PrFleetOptions, PrLandOptions, PrLandRefreshHelper, PrMergeabilityReconcileOptions,
    PrPolicyMergeOptions, PrPolicyOpenOptions, PrPolicyTargetRefs, PrRefreshOptions,
    PrRefreshStrategy,
};

use super::args::{PrArgs, PrCommand, PrPolicyArgs, PrPolicyCommand};
use super::helpers::{parse_pr_state, read_lines_file, resolve_body};
use super::output::GitCommandOutput;
use crate::commands::CmdResult;

// ---------------------------------------------------------------------------
// `git pr` dispatch
// ---------------------------------------------------------------------------

pub(super) fn run_pr(args: PrArgs) -> CmdResult<GitCommandOutput> {
    match args.command {
        PrCommand::Create {
            component_id,
            base,
            head,
            title,
            body,
            body_file,
            draft,
            path,
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
                    path,
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
            path,
        } => {
            let body = resolve_body(body, body_file)?;
            let output = git::pr_edit(
                Some(&component_id),
                PrEditOptions {
                    number,
                    title,
                    body,
                    path,
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
            path,
        } => {
            let state = parse_pr_state(&state)?;
            let output = git::pr_find(
                Some(&component_id),
                PrFindOptions {
                    base,
                    head,
                    state,
                    limit,
                    path,
                },
            )?;
            Ok((GitCommandOutput::Find(output), 0))
        }
        PrCommand::Readiness {
            component_id,
            number,
            path,
        } => {
            let output = git::pr_readiness(Some(&component_id), number, path)?;
            let exit = if output.readiness.mergeable { 0 } else { 1 };
            Ok((GitCommandOutput::PrReadiness(output), exit))
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
            footer,
            footer_file,
            section_order,
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

            // --footer / --footer-file share resolve_body; clap guarantees at
            // most one is set. `None` → preserve existing footer on merge.
            let footer = resolve_body(footer, footer_file)?;

            let mode = match (key, comment_key, section_key) {
                (Some(k), None, None) => PrCommentMode::StickyWholeBody { key: k },
                (None, Some(ck), Some(sk)) => PrCommentMode::Sectioned {
                    comment_key: ck,
                    section_key: sk,
                    header,
                    footer,
                    section_order,
                },
                (None, None, None) => {
                    // Header / footer / section_order without the pair — clap
                    // already caught this via `requires = "comment_key"`, but
                    // double-check.
                    PrCommentMode::Fresh
                }
                // Remaining cases are impossible due to clap `requires` /
                // `conflicts_with_all`, but keep the match exhaustive.
                _ => unreachable!(
                    "clap argument parsing should have rejected incompatible --key / --comment-key / --section-key combos"
                ),
            };

            let output = git::pr_comment(
                Some(&component_id),
                PrCommentOptions {
                    number,
                    body,
                    mode,
                    path,
                },
            )?;
            Ok((GitCommandOutput::Pr(output), 0))
        }
        PrCommand::Fleet {
            component_id,
            refs,
            update_branches,
            apply,
            merge_method,
            path,
        } => {
            if refs.is_empty() {
                return Err(homeboy::core::Error::validation_missing_argument(vec![
                    "at least one PR number or URL".to_string(),
                ]));
            }
            let output = git::pr_fleet(
                Some(&component_id),
                PrFleetOptions {
                    refs,
                    update_branches,
                    apply,
                    merge_method,
                    path,
                },
            )?;
            let exit = if output.success { 0 } else { 1 };
            Ok((GitCommandOutput::Fleet(output), exit))
        }
        PrCommand::ReconcileMergeability {
            component_id,
            number,
            path,
        } => {
            let output = git::pr_reconcile_mergeability(
                Some(&component_id),
                PrMergeabilityReconcileOptions { number, path },
            )?;
            Ok((GitCommandOutput::ReconcileMergeability(output), 0))
        }
        PrCommand::Policy(args) => run_pr_policy(args),
        PrCommand::Refresh {
            component_id,
            pr,
            strategy,
            push,
            checks,
            path,
        } => {
            let strategy = parse_pr_refresh_strategy(&strategy)?;
            let output = git::pr_refresh(
                Some(&component_id),
                PrRefreshOptions {
                    pr,
                    strategy,
                    push,
                    checks,
                    path,
                },
            )?;
            let exit = if output.success { 0 } else { 1 };
            Ok((GitCommandOutput::PrRefresh(output), exit))
        }
        PrCommand::Land {
            repo,
            prs,
            merge_method,
            delete_branch,
            dry_run,
            refresh_helper,
            refresh_helper_args,
            max_base_retries,
        } => {
            let output = git::land_prs(PrLandOptions {
                repo,
                prs,
                merge_method,
                delete_branch,
                dry_run,
                refresh_helper: refresh_helper.map(|program| PrLandRefreshHelper {
                    program,
                    args: refresh_helper_args,
                }),
                max_base_retries,
            })?;
            let exit = if output.summary.blocked > 0 { 1 } else { 0 };
            Ok((GitCommandOutput::Land(output), exit))
        }
    }
}

fn parse_pr_refresh_strategy(value: &str) -> homeboy::core::Result<PrRefreshStrategy> {
    match value {
        "auto" => Ok(PrRefreshStrategy::Auto),
        "rebase" => Ok(PrRefreshStrategy::Rebase),
        "merge" => Ok(PrRefreshStrategy::Merge),
        "ff-only" => Ok(PrRefreshStrategy::FfOnly),
        _ => Err(homeboy::core::Error::validation_invalid_argument(
            "strategy",
            "strategy must be one of auto, rebase, merge, or ff-only",
            Some(value.to_string()),
            None,
        )),
    }
}

fn run_pr_policy(args: PrPolicyArgs) -> CmdResult<GitCommandOutput> {
    match args.command {
        PrPolicyCommand::Open {
            component_id,
            policy,
            source,
            base,
            head,
            head_repository,
            repository,
            mut files,
            files_file,
            files_from_git,
            path,
        } => {
            if let Some(files_file) = files_file {
                files.extend(read_lines_file(&files_file)?);
            }
            let output = git::evaluate_open_policy(PrPolicyOpenOptions {
                component_id,
                path,
                policy_path: policy,
                source,
                refs: PrPolicyTargetRefs {
                    base,
                    head,
                    head_repository,
                    repository,
                },
                files,
                files_from_git,
            })?;
            let exit = if output.allowed { 0 } else { 1 };
            Ok((GitCommandOutput::Policy(output), exit))
        }
        PrPolicyCommand::Merge {
            component_id,
            policy,
            number,
            author,
            base,
            head,
            head_repository,
            repository,
            merge,
            merge_method,
            path,
        } => {
            let output = git::evaluate_merge_policy(PrPolicyMergeOptions {
                component_id,
                path,
                policy_path: policy,
                number,
                author,
                refs: PrPolicyTargetRefs {
                    base,
                    head,
                    head_repository,
                    repository,
                },
                merge,
                merge_method: Some(merge_method),
            })?;
            let exit = if output.allowed { 0 } else { 1 };
            Ok((GitCommandOutput::Policy(output), exit))
        }
    }
}
