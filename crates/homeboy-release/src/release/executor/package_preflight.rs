use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::release::types::ReleaseArtifact;
use homeboy_core::component::{
    CommandScopeConfig, Component, PackageCoverageArtifactMatch, PackageCoverageConfig,
};
use homeboy_core::error::{Error, Result};

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
    component.release.validate_package_coverage()?;
    let zip_artifacts: Vec<(&ReleaseArtifact, BTreeSet<String>)> = artifacts
        .iter()
        .filter_map(|artifact| {
            let artifact_path = resolve_artifact_path(component_path, artifact);
            artifact_path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
                .then(|| read_zip_entries(&artifact_path).map(|entries| (artifact, entries)))
        })
        .collect::<Result<_>>()?;

    if zip_artifacts.is_empty() {
        return Ok(());
    }

    for coverage in &component.release.package_coverage {
        let matches: Vec<_> = zip_artifacts
            .iter()
            .filter(|(artifact, _)| artifact_matches(coverage, artifact))
            .collect();
        if matches.len() != 1 {
            return Err(Error::validation_invalid_argument(
                "release.package_coverage",
                format!(
                    "Package coverage artifact selector '{}' matched {} ZIP artifacts; it must match exactly one",
                    coverage.artifact,
                    matches.len()
                ),
                None,
                None,
            ));
        }
    }

    // Selector ambiguity is invalid configuration independent of repository
    // contents, so reject it before evaluating mapped source roots.
    for (artifact, _) in &zip_artifacts {
        let mappings = component
            .release
            .package_coverage
            .iter()
            .filter(|coverage| artifact_matches(coverage, artifact))
            .count();
        if mappings > 1 {
            return Err(Error::validation_invalid_argument(
                "release.package_coverage",
                format!(
                    "ZIP artifact '{}' matches multiple package coverage declarations",
                    artifact.path
                ),
                None,
                None,
            ));
        }
    }

    let expected = tracked_runtime_files(component, component_path)?;
    for coverage in &component.release.package_coverage {
        for root in &coverage.source_roots {
            if !expected
                .iter()
                .any(|path| source_root_suffix(path, root).is_some())
            {
                return Err(Error::validation_invalid_argument(
                    "release.package_coverage",
                    format!(
                        "Package coverage source root '{}' matches no tracked runtime files",
                        root
                    ),
                    None,
                    None,
                ));
            }
        }
    }
    if expected.is_empty() {
        return Ok(());
    }

    for (artifact, entries) in zip_artifacts {
        let mappings: Vec<&PackageCoverageConfig> = component
            .release
            .package_coverage
            .iter()
            .filter(|coverage| artifact_matches(coverage, artifact))
            .collect();
        debug_assert!(mappings.len() <= 1);
        // Preserve the shipped identity-layout behavior: an empty archive does
        // not assert coverage unless the component explicitly mapped it.
        if entries.is_empty() && mappings.is_empty() {
            continue;
        }

        let (expected_paths, strict_archive_paths) = match mappings.first() {
            Some(coverage) => (mapped_runtime_files(&expected, coverage)?, true),
            None => (expected.clone(), false),
        };
        let missing: Vec<String> = expected_paths
            .into_iter()
            .filter(|path| !archive_contains_path(&entries, path, strict_archive_paths))
            .collect();
        if !missing.is_empty() {
            return package_completeness_error(component_path, artifact, missing);
        }
    }

    Ok(())
}

fn artifact_matches(coverage: &PackageCoverageConfig, artifact: &ReleaseArtifact) -> bool {
    let path = normalize_archive_path(&artifact.path);
    match coverage.artifact_match {
        PackageCoverageArtifactMatch::Exact => path == coverage.artifact,
        PackageCoverageArtifactMatch::Glob => glob_match::glob_match(&coverage.artifact, &path),
    }
}

