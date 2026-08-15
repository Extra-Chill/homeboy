//! Versioned remote admission for a runner-owned staging lifecycle.
//!
//! The controller turns its private source location into sealed source bytes
//! before this boundary. A remote runner receives neither that path nor an
//! instruction to resolve controller state; it durably owns materialization,
//! staging artifacts, and the replayable receipt.

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
#[cfg(unix)]
use std::fs;
use std::path::{Component, Path};
#[cfg(unix)]
use std::process::Command;

use homeboy_core::engine::canonical_json::canonical_json_bytes;
use homeboy_core::{Error, Result};
use homeboy_engine_primitives::content_hash;

use crate::direct_lab_handoff::{DirectLabHandoffEnvelope, DirectLabHandoffReceipt};

pub const REMOTE_RUNNER_STAGING_SCHEMA_V1: &str = "homeboy/remote-runner-staging/v1";
pub const REMOTE_RUNNER_STAGING_SCHEMA: &str = "homeboy/remote-runner-staging/v2";
pub const REMOTE_RUNNER_STAGING_RECEIPT_SCHEMA: &str = "homeboy/remote-runner-staging-receipt/v1";
pub const REMOTE_RUNNER_STAGING_CAPABILITY_V1: &str = "remote-runner-staging/v1";
pub const REMOTE_RUNNER_STAGING_CAPABILITY: &str = "remote-runner-staging/v2";
pub const REMOTE_RUNNER_SOURCE_MATERIALIZATION_CAPABILITY: &str =
    "remote-runner-source-materialization/v2";
pub const REMOTE_RUNNER_SOURCE_ARTIFACT_CAPABILITY: &str = "remote-runner-source-artifact/v1";
pub const REMOTE_RUNNER_SOURCE_ARTIFACT_SYMLINK_CAPABILITY: &str =
    "remote-runner-source-artifact/v2";
pub const SEALED_SOURCE_AUTHORITY_SCHEMA: &str = "homeboy/sealed-source-authority/v1";
pub const SOURCE_ARTIFACT_TRANSFER_SCHEMA: &str = "homeboy/runner-source-artifact-transfer/v1";
pub const RUNNER_SOURCE_ARTIFACT_SCHEMA: &str = "homeboy/runner-source-artifact/v1";
pub const MAX_SOURCE_ARTIFACT_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_SOURCE_PACKAGE_ENTRIES: usize = 1024;
pub const MAX_SOURCE_PACKAGE_FILE_BYTES: u64 = 1024 * 1024;
pub const MAX_SOURCE_PACKAGE_EXCLUSIONS: usize = 1024;
/// Legacy result schema accepted for persisted source-package checks.
pub const SOURCE_PACKAGE_CHECK_SCHEMA: &str = "homeboy/source-package-check/v1";
pub const SOURCE_PACKAGE_CHECK_SCHEMA_V2: &str = "homeboy/source-package-check/v2";
const SOURCE_PACKAGE_LIMIT_DIAGNOSTIC_SCHEMA: &str = "homeboy/source-package-limit-diagnostic/v1";

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
/// The accepted package identity is present only for a stageable result. A
/// blocked result has partial diagnostics without a package identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourcePackageCheckVerdict {
    pub schema: String,
    pub valid: bool,
    /// Present for v2 results. V1 records did not declare aggregate limits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<SourcePackageLimits>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepted: Option<SourcePackageAccepted>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partial: Option<SourcePackagePartial>,
    pub excluded: Vec<SourcePackageExclusion>,
    pub blocked: Vec<SourcePackageFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourcePackageAccepted {
    pub package_format: String,
    pub file_count: usize,
    pub bytes: u64,
    pub digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourcePackagePartial {
    pub file_count: usize,
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub largest_entries: Vec<SourcePackageContributor>,
}

/// The fixed safety bounds applied by sealed source-artifact staging.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourcePackageLimits {
    pub entry_limit: usize,
    pub byte_limit: u64,
    pub file_byte_limit: u64,
    pub exclusion_limit: usize,
}

/// A bounded, deterministic sample of entries consuming the package budget.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourcePackageContributor {
    pub path: String,
    pub bytes: u64,
}

#[derive(Debug, Clone)]
#[allow(
    dead_code,
    reason = "The scanner verdict is the public read-only surface."
)]
pub struct SourcePackageScan {
    pub verdict: SourcePackageCheckVerdict,
    payloads: BTreeMap<String, SourcePackagePayload>,
}

