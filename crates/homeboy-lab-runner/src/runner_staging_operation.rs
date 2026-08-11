//! Versioned remote admission for a runner-owned staging lifecycle.
//!
//! The controller turns its private source location into sealed source bytes
//! before this boundary. A remote runner receives neither that path nor an
//! instruction to resolve controller state; it durably owns materialization,
//! staging artifacts, and the replayable receipt.

use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Component, Path};
use std::process::Command;

use homeboy_core::{Error, Result};

use crate::direct_lab_handoff::{DirectLabHandoffEnvelope, DirectLabHandoffReceipt};

pub const REMOTE_RUNNER_STAGING_SCHEMA: &str = "homeboy/remote-runner-staging/v1";
pub const REMOTE_RUNNER_STAGING_RECEIPT_SCHEMA: &str = "homeboy/remote-runner-staging-receipt/v1";
pub const REMOTE_RUNNER_STAGING_CAPABILITY: &str = "remote-runner-staging/v1";
pub const REMOTE_RUNNER_SOURCE_ARTIFACT_CAPABILITY: &str = "remote-runner-source-artifact/v1";
pub const REMOTE_RUNNER_SOURCE_ARTIFACT_SYMLINK_CAPABILITY: &str =
    "remote-runner-source-artifact/v2";
pub const SEALED_SOURCE_AUTHORITY_SCHEMA: &str = "homeboy/sealed-source-authority/v1";
pub const SOURCE_ARTIFACT_TRANSFER_SCHEMA: &str = "homeboy/runner-source-artifact-transfer/v1";
pub const RUNNER_SOURCE_ARTIFACT_SCHEMA: &str = "homeboy/runner-source-artifact/v1";
pub const MAX_SOURCE_ARTIFACT_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_SOURCE_PACKAGE_ENTRIES: usize = 1024;
pub const MAX_SOURCE_PACKAGE_FILE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourcePackageEntry {
    pub path: String,
    #[serde(
        default = "SourcePackageEntryKind::regular_file",
        skip_serializing_if = "SourcePackageEntryKind::is_regular_file"
    )]
    pub kind: SourcePackageEntryKind,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourcePackageEntryKind {
    File,
    Symlink,
}

impl SourcePackageEntryKind {
    const fn regular_file() -> Self {
        Self::File
    }

    const fn is_regular_file(kind: &Self) -> bool {
        matches!(kind, Self::File)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum SourcePackagePayload {
    File { content_base64: String },
    Symlink { target: String },
}

/// The portable v2 representation of a tracked source symlink. Consumers that
/// scan source trees must use this verdict rather than reimplementing target
/// containment or package identity rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcePackageSymlinkVerdict {
    pub target: String,
    pub sha256: String,
    pub size_bytes: u64,
}

/// Normalizes Windows and Unix separator forms before applying the v2 lexical
/// containment policy. V1 packages do not admit symlinks.
pub fn source_package_symlink_verdict(
    link_path: &str,
    target: &str,
) -> Result<SourcePackageSymlinkVerdict> {
    let target = target.replace('\\', "/");
    if target.is_empty() {
        return Err(Error::validation_invalid_argument(
            "source_package",
            "source package symlink target must be a non-empty relative in-tree path",
            Some(link_path.to_string()),
            None,
        ));
    }
    let target_path = Path::new(&target);
    if target_path.is_absolute()
        || target_path
            .components()
            .any(|component| matches!(component, Component::Prefix(_)))
    {
        return Err(Error::validation_invalid_argument(
            "source_package",
            "source package symlink target must be relative and contained within the source root",
            Some(format!("{link_path} -> {target}")),
            None,
        ));
    }
    let mut depth = link_path.split('/').count().saturating_sub(1);
    for component in target_path.components() {
        match component {
            Component::ParentDir => {
                if depth == 0 {
                    return Err(Error::validation_invalid_argument(
                        "source_package",
                        "source package symlink target must be relative and contained within the source root",
                        Some(format!("{link_path} -> {target}")),
                        None,
                    ));
                }
                depth -= 1;
            }
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) => {
                return Err(Error::validation_invalid_argument(
                    "source_package",
                    "source package symlink target must be relative and contained within the source root",
                    Some(format!("{link_path} -> {target}")),
                    None,
                ))
            }
        }
    }
    Ok(SourcePackageSymlinkVerdict {
        size_bytes: target.len() as u64,
        sha256: format!("sha256:{:x}", Sha256::digest(target.as_bytes())),
        target,
    })
}

