use std::collections::BTreeMap;
use std::fs;
use std::path::{Component as PathComponent, Path};
use std::time::Duration;

use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize};
use sha2::Digest;

use homeboy_core::content_diff::{self, TreeEntry, TreeEntryKind, TreeScanOptions};
use homeboy_core::engine::shell;
use homeboy_core::error::{CommandEvidence, Error, RemoteCapabilityMissingDetails, TargetDetails};
use homeboy_core::server::{CommandOutput, SshClient};

use super::types::PreparedDeployArtifact;
use super::types::{ContentManifestComparison, ContentManifestProvenance};
use homeboy_core::git::release_download::ReleaseArtifactLease;

const ALGORITHM: &str = "sha256-tree-v1";
const WORKSPACE_SCOPE: &str = "source-tree-installed-tree";
const PACKAGE_SCOPE: &str = "canonical-package-installed-tree";
const PACKAGE_UNAVAILABLE_SCOPE: &str = "canonical-package-unavailable";
const MAX_DIFFERENCES: usize = 20;
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);
/// Bound on retained stdout/stderr from the remote manifest probe, matching the
/// release path's treatment of `gh` output.
const REMOTE_PROBE_DIAGNOSTIC_BYTES: usize = 4096;
/// Suffix [`bound_text`] appends when it truncates, used to set
/// [`CommandEvidence::truncated`] honestly.
const DIAGNOSTIC_TRUNCATION_MARKER: &str = "...[truncated]";