/// Apply the same source policy as sealed Lab staging without creating a
/// transfer, artifact, workspace, run, job, or connection.
#[allow(
    unreachable_code,
    reason = "The finalized sealed builder is the scanner authority."
)]
pub fn scan_source_package(root: &Path) -> SourcePackageScan {
    // `from_directory` is the sealed package scanner/builder. Reuse its output
    // so this read-only surface cannot drift from staging's v1/v2 identity.
    match SourceArtifactTransfer::from_directory_with_exclusions("source-package-check", root) {
        Ok((transfer, excluded)) => {
            let file_count = transfer.package.entries.len();
            let bytes = transfer
                .package
                .entries
                .iter()
                .map(|entry| entry.size_bytes)
                .sum();
            return SourcePackageScan {
                verdict: SourcePackageCheckVerdict {
                    schema: SOURCE_PACKAGE_CHECK_SCHEMA_V2.to_string(),
                    valid: true,
                    limits: Some(source_package_limits()),
                    accepted: Some(SourcePackageAccepted {
                        package_format: transfer.package.format,
                        file_count,
                        bytes,
                        digest: transfer.sha256,
                    }),
                    partial: None,
                    excluded,
                    blocked: Vec::new(),
                },
                payloads: BTreeMap::new(),
            };
        }
        Err(error) => {
            return SourcePackageScan {
                verdict: SourcePackageCheckVerdict {
                    schema: SOURCE_PACKAGE_CHECK_SCHEMA_V2.to_string(),
                    valid: false,
                    limits: Some(source_package_limits()),
                    accepted: None,
                    partial: source_package_partial_from_error(&error).or(Some(
                        SourcePackagePartial {
                            file_count: 0,
                            bytes: 0,
                            largest_entries: Vec::new(),
                        },
                    )),
                    excluded: Vec::new(),
                    blocked: vec![SourcePackageFailure {
                        kind: "source_package".to_string(),
                        path: root.display().to_string(),
                        message: error.message,
                    }],
                },
                payloads: BTreeMap::new(),
            };
        }
    }

    #[cfg(unix)]
    {
        #[allow(unreachable_code)]
        fn failure(kind: &str, path: &Path, message: impl Into<String>) -> SourcePackageFailure {
            SourcePackageFailure {
                kind: kind.to_string(),
                path: path.display().to_string(),
                message: message.into(),
            }
        }

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
            Ok(output
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
                .collect())
        }

        fn collect(
            root: &Path,
            directory: &Path,
            tracked_links: &BTreeSet<String>,
            payloads: &mut BTreeMap<String, SourcePackagePayload>,
            exclusions: &mut Vec<SourcePackageExclusion>,
            failures: &mut Vec<SourcePackageFailure>,
        ) -> bool {
            let entries = match fs::read_dir(directory) {
                Ok(entries) => entries,
                Err(error) => {
                    failures.push(failure(
                        "unreadable_directory",
                        directory,
                        error.to_string(),
                    ));
                    return false;
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
                    return false;
                }
            };
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let path = entry.path();
                if path.strip_prefix(root).expect("walk remains under root") == Path::new(".git") {
                    continue;
                }
                let metadata = match fs::symlink_metadata(&path) {
                    Ok(metadata) => metadata,
                    Err(error) => {
                        failures.push(failure("unreadable_entry", &path, error.to_string()));
                        return false;
                    }
                };
                if metadata.file_type().is_symlink() {
                    let relative = path.strip_prefix(root).expect("walk remains under root");
                    let relative = relative.to_string_lossy().replace('\\', "/");
                    if !tracked_links.contains(&relative) {
                        exclusions.push(SourcePackageExclusion {
                            kind: "untracked_symlink".to_string(),
                            path: path.display().to_string(),
                        });
                        continue;
                    }
                    let target = match fs::read_link(&path)
                        .map_err(|error| {
                            Error::internal_io(error.to_string(), Some(path.display().to_string()))
                        })
                        .and_then(|target| {
                            target.into_os_string().into_string().map_err(|_| {
                                Error::validation_invalid_argument(
                                    "source_path",
                                    "tracked source symlink target must be valid UTF-8 text",
                                    Some(path.display().to_string()),
                                    None,
                                )
                            })
                        })
                        .and_then(|target| source_package_symlink_verdict(&relative, &target))
                    {
                        Ok(verdict) => verdict,
                        Err(_error) => {
                            failures.push(failure(
                            "tracked_symlink",
                            &path,
                            "tracked source symlink must have a relative target contained within the source root",
                        ));
                            return false;
                        }
                    };
                    payloads.insert(
                        relative,
                        SourcePackagePayload::Symlink {
                            target: target.target,
                        },
                    );
                    if payloads.len() > MAX_SOURCE_PACKAGE_ENTRIES
                        || payloads
                            .values()
                            .map(SourcePackagePayload::size_bytes)
                            .sum::<u64>()
                            > MAX_SOURCE_ARTIFACT_BYTES
                    {
                        failures.push(failure(
                            "package_too_large",
                            root,
                            "source package exceeds configured entry or total size bounds",
                        ));
                        return false;
                    }
                    continue;
                }
                if metadata.is_dir() {
                    if !collect(root, &path, tracked_links, payloads, exclusions, failures) {
                        return false;
                    }
                    continue;
                }
                if !metadata.is_file() {
                    failures.push(failure(
                        "special_file",
                        &path,
                        "source package accepts only regular files and directories",
                    ));
                    return false;
                }
                let bytes = match read_regular_file_nofollow(&path) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        failures.push(failure("unreadable_file", &path, error.to_string()));
                        return false;
                    }
                };
                let relative = path.strip_prefix(root).expect("walk remains under root");
                payloads.insert(
                    relative.to_string_lossy().replace('\\', "/"),
                    SourcePackagePayload::File {
                        content_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
                    },
                );
                if payloads.len() > MAX_SOURCE_PACKAGE_ENTRIES
                    || payloads
                        .values()
                        .map(SourcePackagePayload::size_bytes)
                        .sum::<u64>()
                        > MAX_SOURCE_ARTIFACT_BYTES
                {
                    failures.push(failure(
                        "package_too_large",
                        root,
                        "source package exceeds configured entry or total size bounds",
                    ));
                    return false;
                }
            }
            true
        }

        let mut payloads = BTreeMap::new();
        let mut exclusions = Vec::new();
        let mut failures = Vec::new();
        match fs::symlink_metadata(root) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                match tracked_symlinks(root) {
                    Ok(tracked_links) => {
                        collect(
                            root,
                            root,
                            &tracked_links,
                            &mut payloads,
                            &mut exclusions,
                            &mut failures,
                        );
                    }
                    Err(error) => failures.push(failure("tracking_metadata", root, error.message)),
                }
            }
            Ok(_) => failures.push(failure(
                "invalid_root",
                root,
                "source package root must be a readable non-symlink directory",
            )),
            Err(error) => failures.push(failure("unreadable_root", root, error.to_string())),
        }
        if failures.is_empty() && payloads.is_empty() {
            failures.push(failure(
                "empty_root",
                root,
                "source package root must contain at least one regular file",
            ));
        }
        let file_count = payloads.len();
        let bytes = payloads
            .values()
            .map(SourcePackagePayload::size_bytes)
            .sum();
        let accepted = failures.is_empty().then(|| {
            let package = source_package_bytes(&payloads);
            SourcePackageAccepted {
                package_format: source_package_format(&payloads).to_string(),
                file_count,
                bytes,
                digest: format!("sha256:{:x}", Sha256::digest(&package)),
            }
        });
        SourcePackageScan {
            verdict: SourcePackageCheckVerdict {
                schema: SOURCE_PACKAGE_CHECK_SCHEMA_V2.to_string(),
                valid: failures.is_empty(),
                limits: Some(source_package_limits()),
                accepted,
                partial: (!failures.is_empty()).then_some(SourcePackagePartial {
                    file_count,
                    bytes,
                    largest_entries: source_package_contributors(&payloads),
                }),
                excluded: exclusions,
                blocked: failures,
            },
            payloads,
        }
    }
}

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

impl SourcePackagePayload {
    fn size_bytes(&self) -> u64 {
        match self {
            Self::File { content_base64 } => base64::engine::general_purpose::STANDARD
                .decode(content_base64)
                .expect("scanner produced base64")
                .len() as u64,
            Self::Symlink { target } => target.len() as u64,
        }
    }
}

fn source_package_limits() -> SourcePackageLimits {
    SourcePackageLimits {
        entry_limit: MAX_SOURCE_PACKAGE_ENTRIES,
        byte_limit: MAX_SOURCE_ARTIFACT_BYTES,
        file_byte_limit: MAX_SOURCE_PACKAGE_FILE_BYTES,
        exclusion_limit: MAX_SOURCE_PACKAGE_EXCLUSIONS,
    }
}

