//! Portable observation-bundle serialization.
//!
//! Boundary: commands select which runs to export/import and orchestrate record
//! remapping; this module owns the bundle *format* — building a bundle from the
//! observation store, writing it to a directory, reading and validating it back,
//! and the artifact-byte packaging (including directory-artifact zipping and
//! extraction). No CLI types cross this boundary.

use std::fs;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::core::execution_contract::EXECUTION_CONTRACT;
use crate::core::observation::{
    ArtifactRecord, ObservationStore, RecordedHomeboyFinding, RunRecord, TraceSpanRecord,
};
use crate::core::runners::is_reportable_artifact_evidence_path;
use crate::core::Error;

pub const BUNDLE_FORMAT: &str = "homeboy-observations";
pub const BUNDLE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObservationBundleManifest {
    pub format: String,
    pub version: u32,
    pub created_at: String,
    pub homeboy_version: String,
    pub run_count: usize,
    pub artifact_count: usize,
    #[serde(default)]
    pub artifact_byte_count: usize,
    pub trace_span_count: usize,
    #[serde(default)]
    pub finding_count: usize,
    #[serde(default)]
    pub test_failure_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ObservationBundle {
    pub manifest: ObservationBundleManifest,
    pub runs: Vec<RunRecord>,
    pub artifacts: Vec<ArtifactRecord>,
    #[serde(default)]
    pub artifact_bytes: Vec<ObservationBundleArtifactBytes>,
    pub trace_spans: Vec<TraceSpanRecord>,
    pub findings: Vec<RecordedHomeboyFinding>,
    pub test_failures: Vec<RecordedHomeboyFinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObservationBundleArtifactBytes {
    pub artifact_id: String,
    pub path: String,
    pub sha256: String,
    pub size_bytes: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_format: Option<String>,
    #[serde(skip)]
    pub source_bytes: Option<Vec<u8>>,
}

/// Build a portable observation bundle from the given runs in the store.
pub fn build_bundle(
    store: &ObservationStore,
    runs: Vec<RunRecord>,
) -> crate::core::Result<ObservationBundle> {
    let mut artifacts = Vec::new();
    let mut artifact_bytes = Vec::new();
    let mut trace_spans = Vec::new();
    let mut findings = Vec::new();
    for run in &runs {
        for artifact in store.list_artifacts(&run.id)? {
            let (artifact, bytes) = portable_bundle_artifact_record(artifact)?;
            artifacts.push(artifact);
            if let Some(bytes) = bytes {
                artifact_bytes.push(bytes);
            }
        }
        trace_spans.extend(store.list_trace_spans(&run.id)?);
        findings.extend(
            store
                .list_findings_for_run(&run.id)?
                .into_iter()
                .map(RecordedHomeboyFinding::from),
        );
    }
    let test_failures = findings
        .iter()
        .filter(|finding| is_test_failure_finding(finding))
        .cloned()
        .collect::<Vec<_>>();
    let manifest = ObservationBundleManifest {
        format: BUNDLE_FORMAT.to_string(),
        version: BUNDLE_VERSION,
        created_at: chrono::Utc::now().to_rfc3339(),
        homeboy_version: env!("CARGO_PKG_VERSION").to_string(),
        run_count: runs.len(),
        artifact_count: artifacts.len(),
        artifact_byte_count: artifact_bytes.len(),
        trace_span_count: trace_spans.len(),
        finding_count: findings.len(),
        test_failure_count: test_failures.len(),
    };
    Ok(ObservationBundle {
        manifest,
        runs,
        artifacts,
        artifact_bytes,
        trace_spans,
        findings,
        test_failures,
    })
}

fn portable_bundle_artifact_record(
    artifact: ArtifactRecord,
) -> crate::core::Result<(ArtifactRecord, Option<ObservationBundleArtifactBytes>)> {
    if !matches!(artifact.artifact_type.as_str(), "file" | "directory") {
        return Ok((artifact, None));
    }

    if artifact.artifact_type == "file" {
        let source_path = PathBuf::from(&artifact.path);
        if source_path.is_file() {
            let bytes = fs::read(&source_path).map_err(|e| {
                Error::internal_io(
                    e.to_string(),
                    Some(format!("read artifact bytes {}", source_path.display())),
                )
            })?;
            return portable_artifact_with_bytes(artifact, bytes, None, None);
        }
    }

    if artifact.artifact_type == "directory" {
        let source_path = PathBuf::from(&artifact.path);
        if source_path.is_dir() {
            let bytes = zip_directory_artifact(&source_path)?;
            return portable_artifact_with_bytes(artifact, bytes, Some("zip"), Some(".zip"));
        }
    }

    if is_reportable_artifact_evidence_path(&artifact.path) {
        return Ok((artifact, None));
    }

    let mut portable = artifact;
    portable.artifact_type = "metadata-only".to_string();
    portable.path = EXECUTION_CONTRACT
        .artifacts
        .metadata_only_ref(&portable_artifact_label(&portable.path, &portable.id));
    Ok((portable, None))
}

fn portable_artifact_with_bytes(
    artifact: ArtifactRecord,
    bytes: Vec<u8>,
    archive_format: Option<&str>,
    extension: Option<&str>,
) -> crate::core::Result<(ArtifactRecord, Option<ObservationBundleArtifactBytes>)> {
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    let size_bytes = i64::try_from(bytes.len()).map_err(|_| {
        Error::internal_unexpected(format!(
            "artifact {} is too large to record a portable size",
            artifact.id
        ))
    })?;
    let mut file_name = portable_artifact_file_name(&artifact);
    if let Some(extension) = extension {
        file_name.push_str(extension);
    }
    let path = format!("artifact-bytes/{file_name}");
    let mut portable = artifact;
    portable.path = bundle_artifact_uri(&path);
    portable.sha256 = Some(sha256.clone());
    portable.size_bytes = Some(size_bytes);
    portable.metadata_json = with_bundle_byte_metadata(
        portable.metadata_json,
        &path,
        &sha256,
        size_bytes,
        archive_format,
    );
    let artifact_id = portable.id.clone();
    Ok((
        portable,
        Some(ObservationBundleArtifactBytes {
            artifact_id,
            path,
            sha256,
            size_bytes,
            archive_format: archive_format.map(str::to_string),
            source_bytes: Some(bytes),
        }),
    ))
}

fn portable_artifact_file_name(artifact: &ArtifactRecord) -> String {
    let label = portable_artifact_label(&artifact.path, &artifact.id);
    format!(
        "{}-{}",
        safe_bundle_segment(&artifact.id),
        safe_bundle_segment(&label)
    )
}

fn safe_bundle_segment(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    sanitized.trim_matches('-').to_string()
}

/// Build the `bundle://` URI for a packaged artifact byte path.
pub fn bundle_artifact_uri(path: &str) -> String {
    format!("bundle://{path}")
}

fn with_bundle_byte_metadata(
    metadata: serde_json::Value,
    path: &str,
    sha256: &str,
    size_bytes: i64,
    archive_format: Option<&str>,
) -> serde_json::Value {
    let mut object = match metadata {
        serde_json::Value::Object(object) => object,
        other if other.is_null() => serde_json::Map::new(),
        other => {
            let mut object = serde_json::Map::new();
            object.insert("original_metadata".to_string(), other);
            object
        }
    };
    let mut portable_bundle = serde_json::json!({
        "byte_ref": path,
        "sha256": sha256,
        "size_bytes": size_bytes,
    });
    if let Some(archive_format) = archive_format {
        portable_bundle["archive_format"] = serde_json::Value::String(archive_format.to_string());
    }
    object.insert("portable_bundle".to_string(), portable_bundle);
    serde_json::Value::Object(object)
}

/// Write a bundle to a directory as the portable `homeboy-observations` format.
pub fn write_bundle_dir(path: &Path, bundle: &ObservationBundle) -> crate::core::Result<()> {
    if path.exists() && !path.is_dir() {
        return Err(Error::validation_invalid_argument(
            "output",
            "observation bundle output must be a directory",
            Some(path.to_string_lossy().to_string()),
            None,
        ));
    }
    fs::create_dir_all(path).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("create observation bundle dir {}", path.display())),
        )
    })?;
    write_json(path.join("manifest.json"), &bundle.manifest)?;
    write_json(path.join("runs.json"), &bundle.runs)?;
    write_json(path.join("artifacts.json"), &bundle.artifacts)?;
    write_json(path.join("artifact_bytes.json"), &bundle.artifact_bytes)?;
    write_artifact_bytes(path, &bundle.artifact_bytes)?;
    write_json(path.join("trace_spans.json"), &bundle.trace_spans)?;
    write_json(path.join("findings.json"), &bundle.findings)?;
    write_json(path.join("test_failures.json"), &bundle.test_failures)?;
    Ok(())
}

