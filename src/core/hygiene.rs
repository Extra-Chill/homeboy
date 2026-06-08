use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use crate::core::component;
use crate::core::error::{Error, ErrorCode, Result, ValidationErrorItem};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CheckoutHygieneSnapshot {
    pub id: String,
    pub role: String,
    pub path: String,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: Option<u32>,
    pub behind: Option<u32>,
    pub dirty: Option<bool>,
    pub allowed: bool,
}

impl CheckoutHygieneSnapshot {
    fn is_stale_or_dirty(&self) -> bool {
        self.dirty == Some(true) || self.behind.unwrap_or(0) > 0
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DependencyHygieneOptions {
    pub allow_stale: bool,
}

pub fn require_dependency_hygiene_for_source(
    source_path: &Path,
    extension_path: Option<&Path>,
    options: DependencyHygieneOptions,
) -> Result<Vec<CheckoutHygieneSnapshot>> {
    let mut checkouts = dependency_checkouts_for_source(source_path)?;
    if let Some(extension_path) = extension_path {
        checkouts.push(DependencyCheckout {
            id: "extension".to_string(),
            role: "extension".to_string(),
            path: extension_path.to_path_buf(),
        });
    }

    require_checkout_hygiene(checkouts, options)
}

pub fn require_checkout_hygiene(
    checkouts: Vec<DependencyCheckout>,
    options: DependencyHygieneOptions,
) -> Result<Vec<CheckoutHygieneSnapshot>> {
    let snapshots = checkouts
        .into_iter()
        .map(|checkout| checkout_hygiene_snapshot(checkout, options.allow_stale))
        .collect::<Result<Vec<_>>>()?;

    let failures = snapshots
        .iter()
        .filter(|snapshot| !snapshot.allowed && snapshot.is_stale_or_dirty())
        .map(|snapshot| ValidationErrorItem {
            field: "dependency_hygiene".to_string(),
            problem: hygiene_failure_message(snapshot),
            context: Some(serde_json::to_value(snapshot).unwrap_or(serde_json::Value::Null)),
        })
        .collect::<Vec<_>>();

    if failures.is_empty() {
        return Ok(snapshots);
    }

    Err(Error::new(
        ErrorCode::ValidationMultipleErrors,
        "Dependency hygiene preflight failed",
        serde_json::json!({
            "errors": failures,
            "checkouts": snapshots,
        }),
    )
    .with_hint("Update the stale dependency checkout or rerun with --allow-stale-dependencies to accept non-deterministic evidence."))
}

#[derive(Debug, Clone)]
pub struct DependencyCheckout {
    pub id: String,
    pub role: String,
    pub path: PathBuf,
}

fn dependency_checkouts_for_source(source_path: &Path) -> Result<Vec<DependencyCheckout>> {
    let ids = validation_dependency_ids(source_path)?;
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    ids.into_iter()
        .map(|id| {
            let path = resolve_validation_dependency_path(source_path, &id)?;
            Ok(DependencyCheckout {
                id,
                role: "validation_dependency".to_string(),
                path,
            })
        })
        .collect()
}

fn validation_dependency_ids(source_path: &Path) -> Result<Vec<String>> {
    crate::core::runner::validation_dependency_ids(source_path)
}

fn resolve_validation_dependency_path(source_path: &Path, dependency: &str) -> Result<PathBuf> {
    let expanded = shellexpand::tilde(dependency).to_string();
    let explicit = Path::new(&expanded);
    if explicit.is_dir() {
        return canonical_existing_dir(explicit, dependency);
    }

    if let Some(parent) = source_path.parent() {
        let sibling = parent.join(dependency);
        if sibling.is_dir() {
            return canonical_existing_dir(&sibling, dependency);
        }
    }

    let component = component::resolve_effective(Some(dependency), None, None).map_err(|err| {
        Error::validation_invalid_argument(
            "validation_dependencies",
            format!(
                "Cannot resolve validation dependency `{dependency}` to a local checkout: {}",
                err.message
            ),
            Some(dependency.to_string()),
            Some(vec![format!(
                "Clone/register `{dependency}` locally before running expensive evidence workflows."
            )]),
        )
    })?;
    canonical_existing_dir(Path::new(&component.local_path), dependency)
}

fn canonical_existing_dir(path: &Path, dependency: &str) -> Result<PathBuf> {
    if !path.is_dir() {
        return Err(Error::validation_invalid_argument(
            "validation_dependencies",
            format!(
                "Validation dependency `{dependency}` path is not a directory: {}",
                path.display()
            ),
            Some(path.display().to_string()),
            None,
        ));
    }
    path.canonicalize().map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!("canonicalize validation dependency {dependency}")),
        )
    })
}

