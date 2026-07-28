use std::collections::BTreeMap;
use std::fs;
use std::path::{Component as PathComponent, Path};
use std::time::Duration;

use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use homeboy_core::content_diff::{self, TreeEntry, TreeEntryKind, TreeScanOptions};
use homeboy_core::engine::shell;
use homeboy_core::server::SshClient;
use homeboy_engine_primitives::content_hash;

use super::types::PreparedDeployArtifact;
use super::types::{ContentManifestComparison, ContentManifestProvenance};
use homeboy_core::git::release_download::ReleaseArtifactLease;

const ALGORITHM: &str = "sha256-tree-v1";
const WORKSPACE_SCOPE: &str = "source-tree-installed-tree";
const PACKAGE_SCOPE: &str = "canonical-package-installed-tree";
const PACKAGE_UNAVAILABLE_SCOPE: &str = "canonical-package-unavailable";
const MAX_DIFFERENCES: usize = 20;
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

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
        let mut hash = Sha256::new();
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

    pub(super) fn validate(&self) -> Result<(), String> {
        for (path, entry) in &self.entries {
            if normalize_path(path)? != *path {
                return Err("receipt manifest contains an invalid entry".to_string());
            }
            if entry.kind == TreeEntryKind::File
                && (entry.mode != "0" && entry.mode != "1"
                    || entry.value.len() != 64
                    || !entry.value.bytes().all(|byte| byte.is_ascii_hexdigit()))
            {
                return Err("receipt manifest contains an invalid file entry".to_string());
            }
            if entry.kind == TreeEntryKind::Symlink
                && validate_link_target(path, &entry.value)? != entry.value
            {
                return Err("receipt manifest contains an invalid symlink entry".to_string());
            }
        }
        Ok(())
    }
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
                format!("local manifest unavailable: {error}"),
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
                format!("remote manifest unavailable: {error}"),
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
                format!("canonical package manifest unavailable: {error}"),
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
                format!("remote manifest unavailable: {error}"),
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

