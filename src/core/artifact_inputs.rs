use std::fs::{self, File};
use std::io::{self, Read, Seek, Write};
use std::path::{Component as PathComponent, Path, PathBuf};

use serde::{Deserialize, Serialize};
use zip::write::FileOptions;

use crate::core::artifact_metadata::sha256_file;
use crate::core::component::{self, ArtifactInput, Component};
use crate::core::error::{Error, Result};
use crate::core::extension::build::resolve_artifact_path_from_root;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedArtifactInput {
    pub component: String,
    pub artifact: String,
    pub target: String,
    pub sha256: String,
}

pub(crate) trait ArtifactInputWriter {
    fn validate_consumer_artifact(&self, consumer_artifact: &Path) -> Result<()>;
    fn write_input(
        &self,
        consumer_artifact: &Path,
        source_artifact: &Path,
        resolved: &ResolvedArtifactInput,
    ) -> Result<()>;
}

#[derive(Debug, Default)]
pub(crate) struct ZipArtifactInputWriter;

impl ArtifactInputWriter for ZipArtifactInputWriter {
    fn validate_consumer_artifact(&self, consumer_artifact: &Path) -> Result<()> {
        validate_zip_artifact(consumer_artifact)
    }

    fn write_input(
        &self,
        consumer_artifact: &Path,
        source_artifact: &Path,
        resolved: &ResolvedArtifactInput,
    ) -> Result<()> {
        append_file_to_zip(consumer_artifact, source_artifact, &resolved.target)
    }
}

struct ResolvedArtifactInputSource {
    metadata: ResolvedArtifactInput,
    source_artifact: PathBuf,
}

pub(crate) fn apply_to_component_artifact(
    consumer: &Component,
    consumer_artifact: &Path,
) -> Result<Vec<ResolvedArtifactInput>> {
    apply_to_component_artifact_with_writer(consumer, consumer_artifact, &ZipArtifactInputWriter)
}

fn apply_to_component_artifact_with_writer(
    consumer: &Component,
    consumer_artifact: &Path,
    writer: &dyn ArtifactInputWriter,
) -> Result<Vec<ResolvedArtifactInput>> {
    if consumer.artifact_inputs.is_empty() {
        return Ok(Vec::new());
    }

    writer.validate_consumer_artifact(consumer_artifact)?;

    let mut resolved = Vec::with_capacity(consumer.artifact_inputs.len());
    for input in &consumer.artifact_inputs {
        let input = resolve_input_source(input, &consumer.id, true)?;
        writer.write_input(consumer_artifact, &input.source_artifact, &input.metadata)?;
        resolved.push(input.metadata);
    }

    Ok(resolved)
}

pub(crate) fn resolve_metadata(component: &Component) -> Result<Vec<ResolvedArtifactInput>> {
    component
        .artifact_inputs
        .iter()
        .map(|input| {
            resolve_input_source(input, &component.id, false).map(|resolved| resolved.metadata)
        })
        .collect()
}

fn resolve_input_source(
    input: &ArtifactInput,
    consumer_id: &str,
    build_producer: bool,
) -> Result<ResolvedArtifactInputSource> {
    let source_artifact = if build_producer {
        build_and_resolve_producer_artifact(input, consumer_id)?
    } else {
        resolve_producer_artifact(input, consumer_id)?
    };
    let sha256 = sha256_file(&source_artifact)?;

    if let Some(expected) = input.sha256.as_deref() {
        if !expected.eq_ignore_ascii_case(&sha256) {
            return Err(Error::validation_invalid_argument(
                "artifact_inputs.sha256",
                format!(
                    "Artifact input '{}' for component '{}' has SHA-256 {}, expected {}",
                    input.artifact, input.component, sha256, expected
                ),
                Some(input.target.clone()),
                None,
            ));
        }
    }

    Ok(ResolvedArtifactInputSource {
        metadata: ResolvedArtifactInput {
            component: input.component.clone(),
            artifact: source_artifact.display().to_string(),
            target: input.target.clone(),
            sha256,
        },
        source_artifact,
    })
}

