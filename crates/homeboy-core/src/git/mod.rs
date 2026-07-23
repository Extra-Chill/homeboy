mod changes;
mod commits;
mod gh_client;
mod github;
mod github_comment_sections;
mod github_pr_comments;
mod github_types;
mod operation_output;
mod operations;
mod operations_changes;
mod operations_commit;
mod operations_push;
mod operations_tags;
mod pr_land;
mod pr_policy;
mod pr_refresh;
mod primitives;
mod primitives_query;
pub mod release_download;

#[cfg(test)]
mod operation_tests;

pub use changes::{
    discard_worktree_changes, get_diff, get_dirty_files, get_files_changed_since, get_range_diff,
    get_uncommitted_changes, resolve_merge_base, UncommittedChanges,
};
pub use commits::extract_version_from_tag;
pub use commits::{
    categorize_commits, find_version_commit, find_version_release_commit, get_commits_in_range,
    get_commits_since_tag, get_commits_since_tag_for_path, get_commits_since_tag_for_paths,
    get_commits_since_tag_for_scope, get_component_changes_since_tag, get_last_n_commits,
    get_latest_tag, get_latest_tag_any_with_prefix, get_latest_tag_with_prefix,
    get_previous_tag_before_any_with_prefix, get_previous_tag_before_with_prefix,
    recommended_bump_from_commits, strip_conventional_prefix, CommitCategory, CommitCounts,
    CommitInfo, MonorepoContext, SemverBump,
};
pub use gh_client::{github_cli_env, GhClient};
pub use github::push_markdown_body_file_arg;
pub use github::{
    gh_probe_succeeds, github_token_from_env_or_gh, issue_close, issue_comment, issue_create,
    issue_edit, issue_find, pr_create, pr_edit, pr_files, pr_find, pr_find_by_commit, pr_fleet,
    pr_merge, pr_readiness, pr_reconcile_mergeability, pr_view, GithubFindItem, GithubFindOutput,
    GithubIssueOutput, GithubPrOutput, GithubPrReadinessOutput, GithubPrView, IssueCloseOptions,
    IssueCloseReason, IssueCommentOptions, IssueCreateOptions, IssueEditOptions, IssueFindOptions,
    IssueState, PrCreateOptions, PrEditOptions, PrFindOptions, PrMergeOptions, PrMergeReadiness,
    PrMergeabilityReconcileOptions, PrMergeabilityReconcileOutput, PrReadinessBlocker, PrState,
};
pub use github_pr_comments::{pr_comment, PrCommentMode, PrCommentOptions};
pub use github_types::{
    GithubPrCheckRollup, GithubPrFleetItem, GithubPrFleetOutput, GithubPrFleetSummary,
    PrFleetOptions,
};
pub use operation_output::GitOutput;
pub use operations::{
    cherry_pick, cherry_pick_at, execute_git_for_release, fetch_and_fast_forward,
    fetch_and_get_behind_count, get_repo_snapshot, pull, pull_at, pull_bulk, rebase, rebase_at,
    status, status_at, status_bulk, CherryPickOptions, RebaseOptions, RepoSnapshot,
};
pub use operations_changes::{
    build_repo_baseline_snapshot, changes, changes_at, changes_bulk, changes_project,
    changes_project_filtered, detect_baseline_with_version,
    detect_baseline_with_version_and_tag_prefix,
    detect_baseline_with_version_and_tag_prefix_from_fetched_tags,
    detect_baseline_with_version_from_fetched_tags, BaselineInfo, BaselineSource, ChangelogInfo,
    ChangesOutput, RepoBaselineSnapshot,
};
pub use operations_commit::{commit, commit_at, commit_from_json, CommitJsonOutput, CommitOptions};
pub use operations_push::{push, push_at, push_bulk, PushOptions};
pub use operations_tags::{
    delete_local_tag, delete_remote_tag, fetch_origin, fetch_tags, get_head_commit, get_tag_commit,
    is_ancestor, remote_branch_commit, remote_tag_commit, short_head_revision_at, tag, tag_at,
    tag_exists_locally, tag_exists_on_remote,
};
pub use pr_land::{land_prs, PrLandOptions, PrLandOutput, PrLandRefreshHelper};
pub use pr_policy::{
    evaluate_merge_policy, evaluate_open_policy, PrPolicyContext, PrPolicyDecision, PrPolicyFile,
    PrPolicyMergeOptions, PrPolicyMode, PrPolicyOpenOptions, PrPolicyRules, PrPolicyTargetRefs,
};
pub use pr_refresh::{
    pr_refresh, PrRefreshCheck, PrRefreshOptions, PrRefreshOutput, PrRefreshStrategy,
};
pub(crate) use primitives::list_tracked_markdown_files;
pub use primitives::{
    clone_repo, clone_repo_at_ref, clone_repo_at_ref_with_timeout, commit_staged_with_author,
    default_branch_name, default_remote_branch, get_component_path_prefix, get_git_root,
    git_probe_path, has_staged_changes, is_workdir_clean_or_not_git, pull_repo,
    resolve_default_remote, run_git, run_git_output, run_git_output_with_env, run_git_with_env,
    run_git_with_env_timeout, stage_all, update_to_remote_default_branch,
};
pub use primitives::{is_git_repo, is_tracked_path};
pub use primitives_query::{
    current_branch, head_sha, head_sha_short, output_allow_empty, output_optional,
    output_optional_bytes, remote_origin_url, remote_url, repo_root, rev_parse,
    short_head_revision, status_porcelain, status_porcelain_bytes, toplevel,
};

