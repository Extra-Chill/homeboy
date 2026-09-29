use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use homeboy_core::component::{self, Component};
use homeboy_core::error::{Error, Result};
use homeboy_core::{git::clone_repo, paths};

use super::workspace::{materialize_snapshot, parent_remote_path, sanitize_path_segment};
use super::Runner;

const PORTABLE_CONFIG_FILE: &str = "homeboy.json";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerValidationDependencySyncOutput {
    pub id: String,
    pub role: String,
    pub local_path: String,
    pub remote_path: String,
    /// How the prepared tree was obtained (`hit`, `miss`, `disabled`,
    /// `uncacheable`), so staging evidence shows whether a dependency build
    /// ran (#15253).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prepare_cache: Option<String>,
}

pub(super) fn sync_validation_dependency_workspaces(
    runner: &Runner,
    local_path: &Path,
    remote_path: &str,
    excludes: &[String],
    selected_dependency_ids: Option<&[String]>,
) -> Result<Vec<RunnerValidationDependencySyncOutput>> {
    let mut synced = Vec::new();
    for dependency in
        validation_dependency_workspaces(local_path, excludes, selected_dependency_ids)?
    {
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
        synced.push(RunnerValidationDependencySyncOutput {
            id: dependency.remote_name,
            role: "validation_dependency".to_string(),
            local_path: dependency.local_path.display().to_string(),
            remote_path: remote_dependency_path,
            prepare_cache: Some(dependency.prepared.outcome.as_str().to_string()),
        });
    }
    Ok(synced)
}

#[derive(Debug)]
struct PreparedValidationDependencyWorkspace {
    remote_name: String,
    local_path: PathBuf,
    prepared_path: PathBuf,
    prepared: crate::validation_dependency_cache::PreparedDependencyCopy,
}

fn validation_dependency_workspaces(
    local_path: &Path,
    excludes: &[String],
    selected_dependency_ids: Option<&[String]>,
) -> Result<Vec<PreparedValidationDependencyWorkspace>> {
    let dependency_ids = match selected_dependency_ids {
        Some(ids) => ids.to_vec(),
        None => homeboy_core::hygiene::validation_dependency_ids(local_path)?,
    };
    if dependency_ids.is_empty() {
        return Ok(Vec::new());
    }

    let Some(parent) = local_path.parent() else {
        return Ok(Vec::new());
    };

    unique_dependency_ids(dependency_ids)
        .into_iter()
        .map(|dependency_id| {
            prepare_validation_dependency_workspace(parent, &dependency_id, excludes)
        })
        .collect()
}

/// Drop repeated dependency ids, keeping first-declared order, so one sync
/// prepares each dependency at most once.
fn unique_dependency_ids(ids: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    ids.into_iter()
        .filter(|id| seen.insert(id.clone()))
        .collect()
}

// validation_dependency_ids (+ its collector) is pure single-machine manifest
// parsing; it now lives in core's hygiene module. Called here via
// homeboy_core::hygiene::validation_dependency_ids.

fn canonicalize_dependency_path(path: &Path, dependency_id: &str) -> Result<PathBuf> {
    path.canonicalize().map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!(
                "canonicalize validation dependency {dependency_id}"
            )),
        )
    })
}