fn build_and_resolve_producer_artifact(
    input: &ArtifactInput,
    consumer_id: &str,
) -> Result<std::path::PathBuf> {
    validate_input(input, consumer_id)?;
    let producer = component::resolve_effective(Some(&input.component), None, None)?;
    let (exit_code, build_error) = crate::core::build::build_component(&producer);
    if let Some(error) = build_error {
        return Err(Error::validation_invalid_argument(
            "artifact_inputs.component",
            format!(
                "Failed to build artifact input component '{}' (exit {:?}): {}",
                input.component, exit_code, error
            ),
            Some(input.component.clone()),
            None,
        ));
    }

    resolve_artifact_path_from_root(&input.artifact, Some(Path::new(&producer.local_path)))
}

fn resolve_producer_artifact(input: &ArtifactInput, consumer_id: &str) -> Result<PathBuf> {
    validate_input(input, consumer_id)?;
    let producer = component::resolve_effective(Some(&input.component), None, None)?;
    resolve_artifact_path_from_root(&input.artifact, Some(Path::new(&producer.local_path)))
}

fn validate_input(input: &ArtifactInput, consumer_id: &str) -> Result<()> {
    if input.component.trim().is_empty() {
        return Err(invalid_input(
            "component",
            "Artifact input component cannot be empty",
            input,
        ));
    }
    if input.artifact.trim().is_empty() {
        return Err(invalid_input(
            "artifact",
            "Artifact input artifact cannot be empty",
            input,
        ));
    }
    validate_target(&input.target)?;
    if input.component == consumer_id {
        return Err(invalid_input(
            "component",
            "Artifact input cannot reference the consumer component itself",
            input,
        ));
    }

    Ok(())
}

fn invalid_input(field: &str, message: &str, input: &ArtifactInput) -> Error {
    Error::validation_invalid_argument(
        format!("artifact_inputs.{field}"),
        message.to_string(),
        Some(input.target.clone()),
        None,
    )
}

fn validate_zip_artifact(path: &Path) -> Result<()> {
    if path.extension().and_then(|ext| ext.to_str()) != Some("zip") {
        return Err(Error::validation_invalid_argument(
            "build_artifact",
            format!(
                "Artifact inputs currently require a ZIP consumer artifact, got {}",
                path.display()
            ),
            Some(path.display().to_string()),
            None,
        ));
    }
    Ok(())
}

fn validate_target(target: &str) -> Result<()> {
    let path = Path::new(target);
    if target.trim().is_empty() || path.is_absolute() {
        return Err(Error::validation_invalid_argument(
            "artifact_inputs.target",
            "Artifact input target must be a relative path inside the consumer artifact",
            Some(target.to_string()),
            None,
        ));
    }

    if path.components().any(|component| {
        matches!(
            component,
            PathComponent::ParentDir | PathComponent::RootDir | PathComponent::Prefix(_)
        )
    }) {
        return Err(Error::validation_invalid_argument(
            "artifact_inputs.target",
            "Artifact input target cannot escape the consumer artifact",
            Some(target.to_string()),
            None,
        ));
    }

    Ok(())
}

fn append_file_to_zip(zip_path: &Path, source: &Path, target: &str) -> Result<()> {
    let source_zip = File::open(zip_path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(zip_path.display().to_string())))?;
    let mut archive = zip::ZipArchive::new(source_zip).map_err(zip_error(zip_path))?;
    let temp_path = zip_path.with_extension("zip.homeboy-artifact-input.tmp");
    let temp_file = File::create(&temp_path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(temp_path.display().to_string())))?;
    let mut zip = zip::ZipWriter::new(temp_file);

    let normalized_target = target.replace('\\', "/");
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(zip_error(zip_path))?;
        if entry.name() == normalized_target {
            continue;
        }
        if entry.is_dir() {
            zip.add_directory(entry.name(), FileOptions::default())
                .map_err(zip_error(zip_path))?;
            continue;
        }

        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .map_err(|e| Error::internal_io(e.to_string(), Some(zip_path.display().to_string())))?;
        zip.start_file(entry.name(), FileOptions::default())
            .map_err(zip_error(zip_path))?;
        zip.write_all(&bytes)
            .map_err(|e| Error::internal_io(e.to_string(), Some(zip_path.display().to_string())))?;
    }

    append_zip_file(&mut zip, source, target)?;
    zip.finish().map_err(zip_error(zip_path))?;
    fs::rename(&temp_path, zip_path).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("replace artifact {}", zip_path.display())),
        )
    })?;
    Ok(())
}