/// How deploy scans a tree, stated once so the local walk cannot drift from the
/// remote probe or from the archive reader.
///
/// `excludes` is deliberately empty. Deploy asks "does the installed tree still
/// hold the bytes the package contract says it should", and every path in that
/// contract is in scope by definition. Folding in a source checkout's ignore
/// rules — the way recovery legitimately does, because recovery is comparing a
/// checkout — would make drift inside shipped-but-gitignored directories
/// (vendored dependencies, built assets) invisible, trading a false positive in
/// a drift detector for a false negative. See #10290.
fn deployed_tree_scan_options() -> TreeScanOptions {
    TreeScanOptions {
        excludes: Vec::new(),
        record_symlinks: true,
        record_executable_mode: true,
        prune_runtime_artifacts: true,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Entry {
    kind: char,
    mode: String,
    value: String,
}

#[derive(Deserialize)]
struct SerializedManifest {
    entries: BTreeMap<String, Entry>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct Manifest {
    entries: BTreeMap<String, TreeEntry>,
}

impl PartialEq for Manifest {
    fn eq(&self, other: &Self) -> bool {
        self.entries.len() == other.entries.len()
            && self.entries.iter().all(|(path, entry)| {
                other
                    .entries
                    .get(path)
                    .is_some_and(|other| entry.matches(other))
            })
    }
}

impl Eq for Manifest {}

impl Serialize for Manifest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let entries = self
            .entries
            .iter()
            .map(|(path, entry)| {
                (
                    path,
                    Entry {
                        kind: entry.kind.tag(),
                        mode: entry.mode.clone(),
                        value: entry.value.clone(),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut manifest = serializer.serialize_struct("Manifest", 1)?;
        manifest.serialize_field("entries", &entries)?;
        manifest.end()
    }
}

impl<'de> Deserialize<'de> for Manifest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let entries = SerializedManifest::deserialize(deserializer)?
            .entries
            .into_iter()
            .map(|(path, entry)| {
                let kind = match entry.kind {
                    'f' => TreeEntryKind::File,
                    'l' => TreeEntryKind::Symlink,
                    _ => return Err(serde::de::Error::custom("invalid manifest entry kind")),
                };
                Ok((
                    path,
                    TreeEntry {
                        kind,
                        mode: entry.mode,
                        value: entry.value,
                        bytes: 0,
                    },
                ))
            })
            .collect::<Result<BTreeMap<_, _>, D::Error>>()?;
        Ok(Self { entries })
    }
}

impl Manifest {
    pub(super) fn digest(&self) -> String {
        let mut hash = homeboy_engine_primitives::content_hash::sha256_hasher();
        for (path, entry) in &self.entries {
            hash.update(path.as_bytes());
            hash.update([0]);
            hash.update([entry.kind.tag() as u8]);
            hash.update([0]);
            hash.update(entry.mode.as_bytes());
            hash.update([0]);
            hash.update(entry.value.as_bytes());
            hash.update([b'\n']);
        }
        format!("{:x}", hash.finalize())
    }

    pub(super) fn validate(&self) -> Result<(), Error> {
        for (path, entry) in &self.entries {
            if normalize_path(path)? != *path {
                return Err(manifest_entry_error(
                    "receipt manifest contains an invalid entry",
                    path,
                ));
            }
            if entry.kind == TreeEntryKind::File
                && (!content_diff::is_executable_mode_tag(&entry.mode)
                    || entry.value.len() != 64
                    || !entry.value.bytes().all(|byte| byte.is_ascii_hexdigit()))
            {
                return Err(manifest_entry_error(
                    "receipt manifest contains an invalid file entry",
                    path,
                ));
            }
            if entry.kind == TreeEntryKind::Symlink
                && validate_link_target(path, &entry.value)? != entry.value
            {
                return Err(manifest_entry_error(
                    "receipt manifest contains an invalid symlink entry",
                    path,
                ));
            }
        }
        Ok(())
    }
}

/// A receipt manifest entry that violates the manifest contract, naming the
/// offending path.
///
/// The path is the whole diagnostic: "receipt manifest contains an invalid
/// entry" told an operator nothing about *which* entry, so the id is carried
/// structurally in `details.id` rather than folded into prose (#11135).
fn manifest_entry_error(problem: &str, path: &str) -> Error {
    Error::invalid_argument_for("receipt_manifest", problem, path)
}

/// Render a structured [`Error`] into the single `diagnostic` string a
/// [`ContentManifestComparison`] can carry.
///
/// `ContentManifestComparison::diagnostic` is a `String`, so this is the last
/// point at which structured evidence can survive into what an operator reads:
/// the error code, the serialized `details` (which carry the captured remote
/// probe command evidence), and every hint are folded into the text instead of
/// being discarded by a bare `{error}` (#11135).
pub(super) fn diagnostic_text(error: &Error) -> String {
    let mut text = format!("[{}] {}", error.code.as_str(), error.message);
    if !error.details.is_null() {
        text.push_str(&format!(" details={}", error.details));
    }
    for hint in &error.hints {
        text.push_str(&format!(" hint={}", hint.message));
    }
    text
}

/// Truncate on a character boundary, marking the cut so a consumer can tell a
/// bounded capture from a complete one.
fn bound_text(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_string();
    }
    let mut end = limit;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{DIAGNOSTIC_TRUNCATION_MARKER}", &value[..end])
}

pub(super) fn compare(
    local: &Path,
    remote: &str,
    client: &SshClient,
    _exclusions: &[String],
) -> ContentManifestComparison {
    let local = match local_manifest(local) {
        Ok(manifest) => manifest,
        Err(error) => {
            return unavailable(
                format!("local manifest unavailable: {}", diagnostic_text(&error)),
                WORKSPACE_SCOPE,
                None,
            )
        }
    };
    let remote = match remote_manifest(remote, client, &[]) {
        Ok(Some(manifest)) => manifest,
        Ok(None) => {
            return comparison(
                &local,
                None,
                "missing",
                Vec::new(),
                None,
                WORKSPACE_SCOPE,
                None,
            )
        }
        Err(error) => {
            return unavailable(
                format!("remote manifest unavailable: {}", diagnostic_text(&error)),
                WORKSPACE_SCOPE,
                None,
            )
        }
    };
    let differences = differences(&local, &remote);
    let status = if differences.is_empty() {
        "match"
    } else {
        "different"
    };
    comparison(
        &local,
        Some(&remote),
        status,
        differences,
        None,
        WORKSPACE_SCOPE,
        None,
    )
}

/// Compare a deployed tree with the canonical archive selected for deployment.
/// Archive entries are the package contract; source-only checkout files are not.
pub(super) fn compare_archive(
    archive: &Path,
    remote: &str,
    client: &SshClient,
    exclusions: &[String],
    artifact: Option<&ReleaseArtifactLease>,
) -> ContentManifestComparison {
    let local = match archive_manifest(archive, exclusions) {
        Ok(manifest) => manifest,
        Err(error) => {
            return unavailable(
                format!(
                    "canonical package manifest unavailable: {}",
                    diagnostic_text(&error)
                ),
                PACKAGE_SCOPE,
                artifact_provenance(artifact),
            )
        }
    };
    let remote = match remote_manifest(remote, client, exclusions) {
        Ok(Some(manifest)) => manifest,
        Ok(None) => {
            return comparison(
                &local,
                None,
                "missing",
                Vec::new(),
                None,
                PACKAGE_SCOPE,
                artifact_provenance(artifact),
            )
        }
        Err(error) => {
            return unavailable(
                format!("remote manifest unavailable: {}", diagnostic_text(&error)),
                PACKAGE_SCOPE,
                artifact_provenance(artifact),
            )
        }
    };
    let differences = differences(&local, &remote);
    let status = if differences.is_empty() {
        "match"
    } else {
        "different"
    };
    comparison(
        &local,
        Some(&remote),
        status,
        differences,
        None,
        PACKAGE_SCOPE,
        artifact_provenance(artifact),
    )
}

pub(super) fn package_manifest(archive: &Path, exclusions: &[String]) -> Result<Manifest, Error> {
    archive_manifest(archive, exclusions)
}

pub(super) fn compare_saved_package_manifest(
    local: &Manifest,
    remote: &str,
    client: &SshClient,
    exclusions: &[String],
    provenance: ContentManifestProvenance,
) -> ContentManifestComparison {
    let remote = match remote_manifest(remote, client, exclusions) {
        Ok(Some(manifest)) => manifest,
        Ok(None) => {
            return comparison(
                local,
                None,
                "missing",
                Vec::new(),
                None,
                PACKAGE_SCOPE,
                Some(provenance),
            )
        }
        Err(error) => {
            return unavailable(
                format!("remote manifest unavailable: {}", diagnostic_text(&error)),
                PACKAGE_SCOPE,
                Some(provenance),
            )
        }
    };
    let differences = differences(local, &remote);
    comparison(
        local,
        Some(&remote),
        if differences.is_empty() {
            "match"
        } else {
            "different"
        },
        differences,
        None,
        PACKAGE_SCOPE,
        Some(provenance),
    )
}

pub(super) fn compare_prepared_archive(
    archive: &Path,
    remote: &str,
    client: &SshClient,
    exclusions: &[String],
    artifact: &PreparedDeployArtifact,
) -> ContentManifestComparison {
    match sha256(archive) {
        Ok(actual) if actual == artifact.sha256 => compare_archive_with_provenance(
            archive,
            remote,
            client,
            exclusions,
            prepared_artifact_provenance(artifact),
        ),
        Ok(actual) => canonical_package_unavailable_with_provenance(
            format!(
                "prepared package SHA-256 mismatch for '{}': expected {}, got {}",
                archive.display(),
                artifact.sha256,
                actual
            ),
            prepared_artifact_provenance_without_sha(artifact),
        ),
        Err(error) => canonical_package_unavailable_with_provenance(
            format!("prepared package unavailable: {}", diagnostic_text(&error)),
            prepared_artifact_provenance_without_sha(artifact),
        ),
    }
}

fn comparison(
    local: &Manifest,
    remote: Option<&Manifest>,
    status: &str,
    differences: Vec<String>,
    diagnostic: Option<String>,
    scope: &str,
    provenance: Option<ContentManifestProvenance>,
) -> ContentManifestComparison {
    ContentManifestComparison {
        algorithm: ALGORITHM.to_string(),
        scope: scope.to_string(),
        provenance,
        local_digest: Some(local.digest()),
        remote_digest: remote.map(Manifest::digest),
        status: status.to_string(),
        differences,
        diagnostic,
    }
}

pub(super) fn verify_archive_hash(
    archive: &Path,
    artifact: &ReleaseArtifactLease,
) -> Result<(), Error> {
    verify_archive_digest(archive, &sha256(archive)?, artifact)
}

/// [`verify_archive_hash`] against an already-computed digest of `archive`.
pub(super) fn verify_archive_digest(
    archive: &Path,
    actual: &str,
    artifact: &ReleaseArtifactLease,
) -> Result<(), Error> {
    if actual != artifact.sha256 {
        return Err(Error::validation_invalid_argument(
            "release_artifact",
            format!(
                "canonical package SHA-256 mismatch for '{}': expected {}, got {}",
                archive.display(),
                artifact.sha256,
                actual
            ),
            Some(archive.display().to_string()),
            None,
        )
        .with_hint(
            "The leased release asset no longer hashes to the digest recorded when it was \
             resolved; re-resolve the asset before deploying, or use --tagged to request an \
             explicit local tag build.",
        ));
    }
    Ok(())
}

pub(super) fn canonical_package_unavailable(
    diagnostic: String,
    artifact_name: Option<&str>,
) -> ContentManifestComparison {
    canonical_package_unavailable_with_provenance(
        diagnostic,
        ContentManifestProvenance {
            source: "release-package".to_string(),
            artifact_name: artifact_name.map(str::to_string),
            artifact_sha256: None,
            artifact_tag: None,
            artifact_commit: None,
        },
    )
}

pub(super) fn canonical_package_unavailable_for_artifact(
    diagnostic: String,
    artifact: &ReleaseArtifactLease,
) -> ContentManifestComparison {
    canonical_package_unavailable_with_provenance(
        diagnostic,
        ContentManifestProvenance {
            source: "release-package".to_string(),
            artifact_name: Some(artifact.name.clone()),
            // The leased digest no longer describes the bytes under inspection.
            artifact_sha256: None,
            artifact_tag: Some(artifact.tag.clone()),
            artifact_commit: artifact.commit.clone(),
        },
    )
}

fn canonical_package_unavailable_with_provenance(
    diagnostic: String,
    provenance: ContentManifestProvenance,
) -> ContentManifestComparison {
    ContentManifestComparison {
        algorithm: ALGORITHM.to_string(),
        scope: PACKAGE_UNAVAILABLE_SCOPE.to_string(),
        provenance: Some(provenance),
        local_digest: None,
        remote_digest: None,
        status: "unavailable".to_string(),
        differences: Vec::new(),
        diagnostic: Some(diagnostic),
    }
}

pub(super) fn local_build_package_unavailable(
    diagnostic: String,
    artifact_name: Option<&str>,
) -> ContentManifestComparison {
    canonical_package_unavailable_with_provenance(
        diagnostic,
        ContentManifestProvenance {
            source: "local-build".to_string(),
            artifact_name: artifact_name.map(str::to_string),
            artifact_sha256: None,
            artifact_tag: None,
            artifact_commit: None,
        },
    )
}

fn compare_archive_with_provenance(
    archive: &Path,
    remote: &str,
    client: &SshClient,
    exclusions: &[String],
    provenance: ContentManifestProvenance,
) -> ContentManifestComparison {
    let local = match archive_manifest(archive, exclusions) {
        Ok(manifest) => manifest,
        Err(error) => {
            return canonical_package_unavailable_with_provenance(
                format!(
                    "canonical package manifest unavailable: {}",
                    diagnostic_text(&error)
                ),
                provenance,
            )
        }
    };
    let remote = match remote_manifest(remote, client, exclusions) {
        Ok(Some(manifest)) => manifest,
        Ok(None) => {
            return comparison(
                &local,
                None,
                "missing",
                Vec::new(),
                None,
                PACKAGE_SCOPE,
                Some(provenance),
            )
        }
        Err(error) => {
            return canonical_package_unavailable_with_provenance(
                format!("remote manifest unavailable: {}", diagnostic_text(&error)),
                provenance,
            )
        }
    };
    let differences = differences(&local, &remote);
    comparison(
        &local,
        Some(&remote),
        if differences.is_empty() {
            "match"
        } else {
            "different"
        },
        differences,
        None,
        PACKAGE_SCOPE,
        Some(provenance),
    )
}

fn prepared_artifact_provenance(artifact: &PreparedDeployArtifact) -> ContentManifestProvenance {
    ContentManifestProvenance {
        source: "prepared-artifact".to_string(),
        artifact_name: Some(artifact.effective_path().to_string()),
        artifact_sha256: Some(artifact.sha256.clone()),
        artifact_tag: Some(artifact.tag.clone()),
        artifact_commit: Some(artifact.source_commit.clone()),
    }
}

fn prepared_artifact_provenance_without_sha(
    artifact: &PreparedDeployArtifact,
) -> ContentManifestProvenance {
    ContentManifestProvenance {
        artifact_sha256: None,
        ..prepared_artifact_provenance(artifact)
    }
}

fn unavailable(
    diagnostic: String,
    scope: &str,
    provenance: Option<ContentManifestProvenance>,
) -> ContentManifestComparison {
    ContentManifestComparison {
        algorithm: ALGORITHM.to_string(),
        scope: scope.to_string(),
        provenance,
        local_digest: None,
        remote_digest: None,
        status: "unavailable".to_string(),
        differences: Vec::new(),
        diagnostic: Some(diagnostic),
    }
}

fn local_manifest(root: &Path) -> Result<Manifest, Error> {
    local_manifest_with_exclusions(root, &[])
}

fn local_manifest_with_exclusions(root: &Path, exclusions: &[String]) -> Result<Manifest, Error> {
    if !root.exists() {
        return Err(Error::invalid_argument_for(
            "local_path",
            format!("{} does not exist", root.display()),
            root.display().to_string(),
        ));
    }
    if root.is_file() {
        return single_file_manifest(root, root, exclusions);
    }
    // `scan_tree` already reports a coded, detailed error. Stringifying it here
    // is what erased the walker's own diagnosis on the deploy path (#11135).
    let entries = content_diff::scan_tree(
        root,
        &TreeScanOptions {
            excludes: exclusions.to_vec(),
            ..deployed_tree_scan_options()
        },
    )?;
    let entries = entries
        .into_iter()
        .map(|(path, mut entry)| {
            if entry.kind == TreeEntryKind::Symlink {
                entry.value = validate_link_target(&path, &entry.value)?;
            }
            Ok((path, entry))
        })
        .collect::<Result<BTreeMap<_, _>, Error>>()?;
    Ok(Manifest { entries })
}

fn single_file_manifest(
    path: &Path,
    identity_root: &Path,
    exclusions: &[String],
) -> Result<Manifest, Error> {
    let relative = normalize_single_file_path(identity_root)?;
    if ignored(&relative, exclusions) {
        return Ok(Manifest::default());
    }
    let metadata = fs::metadata(path).map_err(|error| {
        Error::from_io_error(&error, Some(format!("stat '{}'", path.display())))
    })?;
    Ok(Manifest {
        entries: BTreeMap::from([(
            relative,
            TreeEntry {
                kind: TreeEntryKind::File,
                mode: content_diff::executable_mode_tag(platform_mode(&metadata)),
                value: sha256(path)?,
                bytes: metadata.len(),
            },
        )]),
    })
}

#[cfg(unix)]
fn platform_mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;

    metadata.permissions().mode()
}

#[cfg(not(unix))]
fn platform_mode(_metadata: &fs::Metadata) -> u32 {
    0
}

fn normalize_single_file_path(path: &Path) -> Result<String, Error> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            Error::invalid_argument_for(
                "manifest_path",
                format!("{} has no filename", path.display()),
                path.display().to_string(),
            )
        })?;
    normalize_path(file_name)
}