use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::Path;
use std::process::Command;

fn execute_git(path: &str, args: &[&str]) -> std::io::Result<std::process::Output> {
    Command::new("git").args(args).current_dir(path).output()
}

/// Well-known bot identity for CI commits.
pub const BOT_NAME: &str = "homeboy-ci[bot]";
/// Well-known bot email for CI commits (GitHub noreply address).
pub const BOT_EMAIL: &str = "266378653+homeboy-ci[bot]@users.noreply.github.com";

/// Parsed git identity (name + email).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitIdentity {
    pub name: String,
    pub email: String,
}

/// Evidence that an effective or committed identity matched its origin-host policy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitIdentityProof {
    pub host: String,
    pub name: String,
    pub email: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub committer_name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub committer_email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_sha: Option<String>,
    pub scope: String,
}

/// Validate the effective author and committer Git would use for a new commit.
pub fn validate_publication_identity(path: &str) -> crate::error::Result<GitIdentityProof> {
    let author = effective_identity(path, "GIT_AUTHOR_IDENT")?;
    let committer = effective_identity(path, "GIT_COMMITTER_IDENT")?;
    validate_identities(path, author, committer, None, "effective")
}

fn validate_identities(
    path: &str,
    author: GitIdentity,
    committer: GitIdentity,
    commit_sha: Option<String>,
    scope: &str,
) -> crate::error::Result<GitIdentityProof> {
    let remote = git_config(path, &["config", "--get", "remote.origin.url"])?;
    let host = remote_host(&remote).ok_or_else(|| {
        identity_error(
            "origin remote does not contain a usable hostname",
            json!({ "origin": remote, "remediation": [] }),
        )
    })?;
    let config = crate::defaults::load_config();
    let Some(expected) = config.git_hosts.get(&host) else {
        return Ok(GitIdentityProof {
            host,
            name: author.name,
            email: author.email,
            committer_name: committer.name,
            committer_email: committer.email,
            commit_sha,
            scope: format!("{scope}_unrestricted"),
        });
    };
    if expected.name.trim().is_empty() || expected.email.trim().is_empty() {
        return Err(identity_error(
            "configured Git identity policy requires non-empty name and email",
            json!({ "host": host, "remediation": [{ "kind": "complete_host_policy" }] }),
        ));
    }

    if author.name != expected.name
        || author.email != expected.email
        || committer.name != expected.name
        || committer.email != expected.email
    {
        return Err(identity_error(
            "effective Git author or committer does not match the origin host policy",
            json!({
                "host": host,
                "expected": { "name": expected.name, "email": expected.email },
                "actual": {
                    "author": { "name": author.name, "email": author.email },
                    "committer": { "name": committer.name, "email": committer.email }
                },
                "remediation": [{
                    "kind": "configure_repository_local_identity",
                    "commands": [
                        format!("git -C {} config --local user.name {:?}", path, expected.name),
                        format!("git -C {} config --local user.email {:?}", path, expected.email)
                    ]
                }]
            }),
        ));
    }
    Ok(GitIdentityProof {
        host,
        name: author.name,
        email: author.email,
        committer_name: committer.name,
        committer_email: committer.email,
        commit_sha,
        scope: if scope == "effective" {
            "repository_local".to_string()
        } else {
            format!("{scope}_host_policy")
        },
    })
}

