use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::core::component::{CommandScopeConfig, Component};
use crate::core::error::{Error, Result};
use crate::core::extension::ExtensionManifest;
use crate::core::release::types::{ReleaseArtifact, ReleaseState, ReleaseStepResult};

use super::{run_package, step_success};

/// Validate packaging against a temporary component copy before the release
/// pipeline mutates changelog, version files, or git state.
pub(crate) fn run_package_preflight(
    extensions: &[ExtensionManifest],
    component: &Component,
    component_id: &str,
    component_local_path: &str,
    skip_build_validation: bool,
) -> Result<ReleaseStepResult> {
    // Inspect the original component checkout (which still has its `.git`) for
    // git-pinned dependencies that lack a committed lockfile. The isolated
    // build copy below excludes `.git`, so this committed-state check must run
    // against the source tree before we mutate or build anything.
    super::lockfile_guard::guard_committed_lockfiles(Path::new(component_local_path))?;
    super::lockfile_guard::guard_local_file_dependencies(Path::new(component_local_path))?;

    let source_component_path = Path::new(component_local_path);
    let source_root = release_preflight_source_root(source_component_path)?;
    let temp = create_release_preflight_tempdir()?;
    let temp_root_path = temp.join("repository");
    copy_release_preflight_tree(&source_root, &temp_root_path)?;
    let temp_component_path =
        release_preflight_component_path(source_component_path, &source_root, &temp_root_path)?;

    let mut state = ReleaseState::default();
    let result = run_package(
        extensions,
        &mut state,
        component_id,
        &temp_component_path.to_string_lossy(),
        Some(component_local_path),
        skip_build_validation,
    );

    if let Err(err) = result {
        let diagnostic = package_preflight_failure_diagnostic(
            component_id,
            component_local_path,
            &temp,
            &temp_component_path,
            skip_build_validation,
            &err,
        );
        let artifact_path = persist_package_preflight_failure_diagnostic(component_id, &diagnostic)
            .unwrap_or_else(|persist_err| format!("unavailable: {}", persist_err.message));
        let _ = std::fs::remove_dir_all(&temp);
        return Err(package_preflight_failure_error(
            err,
            diagnostic,
            artifact_path,
        ));
    }

    let _ = std::fs::remove_dir_all(&temp);
    let result = result.expect("checked package preflight error above");
    validate_package_completeness(component, Path::new(component_local_path), &state.artifacts)?;

    let data = serde_json::json!({
        "component_path": component_local_path,
        "validated_action": "release.package",
        "artifacts": state.artifacts,
        "package_result": result.data,
    });
    Ok(step_success(
        "preflight.package",
        "preflight.package",
        Some(data),
        Vec::new(),
    ))
}

fn release_preflight_source_root(component_path: &Path) -> Result<PathBuf> {
    let component_path = component_path.canonicalize().map_err(|e| {
        Error::internal_io(
            format!("Failed to resolve package preflight component path: {}", e),
            Some(component_path.display().to_string()),
        )
    })?;

    let mut current = component_path.as_path();
    loop {
        if current.join(".git").exists() {
            return Ok(current.to_path_buf());
        }
        let Some(parent) = current.parent() else {
            return Ok(component_path);
        };
        current = parent;
    }
}

fn release_preflight_component_path(
    source_component_path: &Path,
    source_root: &Path,
    temp_root_path: &Path,
) -> Result<PathBuf> {
    let source_component_path = source_component_path.canonicalize().map_err(|e| {
        Error::internal_io(
            format!("Failed to resolve package preflight component path: {}", e),
            Some(source_component_path.display().to_string()),
        )
    })?;

    let relative_component_path = source_component_path
        .strip_prefix(source_root)
        .map_err(|e| {
            Error::internal_io(
                format!(
                    "Failed to map package preflight component path into staged repo: {}",
                    e
                ),
                Some(format!(
                    "component: {}; source root: {}",
                    source_component_path.display(),
                    source_root.display()
                )),
            )
        })?;

    Ok(temp_root_path.join(relative_component_path))
}