#[cfg(unix)]
fn read_regular_file_nofollow(path: &Path) -> Result<Vec<u8>> {
    use std::os::unix::fs::OpenOptionsExt;

    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    let metadata = file
        .metadata()
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    if !metadata.is_file() || metadata.len() > MAX_SOURCE_PACKAGE_FILE_BYTES {
        return Err(Error::validation_invalid_argument(
            "source_path",
            "source package file must remain a bounded regular file when opened",
            Some(path.display().to_string()),
            None,
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_SOURCE_PACKAGE_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    if bytes.len() as u64 > MAX_SOURCE_PACKAGE_FILE_BYTES {
        return Err(Error::validation_invalid_argument(
            "source_path",
            "source package file exceeds the configured size bound",
            Some(path.display().to_string()),
            None,
        ));
    }
    Ok(bytes)
}

#[cfg(not(unix))]
fn read_regular_file_nofollow(path: &Path) -> Result<Vec<u8>> {
    Err(Error::validation_invalid_argument(
        "source_path",
        "source package file reads require a no-follow-capable platform",
        Some(path.display().to_string()),
        None,
    ))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourcePackageManifest {
    pub schema: String,
    pub format: String,
    pub extraction_root: String,
    pub entries: Vec<SourcePackageEntry>,
}

/// Bounded package bytes transferred exactly once during staging. The receipt
/// carries only [`RunnerSourceArtifact`], never these potentially large bytes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceArtifactTransfer {
    pub schema: String,
    pub artifact_id: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub content_base64: String,
    pub package: SourcePackageManifest,
}

impl SourceArtifactTransfer {
    /// Packages a controller-owned source tree into the versioned runner
    /// manifest. Git-indexed, lexically in-tree symlinks retain their target
    /// text in v2; untracked links are omitted without reading their targets.
    pub fn from_directory(artifact_id: impl Into<String>, root: &Path) -> Result<Self> {
        fn tracked_symlinks(root: &Path) -> Result<BTreeSet<String>> {
            let output = match Command::new("git")
                .arg("-C")
                .arg(root)
                .args(["ls-files", "--stage", "-z", "--", "."])
                .output()
            {
                Ok(output) => output,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(BTreeSet::new())
                }
                Err(error) => {
                    return Err(Error::internal_io(
                        error.to_string(),
                        Some(root.display().to_string()),
                    ))
                }
            };
            if !output.status.success() {
                // A non-Git source directory has no authoritative link inventory.
                if String::from_utf8_lossy(&output.stderr).contains("not a git repository") {
                    return Ok(BTreeSet::new());
                }
                return Err(Error::validation_invalid_argument(
                    "source_path",
                    "could not read Git tracking metadata for source package",
                    Some(root.display().to_string()),
                    None,
                ));
            }
            let links = output
                .stdout
                .split(|byte| *byte == 0)
                .filter(|record| !record.is_empty())
                .filter_map(|record| {
                    let separator = record.iter().position(|byte| *byte == b'\t')?;
                    let (metadata, path) = record.split_at(separator);
                    let path = &path[1..];
                    (metadata.starts_with(b"120000 ") && std::str::from_utf8(path).is_ok()).then(
                        || {
                            std::str::from_utf8(path)
                                .expect("validated UTF-8")
                                .replace('\\', "/")
                        },
                    )
                })
                .collect::<BTreeSet<_>>();
            Ok(links)
        }
        fn collect(
            root: &Path,
            directory: &Path,
            tracked_links: &BTreeSet<String>,
            entries: &mut BTreeMap<String, SourcePackagePayload>,
        ) -> Result<()> {
            let mut directory_entries = fs::read_dir(directory)
                .map_err(|error| {
                    Error::internal_io(error.to_string(), Some(directory.display().to_string()))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|error| {
                    Error::internal_io(error.to_string(), Some(directory.display().to_string()))
                })?;
            directory_entries.sort_by_key(|entry| entry.file_name());
            for entry in directory_entries {
                let path = entry.path();
                // Git metadata is controller-local state, never source content.
                if path.strip_prefix(root).expect("walk remains under root") == Path::new(".git") {
                    continue;
                }
                let metadata = fs::symlink_metadata(&path).map_err(|error| {
                    Error::internal_io(error.to_string(), Some(path.display().to_string()))
                })?;
                let relative = path.strip_prefix(root).expect("walk remains under root");
                let relative = relative.to_string_lossy().replace('\\', "/");
                if metadata.file_type().is_symlink() {
                    if !tracked_links.contains(&relative) {
                        continue;
                    }
                    let target = fs::read_link(&path).map_err(|error| {
                        Error::internal_io(error.to_string(), Some(path.display().to_string()))
                    })?;
                    let target = target.into_os_string().into_string().map_err(|target| {
                        Error::validation_invalid_argument(
                            "source_path",
                            "tracked source symlink target must be valid UTF-8 text",
                            Some(format!(
                                "{} -> {}",
                                path.display(),
                                target.to_string_lossy()
                            )),
                            None,
                        )
                    })?;
                    let verdict = source_package_symlink_verdict(&relative, &target).map_err(|_| {
                        Error::validation_invalid_argument(
                            "source_path",
                            "tracked source symlink must have a relative target contained within the source root",
                            Some(format!("{} -> {target}", path.display())),
                            None,
                        )
                    })?;
                    entries.insert(
                        relative,
                        SourcePackagePayload::Symlink {
                            target: verdict.target,
                        },
                    );
                    continue;
                }
                if !(metadata.is_file() || metadata.is_dir()) {
                    return Err(Error::validation_invalid_argument(
                        "source_path",
                        "source package accepts only regular files and directories",
                        Some(path.display().to_string()),
                        None,
                    ));
                }
                if metadata.is_dir() {
                    collect(root, &path, tracked_links, entries)?;
                    continue;
                }
                let bytes = read_regular_file_nofollow(&path)?;
                entries.insert(
                    relative,
                    SourcePackagePayload::File {
                        content_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
                    },
                );
                if entries.len() > MAX_SOURCE_PACKAGE_ENTRIES
                    || entries
                        .values()
                        .map(|entry| match entry {
                            SourcePackagePayload::File { content_base64 } => {
                                base64::engine::general_purpose::STANDARD
                                    .decode(content_base64)
                                    .map_or(0, |bytes| bytes.len() as u64)
                            }
                            SourcePackagePayload::Symlink { target } => target.len() as u64,
                        })
                        .sum::<u64>()
                        > MAX_SOURCE_ARTIFACT_BYTES
                {
                    return Err(Error::validation_invalid_argument(
                        "source_path",
                        "source package exceeds configured entry or total size bounds",
                        Some(root.display().to_string()),
                        None,
                    ));
                }
            }
            Ok(())
        }

        let root_metadata = fs::symlink_metadata(root).map_err(|error| {
            Error::internal_io(error.to_string(), Some(root.display().to_string()))
        })?;
        if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
            return Err(Error::validation_invalid_argument(
                "source_path",
                "source package root must be a readable non-symlink directory",
                Some(root.display().to_string()),
                None,
            ));
        }
        let tracked_links = tracked_symlinks(root)?;
        let mut payloads = BTreeMap::new();
        collect(root, root, &tracked_links, &mut payloads)?;
        if payloads.is_empty() {
            return Err(Error::validation_invalid_argument(
                "source_path",
                "source package root must contain at least one regular file",
                Some(root.display().to_string()),
                None,
            ));
        }
        let entries = payloads
            .iter()
            .map(|(path, payload)| SourcePackageEntry {
                path: path.clone(),
                kind: match payload {
                    SourcePackagePayload::File { .. } => SourcePackageEntryKind::File,
                    SourcePackagePayload::Symlink { .. } => SourcePackageEntryKind::Symlink,
                },
                sha256: match payload {
                    SourcePackagePayload::File { content_base64 } => format!(
                        "sha256:{:x}",
                        Sha256::digest(
                            base64::engine::general_purpose::STANDARD
                                .decode(content_base64)
                                .expect("package bytes")
                        )
                    ),
                    SourcePackagePayload::Symlink { target } => {
                        source_package_symlink_verdict(path, target)
                            .expect("scanner normalized symlink target")
                            .sha256
                    }
                },
                size_bytes: match payload {
                    SourcePackagePayload::File { content_base64 } => {
                        base64::engine::general_purpose::STANDARD
                            .decode(content_base64)
                            .expect("package bytes")
                            .len() as u64
                    }
                    SourcePackagePayload::Symlink { target } => {
                        source_package_symlink_verdict(path, target)
                            .expect("scanner normalized symlink target")
                            .size_bytes
                    }
                },
            })
            .collect::<Vec<_>>();
        let has_symlinks = entries
            .iter()
            .any(|entry| entry.kind == SourcePackageEntryKind::Symlink);
        let package = if has_symlinks {
            serde_json::to_vec(&payloads)
        } else {
            serde_json::to_vec(
                &payloads
                    .iter()
                    .map(|(path, payload)| match payload {
                        SourcePackagePayload::File { content_base64 } => (path, content_base64),
                        SourcePackagePayload::Symlink { .. } => unreachable!("v1 has no links"),
                    })
                    .collect::<BTreeMap<_, _>>(),
            )
        }
        .expect("source package serializes");
        let transfer = Self {
            schema: SOURCE_ARTIFACT_TRANSFER_SCHEMA.to_string(),
            artifact_id: artifact_id.into(),
            sha256: format!("sha256:{:x}", Sha256::digest(&package)),
            size_bytes: package.len() as u64,
            content_base64: base64::engine::general_purpose::STANDARD.encode(package),
            package: SourcePackageManifest {
                schema: if has_symlinks {
                    "homeboy/source-package-manifest/v2"
                } else {
                    "homeboy/source-package-manifest/v1"
                }
                .into(),
                format: if has_symlinks {
                    "homeboy/source-package-json/v2"
                } else {
                    "homeboy/source-package-json/v1"
                }
                .into(),
                extraction_root: "workspace".into(),
                entries,
            },
        };
        transfer.decode_verified()?;
        Ok(transfer)
    }

    pub fn from_bytes(artifact_id: impl Into<String>, bytes: &[u8]) -> Self {
        let package = serde_json::to_vec(&BTreeMap::from([(
            "source.bin",
            base64::engine::general_purpose::STANDARD.encode(bytes),
        )]))
        .expect("package");
        Self {
            schema: SOURCE_ARTIFACT_TRANSFER_SCHEMA.to_string(),
            artifact_id: artifact_id.into(),
            sha256: format!("sha256:{:x}", Sha256::digest(&package)),
            size_bytes: package.len() as u64,
            content_base64: base64::engine::general_purpose::STANDARD.encode(package),
            package: SourcePackageManifest {
                schema: "homeboy/source-package-manifest/v1".into(),
                format: "homeboy/source-package-json/v1".into(),
                extraction_root: "workspace".into(),
                entries: vec![SourcePackageEntry {
                    path: "source.bin".into(),
                    kind: SourcePackageEntryKind::File,
                    sha256: format!("sha256:{:x}", Sha256::digest(bytes)),
                    size_bytes: bytes.len() as u64,
                }],
            },
        }
    }

    pub fn decode_verified(&self) -> Result<Vec<u8>> {
        if self.schema != SOURCE_ARTIFACT_TRANSFER_SCHEMA
            || self.artifact_id.trim().is_empty()
            || self.artifact_id.contains('/')
            || self.artifact_id.contains('\\')
            || !self.sha256.starts_with("sha256:")
            || self.size_bytes > MAX_SOURCE_ARTIFACT_BYTES
        {
            return Err(Error::validation_invalid_argument(
                "source_artifact",
                "remote staging requires a bounded v1 source artifact transfer",
                Some(self.artifact_id.clone()),
                None,
            ));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&self.content_base64)
            .map_err(|error| {
                Error::validation_invalid_argument(
                    "source_artifact.content_base64",
                    error.to_string(),
                    Some(self.artifact_id.clone()),
                    None,
                )
            })?;
        if bytes.len() as u64 != self.size_bytes
            || format!("sha256:{:x}", Sha256::digest(&bytes)) != self.sha256
        {
            return Err(Error::validation_invalid_argument(
                "source_artifact",
                "source artifact bytes do not match their declared size and SHA-256 digest",
                Some(self.artifact_id.clone()),
                None,
            ));
        }
        self.package.validate(&bytes)?;
        Ok(bytes)
    }

    pub fn descriptor(&self) -> RunnerSourceArtifact {
        RunnerSourceArtifact {
            schema: RUNNER_SOURCE_ARTIFACT_SCHEMA.to_string(),
            artifact_id: self.artifact_id.clone(),
            sha256: self.sha256.clone(),
            size_bytes: self.size_bytes,
            package: self.package.clone(),
        }
    }
}

/// Immutable, retrievable source-package identity returned by staging.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerSourceArtifact {
    pub schema: String,
    pub artifact_id: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub package: SourcePackageManifest,
}

impl RunnerSourceArtifact {
    pub fn validate(&self) -> Result<()> {
        if self.schema != RUNNER_SOURCE_ARTIFACT_SCHEMA
            || self.artifact_id.trim().is_empty()
            || self.artifact_id.contains('/')
            || self.artifact_id.contains('\\')
            || !self.sha256.starts_with("sha256:")
            || self.size_bytes > MAX_SOURCE_ARTIFACT_BYTES
        {
            return Err(Error::validation_invalid_argument(
                "source_artifact",
                "invalid runner source artifact descriptor",
                Some(self.artifact_id.clone()),
                None,
            ));
        }
        self.package.validate_shape()
    }
}

impl SourcePackageManifest {
    fn validate_shape(&self) -> Result<()> {
        let v1 = self.schema == "homeboy/source-package-manifest/v1"
            && self.format == "homeboy/source-package-json/v1";
        let v2 = self.schema == "homeboy/source-package-manifest/v2"
            && self.format == "homeboy/source-package-json/v2";
        if !(v1 || v2)
            || self.extraction_root != "workspace"
            || self.entries.is_empty()
            || self.entries.len() > MAX_SOURCE_PACKAGE_ENTRIES
        {
            return Err(Error::validation_invalid_argument(
                "source_package",
                "invalid source package manifest",
                None,
                None,
            ));
        }
        let mut paths = BTreeSet::new();
        let mut total = 0u64;
        for entry in &self.entries {
            if entry.path.is_empty()
                || entry.path.starts_with('/')
                || entry.path.contains('\\')
                || entry
                    .path
                    .split('/')
                    .any(|part| part == "." || part == "..")
                || !paths.insert(&entry.path)
                || !entry.sha256.starts_with("sha256:")
                || entry.size_bytes > MAX_SOURCE_PACKAGE_FILE_BYTES
                || (v1 && entry.kind != SourcePackageEntryKind::File)
            {
                return Err(Error::validation_invalid_argument(
                    "source_package",
                    "unsafe, duplicate, or oversized source package path",
                    Some(entry.path.clone()),
                    None,
                ));
            }
            total = total.saturating_add(entry.size_bytes);
        }
        if total > MAX_SOURCE_ARTIFACT_BYTES {
            return Err(Error::validation_invalid_argument(
                "source_package",
                "source package exceeds total size bound",
                None,
                None,
            ));
        }
        if self.entries.iter().any(|entry| {
            self.entries.iter().any(|other| {
                entry.path != other.path && other.path.starts_with(&format!("{}/", entry.path))
            })
        }) {
            return Err(Error::validation_invalid_argument(
                "source_package",
                "source package paths must not overlap file or symlink entries",
                None,
                None,
            ));
        }
        Ok(())
    }
    pub(crate) fn validate(&self, bytes: &[u8]) -> Result<()> {
        self.validate_shape()?;
        let payloads = if self.schema == "homeboy/source-package-manifest/v1" {
            serde_json::from_slice::<BTreeMap<String, String>>(bytes).map(|files| {
                files
                    .into_iter()
                    .map(|(path, content_base64)| {
                        (path, SourcePackagePayload::File { content_base64 })
                    })
                    .collect()
            })
        } else {
            serde_json::from_slice::<BTreeMap<String, SourcePackagePayload>>(bytes)
        }
        .map_err(|error| {
            Error::validation_invalid_argument("source_package", error.to_string(), None, None)
        })?;
        if payloads.len() != self.entries.len() {
            return Err(Error::validation_invalid_argument(
                "source_package",
                "source package entries do not match manifest",
                None,
                None,
            ));
        }
        for entry in &self.entries {
            let payload = payloads.get(&entry.path).ok_or_else(|| {
                Error::validation_invalid_argument(
                    "source_package",
                    "source package entry is missing",
                    Some(entry.path.clone()),
                    None,
                )
            })?;
            let content = match (payload, &entry.kind) {
                (SourcePackagePayload::File { content_base64 }, SourcePackageEntryKind::File) => {
                    base64::engine::general_purpose::STANDARD
                        .decode(content_base64)
                        .map_err(|error| {
                            Error::validation_invalid_argument(
                                "source_package",
                                error.to_string(),
                                Some(entry.path.clone()),
                                None,
                            )
                        })?
                }
                (SourcePackagePayload::Symlink { target }, SourcePackageEntryKind::Symlink)
                    if source_package_symlink_verdict(&entry.path, target)
                        .map(|verdict| verdict.target == *target)
                        .unwrap_or(false) =>
                {
                    target.as_bytes().to_vec()
                }
                _ => {
                    return Err(Error::validation_invalid_argument(
                        "source_package",
                        "source package entry kind or symlink target is invalid",
                        Some(entry.path.clone()),
                        None,
                    ))
                }
            };
            if content.len() as u64 != entry.size_bytes
                || format!("sha256:{:x}", Sha256::digest(&content)) != entry.sha256
            {
                return Err(Error::validation_invalid_argument(
                    "source_package",
                    "source package entry does not match manifest",
                    Some(entry.path.clone()),
                    None,
                ));
            }
        }
        Ok(())
    }
}

/// Opaque, self-contained source authority. The producer seals the source
/// payload before transport; its private filesystem location is never part of
/// this contract.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SealedSourceAuthority {
    pub schema: String,
    pub content_digest: String,
    pub sealed_payload: String,
}