/// Validate the immutable author and committer stored in `HEAD`.
pub fn validate_committed_publication_identity(
    path: &str,
    expected: Option<&GitIdentityProof>,
) -> crate::error::Result<GitIdentityProof> {
    let output = git_config(
        path,
        &["show", "-s", "--format=%H%n%an%n%ae%n%cn%n%ce", "HEAD"],
    )?;
    let mut lines = output.lines();
    let sha = lines.next().unwrap_or_default().to_string();
    let author = GitIdentity {
        name: lines.next().unwrap_or_default().to_string(),
        email: lines.next().unwrap_or_default().to_string(),
    };
    let committer = GitIdentity {
        name: lines.next().unwrap_or_default().to_string(),
        email: lines.next().unwrap_or_default().to_string(),
    };
    if sha.is_empty()
        || author.name.is_empty()
        || author.email.is_empty()
        || committer.name.is_empty()
        || committer.email.is_empty()
    {
        return Err(identity_error(
            "HEAD does not contain a complete commit identity",
            json!({ "remediation": [] }),
        ));
    }
    if let Some(expected) = expected {
        if author.name != expected.name
            || author.email != expected.email
            || committer.name != expected.committer_name
            || committer.email != expected.committer_email
        {
            return Err(identity_error(
                "committed Git identity differs from the identity validated before commit",
                json!({
                    "expected": {
                        "author": { "name": expected.name, "email": expected.email },
                        "committer": {
                            "name": expected.committer_name,
                            "email": expected.committer_email
                        }
                    },
                    "actual": {
                        "author": { "name": author.name, "email": author.email },
                        "committer": { "name": committer.name, "email": committer.email }
                    },
                    "commit_sha": sha,
                    "remediation": []
                }),
            ));
        }
    }
    validate_identities(path, author, committer, Some(sha), "commit")
}

fn effective_identity(path: &str, variable: &str) -> crate::error::Result<GitIdentity> {
    let value = git_config(path, &["var", variable])?;
    let Some(email_end) = value.rfind('>') else {
        return Err(identity_error(
            "Git could not resolve a complete prospective identity",
            json!({ "identity": variable, "remediation": [] }),
        ));
    };
    let Some(email_start) = value[..email_end].rfind(" <") else {
        return Err(identity_error(
            "Git could not resolve a complete prospective identity",
            json!({ "identity": variable, "remediation": [] }),
        ));
    };
    let identity = GitIdentity {
        name: value[..email_start].trim().to_string(),
        email: value[email_start + 2..email_end].trim().to_string(),
    };
    if identity.name.is_empty() || identity.email.is_empty() {
        return Err(identity_error(
            "Git could not resolve a complete prospective identity",
            json!({ "identity": variable, "remediation": [] }),
        ));
    }
    Ok(identity)
}

fn git_config(path: &str, args: &[&str]) -> crate::error::Result<String> {
    let output = execute_git(path, args)
        .map_err(|error| crate::error::Error::git_command_failed(error.to_string()))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
    }
    Ok(String::new())
}

fn remote_host(remote: &str) -> Option<String> {
    let authority = remote
        .trim()
        .strip_prefix("https://")
        .or_else(|| remote.trim().strip_prefix("http://"))
        .or_else(|| remote.trim().strip_prefix("ssh://"))
        .or_else(|| remote.trim().split_once('@').map(|(_, value)| value))?;
    let host = authority
        .split('@')
        .next_back()?
        .split('/')
        .next()?
        .split(':')
        .next()?
        .trim();
    (!host.is_empty()).then(|| host.to_string())
}

fn identity_error(message: &str, details: serde_json::Value) -> crate::error::Error {
    crate::error::Error {
        code: crate::error::ErrorCode::ValidationInvalidArgument,
        message: message.to_string(),
        details,
        hints: Vec::new(),
        retryable: Some(false),
    }
}

