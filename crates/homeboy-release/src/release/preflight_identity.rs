use homeboy_core::component::Component;
use homeboy_core::error::{Error, Result};
use homeboy_core::git;

use super::types::ReleasePreflightSourceIdentity;

/// Freeze the commit whose portable quality evidence may authorize a release.
pub(super) fn capture(component: &Component) -> Result<ReleasePreflightSourceIdentity> {
    Ok(ReleasePreflightSourceIdentity {
        commit: git::get_head_commit(&component.local_path)?,
    })
}

/// Refuse controller mutation when the source inspected by readiness gates moved.
pub(super) fn revalidate(
    component: &Component,
    expected: &ReleasePreflightSourceIdentity,
) -> Result<()> {
    let actual = git::get_head_commit(&component.local_path)?;
    if actual == expected.commit {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "release.preflight_source",
        format!(
            "Release preflight source drift: portable gates validated commit {} but controller mutation would run at {}",
            expected.commit, actual
        ),
        Some(actual.clone()),
        Some(vec![format!(
            "Rerun release preflight so its evidence is bound to the current commit {actual}."
        )]),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn revalidation_fails_closed_when_the_source_moves_after_preflight() {
        let repo = tempfile::tempdir().expect("repo");
        run_git(repo.path(), &["init", "-q"]);
        run_git(repo.path(), &["config", "user.email", "test@example.com"]);
        run_git(repo.path(), &["config", "user.name", "Test"]);
        std::fs::write(repo.path().join("file"), "one\n").expect("write source");
        run_git(repo.path(), &["add", "."]);
        run_git(repo.path(), &["commit", "-q", "-m", "first"]);
        let component = Component {
            local_path: repo.path().display().to_string(),
            ..Component::default()
        };
        let identity = capture(&component).expect("capture identity");

        std::fs::write(repo.path().join("file"), "two\n").expect("move source");
        run_git(repo.path(), &["add", "."]);
        run_git(repo.path(), &["commit", "-q", "-m", "second"]);

        let error =
            revalidate(&component, &identity).expect_err("source drift must block mutation");
        assert_eq!(error.code.as_str(), "validation.invalid_argument");
        assert!(error.message.contains("Release preflight source drift"));
    }

    fn run_git(path: &std::path::Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("git");
        assert!(
            output.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
