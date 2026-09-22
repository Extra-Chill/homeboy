use homeboy_core::component::Component;
use homeboy_core::engine::command;
use homeboy_core::error::{Error, Result};
use homeboy_core::git;

/// Fetch from remote and fast-forward if behind.
///
/// Ensures the release commit is created on top of the actual remote HEAD,
/// preventing detached release tags when PRs merge during a CI quality gate.
/// Returns Err if the branch has diverged and can't be fast-forwarded.
pub(super) fn validate_remote_sync(component: &Component) -> Result<()> {
    let synced = git::fetch_and_fast_forward(&component.local_path)?;

    if let Some(n) = synced {
        homeboy_core::log_status!(
            "release",
            "Fast-forwarded {} commit(s) from remote before release",
            n
        );
    }

    Ok(())
}

/// Refresh remote refs without changing a provider-selected staging checkout.
/// The selected SHA remains the authority from provisioning through mutation.
pub(super) fn validate_remote_sync_at(component: &Component, source_sha: &str) -> Result<()> {
    git::fetch_origin(&component.local_path)?;
    let head = git::get_head_commit(&component.local_path)?;
    if head == source_sha {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        "release.workspace",
        format!(
            "staging checkout moved from immutable source SHA {source_sha} to {head} before release mutation"
        ),
        Some(component.local_path.clone()),
        Some(vec!["Re-provision the release workspace from the verified source SHA.".to_string()]),
    ))
}

pub(super) fn validate_default_branch(component: &Component) -> Result<()> {
    let current_branch = command::run_in_optional(
        &component.local_path,
        "git",
        &["symbolic-ref", "--short", "HEAD"],
    );
    let default_branch = default_branch(component);

    if current_branch.as_deref() == Some(default_branch.as_str()) {
        return Ok(());
    }

    if head_matches_remote_default(component, &default_branch)? {
        homeboy_core::log_status!(
            "release",
            "Local branch '{}' matches the remote default branch tip; release will push HEAD to '{}'.",
            current_branch.as_deref().unwrap_or("detached HEAD"),
            default_branch
        );
        return Ok(());
    }

    if current_branch.is_none() {
        let remote = source_remote(component);
        let remote_default_ref = format!("{remote}/{default_branch}");
        let remote_default_revision = remote_default_revision(component, &remote_default_ref)?;
        let head_revision = head_revision(component)?;

        return Err(Error::validation_invalid_argument(
            "release",
            format!(
                "Refusing to release from detached HEAD at '{head_revision}' because the repo default branch '{default_branch}' is at '{remote_default_revision}'"
            ),
            None,
            Some(vec![format!(
                "Check out '{default_branch}' or release the default branch revision '{remote_default_revision}'"
            )]),
        ));
    }

    Err(Error::validation_invalid_argument(
        "release",
        format!(
            "Refusing to release from branch '{}' because the repo default branch is '{}'",
            current_branch.as_deref().unwrap_or("detached HEAD"),
            default_branch
        ),
        None,
        Some(vec![
            format!(
                "Check out '{}' before running `homeboy release --apply` for a default-branch release workflow",
                default_branch
            ),
            format!(
                "Rebase or merge '{}' onto '{}' and release from '{}' so the tag target is published through the default branch",
                current_branch.as_deref().unwrap_or("detached HEAD"),
                default_branch,
                default_branch
            ),
            "If you only want a preview, use --dry-run".to_string(),
        ]),
    ))
}

pub(super) fn release_push_branch(component: &Component) -> Result<String> {
    let default_branch = default_branch(component);
    if !git::is_git_repo(&component.local_path) {
        return Ok(default_branch);
    }

    // A detached HEAD is not disqualifying on its own. The property this check
    // cares about is whether the commit being released is the default branch,
    // which `head_matches_remote_default` answers by commit identity below.
    let current_branch = command::run_in_optional(
        &component.local_path,
        "git",
        &["symbolic-ref", "--short", "HEAD"],
    );

    if current_branch.as_deref() == Some(default_branch.as_str()) {
        return Ok(default_branch);
    }

    if head_matches_remote_default(component, &default_branch)? {
        return Ok(default_branch);
    }

    Err(Error::validation_invalid_argument(
        "release",
        format!(
            "Refusing to plan release push from {} because the repo default branch is '{}'",
            current_branch
                .as_deref()
                .map(|branch| format!("branch '{branch}'"))
                .unwrap_or_else(|| "detached HEAD".to_string()),
            default_branch
        ),
        None,
        Some(vec![format!(
            "Check out '{}' or rerun preflight after fast-forwarding the release worktree to the remote default branch tip",
            default_branch
        )]),
    ))
}