/// Parse a `--git-identity` value into name + email.
///
/// - `None` or `"bot"` → default CI bot identity
/// - `"Name <email>"` → parsed
/// - `"Name"` → name with bot email
pub fn parse_git_identity(identity: Option<&str>) -> GitIdentity {
    match identity {
        None | Some("bot") => GitIdentity {
            name: BOT_NAME.to_string(),
            email: BOT_EMAIL.to_string(),
        },
        Some(custom) => {
            if let Some(angle_start) = custom.find('<') {
                let name = custom[..angle_start].trim().to_string();
                let email = custom[angle_start + 1..]
                    .trim_end_matches('>')
                    .trim()
                    .to_string();
                if !name.is_empty() && !email.is_empty() {
                    return GitIdentity { name, email };
                }
            }
            GitIdentity {
                name: custom.to_string(),
                email: BOT_EMAIL.to_string(),
            }
        }
    }
}

/// Configure git user.name and user.email in a repository.
pub fn configure_identity(path: &str, identity: &GitIdentity) -> crate::error::Result<()> {
    for (key, value) in [
        ("user.name", identity.name.as_str()),
        ("user.email", identity.email.as_str()),
    ] {
        run_git(
            Path::new(path),
            &["config", key, value],
            &format!("git config {key}"),
        )?;
    }
    Ok(())
}

/// Resolve a (component_id, path) pair for a git operation.
///
/// Resolution order:
/// 1. **Both `component_id` and `path_override` provided** — trust both;
///    no registry lookup. Used by rig pipelines + workflows that already
///    know the path they want to operate on.
/// 2. **`path_override` only** — use the path; derive the ID from a
///    portable `homeboy.json` at the path or its git root, falling back
///    to the directory basename.
/// 3. **`component_id` only** — look the component up in the registry,
///    use its configured `local_path`.
/// 4. **Neither** — fall through to [`crate::component::resolve`], which
///    detects from CWD via the registry first, then portable
///    `homeboy.json` at CWD or git root. This is what makes
///    `homeboy git status` (and friends) work without arguments when
///    run from inside a checkout.
pub fn resolve_target(
    component_id: Option<&str>,
    path_override: Option<&str>,
) -> crate::error::Result<(String, String)> {
    let target = crate::component::resolve_target(crate::component::TargetSpec::new(
        component_id,
        path_override,
    ))?;

    Ok((
        target.component_id,
        target.source_path.to_string_lossy().to_string(),
    ))
}

/// Resolve a target, run a single `git` invocation against it, and wrap the
/// result in a [`GitOutput`].
///
/// This is the shared spine for the simple "resolve → run one git command →
/// report" operations (`status`, `pull`, `tag`, …). Each caller only differs
/// by the argument vector and the `operation` label, so they delegate here
/// instead of repeating the resolve / `execute_git` / `map_err` / `from_output`
/// dance.
pub(crate) fn run_resolved_git(
    component_id: Option<&str>,
    path_override: Option<&str>,
    operation: &str,
    args: &[&str],
) -> crate::error::Result<operation_output::GitOutput> {
    let (id, path) = resolve_target(component_id, path_override)?;
    let output = execute_git(&path, args)
        .map_err(|e| crate::error::Error::git_command_failed(e.to_string()))?;
    Ok(operation_output::GitOutput::from_output(
        id, path, operation, output,
    ))
}

#[cfg(test)]
mod identity_tests {
    use super::*;
    use crate::defaults::{save_config, GitHostConfig, HomeboyConfig};
    use crate::test_support::with_isolated_home;
    use std::collections::HashMap;
    use std::process::Command;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    #[test]
    fn bot_shorthand() {
        let id = parse_git_identity(Some("bot"));
        assert_eq!(id.name, BOT_NAME);
        assert_eq!(id.email, BOT_EMAIL);
    }

    #[test]
    fn none_defaults_to_bot() {
        let id = parse_git_identity(None);
        assert_eq!(id.name, BOT_NAME);
        assert_eq!(id.email, BOT_EMAIL);
    }

    #[test]
    fn custom_name_and_email() {
        let id = parse_git_identity(Some("Deploy Bot <deploy@example.com>"));
        assert_eq!(id.name, "Deploy Bot");
        assert_eq!(id.email, "deploy@example.com");
    }

    #[test]
    fn name_only_uses_bot_email() {
        let id = parse_git_identity(Some("My Service"));
        assert_eq!(id.name, "My Service");
        assert_eq!(id.email, BOT_EMAIL);
    }