impl SealedSourceAuthority {
    pub fn new(content_digest: impl Into<String>, sealed_payload: impl Into<String>) -> Self {
        Self {
            schema: SEALED_SOURCE_AUTHORITY_SCHEMA.to_string(),
            content_digest: content_digest.into(),
            sealed_payload: sealed_payload.into(),
        }
    }

    fn validate(&self) -> Result<()> {
        if self.schema != SEALED_SOURCE_AUTHORITY_SCHEMA
            || !self.content_digest.starts_with("sha256:")
            || self.content_digest.len() <= "sha256:".len()
            || self.sealed_payload.trim().is_empty()
        {
            return Err(Error::validation_invalid_argument(
                "sealed_source_authority",
                "remote staging requires a v1 sealed source payload and SHA-256 content digest",
                None,
                None,
            ));
        }
        Ok(())
    }
}

/// Names the runner-owned materialization target without exposing a filesystem
/// path. The runner maps this opaque key into its own lifecycle store.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerMaterializationAuthority {
    pub authority_id: String,
    pub workspace_key: String,
    pub source: SealedSourceAuthority,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_artifact: Option<SourceArtifactTransfer>,
}

impl RunnerMaterializationAuthority {
    fn validate(&self) -> Result<()> {
        if self.authority_id.trim().is_empty()
            || self.workspace_key.trim().is_empty()
            || self.workspace_key.contains('/')
            || self.workspace_key.contains('\\')
        {
            return Err(Error::validation_invalid_argument(
                "runner_materialization_authority",
                "remote staging requires opaque runner-owned authority and workspace keys",
                None,
                None,
            ));
        }
        self.source.validate()?;
        self.source_artifact
            .as_ref()
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "source_artifact",
                    "remote staging requires a transferable source artifact before admission",
                    Some(self.authority_id.clone()),
                    None,
                )
            })?
            .decode_verified()
            .map(|_| ())
    }
}