pub(super) fn package_manifest(archive: &Path, exclusions: &[String]) -> Result<Manifest, String> {
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
                format!("remote manifest unavailable: {error}"),
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
            format!("prepared package unavailable: {error}"),
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
) -> Result<(), String> {
    let actual = sha256(archive)?;
    if actual != artifact.sha256 {
        return Err(format!(
            "canonical package SHA-256 mismatch for '{}': expected {}, got {}",
            archive.display(),
            artifact.sha256,
            actual
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
                format!("canonical package manifest unavailable: {error}"),
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
                format!("remote manifest unavailable: {error}"),
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

fn local_manifest(root: &Path) -> Result<Manifest, String> {
    local_manifest_with_exclusions(root, &[])
}

fn local_manifest_with_exclusions(root: &Path, exclusions: &[String]) -> Result<Manifest, String> {
    if !root.exists() {
        return Err(format!("{} does not exist", root.display()));
    }
    let entries = content_diff::scan_tree(
        root,
        &TreeScanOptions {
            excludes: exclusions.to_vec(),
            ..deployed_tree_scan_options()
        },
    )
    .map_err(|error| error.to_string())?;
    let entries = entries
        .into_iter()
        .map(|(path, mut entry)| {
            if entry.kind == TreeEntryKind::Symlink {
                entry.value = validate_link_target(&path, &entry.value)?;
            }
            Ok((path, entry))
        })
        .collect::<Result<BTreeMap<_, _>, String>>()?;
    Ok(Manifest { entries })
}

fn archive_manifest(path: &Path, exclusions: &[String]) -> Result<Manifest, String> {
    let file = fs::File::open(path).map_err(|error| error.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| error.to_string())?;
    let mut names = Vec::new();
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|error| error.to_string())?;
        if !entry.is_dir() {
            names.push(normalize_path(entry.name())?);
        }
    }
    let root = archive_root(&names);
    let mut manifest = Manifest::default();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| error.to_string())?;
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
            return Err("archive entry resolves to the package root".to_string());
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
        let mut hash = Sha256::new();
        let value = if kind == TreeEntryKind::Symlink {
            let mut target = String::new();
            std::io::Read::read_to_string(&mut entry, &mut target)
                .map_err(|error| error.to_string())?;
            validate_link_target(&relative, &target)?
        } else {
            std::io::copy(&mut entry, &mut hash).map_err(|error| error.to_string())?;
            format!("{:x}", hash.finalize())
        };
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
            return Err("archive contains duplicate normalized paths".to_string());
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

fn sha256(path: &Path) -> Result<String, String> {
    content_hash::sha256_file(path).map_err(|error| error.to_string())
}

fn remote_manifest(
    root: &str,
    client: &SshClient,
    exclusions: &[String],
) -> Result<Option<Manifest>, String> {
    if client.is_local {
        return local_manifest_with_exclusions(Path::new(root), exclusions).map(Some);
    }
    // The target computes hashes and returns compact records; content never crosses SSH.
    let command = format!("root={}; test -e \"$root\" || exit 44; if ! command -v sha256sum >/dev/null 2>&1 && ! command -v shasum >/dev/null 2>&1; then exit 45; fi; find \"$root\" -mindepth 1 \\( -path '*/.git/*' -o -name .git -o -name '.homeboy-*' \\) -prune -o -type f -exec sh -c 'for f do rel=${{f#\"$1\"/}}; if command -v sha256sum >/dev/null 2>&1; then set -- $(sha256sum \"$f\"); else set -- $(shasum -a 256 \"$f\"); fi; mode=$(stat -c %a \"$f\" 2>/dev/null || stat -f %Lp \"$f\"); printf \"f\\t%s\\t%s\\t%s\\n\" \"$rel\" \"$mode\" \"$1\"; done' sh \"$root\" {{}} + -o -type l -exec sh -c 'for f do rel=${{f#\"$1\"/}}; mode=$(stat -c %a \"$f\" 2>/dev/null || stat -f %Lp \"$f\"); printf \"l\\t%s\\t%s\\t%s\\n\" \"$rel\" \"$mode\" \"$(readlink \"$f\")\"; done' sh \"$root\" {{}} +", shell::quote_path(root));
    let output = client.execute_with_timeout(&command, PROBE_TIMEOUT);
    if output.timed_out {
        return Err(format!("timed out after {}s", PROBE_TIMEOUT.as_secs()));
    }
    if output.exit_code == 44 {
        return Ok(None);
    }
    if !output.success {
        return Err("remote does not provide a supported SHA-256 command".to_string());
    }
    parse_remote_manifest(&output.stdout, exclusions).map(Some)
}

fn parse_remote_manifest(output: &str, exclusions: &[String]) -> Result<Manifest, String> {
    let mut manifest = Manifest::default();
    for line in output.lines() {
        let mut fields = line.splitn(4, '\t');
        let (Some(kind), Some(path), Some(mode), Some(value)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err("malformed remote manifest evidence".to_string());
        };
        let path = normalize_path(path)?;
        if ignored(&path, exclusions) {
            continue;
        }
        let kind = match kind {
            "f" => TreeEntryKind::File,
            "l" => TreeEntryKind::Symlink,
            _ => return Err("unsupported remote manifest entry".to_string()),
        };
        let mode = u32::from_str_radix(mode, 8)
            .map_err(|_| "malformed remote manifest mode".to_string())?;
        let mode = if kind == TreeEntryKind::Symlink {
            "0".to_string()
        } else {
            content_diff::executable_mode_tag(mode)
        };
        if kind == TreeEntryKind::File
            && (value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            return Err("malformed remote SHA-256 evidence".to_string());
        }
        if kind == TreeEntryKind::Symlink {
            let value = validate_link_target(&path, value)?;
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

/// Paths excluded from every deploy manifest, whatever produced it.
///
/// This is the whole exclusion policy: version-control metadata and this
/// product's own transport scratch files. It is intentionally narrower than
/// recovery's exclusion set — see [`deployed_tree_scan_options`].
fn ignored(path: &str, exclusions: &[String]) -> bool {
    content_diff::excluded(path, exclusions) || content_diff::runtime_artifact(path)
}

fn normalize_path(path: &str) -> Result<String, String> {
    let path = path.replace('\\', "/");
    if path.starts_with('/')
        || path
            .bytes()
            .any(|byte| matches!(byte, b'\0' | b'\n' | b'\r' | b'\t'))
    {
        return Err(format!("unsafe manifest path '{path}'"));
    }
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => continue,
            ".." => return Err(format!("unsafe manifest path '{path}'")),
            _ => parts.push(part),
        }
    }
    if parts.is_empty()
        || Path::new(&path)
            .components()
            .any(|part| matches!(part, PathComponent::Prefix(_)))
    {
        return Err(format!("unsafe manifest path '{path}'"));
    }
    Ok(parts.join("/"))
}

fn validate_link_target(entry: &str, target: &str) -> Result<String, String> {
    if target.is_empty()
        || target.starts_with('/')
        || target
            .bytes()
            .any(|byte| matches!(byte, b'\0' | b'\n' | b'\r' | b'\t'))
    {
        return Err("unsafe manifest symlink target".to_string());
    }
    let mut resolved: Vec<&str> = entry.split('/').collect();
    resolved.pop();
    for part in target.split('/') {
        match part {
            "" | "." => continue,
            ".." => {
                if resolved.pop().is_none() {
                    return Err("symlink target escapes package root".to_string());
                }
            }
            _ => {
                resolved.push(part);
            }
        }
    }
    if resolved.is_empty() {
        return Err("unsafe manifest symlink target".to_string());
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
        let result = compare(
            &local,
            remote.to_str().expect("path"),
            &SshClient {
                host: "local".to_string(),
                user: "test".to_string(),
                port: 22,
                identity_file: None,
                auth: None,
                is_local: true,
                env: Default::default(),
            },
            &[],
        );
        assert_eq!(result.status, "different");
        assert!(result.differences.iter().any(|p| p == "changed"));
        assert!(result.differences.iter().any(|p| p == "added"));
        assert!(result.differences.iter().any(|p| p == "deleted"));
        assert!(!result.differences.iter().any(|p| p.contains("homeboy")));
    }
    #[test]
    fn remote_manifest_requires_well_formed_hash_evidence() {
        assert!(parse_remote_manifest("f\tpath\t644\n", &[]).is_err());
        assert!(parse_remote_manifest("f\tpath\t644\tnot-a-hash\n", &[]).is_err());
        assert!(parse_remote_manifest(
            "f\t../path\t644\taaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
            &[]
        )
        .is_err());
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
    fn canonical_package_manifest_ignores_source_only_files_and_detects_production_drift() {
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

        // README, tests, build output, and git metadata are source-only and
        // therefore absent from the authoritative package comparison.
        let source = temp.path().join("source");
        fs::create_dir_all(source.join("tests")).expect("tests");
        fs::create_dir_all(source.join("build")).expect("build");
        fs::write(source.join("README.md"), "source only").expect("readme");
        fs::write(source.join("tests/test.php"), "source only").expect("test");
        fs::write(source.join("build/plugin.zip"), "source only").expect("build");

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
    fn canonical_package_manifest_rejects_stale_or_missing_artifacts() {
        let temp = tempfile::tempdir().expect("temp");
        let stale = temp.path().join("stale.zip");
        let remote = temp.path().join("remote");
        fs::create_dir_all(&remote).expect("remote");
        write_zip(&stale, &[("plugin/version", "1.0.0")]);
        fs::write(remote.join("version"), "2.0.0").expect("remote version");
        let client = local_client();

        assert_eq!(
            compare_archive(&stale, remote.to_str().expect("path"), &client, &[], None).status,
            "different"
        );
        assert_eq!(
            compare_archive(
                &temp.path().join("missing.zip"),
                remote.to_str().expect("path"),
                &client,
                &[],
                None,
            )
            .status,
            "unavailable"
        );
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
    fn package_manifest_normalizes_lexical_paths_and_rejects_collisions() {
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
        assert!(parse_remote_manifest("l\tdir/current\t777\t../outside\n", &[]).is_ok());
    }

    #[test]
    fn release_archive_hash_must_match_the_leased_payload_before_use() {
        let temp = tempfile::tempdir().expect("temp");
        let package = temp.path().join("package.zip");
        write_zip(&package, &[("plugin/version", "1.0.0")]);
        let artifact = release_artifact(&package);
        assert!(verify_archive_hash(&package, &artifact).is_ok());
        fs::write(&package, "mutated bytes").expect("mutate");
        assert!(verify_archive_hash(&package, &artifact).is_err());
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
    fn package_manifest_rejects_unsafe_archive_paths_and_links() {
        let temp = tempfile::tempdir().expect("temp");
        let traversal = temp.path().join("traversal.zip");
        write_zip(&traversal, &[("plugin/../outside", "bad")]);
        assert!(archive_manifest(&traversal, &[]).is_err());

        assert!(parse_remote_manifest("l\tcurrent\t777\t../outside\n", &[]).is_err());
        assert!(parse_remote_manifest("l\tcurrent\t777\t/outside\n", &[]).is_err());
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
}
