use homeboy_core::error::{Error, Result};
use homeboy_engine_primitives::content_hash;
use serde::Deserialize;
use std::collections::BTreeMap;

use super::{step_success, ReleaseArtifact, ReleaseState, ReleaseStepResult};

const PACKAGE_RECOVERY_MANIFEST: &str = "manifest.json";
pub(crate) const PACKAGE_RECOVERY_MANIFEST_SCHEMA: &str = "homeboy.package-recovery";
pub(crate) const PACKAGE_RECOVERY_MANIFEST_SCHEMA_VERSION: u32 = 1;

#[derive(Deserialize)]
struct PackageRecoveryManifest {
    schema: String,
    schema_version: u32,
    component_id: String,
    tag: String,
    version: String,
    commit: String,
    artifacts: Vec<ReleaseArtifact>,
}

pub(crate) struct PackageRecoveryContext<'a> {
    pub(crate) component_id: &'a str,
    pub(crate) tag: &'a str,
    pub(crate) version: &'a str,
    pub(crate) commit: &'a str,
}

/// Inventory artifacts that were already built by an external release build.
/// This lets `homeboy release --head --from-artifacts <dir>` reuse the normal
/// github.release and publish steps without re-running release.package.
pub(crate) fn run_artifact_inventory(
    state: &mut ReleaseState,
    artifact_dir: &str,
    recovery_context: &PackageRecoveryContext,
) -> Result<ReleaseStepResult> {
    let dir = std::path::Path::new(artifact_dir);
    if !dir.is_dir() {
        return Err(Error::validation_invalid_argument(
            "from-artifacts",
            format!("Artifact directory '{}' does not exist", artifact_dir),
            Some(artifact_dir.to_string()),
            None,
        ));
    }

    let manifest_path = dir.join(PACKAGE_RECOVERY_MANIFEST);
    let mut artifacts = if manifest_path.is_file() && is_package_recovery_manifest(&manifest_path) {
        inventory_package_recovery_manifest(dir, &manifest_path, recovery_context)?
    } else {
        inventory_directory_files(dir, artifact_dir)?
    };

    artifacts.sort_by(|a, b| a.path.cmp(&b.path));
    if artifacts.is_empty() {
        return Err(Error::validation_invalid_argument(
            "from-artifacts",
            format!("Artifact directory '{}' contains no files", artifact_dir),
            Some(artifact_dir.to_string()),
            None,
        ));
    }

    let artifact_count = artifacts.len();
    state.artifacts.extend(artifacts);
    let artifacts = state.artifacts.clone();
    let data = serde_json::json!({
        "dir": artifact_dir,
        "artifact_count": artifact_count,
        "artifacts": artifacts,
    });

    Ok(step_success(
        "artifacts.inventory",
        "artifacts.inventory",
        Some(data),
        Vec::new(),
    ))
}

/// Select one generic publication authority for every remote target filename.
/// Final package output supersedes preflight output. Within one phase, a
/// previously declared canonical recovery artifact wins, followed by an
/// explicit component `scripts.build` artifact; identical lower-precedence
/// duplicates collapse, while differing bytes fail before commit/tag/push.
pub(crate) fn establish_publication_authority(state: &mut ReleaseState) -> Result<Vec<String>> {
    let mut selected: BTreeMap<String, usize> = BTreeMap::new();
    let mut diagnostics = Vec::new();
    for artifact in &mut state.artifacts {
        let path = artifact
            .durable_path
            .as_deref()
            .filter(|path| std::path::Path::new(path).is_file())
            .unwrap_or(&artifact.path);
        artifact.sha256 = Some(sha256_file(path)?);
    }
    for index in 0..state.artifacts.len() {
        let target = artifact_target_name(&state.artifacts[index])?;
        let Some(existing_index) = selected.get(&target).copied() else {
            selected.insert(target, index);
            continue;
        };
        let existing = &state.artifacts[existing_index];
        let candidate = &state.artifacts[index];
        let same_bytes = existing.sha256 == candidate.sha256;
        let existing_final = existing.phase == "final";
        let candidate_final = candidate.phase == "final";
        if same_bytes {
            if artifact_precedence(candidate) > artifact_precedence(existing) {
                selected.insert(target, index);
            }
            continue;
        }
        if !existing_final && candidate_final {
            diagnostics.push(format!(
                "Release asset '{}' from {} ({}) is non-authoritative; final package output from {} ({}) supersedes its sha256 {} with {}",
                target, existing.producer, existing.phase, candidate.producer, candidate.phase,
                existing.sha256.as_deref().unwrap_or_default(), candidate.sha256.as_deref().unwrap_or_default()
            ));
            selected.insert(target, index);
        } else if existing_final && !candidate_final {
            diagnostics.push(format!(
                "Release asset '{}' from {} ({}) is non-authoritative; final package output from {} ({}) supersedes its sha256 {} with {}",
                target, candidate.producer, candidate.phase, existing.producer, existing.phase,
                candidate.sha256.as_deref().unwrap_or_default(), existing.sha256.as_deref().unwrap_or_default()
            ));
        } else {
            return Err(Error::validation_invalid_argument(
                "release assets",
                format!("release assets targeting '{}' have conflicting authoritative bytes from {} and {}", target, existing.producer, candidate.producer),
                None,
                None,
            ));
        }
    }
    for artifact in &mut state.artifacts {
        artifact.publication_authority = false;
    }
    for index in selected.into_values() {
        state.artifacts[index].publication_authority = true;
    }
    Ok(diagnostics)
}

