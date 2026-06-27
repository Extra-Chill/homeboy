use crate::core::component::Component;
use crate::core::engine::command;
use crate::core::error::{Error, Result};
use crate::core::git;

/// Fetch from remote and fast-forward if behind.
///
/// Ensures the release commit is created on top of the actual remote HEAD,
/// preventing detached release tags when PRs merge during a CI quality gate.
/// Returns Err if the branch has diverged and can't be fast-forwarded.
pub(super) fn validate_remote_sync(component: &Component) -> Result<()> {
    let synced = git::fetch_and_fast_forward(&component.local_path)?;

    if let Some(n) = synced {
        log_status!(
            "release",
            "Fast-forwarded {} commit(s) from remote before release",
            n
        );
    }

    Ok(())
}

pub(super) fn validate_default_branch(component: &Component) -> Result<()> {
    let current_branch = current_branch(component)?;
    let default_branch = default_branch(component);

    if current_branch == default_branch {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "release",
        format!(
            "Refusing to release from branch '{}' because the repo default branch is '{}'",
            current_branch, default_branch
        ),
        None,
        Some(vec![
            format!(
                "Check out '{}' before running `homeboy release --apply` for a default-branch release workflow",
                default_branch
            ),
            format!(
                "Rebase or merge '{}' onto '{}' and release from '{}' so the tag target is published through the default branch",
                current_branch, default_branch, default_branch
            ),
            "If you only want a preview, use --dry-run".to_string(),
        ]),
    ))
}

pub(super) fn validate_default_branch_ancestry(component: &Component) -> Result<()> {
    let current_branch = current_branch(component)?;
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
                "Refusing to release from branch '{}' because the repo default branch '{}' is not available as '{}'",
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
            "Refusing to release from branch '{}' because it is not safely based on the repo default branch '{}'",
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

fn default_branch(component: &Component) -> String {
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
        validate_default_branch, validate_default_branch_ancestry,
        validate_head_reachable_from_default_branch, validate_remote_sync,
    };
    use crate::core::component::Component;

    fn run_git(dir: &std::path::Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {:?} failed: stdout={} stderr={}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

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
    fn test_validate_default_branch_blocks_release_branch_at_remote_default_tip() {
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

        let err = validate_default_branch(&git_component(&checkout))
            .expect_err("release branch at origin/main tip should fail");

        assert!(err
            .message
            .contains("branch 'release-local' because the repo default branch is 'main'"));
        assert!(err.details.to_string().contains("homeboy release --apply"));
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