pub(super) fn validate_default_branch_ancestry(component: &Component) -> Result<()> {
    // Detachment is only a naming detail here: the check below is an ancestry
    // test against the remote default revision, which is meaningful whether or
    // not HEAD carries a symbolic name.
    let current_branch = command::run_in_optional(
        &component.local_path,
        "git",
        &["symbolic-ref", "--short", "HEAD"],
    )
    .map(|branch| format!("branch '{branch}'"))
    .unwrap_or_else(|| "detached HEAD".to_string());
    let remote = source_remote(component);
    let default_branch = default_branch(component);
    let remote_default_ref = format!("{remote}/{default_branch}");

    let remote_default_revision = git::run_git(
        std::path::Path::new(&component.local_path),
        &["rev-parse", &remote_default_ref],
        "git rev-parse remote default branch",
    )
    .ok()
    .map(|value| value.trim().to_string())
    .filter(|value| !value.is_empty())
    .ok_or_else(|| {
        Error::validation_invalid_argument(
            "release",
            format!(
                "Refusing to release from {} because the repo default branch '{}' is not available as '{}'",
                current_branch, default_branch, remote_default_ref
            ),
            None,
            Some(vec![
                format!(
                    "Fetch the default branch before running `homeboy release --apply`: git fetch {} {}",
                    remote, default_branch
                ),
                format!(
                    "Release from '{}' so the tag target is published through the default branch",
                    default_branch
                ),
            ]),
        )
    })?;

    if git::is_ancestor(&component.local_path, &remote_default_revision, "HEAD")? {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "release",
        format!(
            "Refusing to release from {} because it is not safely based on the repo default branch '{}'",
            current_branch, default_branch
        ),
        None,
        Some(vec![
            format!(
                "Rebase or merge '{}' onto '{}' before running `homeboy release --apply`",
                current_branch, remote_default_ref
            ),
            format!(
                "Release from '{}' so the tag target is reachable from the default branch",
                default_branch
            ),
        ]),
    ))
}

pub(super) fn validate_head_reachable_from_default_branch(component: &Component) -> Result<()> {
    let current_branch = command::run_in_optional(
        &component.local_path,
        "git",
        &["symbolic-ref", "--short", "HEAD"],
    )
    .unwrap_or_else(|| "detached HEAD".to_string());
    let remote = source_remote(component);
    let default_branch = default_branch(component);
    let remote_default_ref = format!("{remote}/{default_branch}");
    let remote_default_revision = git::run_git(
        std::path::Path::new(&component.local_path),
        &["rev-parse", &remote_default_ref],
        "git rev-parse remote default branch",
    )
    .ok()
    .map(|value| value.trim().to_string())
    .filter(|value| !value.is_empty())
    .ok_or_else(|| {
        Error::validation_invalid_argument(
            "release",
            format!(
                "Refusing to release from {} because the repo default branch '{}' is not available as '{}'",
                current_branch, default_branch, remote_default_ref
            ),
            None,
            Some(vec![format!(
                "Fetch the default branch before running `homeboy release --apply`: git fetch {} {}",
                remote, default_branch
            )]),
        )
    })?;

    if git::is_ancestor(&component.local_path, "HEAD", &remote_default_revision)? {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "release",
        format!(
            "Refusing to release from {} because HEAD is not reachable from the repo default branch '{}'",
            current_branch, default_branch
        ),
        None,
        Some(vec![
            format!(
                "Check out '{}' or move the release tag target onto '{}' before running `homeboy release --apply`",
                default_branch, remote_default_ref
            ),
            format!(
                "Publish the release commit through '{}' before creating a GitHub Release",
                default_branch
            ),
        ]),
    ))
}