fn artifact_precedence(artifact: &ReleaseArtifact) -> (u8, u8, u8) {
    (
        u8::from(artifact.phase == "final"),
        u8::from(artifact.publication_authority),
        // `scripts.build` is the generic component-owned build_artifact
        // contract. Extensions remain free to produce additional targets.
        u8::from(artifact.producer == "scripts.build"),
    )
}

fn artifact_target_name(artifact: &ReleaseArtifact) -> Result<String> {
    std::path::Path::new(&artifact.path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "release assets",
                format!("release artifact '{}' has no valid filename", artifact.path),
                None,
                None,
            )
        })
}

fn sha256_file(path: &str) -> Result<String> {
    content_hash::sha256_file(std::path::Path::new(path)).map_err(|error| {
        Error::internal_io(
            format!("Failed to hash release artifact '{}': {}", path, error),
            Some(path.to_string()),
        )
    })
}

fn is_package_recovery_manifest(manifest_path: &std::path::Path) -> bool {
    let Ok(contents) = std::fs::read_to_string(manifest_path) else {
        return false;
    };
    let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&contents) else {
        return false;
    };
    manifest.get("schema").and_then(serde_json::Value::as_str)
        == Some(PACKAGE_RECOVERY_MANIFEST_SCHEMA)
}

fn inventory_directory_files(
    dir: &std::path::Path,
    artifact_dir: &str,
) -> Result<Vec<ReleaseArtifact>> {
    let mut artifacts = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| {
        Error::internal_io(
            format!(
                "Failed to read artifact directory '{}': {}",
                artifact_dir, e
            ),
            Some(artifact_dir.to_string()),
        )
    })? {
        let entry = entry.map_err(|e| {
            Error::internal_io(
                format!("Failed to read artifact directory entry: {}", e),
                Some(artifact_dir.to_string()),
            )
        })?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let canonical = std::fs::canonicalize(&path).map_err(|e| {
            Error::internal_io(
                format!(
                    "Failed to resolve artifact path '{}': {}",
                    path.display(),
                    e
                ),
                Some(path.display().to_string()),
            )
        })?;
        artifacts.push(ReleaseArtifact {
            path: canonical.display().to_string(),
            durable_path: None,
            artifact_type: None,
            platform: None,
            phase: "recovery".to_string(),
            producer: "from-artifacts".to_string(),
            sha256: None,
            publication_authority: false,
        });
    }
    Ok(artifacts)
}

