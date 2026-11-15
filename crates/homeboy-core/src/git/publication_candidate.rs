use crate::{Error, Result};

/// Re-author a candidate tree for handoff without changing its parents, index,
/// or worktree. Internal candidate commits are evidence, not publication commits.
pub fn publication_candidate(path: &str, revision: &str, title: &str) -> Result<String> {
    if title.trim().is_empty() || title.contains('\n') {
        return Err(Error::validation_invalid_argument(
            "title",
            "candidate requires a nonempty single-line publication title",
            None,
            None,
        ));
    }
    let identity = super::validate_publication_identity(path)?;
    let git = |args: &[&str]| -> Result<String> {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .output()
            .map_err(|e| Error::internal_io(e.to_string(), None))?;
        if !output.status.success() {
            return Err(Error::validation_invalid_argument(
                "candidate",
                String::from_utf8_lossy(&output.stderr).trim(),
                None,
                None,
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    let tree = git(&["rev-parse", &format!("{revision}^{{tree}}")])?;
    let lineage = git(&["rev-list", "--parents", "-n", "1", revision])?;
    let mut args = vec!["commit-tree", &tree];
    for parent in lineage.split_whitespace().skip(1) {
        args.extend(["-p", parent]);
    }
    args.extend(["-m", title]);
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .env("GIT_AUTHOR_NAME", &identity.name)
        .env("GIT_AUTHOR_EMAIL", &identity.email)
        .env("GIT_COMMITTER_NAME", &identity.committer_name)
        .env("GIT_COMMITTER_EMAIL", &identity.committer_email)
        .output()
        .map_err(|e| Error::internal_io(e.to_string(), None))?;
    if !output.status.success() {
        return Err(Error::validation_invalid_argument(
            "candidate",
            String::from_utf8_lossy(&output.stderr).trim(),
            None,
            None,
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
