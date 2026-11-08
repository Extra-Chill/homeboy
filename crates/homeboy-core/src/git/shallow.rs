//! Read-only source preflight, separate from admission's explicit network mutation.
use crate::{Error, Result};
use std::path::Path;
use std::process::Command;

fn git(path: &Path, args: &[&str]) -> Result<String> {
    let command = format!(
        "git -C {} {}",
        crate::engine::shell::quote_arg(&path.display().to_string()),
        args.iter()
            .map(|arg| crate::engine::shell::quote_arg(arg))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .map_err(|error| {
            Error::validation_invalid_argument("source", format!("{command}: {error}"), None, None)
        })?;
    if !output.status.success() {
        return Err(Error::validation_invalid_argument(
            "source",
            format!("{command}: {}", String::from_utf8_lossy(&output.stderr)),
            Some(path.display().to_string()),
            None,
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn is_shallow_repository(path: &Path) -> Result<bool> {
    Ok(git(path, &["rev-parse", "--is-shallow-repository"])? == "true")
}

/// Returns a fetchable remote only when history needs deepening. Never fetches.
pub fn shallow_source_remote(path: &Path) -> Result<Option<String>> {
    if !is_shallow_repository(path)? {
        return Ok(None);
    }
    let remotes = git(path, &["remote"])?;
    let branch = git(path, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok();
    let preferred = branch.and_then(|branch| {
        git(
            path,
            &["config", "--get", &format!("branch.{branch}.remote")],
        )
        .ok()
    });
    let remote = preferred
        .filter(|remote| remote != "." && remotes.lines().any(|name| name == remote))
        .or_else(|| {
            remotes
                .lines()
                .find(|name| *name == "origin")
                .map(str::to_string)
        })
        .or_else(|| remotes.lines().next().map(str::to_string));
    let Some(remote) = remote else {
        return Err(Error::validation_invalid_argument(
            "source",
            "source checkout is shallow and has no fetchable remote; configure a remote before admission",
            Some(path.display().to_string()),
            None,
        ));
    };
    git(path, &["remote", "get-url", &remote])?;
    Ok(Some(remote))
}

/// Admission only: fetch objects without updating branch/tag refs or the worktree.
pub fn deepen_shallow_source(path: &Path) -> Result<()> {
    if let Some(remote) = shallow_source_remote(path)? {
        git(
            path,
            &[
                "fetch",
                "--unshallow",
                "--no-tags",
                "--no-write-fetch-head",
                &remote,
                "HEAD",
            ],
        )?;
        if is_shallow_repository(path)? {
            return Err(Error::validation_invalid_argument(
                "source",
                "source checkout remains shallow after fetch --unshallow",
                Some(path.display().to_string()),
                None,
            ));
        }
    }
    Ok(())
}