fn head_matches_remote_default(component: &Component, default_branch: &str) -> Result<bool> {
    let remote = source_remote(component);
    let remote_default_ref = format!("{remote}/{default_branch}");
    let Ok(remote_default_revision) = remote_default_revision(component, &remote_default_ref)
    else {
        return Ok(false);
    };
    let head_revision = head_revision(component)?;

    Ok(head_revision == remote_default_revision)
}

fn head_revision(component: &Component) -> Result<String> {
    git::run_git(
        std::path::Path::new(&component.local_path),
        &["rev-parse", "HEAD"],
        "git rev-parse HEAD",
    )
    .map(|value| value.trim().to_string())
    .map_err(|error| {
        Error::validation_invalid_argument(
            "release",
            format!("Refusing to release because HEAD could not be resolved: {error}"),
            None,
            Some(vec![
                "Ensure the checkout has at least one commit before releasing".to_string(),
            ]),
        )
    })
}

fn remote_default_revision(component: &Component, remote_default_ref: &str) -> Result<String> {
    git::run_git(
        std::path::Path::new(&component.local_path),
        &["rev-parse", remote_default_ref],
        "git rev-parse remote default branch",
    )
    .ok()
    .map(|value| value.trim().to_string())
    .filter(|value| !value.is_empty())
    .ok_or_else(|| {
        let current_branch =
            current_branch(component).unwrap_or_else(|_| "detached HEAD".to_string());
        Error::validation_invalid_argument(
            "release",
            format!(
                "Refusing to release from branch '{}' because the repo default branch is not available as '{}'",
                current_branch, remote_default_ref
            ),
            None,
            Some(vec![format!(
                "Fetch the default branch before running `homeboy release --apply`: git fetch {}",
                source_remote(component)
            )]),
        )
    })
}

fn current_branch(component: &Component) -> Result<String> {
    command::run_in_optional(
        &component.local_path,
        "git",
        &["symbolic-ref", "--short", "HEAD"],
    )
    .ok_or_else(|| {
        Error::validation_invalid_argument(
            "release",
            "Refusing to release from detached HEAD",
            None,
            Some(vec![
                "Check out the default branch before releasing".to_string()
            ]),
        )
    })
}

pub(super) fn default_branch(component: &Component) -> String {
    git::default_branch_name(std::path::Path::new(&component.local_path))
        .unwrap_or_else(|| "main".to_string())
}

/// Remote name to use for the component's source repo (resolved, not assumed).
fn source_remote(component: &Component) -> String {
    git::resolve_default_remote(std::path::Path::new(&component.local_path))
}

#[cfg(test)]
mod tests {
    use super::{
        release_push_branch, validate_default_branch, validate_default_branch_ancestry,
        validate_head_reachable_from_default_branch, validate_remote_sync,
    };
    use homeboy_core::component::Component;

    use homeboy_core::test_support::run_git_command as run_git;

    fn git_component(dir: &std::path::Path) -> Component {
        Component {
            id: "fixture".to_string(),
            local_path: dir.to_string_lossy().to_string(),
            ..Default::default()
        }
    }

    fn configure_git_user(dir: &std::path::Path) {
        run_git(dir, &["config", "user.email", "test@example.com"]);
        run_git(dir, &["config", "user.name", "Test"]);
    }

    #[test]
    fn test_validate_default_branch_allows_default_branch() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path();
        run_git(dir, &["init", "-q"]);
        run_git(dir, &["symbolic-ref", "HEAD", "refs/heads/main"]);