fn resolve_sibling_dependency_workspace(parent: &Path, dependency_id: &str) -> Result<PathBuf> {
    let exact = parent.join(dependency_id);
    if is_homeboy_component_id(&exact, dependency_id) {
        return canonicalize_dependency_path(&exact, dependency_id);
    }

    let mut matches = fs::read_dir(parent)
        .map_err(|err| Error::internal_io(err.to_string(), Some("read workspace parent".into())))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| is_homeboy_component_id(path, dependency_id))
        .collect::<Vec<_>>();
    matches.sort();

    if let Some(path) = matches.into_iter().next() {
        return canonicalize_dependency_path(&path, dependency_id);
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

    // The copy + install + build is content-addressed and shared across calls,
    // transport retries, transitive syncs, and concurrent Cooks (#15253).
    // Every consumer still gets a private copy, because the source evidence
    // below is written into it.
    let prepared = crate::validation_dependency_cache::prepare_with_cache(
        &component,
        &path,
        excludes,
        |prepared_component, prepared_path| {
            run_dependency_lifecycle(prepared_component, prepared_path)
        },
    )?;
    homeboy_core::hygiene::write_validation_dependency_source_evidence(
        &component.id,
        &path,
        &prepared.path,
    )?;

    let prepared_path = canonicalize_dependency_path(&prepared.path, dependency_id)?;

    Ok(PreparedValidationDependencyWorkspace {
        remote_name: component.id,
        local_path: path,
        prepared_path,
        prepared,
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
    canonicalize_dependency_path(path, dependency_id)
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
    homeboy_core::hygiene::require_checkout_hygiene_without_lifecycle(
        vec![homeboy_core::hygiene::DependencyCheckout {
            id: component.id.clone(),
            role: "validation_dependency".to_string(),
            path: path.to_path_buf(),
        }],
        homeboy_core::hygiene::DependencyHygieneOptions { allow_stale: false },
    )?;
    Ok(())
}

fn run_dependency_lifecycle(component: &Component, path: &Path) -> Result<()> {
    homeboy_core::hygiene::run_validation_dependency_lifecycle(component, path)
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
    use crate::workspace::{
        parent_remote_path, sync_workspace, RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions,
    };

    use homeboy_core::test_support::run_git_command as git;

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
        homeboy_core::test_support::with_isolated_home(|_| {
            let workspace_parent = tempfile::tempdir().expect("workspace parent");
            let source = workspace_parent.path().join("host-app");
            let dependency = workspace_parent.path().join("shared-runtime");
            let runner_root = tempfile::tempdir().expect("runner root tempdir");
            fs::create_dir_all(source.join("src")).expect("source dir");
            fs::create_dir_all(dependency.join("lib")).expect("dependency dir");
            fs::write(
                source.join("homeboy.json"),
                serde_json::json!({
                    "id": "host-app",
                    "extensions": {
                        "wordpress": {
                            "settings": {
                                "validation_dependencies": ["shared-runtime"]
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
                serde_json::json!({ "id": "shared-runtime" }).to_string(),
            )
            .expect("dependency manifest");
            fs::write(dependency.join("lib/runtime.php"), "<?php\n").expect("dependency file");
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
                    controller_routed_git: false,
                    changed_since_base: None,
                    git_fetch_refs: Vec::new(),
                    snapshot_includes: Vec::new(),
                    allow_dirty_lab_workspace: false,
                    validation_dependency_ids: None,
                    run_isolation_token: None,
                },
            )
            .expect("sync workspace");

            assert_eq!(exit_code, 0);
            assert_eq!(output.validation_dependencies.len(), 1);
            assert_eq!(output.validation_dependencies[0].id, "shared-runtime");
            assert_eq!(
                output.validation_dependencies[0].role,
                "validation_dependency"
            );
            assert_eq!(
                output.validation_dependencies[0].local_path,
                dependency.canonicalize().unwrap().display().to_string()
            );
            let remote_parent = parent_remote_path(&output.remote_path);
            assert!(Path::new(&output.remote_path).join("src/main.php").exists());
            let remote_dependency = Path::new(&remote_parent).join("shared-runtime");
            assert_eq!(
                output.validation_dependencies[0].remote_path,
                remote_dependency.display().to_string()
            );
            assert!(remote_dependency.join("lib/runtime.php").exists());
            assert!(!remote_dependency.join(".git").exists());
        });
    }

    #[test]
    fn sync_workspace_runs_validation_dependency_lifecycle_before_materializing() {
        homeboy_core::test_support::with_isolated_home(|_| {
            // The dependency lifecycle runs component deps/build scripts through the
            // extension subsystem, which registers its runners at binary startup.
            // Register them explicitly here so this test does not depend on a
            // sibling test having populated the process-global runner slots.
            homeboy_core::extension::component_script::register_component_script_runner();
            homeboy_core::extension::build::register_component_build_runner();
            let workspace_parent = tempfile::tempdir().expect("workspace parent");
            let source = workspace_parent.path().join("host-app");
            let dependency = workspace_parent.path().join("shared-runtime");
            let runner_root = tempfile::tempdir().expect("runner root tempdir");
            fs::create_dir_all(&source).expect("source dir");
            fs::create_dir_all(&dependency).expect("dependency dir");
            fs::write(
                source.join("homeboy.json"),
                serde_json::json!({
                    "id": "host-app",
                    "extensions": {
                        "wordpress": {
                            "settings": {
                                "validation_dependencies": ["shared-runtime"]
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
                    "id": "shared-runtime",
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
                    controller_routed_git: false,
                    changed_since_base: None,
                    git_fetch_refs: Vec::new(),
                    snapshot_includes: Vec::new(),
                    allow_dirty_lab_workspace: false,
                    validation_dependency_ids: None,
                    run_isolation_token: None,
                },
            )
            .expect("sync workspace");

            assert_eq!(exit_code, 0);
            let remote_parent = parent_remote_path(&output.remote_path);
            assert!(Path::new(&remote_parent)
                .join("shared-runtime/deps-installed.txt")
                .exists());
            assert!(Path::new(&remote_parent)
                .join("shared-runtime/build-built.txt")
                .exists());
            assert!(!dependency.join("deps-installed.txt").exists());
            assert!(!dependency.join("build-built.txt").exists());
        });
    }

    #[test]
    fn sync_workspace_uses_manifest_id_for_absolute_validation_dependency() {
        homeboy_core::test_support::with_isolated_home(|_| {
            // Register the extension runners this dependency lifecycle needs (see
            // the sibling test above) rather than relying on cross-test global state.
            homeboy_core::extension::component_script::register_component_script_runner();
            homeboy_core::extension::build::register_component_build_runner();
            let workspace_parent = tempfile::tempdir().expect("workspace parent");
            let source = workspace_parent.path().join("host-app");
            let dependency = workspace_parent.path().join("shared-runtime");
            let runner_root = tempfile::tempdir().expect("runner root tempdir");
            fs::create_dir_all(&source).expect("source dir");
            fs::create_dir_all(&dependency).expect("dependency dir");
            fs::write(
                source.join("homeboy.json"),
                serde_json::json!({
                    "id": "host-app",
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
                    "id": "shared-runtime",
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
                    controller_routed_git: false,
                    changed_since_base: None,
                    git_fetch_refs: Vec::new(),
                    snapshot_includes: Vec::new(),
                    allow_dirty_lab_workspace: false,
                    validation_dependency_ids: None,
                    run_isolation_token: None,
                },
            )
            .expect("sync workspace");

            assert_eq!(exit_code, 0);
            let remote_parent = parent_remote_path(&output.remote_path);
            let remote_dependency = Path::new(&remote_parent).join("shared-runtime");
            assert!(remote_dependency.join("component-id.txt").exists());
            assert_eq!(
                fs::read_to_string(remote_dependency.join("component-id.txt")).unwrap(),
                "shared-runtime"
            );
            assert!(!Path::new(&remote_parent).join("Users").exists());
        });
    }

    /// Write a component checkout whose build appends to an external counter,
    /// so lifecycle runs can be counted across prepared copies.
    fn counting_dependency(
        parent: &Path,
        id: &str,
        validation_dependencies: &[&str],
        counter: &Path,
    ) -> (PathBuf, tempfile::TempDir) {
        let path = parent.join(id);
        fs::create_dir_all(&path).expect("dependency dir");
        let mut manifest = serde_json::json!({
            "id": id,
            "scripts": {
                "build": [format!("sh -c 'printf \"{id} \" >> {}'", counter.display())]
            }
        });
        if !validation_dependencies.is_empty() {
            manifest["validation_dependencies"] = serde_json::json!(validation_dependencies);
        }
        fs::write(path.join("homeboy.json"), manifest.to_string()).expect("manifest");
        fs::write(path.join("lib.php"), format!("<?php // {id}\n")).expect("source");
        let remote = init_checkout_with_upstream(&path);
        (path, remote)
    }

    fn snapshot_sync(runner: &str, path: &Path) -> crate::workspace::RunnerWorkspaceSyncOutput {
        let (output, exit_code) = sync_workspace(
            runner,
            RunnerWorkspaceSyncOptions {
                path: path.display().to_string(),
                mode: RunnerWorkspaceSyncMode::Snapshot,
                controller_routed_git: false,
                changed_since_base: None,
                git_fetch_refs: Vec::new(),
                snapshot_includes: Vec::new(),
                allow_dirty_lab_workspace: false,
                validation_dependency_ids: None,
                run_isolation_token: None,
            },
        )
        .expect("sync workspace");
        assert_eq!(exit_code, 0);
        output
    }

    fn built_ids(counter: &Path) -> Vec<String> {
        fs::read_to_string(counter)
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn repeated_and_transitive_syncs_build_each_dependency_once() {
        homeboy_core::test_support::with_isolated_home(|_| {
            homeboy_core::extension::component_script::register_component_script_runner();
            homeboy_core::extension::build::register_component_build_runner();
            let workspace_parent = tempfile::tempdir().expect("workspace parent");
            let runner_root = tempfile::tempdir().expect("runner root tempdir");
            let counter = workspace_parent.path().join("build-count.txt");

            // host-app -> [core-runtime, core-runtime (duplicate), addon]
            // addon    -> [core-runtime]   (transitive, like staging's extra workspaces)
            let (_core, _core_remote) =
                counting_dependency(workspace_parent.path(), "core-runtime", &[], &counter);
            let (addon, _addon_remote) = counting_dependency(
                workspace_parent.path(),
                "addon",
                &["core-runtime"],
                &counter,
            );
            let source = workspace_parent.path().join("host-app");
            fs::create_dir_all(&source).expect("source dir");
            fs::write(
                source.join("homeboy.json"),
                serde_json::json!({
                    "id": "host-app",
                    "extensions": { "wordpress": { "settings": {
                        "validation_dependencies": ["core-runtime", "core-runtime", "addon"]
                    } } }
                })
                .to_string(),
            )
            .expect("source manifest");

            super::super::create(
                &format!(
                    r#"{{"id":"lab-local-cache","kind":"local","workspace_root":"{}"}}"#,
                    runner_root.path().display()
                ),
                false,
            )
            .expect("create runner");

            let first = snapshot_sync("lab-local-cache", &source);
            assert_eq!(
                first
                    .validation_dependencies
                    .iter()
                    .map(|dependency| dependency.id.as_str())
                    .collect::<Vec<_>>(),
                vec!["addon", "core-runtime"],
                "duplicate declarations are prepared once"
            );
            // Staging syncs each dependency as its own workspace, which prepares
            // that dependency's validation dependencies again.
            snapshot_sync("lab-local-cache", &addon);
            // A transport retry repeats the whole primary sync.
            let retried = snapshot_sync("lab-local-cache", &source);

            let mut built = built_ids(&counter);
            built.sort();
            assert_eq!(built, vec!["addon", "core-runtime"]);
            assert!(
                retried
                    .validation_dependencies
                    .iter()
                    .all(|dependency| dependency.prepare_cache.as_deref() == Some("hit")),
                "the retried sync reports every dependency as a cache hit"
            );

            let remote_parent = parent_remote_path(&retried.remote_path);
            assert!(Path::new(&remote_parent)
                .join("core-runtime/lib.php")
                .exists());
            assert!(Path::new(&remote_parent).join("addon/lib.php").exists());
        });
    }

    #[test]
    fn source_change_rebuilds_only_the_changed_dependency() {
        homeboy_core::test_support::with_isolated_home(|_| {
            homeboy_core::extension::component_script::register_component_script_runner();
            homeboy_core::extension::build::register_component_build_runner();
            let workspace_parent = tempfile::tempdir().expect("workspace parent");
            let runner_root = tempfile::tempdir().expect("runner root tempdir");
            let counter = workspace_parent.path().join("build-count.txt");
            let (core, _core_remote) =
                counting_dependency(workspace_parent.path(), "core-runtime", &[], &counter);
            let (_addon, _addon_remote) =
                counting_dependency(workspace_parent.path(), "addon", &[], &counter);
            let source = workspace_parent.path().join("host-app");
            fs::create_dir_all(&source).expect("source dir");
            fs::write(
                source.join("homeboy.json"),
                serde_json::json!({
                    "id": "host-app",
                    "extensions": { "wordpress": { "settings": {
                        "validation_dependencies": ["core-runtime", "addon"]
                    } } }
                })
                .to_string(),
            )
            .expect("source manifest");
            super::super::create(
                &format!(
                    r#"{{"id":"lab-local-cache-change","kind":"local","workspace_root":"{}"}}"#,
                    runner_root.path().display()
                ),
                false,
            )
            .expect("create runner");

            snapshot_sync("lab-local-cache-change", &source);
            fs::write(core.join("lib.php"), "<?php // v2\n").expect("edit");
            git(&core, &["commit", "-q", "-am", "v2"]);
            git(&core, &["push", "-q"]);
            snapshot_sync("lab-local-cache-change", &source);

            let mut built = built_ids(&counter);
            built.sort();
            assert_eq!(built, vec!["addon", "core-runtime", "core-runtime"]);
        });
    }

    #[test]
    fn sync_workspace_failed_validation_dependency_build_keeps_source_clean() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let workspace_parent = tempfile::tempdir().expect("workspace parent");
            let source = workspace_parent.path().join("host-app");
            let dependency = workspace_parent.path().join("shared-runtime");
            let runner_root = tempfile::tempdir().expect("runner root tempdir");
            fs::create_dir_all(&source).expect("source dir");
            fs::create_dir_all(&dependency).expect("dependency dir");
            fs::write(
                source.join("homeboy.json"),
                serde_json::json!({
                    "id": "host-app",
                    "extensions": {
                        "wordpress": {
                            "settings": {
                                "validation_dependencies": ["shared-runtime"]
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
                    "id": "shared-runtime",
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
                    controller_routed_git: false,
                    changed_since_base: None,
                    git_fetch_refs: Vec::new(),
                    snapshot_includes: Vec::new(),
                    allow_dirty_lab_workspace: false,
                    validation_dependency_ids: None,
                    run_isolation_token: None,
                },
            )
            .expect_err("failed dependency build should fail sync");

            assert_eq!(err.code.as_str(), "dependency_step_failed");
            assert_eq!(
                err.details.get("step_id").and_then(|value| value.as_str()),
                Some("dependency.build")
            );
            assert_eq!(
                err.details
                    .get("component_id")
                    .and_then(|value| value.as_str()),
                Some("shared-runtime")
            );
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
        homeboy_core::test_support::with_isolated_home(|_| {
            let workspace_parent = tempfile::tempdir().expect("workspace parent");
            let remote_parent = tempfile::tempdir().expect("remote parent");
            let source = workspace_parent.path().join("host-app");
            let seed = remote_parent.path().join("shared-runtime-seed");
            let clone_target = workspace_parent.path().join("shared-runtime-clone");
            let runner_root = tempfile::tempdir().expect("runner root tempdir");
            fs::create_dir_all(&source).expect("source dir");
            fs::create_dir_all(seed.join("lib")).expect("seed dir");
            fs::write(
                source.join("homeboy.json"),
                serde_json::json!({
                    "id": "host-app",
                    "extensions": {
                        "wordpress": {
                            "settings": {
                                "validation_dependencies": ["shared-runtime"]
                            }
                        }
                    }
                })
                .to_string(),
            )
            .expect("source manifest");
            fs::write(
                seed.join("homeboy.json"),
                serde_json::json!({ "id": "shared-runtime" }).to_string(),
            )
            .expect("seed manifest");
            fs::write(seed.join("lib/runtime.php"), "<?php\n").expect("seed file");
            let remote = init_checkout_with_upstream(&seed);
            let components_dir = homeboy_core::paths::components().expect("components dir");
            fs::create_dir_all(&components_dir).expect("components dir exists");
            fs::write(
                components_dir.join("shared-runtime.json"),
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
                    controller_routed_git: false,
                    changed_since_base: None,
                    git_fetch_refs: Vec::new(),
                    snapshot_includes: Vec::new(),
                    allow_dirty_lab_workspace: false,
                    validation_dependency_ids: None,
                    run_isolation_token: None,
                },
            )
            .expect("sync workspace");

            assert_eq!(exit_code, 0);
            assert!(clone_target.join("lib/runtime.php").exists());
            let remote_parent = parent_remote_path(&output.remote_path);
            assert!(Path::new(&remote_parent)
                .join("shared-runtime/lib/runtime.php")
                .exists());
        });
    }

    #[test]
    fn sync_workspace_rolls_back_checkout_when_validation_dependency_fails() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let workspace_parent = tempfile::tempdir().expect("workspace parent");
            let source = workspace_parent.path().join("host-app");
            let runner_root = tempfile::tempdir().expect("runner root tempdir");
            fs::create_dir_all(&source).expect("source dir");
            fs::write(
                source.join("homeboy.json"),
                serde_json::json!({
                    "id": "host-app",
                    "extensions": {
                        "wordpress": {
                            "settings": { "validation_dependencies": ["shared-runtime"] }
                        }
                    }
                })
                .to_string(),
            )
            .expect("source manifest");
            fs::write(source.join("main.php"), "<?php\n").expect("source file");

            super::super::create(
                &format!(
                    r#"{{"id":"lab-local-rollback","kind":"local","workspace_root":"{}"}}"#,
                    runner_root.path().display()
                ),
                false,
            )
            .expect("create runner");

            // The missing `shared-runtime` sibling makes validation-dependency
            // sync fail *after* the main checkout and its metadata are already
            // materialized — the exact partial-failure window from #6752.
            let err = sync_workspace(
                "lab-local-rollback",
                RunnerWorkspaceSyncOptions {
                    path: source.display().to_string(),
                    mode: RunnerWorkspaceSyncMode::Snapshot,
                    controller_routed_git: false,
                    changed_since_base: None,
                    git_fetch_refs: Vec::new(),
                    snapshot_includes: Vec::new(),
                    allow_dirty_lab_workspace: false,
                    validation_dependency_ids: None,
                    run_isolation_token: None,
                },
            )
            .expect_err("missing validation dependency should fail sync");
            assert!(err.message.contains("shared-runtime"));

            // The materialized checkout must be rolled back so no orphaned remote
            // directory survives under _lab_workspaces.
            let lab_workspaces = runner_root.path().join("_lab_workspaces");
            let leftovers = fs::read_dir(&lab_workspaces)
                .map(|entries| entries.filter_map(|entry| entry.ok()).count())
                .unwrap_or(0);
            assert_eq!(
                leftovers,
                0,
                "partial sync must not leave an orphaned remote checkout under {}",
                lab_workspaces.display()
            );
        });
    }

    #[test]
    fn validation_dependency_workspace_errors_when_sibling_missing() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let workspace_parent = tempfile::tempdir().expect("workspace parent");
            let source = workspace_parent.path().join("host-app");
            fs::create_dir_all(&source).expect("source dir");
            fs::write(
                source.join("homeboy.json"),
                serde_json::json!({
                    "id": "host-app",
                    "extensions": {
                        "wordpress": {
                            "settings": {
                                "validation_dependencies": ["shared-runtime"]
                            }
                        }
                    }
                })
                .to_string(),
            )
            .expect("source manifest");

            let err = validation_dependency_workspaces(&source, &[], None)
                .expect_err("missing dependency");

            assert_eq!(err.details["field"], "validation_dependencies");
            assert!(err.message.contains("shared-runtime"));
        });
    }
}
