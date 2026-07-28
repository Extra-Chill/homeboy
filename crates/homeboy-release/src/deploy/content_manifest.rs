use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::Duration;

use sha2::{Digest, Sha256};

use homeboy_core::content_diff::{self, TreeEntry, TreeEntryKind, TreeScanOptions};
use homeboy_core::engine::shell;
use homeboy_core::server::SshClient;

use super::types::ContentManifestComparison;

const ALGORITHM: &str = "sha256-tree-v1";
const SCOPE: &str = "deployed-tree-excluding-homeboy-runtime";
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

#[derive(Default)]
struct Manifest {
    entries: BTreeMap<String, TreeEntry>,
}

impl Manifest {
    fn digest(&self) -> String {
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
}

pub(super) fn compare(local: &Path, remote: &str, client: &SshClient) -> ContentManifestComparison {
    let local = match local_manifest(local) {
        Ok(manifest) => manifest,
        Err(error) => return unavailable(format!("local manifest unavailable: {error}")),
    };
    let remote = match remote_manifest(remote, client) {
        Ok(Some(manifest)) => manifest,
        Ok(None) => return comparison(&local, None, "missing", Vec::new(), None),
        Err(error) => return unavailable(format!("remote manifest unavailable: {error}")),
    };
    let differences = differences(&local, &remote);
    let status = if differences.is_empty() {
        "match"
    } else {
        "different"
    };
    comparison(&local, Some(&remote), status, differences, None)
}

/// Compare a deployed tree with the canonical archive selected for deployment.
/// Archive entries are the package contract; source-only checkout files are not.
pub(super) fn compare_archive(
    archive: &Path,
    remote: &str,
    client: &SshClient,
) -> ContentManifestComparison {
    let local = match archive_manifest(archive) {
        Ok(manifest) => manifest,
        Err(error) => {
            return unavailable(format!("canonical package manifest unavailable: {error}"))
        }
    };
    let remote = match remote_manifest(remote, client) {
        Ok(Some(manifest)) => manifest,
        Ok(None) => return comparison(&local, None, "missing", Vec::new(), None),
        Err(error) => return unavailable(format!("remote manifest unavailable: {error}")),
    };
    let differences = differences(&local, &remote);
    let status = if differences.is_empty() {
        "match"
    } else {
        "different"
    };
    comparison(&local, Some(&remote), status, differences, None)
}

fn comparison(
    local: &Manifest,
    remote: Option<&Manifest>,
    status: &str,
    differences: Vec<String>,
    diagnostic: Option<String>,
) -> ContentManifestComparison {
    ContentManifestComparison {
        algorithm: ALGORITHM.to_string(),
        scope: SCOPE.to_string(),
        local_digest: Some(local.digest()),
        remote_digest: remote.map(Manifest::digest),
        status: status.to_string(),
        differences,
        diagnostic,
    }
}

fn unavailable(diagnostic: String) -> ContentManifestComparison {
    ContentManifestComparison {
        algorithm: ALGORITHM.to_string(),
        scope: SCOPE.to_string(),
        local_digest: None,
        remote_digest: None,
        status: "unavailable".to_string(),
        differences: Vec::new(),
        diagnostic: Some(diagnostic),
    }
}

fn local_manifest(root: &Path) -> Result<Manifest, String> {
    if !root.exists() {
        return Err(format!("{} does not exist", root.display()));
    }
    let entries = content_diff::scan_tree(root, &deployed_tree_scan_options())
        .map_err(|error| error.to_string())?;
    Ok(Manifest { entries })
}

fn archive_manifest(path: &Path) -> Result<Manifest, String> {
    let file = fs::File::open(path).map_err(|error| error.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| error.to_string())?;
    let mut names = Vec::new();
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|error| error.to_string())?;
        if !entry.is_dir() {
            names.push(entry.name().trim_matches('/').to_string());
        }
    }
    let root = archive_root(&names);
    let mut manifest = Manifest::default();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| error.to_string())?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().trim_matches('/');
        if name.is_empty() || ignored(name) {
            continue;
        }
        let relative = root
            .as_deref()
            .and_then(|root| {
                name.strip_prefix(root)
                    .and_then(|path| path.strip_prefix('/'))
            })
            .unwrap_or(name)
            .to_string();
        let mode = content_diff::executable_mode_tag(entry.unix_mode().unwrap_or(0));
        let mut hash = Sha256::new();
        std::io::copy(&mut entry, &mut hash).map_err(|error| error.to_string())?;
        manifest.entries.insert(
            relative,
            TreeEntry {
                kind: TreeEntryKind::File,
                mode,
                value: format!("{:x}", hash.finalize()),
                bytes: 0,
            },
        );
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

fn remote_manifest(root: &str, client: &SshClient) -> Result<Option<Manifest>, String> {
    if client.is_local {
        return local_manifest(Path::new(root)).map(Some);
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
    parse_remote_manifest(&output.stdout).map(Some)
}

fn parse_remote_manifest(output: &str) -> Result<Manifest, String> {
    let mut manifest = Manifest::default();
    for line in output.lines() {
        let mut fields = line.splitn(4, '\t');
        let (Some(kind), Some(path), Some(mode), Some(value)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err("malformed remote manifest evidence".to_string());
        };
        if path.contains('\n') || ignored(path) {
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
        manifest.entries.insert(
            path.to_string(),
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
fn ignored(path: &str) -> bool {
    content_diff::excluded(path, &[]) || content_diff::runtime_artifact(path)
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
        );
        assert_eq!(result.status, "different");
        assert!(result.differences.iter().any(|p| p == "changed"));
        assert!(result.differences.iter().any(|p| p == "added"));
        assert!(result.differences.iter().any(|p| p == "deleted"));
        assert!(!result.differences.iter().any(|p| p.contains("homeboy")));
    }
    #[test]
    fn remote_manifest_requires_well_formed_hash_evidence() {
        assert!(parse_remote_manifest("f\tpath\t644\n").is_err());
        assert!(parse_remote_manifest("f\tpath\t644\tnot-a-hash\n").is_err());
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
        assert!(ignored(".git"));
        assert!(ignored(".git/config"));
        assert!(ignored(&format!("{scratch}upload.tmp")));
        assert!(ignored(&format!("assets/{scratch}upload.tmp")));
        assert!(!ignored(".gitignore"));
        assert!(!ignored("vendor/library.php"));
        assert!(!ignored("node_modules/pkg/index.js"));
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
            compare_archive(&package, remote.to_str().expect("path"), &client).status,
            "match"
        );

        fs::write(remote.join("assets/app.js"), "modified").expect("drift");
        let drift = compare_archive(&package, remote.to_str().expect("path"), &client);
        assert_eq!(drift.status, "different");
        assert_eq!(drift.differences, vec!["assets/app.js"]);

        fs::remove_file(remote.join("plugin.php")).expect("missing");
        let missing = compare_archive(&package, remote.to_str().expect("path"), &client);
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
            compare_archive(&stale, remote.to_str().expect("path"), &client).status,
            "different"
        );
        assert_eq!(
            compare_archive(
                &temp.path().join("missing.zip"),
                remote.to_str().expect("path"),
                &client
            )
            .status,
            "unavailable"
        );
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
