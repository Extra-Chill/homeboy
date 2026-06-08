use std::fs;
use std::path::{Path, PathBuf};

use crate::core::error::{Error, Result};

use super::workspace::{materialize_snapshot, parent_remote_path, sanitize_path_segment};
use super::Runner;

const PORTABLE_CONFIG_FILE: &str = concat!("homeboy", ".json");

pub(super) fn sync_validation_dependency_workspaces(
    runner: &Runner,
    local_path: &Path,
    remote_path: &str,
    excludes: &[String],
) -> Result<()> {
    for dependency in validation_dependency_workspaces(local_path)? {
        let remote_dependency_path = format!(
            "{}/{}",
            parent_remote_path(remote_path),
            sanitize_path_segment(
                dependency
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or("dependency"),
            )
        );
        materialize_snapshot(runner, &dependency, &remote_dependency_path, excludes)?;
    }
    Ok(())
}

fn validation_dependency_workspaces(local_path: &Path) -> Result<Vec<PathBuf>> {
    let dependency_ids = validation_dependency_ids(local_path)?;
    if dependency_ids.is_empty() {
        return Ok(Vec::new());
    }

    let Some(parent) = local_path.parent() else {
        return Ok(Vec::new());
    };

    dependency_ids
        .into_iter()
        .map(|dependency_id| resolve_sibling_dependency_workspace(parent, &dependency_id))
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

    use super::*;
    use crate::core::runner::workspace::{
        parent_remote_path, sync_workspace, RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions,
    };

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

        let err = validation_dependency_workspaces(&source).expect_err("missing dependency");

        assert_eq!(err.details["field"], "validation_dependencies");
        assert!(err.message.contains("agents-api"));
    }
}
