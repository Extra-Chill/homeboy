use std::io::Write;

use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use crate::agent_task::AgentTaskArtifact;
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NormalizedPromotionPatch {
    pub(crate) content: String,
    pub(crate) changed_files: Vec<String>,
}

pub(crate) fn normalize_promotion_patch(
    patch: &str,
    target_workspace: &str,
) -> Result<NormalizedPromotionPatch> {
    if patch.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "patch",
            "promotion refuses an empty patch artifact",
            None,
            None,
        ));
    }

    let repo_slug = target_workspace_repo_slug(target_workspace);
    let mut changed_files = Vec::new();
    let mut normalized_lines = Vec::new();
    for line in patch.lines() {
        let normalized_line = normalize_patch_header_line(line, &repo_slug)?;
        if let Some(path) = line
            .strip_prefix("+++ ")
            .or_else(|| line.strip_prefix("--- "))
        {
            let path = normalized_line
                .strip_prefix("+++ ")
                .or_else(|| normalized_line.strip_prefix("--- "))
                .unwrap_or(path)
                .trim();
            if path == "/dev/null" {
                normalized_lines.push(normalized_line);
                continue;
            }
            let path = path
                .strip_prefix("a/")
                .or_else(|| path.strip_prefix("b/"))
                .unwrap_or(path);
            validate_patch_path(path)?;
            if !changed_files.iter().any(|existing| existing == path) {
                changed_files.push(path.to_string());
            }
        }
        normalized_lines.push(normalized_line);
    }

    if changed_files.is_empty() {
        return Err(Error::validation_invalid_argument(
            "patch",
            "promotion requires a unified diff with changed file headers",
            None,
            None,
        ));
    }

    let mut content = normalized_lines.join("\n");
    if patch.ends_with('\n') {
        content.push('\n');
    }

    Ok(NormalizedPromotionPatch {
        content,
        changed_files,
    })
}

pub(crate) fn write_normalized_patch(content: &str) -> Result<NamedTempFile> {
    let mut file = NamedTempFile::new().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("create normalized promotion patch".to_string()),
        )
    })?;
    file.write_all(content.as_bytes()).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!(
                "write normalized promotion patch {}",
                file.path().display()
            )),
        )
    })?;
    Ok(file)
}

fn normalize_patch_header_line(line: &str, repo_slug: &str) -> Result<String> {
    if let Some(rest) = line.strip_prefix("diff --git ") {
        let mut parts = rest.split_whitespace();
        let Some(old_path) = parts.next() else {
            return Ok(line.to_string());
        };
        let Some(new_path) = parts.next() else {
            return Ok(line.to_string());
        };
        if parts.next().is_some() {
            return Ok(line.to_string());
        }
        return Ok(format!(
            "diff --git {} {}",
            normalize_prefixed_diff_path(old_path, repo_slug)?,
            normalize_prefixed_diff_path(new_path, repo_slug)?
        ));
    }

    for prefix in ["--- ", "+++ "] {
        if let Some(path) = line.strip_prefix(prefix) {
            return Ok(format!(
                "{prefix}{}",
                normalize_prefixed_diff_path(path.trim(), repo_slug)?
            ));
        }
    }

    for prefix in ["rename from ", "rename to ", "copy from ", "copy to "] {
        if let Some(path) = line.strip_prefix(prefix) {
            return Ok(format!(
                "{prefix}{}",
                normalize_sandbox_path(path.trim(), repo_slug)?
            ));
        }
    }

    Ok(line.to_string())
}

fn normalize_prefixed_diff_path(path: &str, repo_slug: &str) -> Result<String> {
    if path == "/dev/null" {
        return Ok(path.to_string());
    }
    if let Some(path) = path.strip_prefix("a/") {
        return Ok(format!("a/{}", normalize_sandbox_path(path, repo_slug)?));
    }
    if let Some(path) = path.strip_prefix("b/") {
        return Ok(format!("b/{}", normalize_sandbox_path(path, repo_slug)?));
    }
    normalize_sandbox_path(path, repo_slug)
}

fn normalize_sandbox_path(path: &str, repo_slug: &str) -> Result<String> {
    let Some(rest) = path.strip_prefix("workspace/") else {
        return Ok(path.to_string());
    };
    let Some((sandbox, repo_relative)) = rest.split_once('/') else {
        if sandbox_belongs_to_repo(rest, repo_slug) {
            return Err(Error::validation_invalid_argument(
                "patch",
                format!("Lab sandbox patch path has no repo-relative suffix: {path}"),
                None,
                Some(vec![
                    "Expected paths shaped like workspace/<sandbox-worktree>/<repo-relative-path>.".to_string(),
                    "Regenerate the patch from the repository root or include Lab workspace mapping metadata.".to_string(),
                ]),
            ));
        }
        return Ok(path.to_string());
    };
    if !sandbox_belongs_to_repo(sandbox, repo_slug) {
        return Ok(path.to_string());
    }
    validate_patch_path(repo_relative)?;
    Ok(repo_relative.to_string())
}

fn sandbox_belongs_to_repo(sandbox: &str, repo_slug: &str) -> bool {
    sandbox == repo_slug
        || sandbox
            .strip_prefix(repo_slug)
            .is_some_and(|rest| rest.starts_with('-') || rest.starts_with('@'))
}

fn target_workspace_repo_slug(handle: &str) -> String {
    handle
        .split('@')
        .next()
        .unwrap_or(handle)
        .trim()
        .to_string()
}

pub(crate) fn validate_artifact_content(artifact: &AgentTaskArtifact, patch: &str) -> Result<()> {
    if let Some(expected_size) = artifact.size_bytes {
        let actual_size = patch.len() as u64;
        if expected_size != actual_size {
            return Err(Error::validation_invalid_argument(
                "artifact.size_bytes",
                format!(
                    "patch artifact size mismatch: expected {expected_size} bytes, read {actual_size} bytes"
                ),
                Some(artifact.id.clone()),
                None,
            ));
        }
    }

    if let Some(expected_sha256) = artifact.sha256.as_deref() {
        let mut hasher = Sha256::new();
        hasher.update(patch.as_bytes());
        let actual_sha256 = format!("{:x}", hasher.finalize());
        if expected_sha256 != actual_sha256 {
            return Err(Error::validation_invalid_argument(
                "artifact.sha256",
                format!(
                    "patch artifact sha256 mismatch: expected {expected_sha256}, read {actual_sha256}"
                ),
                Some(artifact.id.clone()),
                None,
            ));
        }
    }

    Ok(())
}

fn validate_patch_path(path: &str) -> Result<()> {
    let invalid = path.starts_with('/')
        || path.starts_with("../")
        || path.contains("/../")
        || path == ".."
        || path.starts_with(".git/")
        || path.contains("/.git/");
    if invalid {
        return Err(Error::validation_invalid_argument(
            "patch",
            format!("promotion refuses unsafe patch path: {path}"),
            None,
            None,
        ));
    }
    Ok(())
}