    #[test]
    fn publication_identity_unrestricted_host_preserves_effective_identity() {
        with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("tempdir");
            let repo = temp.path();
            for args in [
                vec!["init"],
                vec![
                    "remote",
                    "add",
                    "origin",
                    "git@git.example.test:owner/repo.git",
                ],
                vec!["config", "user.name", "Existing Author"],
                vec!["config", "user.email", "author@example.test"],
            ] {
                let status = Command::new("git")
                    .args(args)
                    .current_dir(repo)
                    .status()
                    .expect("run git");
                assert!(status.success());
            }

            let proof = validate_publication_identity(repo.to_str().expect("path"))
                .expect("unrestricted host");

            assert_eq!(proof.host, "git.example.test");
            assert_eq!(proof.name, "Existing Author");
            assert_eq!(proof.email, "author@example.test");
            assert_eq!(proof.committer_name, "Existing Author");
            assert_eq!(proof.committer_email, "author@example.test");
            assert_eq!(proof.scope, "effective_unrestricted");
        });
    }

    #[test]
    fn publication_identity_configured_host_requires_complete_effective_identity() {
        with_isolated_home(|_| {
            let temp = identity_test_repo();
            save_identity_policy();

            let error = with_identity_environment(("", ""), ("", ""), || {
                validate_publication_identity(temp.path().to_str().expect("path"))
                    .expect_err("effective identity is required")
            });

            assert!(error.message.contains("prospective identity"));
        });
    }

    #[test]
    fn publication_identity_configured_host_rejects_incorrect_identity() {
        with_isolated_home(|_| {
            let temp = identity_test_repo();
            save_identity_policy();
            run_identity_git(temp.path(), &["config", "user.name", "Wrong Author"]);
            run_identity_git(temp.path(), &["config", "user.email", "wrong@example.test"]);

            let error = validate_publication_identity(temp.path().to_str().expect("path"))
                .expect_err("incorrect identity");

            assert_eq!(error.details["actual"]["author"]["name"], "Wrong Author");
            assert_eq!(error.details["expected"]["name"], "Expected Author");
        });
    }

    #[test]
    fn publication_identity_configured_host_accepts_correct_identity() {
        with_isolated_home(|_| {
            let temp = identity_test_repo();
            save_identity_policy();
            run_identity_git(temp.path(), &["config", "user.name", "Expected Author"]);
            run_identity_git(
                temp.path(),
                &["config", "user.email", "expected@example.test"],
            );

            let proof = validate_publication_identity(temp.path().to_str().expect("path"))
                .expect("correct identity");

            assert_eq!(proof.name, "Expected Author");
            assert_eq!(proof.email, "expected@example.test");
            assert_eq!(proof.committer_name, "Expected Author");
            assert_eq!(proof.scope, "repository_local");
        });
    }

    #[test]
    fn publication_identity_accepts_allowed_environment_overrides() {
        with_isolated_home(|_| {
            let temp = identity_test_repo();
            save_identity_policy();
            run_identity_git(temp.path(), &["config", "user.name", "Wrong Author"]);
            run_identity_git(temp.path(), &["config", "user.email", "wrong@example.test"]);

            with_identity_environment(
                ("Expected Author", "expected@example.test"),
                ("Expected Author", "expected@example.test"),
                || {
                    let proof = validate_publication_identity(temp.path().to_str().expect("path"))
                        .expect("allowed environment identity");
                    assert_eq!(proof.name, "Expected Author");
                    assert_eq!(proof.committer_name, "Expected Author");
                },
            );
        });
    }

    #[test]
    fn publication_identity_rejects_environment_override() {
        with_isolated_home(|_| {
            let temp = identity_test_repo();
            save_identity_policy();
            run_identity_git(temp.path(), &["config", "user.name", "Expected Author"]);
            run_identity_git(
                temp.path(),
                &["config", "user.email", "expected@example.test"],
            );

            with_identity_environment(
                ("Override Author", "override@example.test"),
                ("Expected Author", "expected@example.test"),
                || {
                    let error = validate_publication_identity(temp.path().to_str().expect("path"))
                        .expect_err("rejected environment identity");
                    assert_eq!(
                        error.details["actual"]["author"]["email"],
                        "override@example.test"
                    );
                },
            );
        });
    }

    #[test]
    fn publication_identity_rejects_different_author_and_committer() {
        with_isolated_home(|_| {
            let temp = identity_test_repo();
            save_identity_policy();

            with_identity_environment(
                ("Expected Author", "expected@example.test"),
                ("Other Committer", "other@example.test"),
                || {
                    let error = validate_publication_identity(temp.path().to_str().expect("path"))
                        .expect_err("different committer");
                    assert_eq!(
                        error.details["actual"]["committer"]["name"],
                        "Other Committer"
                    );
                },
            );
        });
    }

    #[test]
    fn committed_publication_identity_is_bound_to_head_sha() {
        with_isolated_home(|_| {
            let temp = identity_test_repo();
            save_identity_policy();
            run_identity_git(temp.path(), &["config", "user.name", "Expected Author"]);
            run_identity_git(
                temp.path(),
                &["config", "user.email", "expected@example.test"],
            );
            run_identity_git(temp.path(), &["commit", "--allow-empty", "-m", "candidate"]);

            let proof =
                validate_committed_publication_identity(temp.path().to_str().expect("path"), None)
                    .expect("committed identity");
            let head = git_config(temp.path().to_str().expect("path"), &["rev-parse", "HEAD"])
                .expect("head");

            assert_eq!(proof.commit_sha.as_deref(), Some(head.as_str()));
            assert_eq!(proof.name, "Expected Author");
            assert_eq!(proof.committer_name, "Expected Author");
            assert_eq!(proof.scope, "commit_host_policy");
        });
    }

    #[test]
    fn committed_publication_identity_must_match_validated_intent() {
        with_isolated_home(|_| {
            let temp = identity_test_repo();
            run_identity_git(temp.path(), &["config", "user.name", "Intended Author"]);
            run_identity_git(
                temp.path(),
                &["config", "user.email", "intended@example.test"],
            );
            let intended = validate_publication_identity(temp.path().to_str().expect("path"))
                .expect("prospective identity");

            with_identity_environment(
                ("Different Author", "different@example.test"),
                ("Different Committer", "different-committer@example.test"),
                || run_identity_git(temp.path(), &["commit", "--allow-empty", "-m", "candidate"]),
            );
            let error = validate_committed_publication_identity(
                temp.path().to_str().expect("path"),
                Some(&intended),
            )
            .expect_err("commit identity drift");

            assert!(error
                .message
                .contains("differs from the identity validated"));
            assert_eq!(
                error.details["actual"]["author"]["name"],
                "Different Author"
            );
            assert!(!error.details["commit_sha"]
                .as_str()
                .unwrap_or_default()
                .is_empty());
        });
    }

    #[test]
    fn git_identity_proof_deserializes_legacy_serialized_evidence() {
        let proof: GitIdentityProof = serde_json::from_value(serde_json::json!({
            "host": "git.example.test",
            "name": "Existing Author",
            "email": "author@example.test",
            "scope": "repository_local"
        }))
        .expect("legacy identity proof");

        assert!(proof.committer_name.is_empty());
        assert!(proof.committer_email.is_empty());
        assert_eq!(proof.commit_sha, None);
        assert_eq!(
            serde_json::to_value(proof).expect("serialize legacy identity proof"),
            serde_json::json!({
                "host": "git.example.test",
                "name": "Existing Author",
                "email": "author@example.test",
                "scope": "repository_local"
            })
        );
    }

    fn identity_test_repo() -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("tempdir");
        run_identity_git(temp.path(), &["init"]);
        run_identity_git(
            temp.path(),
            &[
                "remote",
                "add",
                "origin",
                "git@git.example.test:owner/repo.git",
            ],
        );
        temp
    }

    fn save_identity_policy() {
        save_config(&HomeboyConfig {
            git_hosts: HashMap::from([(
                "git.example.test".to_string(),
                GitHostConfig {
                    name: "Expected Author".to_string(),
                    email: "expected@example.test".to_string(),
                },
            )]),
            ..HomeboyConfig::default()
        })
        .expect("save config");
    }

    fn run_identity_git(path: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .expect("run git");
        assert!(status.success());
    }

    fn with_identity_environment<R>(
        author: (&str, &str),
        committer: (&str, &str),
        test: impl FnOnce() -> R,
    ) -> R {
        let _environment = IdentityEnvironment::new(author, committer);
        test()
    }

    static IDENTITY_ENVIRONMENT_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    struct IdentityEnvironment {
        _lock: MutexGuard<'static, ()>,
        previous: [Option<String>; 4],
    }

    impl IdentityEnvironment {
        fn new(author: (&str, &str), committer: (&str, &str)) -> Self {
            let lock = IDENTITY_ENVIRONMENT_LOCK
                .get_or_init(|| Mutex::new(()))
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let values = [
                ("GIT_AUTHOR_NAME", author.0),
                ("GIT_AUTHOR_EMAIL", author.1),
                ("GIT_COMMITTER_NAME", committer.0),
                ("GIT_COMMITTER_EMAIL", committer.1),
            ];
            let previous = values.map(|(key, _)| std::env::var(key).ok());
            for (key, value) in values {
                std::env::set_var(key, value);
            }
            Self {
                _lock: lock,
                previous,
            }
        }
    }

    impl Drop for IdentityEnvironment {
        fn drop(&mut self) {
            for (key, value) in [
                "GIT_AUTHOR_NAME",
                "GIT_AUTHOR_EMAIL",
                "GIT_COMMITTER_NAME",
                "GIT_COMMITTER_EMAIL",
            ]
            .into_iter()
            .zip(self.previous.iter())
            {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
}

#[cfg(test)]
mod resolve_target_tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Returns (TempDir, repo_path) where repo_path has a homeboy.json with
    /// the given component id.
    fn make_portable_repo(id: &str) -> (TempDir, std::path::PathBuf) {
        let dir = TempDir::new().expect("Failed to create temp dir");
        let repo = dir.path().join(id);
        fs::create_dir_all(&repo).expect("Failed to create repo dir");

        let portable = serde_json::json!({ "id": id });
        fs::write(
            repo.join("homeboy.json"),
            serde_json::to_string_pretty(&portable).unwrap(),
        )
        .expect("Failed to write homeboy.json");

        // Capture path before moving dir.
        let path = repo.clone();
        (dir, path)
    }

    #[test]
    fn path_only_discovers_id_from_portable_config() {
        let (_dir, repo) = make_portable_repo("my-plugin");

        let (id, path) = resolve_target(None, Some(repo.to_str().unwrap()))
            .expect("resolve_target should succeed with --path pointing at portable config");

        assert_eq!(id, "my-plugin");
        assert_eq!(path, repo.to_string_lossy());
    }

    #[test]
    fn path_only_falls_back_to_basename_when_no_portable_config() {
        let dir = TempDir::new().unwrap();
        let repo = dir.path().join("bare-checkout");
        fs::create_dir_all(&repo).unwrap();

        let (id, path) = resolve_target(None, Some(repo.to_str().unwrap()))
            .expect("resolve_target should succeed with --path even without portable config");

        assert_eq!(id, "bare-checkout");
        assert_eq!(path, repo.to_string_lossy());
    }

    #[test]
    fn path_and_id_both_provided_skips_discovery() {
        let dir = TempDir::new().unwrap();
        let repo = dir.path().join("anywhere");
        fs::create_dir_all(&repo).unwrap();

        let (id, path) = resolve_target(Some("explicit-id"), Some(repo.to_str().unwrap()))
            .expect("resolve_target should succeed with both args");

        // Trusts both inputs verbatim — no registry lookup, no discovery.
        assert_eq!(id, "explicit-id");
        assert_eq!(path, repo.to_string_lossy());
    }

    #[test]
    fn path_only_walks_up_to_git_root_for_portable_config() {
        // Layout:
        //   <tmp>/repo/                  (homeboy.json lives here)
        //   <tmp>/repo/.git/
        //   <tmp>/repo/subdir/
        //
        // Calling with path=<tmp>/repo/subdir should find homeboy.json at
        // the git root via the existing detect_git_root walk.
        let dir = TempDir::new().unwrap();
        let repo = dir.path().join("repo");
        fs::create_dir_all(repo.join("subdir")).unwrap();

        let portable = serde_json::json!({ "id": "monorepo-thing" });
        fs::write(
            repo.join("homeboy.json"),
            serde_json::to_string_pretty(&portable).unwrap(),
        )
        .unwrap();

        // git init so detect_git_root can find the root.
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .output()
            .expect("git init");

        let subdir = repo.join("subdir");
        let (id, path) = resolve_target(None, Some(subdir.to_str().unwrap()))
            .expect("resolve_target should walk up to git root for portable config");

        assert_eq!(id, "monorepo-thing");
        assert_eq!(path, subdir.to_string_lossy());
    }
}