/// Complete remote operation input. `handoff.recipe.source_path` is always
/// absent: source authority is explicit and sealed above.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RemoteRunnerStagingEnvelope {
    pub schema: String,
    pub handoff: DirectLabHandoffEnvelope,
    pub materialization: RunnerMaterializationAuthority,
}

impl RemoteRunnerStagingEnvelope {
    pub fn from_direct_handoff(
        handoff: &DirectLabHandoffEnvelope,
        materialization: RunnerMaterializationAuthority,
    ) -> Result<Self> {
        handoff.validate()?;
        let mut handoff = handoff.clone();
        handoff.recipe.source_path = None;
        let envelope = Self {
            schema: REMOTE_RUNNER_STAGING_SCHEMA.to_string(),
            handoff,
            materialization,
        };
        envelope.validate()?;
        Ok(envelope)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema != REMOTE_RUNNER_STAGING_SCHEMA
            || self.handoff.recipe.source_path.is_some()
            || self.handoff.schema != crate::direct_lab_handoff::DIRECT_LAB_HANDOFF_SCHEMA
            || self.handoff.run_id.trim().is_empty()
            || self.handoff.runner_id.trim().is_empty()
            || self.handoff.idempotency_key != self.handoff.run_id
            || self.handoff.controller_identity.trim().is_empty()
            || self.handoff.recipe.run_id != self.handoff.run_id
            || self.handoff.recipe.runner_id != self.handoff.runner_id
            || self.handoff.durable_plan.plan_id.trim().is_empty()
        {
            return Err(Error::validation_invalid_argument(
                "remote_runner_staging",
                "remote staging requires its v1 schema, bound handoff identities, and no controller-local source path",
                Some(self.handoff.run_id.clone()),
                None,
            ));
        }
        self.handoff.recipe.validate_for_runner_staging()?;
        self.materialization.validate()
    }
}