fn source_package_contributors(
    payloads: &BTreeMap<String, SourcePackagePayload>,
) -> Vec<SourcePackageContributor> {
    let mut entries = payloads
        .iter()
        .map(|(path, payload)| SourcePackageContributor {
            path: path.clone(),
            bytes: payload.size_bytes(),
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        right
            .bytes
            .cmp(&left.bytes)
            .then_with(|| left.path.cmp(&right.path))
    });
    entries.truncate(5);
    entries
}

fn source_package_partial(
    payloads: &BTreeMap<String, SourcePackagePayload>,
) -> SourcePackagePartial {
    SourcePackagePartial {
        file_count: payloads.len(),
        bytes: payloads
            .values()
            .map(SourcePackagePayload::size_bytes)
            .sum(),
        largest_entries: source_package_contributors(payloads),
    }
}

fn source_package_partial_from_error(error: &Error) -> Option<SourcePackagePartial> {
    serde_json::from_value(
        error
            .details
            .get("source_package")?
            .get("measured")?
            .clone(),
    )
    .ok()
}

fn source_package_limit_error(
    root: &Path,
    payloads: &BTreeMap<String, SourcePackagePayload>,
) -> Error {
    let measured = source_package_partial(payloads);
    let limits = source_package_limits();
    let check_command = format!(
        "homeboy source package check --path {}",
        homeboy_core::engine::shell::quote_arg(&root.display().to_string())
    );
    let retry = "Retry the original Lab command after reducing the source package or selecting a Git-capable Lab route.";
    let mut error = Error::validation_invalid_argument(
        "source_path",
        format!(
            "source package exceeds configured bounds: measured {} entries / {} bytes; limits {} entries / {} bytes",
            measured.file_count, measured.bytes, limits.entry_limit, limits.byte_limit,
        ),
        Some(root.display().to_string()),
        Some(vec![
            format!("Inspect the exact package budget and excluded paths: {check_command}"),
            "Remove generated or vendored files from the source worktree before retrying; `.git` and untracked symlinks are excluded automatically.".to_string(),
            retry.to_string(),
        ]),
    );
    error.details["source_package"] = json!({
        "schema": SOURCE_PACKAGE_LIMIT_DIAGNOSTIC_SCHEMA,
        "measured": measured,
        "limits": limits,
        "excluded_path_policy": {
            "excluded": [".git", "untracked_symlink"],
            "tracked_symlinks": "included only when their relative target remains inside the source root"
        },
        "continuation": {
            "preflight_command": check_command,
            "retry": retry,
            "scalable_transport": "Git workspace materialization is selected automatically for eligible clean repository worktrees."
        }
    });
    error
}

fn source_package_format(payloads: &BTreeMap<String, SourcePackagePayload>) -> &'static str {
    if payloads
        .values()
        .any(|payload| matches!(payload, SourcePackagePayload::Symlink { .. }))
    {
        "homeboy/source-package-json/v2"
    } else {
        "homeboy/source-package-json/v1"
    }
}

fn source_package_bytes(payloads: &BTreeMap<String, SourcePackagePayload>) -> Vec<u8> {
    if source_package_format(payloads) == "homeboy/source-package-json/v2" {
        serde_json::to_vec(payloads)
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
    .expect("source package serializes")
}

#[cfg(unix)]
fn read_regular_file_nofollow(path: &Path) -> Result<Vec<u8>> {
    fs::read(path)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))
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