        validate_default_branch(&git_component(dir)).expect("main should be allowed");
    }

    #[test]
    fn test_validate_default_branch_allows_detached_default_tip() {
        let temp = tempfile::tempdir().expect("tempdir");
        let remote = temp.path().join("remote.git");
        let seed = temp.path().join("seed");
        let checkout = temp.path().join("checkout");
        let remote_str = remote.to_string_lossy().to_string();

        run_git(
            temp.path(),
            &["init", "--bare", "--initial-branch", "main", &remote_str],
        );
        run_git(temp.path(), &["clone", &remote_str, "seed"]);
        configure_git_user(&seed);
        std::fs::write(seed.join("README.md"), "fixture\n").expect("write fixture");
        run_git(&seed, &["add", "."]);
        run_git(&seed, &["commit", "-q", "-m", "Initial commit"]);
        run_git(&seed, &["push", "-q", "origin", "main"]);

        run_git(temp.path(), &["clone", &remote_str, "checkout"]);
        run_git(&checkout, &["checkout", "-q", "--detach", "origin/main"]);

        validate_default_branch(&git_component(&checkout))
            .expect("detached default tip should be allowed");
    }

    /// The release plan detaches by construction when a caller nominates a
    /// prepared ref, so every default-branch check on that path has to agree
    /// that a detached tip is the default branch. `validate_default_branch`
    /// alone is not enough: planning the push and the ancestry check run
    /// against the same detached checkout.
    #[test]
    fn test_detached_default_tip_passes_every_default_branch_check() {
        let temp = tempfile::tempdir().expect("tempdir");
        let remote = temp.path().join("remote.git");
        let seed = temp.path().join("seed");
        let checkout = temp.path().join("checkout");
        let remote_str = remote.to_string_lossy().to_string();

        run_git(
            temp.path(),
            &["init", "--bare", "--initial-branch", "main", &remote_str],
        );
        run_git(temp.path(), &["clone", &remote_str, "seed"]);
        configure_git_user(&seed);
        std::fs::write(seed.join("README.md"), "fixture\n").expect("write fixture");
        run_git(&seed, &["add", "."]);
        run_git(&seed, &["commit", "-q", "-m", "Initial commit"]);
        run_git(&seed, &["push", "-q", "origin", "main"]);

        run_git(temp.path(), &["clone", &remote_str, "checkout"]);
        configure_git_user(&checkout);
        run_git(&checkout, &["checkout", "-q", "--detach", "origin/main"]);

        let component = git_component(&checkout);

        validate_default_branch(&component).expect("detached default tip should be allowed");
        validate_default_branch_ancestry(&component)
            .expect("detached default tip should satisfy ancestry");
        assert_eq!(
            release_push_branch(&component).expect("detached default tip should plan a push"),
            "main"
        );
    }

    /// A detached HEAD that is not the default branch must still be refused,
    /// and the refusal has to name the branch it would push to rather than
    /// reporting only that HEAD is detached.
    #[test]
    fn test_release_push_branch_blocks_detached_non_default_commits() {
        let temp = tempfile::tempdir().expect("tempdir");
        let remote = temp.path().join("remote.git");
        let seed = temp.path().join("seed");
        let checkout = temp.path().join("checkout");
        let remote_str = remote.to_string_lossy().to_string();

        run_git(
            temp.path(),
            &["init", "--bare", "--initial-branch", "main", &remote_str],
        );
        run_git(temp.path(), &["clone", &remote_str, "seed"]);
        configure_git_user(&seed);
        std::fs::write(seed.join("README.md"), "initial\n").expect("write fixture");
        run_git(&seed, &["add", "."]);
        run_git(&seed, &["commit", "-q", "-m", "Initial commit"]);
        run_git(&seed, &["push", "-q", "origin", "main"]);
        std::fs::write(seed.join("README.md"), "tip\n").expect("write fixture");
        run_git(&seed, &["add", "."]);
        run_git(&seed, &["commit", "-q", "-m", "Tip commit"]);
        run_git(&seed, &["push", "-q", "origin", "main"]);

        run_git(temp.path(), &["clone", &remote_str, "checkout"]);
        configure_git_user(&checkout);
        run_git(&checkout, &["checkout", "-q", "--detach", "origin/main~1"]);

        let err = release_push_branch(&git_component(&checkout))
            .expect_err("detached older commit should not plan a push");
        assert!(err.message.contains("detached HEAD"));
        assert!(err.message.contains("main"));
    }

    #[test]
    fn test_validate_default_branch_blocks_detached_non_default_commits() {
        let temp = tempfile::tempdir().expect("tempdir");
        let remote = temp.path().join("remote.git");
        let seed = temp.path().join("seed");
        let checkout = temp.path().join("checkout");
        let remote_str = remote.to_string_lossy().to_string();

        run_git(
            temp.path(),
            &["init", "--bare", "--initial-branch", "main", &remote_str],
        );
        run_git(temp.path(), &["clone", &remote_str, "seed"]);
        configure_git_user(&seed);
        std::fs::write(seed.join("README.md"), "initial\n").expect("write fixture");
        run_git(&seed, &["add", "."]);
        run_git(&seed, &["commit", "-q", "-m", "Initial commit"]);
        run_git(&seed, &["push", "-q", "origin", "main"]);
        std::fs::write(seed.join("README.md"), "tip\n").expect("write fixture");
        run_git(&seed, &["add", "."]);
        run_git(&seed, &["commit", "-q", "-m", "Tip commit"]);
        run_git(&seed, &["push", "-q", "origin", "main"]);

        run_git(temp.path(), &["clone", &remote_str, "checkout"]);
        configure_git_user(&checkout);
        run_git(&checkout, &["checkout", "-q", "--detach", "origin/main~1"]);
        let old_revision = git_revision(&checkout, "HEAD");
        let default_revision = git_revision(&checkout, "origin/main");

        let err = validate_default_branch(&git_component(&checkout))
            .expect_err("detached older commit should fail");
        assert!(err.message.contains(&old_revision));
        assert!(err.message.contains(&default_revision));

        run_git(&checkout, &["checkout", "-q", "--orphan", "unrelated"]);
        std::fs::write(checkout.join("README.md"), "unrelated\n").expect("write fixture");
        run_git(&checkout, &["add", "."]);
        run_git(&checkout, &["commit", "-q", "-m", "Unrelated commit"]);
        run_git(&checkout, &["checkout", "-q", "--detach"]);
        let unrelated_revision = git_revision(&checkout, "HEAD");

        let err = validate_default_branch(&git_component(&checkout))
            .expect_err("detached unrelated commit should fail");
        assert!(err.message.contains(&unrelated_revision));
        assert!(err.message.contains(&default_revision));
    }

    fn git_revision(dir: &std::path::Path, reference: &str) -> String {
        let output = std::process::Command::new("git")
            .args(["rev-parse", reference])
            .current_dir(dir)
            .output()
            .expect("read revision");
        assert!(output.status.success());
        String::from_utf8(output.stdout)
            .expect("revision is UTF-8")
            .trim()
            .to_string()
    }

    #[test]
    fn test_validate_default_branch_allows_default_branch_with_non_origin_remote() {
        // A framework-agnostic orchestrator must release repos whose remote is
        // not named `origin`. Clone, rename the remote to `upstream`, and verify
        // the default-branch validation still resolves the default through the
        // renamed remote.
        let temp = tempfile::tempdir().expect("tempdir");
        let remote = temp.path().join("remote.git");
        let seed = temp.path().join("seed");
        let checkout = temp.path().join("checkout");
        let remote_str = remote.to_string_lossy().to_string();

        run_git(
            temp.path(),
            &["init", "--bare", "--initial-branch", "main", &remote_str],
        );
        run_git(temp.path(), &["clone", &remote_str, "seed"]);
        configure_git_user(&seed);
        std::fs::write(seed.join("README.md"), "fixture\n").expect("write fixture");
        run_git(&seed, &["add", "."]);
        run_git(&seed, &["commit", "-q", "-m", "Initial commit"]);
        run_git(&seed, &["push", "-q", "origin", "main"]);

        run_git(temp.path(), &["clone", &remote_str, "checkout"]);
        configure_git_user(&checkout);
        run_git(&checkout, &["remote", "rename", "origin", "upstream"]);

        validate_default_branch(&git_component(&checkout))
            .expect("default branch should be allowed through a non-origin remote");
    }

    #[test]
    fn test_validate_default_branch_blocks_non_default_branch() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path();
        run_git(dir, &["init", "-q"]);
        run_git(dir, &["symbolic-ref", "HEAD", "refs/heads/feature"]);

        let err = validate_default_branch(&git_component(dir)).expect_err("feature should fail");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err
            .message
            .contains("branch 'feature' because the repo default branch is 'main'"));
    }

    #[test]
    fn test_validate_default_branch_allows_release_branch_at_remote_default_tip() {
        let temp = tempfile::tempdir().expect("tempdir");
        let remote = temp.path().join("remote.git");
        let seed = temp.path().join("seed");
        let checkout = temp.path().join("checkout");
        let remote_str = remote.to_string_lossy().to_string();

        run_git(
            temp.path(),
            &["init", "--bare", "--initial-branch", "main", &remote_str],
        );
        run_git(temp.path(), &["clone", &remote_str, "seed"]);
        configure_git_user(&seed);
        std::fs::write(seed.join("README.md"), "fixture\n").expect("write fixture");
        run_git(&seed, &["add", "."]);
        run_git(&seed, &["commit", "-q", "-m", "Initial commit"]);
        run_git(&seed, &["push", "-q", "origin", "main"]);

        run_git(temp.path(), &["clone", &remote_str, "checkout"]);
        run_git(&checkout, &["checkout", "-q", "-b", "release-local"]);

        validate_default_branch(&git_component(&checkout))
            .expect("release branch at origin/main tip should pass");
        assert_eq!(
            release_push_branch(&git_component(&checkout)).expect("push branch"),
            "main"
        );
    }

    #[test]
    fn test_validate_default_branch_blocks_feature_branch_ahead_of_remote_default() {
        let temp = tempfile::tempdir().expect("tempdir");
        let remote = temp.path().join("remote.git");
        let seed = temp.path().join("seed");
        let checkout = temp.path().join("checkout");
        let remote_str = remote.to_string_lossy().to_string();

        run_git(
            temp.path(),
            &["init", "--bare", "--initial-branch", "main", &remote_str],
        );
        run_git(temp.path(), &["clone", &remote_str, "seed"]);
        configure_git_user(&seed);
        std::fs::write(seed.join("README.md"), "fixture\n").expect("write fixture");
        run_git(&seed, &["add", "."]);
        run_git(&seed, &["commit", "-q", "-m", "Initial commit"]);
        run_git(&seed, &["push", "-q", "origin", "main"]);

        run_git(temp.path(), &["clone", &remote_str, "checkout"]);
        configure_git_user(&checkout);
        run_git(&checkout, &["checkout", "-q", "-b", "feature"]);
        std::fs::write(checkout.join("README.md"), "fixture\nfeature\n").expect("write feature");
        run_git(&checkout, &["add", "."]);
        run_git(&checkout, &["commit", "-q", "-m", "Feature commit"]);

        let err = validate_default_branch(&git_component(&checkout))
            .expect_err("feature branch ahead of origin/main should fail");

        assert!(err
            .message
            .contains("branch 'feature' because the repo default branch is 'main'"));
        assert!(err.details.to_string().contains(
            "release from 'main' so the tag target is published through the default branch"
        ));
    }

    #[test]
    fn test_validate_default_branch_ancestry_blocks_default_branch_not_based_on_remote_default() {
        let temp = tempfile::tempdir().expect("tempdir");
        let remote = temp.path().join("remote.git");
        let seed = temp.path().join("seed");
        let checkout = temp.path().join("checkout");
        let remote_str = remote.to_string_lossy().to_string();

        run_git(
            temp.path(),
            &["init", "--bare", "--initial-branch", "main", &remote_str],
        );
        run_git(temp.path(), &["clone", &remote_str, "seed"]);
        configure_git_user(&seed);
        std::fs::write(seed.join("README.md"), "fixture\n").expect("write fixture");
        run_git(&seed, &["add", "."]);
        run_git(&seed, &["commit", "-q", "-m", "Initial commit"]);
        run_git(&seed, &["push", "-q", "origin", "main"]);

        run_git(temp.path(), &["clone", &remote_str, "checkout"]);
        configure_git_user(&checkout);
        run_git(&checkout, &["checkout", "--orphan", "replacement"]);
        std::fs::write(checkout.join("README.md"), "replacement\n").expect("write fixture");
        run_git(&checkout, &["add", "."]);
        run_git(&checkout, &["commit", "-q", "-m", "Replacement root"]);
        run_git(&checkout, &["branch", "-M", "main"]);

        let err = validate_default_branch_ancestry(&git_component(&checkout))
            .expect_err("unrelated local main should fail");

        assert!(err.message.contains(
            "branch 'main' because it is not safely based on the repo default branch 'main'"
        ));
        assert!(err.details.to_string().contains("homeboy release --apply"));
    }

    #[test]
    fn test_validate_head_reachable_from_default_branch_blocks_detached_unreachable_head() {
        let temp = tempfile::tempdir().expect("tempdir");
        let remote = temp.path().join("remote.git");
        let seed = temp.path().join("seed");
        let checkout = temp.path().join("checkout");
        let remote_str = remote.to_string_lossy().to_string();

        run_git(
            temp.path(),
            &["init", "--bare", "--initial-branch", "main", &remote_str],
        );
        run_git(temp.path(), &["clone", &remote_str, "seed"]);
        configure_git_user(&seed);
        std::fs::write(seed.join("README.md"), "fixture\n").expect("write fixture");
        run_git(&seed, &["add", "."]);
        run_git(&seed, &["commit", "-q", "-m", "Initial commit"]);
        run_git(&seed, &["push", "-q", "origin", "main"]);

        run_git(temp.path(), &["clone", &remote_str, "checkout"]);
        configure_git_user(&checkout);
        run_git(&checkout, &["checkout", "--orphan", "replacement"]);
        std::fs::write(checkout.join("README.md"), "replacement\n").expect("write fixture");
        run_git(&checkout, &["add", "."]);
        run_git(&checkout, &["commit", "-q", "-m", "Replacement root"]);
        run_git(&checkout, &["checkout", "--detach"]);

        let err = validate_head_reachable_from_default_branch(&git_component(&checkout))
            .expect_err("detached unreachable HEAD should fail");

        assert!(err.message.contains(
            "detached HEAD because HEAD is not reachable from the repo default branch 'main'"
        ));
        assert!(err
            .details
            .to_string()
            .contains("creating a GitHub Release"));
    }

    #[test]
    fn test_validate_remote_sync() {
        let temp = tempfile::tempdir().expect("tempdir");
        let remote = temp.path().join("remote.git");
        let seed = temp.path().join("seed");
        let checkout = temp.path().join("checkout");
        let remote_str = remote.to_string_lossy().to_string();

        run_git(
            temp.path(),
            &["init", "--bare", "--initial-branch", "main", &remote_str],
        );
        run_git(temp.path(), &["clone", &remote_str, "seed"]);
        configure_git_user(&seed);
        std::fs::write(seed.join("README.md"), "fixture\n").expect("write fixture");
        run_git(&seed, &["add", "."]);
        run_git(&seed, &["commit", "-q", "-m", "Initial commit"]);
        run_git(&seed, &["push", "-q", "origin", "main"]);

        run_git(temp.path(), &["clone", &remote_str, "checkout"]);
        configure_git_user(&checkout);

        std::fs::write(seed.join("README.md"), "fixture\nsecond\n").expect("write update");
        run_git(&seed, &["add", "."]);
        run_git(&seed, &["commit", "-q", "-m", "Second commit"]);
        run_git(&seed, &["push", "-q", "origin", "main"]);

        validate_remote_sync(&git_component(&checkout)).expect("checkout should fast-forward");

        assert_eq!(
            std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&checkout)
                .output()
                .expect("read HEAD")
                .stdout,
            std::process::Command::new("git")
                .args(["rev-parse", "origin/main"])
                .current_dir(&checkout)
                .output()
                .expect("read origin/main")
                .stdout
        );
    }
}