/// Runner-owned artifact identities created before a provider can execute.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerStagingArtifacts {
    pub lifecycle_id: String,
    pub source_artifact_id: String,
    pub workspace_artifact_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_artifact: Option<RunnerSourceArtifact>,
}

/// Durable runner receipt. Replays return this exact value for the same
/// idempotency key, including runner-owned artifact identities.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteRunnerStagingReceipt {
    pub schema: String,
    pub handoff: DirectLabHandoffReceipt,
    pub artifacts: RunnerStagingArtifacts,
}

impl RemoteRunnerStagingReceipt {
    pub fn validate_for(&self, envelope: &RemoteRunnerStagingEnvelope) -> Result<()> {
        if self.schema != REMOTE_RUNNER_STAGING_RECEIPT_SCHEMA
            || self.artifacts.lifecycle_id.trim().is_empty()
            || self.artifacts.source_artifact_id.trim().is_empty()
            || self.artifacts.workspace_artifact_id.trim().is_empty()
        {
            return Err(Error::validation_invalid_argument(
                "remote_runner_staging_receipt",
                "remote staging receipt is missing runner-owned lifecycle artifacts",
                Some(envelope.handoff.run_id.clone()),
                None,
            ));
        }
        self.artifacts
            .source_artifact
            .as_ref()
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "remote_runner_staging_receipt.source_artifact",
                    "remote staging receipt is missing its immutable source artifact",
                    Some(envelope.handoff.run_id.clone()),
                    None,
                )
            })?
            .validate()?;
        self.handoff.validate_for(&envelope.handoff)
    }
}