/// Applies the producing platform's separator contract before the v2 lexical
/// containment policy. Unix keeps backslashes literal; Windows serializes
/// separators as `/`. V1 packages do not admit symlinks.
pub fn source_package_symlink_verdict(
    link_path: &str,
    target: &str,
) -> Result<SourcePackageSymlinkVerdict> {
    let link_path = source_package_path_text(link_path);
    let target = source_package_path_text(target);
    if target.is_empty() {
        return Err(Error::validation_invalid_argument(
            "source_package",
            "source package symlink target must be a non-empty relative in-tree path",
            Some(link_path.clone()),
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

fn source_package_path_text(value: &str) -> String {
    #[cfg(windows)]
    {
        value.replace('\\', "/")
    }
    #[cfg(not(windows))]
    {
        value.to_string()
    }
}

#[cfg(unix)]
mod source_directory {
    use std::ffi::{CStr, CString, OsStr, OsString};
    use std::fs::File;
    use std::io::Read;
    use std::os::fd::{AsRawFd, FromRawFd, RawFd};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::path::Path;

    use homeboy_core::{Error, Result};

    use super::{
        MAX_SOURCE_ARTIFACT_BYTES, MAX_SOURCE_PACKAGE_ENTRIES, MAX_SOURCE_PACKAGE_FILE_BYTES,
        SOURCE_PACKAGE_LIMIT_DIAGNOSTIC_SCHEMA,
    };

    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd"
    ))]
    unsafe fn clear_errno() {
        *libc::__error() = 0;
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    unsafe fn clear_errno() {
        *libc::__errno_location() = 0;
    }

    #[cfg(not(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "linux",
        target_os = "android"
    )))]
    unsafe fn clear_errno() {}

    fn c_string(value: &OsStr) -> Result<CString> {
        CString::new(value.as_bytes()).map_err(|_| {
            Error::validation_invalid_argument(
                "source_path",
                "source package paths cannot contain NUL bytes",
                None,
                None,
            )
        })
    }

    fn open_at(directory: RawFd, name: &OsStr, flags: libc::c_int) -> Result<File> {
        let name = c_string(name)?;
        let descriptor =
            unsafe { libc::openat(directory, name.as_ptr(), flags | libc::O_NOFOLLOW) };
        if descriptor < 0 {
            return Err(Error::internal_io(
                std::io::Error::last_os_error().to_string(),
                Some(name.to_string_lossy().into_owned()),
            ));
        }
        // Git uses `fchdir` before exec; close this descriptor at exec.
        let descriptor_flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
        if descriptor_flags < 0
            || unsafe {
                libc::fcntl(
                    descriptor,
                    libc::F_SETFD,
                    descriptor_flags | libc::FD_CLOEXEC,
                )
            } < 0
        {
            let error = std::io::Error::last_os_error();
            unsafe { libc::close(descriptor) };
            return Err(Error::internal_io(error.to_string(), None));
        }
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }

    pub(super) fn open_root(root: &Path) -> Result<File> {
        open_at(
            libc::AT_FDCWD,
            root.as_os_str(),
            libc::O_RDONLY | libc::O_DIRECTORY,
        )
    }

    pub(super) fn directory_entries(directory: &File) -> Result<Vec<OsString>> {
        let duplicate = unsafe { libc::dup(directory.as_raw_fd()) };
        if duplicate < 0 {
            return Err(Error::internal_io(
                std::io::Error::last_os_error().to_string(),
                None,
            ));
        }
        let stream = unsafe { libc::fdopendir(duplicate) };
        if stream.is_null() {
            unsafe { libc::close(duplicate) };
            return Err(Error::internal_io(
                std::io::Error::last_os_error().to_string(),
                None,
            ));
        }
        let mut entries = Vec::new();
        loop {
            unsafe { clear_errno() };
            let entry = unsafe { libc::readdir(stream) };
            if entry.is_null() {
                let error = std::io::Error::last_os_error();
                unsafe { libc::closedir(stream) };
                if error.raw_os_error() == Some(0) {
                    break;
                }
                return Err(Error::internal_io(error.to_string(), None));
            }
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if name != b"." && name != b".." {
                entries.push(OsString::from_vec(name.to_vec()));
            }
        }
        entries.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
        Ok(entries)
    }

    pub(super) fn mode_at(directory: &File, name: &OsStr) -> Result<libc::mode_t> {
        let name = c_string(name)?;
        let mut stat = unsafe { std::mem::zeroed::<libc::stat>() };
        if unsafe {
            libc::fstatat(
                directory.as_raw_fd(),
                name.as_ptr(),
                &mut stat,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return Err(Error::internal_io(
                std::io::Error::last_os_error().to_string(),
                Some(name.to_string_lossy().into_owned()),
            ));
        }
        Ok(stat.st_mode)
    }

    pub(super) fn read_link_at(directory: &File, name: &OsStr) -> Result<OsString> {
        let name = c_string(name)?;
        let mut capacity = 256;
        loop {
            let mut target = vec![0u8; capacity];
            let length = unsafe {
                libc::readlinkat(
                    directory.as_raw_fd(),
                    name.as_ptr(),
                    target.as_mut_ptr().cast(),
                    target.len(),
                )
            };
            if length < 0 {
                return Err(Error::internal_io(
                    std::io::Error::last_os_error().to_string(),
                    Some(name.to_string_lossy().into_owned()),
                ));
            }
            let length = length as usize;
            if length < target.len() {
                target.truncate(length);
                return Ok(OsString::from_vec(target));
            }
            capacity *= 2;
            if capacity > 64 * 1024 {
                return Err(Error::validation_invalid_argument(
                    "source_path",
                    "source package symlink target exceeds the configured bound",
                    Some(name.to_string_lossy().into_owned()),
                    None,
                ));
            }
        }
    }

    pub(super) fn open_directory_at(directory: &File, name: &OsStr) -> Result<File> {
        open_at(
            directory.as_raw_fd(),
            name,
            libc::O_RDONLY | libc::O_DIRECTORY,
        )
    }

    pub(super) fn read_regular_file_at(directory: &File, name: &OsStr) -> Result<Vec<u8>> {
        let file = open_at(directory.as_raw_fd(), name, libc::O_RDONLY)?;
        let metadata = file.metadata().map_err(|error| {
            Error::internal_io(error.to_string(), Some(name.to_string_lossy().into_owned()))
        })?;
        if !metadata.is_file() {
            return Err(Error::validation_invalid_argument(
                "source_path",
                "source package file must remain a regular file when opened",
                Some(name.to_string_lossy().into_owned()),
                None,
            ));
        }
        if metadata.len() > MAX_SOURCE_PACKAGE_FILE_BYTES {
            let mut error = Error::validation_invalid_argument(
                "source_path",
                "source package file exceeds the configured size bound",
                Some(name.to_string_lossy().into_owned()),
                None,
            );
            error.details["source_package"] = serde_json::json!({
                "schema": SOURCE_PACKAGE_LIMIT_DIAGNOSTIC_SCHEMA,
                "measured": { "file_count": 1, "bytes": metadata.len(), "largest_entries": [] },
                "limits": {
                    "entry_limit": MAX_SOURCE_PACKAGE_ENTRIES,
                    "byte_limit": MAX_SOURCE_ARTIFACT_BYTES,
                    "file_byte_limit": MAX_SOURCE_PACKAGE_FILE_BYTES,
                    "exclusion_limit": super::MAX_SOURCE_PACKAGE_EXCLUSIONS,
                },
            });
            return Err(error);
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.take(MAX_SOURCE_PACKAGE_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| {
                Error::internal_io(error.to_string(), Some(name.to_string_lossy().into_owned()))
            })?;
        if bytes.len() as u64 > MAX_SOURCE_PACKAGE_FILE_BYTES {
            return Err(Error::validation_invalid_argument(
                "source_path",
                "source package file exceeds the configured size bound",
                Some(name.to_string_lossy().into_owned()),
                None,
            ));
        }
        Ok(bytes)
    }
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
        Self::from_directory_with_exclusions(artifact_id, root).map(|(transfer, _)| transfer)
    }

    fn from_directory_with_exclusions(
        artifact_id: impl Into<String>,
        root: &Path,
    ) -> Result<(Self, Vec<SourcePackageExclusion>)> {
        #[cfg(not(unix))]
        {
            let _ = artifact_id;
            return Err(Error::validation_invalid_argument(
                "source_path",
                "source package directory scanning requires descriptor-relative no-follow traversal",
                Some(root.display().to_string()),
                None,
            ));
        }
        #[cfg(unix)]
        {
            fn tracked_symlinks(root: &std::fs::File) -> Result<BTreeSet<String>> {
                use std::os::fd::AsRawFd;
                use std::os::unix::process::CommandExt;

                let root_fd = root.as_raw_fd();
                let mut command = Command::new("git");
                command.args(["ls-files", "--stage", "-z", "--", "."]);
                // `fchdir` binds Git's index inspection to the retained root
                // descriptor, avoiding a second lookup of the mutable root path.
                unsafe {
                    command.pre_exec(move || {
                        if libc::fchdir(root_fd) == 0 {
                            Ok(())
                        } else {
                            Err(std::io::Error::last_os_error())
                        }
                    });
                }
                let output = match command.output() {
                    Ok(output) => output,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        return Ok(BTreeSet::new())
                    }
                    Err(error) => return Err(Error::internal_io(error.to_string(), None)),
                };
                if !output.status.success() {
                    // A non-Git source directory has no authoritative link inventory.
                    if String::from_utf8_lossy(&output.stderr).contains("not a git repository") {
                        return Ok(BTreeSet::new());
                    }
                    return Err(Error::validation_invalid_argument(
                        "source_path",
                        format!(
                            "could not read Git tracking metadata for source package: {}",
                            String::from_utf8_lossy(&output.stderr).trim()
                        ),
                        None,
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
                        (metadata.starts_with(b"120000 ") && std::str::from_utf8(path).is_ok())
                            .then(|| {
                                source_package_path_text(
                                    std::str::from_utf8(path).expect("validated UTF-8"),
                                )
                            })
                    })
                    .collect::<BTreeSet<_>>();
                Ok(links)
            }
            #[cfg(unix)]
            fn collect(
                root: &Path,
                directory: &std::fs::File,
                relative_directory: &str,
                tracked_links: &BTreeSet<String>,
                entries: &mut BTreeMap<String, SourcePackagePayload>,
                exclusions: &mut Vec<SourcePackageExclusion>,
            ) -> Result<()> {
                for name in source_directory::directory_entries(directory)? {
                    let name_text = name.to_str().ok_or_else(|| {
                        Error::validation_invalid_argument(
                            "source_path",
                            "source package paths must be valid UTF-8 text",
                            None,
                            None,
                        )
                    })?;
                    let relative = if relative_directory.is_empty() {
                        name_text.to_string()
                    } else {
                        format!("{relative_directory}/{name_text}")
                    };
                    // Git metadata is controller-local state, never source content.
                    if relative == ".git" {
                        continue;
                    }
                    let mode = source_directory::mode_at(directory, &name)?;
                    match mode & libc::S_IFMT {
                        libc::S_IFLNK => {
                            if !tracked_links.contains(&relative) {
                                if exclusions.len() >= MAX_SOURCE_PACKAGE_EXCLUSIONS {
                                    return Err(Error::validation_invalid_argument(
                                        "source_path",
                                        "source package exceeds configured exclusion bound",
                                        Some(relative),
                                        None,
                                    ));
                                }
                                exclusions.push(SourcePackageExclusion {
                                    kind: "untracked_symlink".to_string(),
                                    path: relative,
                                });
                                continue;
                            }
                            let target = source_directory::read_link_at(directory, &name)?;
                            let target = target.into_string().map_err(|target| {
                                Error::validation_invalid_argument(
                                    "source_path",
                                    "tracked source symlink target must be valid UTF-8 text",
                                    Some(format!("{} -> {}", relative, target.to_string_lossy())),
                                    None,
                                )
                            })?;
                            let verdict = source_package_symlink_verdict(&relative, &target).map_err(|_| {
                        Error::validation_invalid_argument(
                            "source_path",
                            "tracked source symlink must have a relative target contained within the source root",
                            Some(format!("{relative} -> {target}")),
                            None,
                        )
                    })?;
                            entries.insert(
                                relative,
                                SourcePackagePayload::Symlink {
                                    target: verdict.target,
                                },
                            );
                            if entries.len() > MAX_SOURCE_PACKAGE_ENTRIES
                                || entries
                                    .values()
                                    .map(SourcePackagePayload::size_bytes)
                                    .sum::<u64>()
                                    > MAX_SOURCE_ARTIFACT_BYTES
                            {
                                return Err(source_package_limit_error(root, entries));
                            }
                            continue;
                        }
                        libc::S_IFDIR => {
                            let child = source_directory::open_directory_at(directory, &name)?;
                            collect(root, &child, &relative, tracked_links, entries, exclusions)?;
                        }
                        libc::S_IFREG => {
                            let bytes = source_directory::read_regular_file_at(directory, &name)?;
                            entries.insert(
                                relative,
                                SourcePackagePayload::File {
                                    content_base64: base64::engine::general_purpose::STANDARD
                                        .encode(bytes),
                                },
                            );
                        }
                        _ => {
                            return Err(Error::validation_invalid_argument(
                                "source_path",
                                "source package accepts only regular files and directories",
                                Some(relative),
                                None,
                            ));
                        }
                    }
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
                        return Err(source_package_limit_error(root, entries));
                    }
                }
                Ok(())
            }

            #[cfg(unix)]
            let root_metadata = fs::symlink_metadata(root).map_err(|error| {
                Error::internal_io(error.to_string(), Some(root.display().to_string()))
            })?;
            #[cfg(unix)]
            if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
                return Err(Error::validation_invalid_argument(
                    "source_path",
                    "source package root must be a readable non-symlink directory",
                    Some(root.display().to_string()),
                    None,
                ));
            }
            #[cfg(unix)]
            let root_directory = source_directory::open_root(root)?;
            #[cfg(unix)]
            let tracked_links = tracked_symlinks(&root_directory)?;
            let mut payloads = BTreeMap::new();
            let mut exclusions = Vec::new();
            #[cfg(unix)]
            collect(
                root,
                &root_directory,
                "",
                &tracked_links,
                &mut payloads,
                &mut exclusions,
            )?;
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
            // The HTTP envelope carries Base64, not raw package bytes. Keep the
            // inline transport bound truthful and route its overflow through
            // the same typed scalable-materialization decision.
            if transfer.content_base64.len() as u64 > MAX_SOURCE_ARTIFACT_BYTES {
                let mut error = Error::validation_invalid_argument(
                    "source_path",
                    "source package exceeds the encoded inline transfer bound",
                    Some(root.display().to_string()),
                    None,
                );
                error.details["source_package"] = json!({
                    "schema": SOURCE_PACKAGE_LIMIT_DIAGNOSTIC_SCHEMA,
                    "measured": { "file_count": transfer.package.entries.len(), "bytes": transfer.content_base64.len(), "largest_entries": [] },
                    "limits": { "entry_limit": MAX_SOURCE_PACKAGE_ENTRIES, "byte_limit": MAX_SOURCE_ARTIFACT_BYTES, "file_byte_limit": MAX_SOURCE_PACKAGE_FILE_BYTES, "exclusion_limit": MAX_SOURCE_PACKAGE_EXCLUSIONS }
                });
                return Err(error);
            }
            transfer.decode_verified()?;
            Ok((transfer, exclusions))
        }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<ControllerWorkspaceMaterialization>,
}

