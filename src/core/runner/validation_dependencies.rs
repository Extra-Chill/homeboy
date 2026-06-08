use std::fs;
use std::path::{Path, PathBuf};

use crate::core::component::{self, Component};
use crate::core::error::{Error, Result};
use crate::core::{git::clone_repo, paths};

use super::workspace::{materialize_snapshot, parent_remote_path, sanitize_path_segment};
use super::Runner;

const PORTABLE_CONFIG_FILE: &str = concat!("homeboy", ".json");

pub(super) fn sync_validation_dependency_workspaces(
    runner: &Runner,
    local_path: &Path,
    remote_path: &str,
    excludes: &[String],
) -> Result<()> {
    for dependency in validation_dependency_workspaces(local_path, excludes)? {
        let remote_dependency_path = format!(
            "{}/{}",
            parent_remote_path(remote_path),
            sanitize_path_segment(&dependency.remote_name)
        );
        materialize_snapshot(
            runner,
            &dependency.prepared_path,
            &remote_dependency_path,
            excludes,
        )?;
    }
    Ok(())
}

#[derive(Debug)]
struct PreparedValidationDependencyWorkspace {
    remote_name: String,
    prepared_path: PathBuf,
    _tempdir: tempfile::TempDir,
}

fn validation_dependency_workspaces(
    local_path: &Path,
    excludes: &[String],
) -> Result<Vec<PreparedValidationDependencyWorkspace>> {
    let dependency_ids = validation_dependency_ids(local_path)?;
    if dependency_ids.is_empty() {
        return Ok(Vec::new());
    }

    let Some(parent) = local_path.parent() else {
        return Ok(Vec::new());
    };

    dependency_ids
        .into_iter()
        .map(|dependency_id| {
            prepare_validation_dependency_workspace(parent, &dependency_id, excludes)
        })
        .collect()
}

pub(crate) fn validation_dependency_ids(local_path: &Path) -> Result<Vec<String>> {
    let manifest_path = local_path.join(PORTABLE_CONFIG_FILE);
    let Ok(content) = fs::read_to_string(&manifest_path) else {
        return Ok(Vec::new());
    };
    let manifest: serde_json::Value = serde_json::from_str(&content).map_err(|err| {
        Error::validation_invalid_argument(
            PORTABLE_CONFIG_FILE,
            format!("failed to parse {}: {err}", manifest_path.display()),
            None,
            None,
        )
    })?;

    let mut ids = Vec::new();
    let Some(extensions) = manifest
        .get("extensions")
        .and_then(|value| value.as_object())
    else {
        return Ok(ids);
    };

    for extension in extensions.values() {
        collect_validation_dependency_ids(extension, &mut ids);
        if let Some(settings) = extension.get("settings") {
            collect_validation_dependency_ids(settings, &mut ids);
        }
    }

    ids.sort();
    ids.dedup();
    Ok(ids)
}

fn collect_validation_dependency_ids(value: &serde_json::Value, ids: &mut Vec<String>) {
    let Some(dependencies) = value
        .get("validation_dependencies")
        .and_then(|value| value.as_array())
    else {
        return;
    };

    ids.extend(
        dependencies
            .iter()
            .filter_map(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
    );
}

fn resolve_sibling_dependency_workspace(parent: &Path, dependency_id: &str) -> Result<PathBuf> {
    let exact = parent.join(dependency_id);
    if is_homeboy_component_id(&exact, dependency_id) {
        return exact.canonicalize().map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some(format!(
                    "canonicalize validation dependency {dependency_id}"
                )),
            )
        });
    }

    let mut matches = fs::read_dir(parent)
        .map_err(|err| Error::internal_io(err.to_string(), Some("read workspace parent".into())))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| is_homeboy_component_id(path, dependency_id))
        .collect::<Vec<_>>();
    matches.sort();

    if let Some(path) = matches.into_iter().next() {
        return path.canonicalize().map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some(format!(
                    "canonicalize validation dependency {dependency_id}"
                )),
            )
        });
    }

    Err(Error::validation_invalid_argument(
        "validation_dependencies",
        format!(
            "Runner workspace sync could not find local sibling checkout for validation dependency `{dependency_id}`"
        ),
        Some(parent.display().to_string()),
        Some(vec![format!(
            "Clone or attach `{dependency_id}` next to the source checkout before runner dispatch."
        )]),
    ))
}

