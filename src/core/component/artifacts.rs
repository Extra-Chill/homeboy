use std::collections::BTreeMap;
use std::fs;
use std::path::{Component as PathComponent, Path, PathBuf};

use serde::Serialize;

use crate::core::component::{CleanupArtifactDeclaration, Component};
use crate::core::error::{Error, Result};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CleanupArtifactCandidate {
    pub label: String,
    pub source: String,
    pub relative_path: String,
    pub absolute_path: String,
    pub exists: bool,
    pub size_bytes: u64,
    pub applied: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CleanupArtifactReport {
    pub command: String,
    pub component_id: String,
    pub component_path: String,
    pub applied: bool,
    pub candidate_count: usize,
    pub applied_count: usize,
    pub reclaimable_bytes: u64,
    pub candidates: Vec<CleanupArtifactCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedDeclaration {
    label: String,
    source: String,
    relative_path: String,
}

pub fn cleanup_artifact_report(
    component: &Component,
    apply: bool,
) -> Result<CleanupArtifactReport> {
    let component_path = PathBuf::from(&component.local_path);
    let declarations = cleanup_artifact_declarations(component, &component_path)?;
    let mut candidates = Vec::new();
    let mut reclaimable_bytes = 0_u64;
    let mut applied_count = 0_usize;

    for declaration in declarations {
        let absolute_path = component_path.join(&declaration.relative_path);
        let exists = absolute_path.exists();
        let size_bytes = if exists {
            path_size(&absolute_path)?
        } else {
            0
        };
        let mut applied = false;
        let mut skipped_reason = None;

        if exists {
            reclaimable_bytes = reclaimable_bytes.saturating_add(size_bytes);
            if apply {
                remove_artifact_path(&absolute_path)?;
                applied = true;
                applied_count += 1;
            }
        } else {
            skipped_reason = Some("path does not exist".to_string());
        }

        candidates.push(CleanupArtifactCandidate {
            label: declaration.label,
            source: declaration.source,
            relative_path: declaration.relative_path,
            absolute_path: absolute_path.display().to_string(),
            exists,
            size_bytes,
            applied,
            skipped_reason,
        });
    }

    Ok(CleanupArtifactReport {
        command: "component.artifacts".to_string(),
        component_id: component.id.clone(),
        component_path: component.local_path.clone(),
        applied: apply,
        candidate_count: candidates.len(),
        applied_count,
        reclaimable_bytes,
        candidates,
    })
}

fn cleanup_artifact_declarations(
    component: &Component,
    component_path: &Path,
) -> Result<Vec<ResolvedDeclaration>> {
    let mut declarations = BTreeMap::<String, ResolvedDeclaration>::new();

    for artifact in &component.cleanup_artifacts {
        for declaration in resolve_component_declaration(artifact, component_path)? {
            declarations
                .entry(declaration.relative_path.clone())
                .or_insert(declaration);
        }
    }

    for declaration in inferred_cleanup_artifacts(component_path) {
        declarations
            .entry(declaration.relative_path.clone())
            .or_insert(declaration);
    }

    Ok(declarations.into_values().collect())
}

fn resolve_component_declaration(
    artifact: &CleanupArtifactDeclaration,
    component_path: &Path,
) -> Result<Vec<ResolvedDeclaration>> {
    match (&artifact.path, &artifact.glob) {
        (Some(path), None) => Ok(vec![ResolvedDeclaration {
            label: artifact.label.clone(),
            source: "component".to_string(),
            relative_path: normalize_relative_artifact_path(path)?,
        }]),
        (None, Some(pattern)) => resolve_glob_declaration(&artifact.label, pattern, component_path),
        (Some(_), Some(_)) => Err(Error::validation_invalid_argument(
            "cleanup_artifacts",
            "Cleanup artifact declarations must set either path or glob, not both",
            Some(artifact.label.clone()),
            None,
        )),
        (None, None) => Err(Error::validation_invalid_argument(
            "cleanup_artifacts",
            "Cleanup artifact declarations must set path or glob",
            Some(artifact.label.clone()),
            None,
        )),
    }
}

fn resolve_glob_declaration(
    label: &str,
    pattern: &str,
    component_path: &Path,
) -> Result<Vec<ResolvedDeclaration>> {
    let pattern = normalize_relative_artifact_path(pattern)?;
    let absolute_pattern = component_path.join(&pattern).display().to_string();
    let entries = glob::glob(&absolute_pattern).map_err(|error| {
        Error::validation_invalid_argument(
            "cleanup_artifacts.glob",
            "Cleanup artifact glob is invalid",
            Some(error.to_string()),
            None,
        )
    })?;

    let mut declarations = Vec::new();
    for entry in entries.flatten() {
        let Ok(relative) = entry.strip_prefix(component_path) else {
            continue;
        };
        declarations.push(ResolvedDeclaration {
            label: label.to_string(),
            source: "component".to_string(),
            relative_path: normalize_relative_artifact_path(&relative.to_string_lossy())?,
        });
    }

    if declarations.is_empty() {
        declarations.push(ResolvedDeclaration {
            label: label.to_string(),
            source: "component".to_string(),
            relative_path: pattern,
        });
    }

    Ok(declarations)
}

fn inferred_cleanup_artifacts(component_path: &Path) -> Vec<ResolvedDeclaration> {
    let mut declarations = Vec::new();
    if component_path.join("Cargo.toml").is_file() {
        declarations.push(inferred("Rust build output", "target"));
    }
    if component_path.join("package.json").is_file() {
        declarations.push(inferred("Node dependencies", "node_modules"));
        declarations.push(inferred("Generated distribution", "dist"));
    }
    if component_path.join("wordpress/homeboy.json").is_file() {
        declarations.push(inferred(
            "Homeboy Extensions WordPress runtime fixture",
            "wordpress",
        ));
    }
    declarations
}

fn inferred(label: &str, relative_path: &str) -> ResolvedDeclaration {
    ResolvedDeclaration {
        label: label.to_string(),
        source: "inferred".to_string(),
        relative_path: relative_path.to_string(),
    }
}

fn normalize_relative_artifact_path(path: &str) -> Result<String> {
    let trimmed = path.trim().trim_end_matches('/');
    let relative = Path::new(trimmed);
    if trimmed.is_empty() || relative.is_absolute() {
        return Err(invalid_artifact_path(path));
    }
    if relative.components().any(|component| {
        matches!(
            component,
            PathComponent::ParentDir | PathComponent::RootDir | PathComponent::Prefix(_)
        )
    }) {
        return Err(invalid_artifact_path(path));
    }
    Ok(trimmed.to_string())
}

fn invalid_artifact_path(path: &str) -> Error {
    Error::validation_invalid_argument(
        "cleanup_artifacts.path",
        "Cleanup artifact paths must be repo-relative and stay inside the component root",
        Some(path.to_string()),
        None,
    )
}

fn path_size(path: &Path) -> Result<u64> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        Error::internal_io(error.to_string(), Some(format!("stat {}", path.display())))
    })?;
    if metadata.is_file() || metadata.file_type().is_symlink() {
        return Ok(metadata.len());
    }

    let mut total = metadata.len();
    for entry in fs::read_dir(path).map_err(|error| {
        Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
    })? {
        let entry = entry.map_err(|error| {
            Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
        })?;
        total = total.saturating_add(path_size(&entry.path())?);
    }
    Ok(total)
}

