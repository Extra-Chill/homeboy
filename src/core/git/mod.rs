mod changes;
mod commits;
mod github;
mod github_comment_sections;
mod github_pr_comments;
mod github_types;
mod operation_output;
mod operations;
mod operations_changes;
mod operations_commit;
mod operations_push;
mod pr_policy;
mod primitives;

#[cfg(test)]
mod operation_tests;

pub use changes::{
    discard_worktree_changes, get_diff, get_dirty_files, get_files_changed_since, get_range_diff,
    get_uncommitted_changes, UncommittedChanges,
};
pub(crate) use commits::extract_version_from_tag;
pub use commits::{
    categorize_commits, find_version_commit, find_version_release_commit, get_commits_since_tag,
    get_commits_since_tag_for_path, get_last_n_commits, get_latest_tag, get_latest_tag_with_prefix,
    get_previous_tag_before_with_prefix, recommended_bump_from_commits, strip_conventional_prefix,
    CommitCategory, CommitCounts, CommitInfo, MonorepoContext, SemverBump,
};
pub use github::{
    gh_probe_succeeds, github_token_from_env_or_gh, issue_close, issue_comment, issue_create,
    issue_edit, issue_find, pr_create, pr_edit, pr_files, pr_find, pr_find_by_commit, pr_merge,
    pr_view, GithubFindItem, GithubFindOutput, GithubIssueOutput, GithubPrOutput, GithubPrView,
    IssueCloseOptions, IssueCloseReason, IssueCommentOptions, IssueCreateOptions, IssueEditOptions,
    IssueFindOptions, IssueState, PrCreateOptions, PrEditOptions, PrFindOptions, PrMergeOptions,
    PrState,
};
pub use github_pr_comments::{pr_comment, PrCommentMode, PrCommentOptions};
pub use operation_output::GitOutput;
pub use operations::{
    cherry_pick, cherry_pick_at, delete_local_tag, delete_remote_tag, execute_git_for_release,
    fetch_and_fast_forward, fetch_and_get_behind_count, fetch_origin, get_head_commit,
    get_repo_snapshot, get_tag_commit, is_ancestor, pull, pull_at, pull_bulk, rebase, rebase_at,
    remote_branch_commit, remote_tag_commit, short_head_revision_at, status, status_at,
    status_bulk, tag, tag_at, tag_exists_locally, tag_exists_on_remote, CherryPickOptions,
    RebaseOptions, RepoSnapshot,
};
pub use operations_changes::{
    build_repo_baseline_snapshot, changes, changes_at, changes_bulk, changes_project,
    changes_project_filtered, detect_baseline_with_version, BaselineInfo, BaselineSource,
    ChangelogInfo, ChangesOutput, RepoBaselineSnapshot,
};
pub use operations_commit::{commit, commit_at, commit_from_json, CommitJsonOutput, CommitOptions};
pub use operations_push::{push, push_at, push_bulk, PushOptions};
pub use pr_policy::{
    evaluate_merge_policy, evaluate_open_policy, PrPolicyContext, PrPolicyDecision, PrPolicyFile,
    PrPolicyMergeOptions, PrPolicyMode, PrPolicyOpenOptions, PrPolicyRules,
};
pub use primitives::{
    clone_repo, clone_repo_at_ref, commit_staged_with_author, current_branch,
    get_component_path_prefix, get_git_root, git_probe_path, has_staged_changes, head_sha,
    head_sha_short, is_workdir_clean_or_not_git, output_optional, output_optional_bytes, pull_repo,
    remote_origin_url, remote_url, repo_root, rev_parse, run_git, run_git_output,
    short_head_revision, stage_all, status_porcelain, status_porcelain_bytes, toplevel,
    update_to_remote_default_branch,
};
pub(crate) use primitives::{is_git_repo, list_tracked_markdown_files};

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
pub struct GitIdentity {
    pub name: String,
    pub email: String,
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
pub fn configure_identity(path: &str, identity: &GitIdentity) -> crate::core::error::Result<()> {
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
/// 4. **Neither** — fall through to [`crate::core::component::resolve`], which
///    detects from CWD via the registry first, then portable
///    `homeboy.json` at CWD or git root. This is what makes
///    `homeboy git status` (and friends) work without arguments when
///    run from inside a checkout.
pub(crate) fn resolve_target(
    component_id: Option<&str>,
    path_override: Option<&str>,
) -> crate::core::error::Result<(String, String)> {
    let target = crate::core::component::resolve_target(crate::core::component::TargetSpec::new(
        component_id,
        path_override,
    ))?;

    Ok((
        target.component_id,
        target.source_path.to_string_lossy().to_string(),
    ))
}

#[cfg(test)]
mod identity_tests {
    use super::*;

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