fn package_preflight_failure_diagnostic(
    component_id: &str,
    component_local_path: &str,
    temp_root: &Path,
    temp_component_path: &Path,
    skip_build_validation: bool,
    err: &Error,
) -> serde_json::Value {
    let source = err.details.get("source").unwrap_or(&err.details);
    let command = source
        .get("command")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let build_cwd = source
        .get("cwd")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| temp_component_path.display().to_string());
    let exit_code = source
        .get("exit_code")
        .or_else(|| source.get("exitCode"))
        .and_then(serde_json::Value::as_i64);
    let relevant_error_lines = source
        .get("relevant_error_lines")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));

    serde_json::json!({
        "component_id": component_id,
        "package_root": component_local_path,
        "build_cwd": build_cwd,
        "materialized_temp_root": temp_root.display().to_string(),
        "command": command,
        "exit_code": exit_code,
        "relevant_error_lines": relevant_error_lines,
        "config_fields": {
            "component.local_path": component_local_path,
            "release.local_path": temp_component_path.display().to_string(),
            "release.source_path": component_local_path,
            "config.skip_build_validation": skip_build_validation,
        },
        "error": {
            "message": err.message,
            "details": err.details,
        }
    })
}

fn persist_package_preflight_failure_diagnostic(
    component_id: &str,
    diagnostic: &serde_json::Value,
) -> Result<String> {
    let artifact_root = crate::core::paths::artifact_root()?;
    let path = artifact_root
        .join("release")
        .join(crate::core::paths::sanitize_path_segment(component_id))
        .join("package-preflight-failure.json");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            Error::internal_io(
                format!(
                    "Failed to create package preflight diagnostic directory: {}",
                    e
                ),
                Some(parent.display().to_string()),
            )
        })?;
    }
    let body = serde_json::to_string_pretty(diagnostic).map_err(|e| {
        Error::internal_json(
            e.to_string(),
            Some("package preflight diagnostic".to_string()),
        )
    })?;
    std::fs::write(&path, body).map_err(|e| {
        Error::internal_io(
            format!("Failed to write package preflight diagnostic: {}", e),
            Some(path.display().to_string()),
        )
    })?;
    Ok(path.display().to_string())
}

fn package_preflight_failure_error(
    err: Error,
    diagnostic: serde_json::Value,
    artifact_path: String,
) -> Error {
    let mut details = serde_json::json!({
        "diagnostic": diagnostic,
        "artifact_path": artifact_path,
    });
    details["diagnostic"]["artifact_path"] = serde_json::Value::String(artifact_path.clone());

    let mut wrapped = Error::new(
        err.code,
        format!(
            "Package preflight failed for component '{}'; diagnostic artifact: {}. {}",
            details["diagnostic"]["component_id"].as_str().unwrap_or(""),
            artifact_path,
            err.message
        ),
        details,
    );
    wrapped.hints = err.hints;
    wrapped.retryable = err.retryable;
    wrapped
}

fn create_release_preflight_tempdir() -> Result<PathBuf> {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "homeboy-release-package-preflight-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    ));

    std::fs::create_dir_all(&path).map_err(|e| {
        Error::internal_io(
            format!("Failed to create package preflight tempdir: {}", e),
            Some(path.display().to_string()),
        )
    })?;

    Ok(path)
}

fn copy_release_preflight_tree(source: &Path, destination: &Path) -> Result<()> {
    std::fs::create_dir_all(destination).map_err(|e| {
        Error::internal_io(
            format!("Failed to create package preflight copy: {}", e),
            Some(destination.display().to_string()),
        )
    })?;

    for entry in std::fs::read_dir(source).map_err(|e| {
        Error::internal_io(
            format!("Failed to read package preflight source: {}", e),
            Some(source.display().to_string()),
        )
    })? {
        let entry = entry.map_err(|e| {
            Error::internal_io(
                format!("Failed to read package preflight source entry: {}", e),
                Some(source.display().to_string()),
            )
        })?;
        let file_name = entry.file_name();
        if file_name == std::ffi::OsStr::new(".git") {
            continue;
        }

        let from = entry.path();
        let to = destination.join(&file_name);
        copy_release_preflight_entry(&from, &to)?;
    }

    Ok(())
}

fn copy_release_preflight_entry(source: &Path, destination: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(source).map_err(|e| {
        Error::internal_io(
            format!("Failed to inspect package preflight entry: {}", e),
            Some(source.display().to_string()),
        )
    })?;

    if metadata.file_type().is_symlink() {
        copy_release_preflight_symlink(source, destination)
    } else if metadata.is_dir() {
        copy_release_preflight_tree(source, destination)
    } else if metadata.is_file() {
        std::fs::copy(source, destination).map(|_| ()).map_err(|e| {
            Error::internal_io(
                format!("Failed to copy package preflight file: {}", e),
                Some(source.display().to_string()),
            )
        })
    } else {
        Ok(())
    }
}

