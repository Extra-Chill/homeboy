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

    Err(Error::validation_invalid_argument(
        "release",
        format!(
            "Refusing to release from non-default branch '{}' (default: '{}')",
            current_branch, default_branch
        ),
        None,
        Some(vec![
            format!("Check out '{}' before releasing", default_branch),
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