/// A failure reading or interpreting the canonical package archive, naming the
/// archive so the operator does not have to infer which package was inspected.
fn archive_error(path: &Path, problem: impl std::fmt::Display) -> Error {
    Error::invalid_argument_for(
        "package_archive",
        problem.to_string(),
        path.display().to_string(),
    )
}

fn archive_manifest(path: &Path, exclusions: &[String]) -> Result<Manifest, Error> {
    let file = fs::File::open(path).map_err(|error| {
        Error::from_io_error(
            &error,
            Some(format!("open package archive '{}'", path.display())),
        )
    })?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|error| archive_error(path, format!("package archive is unreadable: {error}")))?;
    let mut names = Vec::new();
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|error| {
            archive_error(
                path,
                format!("package archive entry {index} failed: {error}"),
            )
        })?;
        if !entry.is_dir() {
            names.push(normalize_path(entry.name())?);
        }
    }
    let root = archive_root(&names);
    let mut manifest = Manifest::default();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| {
            archive_error(
                path,
                format!("package archive entry {index} failed: {error}"),
            )
        })?;
        if entry.is_dir() {
            continue;
        }
        let name = normalize_path(entry.name())?;
        let relative = root
            .as_deref()
            .and_then(|root| {
                name.strip_prefix(root)
                    .and_then(|path| path.strip_prefix('/'))
            })
            .unwrap_or(&name)
            .to_string();
        if relative.is_empty() {
            return Err(archive_error(
                path,
                format!("archive entry '{name}' resolves to the package root"),
            ));
        }
        // Deploy scopes describe the installed payload, not an archive wrapper.
        if ignored(&relative, exclusions) {
            continue;
        }
        let kind = if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            TreeEntryKind::Symlink
        } else {
            TreeEntryKind::File
        };
        let mut hash = homeboy_engine_primitives::content_hash::sha256_hasher();
        let value = if kind == TreeEntryKind::Symlink {
            let mut target = String::new();
            std::io::Read::read_to_string(&mut entry, &mut target).map_err(|error| {
                Error::from_io_error(
                    &error,
                    Some(format!(
                        "read symlink target '{relative}' from package archive '{}'",
                        path.display()
                    )),
                )
            })?;
            validate_link_target(&relative, &target)?
        } else {
            std::io::copy(&mut entry, &mut hash).map_err(|error| {
                Error::from_io_error(
                    &error,
                    Some(format!(
                        "hash entry '{relative}' from package archive '{}'",
                        path.display()
                    )),
                )
            })?;
            format!("{:x}", hash.finalize())
        };
        let duplicate = relative.clone();
        if manifest
            .entries
            .insert(
                relative,
                TreeEntry {
                    kind,
                    mode: if kind == TreeEntryKind::Symlink {
                        "0".to_string()
                    } else {
                        content_diff::executable_mode_tag(entry.unix_mode().unwrap_or(0))
                    },
                    value,
                    bytes: 0,
                },
            )
            .is_some()
        {
            return Err(archive_error(
                path,
                format!("archive contains duplicate normalized path '{duplicate}'"),
            ));
        }
    }
    Ok(manifest)
}

fn archive_root(names: &[String]) -> Option<String> {
    let first = names.first()?.split('/').next()?;
    (!first.is_empty()
        && names.iter().all(|name| {
            name.strip_prefix(first)
                .is_some_and(|suffix| suffix.starts_with('/'))
        }))
    .then(|| first.to_string())
}

use homeboy_engine_primitives::content_hash::sha256_file as sha256;