fn prepare_validation_dependency_workspace(
    parent: &Path,
    dependency_id: &str,
    excludes: &[String],
) -> Result<PreparedValidationDependencyWorkspace> {
    let (mut component, path) = resolve_managed_dependency_workspace(parent, dependency_id)?;
    component.local_path = path.display().to_string();

    prepare_dependency_git_state(&component, &path)?;

    let tempdir = tempfile::tempdir().map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!(
                "create validation dependency workspace {}",
                component.id
            )),
        )
    })?;
    let prepared_path = tempdir.path().join(sanitize_path_segment(&component.id));
    crate::core::runner::copy_snapshot_to_directory(&path, &prepared_path, excludes)?;

    component.local_path = prepared_path.display().to_string();
    run_dependency_lifecycle(&component, &prepared_path)?;

    let prepared_path = prepared_path.canonicalize().map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!(
                "canonicalize validation dependency {dependency_id}"
            )),
        )
    })?;

    Ok(PreparedValidationDependencyWorkspace {
        remote_name: component.id,
        prepared_path,
        _tempdir: tempdir,
    })
}

fn resolve_managed_dependency_workspace(
    parent: &Path,
    dependency_id: &str,
) -> Result<(Component, PathBuf)> {
    if let Ok(path) = resolve_sibling_dependency_workspace(parent, dependency_id) {
        let mut component =
            component::resolve_effective(None, Some(&path.display().to_string()), None)?;
        component.local_path = path.display().to_string();
        return Ok((component, path));
    }

    if let Ok(component) = component::resolve_effective(Some(dependency_id), None, None) {
        let path = PathBuf::from(shellexpand::tilde(&component.local_path).as_ref());
        if path.is_dir() {
            return Ok((
                component,
                canonical_existing_dependency_dir(&path, dependency_id)?,
            ));
        }
        return clone_missing_dependency(component, dependency_id);
    }

    if let Some(component) = read_standalone_dependency_config(dependency_id)? {
        let path = PathBuf::from(shellexpand::tilde(&component.local_path).as_ref());
        if path.is_dir() {
            return Ok((
                component,
                canonical_existing_dependency_dir(&path, dependency_id)?,
            ));
        }
        return clone_missing_dependency(component, dependency_id);
    }

    Err(Error::validation_invalid_argument(
        "validation_dependencies",
        format!(
            "Runner workspace sync could not resolve validation dependency `{dependency_id}` as a sibling checkout or registered component"
        ),
        Some(parent.display().to_string()),
        Some(vec![format!(
            "Register `{dependency_id}` as a Homeboy component or place its checkout next to the source checkout before runner dispatch."
        )]),
    ))
}

fn clone_missing_dependency(
    component: Component,
    dependency_id: &str,
) -> Result<(Component, PathBuf)> {
    let path = PathBuf::from(shellexpand::tilde(&component.local_path).as_ref());
    if path.as_os_str().is_empty() {
        return Err(unresolvable_dependency_error(
            dependency_id,
            "has no local_path to clone into",
        ));
    }
    if path.exists() && !path.is_dir() {
        return Err(unresolvable_dependency_error(
            dependency_id,
            &format!(
                "local_path exists but is not a directory: {}",
                path.display()
            ),
        ));
    }
    let Some(remote_url) = component
        .remote_url
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    else {
        return Err(unresolvable_dependency_error(
            dependency_id,
            "is missing locally and has no remote_url for deterministic clone",
        ));
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some(format!(
                    "create validation dependency parent {}",
                    parent.display()
                )),
            )
        })?;
    }

    clone_repo(remote_url, &path)?;
    Ok((
        component,
        canonical_existing_dependency_dir(&path, dependency_id)?,
    ))
}

