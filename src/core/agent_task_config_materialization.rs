use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Map, Value};

use crate::core::component::{self, TargetSpec};
use crate::core::git;
use crate::core::{paths, Error, Result};

const MATERIALIZATION_DIR: &str = "agent-task-provider-refs";

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConfiguredRef {
    repo: String,
    ref_name: String,
    path_in_repo: Option<String>,
    setup: Vec<String>,
    readiness: Vec<ConfiguredRefPathProbe>,
    conflicts: Vec<ConfiguredRefPathProbe>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MaterializedRef {
    path: String,
    repo: String,
    ref_name: String,
    path_in_repo: Option<String>,
    setup: Vec<MaterializedRefSetupEvidence>,
    readiness: Vec<MaterializedRefPathEvidence>,
    conflicts: Vec<MaterializedRefPathEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConfiguredRefPathProbe {
    path: String,
    label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MaterializedRefSetupEvidence {
    command: String,
    status: i32,
    stdout: String,
    stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MaterializedRefPathEvidence {
    path: String,
    label: Option<String>,
    exists: bool,
}

pub(crate) fn materialize_provider_config_refs(config: Value) -> Result<Value> {
    materialize_value(config)
}

fn materialize_value(value: Value) -> Result<Value> {
    match value {
        Value::Array(items) => items
            .into_iter()
            .map(materialize_value)
            .collect::<Result<Vec<_>>>()
            .map(Value::Array),
        Value::Object(map) => materialize_object(map),
        other => Ok(other),
    }
}

fn materialize_object(mut map: Map<String, Value>) -> Result<Value> {
    let keys = map.keys().cloned().collect::<Vec<_>>();
    for key in keys {
        if let Some(value) = map.remove(&key) {
            map.insert(key, materialize_value(value)?);
        }
    }

    if let Some(configured_ref) = configured_ref_from_map(&map)? {
        let materialized = materialize_configured_ref(&configured_ref)?;
        if ref_object_is_path_alias(&map) {
            return Ok(Value::String(materialized.path));
        }
        map.insert("path".to_string(), Value::String(materialized.path.clone()));
        map.insert(
            "materialized_path".to_string(),
            Value::String(materialized.path.clone()),
        );
        map.insert(
            "materialized_ref".to_string(),
            serde_json::json!({
                "repo": materialized.repo,
                "ref": materialized.ref_name,
                "path_in_repo": materialized.path_in_repo,
                "path": materialized.path,
                "setup": materialized.setup.into_iter().map(|evidence| serde_json::json!({
                    "command": evidence.command,
                    "status": evidence.status,
                    "stdout": evidence.stdout,
                    "stderr": evidence.stderr,
                })).collect::<Vec<_>>(),
                "readiness": materialized.readiness.into_iter().map(|evidence| serde_json::json!({
                    "path": evidence.path,
                    "label": evidence.label,
                    "exists": evidence.exists,
                })).collect::<Vec<_>>(),
                "conflicts": materialized.conflicts.into_iter().map(|evidence| serde_json::json!({
                    "path": evidence.path,
                    "label": evidence.label,
                    "exists": evidence.exists,
                })).collect::<Vec<_>>(),
            }),
        );
    }
    Ok(Value::Object(map))
}

fn configured_ref_from_map(map: &Map<String, Value>) -> Result<Option<ConfiguredRef>> {
    let Some(repo) = map.get("repo").and_then(Value::as_str) else {
        return Ok(None);
    };
    let Some(ref_name) = map.get("ref").and_then(Value::as_str) else {
        return Ok(None);
    };
    let path_in_repo = map
        .get("path_in_repo")
        .or_else(|| map.get("pathInRepo"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(validate_relative_path)
        .transpose()?;
    let setup = string_array_from_map(map, "setup")?;
    let readiness = path_probes_from_map(map, "readiness")?;
    let conflicts = path_probes_from_map(map, "conflicts")?;

    Ok(Some(ConfiguredRef {
        repo: non_empty(repo, "repo")?,
        ref_name: non_empty(ref_name, "ref")?,
        path_in_repo,
        setup,
        readiness,
        conflicts,
    }))
}

fn ref_object_is_path_alias(map: &Map<String, Value>) -> bool {
    map.keys()
        .all(|key| matches!(key.as_str(), "repo" | "ref" | "path_in_repo" | "pathInRepo"))
}

fn materialize_configured_ref(configured_ref: &ConfiguredRef) -> Result<MaterializedRef> {
    let remote = resolve_repo_remote(&configured_ref.repo)?;
    let checkout = materialized_checkout_path(&configured_ref.repo, &configured_ref.ref_name)?;
    if checkout.join(".git").exists() {
        git::run_git(
            &checkout,
            &["fetch", "--prune", "origin"],
            "git fetch provider ref",
        )?;
    } else {
        if let Some(parent) = checkout.parent() {
            fs::create_dir_all(parent).map_err(|err| {
                Error::internal_io(err.to_string(), Some(parent.display().to_string()))
            })?;
        }
        git::clone_repo(&remote, &checkout)?;
    }

    let checkout_ref = checkout_ref(&checkout, &configured_ref.ref_name)?;
    git::run_git(
        &checkout,
        &["checkout", "--detach", &checkout_ref],
        "git checkout provider ref",
    )?;
    git::run_git(
        &checkout,
        &["reset", "--hard", &checkout_ref],
        "git reset provider ref",
    )?;

    let path = match &configured_ref.path_in_repo {
        Some(path_in_repo) => checkout.join(path_in_repo),
        None => checkout.clone(),
    };
    if !path.exists() {
        return Err(Error::validation_invalid_argument(
            "provider_config",
            "materialized provider/runtime ref is missing path_in_repo",
            Some(path.display().to_string()),
            Some(vec![format!(
                "repo={} ref={} path_in_repo={}",
                configured_ref.repo,
                configured_ref.ref_name,
                configured_ref.path_in_repo.as_deref().unwrap_or("")
            )]),
        ));
    }

    let setup = run_setup_commands(configured_ref, &path)?;
    let readiness = collect_path_evidence(&path, &configured_ref.readiness);
    let missing_readiness: Vec<_> = readiness
        .iter()
        .filter(|evidence| !evidence.exists)
        .collect();
    if !missing_readiness.is_empty() {
        return Err(Error::validation_invalid_argument(
            "provider_config",
            format!(
                "materialized provider/runtime ref '{}' is missing {} declared readiness path(s)",
                configured_ref.repo,
                missing_readiness.len()
            ),
            Some(configured_ref.ref_name.clone()),
            Some(
                missing_readiness
                    .iter()
                    .map(|evidence| evidence.path.clone())
                    .collect(),
            ),
        ));
    }

    let conflicts = collect_path_evidence(&path, &configured_ref.conflicts);
    let present_conflicts: Vec<_> = conflicts
        .iter()
        .filter(|evidence| evidence.exists)
        .collect();
    if !present_conflicts.is_empty() {
        return Err(Error::validation_invalid_argument(
            "provider_config",
            format!(
                "materialized provider/runtime ref '{}' contains {} declared conflict path(s)",
                configured_ref.repo,
                present_conflicts.len()
            ),
            Some(configured_ref.ref_name.clone()),
            Some(
                present_conflicts
                    .iter()
                    .map(|evidence| evidence.path.clone())
                    .collect(),
            ),
        ));
    }

    Ok(MaterializedRef {
        path: path.display().to_string(),
        repo: configured_ref.repo.clone(),
        ref_name: configured_ref.ref_name.clone(),
        path_in_repo: configured_ref.path_in_repo.clone(),
        setup,
        readiness,
        conflicts,
    })
}

fn run_setup_commands(
    configured_ref: &ConfiguredRef,
    path: &Path,
) -> Result<Vec<MaterializedRefSetupEvidence>> {
    let mut evidence = Vec::new();
    for command in &configured_ref.setup {
        let output = Command::new("sh")
            .arg("-lc")
            .arg(command)
            .current_dir(path)
            .output()
            .map_err(|err| {
                Error::internal_io(
                    err.to_string(),
                    Some(format!("run provider/runtime ref setup: {command}")),
                )
            })?;
        let status = output.status.code().unwrap_or(1);
        let setup_evidence = MaterializedRefSetupEvidence {
            command: command.clone(),
            status,
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        };
        if !output.status.success() {
            return Err(Error::validation_invalid_argument(
                "provider_config",
                format!(
                    "materialized provider/runtime ref '{}' setup command failed",
                    configured_ref.repo
                ),
                Some(command.clone()),
                Some(vec![format!(
                    "status={} stdout={} stderr={}",
                    setup_evidence.status, setup_evidence.stdout, setup_evidence.stderr
                )]),
            ));
        }
        evidence.push(setup_evidence);
    }
    Ok(evidence)
}

fn collect_path_evidence(
    root: &Path,
    probes: &[ConfiguredRefPathProbe],
) -> Vec<MaterializedRefPathEvidence> {
    probes
        .iter()
        .map(|probe| {
            let path = root.join(&probe.path);
            MaterializedRefPathEvidence {
                path: path.display().to_string(),
                label: probe.label.clone(),
                exists: path.exists(),
            }
        })
        .collect()
}

fn resolve_repo_remote(repo: &str) -> Result<String> {
    let expanded = shellexpand::tilde(repo).into_owned();
    let path = Path::new(&expanded);
    if path.exists() {
        return Ok(expanded);
    }
    if repo.contains("://") || repo.starts_with("git@") || repo.ends_with(".git") {
        return Ok(repo.to_string());
    }
    if repo.matches('/').count() == 1 {
        return Ok(format!("https://github.com/{repo}.git"));
    }
    if let Ok(target) = component::resolve_target(TargetSpec {
        component_id: Some(repo),
        path_override: None,
        project: None,
        capability: None,
        allow_synthetic: false,
        accept_bare_directory: false,
        ..TargetSpec::default()
    }) {
        if let Some(git_root) = target.git_root {
            if let Some(remote) =
                git::output_optional(&git_root, &["config", "--get", "remote.origin.url"])
            {
                if !remote.trim().is_empty() {
                    return Ok(remote);
                }
            }
            return Ok(git_root.display().to_string());
        }
    }

    Err(Error::validation_invalid_argument(
        "provider_config",
        "configured provider/runtime ref repo is not a path, URL, GitHub owner/name, or registered component id",
        Some(repo.to_string()),
        None,
    ))
}

fn materialized_checkout_path(repo: &str, ref_name: &str) -> Result<PathBuf> {
    Ok(paths::homeboy_data()?
        .join(MATERIALIZATION_DIR)
        .join(slug(repo))
        .join(slug(ref_name)))
}

fn checkout_ref(checkout: &Path, ref_name: &str) -> Result<String> {
    if git::run_git(
        checkout,
        &["rev-parse", "--verify", &format!("{ref_name}^{{commit}}")],
        "git verify provider ref",
    )
    .is_ok()
    {
        return Ok(ref_name.to_string());
    }
    let remote_ref = format!("origin/{ref_name}");
    if git::run_git(
        checkout,
        &["rev-parse", "--verify", &format!("{remote_ref}^{{commit}}")],
        "git verify remote provider ref",
    )
    .is_ok()
    {
        return Ok(remote_ref);
    }
    Ok(ref_name.to_string())
}

fn validate_relative_path(value: &str) -> Result<String> {
    let path = Path::new(value);
    if path.is_absolute() || value.split('/').any(|part| part == "..") {
        return Err(Error::validation_invalid_argument(
            "path_in_repo",
            "provider/runtime ref path_in_repo must be relative and stay inside the repository",
            Some(value.to_string()),
            None,
        ));
    }
    Ok(value.to_string())
}

fn string_array_from_map(map: &Map<String, Value>, key: &str) -> Result<Vec<String>> {
    let Some(value) = map.get(key) else {
        return Ok(Vec::new());
    };
    let Some(items) = value.as_array() else {
        return Err(Error::validation_invalid_argument(
            key,
            "provider/runtime ref field must be an array",
            None,
            None,
        ));
    };
    items
        .iter()
        .map(|item| {
            item.as_str()
                .ok_or_else(|| {
                    Error::validation_invalid_argument(
                        key,
                        "provider/runtime ref setup entries must be strings",
                        None,
                        None,
                    )
                })
                .and_then(|value| non_empty(value, key))
        })
        .collect()
}

fn path_probes_from_map(
    map: &Map<String, Value>,
    key: &str,
) -> Result<Vec<ConfiguredRefPathProbe>> {
    let Some(value) = map.get(key) else {
        return Ok(Vec::new());
    };
    let Some(items) = value.as_array() else {
        return Err(Error::validation_invalid_argument(
            key,
            "provider/runtime ref path probe field must be an array",
            None,
            None,
        ));
    };
    items
        .iter()
        .map(|item| path_probe_from_value(item, key))
        .collect()
}

fn path_probe_from_value(value: &Value, key: &str) -> Result<ConfiguredRefPathProbe> {
    if let Some(path) = value.as_str() {
        return Ok(ConfiguredRefPathProbe {
            path: validate_relative_path(&non_empty(path, key)?)?,
            label: None,
        });
    }
    let Some(map) = value.as_object() else {
        return Err(Error::validation_invalid_argument(
            key,
            "provider/runtime ref path probes must be strings or objects",
            None,
            None,
        ));
    };
    let Some(path) = map.get("path").and_then(Value::as_str) else {
        return Err(Error::validation_invalid_argument(
            key,
            "provider/runtime ref path probe objects must include a path string",
            None,
            None,
        ));
    };
    Ok(ConfiguredRefPathProbe {
        path: validate_relative_path(&non_empty(path, key)?)?,
        label: map
            .get("label")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|label| !label.is_empty())
            .map(str::to_string),
    })
}

fn non_empty(value: &str, field: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(Error::validation_invalid_argument(
            field,
            "configured provider/runtime ref field cannot be empty",
            None,
            None,
        ));
    }
    Ok(value.to_string())
}

fn slug(value: &str) -> String {
    let slug = paths::sanitize_path_segment(value)
        .trim_matches('_')
        .to_string();
    if slug.is_empty() {
        "ref".to_string()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_isolated_home;

    #[test]
    fn path_alias_refs_become_local_path_strings() {
        with_isolated_home(|_| {
            let repo = tempfile::tempdir().expect("repo");
            init_repo(repo.path());
            fs::create_dir_all(repo.path().join("plugin")).expect("plugin dir");
            fs::write(repo.path().join("plugin/main.php"), "<?php").expect("plugin file");
            commit_all(repo.path());
            let head = git::run_git(repo.path(), &["rev-parse", "HEAD"], "head")
                .expect("head")
                .trim()
                .to_string();

            let config = serde_json::json!({
                "provider_plugin_paths": [{
                    "repo": repo.path().display().to_string(),
                    "ref": head,
                    "path_in_repo": "plugin"
                }]
            });

            let materialized = materialize_provider_config_refs(config).expect("materialized");
            let path = materialized["provider_plugin_paths"][0]
                .as_str()
                .expect("path string");
            assert!(Path::new(path).join("main.php").is_file());
        });
    }

    #[test]
    fn ref_objects_with_extra_fields_preserve_shape_and_gain_path() {
        with_isolated_home(|_| {
            let repo = tempfile::tempdir().expect("repo");
            init_repo(repo.path());
            fs::create_dir_all(repo.path().join("runtime")).expect("runtime dir");
            fs::write(repo.path().join("runtime/bootstrap.php"), "<?php").expect("runtime file");
            commit_all(repo.path());
            let head = git::run_git(repo.path(), &["rev-parse", "HEAD"], "head")
                .expect("head")
                .trim()
                .to_string();

            let config = serde_json::json!({
                "runtime_overlays": [{
                    "name": "runtime-client",
                    "repo": repo.path().display().to_string(),
                    "ref": head,
                    "path_in_repo": "runtime"
                }]
            });

            let materialized = materialize_provider_config_refs(config).expect("materialized");
            let overlay = &materialized["runtime_overlays"][0];
            assert_eq!(overlay["name"], "runtime-client");
            let path = overlay["path"].as_str().expect("path");
            assert_eq!(overlay["materialized_path"], path);
            assert!(Path::new(path).join("bootstrap.php").is_file());
        });
    }

    #[test]
    fn ref_setup_materializes_declared_readiness_evidence() {
        with_isolated_home(|_| {
            let repo = tempfile::tempdir().expect("repo");
            init_repo(repo.path());
            fs::create_dir_all(repo.path().join("runtime")).expect("runtime dir");
            fs::write(
                repo.path().join("runtime/setup.sh"),
                "printf ready > ready.txt",
            )
            .expect("setup script");
            commit_all(repo.path());
            let head = git::run_git(repo.path(), &["rev-parse", "HEAD"], "head")
                .expect("head")
                .trim()
                .to_string();

            let config = serde_json::json!({
                "runtime_overlays": [{
                    "name": "runtime-client",
                    "repo": repo.path().display().to_string(),
                    "ref": head,
                    "path_in_repo": "runtime",
                    "setup": ["sh setup.sh"],
                    "readiness": [{ "path": "ready.txt", "label": "setup marker" }],
                    "conflicts": ["vendor/stale-runtime-copy"]
                }]
            });

            let materialized = materialize_provider_config_refs(config).expect("materialized");
            let overlay = &materialized["runtime_overlays"][0];
            let path = overlay["path"].as_str().expect("path");
            assert_eq!(
                fs::read_to_string(Path::new(path).join("ready.txt")).unwrap(),
                "ready"
            );
            let materialized_ref = &overlay["materialized_ref"];
            assert_eq!(materialized_ref["setup"][0]["command"], "sh setup.sh");
            assert_eq!(materialized_ref["setup"][0]["status"], 0);
            assert_eq!(materialized_ref["readiness"][0]["label"], "setup marker");
            assert_eq!(materialized_ref["readiness"][0]["exists"], true);
            assert_eq!(materialized_ref["conflicts"][0]["exists"], false);
        });
    }

    #[test]
    fn missing_readiness_probe_fails_closed() {
        with_isolated_home(|_| {
            let repo = tempfile::tempdir().expect("repo");
            init_repo(repo.path());
            fs::create_dir_all(repo.path().join("runtime")).expect("runtime dir");
            fs::write(repo.path().join("runtime/bootstrap.txt"), "runtime").expect("runtime file");
            commit_all(repo.path());
            let head = git::run_git(repo.path(), &["rev-parse", "HEAD"], "head")
                .expect("head")
                .trim()
                .to_string();

            let err = materialize_provider_config_refs(serde_json::json!({
                "runtime_overlays": [{
                    "repo": repo.path().display().to_string(),
                    "ref": head,
                    "path_in_repo": "runtime",
                    "readiness": ["vendor/autoload.php"]
                }]
            }))
            .expect_err("missing readiness should fail");

            assert_eq!(err.code.as_str(), "validation.invalid_argument");
            assert!(err.message.contains("missing 1 declared readiness path"));
            assert!(err.message.contains(repo.path().to_string_lossy().as_ref()));
        });
    }

    #[test]
    fn present_conflict_probe_fails_closed() {
        with_isolated_home(|_| {
            let repo = tempfile::tempdir().expect("repo");
            init_repo(repo.path());
            fs::create_dir_all(repo.path().join("runtime/vendor/stale-runtime-copy"))
                .expect("conflict dir");
            fs::write(
                repo.path()
                    .join("runtime/vendor/stale-runtime-copy/package.txt"),
                "conflict",
            )
            .expect("conflict file");
            fs::write(repo.path().join("runtime/bootstrap.txt"), "runtime").expect("runtime file");
            commit_all(repo.path());
            let head = git::run_git(repo.path(), &["rev-parse", "HEAD"], "head")
                .expect("head")
                .trim()
                .to_string();

            let err = materialize_provider_config_refs(serde_json::json!({
                "runtime_overlays": [{
                    "repo": repo.path().display().to_string(),
                    "ref": head,
                    "path_in_repo": "runtime",
                    "conflicts": ["vendor/stale-runtime-copy"]
                }]
            }))
            .expect_err("present conflict should fail");

            assert_eq!(err.code.as_str(), "validation.invalid_argument");
            assert!(err.message.contains("contains 1 declared conflict path"));
        });
    }

    fn init_repo(path: &Path) {
        git::run_git(path, &["init"], "init").expect("git init");
        git::run_git(path, &["config", "user.name", "Test"], "name").expect("git name");
        git::run_git(path, &["config", "user.email", "test@example.com"], "email")
            .expect("git email");
    }

    fn commit_all(path: &Path) {
        git::run_git(path, &["add", "."], "add").expect("git add");
        git::run_git(path, &["commit", "-m", "fixture"], "commit").expect("git commit");
    }
}