fn inventory_package_recovery_manifest(
    artifact_dir: &std::path::Path,
    manifest_path: &std::path::Path,
    recovery_context: &PackageRecoveryContext,
) -> Result<Vec<ReleaseArtifact>> {
    let artifact_dir = std::fs::canonicalize(artifact_dir).map_err(|error| {
        Error::internal_io(
            format!(
                "Failed to resolve release package artifact directory '{}': {}",
                artifact_dir.display(),
                error
            ),
            Some(artifact_dir.display().to_string()),
        )
    })?;
    let manifest = std::fs::read_to_string(manifest_path).map_err(|error| {
        Error::internal_io(
            format!(
                "Failed to read release package manifest '{}': {}",
                manifest_path.display(),
                error
            ),
            Some(manifest_path.display().to_string()),
        )
    })?;
    let result: PackageRecoveryManifest = serde_json::from_str(&manifest).map_err(|error| {
        Error::validation_invalid_argument(
            "from-artifacts",
            format!(
                "Release package manifest '{}' is invalid: {}",
                manifest_path.display(),
                error
            ),
            Some(manifest_path.display().to_string()),
            None,
        )
    })?;
    if result.schema != PACKAGE_RECOVERY_MANIFEST_SCHEMA {
        return Err(Error::validation_invalid_argument(
            "from-artifacts",
            format!(
                "Release package manifest '{}' has unsupported schema '{}'",
                manifest_path.display(),
                result.schema
            ),
            Some(manifest_path.display().to_string()),
            None,
        ));
    }
    if result.schema_version != PACKAGE_RECOVERY_MANIFEST_SCHEMA_VERSION {
        return Err(Error::validation_invalid_argument(
            "from-artifacts",
            format!(
                "Release package manifest '{}' has unsupported schema version {}",
                manifest_path.display(),
                result.schema_version
            ),
            Some(manifest_path.display().to_string()),
            None,
        ));
    }
    if [
        result.component_id.as_str(),
        result.tag.as_str(),
        result.version.as_str(),
        result.commit.as_str(),
    ]
    .iter()
    .any(|value| value.trim().is_empty())
    {
        return Err(Error::validation_invalid_argument(
            "from-artifacts",
            format!(
                "Release package manifest '{}' has incomplete release identity",
                manifest_path.display()
            ),
            Some(manifest_path.display().to_string()),
            None,
        ));
    }
    validate_recovery_identity(&result, manifest_path, recovery_context)?;
    if result.artifacts.is_empty() {
        return Err(Error::validation_invalid_argument(
            "from-artifacts",
            format!(
                "Release package manifest '{}' contains no release assets",
                manifest_path.display()
            ),
            Some(manifest_path.display().to_string()),
            None,
        ));
    }

    result
        .artifacts
        .into_iter()
        .map(|artifact| validate_recovery_artifact(&artifact_dir, artifact))
        .collect()
}

fn validate_recovery_identity(
    manifest: &PackageRecoveryManifest,
    manifest_path: &std::path::Path,
    context: &PackageRecoveryContext,
) -> Result<()> {
    for (field, actual, expected) in [
        (
            "component_id",
            manifest.component_id.as_str(),
            context.component_id,
        ),
        ("tag", manifest.tag.as_str(), context.tag),
        ("version", manifest.version.as_str(), context.version),
        ("commit", manifest.commit.as_str(), context.commit),
    ] {
        if actual != expected {
            return Err(Error::validation_invalid_argument(
                "from-artifacts",
                format!(
                    "Release package manifest '{}' {} '{}' does not match active release {} '{}'",
                    manifest_path.display(),
                    field,
                    actual,
                    field,
                    expected
                ),
                Some(manifest_path.display().to_string()),
                None,
            ));
        }
    }

    Ok(())
}

fn validate_recovery_artifact(
    artifact_dir: &std::path::Path,
    mut artifact: ReleaseArtifact,
) -> Result<ReleaseArtifact> {
    let path = std::path::Path::new(&artifact.path);
    let canonical = std::fs::canonicalize(path).map_err(|error| {
        Error::validation_invalid_argument(
            "from-artifacts",
            format!(
                "Recovered release asset '{}' is missing: {}",
                path.display(),
                error
            ),
            Some(path.display().to_string()),
            None,
        )
    })?;
    if !canonical.is_file() || !canonical.starts_with(artifact_dir) {
        return Err(Error::validation_invalid_argument(
            "from-artifacts",
            format!(
                "Recovered release asset '{}' must be a file inside '{}'",
                canonical.display(),
                artifact_dir.display()
            ),
            Some(canonical.display().to_string()),
            None,
        ));
    }
    artifact.path = canonical.display().to_string();
    artifact.durable_path = Some(artifact.path.clone());
    Ok(artifact)
}

#[cfg(test)]
mod tests {
    use super::{
        establish_publication_authority, run_artifact_inventory, PackageRecoveryContext,
        PACKAGE_RECOVERY_MANIFEST, PACKAGE_RECOVERY_MANIFEST_SCHEMA,
        PACKAGE_RECOVERY_MANIFEST_SCHEMA_VERSION,
    };
    use crate::release::executor::github_release::github_release_publications;
    use crate::release::types::ReleaseArtifact;
    use crate::release::types::ReleaseState;
    use crate::release::ReleaseStepStatus;