fn copy_release_preflight_symlink(source: &Path, destination: &Path) -> Result<()> {
    let target = std::fs::read_link(source).map_err(|e| {
        Error::internal_io(
            format!("Failed to read package preflight symlink: {}", e),
            Some(source.display().to_string()),
        )
    })?;

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, destination).map_err(|e| {
            Error::internal_io(
                format!("Failed to copy package preflight symlink: {}", e),
                Some(source.display().to_string()),
            )
        })
    }

    #[cfg(not(unix))]
    {
        let target_path = if target.is_absolute() {
            target
        } else {
            source
                .parent()
                .map(|parent| parent.join(&target))
                .unwrap_or(target)
        };
        copy_release_preflight_entry(&target_path, destination)
    }
}

fn validate_package_completeness(
    component: &Component,
    component_path: &Path,
    artifacts: &[ReleaseArtifact],
) -> Result<()> {
    let expected = tracked_runtime_files(component, component_path)?;
    if expected.is_empty() {
        return Ok(());
    }

    let mut archive_entries = BTreeSet::new();
    for artifact in artifacts {
        let artifact_path = resolve_artifact_path(component_path, artifact);
        if !artifact_path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
        {
            continue;
        }
        archive_entries.extend(read_zip_entries(&artifact_path)?);
    }

    if archive_entries.is_empty() {
        return Ok(());
    }

    let missing: Vec<String> = expected
        .into_iter()
        .filter(|path| !archive_contains_path(&archive_entries, path))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "preflight.package",
        format!(
            "Release package is missing {} tracked runtime file(s): {}",
            missing.len(),
            missing.iter().take(20).cloned().collect::<Vec<_>>().join(", ")
        ),
        Some(format!(
            "{}",
            serde_json::json!({
                "missing": missing,
                "source": component_path,
                "checked_artifact_type": "zip"
            })
        )),
        Some(vec![
            "Update the release.package action so the artifact includes every tracked runtime file, or add an explicit release scope exclude for intentional omissions.".to_string(),
        ]),
    ))
}

fn tracked_runtime_files(component: &Component, component_path: &Path) -> Result<Vec<String>> {
    let output = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(component_path)
        .output()
        .map_err(|e| {
            Error::internal_io(
                format!(
                    "Failed to inspect tracked files for package preflight: {}",
                    e
                ),
                Some(component_path.display().to_string()),
            )
        })?;
    if !output.status.success() {
        return Err(Error::internal_unexpected(format!(
            "Failed to inspect tracked files for package preflight: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    let scope = release_scope(component);
    let files = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|raw| !raw.is_empty())
        .filter_map(|raw| String::from_utf8(raw.to_vec()).ok())
        .map(|path| normalize_archive_path(&path))
        .filter(|path| runtime_candidate(path))
        .filter(|path| scope_allows(path, &scope))
        .collect::<Vec<_>>();
    Ok(files)
}

fn release_scope(component: &Component) -> CommandScopeConfig {
    let mut scope = CommandScopeConfig::default();
    if let Some(scopes) = component.scopes.as_ref() {
        if let Some(defaults) = scopes.defaults.as_ref() {
            scope.include.extend(defaults.include.clone());
            scope.exclude.extend(defaults.exclude.clone());
        }
        if let Some(release) = scopes.release.as_ref() {
            scope.include.extend(release.include.clone());
            scope.exclude.extend(release.exclude.clone());
        }
    }
    scope
}

fn runtime_candidate(path: &str) -> bool {
    let file_name = path.rsplit('/').next().unwrap_or(path);
    if matches!(
        file_name,
        "package.json"
            | "package-lock.json"
            | "composer.json"
            | "composer.lock"
            | "tsconfig.json"
            | "phpunit.xml"
            | "phpunit.xml.dist"
    ) {
        return false;
    }
    if path.starts_with(".github/")
        || path.starts_with("docs/")
        || path.starts_with("test/")
        || path.starts_with("tests/")
        || path.starts_with("__tests__/")
        || path.starts_with("node_modules/")
        || path.starts_with("target/")
    {
        return false;
    }

    matches!(
        Path::new(path)
            .extension()
            .and_then(|extension| extension.to_str()),
        Some("php" | "inc" | "phtml" | "js" | "mjs" | "cjs" | "css" | "json")
    )
}

fn scope_allows(path: &str, scope: &CommandScopeConfig) -> bool {
    if !scope.include.is_empty()
        && !scope
            .include
            .iter()
            .any(|pattern| path_matches(pattern, path))
    {
        return false;
    }
    !scope
        .exclude
        .iter()
        .any(|pattern| path_matches(pattern, path))
}

fn path_matches(pattern: &str, path: &str) -> bool {
    glob_match::glob_match(pattern, path)
        || glob_match::glob_match(pattern, &format!("/{}", path))
        || path.starts_with(pattern.trim_end_matches('/'))
}

fn resolve_artifact_path(component_path: &Path, artifact: &ReleaseArtifact) -> PathBuf {
    let path = artifact
        .durable_path
        .as_deref()
        .filter(|path| !path.trim().is_empty())
        .unwrap_or(&artifact.path);
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        component_path.join(path)
    }
}

fn read_zip_entries(path: &Path) -> Result<BTreeSet<String>> {
    let file = std::fs::File::open(path).map_err(|e| {
        Error::internal_io(
            format!(
                "Failed to open release package artifact for completeness check: {}",
                e
            ),
            Some(path.display().to_string()),
        )
    })?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| {
        Error::internal_unexpected(format!(
            "Failed to inspect release package artifact '{}': {}",
            path.display(),
            e
        ))
    })?;

    let mut entries = BTreeSet::new();
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|e| {
            Error::internal_unexpected(format!(
                "Failed to inspect release package artifact '{}': {}",
                path.display(),
                e
            ))
        })?;
        if entry.is_dir() {
            continue;
        }
        entries.insert(normalize_archive_path(entry.name()));
    }
    Ok(entries)
}