/// A controller-routed workspace has already crossed the controller-to-runner
/// boundary. It identifies the runner-owned execution target without exporting
/// controller paths or reopening the runner's private Git-origin access.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControllerWorkspaceMaterialization {
    pub schema: String,
    pub workspace_id: String,
    pub remote_cwd: String,
    pub source_snapshot_id: String,
    pub workspace_lease: String,
    pub source_commit: String,
    pub run_id: String,
    pub durable_plan_digest: String,
}

pub const CONTROLLER_WORKSPACE_MATERIALIZATION_SCHEMA: &str =
    "homeboy/controller-workspace-materialization/v1";

impl ControllerWorkspaceMaterialization {
    pub fn new(
        workspace_id: impl Into<String>,
        remote_cwd: impl Into<String>,
        source_snapshot_id: impl Into<String>,
        workspace_lease: impl Into<String>,
        source_commit: impl Into<String>,
        run_id: impl Into<String>,
        durable_plan_digest: impl Into<String>,
    ) -> Self {
        Self {
            schema: CONTROLLER_WORKSPACE_MATERIALIZATION_SCHEMA.to_string(),
            workspace_id: workspace_id.into(),
            remote_cwd: remote_cwd.into(),
            source_snapshot_id: source_snapshot_id.into(),
            workspace_lease: workspace_lease.into(),
            source_commit: source_commit.into(),
            run_id: run_id.into(),
            durable_plan_digest: durable_plan_digest.into(),
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema != CONTROLLER_WORKSPACE_MATERIALIZATION_SCHEMA
            || self.workspace_id.trim().is_empty()
            || self.remote_cwd.trim().is_empty()
            || self.source_snapshot_id.trim().is_empty()
            || self.workspace_lease.trim().is_empty()
            || self.source_commit.len() != 40
            || !self
                .source_commit
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || self.run_id.trim().is_empty()
            || !self.durable_plan_digest.starts_with("sha256:")
        {
            return Err(Error::validation_invalid_argument(
                "controller_workspace_materialization",
                "remote staging requires a v1 runner workspace target, source identity, and durable-plan digest",
                None,
                None,
            ));
        }
        Ok(())
    }
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
        match (&self.source_artifact, &self.workspace) {
            (Some(artifact), None) => artifact.decode_verified().map(|_| ()),
            (None, Some(workspace)) => workspace.validate(),
            _ => Err(Error::validation_invalid_argument(
                "source_materialization",
                "remote staging requires exactly one bounded source artifact or controller-materialized workspace",
                Some(self.authority_id.clone()),
                None,
            )),
        }
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
            schema: if handoff.schema == crate::direct_lab_handoff::DIRECT_LAB_HANDOFF_SCHEMA_V1 {
                REMOTE_RUNNER_STAGING_SCHEMA_V1.to_string()
            } else {
                REMOTE_RUNNER_STAGING_SCHEMA.to_string()
            },
            handoff,
            materialization,
        };
        envelope.validate()?;
        Ok(envelope)
    }

    pub fn validate(&self) -> Result<()> {
        if !matches!(
            self.schema.as_str(),
            REMOTE_RUNNER_STAGING_SCHEMA_V1 | REMOTE_RUNNER_STAGING_SCHEMA
        ) || self.handoff.recipe.source_path.is_some()
            || !matches!(
                self.handoff.schema.as_str(),
                crate::direct_lab_handoff::DIRECT_LAB_HANDOFF_SCHEMA_V1
                    | crate::direct_lab_handoff::DIRECT_LAB_HANDOFF_SCHEMA
            )
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
                "remote staging requires its v2 schema, bound handoff identities, and no controller-local source path",
                Some(self.handoff.run_id.clone()),
                None,
            ));
        }
        self.handoff.recipe.validate_for_runner_staging()?;
        self.materialization.validate()?;
        if self.schema == REMOTE_RUNNER_STAGING_SCHEMA_V1
            && (self.handoff.schema != crate::direct_lab_handoff::DIRECT_LAB_HANDOFF_SCHEMA_V1
                || self.materialization.source_artifact.is_none()
                || self.materialization.workspace.is_some())
        {
            return Err(Error::validation_invalid_argument(
                "remote_runner_staging",
                "v1 staging accepts bounded source artifacts only",
                Some(self.handoff.run_id.clone()),
                None,
            ));
        }
        if let Some(workspace) = &self.materialization.workspace {
            let plan = canonical_json_bytes(&self.handoff.durable_plan).map_err(|error| {
                Error::internal_json(
                    error.to_string(),
                    Some("canonicalize staged durable plan".to_string()),
                )
            })?;
            if workspace.durable_plan_digest
                != format!("sha256:{}", content_hash::sha256_hex(&plan))
            {
                return Err(Error::validation_invalid_argument(
                    "controller_workspace_materialization.durable_plan_digest",
                    "controller-materialized workspace is not bound to this durable plan",
                    Some(self.handoff.run_id.clone()),
                    None,
                ));
            }
        }
        Ok(())
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
        match (
            &self.artifacts.source_artifact,
            &envelope.materialization.workspace,
        ) {
            (Some(artifact), None) => artifact.validate()?,
            (None, Some(_)) => {}
            _ => {
                return Err(Error::validation_invalid_argument(
                    "remote_runner_staging_receipt.source_materialization",
                    "remote staging receipt does not match its accepted source materialization",
                    Some(envelope.handoff.run_id.clone()),
                    None,
                ));
            }
        }
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
    let staging_capability = if envelope.schema == REMOTE_RUNNER_STAGING_SCHEMA_V1 {
        REMOTE_RUNNER_STAGING_CAPABILITY_V1
    } else {
        REMOTE_RUNNER_STAGING_CAPABILITY
    };
    if !transport.supports_capability(staging_capability) {
        return Err(Error::runner_capability_missing(
            &envelope.handoff.runner_id,
            "sealed runner staging",
            vec![staging_capability.to_string()],
            Vec::new(),
        ));
    }
    if envelope.materialization.workspace.is_some()
        && !transport.supports_capability(REMOTE_RUNNER_SOURCE_MATERIALIZATION_CAPABILITY)
    {
        return Err(Error::runner_capability_missing(
            &envelope.handoff.runner_id,
            "versioned runner source materialization",
            vec![REMOTE_RUNNER_SOURCE_MATERIALIZATION_CAPABILITY.to_string()],
            Vec::new(),
        ));
    }
    if envelope.materialization.source_artifact.is_some()
        && !transport.supports_capability(REMOTE_RUNNER_SOURCE_ARTIFACT_CAPABILITY)
    {
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
            (self.compatible
                && matches!(
                    capability,
                    REMOTE_RUNNER_STAGING_CAPABILITY | REMOTE_RUNNER_STAGING_CAPABILITY_V1
                ))
                || (self.compatible
                    && capability == REMOTE_RUNNER_SOURCE_MATERIALIZATION_CAPABILITY)
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
                workspace: None,
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
    fn controller_materialized_workspace_is_accepted_without_source_bytes() {
        let mut envelope = envelope();
        let durable_plan_digest = format!(
            "sha256:{}",
            content_hash::sha256_hex(
                &canonical_json_bytes(&envelope.handoff.durable_plan).expect("canonical plan"),
            )
        );
        envelope.materialization.source_artifact = None;
        envelope.materialization.workspace = Some(ControllerWorkspaceMaterialization::new(
            "runner-workspace-1",
            "/runner/workspaces/run-1",
            "git:abc123",
            "workspace:lease-1",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "run-1",
            durable_plan_digest,
        ));
        let receipt = submit_remote_runner_staging(&mut transport(), &envelope)
            .expect("accept controller-materialized source");
        assert!(receipt.artifacts.source_artifact.is_none());
        assert_eq!(
            envelope
                .materialization
                .workspace
                .as_ref()
                .expect("workspace")
                .remote_cwd,
            "/runner/workspaces/run-1"
        );
    }

    #[test]
    fn bounded_artifact_v1_interoperates_with_a_new_runner() {
        let mut envelope = envelope();
        envelope.handoff.schema =
            crate::direct_lab_handoff::DIRECT_LAB_HANDOFF_SCHEMA_V1.to_string();
        envelope.schema = REMOTE_RUNNER_STAGING_SCHEMA_V1.to_string();
        let receipt = submit_remote_runner_staging(&mut transport(), &envelope)
            .expect("new runner accepts old bounded staging");
        assert!(receipt.artifacts.source_artifact.is_some());
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

        let check = scan_source_package(source.path()).verdict;
        let first =
            SourceArtifactTransfer::from_directory("source-1", source.path()).expect("pack");
        let second =
            SourceArtifactTransfer::from_directory("source-1", source.path()).expect("repack");
        assert_eq!(first, second);
        assert!(check.valid);
        assert_eq!(
            check.accepted.as_ref().expect("accepted").package_format,
            "homeboy/source-package-json/v2"
        );
        assert_eq!(
            check.accepted.as_ref().expect("accepted").digest,
            first.sha256
        );
        assert_eq!(check.excluded[0].kind, "untracked_symlink");
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
            let check = scan_source_package(source.path()).verdict;
            assert!(!check.valid);
            assert!(check.accepted.is_none());
            assert_eq!(check.blocked.len(), 1);
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
        let check = scan_source_package(source.path()).verdict;
        assert!(!check.valid);
        assert_eq!(check.blocked.len(), 1);
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
        let root = source_directory::open_root(source.path()).expect("root descriptor");
        assert!(
            source_directory::read_regular_file_at(&root, std::ffi::OsStr::new("replacement"))
                .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn retained_directory_descriptor_cannot_be_redirected_by_a_rename_to_symlink() {
        use std::os::unix::fs::symlink;

        let source = tempfile::tempdir().expect("source");
        let outside = tempfile::tempdir().expect("outside");
        std::fs::create_dir(source.path().join("nested")).expect("nested");
        std::fs::write(source.path().join("nested/safe"), b"safe").expect("safe");
        std::fs::write(outside.path().join("outside"), b"outside").expect("outside file");
        let root = source_directory::open_root(source.path()).expect("root descriptor");
        let nested = source_directory::open_directory_at(&root, std::ffi::OsStr::new("nested"))
            .expect("nested descriptor");
        std::fs::rename(source.path().join("nested"), source.path().join("moved"))
            .expect("move nested");
        symlink(outside.path(), source.path().join("nested")).expect("replacement link");

        assert_eq!(
            source_directory::directory_entries(&nested)
                .expect("retained entries")
                .iter()
                .map(|entry| entry.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            ["safe"]
        );
    }

    #[test]
    fn symlink_verdict_normalizes_windows_separators_and_rejects_windows_escape() {
        let verdict = source_package_symlink_verdict("links\\tool", "..\\shared\\tool")
            .expect("normalized target");
        #[cfg(windows)]
        let expected_target = "../shared/tool";
        #[cfg(not(windows))]
        let expected_target = "..\\shared\\tool";
        assert_eq!(verdict.target, expected_target);
        assert_eq!(verdict.size_bytes, expected_target.len() as u64);
        assert_eq!(
            verdict.sha256,
            format!("sha256:{:x}", Sha256::digest(expected_target.as_bytes()))
        );
        #[cfg(windows)]
        assert!(source_package_symlink_verdict("links/tool", "..\\..\\outside").is_err());
        #[cfg(not(windows))]
        assert!(source_package_symlink_verdict("links/tool", "/outside").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn tracked_backslash_link_cannot_authorize_an_untracked_slash_link() {
        use std::os::unix::fs::symlink;

        let source = tempfile::tempdir().expect("source");
        std::fs::write(source.path().join("file"), b"safe").expect("file");
        symlink("file", source.path().join("link\\literal")).expect("tracked literal");
        std::fs::create_dir(source.path().join("link")).expect("untracked parent");
        symlink("/outside/secret", source.path().join("link/literal")).expect("untracked slash");
        for args in [
            ["init"].as_slice(),
            ["add", "file", "link\\literal"].as_slice(),
        ] {
            assert!(Command::new("git")
                .args(args)
                .current_dir(source.path())
                .status()
                .expect("git")
                .success());
        }

        let error = SourceArtifactTransfer::from_directory("source-1", source.path())
            .expect_err("backslash package path is not portable");
        assert!(error.message.contains("unsafe, duplicate, or oversized"));
        assert!(!error.message.contains("/outside/secret"));
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

    #[test]
    fn source_package_check_omits_ignored_injected_symlinks() {
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
            assert!(first.verdict.valid);
            assert_eq!(first.verdict.excluded[0].kind, "untracked_symlink");
            let transfer = SourceArtifactTransfer::from_directory("source-1", source.path())
                .expect("staging omits the same symlink");
            assert_eq!(
                first.verdict.accepted.expect("accepted").digest,
                transfer.sha256
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn source_package_check_exclusions_are_lexical_bounded_and_never_read_targets() {
        use std::os::unix::fs::symlink;

        let source = tempfile::tempdir().expect("source");
        std::fs::write(source.path().join("file"), b"safe").expect("file");
        symlink("/outside/first-secret", source.path().join("a-link")).expect("first link");
        symlink("/outside/second-secret", source.path().join("z-link")).expect("second link");

        let first = scan_source_package(source.path()).verdict;
        let second = scan_source_package(source.path()).verdict;
        let transfer = SourceArtifactTransfer::from_directory("source-1", source.path())
            .expect("staging package");

        assert_eq!(first, second);
        assert_eq!(
            first
                .excluded
                .iter()
                .map(|entry| (entry.path.as_str(), entry.kind.as_str()))
                .collect::<Vec<_>>(),
            [
                ("a-link", "untracked_symlink"),
                ("z-link", "untracked_symlink")
            ]
        );
        assert!(
            !String::from_utf8(transfer.decode_verified().expect("package"))
                .expect("json")
                .contains("/outside/")
        );
    }

    #[cfg(unix)]
    #[test]
    fn source_package_check_refuses_exclusions_past_the_deterministic_bound() {
        use std::os::unix::fs::symlink;

        let source = tempfile::tempdir().expect("source");
        std::fs::write(source.path().join("file"), b"safe").expect("file");
        for index in 0..=MAX_SOURCE_PACKAGE_EXCLUSIONS {
            symlink("/outside", source.path().join(format!("link-{index:04}"))).expect("link");
        }

        let verdict = scan_source_package(source.path()).verdict;

        assert!(!verdict.valid);
        assert!(verdict.accepted.is_none());
        assert_eq!(verdict.blocked.len(), 1);
        assert!(verdict.blocked[0].message.contains("exclusion bound"));
    }

    #[cfg(unix)]
    #[test]
    fn source_package_check_stops_at_the_first_nested_over_limit_entry() {
        let source = tempfile::tempdir().expect("source");
        let nested = source.path().join("a");
        std::fs::create_dir(&nested).expect("nested");
        for index in 0..=MAX_SOURCE_PACKAGE_ENTRIES {
            std::fs::write(nested.join(format!("{index:04}")), b"").expect("file");
        }
        std::os::unix::fs::symlink("outside", source.path().join("z-link")).expect("link");

        let verdict = scan_source_package(source.path()).verdict;

        assert!(!verdict.valid);
        assert!(verdict.accepted.is_none());
        assert_eq!(verdict.blocked.len(), 1);
        assert_eq!(verdict.blocked[0].kind, "source_package");
        assert!(verdict.excluded.is_empty());
        assert!(SourceArtifactTransfer::from_directory("source-1", source.path()).is_err());
        assert_eq!(
            verdict.partial,
            Some(SourcePackagePartial {
                file_count: MAX_SOURCE_PACKAGE_ENTRIES + 1,
                bytes: 0,
                largest_entries: vec![
                    SourcePackageContributor {
                        path: "a/0000".to_string(),
                        bytes: 0,
                    },
                    SourcePackageContributor {
                        path: "a/0001".to_string(),
                        bytes: 0,
                    },
                    SourcePackageContributor {
                        path: "a/0002".to_string(),
                        bytes: 0,
                    },
                    SourcePackageContributor {
                        path: "a/0003".to_string(),
                        bytes: 0,
                    },
                    SourcePackageContributor {
                        path: "a/0004".to_string(),
                        bytes: 0,
                    },
                ],
            })
        );
    }

    #[test]
    fn source_package_entry_limit_reports_measured_values_limits_and_continuation() {
        let source = tempfile::tempdir().expect("source");
        for index in 0..=MAX_SOURCE_PACKAGE_ENTRIES {
            std::fs::write(source.path().join(format!("{index:04}")), b"x").expect("file");
        }

        let error = SourceArtifactTransfer::from_directory("source-1", source.path())
            .expect_err("entry limit");
        assert!(error.message.contains("1025 entries / 1025 bytes"));
        assert_eq!(
            error.details["source_package"]["measured"]["file_count"],
            MAX_SOURCE_PACKAGE_ENTRIES + 1
        );
        assert_eq!(
            error.details["source_package"]["limits"]["entry_limit"],
            MAX_SOURCE_PACKAGE_ENTRIES
        );
        assert!(
            error.details["source_package"]["continuation"]["preflight_command"]
                .as_str()
                .expect("preflight command")
                .contains("homeboy source package check --path")
        );
        let verdict = scan_source_package(source.path()).verdict;
        assert_eq!(
            verdict.limits.expect("v2 limits").entry_limit,
            MAX_SOURCE_PACKAGE_ENTRIES
        );
        assert_eq!(
            verdict.partial.expect("partial measurement").file_count,
            MAX_SOURCE_PACKAGE_ENTRIES + 1
        );
    }

    #[test]
    fn source_package_byte_limit_reports_measured_values_limits_and_contributors() {
        let source = tempfile::tempdir().expect("source");
        for index in 0..4 {
            std::fs::write(
                source.path().join(format!("{index:04}")),
                vec![b'x'; MAX_SOURCE_PACKAGE_FILE_BYTES as usize],
            )
            .expect("file");
        }
        std::fs::write(source.path().join("0004"), b"x").expect("overflow file");

        let error = SourceArtifactTransfer::from_directory("source-1", source.path())
            .expect_err("byte limit");
        assert_eq!(
            error.details["source_package"]["measured"]["bytes"],
            MAX_SOURCE_ARTIFACT_BYTES + 1
        );
        assert_eq!(
            error.details["source_package"]["limits"]["byte_limit"],
            MAX_SOURCE_ARTIFACT_BYTES
        );
        assert_eq!(
            error.details["source_package"]["measured"]["largest_entries"]
                .as_array()
                .expect("contributors")[0]["path"],
            "0000"
        );
    }

    #[test]
    fn source_package_check_v1_round_trips_without_v2_limits() {
        let legacy = json!({
            "schema": SOURCE_PACKAGE_CHECK_SCHEMA,
            "valid": false,
            "partial": {"file_count": 3, "bytes": 12},
            "excluded": [],
            "blocked": []
        });

        let verdict: SourcePackageCheckVerdict =
            serde_json::from_value(legacy).expect("deserialize v1 check");
        assert_eq!(verdict.schema, SOURCE_PACKAGE_CHECK_SCHEMA);
        assert!(verdict.limits.is_none());
        let serialized = serde_json::to_value(&verdict).expect("serialize v1 check");
        assert!(serialized.get("limits").is_none());
        assert_eq!(
            serialized["partial"]["largest_entries"],
            serde_json::Value::Null
        );
    }

    #[test]
    fn source_package_check_v2_round_trips_with_limits() {
        let verdict = scan_source_package(tempfile::tempdir().expect("source").path()).verdict;
        assert_eq!(verdict.schema, SOURCE_PACKAGE_CHECK_SCHEMA_V2);
        assert_eq!(
            verdict.limits.as_ref().expect("v2 limits").byte_limit,
            MAX_SOURCE_ARTIFACT_BYTES
        );
        assert_eq!(
            serde_json::from_value::<SourcePackageCheckVerdict>(
                serde_json::to_value(&verdict).expect("serialize v2 check")
            )
            .expect("deserialize v2 check"),
            verdict
        );
    }
}