fn canonical_existing_dependency_dir(path: &Path, dependency_id: &str) -> Result<PathBuf> {
    if !path.is_dir() {
        return Err(unresolvable_dependency_error(
            dependency_id,
            &format!("path is not a directory: {}", path.display()),
        ));
    }
    path.canonicalize().map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!(
                "canonicalize validation dependency {dependency_id}"
            )),
        )
    })
}

fn read_standalone_dependency_config(dependency_id: &str) -> Result<Option<Component>> {
    let path = paths::components()?.join(format!("{dependency_id}.json"));
    if !path.is_file() {
        return Ok(None);
    }
    let content = fs::read_to_string(&path)
        .map_err(|err| Error::internal_io(err.to_string(), Some(path.display().to_string())))?;
    let mut value: serde_json::Value = serde_json::from_str(&content).map_err(|err| {
        Error::validation_invalid_argument(
            "validation_dependencies",
            format!(
                "failed to parse registered component {}: {err}",
                path.display()
            ),
            Some(dependency_id.to_string()),
            None,
        )
    })?;
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "id".to_string(),
            serde_json::Value::String(dependency_id.to_string()),
        );
    }
    serde_json::from_value(value).map(Some).map_err(|err| {
        Error::validation_invalid_argument(
            "validation_dependencies",
            format!("failed to load registered component {dependency_id}: {err}"),
            Some(path.display().to_string()),
            None,
        )
    })
}

fn prepare_dependency_git_state(component: &Component, path: &Path) -> Result<()> {
    crate::core::hygiene::require_checkout_hygiene_without_lifecycle(
        vec![crate::core::hygiene::DependencyCheckout {
            id: component.id.clone(),
            role: "validation_dependency".to_string(),
            path: path.to_path_buf(),
        }],
        crate::core::hygiene::DependencyHygieneOptions { allow_stale: false },
    )?;
    Ok(())
}

fn run_dependency_lifecycle(component: &Component, path: &Path) -> Result<()> {
    crate::core::hygiene::run_validation_dependency_lifecycle(component, path)
}

fn unresolvable_dependency_error(dependency_id: &str, reason: &str) -> Error {
    Error::validation_invalid_argument(
        "validation_dependencies",
        format!("Validation dependency `{dependency_id}` {reason}"),
        Some(dependency_id.to_string()),
        Some(vec![
            "Homeboy can only repair missing validation dependencies when the component has a deterministic remote_url and local_path.".to_string(),
            "Dirty, divergent, non-Git, missing-upstream, or unresolvable dependency states block Lab evidence runs.".to_string(),
        ]),
    )
}