fn checkout_hygiene_snapshot(
    checkout: DependencyCheckout,
    allowed: bool,
) -> Result<CheckoutHygieneSnapshot> {
    let path = checkout.path;
    let head = git_output(&path, &["rev-parse", "HEAD"]);
    let branch = git_output(&path, &["rev-parse", "--abbrev-ref", "HEAD"]);
    let upstream = git_output(&path, &["rev-parse", "--abbrev-ref", "@{upstream}"]);
    if upstream.is_some() {
        let _ = git_output(&path, &["fetch", "--quiet"]);
    }
    let (behind, ahead) = git_output(
        &path,
        &["rev-list", "--left-right", "--count", "@{upstream}...HEAD"],
    )
    .and_then(|value| {
        let mut parts = value.split_whitespace();
        let behind = parts.next()?.parse::<u32>().ok()?;
        let ahead = parts.next()?.parse::<u32>().ok()?;
        Some((Some(behind), Some(ahead)))
    })
    .unwrap_or((None, None));
    let dirty = git_output(&path, &["status", "--porcelain=v1"]).map(|value| !value.is_empty());

    Ok(CheckoutHygieneSnapshot {
        id: checkout.id,
        role: checkout.role,
        path: path.to_string_lossy().to_string(),
        head,
        branch,
        upstream,
        ahead,
        behind,
        dirty,
        allowed,
    })
}

fn hygiene_failure_message(snapshot: &CheckoutHygieneSnapshot) -> String {
    let mut problems = Vec::new();
    if snapshot.dirty == Some(true) {
        problems.push("dirty".to_string());
    }
    if snapshot.behind.unwrap_or(0) > 0 {
        problems.push(format!(
            "behind upstream by {} commit(s)",
            snapshot.behind.unwrap_or(0)
        ));
    }
    format!(
        "{} `{}` checkout is {}: {}",
        snapshot.role,
        snapshot.id,
        problems.join(" and "),
        snapshot.path
    )
}

fn git_output(path: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const PORTABLE_CONFIG_FILE: &str = concat!("homeboy", ".json");

    fn git(path: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_repo(path: &Path) {
        git(path, &["init", "-b", "main"]);
        git(path, &["config", "user.email", "test@example.com"]);
        git(path, &["config", "user.name", "Homeboy Test"]);
        fs::write(path.join("README.md"), "initial\n").unwrap();
        git(path, &["add", "."]);
        git(path, &["commit", "-m", "initial"]);
    }

    #[test]
    fn dependency_hygiene_fails_when_checkout_is_behind_upstream() {
        let local = tempfile::tempdir().unwrap();
        let remote = tempfile::tempdir().unwrap();
        let writer = tempfile::tempdir().unwrap();
        init_repo(local.path());
        git(remote.path(), &["init", "--bare", "-b", "main"]);
        git(
            local.path(),
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        );
        git(local.path(), &["push", "-u", "origin", "main"]);
        git(
            writer.path(),
            &[
                "clone",
                "--branch",
                "main",
                remote.path().to_str().unwrap(),
                ".",
            ],
        );
        git(writer.path(), &["config", "user.email", "test@example.com"]);
        git(writer.path(), &["config", "user.name", "Homeboy Test"]);
        fs::write(writer.path().join("remote.txt"), "remote\n").unwrap();
        git(writer.path(), &["add", "."]);
        git(writer.path(), &["commit", "-m", "remote update"]);
        git(writer.path(), &["push", "origin", "HEAD:main"]);

        let err = require_checkout_hygiene(
            vec![DependencyCheckout {
                id: "dep".to_string(),
                role: "validation_dependency".to_string(),
                path: local.path().to_path_buf(),
            }],
            DependencyHygieneOptions { allow_stale: false },
        )
        .expect_err("behind checkout should fail");

        assert_eq!(err.code, ErrorCode::ValidationMultipleErrors);
        assert_eq!(err.details["checkouts"][0]["behind"].as_u64(), Some(1));
    }

    #[test]
    fn dependency_hygiene_allows_stale_with_explicit_opt_in() {
        let local = tempfile::tempdir().unwrap();
        init_repo(local.path());
        fs::write(local.path().join("dirty.txt"), "dirty\n").unwrap();

        let snapshots = require_checkout_hygiene(
            vec![DependencyCheckout {
                id: "dep".to_string(),
                role: "validation_dependency".to_string(),
                path: local.path().to_path_buf(),
            }],
            DependencyHygieneOptions { allow_stale: true },
        )
        .expect("explicit opt-in should allow dirty checkout");

        assert!(snapshots[0].allowed);
        assert_eq!(snapshots[0].dirty, Some(true));
    }

    #[test]
    fn dependency_hygiene_fails_when_declared_dependency_is_missing() {
        let source = tempfile::tempdir().unwrap();
        fs::write(
            source.path().join(PORTABLE_CONFIG_FILE),
            serde_json::json!({
                "id": "source",
                "extensions": {
                    "example": {
                        "settings": { "validation_dependencies": ["missing-dep"] }
                    }
                }
            })
            .to_string(),
        )
        .unwrap();

        let err = require_dependency_hygiene_for_source(
            source.path(),
            None,
            DependencyHygieneOptions { allow_stale: false },
        )
        .expect_err("missing dependency should fail");

        assert_eq!(err.details["field"], "validation_dependencies");
        assert!(err.message.contains("missing-dep"));
    }
}