/// Transport/API boundary. The implementation must atomically persist the
/// envelope, runner lifecycle artifacts, and receipt, or replay its receipt.
/// Provider budget consumption belongs after this admission boundary.
pub trait RemoteRunnerStagingTransport {
    fn is_connected(&self) -> bool;
    fn supports_capability(&self, capability: &str) -> bool;
    fn stage_durable(
        &mut self,
        envelope: &RemoteRunnerStagingEnvelope,
    ) -> Result<RemoteRunnerStagingReceipt>;
}

/// Validates version and runner availability before invoking the mutation
/// boundary, so an unavailable or incompatible runner consumes no provider
/// budget. Idempotency is owned by `stage_durable` and is checked before its
/// provider execution boundary.
pub fn submit_remote_runner_staging(
    transport: &mut impl RemoteRunnerStagingTransport,
    envelope: &RemoteRunnerStagingEnvelope,
) -> Result<RemoteRunnerStagingReceipt> {
    envelope.validate()?;
    if !transport.is_connected() {
        return Err(Error::validation_invalid_argument(
            "runner_connection",
            format!("runner `{}` is disconnected", envelope.handoff.runner_id),
            Some(envelope.handoff.runner_id.clone()),
            None,
        ));
    }
    if !transport.supports_capability(REMOTE_RUNNER_STAGING_CAPABILITY) {
        return Err(Error::runner_capability_missing(
            &envelope.handoff.runner_id,
            "sealed runner staging",
            vec![REMOTE_RUNNER_STAGING_CAPABILITY.to_string()],
            Vec::new(),
        ));
    }
    if !transport.supports_capability(REMOTE_RUNNER_SOURCE_ARTIFACT_CAPABILITY) {
        return Err(Error::runner_capability_missing(
            &envelope.handoff.runner_id,
            "sealed runner source artifact transfer",
            vec![REMOTE_RUNNER_SOURCE_ARTIFACT_CAPABILITY.to_string()],
            Vec::new(),
        ));
    }
    if envelope
        .materialization
        .source_artifact
        .as_ref()
        .is_some_and(|artifact| artifact.package.schema == "homeboy/source-package-manifest/v2")
        && !transport.supports_capability(REMOTE_RUNNER_SOURCE_ARTIFACT_SYMLINK_CAPABILITY)
    {
        return Err(Error::runner_capability_missing(
            &envelope.handoff.runner_id,
            "sealed runner source artifact symlink transfer",
            vec![REMOTE_RUNNER_SOURCE_ARTIFACT_SYMLINK_CAPABILITY.to_string()],
            Vec::new(),
        ));
    }
    let receipt = transport.stage_durable(envelope)?;
    receipt.validate_for(envelope)?;
    Ok(receipt)
}

#[cfg(test)]
pub(crate) mod tests_support {
    use std::collections::HashMap;

    use super::*;
    use crate::direct_lab_handoff::DirectLabHandoffEnvelope;
    use crate::lab_staging_controller::LabStagingRecipe;
    use homeboy_core::lab_contract::LabCommandContract;

    pub(crate) struct Transport {
        connected: bool,
        compatible: bool,
        source_artifact_compatible: bool,
        symlink_artifact_compatible: bool,
        calls: usize,
        provider_budget: usize,
        receipts: HashMap<String, RemoteRunnerStagingReceipt>,
    }

    impl RemoteRunnerStagingTransport for Transport {
        fn is_connected(&self) -> bool {
            self.connected
        }
        fn supports_capability(&self, capability: &str) -> bool {
            (self.compatible && capability == REMOTE_RUNNER_STAGING_CAPABILITY)
                || (self.source_artifact_compatible
                    && capability == REMOTE_RUNNER_SOURCE_ARTIFACT_CAPABILITY)
                || (self.symlink_artifact_compatible
                    && capability == REMOTE_RUNNER_SOURCE_ARTIFACT_SYMLINK_CAPABILITY)
        }
        fn stage_durable(
            &mut self,
            envelope: &RemoteRunnerStagingEnvelope,
        ) -> Result<RemoteRunnerStagingReceipt> {
            self.calls += 1;
            if let Some(receipt) = self.receipts.get(&envelope.handoff.idempotency_key) {
                return Ok(receipt.clone());
            }
            // This is the runner-side order: persist staging before provider work.
            let receipt = RemoteRunnerStagingReceipt {
                schema: REMOTE_RUNNER_STAGING_RECEIPT_SCHEMA.to_string(),
                handoff: DirectLabHandoffReceipt::accepted(&envelope.handoff, "runner-job-1"),
                artifacts: RunnerStagingArtifacts {
                    lifecycle_id: "runner-lifecycle-1".to_string(),
                    source_artifact_id: "runner-source-1".to_string(),
                    workspace_artifact_id: "runner-workspace-1".to_string(),
                    source_artifact: envelope
                        .materialization
                        .source_artifact
                        .as_ref()
                        .map(SourceArtifactTransfer::descriptor),
                },
            };
            self.receipts
                .insert(envelope.handoff.idempotency_key.clone(), receipt.clone());
            Ok(receipt)
        }
    }