/// Structured evidence for a failed remote content-manifest probe.
///
/// The probe is a command, so its exit code and bounded stdout/stderr belong in
/// [`CommandEvidence`] exactly as the release path carries `gh` output. Before
/// #11135 this channel returned a bare `String` and the target's stderr — the
/// only thing that says *why* the probe failed — was discarded outright.
fn remote_probe_error(
    root: &str,
    problem: impl Into<String>,
    location: &str,
    output: &CommandOutput,
) -> Error {
    let stdout = bound_text(&output.stdout, REMOTE_PROBE_DIAGNOSTIC_BYTES);
    let stderr = bound_text(&output.stderr, REMOTE_PROBE_DIAGNOSTIC_BYTES);
    let truncated = stdout.ends_with(DIAGNOSTIC_TRUNCATION_MARKER)
        || stderr.ends_with(DIAGNOSTIC_TRUNCATION_MARKER);
    Error::validation_invalid_argument_with_evidence(
        "remote_manifest",
        problem,
        Some(root.to_string()),
        None,
        Some(CommandEvidence {
            command: format!("deploy content-manifest probe of {root}"),
            cwd: None,
            location: Some(location.to_string()),
            // A timeout kills the probe before it reports a status, so there is
            // no real exit code to report.
            exit_code: if output.timed_out {
                -1
            } else {
                output.exit_code
            },
            stdout,
            stderr,
            truncated,
        }),
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RemoteProbeCapability {
    Sha256,
    Find,
    Stat,
    Readlink,
}

impl RemoteProbeCapability {
    fn exit_code(self) -> i32 {
        match self {
            Self::Sha256 => 45,
            Self::Find => 46,
            Self::Stat => 47,
            Self::Readlink => 48,
        }
    }

    fn primitive(self) -> &'static str {
        match self {
            Self::Sha256 => "sha256sum",
            Self::Find => "find",
            Self::Stat => "stat",
            Self::Readlink => "readlink",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Self::Sha256 => "Install `sha256sum` (coreutils) or `shasum` on the deploy target.",
            Self::Find => "Install a POSIX-compatible `find` on the deploy target.",
            Self::Stat => {
                "Install a `stat` implementation that supports either `-c %a` or `-f %Lp`."
            }
            Self::Readlink => {
                "Install `readlink` on the deploy target so symlink payloads can be inspected."
            }
        }
    }

    fn from_exit_code(exit_code: i32) -> Option<Self> {
        [Self::Sha256, Self::Find, Self::Stat, Self::Readlink]
            .into_iter()
            .find(|capability| capability.exit_code() == exit_code)
    }
}

fn remote_probe_capability_error(
    root: &str,
    capability: RemoteProbeCapability,
    location: &str,
    output: &CommandOutput,
) -> Error {
    Error::remote_capability_missing(
        format!(
            "remote content manifest probe requires unsupported command '{}'",
            capability.primitive()
        ),
        RemoteCapabilityMissingDetails {
            capability: capability.primitive().to_string(),
            target: Some(TargetDetails {
                project_id: None,
                server_id: None,
                host: None,
            }),
            command_evidence: Some(CommandEvidence {
                command: format!("deploy content-manifest probe of {root}"),
                cwd: None,
                location: Some(location.to_string()),
                exit_code: output.exit_code,
                stdout: bound_text(&output.stdout, REMOTE_PROBE_DIAGNOSTIC_BYTES),
                stderr: bound_text(&output.stderr, REMOTE_PROBE_DIAGNOSTIC_BYTES),
                truncated: output.stdout.len() > REMOTE_PROBE_DIAGNOSTIC_BYTES
                    || output.stderr.len() > REMOTE_PROBE_DIAGNOSTIC_BYTES,
            }),
        },
    )
    .with_hint(capability.hint())
}

/// Builds the shell probe that describes a deployed tree as compact records.
///
/// Regular files are hashed and stat'ed in batches: `find -exec ... {} +` hands
/// each batch of paths to one inner shell, which runs a single `stat` and a
/// single `sha256sum`/`shasum` over the whole batch and then pairs the outputs
/// with the batch's positional arguments in lockstep using shell builtins.
/// File names are never parsed back out of tool output, so names containing
/// spaces or backslashes survive intact, and names containing tabs or
/// newlines still produce records that the parser rejects, exactly like the
/// former one-file-at-a-time loop. Symlinks stay on a per-link `readlink`
/// loop; they are rare and `readlink` has no portable multi-operand form.
fn remote_manifest_probe_command(root: &str) -> String {
    let root = shell::quote_path(root);
    let sha_exit = RemoteProbeCapability::Sha256.exit_code();
    let find_exit = RemoteProbeCapability::Find.exit_code();
    let stat_exit = RemoteProbeCapability::Stat.exit_code();
    let readlink_exit = RemoteProbeCapability::Readlink.exit_code();
    format!(
        concat!(
            "root={root}; test -e \"$root\" || exit 44; ",
            // Pick the hash and stat flavours once so every batch uses the same tools.
            "if command -v sha256sum >/dev/null 2>&1; then hb_sha=sha256sum; ",
            "elif command -v shasum >/dev/null 2>&1; then hb_sha=shasum; ",
            "else printf '%s\\n' sha256sum >&2; exit {sha_exit}; fi; ",
            "if stat -c %a \"$root\" >/dev/null 2>&1; then hb_stat=gnu; ",
            "elif stat -f %Lp \"$root\" >/dev/null 2>&1; then hb_stat=bsd; ",
            "else printf '%s\\n' stat >&2; exit {stat_exit}; fi; ",
            "if [ -d \"$root\" ]; then ",
            // A symlinked directory root (the versioned release layout,
            // #18365) is walked through its `/.` entry: plain `find` never
            // descends into a starting-point symlink. Entries keep their
            // paths relative to the logical root.
            "if [ -L \"$root\" ]; then root=\"$root/.\"; fi; ",
            "command -v find >/dev/null 2>&1 || {{ printf '%s\\n' find >&2; exit {find_exit}; }}; ",
            "if [ -n \"$(find \"$root\" -type l -print -quit)\" ]; then command -v readlink >/dev/null 2>&1 || {{ printf '%s\\n' readlink >&2; exit {readlink_exit}; }}; fi; ",
            "find \"$root\" -mindepth 1 \\( -path '*/.git/*' -o -name .git -o -name '.homeboy-*' \\) -prune -o ",
            // Regular files: one stat + one hash process per batch.
            "\\( -type f -exec sh -c '",
            "root=$1; hb_stat=$2; hb_sha=$3; shift 3; ",
            "if [ \"$hb_stat\" = gnu ]; then modes=$(stat -c %a \"$@\") || exit {stat_exit}; ",
            "else modes=$(stat -f %Lp \"$@\") || exit {stat_exit}; fi; ",
            "if [ \"$hb_sha\" = sha256sum ]; then hashes=$(sha256sum \"$@\") || exit $?; ",
            "else hashes=$(shasum -a 256 \"$@\") || exit $?; fi; ",
            "printf \"%s\\n\" \"$modes\" | {{ exec 3<&0; printf \"%s\\n\" \"$hashes\" | {{ ",
            "for f do IFS= read -r mode <&3 || exit 1; IFS= read -r hash || exit 1; ",
            // Hash tools prefix a backslash when they escape an unusual file name.
            "hash=${{hash#\\\\}}; hash=${{hash%% *}}; ",
            "printf \"f\\t%s\\t%s\\t%s\\n\" \"${{f#\"$root\"/}}\" \"$mode\" \"$hash\"; ",
            "done; }}; }}' sh \"$root\" \"$hb_stat\" \"$hb_sha\" {{}} + \\) -o ",
            // Symlinks: per-link readlink, matching the previous behaviour.
            "\\( -type l -exec sh -c '",
            "root=$1; hb_stat=$2; shift 2; ",
            "for f do rel=${{f#\"$root\"/}}; ",
            "if [ \"$hb_stat\" = gnu ]; then mode=$(stat -c %a \"$f\") || exit {stat_exit}; ",
            "else mode=$(stat -f %Lp \"$f\") || exit {stat_exit}; fi; ",
            "printf \"l\\t%s\\t%s\\t%s\\n\" \"$rel\" \"$mode\" \"$(readlink \"$f\")\"; ",
            "done' sh \"$root\" \"$hb_stat\" {{}} + \\); ",
            "else rel=${{root##*/}}; ",
            "if [ \"$hb_stat\" = gnu ]; then mode=$(stat -c %a \"$root\") || exit {stat_exit}; ",
            "else mode=$(stat -f %Lp \"$root\") || exit {stat_exit}; fi; ",
            "if [ -L \"$root\" ]; then ",
            "command -v readlink >/dev/null 2>&1 || {{ printf '%s\\n' readlink >&2; exit {readlink_exit}; }}; ",
            "printf 'l\\t%s\\t%s\\t%s\\n' \"$rel\" \"$mode\" \"$(readlink \"$root\")\"; ",
            "else ",
            "if [ \"$hb_sha\" = sha256sum ]; then hash=$(sha256sum \"$root\") || exit $?; ",
            "else hash=$(shasum -a 256 \"$root\") || exit $?; fi; ",
            "hash=${{hash#\\\\}}; hash=${{hash%% *}}; ",
            "printf 'f\\t%s\\t%s\\t%s\\n' \"$rel\" \"$mode\" \"$hash\"; ",
            "fi; fi"
        ),
        root = root,
        sha_exit = sha_exit,
        find_exit = find_exit,
        stat_exit = stat_exit,
        readlink_exit = readlink_exit,
    )
}

fn remote_manifest(
    root: &str,
    client: &SshClient,
    exclusions: &[String],
) -> Result<Option<Manifest>, Error> {
    if client.is_local {
        return local_manifest_with_exclusions(Path::new(root), exclusions).map(Some);
    }
    // The target computes hashes and returns compact records; content never crosses SSH.
    let command = remote_manifest_probe_command(root);
    // The local case returned above, so anything reaching here ran on the target.
    let output = client.execute_with_timeout(&command, PROBE_TIMEOUT);
    let location = "remote";
    if output.timed_out {
        return Err(remote_probe_error(
            root,
            format!("timed out after {}s", PROBE_TIMEOUT.as_secs()),
            location,
            &output,
        )
        .with_hint(format!(
            "The probe hashes every file under {root}; raise the deploy target's throughput or \
             narrow the component scope before retrying.",
        ))
        .with_retryable(true));
    }
    if output.exit_code == 44 {
        return Ok(None);
    }
    if let Some(capability) = RemoteProbeCapability::from_exit_code(output.exit_code) {
        return Err(remote_probe_capability_error(
            root, capability, location, &output,
        ));
    }
    if !output.success {
        return Err(remote_probe_error(
            root,
            format!(
                "remote content manifest probe failed with exit code {}",
                output.exit_code
            ),
            location,
            &output,
        )
        .with_hint(
            "Inspect the captured `command_evidence.stderr` for the target's own explanation \
             before retrying.",
        ));
    }
    parse_remote_manifest(&output.stdout, exclusions).map(Some)
}

fn parse_remote_manifest(output: &str, exclusions: &[String]) -> Result<Manifest, Error> {
    let mut manifest = Manifest::default();
    for line in output.lines() {
        let mut fields = line.splitn(4, '\t');
        let kind = fields
            .next()
            .ok_or_else(|| malformed_remote_field("kind", line))?;
        let path = fields
            .next()
            .ok_or_else(|| malformed_remote_field("path", line))?;
        let mode = fields
            .next()
            .ok_or_else(|| malformed_remote_field("mode", line))?;
        let value = fields
            .next()
            .ok_or_else(|| malformed_remote_field("value", line))?;
        let path = normalize_path(path).map_err(|_| malformed_remote_field("path", path))?;
        if ignored(&path, exclusions) {
            continue;
        }
        let kind = match kind {
            "f" => TreeEntryKind::File,
            "l" => TreeEntryKind::Symlink,
            _ => return Err(malformed_remote_field("kind", kind)),
        };
        let mode =
            u32::from_str_radix(mode, 8).map_err(|_| malformed_remote_field("mode", mode))?;
        let mode = if kind == TreeEntryKind::Symlink {
            "0".to_string()
        } else {
            content_diff::executable_mode_tag(mode)
        };
        if kind == TreeEntryKind::File
            && (value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            return Err(malformed_remote_field("sha256", value));
        }
        if kind == TreeEntryKind::Symlink {
            let value = validate_link_target(&path, value)
                .map_err(|_| malformed_remote_field("symlink_target", value))?;
            manifest.entries.insert(
                path,
                TreeEntry {
                    kind,
                    mode,
                    value,
                    bytes: 0,
                },
            );
            continue;
        }
        manifest.entries.insert(
            path,
            TreeEntry {
                kind,
                mode,
                value: value.to_string(),
                // A remote probe reports content identity, never a stat size.
                bytes: 0,
            },
        );
    }
    Ok(manifest)
}

fn malformed_remote_field(field: &str, value: &str) -> Error {
    const MAX_DIAGNOSTIC_VALUE_BYTES: usize = 160;

    let mut diagnostic = String::new();
    for character in value.chars() {
        let escaped = character.escape_default().to_string();
        if diagnostic.len() + escaped.len() > MAX_DIAGNOSTIC_VALUE_BYTES {
            diagnostic.push_str("...");
            break;
        }
        diagnostic.push_str(&escaped);
    }
    Error::validation_invalid_argument(
        "remote_manifest",
        format!("malformed remote manifest field '{field}': '{diagnostic}'"),
        Some(field.to_string()),
        None,
    )
    .with_hint(
        "The deploy target's probe emitted a record this build cannot parse; confirm the target's \
         `find`/`stat`/`sha256sum` are the coreutils implementations the probe assumes.",
    )
}

/// Paths excluded from every deploy manifest, whatever produced it.
///
/// This is the whole exclusion policy: version-control metadata and this
/// product's own transport scratch files. It is intentionally narrower than
/// recovery's exclusion set — see [`deployed_tree_scan_options`].
fn ignored(path: &str, exclusions: &[String]) -> bool {
    content_diff::excluded(path, exclusions) || content_diff::runtime_artifact(path)
}

fn unsafe_manifest_path(path: &str) -> Error {
    Error::invalid_argument_for(
        "manifest_path",
        format!("unsafe manifest path '{path}'"),
        path,
    )
}

fn normalize_path(path: &str) -> Result<String, Error> {
    let path = path.replace('\\', "/");
    if path.starts_with('/')
        || path
            .bytes()
            .any(|byte| matches!(byte, b'\0' | b'\n' | b'\r' | b'\t'))
    {
        return Err(unsafe_manifest_path(&path));
    }
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => continue,
            ".." => return Err(unsafe_manifest_path(&path)),
            _ => parts.push(part),
        }
    }
    if parts.is_empty()
        || Path::new(&path)
            .components()
            .any(|part| matches!(part, PathComponent::Prefix(_)))
    {
        return Err(unsafe_manifest_path(&path));
    }
    Ok(parts.join("/"))
}

