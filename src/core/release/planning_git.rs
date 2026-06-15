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
    let path = std::path::Path::new(&component.local_path);
    let current_branch = command::run_in_optional(
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
    })?;

    let default_branch = command::run_in_optional(
        &component.local_path,
        "git",
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .map(|value| value.trim().trim_start_matches("origin/").to_string())
    .filter(|value| !value.is_empty())
    .unwrap_or_else(|| "main".to_string());

    if current_branch == default_branch {
        return Ok(());
    }

    let remote_default_ref = format!("origin/{default_branch}");
    let head_revision = git::run_git(path, &["rev-parse", "HEAD"], "git rev-parse HEAD")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let remote_default_revision = git::run_git(
        path,
        &["rev-parse", &remote_default_ref],
        "git rev-parse remote default branch",
    )
    .ok()
    .map(|value| value.trim().to_string())
    .filter(|value| !value.is_empty());

    if head_revision.is_some() && head_revision == remote_default_revision {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "release",
        format!(
            "Refusing to release from non-default branch '{}' (default: '{}')",
            current_branch, default_branch
        ),
        None,
        Some(vec![
            format!("Check out '{}' before releasing", default_branch),
            format!(
                "A managed release worktree on another local branch is allowed only when HEAD exactly matches {}",
                remote_default_ref
            ),
            "If you only want a preview, use --dry-run".to_string(),
        ]),
    ))
}

#[cfg(test)]
mod tests {
    use super::{validate_default_branch, validate_remote_sync};
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
    fn test_validate_default_branch_blocks_non_default_branch() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path();
        run_git(dir, &["init", "-q"]);
        run_git(dir, &["symbolic-ref", "HEAD", "refs/heads/feature"]);

        let err = validate_default_branch(&git_component(dir)).expect_err("feature should fail");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("non-default branch 'feature'"));
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
            .expect("release branch at origin/main tip should be allowed");
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

        assert!(err.message.contains("non-default branch 'feature'"));
        assert!(err
            .details
            .to_string()
            .contains("HEAD exactly matches origin/main"));
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