    #[test]
    fn test_run_artifact_inventory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let artifact_path = temp.path().join("homeboy.tar.gz");
        std::fs::write(&artifact_path, "artifact").expect("write artifact");
        std::fs::create_dir(temp.path().join("nested")).expect("nested dir");

        let mut state = ReleaseState::default();
        let result = run_artifact_inventory(
            &mut state,
            &temp.path().to_string_lossy(),
            &recovery_context(),
        )
        .expect("inventory should succeed");

        assert_eq!(result.status, ReleaseStepStatus::Success);
        assert_eq!(state.artifacts.len(), 1);
        assert_eq!(
            state.artifacts[0].path,
            std::fs::canonicalize(&artifact_path)
                .expect("canonical artifact")
                .display()
                .to_string()
        );
    }

    #[test]
    fn recovery_manifest_inventories_npm_tarball_and_wordpress_zip() {
        let temp = tempfile::tempdir().expect("tempdir");
        let npm = temp.path().join("plugin-1.2.3.tgz");
        let wordpress = temp.path().join("plugin.zip");
        std::fs::write(&npm, "npm").expect("npm artifact");
        std::fs::write(&wordpress, "wordpress").expect("wordpress artifact");
        std::fs::write(
            temp.path().join(PACKAGE_RECOVERY_MANIFEST),
            serde_json::json!({
                "schema": PACKAGE_RECOVERY_MANIFEST_SCHEMA,
                "schema_version": PACKAGE_RECOVERY_MANIFEST_SCHEMA_VERSION,
                "component_id": "plugin",
                "tag": "v1.2.3",
                "version": "1.2.3",
                "commit": "abc123",
                "artifacts": [
                    {
                        "path": npm,
                        "durable_path": npm,
                        "artifact_type": "npm"
                    },
                    {
                        "path": wordpress,
                        "durable_path": wordpress,
                        "artifact_type": "archive"
                    }
                ]
            })
            .to_string(),
        )
        .expect("manifest");

        let mut state = ReleaseState::default();
        run_artifact_inventory(
            &mut state,
            &temp.path().to_string_lossy(),
            &recovery_context(),
        )
        .expect("recovery inventory");
        assert_eq!(state.artifacts.len(), 2);
        assert_eq!(state.artifacts[0].artifact_type.as_deref(), Some("npm"));
        assert_eq!(state.artifacts[1].artifact_type.as_deref(), Some("archive"));

        std::fs::remove_file(&wordpress).expect("remove required artifact");
        let error = run_artifact_inventory(
            &mut ReleaseState::default(),
            &temp.path().to_string_lossy(),
            &recovery_context(),
        )
        .expect_err("incomplete recovery inventory must fail closed");
        assert!(error.message.contains("Recovered release asset"));
    }

    #[test]
    fn recovery_manifest_rejects_identity_mismatches_and_incomplete_schema() {
        for (field, value, expected) in [
            ("component_id", "other-plugin", "component_id"),
            ("tag", "v9.9.9", "tag"),
            ("version", "9.9.9", "version"),
            ("commit", "different", "commit"),
        ] {
            let temp = tempfile::tempdir().expect("tempdir");
            let artifact = temp.path().join("plugin-1.2.3.tgz");
            std::fs::write(&artifact, "npm").expect("npm artifact");
            let mut manifest = recovery_manifest(&artifact);
            manifest[field] = serde_json::Value::String(value.to_string());
            std::fs::write(
                temp.path().join(PACKAGE_RECOVERY_MANIFEST),
                manifest.to_string(),
            )
            .expect("manifest");

            let error = run_artifact_inventory(
                &mut ReleaseState::default(),
                &temp.path().to_string_lossy(),
                &recovery_context(),
            )
            .expect_err("identity mismatch must fail closed");
            assert!(error.message.contains(expected));
        }

        let temp = tempfile::tempdir().expect("tempdir");
        let artifact = temp.path().join("plugin-1.2.3.tgz");
        std::fs::write(&artifact, "npm").expect("npm artifact");
        let mut manifest = recovery_manifest(&artifact);
        manifest.as_object_mut().expect("object").remove("commit");
        std::fs::write(
            temp.path().join(PACKAGE_RECOVERY_MANIFEST),
            manifest.to_string(),
        )
        .expect("manifest");

        let error = run_artifact_inventory(
            &mut ReleaseState::default(),
            &temp.path().to_string_lossy(),
            &recovery_context(),
        )
        .expect_err("incomplete recovery manifest must fail closed");
        assert!(error.message.contains("invalid"));

        let temp = tempfile::tempdir().expect("tempdir");
        let artifact = temp.path().join("plugin-1.2.3.tgz");
        std::fs::write(&artifact, "npm").expect("npm artifact");
        let mut manifest = recovery_manifest(&artifact);
        manifest["schema_version"] = serde_json::Value::String("one".to_string());
        std::fs::write(
            temp.path().join(PACKAGE_RECOVERY_MANIFEST),
            manifest.to_string(),
        )
        .expect("manifest");

        let error = run_artifact_inventory(
            &mut ReleaseState::default(),
            &temp.path().to_string_lossy(),
            &recovery_context(),
        )
        .expect_err("malformed recovery manifest must fail closed");
        assert!(error.message.contains("invalid"));
    }

    #[test]
    fn ordinary_manifest_json_remains_an_external_artifact() {
        let temp = tempfile::tempdir().expect("tempdir");
        let manifest = temp.path().join(PACKAGE_RECOVERY_MANIFEST);
        std::fs::write(&manifest, r#"{"component_id":"unrelated"}"#).expect("manifest");

        let mut state = ReleaseState::default();
        run_artifact_inventory(
            &mut state,
            &temp.path().to_string_lossy(),
            &recovery_context(),
        )
        .expect("ordinary external manifest inventory");
        assert_eq!(state.artifacts.len(), 1);
        assert_eq!(
            state.artifacts[0].path,
            std::fs::canonicalize(manifest)
                .unwrap()
                .display()
                .to_string()
        );
    }

    fn recovery_context() -> PackageRecoveryContext<'static> {
        PackageRecoveryContext {
            component_id: "plugin",
            tag: "v1.2.3",
            version: "1.2.3",
            commit: "abc123",
        }
    }

    fn recovery_manifest(artifact: &std::path::Path) -> serde_json::Value {
        serde_json::json!({
            "schema": PACKAGE_RECOVERY_MANIFEST_SCHEMA,
            "schema_version": PACKAGE_RECOVERY_MANIFEST_SCHEMA_VERSION,
            "component_id": "plugin",
            "tag": "v1.2.3",
            "version": "1.2.3",
            "commit": "abc123",
            "artifacts": [{ "path": artifact }]
        })
    }

    #[test]
    fn final_package_authority_supersedes_different_preflight_bytes_and_publishes_once() {
        let temp = tempfile::tempdir().expect("tempdir");
        let preflight_dir = temp.path().join("preflight");
        let final_dir = temp.path().join("final");
        std::fs::create_dir_all(&preflight_dir).expect("preflight dir");
        std::fs::create_dir_all(&final_dir).expect("final dir");
        let preflight = preflight_dir.join("component.asset");
        let final_package = final_dir.join("component.asset");
        std::fs::write(&preflight, b"validation bytes").expect("preflight bytes");
        std::fs::write(&final_package, b"production bytes").expect("final bytes");
        let mut state = ReleaseState {
            artifacts: vec![
                artifact(&preflight, "preflight", "validation"),
                artifact(&final_package, "final", "package"),
            ],
            ..ReleaseState::default()
        };

        let diagnostics = establish_publication_authority(&mut state).expect("authority");
        let publications = github_release_publications(&state).expect("publications");

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            state
                .artifacts
                .iter()
                .filter(|artifact| artifact.publication_authority)
                .count(),
            1
        );
        assert!(state.artifacts[1].publication_authority);
        assert_eq!(publications.len(), 1);
        assert_eq!(
            std::fs::read(&publications[0].source_path).expect("published bytes"),
            b"production bytes"
        );
    }

    #[test]
    fn authority_rejects_conflicting_final_bytes_before_tagging() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first_dir = temp.path().join("first");
        let second_dir = temp.path().join("second");
        std::fs::create_dir_all(&first_dir).expect("first dir");
        std::fs::create_dir_all(&second_dir).expect("second dir");
        let first = first_dir.join("asset.bin");
        let second = second_dir.join("asset.bin");
        std::fs::write(&first, b"first").expect("first bytes");
        std::fs::write(&second, b"second").expect("second bytes");
        let mut state = ReleaseState {
            artifacts: vec![
                artifact(&first, "final", "one"),
                artifact(&second, "final", "two"),
            ],
            ..ReleaseState::default()
        };

        let error =
            establish_publication_authority(&mut state).expect_err("conflicting final assets");
        assert!(error.message.contains("conflicting authoritative bytes"));
    }

    #[test]
    fn authority_collapses_identical_duplicate_bytes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first_dir = temp.path().join("first");
        let second_dir = temp.path().join("second");
        std::fs::create_dir_all(&first_dir).expect("first dir");
        std::fs::create_dir_all(&second_dir).expect("second dir");
        let first = first_dir.join("asset.bin");
        let second = second_dir.join("asset.bin");
        std::fs::write(&first, b"same").expect("first bytes");
        std::fs::write(&second, b"same").expect("second bytes");
        let mut state = ReleaseState {
            artifacts: vec![
                artifact(&first, "preflight", "one"),
                artifact(&second, "final", "two"),
            ],
            ..ReleaseState::default()
        };

        establish_publication_authority(&mut state).expect("identical assets");
        assert_eq!(
            github_release_publications(&state)
                .expect("publications")
                .len(),
            1
        );
        assert!(state.artifacts[1].publication_authority);
    }

    #[test]
    fn identical_component_build_artifact_is_the_canonical_package_owner() {
        let temp = tempfile::tempdir().expect("tempdir");
        let extension_dir = temp.path().join("extension");
        let component_dir = temp.path().join("component");
        std::fs::create_dir_all(&extension_dir).expect("extension dir");
        std::fs::create_dir_all(&component_dir).expect("component dir");
        let extension = extension_dir.join("plugin.zip");
        let component = component_dir.join("plugin.zip");
        std::fs::write(&extension, b"same").expect("extension bytes");
        std::fs::write(&component, b"same").expect("component bytes");
        let mut state = ReleaseState {
            artifacts: vec![
                artifact(&extension, "final", "extension:wordpress"),
                artifact(&component, "final", "scripts.build"),
            ],
            ..ReleaseState::default()
        };

        establish_publication_authority(&mut state).expect("identical assets");
        let publications = github_release_publications(&state).expect("publications");

        assert!(state.artifacts[1].publication_authority);
        assert_eq!(publications.len(), 1);
        assert_eq!(
            std::fs::read(&publications[0].source_path).expect("canonical bytes"),
            b"same"
        );
    }

    #[test]
    fn recovery_preserves_the_declared_canonical_artifact_without_rebuild() {
        let temp = tempfile::tempdir().expect("tempdir");
        let canonical = temp.path().join("canonical/plugin.zip");
        let duplicate = temp.path().join("duplicate/plugin.zip");
        std::fs::create_dir_all(canonical.parent().unwrap()).expect("canonical dir");
        std::fs::create_dir_all(duplicate.parent().unwrap()).expect("duplicate dir");
        std::fs::write(&canonical, b"same").expect("canonical bytes");
        std::fs::write(&duplicate, b"same").expect("duplicate bytes");
        let mut selected = artifact(&canonical, "recovery", "from-artifacts");
        selected.publication_authority = true;
        let mut state = ReleaseState {
            artifacts: vec![selected, artifact(&duplicate, "recovery", "from-artifacts")],
            ..ReleaseState::default()
        };

        establish_publication_authority(&mut state).expect("recovery authority");
        let publications = github_release_publications(&state).expect("publications");

        assert!(state.artifacts[0].publication_authority);
        assert_eq!(publications.len(), 1);
        assert_eq!(
            std::fs::read(&publications[0].source_path).expect("canonical bytes"),
            b"same"
        );
    }

    #[test]
    fn authority_preserves_distinct_platform_assets() {
        let temp = tempfile::tempdir().expect("tempdir");
        let macos = temp.path().join("component-macos.zip");
        let linux = temp.path().join("component-linux.zip");
        std::fs::write(&macos, b"macos").expect("macos bytes");
        std::fs::write(&linux, b"linux").expect("linux bytes");
        let mut state = ReleaseState {
            artifacts: vec![
                artifact(&macos, "final", "package"),
                artifact(&linux, "final", "package"),
            ],
            ..ReleaseState::default()
        };

        establish_publication_authority(&mut state).expect("authority");
        let publications = github_release_publications(&state).expect("publications");

        assert_eq!(publications.len(), 2);
        assert!(state
            .artifacts
            .iter()
            .all(|artifact| artifact.publication_authority));
    }

    fn artifact(path: &std::path::Path, phase: &str, producer: &str) -> ReleaseArtifact {
        ReleaseArtifact {
            path: path.display().to_string(),
            durable_path: Some(path.display().to_string()),
            artifact_type: None,
            platform: None,
            phase: phase.to_string(),
            producer: producer.to_string(),
            sha256: None,
            publication_authority: false,
        }
    }
}