/// Read and validate a bundle directory produced by [`write_bundle_dir`].
pub fn read_bundle_dir(path: &Path) -> crate::core::Result<ObservationBundle> {
    if !path.is_dir() {
        return Err(Error::validation_invalid_argument(
            "input",
            "observation bundle input must be a directory",
            Some(path.to_string_lossy().to_string()),
            None,
        ));
    }
    let manifest: ObservationBundleManifest = read_json(path.join("manifest.json"))?;
    if manifest.format != BUNDLE_FORMAT {
        return Err(Error::validation_invalid_argument(
            "manifest.format",
            format!("expected {BUNDLE_FORMAT}, got {}", manifest.format),
            Some(manifest.format),
            None,
        ));
    }
    if manifest.version != BUNDLE_VERSION {
        return Err(Error::validation_invalid_argument(
            "manifest.version",
            format!(
                "expected version {BUNDLE_VERSION}, got {}",
                manifest.version
            ),
            Some(manifest.version.to_string()),
            None,
        ));
    }

    let runs: Vec<RunRecord> = read_json(path.join("runs.json"))?;
    let artifacts: Vec<ArtifactRecord> = read_json(path.join("artifacts.json"))?;
    let artifact_bytes: Vec<ObservationBundleArtifactBytes> =
        read_optional_json(path.join("artifact_bytes.json"))?;
    validate_artifact_bytes(path, &artifact_bytes)?;
    let trace_spans: Vec<TraceSpanRecord> = read_json(path.join("trace_spans.json"))?;
    let mut findings: Vec<RecordedHomeboyFinding> = read_optional_json(path.join("findings.json"))?;
    let test_failures: Vec<RecordedHomeboyFinding> =
        read_optional_json(path.join("test_failures.json"))?;
    for test_failure in &test_failures {
        if !findings.iter().any(|finding| finding.id == test_failure.id) {
            findings.push(test_failure.clone());
        }
    }
    if manifest.run_count != runs.len()
        || manifest.artifact_count != artifacts.len()
        || manifest.artifact_byte_count != artifact_bytes.len()
        || manifest.trace_span_count != trace_spans.len()
        || manifest.finding_count != findings.len()
        || manifest.test_failure_count != test_failures.len()
    {
        return Err(Error::validation_invalid_argument(
            "manifest",
            "bundle manifest counts do not match record files",
            Some(path.to_string_lossy().to_string()),
            None,
        ));
    }
    Ok(ObservationBundle {
        manifest,
        runs,
        artifacts,
        artifact_bytes,
        trace_spans,
        findings,
        test_failures,
    })
}