fn is_homeboy_component_id(path: &Path, dependency_id: &str) -> bool {
    if !path.is_dir() {
        return false;
    }
    let Ok(content) = fs::read_to_string(path.join(PORTABLE_CONFIG_FILE)) else {
        return false;
    };
    let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&content) else {
        return false;
    };
    manifest
        .get("id")
        .and_then(|value| value.as_str())
        .is_some_and(|id| id == dependency_id)
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;

    use super::*;
    use crate::core::runner::workspace::{
        parent_remote_path, sync_workspace, RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions,
    };

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

    fn init_checkout_with_upstream(path: &Path) -> tempfile::TempDir {
        let remote = tempfile::tempdir().expect("remote");
        git(path, &["init", "-b", "main"]);
        git(path, &["config", "user.email", "test@example.com"]);
        git(path, &["config", "user.name", "Homeboy Test"]);
        git(remote.path(), &["init", "--bare", "-b", "main"]);
        git(
            path,
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        );
        git(path, &["add", "."]);
        git(path, &["commit", "-m", "initial"]);
        git(path, &["push", "-u", "origin", "main"]);
        remote
    }

    #[test]
    fn sync_workspace_materializes_validation_dependency_siblings() {
        crate::test_support::with_isolated_home(|_| {
            let workspace_parent = tempfile::tempdir().expect("workspace parent");
            let source = workspace_parent.path().join("studio-web");
            let dependency = workspace_parent.path().join("agents-api");
            let runner_root = tempfile::tempdir().expect("runner root tempdir");
            fs::create_dir_all(source.join("src")).expect("source dir");
            fs::create_dir_all(dependency.join("lib")).expect("dependency dir");
            fs::write(
                source.join("homeboy.json"),
                serde_json::json!({
                    "id": "studio-web",
                    "extensions": {
                        "wordpress": {
                            "settings": {
                                "validation_dependencies": ["agents-api"]
                            }
                        }
                    }
                })
                .to_string(),
            )
            .expect("source manifest");
            fs::write(source.join("src/main.php"), "<?php\n").expect("source file");
            fs::write(
                dependency.join("homeboy.json"),
                serde_json::json!({ "id": "agents-api" }).to_string(),
            )
            .expect("dependency manifest");
            fs::write(dependency.join("lib/agents.php"), "<?php\n").expect("dependency file");
            let _remote = init_checkout_with_upstream(&dependency);

            super::super::create(
                &format!(
                    r#"{{"id":"lab-local","kind":"local","workspace_root":"{}"}}"#,
                    runner_root.path().display()
                ),
                false,
            )
            .expect("create runner");

            let (output, exit_code) = sync_workspace(
                "lab-local",
                RunnerWorkspaceSyncOptions {
                    path: source.display().to_string(),
                    mode: RunnerWorkspaceSyncMode::Snapshot,
                    changed_since_base: None,
                },
            )
            .expect("sync workspace");

            assert_eq!(exit_code, 0);
            let remote_parent = parent_remote_path(&output.remote_path);
            assert!(Path::new(&output.remote_path).join("src/main.php").exists());
            assert!(Path::new(&remote_parent)
                .join("agents-api/lib/agents.php")
                .exists());
        });
    }

    #[test]
    fn sync_workspace_runs_validation_dependency_lifecycle_before_materializing() {
        crate::test_support::with_isolated_home(|_| {
            let workspace_parent = tempfile::tempdir().expect("workspace parent");
            let source = workspace_parent.path().join("studio-web");
            let dependency = workspace_parent.path().join("agents-api");
            let runner_root = tempfile::tempdir().expect("runner root tempdir");
            fs::create_dir_all(&source).expect("source dir");
            fs::create_dir_all(&dependency).expect("dependency dir");
            fs::write(
                source.join("homeboy.json"),
                serde_json::json!({
                    "id": "studio-web",
                    "extensions": {
                        "wordpress": {
                            "settings": {
                                "validation_dependencies": ["agents-api"]
                            }
                        }
                    }
                })
                .to_string(),
            )
            .expect("source manifest");
            fs::write(
                dependency.join("homeboy.json"),
                serde_json::json!({
                    "id": "agents-api",
                    "scripts": {
                        "deps": ["sh -c 'printf install > deps-installed.txt'"],
                        "build": ["sh -c 'printf build > build-built.txt'"]
                    }
                })
                .to_string(),
            )
            .expect("dependency manifest");
            fs::write(
                dependency.join(".gitignore"),
                "deps-installed.txt\nbuild-built.txt\n",
            )
            .expect("dependency gitignore");
            let _remote = init_checkout_with_upstream(&dependency);

            super::super::create(
                &format!(
                    r#"{{"id":"lab-local-lifecycle","kind":"local","workspace_root":"{}"}}"#,
                    runner_root.path().display()
                ),
                false,
            )
            .expect("create runner");

            let (output, exit_code) = sync_workspace(
                "lab-local-lifecycle",
                RunnerWorkspaceSyncOptions {
                    path: source.display().to_string(),
                    mode: RunnerWorkspaceSyncMode::Snapshot,
                    changed_since_base: None,
                },
            )
            .expect("sync workspace");

            assert_eq!(exit_code, 0);
            let remote_parent = parent_remote_path(&output.remote_path);
            assert!(Path::new(&remote_parent)
                .join("agents-api/deps-installed.txt")
                .exists());
            assert!(Path::new(&remote_parent)
                .join("agents-api/build-built.txt")
                .exists());
            assert!(!dependency.join("deps-installed.txt").exists());
            assert!(!dependency.join("build-built.txt").exists());
        });
    }

    #[test]
    fn sync_workspace_uses_manifest_id_for_absolute_validation_dependency() {
        crate::test_support::with_isolated_home(|_| {
            let workspace_parent = tempfile::tempdir().expect("workspace parent");
            let source = workspace_parent.path().join("studio-web");
            let dependency = workspace_parent.path().join("agents-api");
            let runner_root = tempfile::tempdir().expect("runner root tempdir");
            fs::create_dir_all(&source).expect("source dir");
            fs::create_dir_all(&dependency).expect("dependency dir");
            fs::write(
                source.join("homeboy.json"),
                serde_json::json!({
                    "id": "studio-web",
                    "extensions": {
                        "wordpress": {
                            "settings": {
                                "validation_dependencies": [dependency.display().to_string()]
                            }
                        }
                    }
                })
                .to_string(),
            )
            .expect("source manifest");
            fs::write(
                dependency.join("homeboy.json"),
                serde_json::json!({
                    "id": "agents-api",
                    "scripts": {
                        "build": ["sh -c 'printf \"$HOMEBOY_COMPONENT_ID\" > component-id.txt'"]
                    }
                })
                .to_string(),
            )
            .expect("dependency manifest");
            fs::write(dependency.join(".gitignore"), "component-id.txt\n")
                .expect("dependency gitignore");
            let _remote = init_checkout_with_upstream(&dependency);

            super::super::create(
                &format!(
                    r#"{{"id":"lab-local-absolute-dependency","kind":"local","workspace_root":"{}"}}"#,
                    runner_root.path().display()
                ),
                false,
            )
            .expect("create runner");

            let (output, exit_code) = sync_workspace(
                "lab-local-absolute-dependency",
                RunnerWorkspaceSyncOptions {
                    path: source.display().to_string(),
                    mode: RunnerWorkspaceSyncMode::Snapshot,
                    changed_since_base: None,
                },
            )
            .expect("sync workspace");

            assert_eq!(exit_code, 0);
            let remote_parent = parent_remote_path(&output.remote_path);
            let remote_dependency = Path::new(&remote_parent).join("agents-api");
            assert!(remote_dependency.join("component-id.txt").exists());
            assert_eq!(
                fs::read_to_string(remote_dependency.join("component-id.txt")).unwrap(),
                "agents-api"
            );
            assert!(!Path::new(&remote_parent).join("Users").exists());
        });
    }

    #[test]
    fn sync_workspace_failed_validation_dependency_build_keeps_source_clean() {
        crate::test_support::with_isolated_home(|_| {
            let workspace_parent = tempfile::tempdir().expect("workspace parent");
            let source = workspace_parent.path().join("studio-web");
            let dependency = workspace_parent.path().join("agents-api");
            let runner_root = tempfile::tempdir().expect("runner root tempdir");
            fs::create_dir_all(&source).expect("source dir");
            fs::create_dir_all(&dependency).expect("dependency dir");
            fs::write(
                source.join("homeboy.json"),
                serde_json::json!({
                    "id": "studio-web",
                    "extensions": {
                        "wordpress": {
                            "settings": {
                                "validation_dependencies": ["agents-api"]
                            }
                        }
                    }
                })
                .to_string(),
            )
            .expect("source manifest");
            fs::write(
                dependency.join("homeboy.json"),
                serde_json::json!({
                    "id": "agents-api",
                    "scripts": {
                        "build": ["sh -c 'mkdir .homeboy-build && printf dirty > .homeboy-build/state && exit 7'"]
                    }
                })
                .to_string(),
            )
            .expect("dependency manifest");
            let _remote = init_checkout_with_upstream(&dependency);

            super::super::create(
                &format!(
                    r#"{{"id":"lab-local-failed-dependency","kind":"local","workspace_root":"{}"}}"#,
                    runner_root.path().display()
                ),
                false,
            )
            .expect("create runner");

            let err = sync_workspace(
                "lab-local-failed-dependency",
                RunnerWorkspaceSyncOptions {
                    path: source.display().to_string(),
                    mode: RunnerWorkspaceSyncMode::Snapshot,
                    changed_since_base: None,
                },
            )
            .expect_err("failed dependency build should fail sync");

            assert!(err.message.contains("build lifecycle failed"));
            assert!(!dependency.join(".homeboy-build").exists());
            let output = Command::new("git")
                .args(["status", "--porcelain=v1"])
                .current_dir(&dependency)
                .output()
                .expect("git status");
            assert!(output.status.success());
            assert_eq!(String::from_utf8_lossy(&output.stdout), "");
        });
    }

    #[test]
    fn sync_workspace_clones_missing_registered_validation_dependency() {
        crate::test_support::with_isolated_home(|_| {
            let workspace_parent = tempfile::tempdir().expect("workspace parent");
            let remote_parent = tempfile::tempdir().expect("remote parent");
            let source = workspace_parent.path().join("studio-web");
            let seed = remote_parent.path().join("agents-api-seed");
            let clone_target = workspace_parent.path().join("agents-api-clone");
            let runner_root = tempfile::tempdir().expect("runner root tempdir");
            fs::create_dir_all(&source).expect("source dir");
            fs::create_dir_all(seed.join("lib")).expect("seed dir");
            fs::write(
                source.join("homeboy.json"),
                serde_json::json!({
                    "id": "studio-web",
                    "extensions": {
                        "wordpress": {
                            "settings": {
                                "validation_dependencies": ["agents-api"]
                            }
                        }
                    }
                })
                .to_string(),
            )
            .expect("source manifest");
            fs::write(
                seed.join("homeboy.json"),
                serde_json::json!({ "id": "agents-api" }).to_string(),
            )
            .expect("seed manifest");
            fs::write(seed.join("lib/agents.php"), "<?php\n").expect("seed file");
            let remote = init_checkout_with_upstream(&seed);
            let components_dir = crate::core::paths::components().expect("components dir");
            fs::create_dir_all(&components_dir).expect("components dir exists");
            fs::write(
                components_dir.join("agents-api.json"),
                serde_json::json!({
                    "local_path": clone_target,
                    "remote_url": remote.path()
                })
                .to_string(),
            )
            .expect("registered dependency config");

            super::super::create(
                &format!(
                    r#"{{"id":"lab-local-clone","kind":"local","workspace_root":"{}"}}"#,
                    runner_root.path().display()
                ),
                false,
            )
            .expect("create runner");

            let (output, exit_code) = sync_workspace(
                "lab-local-clone",
                RunnerWorkspaceSyncOptions {
                    path: source.display().to_string(),
                    mode: RunnerWorkspaceSyncMode::Snapshot,
                    changed_since_base: None,
                },
            )
            .expect("sync workspace");

            assert_eq!(exit_code, 0);
            assert!(clone_target.join("lib/agents.php").exists());
            let remote_parent = parent_remote_path(&output.remote_path);
            assert!(Path::new(&remote_parent)
                .join("agents-api/lib/agents.php")
                .exists());
        });
    }

    #[test]
    fn validation_dependency_workspace_errors_when_sibling_missing() {
        let workspace_parent = tempfile::tempdir().expect("workspace parent");
        let source = workspace_parent.path().join("studio-web");
        fs::create_dir_all(&source).expect("source dir");
        fs::write(
            source.join("homeboy.json"),
            serde_json::json!({
                "id": "studio-web",
                "extensions": {
                    "wordpress": {
                        "settings": {
                            "validation_dependencies": ["agents-api"]
                        }
                    }
                }
            })
            .to_string(),
        )
        .expect("source manifest");

        let err = validation_dependency_workspaces(&source, &[]).expect_err("missing dependency");

        assert_eq!(err.details["field"], "validation_dependencies");
        assert!(err.message.contains("agents-api"));
    }
}