    impl Transport {
        pub(crate) fn compatible() -> Self {
            transport()
        }

        pub(crate) fn incompatible() -> Self {
            Self {
                compatible: false,
                ..transport()
            }
        }

        pub(crate) fn disconnected() -> Self {
            Self {
                connected: false,
                ..transport()
            }
        }

        pub(crate) fn calls(&self) -> usize {
            self.calls
        }

        pub(crate) fn provider_budget(&self) -> usize {
            self.provider_budget
        }
    }

    pub(crate) fn envelope() -> RemoteRunnerStagingEnvelope {
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "run".to_string(),
        ];
        let request = crate::LabOffloadRequest {
            placement_decision: homeboy_core::lab_routing::compatibility_placement_decision(
                homeboy_lab_runner_contract::Placement::Lab,
                Some("runner-1"),
                false,
            ),
            command: Some(crate::LabOffloadCommand {
                command: LabCommandContract::portable("agent-task", None, false, &[]),
                required_extensions: Vec::new(),
                required_capabilities: Vec::new(),
                workload: None,
            }),
            normalized_args: &args,
            placement: homeboy_lab_runner_contract::Placement::Lab,
            detach_after_handoff: true,
            source_path: Some(std::path::Path::new("/controller/private/source")),
            job_overrides: homeboy_core::lab_offload::LabJobOverrides {
                env: HashMap::new(),
                secret_env_names: Vec::new(),
                workspace_root: None,
            },
            ..crate::LabOffloadRequest::for_test(&args)
        };
        let handoff = DirectLabHandoffEnvelope::new(
            "controller-identity",
            LabStagingRecipe::from_request("run-1", "runner-1", &request).expect("recipe"),
            homeboy_agents::agent_task_scheduler::AgentTaskPlan::new("plan-1", Vec::new()),
        );
        RemoteRunnerStagingEnvelope::from_direct_handoff(
            &handoff,
            RunnerMaterializationAuthority {
                authority_id: "authority-1".to_string(),
                workspace_key: "run-1".to_string(),
                source: SealedSourceAuthority::new("sha256:source-1", "sealed-source-payload"),
                source_artifact: Some(SourceArtifactTransfer::from_bytes(
                    "source-package-1",
                    b"source package",
                )),
            },
        )
        .expect("sealed envelope")
    }

    fn transport() -> Transport {
        Transport {
            connected: true,
            compatible: true,
            source_artifact_compatible: true,
            symlink_artifact_compatible: true,
            calls: 0,
            provider_budget: 0,
            receipts: HashMap::new(),
        }
    }

    #[test]
    fn compatible_transport_accepts_self_contained_envelope_and_replays_receipt() {
        let envelope = envelope();
        assert!(envelope.handoff.recipe.source_path.is_none());
        assert!(!serde_json::to_string(&envelope)
            .expect("serialize")
            .contains("/controller/private/source"));
        let mut transport = transport();
        let first = submit_remote_runner_staging(&mut transport, &envelope).expect("accept");
        let replay = submit_remote_runner_staging(&mut transport, &envelope).expect("replay");
        assert_eq!(first, replay);
        assert_eq!(transport.receipts.len(), 1);
        assert_eq!(transport.provider_budget, 0);
        assert_eq!(first.artifacts.lifecycle_id, "runner-lifecycle-1");
    }

    #[test]
    fn incompatible_or_disconnected_transport_refuses_before_provider_budget() {
        let envelope = envelope();
        let mut incompatible = Transport {
            compatible: false,
            ..transport()
        };
        let error = submit_remote_runner_staging(&mut incompatible, &envelope).expect_err("refuse");
        assert_eq!(error.code, homeboy_core::ErrorCode::RunnerCapabilityMissing);
        assert_eq!(incompatible.calls, 0);
        assert_eq!(incompatible.provider_budget, 0);
        let mut disconnected = Transport {
            connected: false,
            ..transport()
        };
        assert!(submit_remote_runner_staging(&mut disconnected, &envelope).is_err());
        assert_eq!(disconnected.calls, 0);
        assert_eq!(disconnected.provider_budget, 0);
    }

    #[test]
    fn source_artifact_capability_is_negotiated_before_admission() {
        let envelope = envelope();
        let mut incompatible = Transport {
            source_artifact_compatible: false,
            ..transport()
        };
        let error = submit_remote_runner_staging(&mut incompatible, &envelope).expect_err("refuse");
        assert_eq!(error.code, homeboy_core::ErrorCode::RunnerCapabilityMissing);
        assert_eq!(incompatible.calls, 0);
        assert_eq!(incompatible.provider_budget, 0);
    }

    #[test]
    fn source_tree_package_is_deterministic_and_preserves_manifest_entries() {
        let source = tempfile::tempdir().expect("source");
        std::fs::create_dir(source.path().join("nested")).expect("nested");
        std::fs::write(source.path().join("z.txt"), b"z").expect("z");
        std::fs::write(source.path().join("nested/a.txt"), b"a").expect("a");

        let first =
            SourceArtifactTransfer::from_directory("source-1", source.path()).expect("pack");
        let second =
            SourceArtifactTransfer::from_directory("source-1", source.path()).expect("repack");

        assert_eq!(first, second);
        assert_eq!(
            first
                .package
                .entries
                .iter()
                .map(|entry| entry.path.as_str())
                .collect::<Vec<_>>(),
            ["nested/a.txt", "z.txt"]
        );
        first.decode_verified().expect("verified package");
        assert!(!serde_json::to_string(&first.descriptor())
            .expect("descriptor")
            .contains("\"kind\""));
    }

    #[cfg(unix)]
    #[test]
    fn git_tracked_safe_symlinks_use_v2_without_reading_untracked_targets() {
        use std::os::unix::fs::symlink;

        let source = tempfile::tempdir().expect("source");
        std::fs::create_dir(source.path().join("nested")).expect("nested");
        std::fs::write(source.path().join("nested/file.txt"), b"safe").expect("file");
        symlink("nested/file.txt", source.path().join("file-link")).expect("file link");
        symlink("missing-target", source.path().join("missing-link")).expect("missing link");
        symlink("/outside/secret", source.path().join("AGENTS.md")).expect("injected link");
        for args in [
            ["init"].as_slice(),
            ["add", "nested", "file-link", "missing-link"].as_slice(),
        ] {
            assert!(Command::new("git")
                .args(args)
                .current_dir(source.path())
                .status()
                .expect("git")
                .success());
        }

        let first =
            SourceArtifactTransfer::from_directory("source-1", source.path()).expect("pack");
        let second =
            SourceArtifactTransfer::from_directory("source-1", source.path()).expect("repack");
        assert_eq!(first, second);
        assert_eq!(first.package.schema, "homeboy/source-package-manifest/v2");
        assert_eq!(
            first
                .package
                .entries
                .iter()
                .map(|entry| (entry.path.as_str(), entry.kind.clone()))
                .collect::<Vec<_>>(),
            vec![
                ("file-link", SourcePackageEntryKind::Symlink),
                ("missing-link", SourcePackageEntryKind::Symlink),
                ("nested/file.txt", SourcePackageEntryKind::File),
            ]
        );
        assert!(
            !String::from_utf8(first.decode_verified().expect("package"))
                .expect("JSON")
                .contains("/outside/secret")
        );
    }

    #[cfg(unix)]
    #[test]
    fn tracked_absolute_or_escaping_symlink_is_refused() {
        use std::os::unix::fs::symlink;

        for target in ["/outside", "../../outside"] {
            let source = tempfile::tempdir().expect("source");
            std::fs::write(source.path().join("file"), b"safe").expect("file");
            symlink(target, source.path().join("unsafe")).expect("link");
            for args in [["init"].as_slice(), ["add", "."].as_slice()] {
                assert!(Command::new("git")
                    .args(args)
                    .current_dir(source.path())
                    .status()
                    .expect("git")
                    .success());
            }
            let error = SourceArtifactTransfer::from_directory("source-1", source.path())
                .expect_err("unsafe link");
            assert!(error.message.contains("relative target contained"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn special_files_remain_rejected() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let source = tempfile::tempdir().expect("source");
        std::fs::write(source.path().join("file"), b"safe").expect("file");
        let fifo = source.path().join("special");
        let fifo = CString::new(fifo.as_os_str().as_bytes()).expect("path");
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        let error = SourceArtifactTransfer::from_directory("source-1", source.path())
            .expect_err("special file");
        assert!(error.message.contains("regular files and directories"));
    }

    #[cfg(unix)]
    #[test]
    fn source_root_and_regular_file_links_are_never_followed() {
        use std::os::unix::fs::symlink;

        let source = tempfile::tempdir().expect("source");
        std::fs::write(source.path().join("file"), b"safe").expect("file");
        let root_link = source.path().with_extension("link");
        symlink(source.path(), &root_link).expect("root link");
        let error =
            SourceArtifactTransfer::from_directory("source-1", &root_link).expect_err("root link");
        assert!(error.message.contains("non-symlink directory"));

        let replacement = source.path().join("replacement");
        symlink("file", &replacement).expect("replacement link");
        assert!(read_regular_file_nofollow(&replacement).is_err());
    }

    #[test]
    fn symlink_verdict_normalizes_windows_separators_and_rejects_windows_escape() {
        let verdict = source_package_symlink_verdict("links/tool", "..\\shared\\tool")
            .expect("normalized target");
        assert_eq!(verdict.target, "../shared/tool");
        assert_eq!(verdict.size_bytes, "../shared/tool".len() as u64);
        assert_eq!(
            verdict.sha256,
            format!("sha256:{:x}", Sha256::digest(b"../shared/tool"))
        );
        assert!(source_package_symlink_verdict("links/tool", "..\\..\\outside").is_err());
    }

    #[test]
    fn v2_source_artifacts_refuse_old_runners_before_admission() {
        let mut envelope = envelope();
        let artifact = envelope
            .materialization
            .source_artifact
            .as_mut()
            .expect("artifact");
        let v1: BTreeMap<String, String> =
            serde_json::from_slice(&artifact.decode_verified().expect("v1 package"))
                .expect("v1 JSON");
        let v2 = serde_json::to_vec(&BTreeMap::from([(
            "source.bin",
            SourcePackagePayload::File {
                content_base64: v1["source.bin"].clone(),
            },
        )]))
        .expect("v2 package");
        artifact.package.schema = "homeboy/source-package-manifest/v2".into();
        artifact.package.format = "homeboy/source-package-json/v2".into();
        artifact.sha256 = format!("sha256:{:x}", Sha256::digest(&v2));
        artifact.size_bytes = v2.len() as u64;
        artifact.content_base64 = base64::engine::general_purpose::STANDARD.encode(v2);
        let mut incompatible = Transport {
            symlink_artifact_compatible: false,
            ..transport()
        };
        let error = submit_remote_runner_staging(&mut incompatible, &envelope).expect_err("refuse");
        assert_eq!(error.code, homeboy_core::ErrorCode::RunnerCapabilityMissing);
        assert_eq!(incompatible.calls, 0);
    }
}
