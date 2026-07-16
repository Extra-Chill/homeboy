use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::component::{CommandScopeConfig, Component};
use crate::error::{Error, Result};
use crate::release::types::ReleaseArtifact;

/// Run the package guards that must complete before release preparation mutates
/// the checkout. The versioned package itself is built once by the final
/// `package` step, after the release commit exists.
pub(crate) fn run_package_preflight(component_local_path: &str) -> Result<()> {
    let component_path = Path::new(component_local_path);
    super::lockfile_guard::guard_committed_lockfiles(component_path)?;
    super::lockfile_guard::guard_local_file_dependencies(component_path)
}

/// Whether the package-completeness structure assertion should run.
///
/// `--skip-build-validation` is documented to bypass build-structure
/// assertions in both `preflight.package` and `package`. Package-completeness
/// (every tracked runtime file present in the ZIP) is such a structure
/// assertion, so an explicit override must skip it rather than fail closed
/// (#8189).
pub(crate) fn should_validate_package_completeness(skip_build_validation: bool) -> bool {
    !skip_build_validation
}

/// Confirm that the durable artifacts produced by the final package step
/// include every tracked runtime file in the configured release scope.
pub(crate) fn validate_package_completeness(
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
        "package",
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
                    "Failed to inspect tracked files for package completeness: {}",
                    e
                ),
                Some(component_path.display().to_string()),
            )
        })?;
    if !output.status.success() {
        return Err(Error::internal_unexpected(format!(
            "Failed to inspect tracked files for package completeness: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    let scope = release_scope(component);
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|raw| !raw.is_empty())
        .filter_map(|raw| String::from_utf8(raw.to_vec()).ok())
        .map(|path| normalize_archive_path(&path))
        .filter(|path| runtime_candidate(path))
        .filter(|path| scope_allows(path, &scope))
        .collect())
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
        if !entry.is_dir() {
            entries.insert(normalize_archive_path(entry.name()));
        }
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
            scopes: Some(crate::component::ScopeConfig {
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

    #[test]
    fn skip_build_validation_bypasses_package_completeness() {
        // #8189: when the operator passes --skip-build-validation, the
        // package-completeness structure assertion must not run, matching the
        // documented CLI contract.
        assert!(!should_validate_package_completeness(true));
        // Default release behavior still enforces completeness.
        assert!(should_validate_package_completeness(false));
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
