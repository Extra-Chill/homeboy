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
use std::path::Path;

use homeboy_core::{Error, Result};

use crate::direct_lab_handoff::{DirectLabHandoffEnvelope, DirectLabHandoffReceipt};

pub const REMOTE_RUNNER_STAGING_SCHEMA: &str = "homeboy/remote-runner-staging/v1";
pub const REMOTE_RUNNER_STAGING_RECEIPT_SCHEMA: &str = "homeboy/remote-runner-staging-receipt/v1";
pub const REMOTE_RUNNER_STAGING_CAPABILITY: &str = "remote-runner-staging/v1";
pub const REMOTE_RUNNER_SOURCE_ARTIFACT_CAPABILITY: &str = "remote-runner-source-artifact/v1";
pub const SEALED_SOURCE_AUTHORITY_SCHEMA: &str = "homeboy/sealed-source-authority/v1";
pub const SOURCE_ARTIFACT_TRANSFER_SCHEMA: &str = "homeboy/runner-source-artifact-transfer/v1";
pub const RUNNER_SOURCE_ARTIFACT_SCHEMA: &str = "homeboy/runner-source-artifact/v1";
pub const MAX_SOURCE_ARTIFACT_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_SOURCE_PACKAGE_ENTRIES: usize = 1024;
pub const MAX_SOURCE_PACKAGE_FILE_BYTES: u64 = 1024 * 1024;
pub const SOURCE_PACKAGE_CHECK_SCHEMA: &str = "homeboy/source-package-check/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourcePackageExclusion {
    pub kind: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourcePackageFailure {
    pub kind: String,
    pub path: String,
    pub message: String,
}

/// Read-only, deterministic result of applying the sealed source-package policy.
///
/// Symlinks are recorded separately from failures so a future package format can
/// omit them without adding another traversal path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourcePackageCheckVerdict {
    pub schema: String,
    pub package_format: String,
    pub valid: bool,
    pub accepted_file_count: usize,
    pub accepted_bytes: u64,
    pub digest: String,
    pub exclusions: Vec<SourcePackageExclusion>,
    pub failures: Vec<SourcePackageFailure>,
}

#[derive(Debug, Clone)]
pub struct SourcePackageScan {
    pub verdict: SourcePackageCheckVerdict,
    files: BTreeMap<String, Vec<u8>>,
}

