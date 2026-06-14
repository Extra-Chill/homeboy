use std::path::Path;
use std::process::Command;

use crate::core::error::{Error, Result};

pub(super) fn advertised_origin_refs_for_commit(
    path: &Path,
    commit: &str,
    error_field: &str,
    error_message: &str,
    error_id: String,
    error_hints: Vec<String>,
) -> Result<Vec<String>> {
    let output = Command::new("git")
        .args(["ls-remote", "origin"])
        .current_dir(path)
        .output()
        .map_err(|err| {
            Error::internal_io(err.to_string(), Some("run git ls-remote".to_string()))
        })?;
    if !output.status.success() {
        let mut hints = vec![String::from_utf8_lossy(&output.stderr).trim().to_string()];
        hints.extend(error_hints);
        return Err(Error::validation_invalid_argument(
            error_field,
            error_message,
            Some(error_id),
            Some(hints),
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let (sha, git_ref) = line.split_once('\t')?;
            (sha == commit && !git_ref.ends_with("^{}")).then(|| git_ref.to_string())
        })
        .collect())
}

pub(super) fn best_advertised_ref(refs: Vec<String>) -> Option<String> {
    refs.iter()
        .find(|git_ref| git_ref.starts_with("refs/pull/") && git_ref.ends_with("/head"))
        .cloned()
        .or_else(|| {
            refs.iter()
                .find(|git_ref| git_ref.starts_with("refs/heads/"))
                .cloned()
        })
        .or_else(|| {
            refs.iter()
                .find(|git_ref| git_ref.starts_with("refs/tags/"))
                .cloned()
        })
        .or_else(|| refs.into_iter().next())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertised_ref_selection_prefers_pull_head_refs() {
        let selected = best_advertised_ref(vec![
            "refs/heads/fix-branch".to_string(),
            "refs/pull/5530/head".to_string(),
            "refs/tags/v1".to_string(),
        ]);

        assert_eq!(selected, Some("refs/pull/5530/head".to_string()));
    }
}
