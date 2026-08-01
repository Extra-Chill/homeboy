//! Portable repository checks evaluated from tracked Git objects.

use std::path::Path;
use std::process::Command;

use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::Value;

use crate::error::{Error, Result};

/// Verify every tracked symlink in `revision` without consulting the checkout.
/// Exceptions are exact repository-relative paths declared in the candidate's
/// `homeboy.json` at `repository_integrity.symlink_exceptions` with a `reason`.
pub fn verify_tracked_symlink_portability(path: &Path, revision: &str) -> Result<()> {
    let Some(tree) = git_optional(
        path,
        &["rev-parse", "--verify", &format!("{revision}^{{tree}}")],
    )?
    else {
        return Ok(());
    };
    let exceptions = exceptions(path, tree.trim())?;
    let entries = git_bytes(path, &["ls-tree", "-rz", "--full-tree", tree.trim()])?;
    for entry in entries
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let Some(separator) = entry.iter().position(|byte| *byte == b'\t') else {
            return Err(Error::git_command_failed(
                "malformed git ls-tree entry".to_string(),
            ));
        };
        let (header, raw_path) = entry.split_at(separator);
        let raw_path = &raw_path[1..];
        let mut fields = header.split(|byte| *byte == b' ');
        if fields.next() != Some(b"120000".as_slice()) {
            continue;
        }
        let Some(object) = fields.nth(1) else {
            return Err(Error::git_command_failed(
                "symlink entry has no blob object".to_string(),
            ));
        };
        let link_path = String::from_utf8_lossy(raw_path).to_string();
        let target = git_bytes(
            path,
            &[
                "cat-file",
                "blob",
                std::str::from_utf8(object).map_err(|_| {
                    Error::git_command_failed("non-UTF-8 Git object id".to_string())
                })?,
            ],
        )?;
        let violation = if is_absolute(&target) {
            Some("absolute")
        } else if lexically_escapes(&link_path, &target) {
            Some("repository-escaping relative")
        } else {
            None
        };
        if let Some(violation) = violation {
            if exceptions
                .iter()
                .any(|(exception_path, _)| exception_path == &link_path)
            {
                continue;
            }
            return Err(Error::validation_invalid_argument(
                "repository_integrity.symlink",
                format!(
                    "candidate revision {} tracks a {} symlink at `{}` with raw target bytes base64 `{}` under the default repository portability policy; replace it with an internal relative target, or add an exact reviewed exception with a reason in candidate homeboy.json repository_integrity.symlink_exceptions",
                    revision, violation, link_path, STANDARD.encode(&target)
                ),
                Some(link_path),
                None,
            ));
        }
    }
    Ok(())
}

fn exceptions(path: &Path, tree: &str) -> Result<Vec<(String, String)>> {
    let Some(raw) = git_optional(path, &["show", &format!("{tree}:homeboy.json")])? else {
        return Ok(Vec::new());
    };
    let config: Value = serde_json::from_str(&raw).map_err(|error| {
        Error::validation_invalid_json(error, Some("candidate homeboy.json".to_string()), None)
    })?;
    let Some(entries) = config
        .pointer("/repository_integrity/symlink_exceptions")
        .and_then(Value::as_array)
    else {
        return Ok(Vec::new());
    };
    entries
        .iter()
        .map(|entry| {
            let path = entry
                .get("path")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty());
            let reason = entry
                .get("reason")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty());
            match (path, reason) {
                (Some(path), Some(reason)) => Ok((path.to_string(), reason.to_string())),
                _ => Err(Error::validation_invalid_argument(
                    "repository_integrity.symlink_exceptions",
                    "each symlink exception requires exact `path` and reviewed `reason` fields",
                    Some("candidate homeboy.json".to_string()),
                    None,
                )),
            }
        })
        .collect()
}

fn is_absolute(target: &[u8]) -> bool {
    target.starts_with(b"/")
        || target.starts_with(b"\\")
        || (target.len() >= 3
            && target[0].is_ascii_alphabetic()
            && target[1] == b':'
            && matches!(target[2], b'/' | b'\\'))
}

fn lexically_escapes(link_path: &str, target: &[u8]) -> bool {
    let mut depth = link_path.split('/').count().saturating_sub(1);
    for component in target.split(|byte| matches!(*byte, b'/' | b'\\')) {
        if component.is_empty() || component == b"." {
            continue;
        }
        if component == b".." {
            if depth == 0 {
                return true;
            }
            depth -= 1;
        } else {
            depth += 1;
        }
    }
    false
}

fn git_optional(path: &Path, args: &[&str]) -> Result<Option<String>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    if output.status.success() {
        Ok(Some(String::from_utf8_lossy(&output.stdout).to_string()))
    } else {
        Ok(None)
    }
}

fn git_bytes(path: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(Error::git_command_failed(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn repo(link: &str, config: Option<&str>) -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        Command::new("git")
            .args(["init"])
            .current_dir(repo.path())
            .status()
            .unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(link, repo.path().join("link")).unwrap();
        if let Some(config) = config {
            fs::write(repo.path().join("homeboy.json"), config).unwrap();
        }
        Command::new("git")
            .args(["add", "."])
            .current_dir(repo.path())
            .status()
            .unwrap();
        Command::new("git")
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.test",
                "commit",
                "-m",
                "fixture",
            ])
            .current_dir(repo.path())
            .status()
            .unwrap();
        repo
    }

    #[cfg(unix)]
    #[test]
    fn rejects_absolute_and_escaping_targets_but_preserves_internal_unresolved_links() {
        for target in ["/tmp/external", "../../external"] {
            let fixture = repo(target, None);
            let error = verify_tracked_symlink_portability(fixture.path(), "HEAD")
                .expect_err("non-portable link is rejected");
            assert!(error.message.contains("raw target bytes base64"));
            assert!(error.message.contains("homeboy.json"));
        }
        for target in ["missing", "dir/missing"] {
            let fixture = repo(target, None);
            verify_tracked_symlink_portability(fixture.path(), "HEAD").unwrap();
        }
        let fixture = tempfile::tempdir().unwrap();
        Command::new("git")
            .args(["init"])
            .current_dir(fixture.path())
            .status()
            .unwrap();
        fs::create_dir(fixture.path().join("dir")).unwrap();
        std::os::unix::fs::symlink("../missing", fixture.path().join("dir/link")).unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(fixture.path())
            .status()
            .unwrap();
        Command::new("git")
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.test",
                "commit",
                "-m",
                "nested fixture",
            ])
            .current_dir(fixture.path())
            .status()
            .unwrap();
        verify_tracked_symlink_portability(fixture.path(), "HEAD").unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn preserves_internal_relative_cycles() {
        let fixture = tempfile::tempdir().unwrap();
        Command::new("git")
            .args(["init"])
            .current_dir(fixture.path())
            .status()
            .unwrap();
        std::os::unix::fs::symlink("b", fixture.path().join("a")).unwrap();
        std::os::unix::fs::symlink("a", fixture.path().join("b")).unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(fixture.path())
            .status()
            .unwrap();
        Command::new("git")
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.test",
                "commit",
                "-m",
                "cycle fixture",
            ])
            .current_dir(fixture.path())
            .status()
            .unwrap();
        verify_tracked_symlink_portability(fixture.path(), "HEAD").unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn allows_reviewed_path_scoped_exception() {
        let fixture = repo(
            "/opt/shared",
            Some(
                r#"{"repository_integrity":{"symlink_exceptions":[{"path":"link","reason":"shared fixture dependency"}]}}"#,
            ),
        );
        verify_tracked_symlink_portability(fixture.path(), "HEAD").unwrap();
    }
}