fn mapped_runtime_files(
    expected: &[String],
    coverage: &PackageCoverageConfig,
) -> Result<Vec<String>> {
    let mut archive_paths = std::collections::BTreeMap::new();
    for root in &coverage.source_roots {
        for source_path in expected {
            let Some(suffix) = source_root_suffix(source_path, root) else {
                continue;
            };
            let archive_path = if coverage.archive_root == "." {
                suffix.to_string()
            } else {
                format!("{}/{}", coverage.archive_root, suffix)
            };
            if let Some(existing) = archive_paths.insert(archive_path.clone(), source_path.clone())
            {
                return Err(Error::validation_invalid_argument(
                    "release.package_coverage",
                    format!(
                        "Package coverage maps source files '{}' and '{}' to the same archive path '{}'",
                        existing, source_path, archive_path
                    ),
                    None,
                    None,
                ));
            }
        }
    }
    Ok(archive_paths.into_keys().collect())
}

fn source_root_suffix<'a>(path: &'a str, root: &str) -> Option<&'a str> {
    if root == "." {
        Some(path)
    } else {
        path.strip_prefix(root)
            .and_then(|suffix| suffix.strip_prefix('/'))
    }
}

fn package_completeness_error(
    component_path: &Path,
    artifact: &ReleaseArtifact,
    missing: Vec<String>,
) -> Result<()> {
    Err(Error::validation_invalid_argument(
        "package",
        format!(
            "Release package '{}' is missing {} tracked runtime file(s): {}",
            artifact.path,
            missing.len(),
            missing.iter().take(20).cloned().collect::<Vec<_>>().join(", ")
        ),
        Some(serde_json::json!({
            "missing": missing,
            "source": component_path,
            "artifact": artifact.path,
            "checked_artifact_type": "zip"
        }).to_string()),
        Some(vec![
            "Update the release.package action so this artifact includes every mapped tracked runtime file, or add an explicit release scope exclude for intentional omissions.".to_string(),
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

fn archive_contains_path(entries: &BTreeSet<String>, path: &str, strict: bool) -> bool {
    entries
        .iter()
        .any(|entry| entry == path || (!strict && entry.ends_with(&format!("/{}", path))))
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
            scopes: Some(homeboy_core::component::ScopeConfig {
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
    fn package_completeness_validates_transformed_source_root_and_direct_config() {
        let repo = tempfile::tempdir().expect("repo");
        std::fs::create_dir_all(repo.path().join("source/runtime")).expect("runtime dir");
        std::fs::write(repo.path().join("source/runtime/main.php"), "<?php\n").expect("main");
        run_git(repo.path(), &["init"]);
        run_git(repo.path(), &["add", "source/runtime/main.php"]);
        let artifact_path = repo.path().join("build/runtime.zip");
        std::fs::create_dir_all(artifact_path.parent().unwrap()).expect("build dir");
        write_zip(&artifact_path, &[("bundle/main.php", "<?php\n")]);
        let mut component = test_component(repo.path());
        component.release.package_coverage = vec![PackageCoverageConfig {
            artifact: "build/runtime.zip".to_string(),
            artifact_match: PackageCoverageArtifactMatch::Exact,
            source_roots: vec!["source/runtime".to_string()],
            archive_root: "bundle".to_string(),
        }];

        validate_package_completeness(&component, repo.path(), &zip_artifacts("build/runtime.zip"))
            .expect("mapped runtime file should be present at its archive path");

        component.release.package_coverage[0].archive_root = "../bundle".to_string();
        write_zip(&artifact_path, &[("../bundle/main.php", "<?php\n")]);
        let error = validate_package_completeness(
            &component,
            repo.path(),
            &zip_artifacts("build/runtime.zip"),
        )
        .expect_err("direct traversal must fail before inspecting archive entries");
        assert_eq!(error.code.as_str(), "validation.invalid_argument");
        assert!(error.message.contains("parent traversal"));
    }

    #[test]
    fn package_completeness_rejects_missing_transformed_source_file() {
        let repo = tempfile::tempdir().expect("repo");
        std::fs::create_dir_all(repo.path().join("source/runtime")).expect("runtime dir");
        std::fs::write(repo.path().join("source/runtime/main.php"), "<?php\n").expect("main");
        std::fs::write(repo.path().join("source/runtime/extra.php"), "<?php\n").expect("extra");
        run_git(repo.path(), &["init"]);
        run_git(repo.path(), &["add", "source/runtime"]);
        let artifact_path = repo.path().join("build/runtime.zip");
        std::fs::create_dir_all(artifact_path.parent().unwrap()).expect("build dir");
        write_zip(&artifact_path, &[("bundle/main.php", "<?php\n")]);
        let mut component = test_component(repo.path());
        component.release.package_coverage = vec![PackageCoverageConfig {
            artifact: "build/runtime.zip".to_string(),
            artifact_match: PackageCoverageArtifactMatch::Exact,
            source_roots: vec!["source/runtime".to_string()],
            archive_root: "bundle".to_string(),
        }];

        let error = validate_package_completeness(
            &component,
            repo.path(),
            &zip_artifacts("build/runtime.zip"),
        )
        .expect_err("missing mapped runtime file should fail");
        assert!(error.message.contains("bundle/extra.php"));
    }

    #[test]
    fn package_completeness_does_not_union_zip_entries() {
        let repo = tempfile::tempdir().expect("repo");
        std::fs::write(repo.path().join("first.php"), "<?php\n").expect("first");
        std::fs::write(repo.path().join("second.php"), "<?php\n").expect("second");
        run_git(repo.path(), &["init"]);
        run_git(repo.path(), &["add", "first.php", "second.php"]);
        let first_zip = repo.path().join("build/first.zip");
        let second_zip = repo.path().join("build/second.zip");
        std::fs::create_dir_all(first_zip.parent().unwrap()).expect("build dir");
        write_zip(&first_zip, &[("first.php", "<?php\n")]);
        write_zip(&second_zip, &[("second.php", "<?php\n")]);
        let mut first_artifacts = zip_artifacts("build/first.zip");
        let mut second_artifacts = zip_artifacts("build/second.zip");

        let error = validate_package_completeness(
            &test_component(repo.path()),
            repo.path(),
            &[first_artifacts.remove(0), second_artifacts.remove(0)],
        )
        .expect_err("each ZIP must cover its own runtime files");
        assert!(error.message.contains("build/first.zip"));
        assert!(error.message.contains("second.php"));
    }

    #[test]
    fn package_completeness_preserves_identity_layout_success_and_failure() {
        let repo = tempfile::tempdir().expect("repo");
        std::fs::write(repo.path().join("runtime.php"), "<?php\n").expect("runtime");
        run_git(repo.path(), &["init"]);
        run_git(repo.path(), &["add", "runtime.php"]);
        let artifact_path = repo.path().join("build/package.zip");
        std::fs::create_dir_all(artifact_path.parent().unwrap()).expect("build dir");
        write_zip(&artifact_path, &[("wrapped/runtime.php", "<?php\n")]);
        validate_package_completeness(
            &test_component(repo.path()),
            repo.path(),
            &zip_artifacts("build/package.zip"),
        )
        .expect("unmapped identity archive should retain wrapped-root support");

        write_zip(&artifact_path, &[("other.php", "<?php\n")]);
        let error = validate_package_completeness(
            &test_component(repo.path()),
            repo.path(),
            &zip_artifacts("build/package.zip"),
        )
        .expect_err("identity archive missing a tracked runtime file should fail");
        assert!(error.message.contains("runtime.php"));
    }

    #[test]
    fn package_completeness_rejects_zero_match_source_root() {
        let repo = tempfile::tempdir().expect("repo");
        std::fs::write(repo.path().join("runtime.php"), "<?php\n").expect("runtime");
        run_git(repo.path(), &["init"]);
        run_git(repo.path(), &["add", "runtime.php"]);
        let artifact_path = repo.path().join("build/package.zip");
        std::fs::create_dir_all(artifact_path.parent().unwrap()).expect("build dir");
        write_zip(&artifact_path, &[("bundle/runtime.php", "<?php\n")]);
        let mut component = test_component(repo.path());
        component.release.package_coverage = vec![PackageCoverageConfig {
            artifact: "build/package.zip".to_string(),
            artifact_match: PackageCoverageArtifactMatch::Exact,
            source_roots: vec!["missing".to_string()],
            archive_root: "bundle".to_string(),
        }];

        let error = validate_package_completeness(
            &component,
            repo.path(),
            &zip_artifacts("build/package.zip"),
        )
        .expect_err("unmatched source roots must fail closed");
        assert!(error.message.contains("matches no tracked runtime files"));
    }

    #[test]
    fn package_completeness_rejects_archive_path_collisions() {
        let repo = tempfile::tempdir().expect("repo");
        for root in ["one", "two"] {
            std::fs::create_dir_all(repo.path().join(root)).expect("source dir");
            std::fs::write(repo.path().join(root).join("main.php"), "<?php\n").expect("runtime");
        }
        run_git(repo.path(), &["init"]);
        run_git(repo.path(), &["add", "one", "two"]);
        let artifact_path = repo.path().join("build/package.zip");
        std::fs::create_dir_all(artifact_path.parent().unwrap()).expect("build dir");
        write_zip(&artifact_path, &[("bundle/main.php", "<?php\n")]);
        let mut component = test_component(repo.path());
        component.release.package_coverage = vec![PackageCoverageConfig {
            artifact: "build/package.zip".to_string(),
            artifact_match: PackageCoverageArtifactMatch::Exact,
            source_roots: vec!["one".to_string(), "two".to_string()],
            archive_root: "bundle".to_string(),
        }];

        let error = validate_package_completeness(
            &component,
            repo.path(),
            &zip_artifacts("build/package.zip"),
        )
        .expect_err("colliding mapped archive paths must fail");
        assert!(error.message.contains("one/main.php"));
        assert!(error.message.contains("two/main.php"));
        assert!(error.message.contains("bundle/main.php"));
    }

    #[test]
    fn package_completeness_rejects_overlapping_selectors_without_runtime_candidates() {
        let repo = tempfile::tempdir().expect("repo");
        run_git(repo.path(), &["init"]);
        let artifact_path = repo.path().join("build/package.zip");
        std::fs::create_dir_all(artifact_path.parent().unwrap()).expect("build dir");
        write_zip(&artifact_path, &[]);
        let mut component = test_component(repo.path());
        component.release.package_coverage = vec![
            PackageCoverageConfig {
                artifact: "build/package.zip".to_string(),
                artifact_match: PackageCoverageArtifactMatch::Exact,
                source_roots: vec![".".to_string()],
                archive_root: "bundle".to_string(),
            },
            PackageCoverageConfig {
                artifact: "build/*.zip".to_string(),
                artifact_match: PackageCoverageArtifactMatch::Glob,
                source_roots: vec![".".to_string()],
                archive_root: "bundle".to_string(),
            },
        ];

        let error = validate_package_completeness(
            &component,
            repo.path(),
            &zip_artifacts("build/package.zip"),
        )
        .expect_err("overlapping selectors must fail before runtime coverage is evaluated");
        assert_eq!(error.code.as_str(), "validation.invalid_argument");
        assert!(error
            .message
            .contains("matches multiple package coverage declarations"));
    }

    #[test]
    fn package_completeness_rejects_empty_mapped_zip() {
        let repo = tempfile::tempdir().expect("repo");
        std::fs::create_dir_all(repo.path().join("source")).expect("source dir");
        std::fs::write(repo.path().join("source/runtime.php"), "<?php\n").expect("runtime");
        run_git(repo.path(), &["init"]);
        run_git(repo.path(), &["add", "source/runtime.php"]);
        let artifact_path = repo.path().join("build/package.zip");
        std::fs::create_dir_all(artifact_path.parent().unwrap()).expect("build dir");
        write_zip(&artifact_path, &[]);
        let mut component = test_component(repo.path());
        component.release.package_coverage = vec![PackageCoverageConfig {
            artifact: "build/package.zip".to_string(),
            artifact_match: PackageCoverageArtifactMatch::Exact,
            source_roots: vec!["source".to_string()],
            archive_root: "bundle".to_string(),
        }];

        let error = validate_package_completeness(
            &component,
            repo.path(),
            &zip_artifacts("build/package.zip"),
        )
        .expect_err("explicit mappings make an empty ZIP incomplete");
        assert!(error.message.contains("bundle/runtime.php"));
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

    fn test_component(repo: &Path) -> Component {
        Component {
            id: "package".to_string(),
            local_path: repo.to_string_lossy().to_string(),
            ..Component::default()
        }
    }

    fn zip_artifacts(path: &str) -> Vec<ReleaseArtifact> {
        vec![ReleaseArtifact {
            path: path.to_string(),
            durable_path: None,
            artifact_type: Some("archive".to_string()),
            platform: None,
        }]
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