fn append_zip_file<W: Write + Seek>(
    zip: &mut zip::ZipWriter<W>,
    source: &Path,
    target: &str,
) -> Result<()> {
    let mut source_file = File::open(source)
        .map_err(|e| Error::internal_io(e.to_string(), Some(source.display().to_string())))?;
    zip.start_file(target.replace('\\', "/"), FileOptions::default())
        .map_err(zip_error(source))?;
    io::copy(&mut source_file, zip)
        .map_err(|e| Error::internal_io(e.to_string(), Some(source.display().to_string())))?;
    Ok(())
}

fn zip_error(path: &Path) -> impl FnOnce(zip::result::ZipError) -> Error + '_ {
    |e| Error::internal_io(e.to_string(), Some(path.display().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn validate_target_rejects_escape_paths() {
        assert!(validate_target("runtime/package.zip").is_ok());
        assert!(validate_target("../package.zip").is_err());
        assert!(validate_target("/tmp/package.zip").is_err());
    }

    #[test]
    fn append_file_to_zip_places_artifact_at_target() {
        let dir = TempDir::new().unwrap();
        let zip_path = dir.path().join("consumer.zip");
        let source_path = dir.path().join("producer.zip");
        fs::write(&source_path, b"producer bytes").unwrap();

        {
            let file = File::create(&zip_path).unwrap();
            zip::ZipWriter::new(file).finish().unwrap();
        }

        append_file_to_zip(&zip_path, &source_path, "runtime/packages/producer.zip").unwrap();

        let file = File::open(&zip_path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let mut embedded = archive.by_name("runtime/packages/producer.zip").unwrap();
        let mut bytes = Vec::new();
        std::io::copy(&mut embedded, &mut bytes).unwrap();
        assert_eq!(bytes, b"producer bytes");
    }

    #[test]
    fn append_file_to_zip_replaces_existing_target() {
        let dir = TempDir::new().unwrap();
        let zip_path = dir.path().join("consumer.zip");
        let source_path = dir.path().join("producer.zip");
        fs::write(&source_path, b"new bytes").unwrap();

        {
            let file = File::create(&zip_path).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            zip.start_file("runtime/packages/producer.zip", FileOptions::default())
                .unwrap();
            zip.write_all(b"old bytes").unwrap();
            zip.finish().unwrap();
        }

        append_file_to_zip(&zip_path, &source_path, "runtime/packages/producer.zip").unwrap();

        let file = File::open(&zip_path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        assert_eq!(archive.len(), 1);
        let mut embedded = archive.by_name("runtime/packages/producer.zip").unwrap();
        let mut bytes = Vec::new();
        std::io::copy(&mut embedded, &mut bytes).unwrap();
        assert_eq!(bytes, b"new bytes");
    }

    #[test]
    fn zip_writer_places_resolved_input_at_target() {
        let dir = TempDir::new().unwrap();
        let zip_path = dir.path().join("consumer.zip");
        let source_path = dir.path().join("producer.artifact");
        fs::write(&source_path, b"producer bytes").unwrap();

        {
            let file = File::create(&zip_path).unwrap();
            zip::ZipWriter::new(file).finish().unwrap();
        }

        let resolved = ResolvedArtifactInput {
            component: "producer".to_string(),
            artifact: source_path.display().to_string(),
            target: "runtime/packages/producer.artifact".to_string(),
            sha256: "sha".to_string(),
        };

        let writer = ZipArtifactInputWriter;
        writer.validate_consumer_artifact(&zip_path).unwrap();
        writer
            .write_input(&zip_path, &source_path, &resolved)
            .unwrap();

        let file = File::open(&zip_path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let mut embedded = archive
            .by_name("runtime/packages/producer.artifact")
            .unwrap();
        let mut bytes = Vec::new();
        std::io::copy(&mut embedded, &mut bytes).unwrap();
        assert_eq!(bytes, b"producer bytes");
    }

    #[test]
    fn zip_writer_rejects_non_zip_consumer_artifact() {
        let dir = TempDir::new().unwrap();
        let artifact_path = dir.path().join("consumer.tar");
        fs::write(&artifact_path, b"consumer bytes").unwrap();

        let writer = ZipArtifactInputWriter;
        let err = writer
            .validate_consumer_artifact(&artifact_path)
            .unwrap_err();

        assert_eq!(err.details["field"], "build_artifact");
        assert!(err.details.to_string().contains("ZIP consumer artifact"));
    }
}