fn remove_artifact_path(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        Error::internal_io(error.to_string(), Some(format!("stat {}", path.display())))
    })?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("remove {}", path.display())),
            )
        })
    } else {
        fs::remove_file(path).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("remove {}", path.display())),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn component(dir: &Path) -> Component {
        Component {
            id: "fixture".to_string(),
            local_path: dir.display().to_string(),
            ..Component::default()
        }
    }

    #[test]
    fn parses_declared_cleanup_artifacts_from_component_config() {
        let raw = serde_json::json!({
            "id": "fixture",
            "local_path": "/tmp/fixture",
            "cleanup_artifacts": [
                { "label": "WordPress runtime", "path": "wordpress/" },
                { "label": "generated packages", "glob": "packages/*/dist" }
            ]
        });

        let parsed: Component = serde_json::from_value(raw).expect("component parses");

        assert_eq!(parsed.cleanup_artifacts.len(), 2);
        assert_eq!(parsed.cleanup_artifacts[0].label, "WordPress runtime");
        assert_eq!(
            parsed.cleanup_artifacts[0].path.as_deref(),
            Some("wordpress/")
        );
    }

    #[test]
    fn dry_run_reports_declared_and_inferred_artifacts() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"fixture\"\n",
        )
        .unwrap();
        fs::create_dir_all(dir.path().join("target/debug")).unwrap();
        fs::write(dir.path().join("target/debug/bin"), "binary").unwrap();
        fs::create_dir_all(dir.path().join("wordpress")).unwrap();
        fs::write(dir.path().join("wordpress/homeboy.json"), "{}").unwrap();

        let mut component = component(dir.path());
        component
            .cleanup_artifacts
            .push(CleanupArtifactDeclaration {
                label: "custom cache".to_string(),
                path: Some("cache".to_string()),
                glob: None,
            });

        let report = cleanup_artifact_report(&component, false).expect("report");

        assert_eq!(report.applied_count, 0);
        assert!(report.candidates.iter().any(|candidate| {
            candidate.relative_path == "target" && candidate.source == "inferred"
        }));
        assert!(report.candidates.iter().any(|candidate| {
            candidate.relative_path == "wordpress" && candidate.label.contains("WordPress")
        }));
        assert!(report.candidates.iter().any(|candidate| {
            candidate.relative_path == "cache" && candidate.skipped_reason.is_some()
        }));
    }

    #[test]
    fn apply_removes_only_declared_artifact_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join("dist")).unwrap();
        fs::write(dir.path().join("dist/app.js"), "generated").unwrap();
        fs::write(dir.path().join("src.js"), "source").unwrap();

        let mut component = component(dir.path());
        component
            .cleanup_artifacts
            .push(CleanupArtifactDeclaration {
                label: "dist".to_string(),
                path: Some("dist".to_string()),
                glob: None,
            });

        let report = cleanup_artifact_report(&component, true).expect("apply");

        assert_eq!(report.applied_count, 1);
        assert!(!dir.path().join("dist").exists());
        assert!(dir.path().join("src.js").exists());
    }

    #[test]
    fn rejects_paths_outside_component_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut component = component(dir.path());
        component
            .cleanup_artifacts
            .push(CleanupArtifactDeclaration {
                label: "bad".to_string(),
                path: Some("../outside".to_string()),
                glob: None,
            });

        let err = cleanup_artifact_report(&component, false).expect_err("invalid path");

        assert!(err.to_string().contains("repo-relative"));
    }
}
