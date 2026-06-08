use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use crate::core::component::{self, Component};
use crate::core::error::{Error, ErrorCode, Result, ValidationErrorItem};
use crate::core::extension::{build, ExtensionCapability};
use crate::extensions::deps_provider;

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
    fn missing_required_git_metadata(&self) -> bool {
        self.role == "validation_dependency"
            && (self.head.is_none() || self.dirty.is_none() || self.upstream.is_none())
    }

    fn is_stale_or_dirty(&self) -> bool {
        self.missing_required_git_metadata()
            || self.dirty == Some(true)
            || self.behind.unwrap_or(0) > 0
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
    require_dependency_hygiene_for_source_with_settings(source_path, extension_path, &[], options)
}

pub fn require_dependency_hygiene_for_source_with_settings(
    source_path: &Path,
    extension_path: Option<&Path>,
    settings: &[(String, serde_json::Value)],
    options: DependencyHygieneOptions,
) -> Result<Vec<CheckoutHygieneSnapshot>> {
    let mut checkouts = dependency_checkouts_for_source(source_path)?;
    checkouts.extend(dependency_checkouts_for_settings(source_path, settings)?);
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
    require_checkout_hygiene_inner(checkouts, options, true)
}

pub(crate) fn require_checkout_hygiene_without_lifecycle(
    checkouts: Vec<DependencyCheckout>,
    options: DependencyHygieneOptions,
) -> Result<Vec<CheckoutHygieneSnapshot>> {
    require_checkout_hygiene_inner(checkouts, options, false)
}