/// Apply the same source policy as sealed Lab staging without creating a
/// transfer, artifact, workspace, run, job, or connection.
pub fn scan_source_package(root: &Path) -> SourcePackageScan {
    fn failure(kind: &str, path: &Path, message: impl Into<String>) -> SourcePackageFailure {
        SourcePackageFailure {
            kind: kind.to_string(),
            path: path.display().to_string(),
            message: message.into(),
        }
    }

    fn collect(
        root: &Path,
        directory: &Path,
        files: &mut BTreeMap<String, Vec<u8>>,
        exclusions: &mut Vec<SourcePackageExclusion>,
        failures: &mut Vec<SourcePackageFailure>,
    ) {
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) => {
                failures.push(failure(
                    "unreadable_directory",
                    directory,
                    error.to_string(),
                ));
                return;
            }
        };
        let mut entries = match entries.collect::<std::result::Result<Vec<_>, _>>() {
            Ok(entries) => entries,
            Err(error) => {
                failures.push(failure(
                    "unreadable_directory",
                    directory,
                    error.to_string(),
                ));
                return;
            }
        };
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) => {
                    failures.push(failure("unreadable_entry", &path, error.to_string()));
                    continue;
                }
            };
            if metadata.file_type().is_symlink() {
                exclusions.push(SourcePackageExclusion {
                    kind: "symlink".to_string(),
                    path: path.display().to_string(),
                });
                failures.push(failure(
                    "symlink",
                    &path,
                    "source package accepts only regular files and directories",
                ));
                continue;
            }
            if metadata.is_dir() {
                collect(root, &path, files, exclusions, failures);
                continue;
            }
            if !metadata.is_file() {
                failures.push(failure(
                    "special_file",
                    &path,
                    "source package accepts only regular files and directories",
                ));
                continue;
            }
            if metadata.len() > MAX_SOURCE_PACKAGE_FILE_BYTES {
                failures.push(failure(
                    "file_too_large",
                    &path,
                    "source package file exceeds the configured size bound",
                ));
                continue;
            }
            let bytes = match fs::read(&path) {
                Ok(bytes) => bytes,
                Err(error) => {
                    failures.push(failure("unreadable_file", &path, error.to_string()));
                    continue;
                }
            };
            let relative = path.strip_prefix(root).expect("walk remains under root");
            files.insert(relative.to_string_lossy().replace('\\', "/"), bytes);
            let bytes = files.values().map(|bytes| bytes.len() as u64).sum::<u64>();
            if files.len() > MAX_SOURCE_PACKAGE_ENTRIES || bytes > MAX_SOURCE_ARTIFACT_BYTES {
                failures.push(failure(
                    "package_too_large",
                    root,
                    "source package exceeds configured entry or total size bounds",
                ));
                return;
            }
        }
    }

    let mut files = BTreeMap::new();
    let mut exclusions = Vec::new();
    let mut failures = Vec::new();
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            collect(root, root, &mut files, &mut exclusions, &mut failures)
        }
        Ok(_) => failures.push(failure(
            "invalid_root",
            root,
            "source package root must be a readable directory",
        )),
        Err(error) => failures.push(failure("unreadable_root", root, error.to_string())),
    }
    if failures.is_empty() && files.is_empty() {
        failures.push(failure(
            "empty_root",
            root,
            "source package root must contain at least one regular file",
        ));
    }
    let package = serde_json::to_vec(
        &files
            .iter()
            .map(|(path, bytes)| {
                (
                    path,
                    base64::engine::general_purpose::STANDARD.encode(bytes),
                )
            })
            .collect::<BTreeMap<_, _>>(),
    )
    .expect("source package serializes");
    SourcePackageScan {
        verdict: SourcePackageCheckVerdict {
            schema: SOURCE_PACKAGE_CHECK_SCHEMA.to_string(),
            package_format: "homeboy/source-package-json/v1".to_string(),
            valid: failures.is_empty(),
            accepted_file_count: files.len(),
            accepted_bytes: files.values().map(|bytes| bytes.len() as u64).sum(),
            digest: format!("sha256:{:x}", Sha256::digest(&package)),
            exclusions,
            failures,
        },
        files,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourcePackageEntry {
    pub path: String,
    pub sha256: String,
    pub size_bytes: u64,
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
    /// manifest. Traversal is lexical and deterministic; links and non-files
    /// are refused rather than crossing a controller filesystem boundary.
    pub fn from_directory(artifact_id: impl Into<String>, root: &Path) -> Result<Self> {
        let scan = scan_source_package(root);
        if let Some(failure) = scan.verdict.failures.first() {
            return Err(Error::validation_invalid_argument(
                "source_path",
                &failure.message,
                Some(failure.path.clone()),
                None,
            ));
        }
        let files = scan.files;
        let entries = files
            .iter()
            .map(|(path, bytes)| SourcePackageEntry {
                path: path.clone(),
                sha256: format!("sha256:{:x}", Sha256::digest(bytes)),
                size_bytes: bytes.len() as u64,
            })
            .collect::<Vec<_>>();
        let package = serde_json::to_vec(
            &files
                .iter()
                .map(|(path, bytes)| {
                    (
                        path,
                        base64::engine::general_purpose::STANDARD.encode(bytes),
                    )
                })
                .collect::<BTreeMap<_, _>>(),
        )
        .expect("source package serializes");
        let transfer = Self {
            schema: SOURCE_ARTIFACT_TRANSFER_SCHEMA.to_string(),
            artifact_id: artifact_id.into(),
            sha256: format!("sha256:{:x}", Sha256::digest(&package)),
            size_bytes: package.len() as u64,
            content_base64: base64::engine::general_purpose::STANDARD.encode(package),
            package: SourcePackageManifest {
                schema: "homeboy/source-package-manifest/v1".into(),
                format: "homeboy/source-package-json/v1".into(),
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
        if self.schema != "homeboy/source-package-manifest/v1"
            || self.format != "homeboy/source-package-json/v1"
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
        Ok(())
    }
    fn validate(&self, bytes: &[u8]) -> Result<()> {
        self.validate_shape()?;
        let files: BTreeMap<String, String> = serde_json::from_slice(bytes).map_err(|error| {
            Error::validation_invalid_argument("source_package", error.to_string(), None, None)
        })?;
        if files.len() != self.entries.len() {
            return Err(Error::validation_invalid_argument(
                "source_package",
                "source package entries do not match manifest",
                None,
                None,
            ));
        }
        for entry in &self.entries {
            let encoded = files.get(&entry.path).ok_or_else(|| {
                Error::validation_invalid_argument(
                    "source_package",
                    "source package entry is missing",
                    Some(entry.path.clone()),
                    None,
                )
            })?;
            let content = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|error| {
                    Error::validation_invalid_argument(
                        "source_package",
                        error.to_string(),
                        Some(entry.path.clone()),
                        None,
                    )
                })?;
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
    }

    #[test]
    fn source_package_check_matches_staging_and_reports_rejected_symlinks() {
        let source = tempfile::tempdir().expect("source");
        let external = tempfile::NamedTempFile::new().expect("external");
        std::fs::write(source.path().join("source.txt"), b"source").expect("source");
        #[cfg(unix)]
        std::os::unix::fs::symlink(external.path(), source.path().join("AGENTS.md")).expect("link");

        let first = scan_source_package(source.path());
        let second = scan_source_package(source.path());

        assert_eq!(first.verdict, second.verdict);
        #[cfg(unix)]
        {
            assert!(!first.verdict.valid);
            assert_eq!(first.verdict.accepted_file_count, 1);
            assert_eq!(first.verdict.exclusions[0].kind, "symlink");
            assert_eq!(first.verdict.failures[0].kind, "symlink");
            let error = SourceArtifactTransfer::from_directory("source-1", source.path())
                .expect_err("staging rejects the same symlink");
            assert!(error.message.contains(&first.verdict.failures[0].message));
        }
    }
}