/// A symlink the manifest refuses to record, naming both the entry that holds
/// it and the target it points at. Neither survived the `String` channel.
fn unsafe_symlink_target(problem: &str, entry: &str, target: &str) -> Error {
    Error::invalid_argument_for(
        "manifest_symlink",
        format!("{problem} (entry '{entry}' -> '{target}')"),
        entry,
    )
}

fn validate_link_target(entry: &str, target: &str) -> Result<String, Error> {
    if target.is_empty()
        || target.starts_with('/')
        || target
            .bytes()
            .any(|byte| matches!(byte, b'\0' | b'\n' | b'\r' | b'\t'))
    {
        return Err(unsafe_symlink_target(
            "unsafe manifest symlink target",
            entry,
            target,
        ));
    }
    let mut resolved: Vec<&str> = entry.split('/').collect();
    resolved.pop();
    for part in target.split('/') {
        match part {
            "" | "." => continue,
            ".." => {
                if resolved.pop().is_none() {
                    return Err(unsafe_symlink_target(
                        "symlink target escapes package root",
                        entry,
                        target,
                    ));
                }
            }
            _ => {
                resolved.push(part);
            }
        }
    }
    if resolved.is_empty() {
        return Err(unsafe_symlink_target(
            "unsafe manifest symlink target",
            entry,
            target,
        ));
    }
    Ok(resolved.join("/"))
}

fn artifact_provenance(
    artifact: Option<&ReleaseArtifactLease>,
) -> Option<ContentManifestProvenance> {
    artifact.map(|artifact| ContentManifestProvenance {
        source: "release-package".to_string(),
        artifact_name: Some(artifact.name.clone()),
        artifact_sha256: Some(artifact.sha256.clone()),
        artifact_tag: Some(artifact.tag.clone()),
        artifact_commit: artifact.commit.clone(),
    })
}

fn differences(local: &Manifest, remote: &Manifest) -> Vec<String> {
    let mut paths: BTreeMap<&str, ()> = BTreeMap::new();
    paths.extend(local.entries.keys().map(|p| (p.as_str(), ())));
    paths.extend(remote.entries.keys().map(|p| (p.as_str(), ())));
    paths
        .into_keys()
        .filter(|path| !entries_match(local.entries.get(*path), remote.entries.get(*path)))
        .take(MAX_DIFFERENCES)
        .map(str::to_string)
        .collect()
}