fn archive_contains_path(entries: &BTreeSet<String>, path: &str) -> bool {
    entries
        .iter()
        .any(|entry| entry == path || entry.ends_with(&format!("/{}", path)))
}

fn normalize_archive_path(path: &str) -> String {
    path.replace('\\', "/")
        .trim_start_matches("./")
        .trim_start_matches('/')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn package_completeness_flags_tracked_runtime_dir_missing_from_zip() {
        let repo = tempfile::tempdir().expect("repo");
        std::fs::create_dir_all(repo.path().join("agents")).expect("agents dir");
        std::fs::write(repo.path().join("plugin.php"), "<?php\n").expect("plugin");
        std::fs::write(repo.path().join("agents/runtime.php"), "<?php\n").expect("agent");
        run_git(repo.path(), &["init"]);
        run_git(repo.path(), &["add", "plugin.php", "agents/runtime.php"]);

        let artifact_path = repo.path().join("build/package.zip");
        std::fs::create_dir_all(artifact_path.parent().unwrap()).expect("build dir");
        write_zip(&artifact_path, &[("plugin/plugin.php", "<?php\n")]);

        let component = Component {
            id: "plugin".to_string(),
            local_path: repo.path().to_string_lossy().to_string(),
            ..Component::default()
        };
        let artifacts = vec![ReleaseArtifact {
            path: "build/package.zip".to_string(),
            durable_path: None,
            artifact_type: Some("archive".to_string()),
            platform: None,
        }];

        let error = validate_package_completeness(&component, repo.path(), &artifacts)
            .expect_err("missing tracked runtime file should fail");

        assert!(error.message.contains("agents/runtime.php"));
    }

    #[test]
    fn package_completeness_honors_release_scope_excludes() {
        let repo = tempfile::tempdir().expect("repo");
        std::fs::create_dir_all(repo.path().join("agents")).expect("agents dir");
        std::fs::write(repo.path().join("plugin.php"), "<?php\n").expect("plugin");
        std::fs::write(repo.path().join("agents/runtime.php"), "<?php\n").expect("agent");
        run_git(repo.path(), &["init"]);
        run_git(repo.path(), &["add", "plugin.php", "agents/runtime.php"]);

        let artifact_path = repo.path().join("build/package.zip");
        std::fs::create_dir_all(artifact_path.parent().unwrap()).expect("build dir");
        write_zip(&artifact_path, &[("plugin/plugin.php", "<?php\n")]);

        let component = Component {
            id: "plugin".to_string(),
            local_path: repo.path().to_string_lossy().to_string(),
            scopes: Some(crate::core::component::ScopeConfig {
                release: Some(CommandScopeConfig {
                    include: Vec::new(),
                    exclude: vec!["agents/**".to_string()],
                }),
                ..Default::default()
            }),
            ..Component::default()
        };
        let artifacts = vec![ReleaseArtifact {
            path: "build/package.zip".to_string(),
            durable_path: None,
            artifact_type: Some("archive".to_string()),
            platform: None,
        }];

        validate_package_completeness(&component, repo.path(), &artifacts)
            .expect("excluded runtime file should not fail");
    }

    fn run_git(repo: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn write_zip(path: &Path, entries: &[(&str, &str)]) {
        let file = std::fs::File::create(path).expect("zip file");
        let mut zip = zip::ZipWriter::new(file);
        for (name, body) in entries {
            zip.start_file(*name, zip::write::FileOptions::default())
                .expect("zip entry");
            zip.write_all(body.as_bytes()).expect("zip body");
        }
        zip.finish().expect("finish zip");
    }
}