fn require_checkout_hygiene_inner(
    checkouts: Vec<DependencyCheckout>,
    options: DependencyHygieneOptions,
    run_lifecycle: bool,
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
        if !run_lifecycle {
            return Ok(snapshots);
        }
        for snapshot in snapshots
            .iter()
            .filter(|snapshot| snapshot.role == "validation_dependency")
        {
            run_validation_dependency_snapshot_lifecycle(snapshot)?;
        }
        let lifecycle_failures = snapshots
            .iter()
            .filter(|snapshot| snapshot.role == "validation_dependency")
            .map(|snapshot| {
                checkout_hygiene_snapshot(
                    DependencyCheckout {
                        id: snapshot.id.clone(),
                        role: snapshot.role.clone(),
                        path: PathBuf::from(&snapshot.path),
                    },
                    snapshot.allowed,
                )
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .filter(|snapshot| !snapshot.allowed && snapshot.is_stale_or_dirty())
            .map(|snapshot| ValidationErrorItem {
                field: "dependency_hygiene".to_string(),
                problem: hygiene_failure_message(&snapshot),
                context: Some(serde_json::to_value(snapshot).unwrap_or(serde_json::Value::Null)),
            })
            .collect::<Vec<_>>();
        if !lifecycle_failures.is_empty() {
            return Err(Error::new(
                ErrorCode::ValidationMultipleErrors,
                "Dependency hygiene preflight failed after lifecycle preparation",
                serde_json::json!({
                    "errors": lifecycle_failures,
                    "checkouts": snapshots,
                }),
            )
            .with_hint("Dependency lifecycle preparation must leave validation dependencies clean and current before expensive evidence workflows."));
        }
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
    .with_hint("Update the stale dependency checkout. Homeboy automatically fast-forwards clean validation dependencies, but dirty, non-Git, missing-upstream, or divergent checkouts must be fixed before running expensive evidence workflows."))
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

fn dependency_checkouts_for_settings(
    source_path: &Path,
    settings: &[(String, serde_json::Value)],
) -> Result<Vec<DependencyCheckout>> {
    let ids = settings
        .iter()
        .filter(|(key, _)| key == "validation_dependencies")
        .flat_map(|(_, value)| validation_dependency_ids_from_value(value))
        .collect::<Vec<_>>();

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

fn validation_dependency_ids_from_value(value: &serde_json::Value) -> Vec<String> {
    let Some(dependencies) = value.as_array() else {
        return Vec::new();
    };

    dependencies
        .iter()
        .filter_map(|item| item.as_str())
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
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
    let mut head = git_output(&path, &["rev-parse", "HEAD"]);
    let branch = git_output(&path, &["rev-parse", "--abbrev-ref", "HEAD"]);
    let upstream = git_output(&path, &["rev-parse", "--abbrev-ref", "@{upstream}"]);
    if upstream.is_some() {
        let _ = git_output(&path, &["fetch", "--quiet"]);
    }
    let dirty = git_output(&path, &["status", "--porcelain=v1"]).map(|value| !value.is_empty());
    let (mut behind, mut ahead) = git_ahead_behind(&path);
    if checkout.role == "validation_dependency"
        && !allowed
        && upstream.is_some()
        && dirty == Some(false)
        && ahead.unwrap_or(0) == 0
        && behind.unwrap_or(0) > 0
        && git_status(&path, &["merge", "--ff-only", "@{upstream}"])
    {
        head = git_output(&path, &["rev-parse", "HEAD"]);
        (behind, ahead) = git_ahead_behind(&path);
    }
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

fn git_ahead_behind(path: &Path) -> (Option<u32>, Option<u32>) {
    git_output(
        &path,
        &["rev-list", "--left-right", "--count", "@{upstream}...HEAD"],
    )
    .and_then(|value| {
        let mut parts = value.split_whitespace();
        let behind = parts.next()?.parse::<u32>().ok()?;
        let ahead = parts.next()?.parse::<u32>().ok()?;
        Some((Some(behind), Some(ahead)))
    })
    .unwrap_or((None, None))
}

fn hygiene_failure_message(snapshot: &CheckoutHygieneSnapshot) -> String {
    let mut problems = Vec::new();
    if snapshot.missing_required_git_metadata() {
        problems.push("missing git metadata".to_string());
    }
    if snapshot.role == "validation_dependency" && snapshot.upstream.is_none() {
        problems.push("missing upstream".to_string());
    }
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

fn git_status(path: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn run_validation_dependency_snapshot_lifecycle(snapshot: &CheckoutHygieneSnapshot) -> Result<()> {
    let path = Path::new(&snapshot.path);
    let mut component =
        component::resolve_effective(Some(&snapshot.id), Some(&snapshot.path), None)?;
    component.local_path = snapshot.path.clone();

    run_validation_dependency_lifecycle_isolated(&component, path)
}

pub(crate) fn run_validation_dependency_lifecycle(
    component: &Component,
    path: &Path,
) -> Result<()> {
    run_dependency_install_lifecycle(component, path)?;
    run_dependency_build_lifecycle(component, path)
}

fn run_validation_dependency_lifecycle_isolated(component: &Component, path: &Path) -> Result<()> {
    let tempdir = tempfile::tempdir().map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!(
                "create validation dependency lifecycle workspace {}",
                component.id
            )),
        )
    })?;
    let prepared_path = tempdir.path().join("dependency");
    let excludes = [".git", ".git/**", "node_modules", "node_modules/**"]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    crate::core::runner::copy_snapshot_to_directory(path, &prepared_path, &excludes)?;

    let mut prepared = component.clone();
    prepared.local_path = prepared_path.display().to_string();

    run_validation_dependency_lifecycle(&prepared, &prepared_path)
}

fn run_dependency_install_lifecycle(component: &Component, path: &Path) -> Result<()> {
    let providers = match deps_provider::resolve_dependency_providers(component, path) {
        Ok(providers) => providers,
        Err(_) => return Ok(()),
    };

    for provider in providers {
        provider.install(component, path)?;
    }
    Ok(())
}

fn run_dependency_build_lifecycle(component: &Component, path: &Path) -> Result<()> {
    if !component.has_script(ExtensionCapability::Build)
        && build::resolve_build_command(component).is_err()
    {
        return Ok(());
    }

    let (result, exit_code) = build::run_component(component)?;
    if exit_code == 0 {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "validation_dependencies",
        format!(
            "Validation dependency `{}` build lifecycle failed with status {exit_code}",
            component.id
        ),
        Some(path.display().to_string()),
        Some(vec![
            format!(
                "Run manually: homeboy build {} --path {}",
                component.id,
                path.display()
            ),
            serde_json::to_string(&result).unwrap_or_default(),
        ]),
    ))
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

    fn init_repo_with_upstream(path: &Path) -> tempfile::TempDir {
        let remote = tempfile::tempdir().unwrap();
        init_repo(path);
        git(remote.path(), &["init", "--bare", "-b", "main"]);
        git(
            path,
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        );
        git(path, &["push", "-u", "origin", "main"]);
        remote
    }

    #[test]
    fn dependency_hygiene_fast_forwards_validation_dependency_behind_upstream() {
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

        let snapshots = require_checkout_hygiene(
            vec![DependencyCheckout {
                id: "dep".to_string(),
                role: "validation_dependency".to_string(),
                path: local.path().to_path_buf(),
            }],
            DependencyHygieneOptions { allow_stale: false },
        )
        .expect("clean validation dependency should fast-forward");

        assert_eq!(snapshots[0].behind, Some(0));
        assert!(local.path().join("remote.txt").exists());
    }

    #[test]
    fn dependency_hygiene_fails_when_trace_dependency_is_behind_upstream() {
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
                role: "trace_dependency".to_string(),
                path: local.path().to_path_buf(),
            }],
            DependencyHygieneOptions { allow_stale: false },
        )
        .expect_err("behind trace dependency should fail");

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
    fn dependency_hygiene_fails_when_git_metadata_is_missing() {
        let dependency = tempfile::tempdir().unwrap();
        fs::write(dependency.path().join("README.md"), "snapshot\n").unwrap();

        let err = require_checkout_hygiene(
            vec![DependencyCheckout {
                id: "snapshot-dep".to_string(),
                role: "validation_dependency".to_string(),
                path: dependency.path().to_path_buf(),
            }],
            DependencyHygieneOptions { allow_stale: false },
        )
        .expect_err("dependency without git metadata should fail");

        assert_eq!(err.code, ErrorCode::ValidationMultipleErrors);
        assert!(err.details["errors"][0]["problem"]
            .as_str()
            .unwrap()
            .contains("missing git metadata"));
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

    #[test]
    fn dependency_hygiene_reads_validation_dependencies_from_runtime_settings() {
        let source = tempfile::tempdir().unwrap();
        let dependency = tempfile::tempdir().unwrap();
        init_repo(dependency.path());
        fs::write(dependency.path().join("dirty.txt"), "dirty\n").unwrap();

        let settings = vec![(
            "validation_dependencies".to_string(),
            serde_json::json!([dependency.path().to_str().unwrap()]),
        )];
        let err = require_dependency_hygiene_for_source_with_settings(
            source.path(),
            None,
            &settings,
            DependencyHygieneOptions { allow_stale: false },
        )
        .expect_err("dirty dependency from settings should fail");

        assert_eq!(err.code, ErrorCode::ValidationMultipleErrors);
        assert_eq!(err.details["checkouts"][0]["dirty"].as_bool(), Some(true));
    }

    #[test]
    fn dependency_hygiene_runs_validation_dependency_lifecycle() {
        crate::test_support::with_isolated_home(|_| {
            let workspace_parent = tempfile::tempdir().unwrap();
            let source = workspace_parent.path().join("source");
            let dependency = workspace_parent.path().join("dep");
            fs::create_dir_all(&source).unwrap();
            fs::create_dir_all(&dependency).unwrap();
            fs::write(
                source.join(PORTABLE_CONFIG_FILE),
                serde_json::json!({
                    "id": "source",
                    "extensions": {
                        "example": {
                            "settings": { "validation_dependencies": ["dep"] }
                        }
                    }
                })
                .to_string(),
            )
            .unwrap();
            fs::write(
                dependency.join(PORTABLE_CONFIG_FILE),
                serde_json::json!({
                    "id": "dep",
                    "scripts": {
                        "deps": ["sh -c 'printf install > deps-installed.txt'"],
                        "build": ["sh -c 'printf build > build-built.txt'"]
                    }
                })
                .to_string(),
            )
            .unwrap();
            fs::write(
                dependency.join(".gitignore"),
                "deps-installed.txt\nbuild-built.txt\n",
            )
            .unwrap();
            let _remote = init_repo_with_upstream(&dependency);

            require_dependency_hygiene_for_source(
                &source,
                None,
                DependencyHygieneOptions { allow_stale: false },
            )
            .expect("validation dependency lifecycle should run after hygiene passes");

            assert!(!dependency.join("deps-installed.txt").exists());
            assert!(!dependency.join("build-built.txt").exists());
            let status =
                git_output(&dependency, &["status", "--porcelain=v1"]).expect("dependency status");
            assert_eq!(status, "");
        });
    }
}