fn entries_match(local: Option<&TreeEntry>, remote: Option<&TreeEntry>) -> bool {
    match (local, remote) {
        (Some(local), Some(remote)) => local.matches(remote),
        (None, None) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_core::server::CommandObservation;
    use std::process::Command;

    #[test]
    fn receipt_manifest_validates_canonical_executable_mode_tags() {
        let mut manifest = Manifest {
            entries: BTreeMap::from([(
                "script.sh".to_string(),
                TreeEntry {
                    kind: TreeEntryKind::File,
                    mode: String::new(),
                    value: "a".repeat(64),
                    bytes: 0,
                },
            )]),
        };
        for mode in ["0", "1", "10", "11", "100", "101", "110", "111"] {
            manifest.entries.get_mut("script.sh").unwrap().mode = mode.to_string();
            assert!(manifest.validate().is_ok(), "rejected {mode:?}");
        }
        for mode in ["2", "644", "0111", "abc", ""] {
            manifest.entries.get_mut("script.sh").unwrap().mode = mode.to_string();
            assert!(manifest.validate().is_err(), "accepted {mode:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn executable_tree_manifest_survives_receipt_round_trip() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("temp");
        for mode in [0o755, 0o775] {
            let script = temp.path().join("deploy.sh");
            fs::write(&script, "#!/bin/sh\nexit 0\n").expect("script");
            fs::set_permissions(&script, fs::Permissions::from_mode(mode)).expect("mode");
            let manifest = local_manifest(temp.path()).expect("local manifest");
            assert_eq!(manifest.entries["deploy.sh"].mode, "111");
            let digest = manifest.digest();

            let serialized = serde_json::to_string(&manifest).expect("serialize receipt manifest");
            let restored: Manifest =
                serde_json::from_str(&serialized).expect("deserialize receipt manifest");
            restored.validate().expect("validate receipt manifest");
            assert_eq!(restored, manifest);
            assert_eq!(restored.digest(), digest);
        }
    }

    #[test]
    fn local_manifest_detects_content_add_delete_mode_symlink_and_ignores_runtime() {
        let temp = tempfile::tempdir().expect("temp");
        let local = temp.path().join("local");
        let remote = temp.path().join("plugin");
        fs::create_dir_all(&local).expect("local");
        fs::create_dir_all(&remote).expect("remote");
        fs::write(local.join("same"), "same").expect("same");
        fs::write(remote.join("same"), "same").expect("same");
        fs::write(local.join("changed"), "local").expect("local");
        fs::write(remote.join("changed"), "remote").expect("remote");
        fs::write(local.join("added"), "add").expect("add");
        fs::write(remote.join("deleted"), "delete").expect("delete");
        fs::write(local.join(".homeboy-upload.tmp"), "ignored").expect("runtime");
        fs::write(remote.join(".homeboy-upload.tmp"), "other").expect("runtime");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("same", local.join("link")).expect("link");
            std::os::unix::fs::symlink("changed", remote.join("link")).expect("link");
        }
        let result = compare(&local, remote.to_str().expect("path"), &local_client(), &[]);
        assert_eq!(result.status, "different");
        assert!(result.differences.iter().any(|p| p == "changed"));
        assert!(result.differences.iter().any(|p| p == "added"));
        assert!(result.differences.iter().any(|p| p == "deleted"));
        assert!(!result.differences.iter().any(|p| p.contains("homeboy")));
    }
    /// Read the structured `problem` out of an invalid-argument error.
    ///
    /// Asserting on this rather than on `Display` is the point of #11135: the
    /// diagnostic now lives in a machine-readable field, and a test that only
    /// matched rendered prose would pass just as well against the old `String`
    /// channel.
    fn problem(error: &Error) -> String {
        error
            .details
            .get("problem")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_else(|| panic!("structured problem detail, got {}", error.details))
            .to_string()
    }

    #[test]
    fn remote_manifest_requires_well_formed_hash_evidence() {
        let missing = parse_remote_manifest("f\tpath\t644\n", &[]).expect_err("missing hash");
        assert_eq!(
            missing.code,
            homeboy_core::error::ErrorCode::ValidationInvalidArgument
        );
        assert_eq!(
            problem(&missing),
            "malformed remote manifest field 'value': 'f\\tpath\\t644'"
        );
        // The remediation is carried as a hint, not buried in the message.
        assert!(!missing.hints.is_empty());
        let malformed =
            parse_remote_manifest("f\tpath\t644\tnot-a-hash\n", &[]).expect_err("malformed hash");
        assert_eq!(
            problem(&malformed),
            "malformed remote manifest field 'sha256': 'not-a-hash'"
        );
        let oversized = problem(
            &parse_remote_manifest(&format!("f\tpath\t644\t{}\n", "x".repeat(200)), &[])
                .expect_err("oversized malformed hash"),
        );
        assert!(oversized.ends_with("...'"), "{oversized}");
        assert!(oversized.len() < 210, "{oversized}");
        assert!(parse_remote_manifest(
            "f\t../path\t644\taaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
            &[]
        )
        .is_err());
    }

    /// #11135: a failed remote probe must reach the caller carrying what ran and
    /// what the target said, not a sentence that guesses at the cause.
    #[test]
    fn remote_probe_failure_retains_command_evidence_and_survives_into_the_diagnostic() {
        let output = CommandOutput {
            stdout: "partial\n".to_string(),
            stderr: "find: '/srv/site': Permission denied".to_string(),
            success: false,
            exit_code: 1,
            timed_out: false,
            observation: CommandObservation::Complete,
            child_resource: None,
        };
        let error = remote_probe_error(
            "/srv/site",
            "remote content manifest probe failed with exit code 1",
            "remote",
            &output,
        )
        .with_hint("Inspect the captured `command_evidence.stderr`.");

        let evidence = error
            .details
            .get("command_evidence")
            .expect("command evidence survives the conversion");
        assert_eq!(evidence.get("exit_code").and_then(|v| v.as_i64()), Some(1));
        assert_eq!(
            evidence.get("stderr").and_then(|v| v.as_str()),
            Some("find: '/srv/site': Permission denied")
        );
        assert_eq!(
            evidence.get("location").and_then(|v| v.as_str()),
            Some("remote")
        );

        // `ContentManifestComparison.diagnostic` is a `String`, so the evidence
        // has to be rendered into it or it is lost at the last hop.
        let diagnostic = diagnostic_text(&error);
        assert!(diagnostic.contains("Permission denied"), "{diagnostic}");
        assert!(
            diagnostic.contains("validation.invalid_argument"),
            "{diagnostic}"
        );
        assert!(
            diagnostic.contains("hint=Inspect the captured"),
            "{diagnostic}"
        );
    }

    /// A timed-out probe has no exit status to report, and is retryable.
    #[test]
    fn remote_probe_timeout_is_marked_retryable_without_a_fabricated_exit_code() {
        let output = CommandOutput {
            stdout: String::new(),
            stderr: String::new(),
            success: false,
            exit_code: 0,
            timed_out: true,
            observation: CommandObservation::Complete,
            child_resource: None,
        };
        let error = remote_probe_error("/srv/site", "timed out after 20s", "remote", &output)
            .with_retryable(true);
        assert_eq!(error.retryable, Some(true));
        assert_eq!(
            error
                .details
                .get("command_evidence")
                .and_then(|evidence| evidence.get("exit_code"))
                .and_then(|code| code.as_i64()),
            Some(-1)
        );
    }

    /// Long probe output is bounded and the truncation is declared, so a
    /// consumer never mistakes a clipped capture for the whole story.
    #[test]
    fn remote_probe_evidence_is_bounded_and_declares_truncation() {
        let output = CommandOutput {
            stdout: String::new(),
            stderr: "x".repeat(REMOTE_PROBE_DIAGNOSTIC_BYTES * 2),
            success: false,
            exit_code: 1,
            timed_out: false,
            observation: CommandObservation::Complete,
            child_resource: None,
        };
        let error = remote_probe_error("/srv/site", "probe failed", "remote", &output);
        let evidence = error
            .details
            .get("command_evidence")
            .expect("command evidence");
        let stderr = evidence
            .get("stderr")
            .and_then(|value| value.as_str())
            .expect("stderr");
        assert!(stderr.ends_with(DIAGNOSTIC_TRUNCATION_MARKER));
        assert!(stderr.len() <= REMOTE_PROBE_DIAGNOSTIC_BYTES + DIAGNOSTIC_TRUNCATION_MARKER.len());
        assert_eq!(
            evidence.get("truncated").and_then(|value| value.as_bool()),
            Some(true)
        );
    }

    /// The offending path is the diagnostic for a rejected manifest entry, and
    /// it is carried structurally rather than dropped.
    #[test]
    fn rejected_manifest_entries_name_the_offending_path() {
        let unsafe_path = normalize_path("../outside").expect_err("traversal rejected");
        assert_eq!(
            unsafe_path.details.get("id").and_then(|id| id.as_str()),
            Some("../outside")
        );

        let escaping_link =
            validate_link_target("current", "../outside").expect_err("escaping symlink rejected");
        assert_eq!(
            escaping_link.details.get("id").and_then(|id| id.as_str()),
            Some("current")
        );
        assert!(
            problem(&escaping_link).contains("../outside"),
            "{escaping_link}"
        );
    }

    /// A canonical package whose bytes no longer match the leased digest fails
    /// with the remediation attached, not just a mismatch sentence.
    #[test]
    fn archive_hash_mismatch_carries_the_offending_archive_and_a_next_action() {
        let temp = tempfile::tempdir().expect("temp");
        let package = temp.path().join("package.zip");
        write_zip(&package, &[("plugin/version", "1.0.0")]);
        let artifact = release_artifact(&package);
        verify_archive_hash(&package, &artifact).expect("leased bytes verify");
        fs::write(&package, "mutated bytes").expect("mutate");

        let error = verify_archive_hash(&package, &artifact).expect_err("mismatch");
        let package_id = package.display().to_string();
        assert_eq!(
            error.details.get("id").and_then(|id| id.as_str()),
            Some(package_id.as_str())
        );
        assert!(problem(&error).contains("SHA-256 mismatch"));
        assert!(error
            .hints
            .iter()
            .any(|hint| hint.message.contains("--tagged")));
    }

    #[test]
    fn probe_command_and_local_manifest_support_single_file_roots() {
        let temp = tempfile::tempdir().expect("temp");
        let root = temp.path().join("plugin.php");
        fs::write(&root, "release").expect("file");

        let output = run_probe_command(&root, None);
        assert!(
            output.status.success(),
            "stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );

        let remote = parse_remote_manifest(&String::from_utf8_lossy(&output.stdout), &[])
            .expect("remote manifest");
        assert_eq!(
            remote,
            local_manifest(&root).expect("local manifest"),
            "single-file deliverables should use the same canonical entry identity locally and remotely"
        );
    }

    fn probe_fixture_tree(root: &Path) {
        use std::os::unix::fs::PermissionsExt;

        fs::create_dir_all(root.join("assets/js/vendor")).expect("nested");
        fs::create_dir_all(root.join(".git/objects")).expect(".git");
        fs::write(root.join("plugin.php"), "<?php // release").expect("plugin");
        fs::write(root.join("assets/app.js"), "bundle").expect("asset");
        fs::write(root.join("assets/js/vendor/lib.min.js"), "vendor").expect("vendor");
        fs::write(root.join("file with space.txt"), "spaced").expect("space");
        fs::write(root.join("back\\slash.txt"), "backslash").expect("backslash");
        fs::write(root.join("empty.txt"), "").expect("empty");
        fs::write(root.join(".git/HEAD"), "ref: refs/heads/main").expect("git head");
        fs::write(root.join(".git/objects/blob"), "object").expect("git object");
        fs::write(root.join(".homeboy-receipt.json"), "{}").expect("receipt");
        let script = root.join("bin.sh");
        fs::write(&script, "#!/bin/sh\n").expect("script");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod");
        std::os::unix::fs::symlink("assets/app.js", root.join("link.js")).expect("link");
        std::os::unix::fs::symlink("../app.js", root.join("assets/js/up.js")).expect("nested link");
    }

    fn assert_probe_matches_local_manifest(root: &Path, prefix: Option<&str>) {
        let output = run_probe_command(root, prefix);
        assert!(
            output.status.success(),
            "stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            !stdout.contains(".git/") && !stdout.contains(".homeboy-"),
            "excluded paths must be pruned on the target: {stdout}"
        );
        let remote = parse_remote_manifest(&stdout, &[]).expect("remote manifest");
        let local = local_manifest_with_exclusions(root, &[]).expect("local manifest");
        assert_eq!(remote, local, "probe output:\n{stdout}");
        assert!(remote.entries.contains_key("file with space.txt"));
        assert!(remote.entries.contains_key("assets/js/vendor/lib.min.js"));
        assert_eq!(
            remote.entries.get("link.js").map(|entry| entry.kind),
            Some(TreeEntryKind::Symlink)
        );
    }

    #[test]
    fn batched_probe_command_matches_local_manifest_for_mixed_tree() {
        let temp = tempfile::tempdir().expect("temp");
        let root = temp.path().join("extrachill-plugin");
        probe_fixture_tree(&root);
        assert_probe_matches_local_manifest(&root, None);
    }

    /// Release layout (#18365): the live target is a relative symlink into a
    /// versioned release directory. Probing the symlink must describe the
    /// release tree exactly as probing a real directory with the same
    /// contents does, locally and on the target.
    #[test]
    fn symlinked_release_target_probes_like_a_real_directory() {
        let temp = tempfile::tempdir().expect("temp");
        let real = temp.path().join("real").join("extrachill-plugin");
        probe_fixture_tree(&real);
        let parent = temp.path().join("plugins");
        let release =
            parent.join(".homeboy-releases/extrachill-plugin/20261117T000000Z-1/extrachill-plugin");
        probe_fixture_tree(&release);
        let target = parent.join("extrachill-plugin");
        std::os::unix::fs::symlink(
            ".homeboy-releases/extrachill-plugin/20261117T000000Z-1/extrachill-plugin",
            &target,
        )
        .expect("release symlink");

        let probe = |root: &Path| {
            let output = run_probe_command(root, None);
            assert!(output.status.success(), "{output:?}");
            parse_remote_manifest(&String::from_utf8_lossy(&output.stdout), &[])
                .expect("remote manifest")
        };
        let symlinked = probe(&target);
        assert!(
            !symlinked.entries.is_empty(),
            "the probe follows the symlinked root"
        );
        assert_eq!(symlinked, probe(&real), "same manifest as a real directory");
        assert_eq!(
            local_manifest(&target).expect("local manifest"),
            local_manifest(&real).expect("local manifest"),
            "the local walker follows the symlinked root too"
        );
        assert_probe_matches_local_manifest(&target, None);
    }

    #[test]
    fn batched_probe_command_matches_local_manifest_with_shasum_fallback() {
        if Command::new("shasum").arg("--version").output().is_err() {
            return;
        }
        let temp = tempfile::tempdir().expect("temp");
        let root = temp.path().join("extrachill-plugin");
        probe_fixture_tree(&root);
        // Hide sha256sum so the probe takes the `shasum -a 256` path.
        let bin = temp.path().join("bin");
        fs::create_dir_all(&bin).expect("bin");
        for tool in ["sh", "find", "stat", "readlink", "shasum", "perl"] {
            if let Ok(found) = Command::new("sh")
                .arg("-c")
                .arg(format!("command -v {tool}"))
                .output()
            {
                let found = String::from_utf8_lossy(&found.stdout).trim().to_string();
                if !found.is_empty() {
                    std::os::unix::fs::symlink(&found, bin.join(tool)).expect("tool link");
                }
            }
        }
        let prefix = format!("PATH={}", bin.display());
        let output = run_probe_command(&root, Some(&prefix));
        if !output.status.success() {
            // shasum's interpreter may need more of the host PATH than we expose.
            return;
        }
        assert_probe_matches_local_manifest(&root, Some(&prefix));
    }

    #[test]
    fn batched_probe_command_spans_multiple_exec_batches() {
        let temp = tempfile::tempdir().expect("temp");
        let root = temp.path().join("many");
        fs::create_dir_all(&root).expect("root");
        // Long names push `find -exec {} +` past a single argument batch.
        let stem = "n".repeat(200);
        for index in 0..1500 {
            fs::write(root.join(format!("{stem}-{index}.txt")), index.to_string()).expect("file");
        }
        let output = run_probe_command(&root, None);
        assert!(output.status.success());
        let remote = parse_remote_manifest(&String::from_utf8_lossy(&output.stdout), &[])
            .expect("remote manifest");
        assert_eq!(remote.entries.len(), 1500);
        assert_eq!(remote, local_manifest(&root).expect("local manifest"));
    }

    #[test]
    fn batched_probe_command_rejects_newline_and_tab_names() {
        for name in ["line\nbreak.txt", "tab\tname.txt"] {
            let temp = tempfile::tempdir().expect("temp");
            let root = temp.path().join("plugin");
            fs::create_dir_all(&root).expect("root");
            fs::write(root.join("plugin.php"), "release").expect("plugin");
            fs::write(root.join(name), "odd").expect("odd");
            let output = run_probe_command(&root, None);
            assert!(output.status.success());
            assert!(
                parse_remote_manifest(&String::from_utf8_lossy(&output.stdout), &[]).is_err(),
                "record-breaking name {name:?} must fail closed"
            );
        }
    }

    #[test]
    fn missing_remote_probe_tools_return_typed_capability_failures() {
        let temp = tempfile::tempdir().expect("temp");
        let root = temp.path().join("plugin.php");
        fs::write(&root, "release").expect("file");

        for (prefix, capability, name) in [
            ("PATH=''", RemoteProbeCapability::Sha256, "sha256sum"),
            (
                "stat() { return 127; }",
                RemoteProbeCapability::Stat,
                "stat",
            ),
        ] {
            let output = run_probe_command(&root, Some(prefix));
            assert_eq!(output.status.code(), Some(capability.exit_code()), "{name}");

            let error = remote_probe_capability_error(
                root.to_str().expect("path"),
                capability,
                "remote",
                &command_output(&output),
            );
            assert_eq!(
                error.code,
                homeboy_core::error::ErrorCode::RemoteCapabilityMissing
            );
            assert_eq!(
                error
                    .details
                    .get("capability")
                    .and_then(|value| value.as_str()),
                Some(name)
            );
        }
    }

    /// #10290: deploy and recovery now share one walker, and this pins the two
    /// deliberate divergences so a future "just unify them" change has to argue
    /// with a test instead of silently flipping a drift detector's answer.
    ///
    /// Deploy records symlinks and executability, recovery does not. Deploy
    /// applies no caller excludes, so drift inside a shipped directory that the
    /// source checkout happens to gitignore is still reported.
    #[test]
    fn deploy_scan_options_state_the_divergence_from_recovery() {
        let options = deployed_tree_scan_options();
        assert!(options.excludes.is_empty());
        assert!(options.record_symlinks);
        assert!(options.record_executable_mode);
        assert!(options.prune_runtime_artifacts);

        let temp = tempfile::tempdir().expect("temp");
        let local = temp.path().join("local");
        let remote = temp.path().join("plugin");
        fs::create_dir_all(local.join("vendor")).expect("local vendor");
        fs::create_dir_all(remote.join("vendor")).expect("remote vendor");
        fs::write(local.join("vendor/library.php"), "shipped").expect("local vendor file");
        fs::write(remote.join("vendor/library.php"), "tampered").expect("remote vendor file");
        // A checkout-derived ignore rule would hide this; a deploy manifest
        // must not, because the tampered bytes are live on the target.
        assert_eq!(
            differences(
                &local_manifest(&local).expect("local manifest"),
                &local_manifest(&remote).expect("remote manifest"),
            ),
            vec!["vendor/library.php"]
        );
    }

    /// Version-control metadata and this product's transport scratch files are
    /// the entire deploy exclusion policy, and it holds at any depth.
    #[test]
    fn deploy_exclusion_policy_is_vcs_metadata_and_transport_scratch_only() {
        let scratch = homeboy_core::product_identity::PRODUCT_IDENTITY.artifact_prefix;
        assert!(ignored(".git", &[]));
        assert!(ignored(".git/config", &[]));
        assert!(ignored(&format!("{scratch}upload.tmp"), &[]));
        assert!(ignored(&format!("assets/{scratch}upload.tmp"), &[]));
        assert!(!ignored(".gitignore", &[]));
        assert!(!ignored("vendor/library.php", &[]));
        assert!(!ignored("node_modules/pkg/index.js", &[]));
    }
    #[test]
    #[cfg(unix)]
    fn manifest_compares_executability_but_ignores_deploy_normalized_write_bits() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("temp");
        let local = temp.path().join("local");
        let remote = temp.path().join("plugin");
        fs::create_dir_all(&local).expect("local");
        fs::create_dir_all(&remote).expect("remote");
        fs::write(local.join("script"), "same").expect("local script");
        fs::write(remote.join("script"), "same").expect("remote script");
        fs::set_permissions(local.join("script"), fs::Permissions::from_mode(0o644))
            .expect("local mode");
        fs::set_permissions(remote.join("script"), fs::Permissions::from_mode(0o664))
            .expect("remote mode");
        assert!(differences(
            &local_manifest(&local).expect("local manifest"),
            &local_manifest(&remote).expect("remote manifest")
        )
        .is_empty());

        fs::set_permissions(remote.join("script"), fs::Permissions::from_mode(0o775))
            .expect("remote executable mode");
        assert_eq!(
            differences(
                &local_manifest(&local).expect("local manifest"),
                &local_manifest(&remote).expect("remote manifest")
            ),
            vec!["script"]
        );
    }

    #[test]
    fn canonical_package_manifest_matches_install_and_detects_production_drift() {
        let temp = tempfile::tempdir().expect("temp");
        let package = temp.path().join("plugin.zip");
        let remote = temp.path().join("remote");
        fs::create_dir_all(&remote).expect("remote");
        write_zip(
            &package,
            &[
                ("plugin/plugin.php", "<?php // 1.0.0"),
                ("plugin/assets/app.js", "production"),
            ],
        );
        fs::write(remote.join("plugin.php"), "<?php // 1.0.0").expect("plugin");
        fs::create_dir_all(remote.join("assets")).expect("assets");
        fs::write(remote.join("assets/app.js"), "production").expect("asset");

        let client = local_client();
        assert_eq!(
            compare_archive(&package, remote.to_str().expect("path"), &client, &[], None).status,
            "match"
        );

        fs::write(remote.join("assets/app.js"), "modified").expect("drift");
        let drift = compare_archive(&package, remote.to_str().expect("path"), &client, &[], None);
        assert_eq!(drift.status, "different");
        assert_eq!(drift.differences, vec!["assets/app.js"]);

        fs::remove_file(remote.join("plugin.php")).expect("missing");
        let missing = compare_archive(&package, remote.to_str().expect("path"), &client, &[], None);
        assert!(missing.differences.contains(&"plugin.php".to_string()));
    }

    #[test]
    fn package_unavailable_evidence_retains_canonical_scope_and_provenance() {
        let temp = tempfile::tempdir().expect("temp");
        let package = temp.path().join("package.zip");
        write_zip(&package, &[("plugin/version", "1.0.0")]);
        let artifact = release_artifact(&package);
        let result = compare_archive(
            &temp.path().join("missing.zip"),
            temp.path().to_str().expect("path"),
            &local_client(),
            &[],
            Some(&artifact),
        );
        assert_eq!(result.status, "unavailable");
        assert_eq!(result.scope, PACKAGE_SCOPE);
        assert_eq!(
            result
                .provenance
                .as_ref()
                .and_then(|provenance| provenance.artifact_sha256.as_deref()),
            Some(artifact.sha256.as_str())
        );
    }

    #[test]
    fn package_manifest_normalizes_lexical_paths_and_rejects_collisions_and_escapes() {
        let temp = tempfile::tempdir().expect("temp");
        let package = temp.path().join("package.zip");
        write_zip(
            &package,
            &[("./plugin//main.php", "one"), ("plugin/main.php", "two")],
        );
        assert!(archive_manifest(&package, &[]).is_err());
        assert_eq!(
            normalize_path("./plugin//main.php/").expect("normal path"),
            "plugin/main.php"
        );

        let traversal = temp.path().join("traversal.zip");
        write_zip(&traversal, &[("plugin/../outside", "bad")]);
        assert!(archive_manifest(&traversal, &[]).is_err());

        assert!(parse_remote_manifest("l\tdir/current\t777\t../outside\n", &[]).is_ok());
        assert!(parse_remote_manifest("l\tcurrent\t777\t../outside\n", &[]).is_err());
        assert!(parse_remote_manifest("l\tcurrent\t777\t/outside\n", &[]).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn package_manifest_applies_configured_generated_exclusions() {
        let temp = tempfile::tempdir().expect("temp");
        let package = temp.path().join("package.zip");
        let remote = temp.path().join("remote");
        fs::create_dir_all(&remote).expect("remote");
        write_zip(
            &package,
            &[
                ("plugin/plugin.php", "release"),
                ("plugin/cache/state.json", "package state"),
            ],
        );
        fs::write(remote.join("plugin.php"), "release").expect("plugin");
        fs::create_dir_all(remote.join("cache")).expect("cache");
        fs::write(remote.join("cache/state.json"), "runtime state").expect("state");

        let result = compare_archive(
            &package,
            remote.to_str().expect("path"),
            &local_client(),
            &["cache/**".to_string()],
            None,
        );
        assert_eq!(result.status, "match");
        assert_eq!(result.scope, PACKAGE_SCOPE);
    }

    #[test]
    #[cfg(unix)]
    fn archive_symlinks_match_local_and_remote_installs_after_normalization() {
        let temp = tempfile::tempdir().expect("temp");
        let package = temp.path().join("package.zip");
        write_zip(
            &package,
            &[("plugin/target", "same"), ("plugin/link", "./target")],
        );
        mark_zip_entry_as_symlink(&package, "plugin/link");
        let archive = archive_manifest(&package, &[]).expect("archive manifest");

        let local = temp.path().join("installed");
        fs::create_dir_all(&local).expect("installed");
        fs::write(local.join("target"), "same").expect("target");
        std::os::unix::fs::symlink("./target", local.join("link")).expect("link");
        assert!(differences(&archive, &local_manifest(&local).expect("local manifest")).is_empty());

        let remote = parse_remote_manifest(
            &format!(
                "f\ttarget\t644\t{}\nl\tlink\t777\t./target\n",
                sha256(&local.join("target")).expect("hash")
            ),
            &[],
        )
        .expect("remote manifest");
        assert!(differences(&archive, &remote).is_empty());
    }

    fn write_zip(path: &Path, entries: &[(&str, &str)]) {
        let file = fs::File::create(path).expect("zip");
        let mut zip = zip::ZipWriter::new(file);
        for (name, contents) in entries {
            zip.start_file(*name, zip::write::FileOptions::default())
                .expect("zip entry");
            std::io::Write::write_all(&mut zip, contents.as_bytes()).expect("zip contents");
        }
        zip.finish().expect("finish zip");
    }

    fn mark_zip_entry_as_symlink(path: &Path, name: &str) {
        let mut bytes = fs::read(path).expect("zip bytes");
        let mut offset = 0;
        while offset + 46 <= bytes.len() {
            if bytes[offset..].starts_with(b"PK\x01\x02") {
                let name_len =
                    u16::from_le_bytes([bytes[offset + 28], bytes[offset + 29]]) as usize;
                let extra_len =
                    u16::from_le_bytes([bytes[offset + 30], bytes[offset + 31]]) as usize;
                let comment_len =
                    u16::from_le_bytes([bytes[offset + 32], bytes[offset + 33]]) as usize;
                let entry_name = std::str::from_utf8(&bytes[offset + 46..offset + 46 + name_len])
                    .expect("entry name");
                if entry_name == name {
                    bytes[offset + 38..offset + 42]
                        .copy_from_slice(&(0o120777_u32 << 16).to_le_bytes());
                    fs::write(path, bytes).expect("updated zip");
                    return;
                }
                offset += 46 + name_len + extra_len + comment_len;
            } else {
                offset += 1;
            }
        }
        panic!("zip entry not found: {name}");
    }

    fn release_artifact(path: &Path) -> ReleaseArtifactLease {
        ReleaseArtifactLease::test_new(homeboy_core::git::release_download::ReleaseArtifact {
            path: path.to_path_buf(),
            tag: "v1.0.0".to_string(),
            commit: Some("release-commit".to_string()),
            url: "https://example.test/package.zip".to_string(),
            name: "package.zip".to_string(),
            size: fs::metadata(path).expect("metadata").len(),
            sha256: sha256(path).expect("sha"),
        })
        .expect("lease")
    }

    fn local_client() -> SshClient {
        SshClient {
            host: "local".to_string(),
            user: "test".to_string(),
            port: 22,
            identity_file: None,
            auth: None,
            is_local: true,
            env: Default::default(),
        }
    }

    fn run_probe_command(root: &Path, prefix: Option<&str>) -> std::process::Output {
        let command = remote_manifest_probe_command(root.to_str().expect("path"));
        let command = prefix
            .map(|prefix| format!("{prefix}; {command}"))
            .unwrap_or(command);
        Command::new("sh")
            .arg("-c")
            .arg(command)
            .output()
            .expect("probe command")
    }

    fn command_output(output: &std::process::Output) -> CommandOutput {
        CommandOutput {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            success: output.status.success(),
            exit_code: output.status.code().unwrap_or(-1),
            timed_out: false,
            observation: CommandObservation::Complete,
            child_resource: None,
        }
    }
}