fn write_artifact_bytes(
    bundle_dir: &Path,
    artifact_bytes: &[ObservationBundleArtifactBytes],
) -> crate::core::Result<()> {
    for bytes in artifact_bytes {
        let Some(source_bytes) = bytes.source_bytes.as_ref() else {
            continue;
        };
        let output = bundle_dir.join(&bytes.path);
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                Error::internal_io(
                    e.to_string(),
                    Some(format!("create artifact byte dir {}", parent.display())),
                )
            })?;
        }
        fs::write(&output, source_bytes).map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!("write artifact bytes {}", output.display())),
            )
        })?;
    }
    Ok(())
}

fn zip_directory_artifact(path: &Path) -> crate::core::Result<Vec<u8>> {
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut cursor);
        let options =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for file in sorted_directory_files(path)? {
            let relative = file.strip_prefix(path).map_err(|error| {
                Error::internal_unexpected(format!(
                    "failed to compute relative path for {}: {}",
                    file.display(),
                    error
                ))
            })?;
            let relative = relative.to_string_lossy().replace('\\', "/");
            zip.start_file(relative, options).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!("archive directory artifact {}", path.display())),
                )
            })?;
            let bytes = fs::read(&file).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!("read directory artifact file {}", file.display())),
                )
            })?;
            zip.write_all(&bytes).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!(
                        "write directory artifact archive {}",
                        path.display()
                    )),
                )
            })?;
        }
        zip.finish().map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!(
                    "finish directory artifact archive {}",
                    path.display()
                )),
            )
        })?;
    }
    Ok(cursor.into_inner())
}

fn sorted_directory_files(path: &Path) -> crate::core::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_directory_files(path, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_directory_files(path: &Path, files: &mut Vec<PathBuf>) -> crate::core::Result<()> {
    for entry in fs::read_dir(path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("read directory artifact {}", path.display())),
        )
    })? {
        let entry = entry.map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("read directory artifact entry {}", path.display())),
            )
        })?;
        let file_type = entry.file_type().map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!(
                    "read directory artifact entry type {}",
                    entry.path().display()
                )),
            )
        })?;
        if file_type.is_dir() {
            collect_directory_files(&entry.path(), files)?;
        } else if file_type.is_file() {
            files.push(entry.path());
        }
    }
    Ok(())
}

/// Extract a packaged directory-artifact zip into a sibling `-contents` dir.
pub fn extract_directory_artifact_archive(
    bundle_dir: &Path,
    bytes: &ObservationBundleArtifactBytes,
) -> crate::core::Result<PathBuf> {
    let archive = bundle_dir.join(&bytes.path);
    let output = bundle_dir.join(format!("{}-contents", bytes.path.trim_end_matches(".zip")));
    fs::create_dir_all(&output).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!(
                "create extracted artifact directory {}",
                output.display()
            )),
        )
    })?;
    let file = fs::File::open(&archive).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!(
                "open directory artifact archive {}",
                archive.display()
            )),
        )
    })?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| {
        Error::validation_invalid_argument(
            "artifact_bytes",
            format!("directory artifact archive is not a valid zip: {error}"),
            Some(bytes.path.clone()),
            None,
        )
    })?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("read directory artifact archive entry {index}")),
            )
        })?;
        let Some(name) = entry.enclosed_name().map(Path::to_path_buf) else {
            return Err(Error::validation_invalid_argument(
                "artifact_bytes",
                "directory artifact archive contains an unsafe path",
                Some(bytes.path.clone()),
                None,
            ));
        };
        let target = output.join(name);
        if entry.is_dir() {
            fs::create_dir_all(&target).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!(
                        "create artifact archive directory {}",
                        target.display()
                    )),
                )
            })?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!(
                        "create artifact archive parent {}",
                        parent.display()
                    )),
                )
            })?;
        }
        let mut output_file = fs::File::create(&target).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!(
                    "create extracted artifact file {}",
                    target.display()
                )),
            )
        })?;
        std::io::copy(&mut entry, &mut output_file).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!(
                    "extract artifact archive file {}",
                    target.display()
                )),
            )
        })?;
    }
    Ok(output)
}

fn validate_artifact_bytes(
    bundle_dir: &Path,
    artifact_bytes: &[ObservationBundleArtifactBytes],
) -> crate::core::Result<()> {
    for bytes in artifact_bytes {
        let path = bundle_dir.join(&bytes.path);
        let raw = fs::read(&path).map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!("read bundled artifact bytes {}", path.display())),
            )
        })?;
        let sha256 = format!("{:x}", Sha256::digest(&raw));
        let size_bytes = i64::try_from(raw.len()).map_err(|_| {
            Error::internal_unexpected(format!(
                "bundled artifact {} is too large to record a portable size",
                bytes.artifact_id
            ))
        })?;
        if sha256 != bytes.sha256 || size_bytes != bytes.size_bytes {
            return Err(Error::validation_invalid_argument(
                "artifact_bytes",
                format!(
                    "bundled artifact bytes for {} do not match recorded checksum/size",
                    bytes.artifact_id
                ),
                Some(path.to_string_lossy().to_string()),
                None,
            ));
        }
    }
    Ok(())
}

fn is_test_failure_finding(finding: &RecordedHomeboyFinding) -> bool {
    let metadata_json = finding.finding.metadata_json();
    finding.finding.tool == "test"
        && (metadata_json
            .get("record_kind")
            .and_then(|value| value.as_str())
            == Some("failure")
            || metadata_json
                .get("source_sidecar")
                .and_then(|value| value.as_str())
                == Some("test-failures"))
}

/// Return the trailing path segment of an artifact path, or `fallback`.
pub fn portable_artifact_label(path: &str, fallback: &str) -> String {
    path.rsplit(['/', '\\'])
        .find(|segment| !segment.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

fn write_json(path: PathBuf, value: &impl Serialize) -> crate::core::Result<()> {
    let json = serde_json::to_string_pretty(value).map_err(|e| {
        Error::internal_json(e.to_string(), Some(format!("serialize {}", path.display())))
    })?;
    fs::write(&path, json).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("write observation bundle file {}", path.display())),
        )
    })
}

fn read_json<T: for<'de> Deserialize<'de>>(path: PathBuf) -> crate::core::Result<T> {
    crate::core::config::read_json_file_with(
        &path,
        |e| {
            Error::internal_io(
                e.to_string(),
                Some(format!("read observation bundle file {}", path.display())),
            )
        },
        |e, raw| {
            Error::validation_invalid_json(
                e,
                Some(format!("parse observation bundle file {}", path.display())),
                Some(raw),
            )
        },
    )
}

fn read_optional_json<T: for<'de> Deserialize<'de> + Default>(
    path: PathBuf,
) -> crate::core::Result<T> {
    if !path.exists() {
        return Ok(T::default());
    }
    read_json(path)
}
