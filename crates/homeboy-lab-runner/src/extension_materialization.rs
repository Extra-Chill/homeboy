use homeboy_core::error::posix_quote_arg;
use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use sha2::Digest;

use homeboy_core::engine::shell;
use homeboy_core::error::{Error, Result};
use homeboy_core::extension::lifecycle::is_generated_extension_metadata_path;
use homeboy_core::resource_lifecycle_index::{
    ResourceCleanupPolicy, ResourceEvidenceRetention, ResourceLifecycleRecord,
    ResourceLifecycleResourceStatus,
};

use super::{
    copy_snapshot_to_directory, exec, Runner, RunnerExecOptions, RunnerFileTransfer, RunnerKind,
};

fn extension_snapshot_excludes() -> Vec<String> {
    vec!["._*".to_string(), "**/._*".to_string()]
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RunnerExtensionMaterializationRequest {
    pub(crate) id: String,
    pub(crate) revision: String,
    pub(crate) source: RunnerExtensionMaterializationSource,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) enum RunnerExtensionMaterializationSource {
    RemoteGit { url: String, git_ref: String },
    ControllerSnapshot { local_path: PathBuf },
    RunnerPath { path: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerExtensionMaterializationProvenance {
    pub id: String,
    pub source_path: String,
    pub synced_source_path: String,
    pub content_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
    pub dirty: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dirty_fingerprint: Option<String>,
    pub synced_at: String,
    pub dev_overlay: bool,
    pub lifecycle: ResourceLifecycleRecord,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub materialization_source: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerExtensionMaterializationPlan {
    pub id: String,
    pub source_path: String,
    pub synced_source_path: String,
    pub content_hash: String,
}

/// A source snapshot resolved into one Lab job's private extension registry.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct LabJobExtensionOverlay {
    pub(crate) id: String,
    pub(crate) source_path: String,
    pub(crate) remote_path: String,
    pub(crate) source_revision: Option<String>,
    pub(crate) content_hash: String,
    pub(crate) shared_assets: Vec<LabJobExtensionSharedAsset>,
    /// HOME-relative runtime paths the extension declares for isolated HOMEs
    /// (`isolated_home_paths`). The job's private HOME links each one to the
    /// runner account's HOME, matching the local Cook gate (#17754).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) isolated_home_paths: Vec<String>,
}

/// A root-manifest asset staged alongside one extension's isolated Lab overlay.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct LabJobExtensionSharedAsset {
    pub(crate) path: String,
    pub(crate) remote_path: String,
}

pub(crate) fn materialize_lab_job_extension_overlays(
    runner: &Runner,
    runtime_root: &str,
    extension_ids: &[String],
) -> Result<Vec<LabJobExtensionOverlay>> {
    let overlays =
        materialize_lab_job_extension_overlay_snapshots(runner, runtime_root, extension_ids)?;
    if overlays.is_empty() {
        return Ok(overlays);
    }
    let command = lab_job_extension_overlay_command(runtime_root, &overlays);
    let (_, exit_code) = exec(&runner.id, runner_raw_shell(command))?;
    if exit_code != 0 {
        return Err(Error::validation_invalid_argument(
            "extensions",
            "failed to create the job-scoped Homeboy extension overlay on the runner",
            Some(runtime_root.to_string()),
            None,
        ));
    }
    Ok(overlays)
}

fn materialize_lab_job_extension_overlay_snapshots(
    runner: &Runner,
    runtime_root: &str,
    extension_ids: &[String],
) -> Result<Vec<LabJobExtensionOverlay>> {
    let mut overlays = Vec::new();
    let workspace_root = if extension_ids.is_empty() {
        ""
    } else {
        runner.workspace_root.as_deref().ok_or_else(|| {
            Error::validation_invalid_argument(
                "workspace_root",
                "Lab job extension overlays require runner.workspace_root for the hydrated extension cache",
                Some(runner.id.clone()),
                None,
            )
        })?
    };
    for id in extension_ids {
        let manifest = homeboy_core::extension::catalog::load_extension(id)?;
        if let Some(warning) =
            homeboy_core::extension::lifecycle::linked_extension_source_divergence(id)
        {
            eprintln!("WARNING: {}", warning.attributed_message());
        }
        let source_path = manifest.extension_path.clone().ok_or_else(|| {
            Error::validation_invalid_argument(
                "extensions",
                format!("Lab extension `{id}` has no source path"),
                Some(id.clone()),
                None,
            )
        })?;
        let source = PathBuf::from(&source_path);
        let content_hash = extension_source_content_hash(&source)?;
        let resolved_source = source.canonicalize().map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("resolve Lab extension source `{id}`")),
            )
        })?;
        let mut asset_sources = Vec::new();
        for (asset_path, asset_source) in
            homeboy_core::extension::lifecycle::shared_assets_for_extension_source(&resolved_source)
        {
            if asset_source.is_dir() {
                let asset_hash = extension_source_content_hash(&asset_source)?;
                asset_sources.push((asset_path, asset_source, asset_hash));
            }
        }
        let cache_key = lab_extension_cache_key(&content_hash, &asset_sources);
        let remote_path = lab_extension_cache_path(workspace_root, id, &cache_key);
        let setup_command = manifest
            .runtime()
            .and_then(|runtime| runtime.setup_command.clone().map(|cmd| (cmd, runtime)))
            .map(|(cmd, runtime)| (cmd, runtime.entrypoint.clone().unwrap_or_default()));
        ensure_lab_extension_cache(
            runner,
            &LabExtensionCacheBuild {
                id,
                source_path: &source_path,
                content_hash: &content_hash,
                entry: &remote_path,
                shared_assets: &asset_sources,
            },
            setup_command
                .as_ref()
                .map(|(cmd, entrypoint)| (cmd.as_str(), entrypoint.as_str())),
        )?;
        let mut shared_assets = Vec::new();
        for (asset_path, asset_source, asset_hash) in asset_sources {
            let remote_path = format!(
                "{}/shared-assets/{}/{}",
                runtime_root.trim_end_matches('/'),
                sanitize_ref(&asset_path),
                &asset_hash[..16]
            );
            sync_controller_snapshot(
                runner,
                &RunnerExtensionMaterializationPlan {
                    id: format!("{id}-{asset_path}"),
                    source_path: asset_source.display().to_string(),
                    synced_source_path: remote_path.clone(),
                    content_hash: asset_hash,
                },
            )?;
            shared_assets.push(LabJobExtensionSharedAsset {
                path: asset_path,
                remote_path,
            });
        }
        let isolated_home_paths = manifest
            .isolated_home_paths
            .iter()
            .filter(|path| {
                homeboy_extension_contract::manifest::validate_isolated_home_paths(
                    std::slice::from_ref(*path),
                )
                .is_ok()
            })
            .cloned()
            .collect();
        overlays.push(LabJobExtensionOverlay {
            id: id.clone(),
            source_path,
            remote_path,
            source_revision: git_revision(&source),
            content_hash,
            shared_assets,
            isolated_home_paths,
        });
    }
    Ok(overlays)
}

/// Marker written inside a hydrated cache entry once the extension's own
/// `setup_command` has succeeded there. Every job that reuses the entry touches
/// it, so its mtime is the entry's last use for retention.
const LAB_EXTENSION_CACHE_READY: &str = ".homeboy-lab-extension-ready";
/// Cache entries unused for this many days are reclaimed when a new revision
/// of the same extension is hydrated.
const LAB_EXTENSION_CACHE_RETENTION_DAYS: u32 = 7;

/// Runner-wide, content-addressed home of one extension revision.
///
/// Extension snapshots carry only tracked source, so installed dependencies
/// (`vendor/`, `node_modules/`) never cross the wire. A Lab job therefore
/// links to an entry the runner hydrated with the extension's own
/// `setup_command`, exactly as `homeboy extension install` does for the
/// runner's global install. The entry is keyed by the controller content hash,
/// so concurrent jobs on different revisions still see distinct sources
/// (Extra-Chill/homeboy#19196).
pub(crate) fn lab_extension_cache_path(
    workspace_root: &str,
    id: &str,
    content_hash: &str,
) -> String {
    format!(
        "{}/_lab_workspaces/extension-cache/{}/{}",
        workspace_root.trim_end_matches('/'),
        sanitize_ref(id),
        &content_hash[..16]
    )
}

/// One extension revision to hydrate, with the shared assets its root
/// manifest declares (path relative to the extensions root, source, hash).
struct LabExtensionCacheBuild<'a> {
    id: &'a str,
    source_path: &'a str,
    content_hash: &'a str,
    entry: &'a str,
    shared_assets: &'a [(String, PathBuf, String)],
}

/// The cache key covers the extension and every shared asset it is hydrated
/// beside, so a changed asset gets a fresh, re-verified build.
fn lab_extension_cache_key(content_hash: &str, assets: &[(String, PathBuf, String)]) -> String {
    let mut hasher = homeboy_engine_primitives::content_hash::sha256_hasher();
    hasher.update(content_hash.as_bytes());
    for (path, _, hash) in assets {
        hasher.update([0]);
        hasher.update(path.as_bytes());
        hasher.update([0]);
        hasher.update(hash.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

/// Make `build.entry` a ready, hydrated extension entry.
///
/// A ready entry is reused without uploading. Otherwise a unique build
/// directory is laid out like an installed extensions root: the extension at
/// `<build>/<id>` and each declared shared asset at `<build>/<asset>`, so
/// `setup_command` sees the same siblings `homeboy extension install` gives
/// it. Setup runs in `<build>/<id>`, which stays the entry's permanent
/// location because setup may bake absolute paths. The entry is published
/// with `ln -sn`, which fails instead of replacing when another job won the
/// race; the loser discards its build.
fn ensure_lab_extension_cache(
    runner: &Runner,
    build: &LabExtensionCacheBuild<'_>,
    setup: Option<(&str, &str)>,
) -> Result<()> {
    let entry = build.entry.trim_end_matches('/');
    let (_, ready) = exec(
        &runner.id,
        runner_raw_shell(lab_extension_cache_probe_command(entry)),
    )?;
    if ready == 0 {
        return Ok(());
    }
    let build_root = format!("{entry}.build-{}", uuid::Uuid::new_v4().simple());
    let extension_dir = format!("{build_root}/{}", sanitize_ref(build.id));
    sync_controller_snapshot(
        runner,
        &RunnerExtensionMaterializationPlan {
            id: build.id.to_string(),
            source_path: build.source_path.to_string(),
            synced_source_path: extension_dir.clone(),
            content_hash: build.content_hash.to_string(),
        },
    )?;
    for (asset_path, asset_source, asset_hash) in build.shared_assets {
        sync_controller_snapshot(
            runner,
            &RunnerExtensionMaterializationPlan {
                id: format!("{}-{asset_path}", build.id),
                source_path: asset_source.display().to_string(),
                synced_source_path: format!("{build_root}/{asset_path}"),
                content_hash: asset_hash.clone(),
            },
        )?;
    }
    let setup = setup.map(|(command, entrypoint)| {
        homeboy_engine_primitives::template::render(
            command,
            &[
                ("extension_path", extension_dir.as_str()),
                ("entrypoint", entrypoint),
            ],
        )
    });
    let (output, exit_code) = exec(
        &runner.id,
        runner_raw_shell(lab_extension_cache_publish_command(
            entry,
            &build_root,
            &extension_dir,
            setup.as_deref(),
        ))
        .with_env(runner.env.clone()),
    )?;
    if exit_code != 0 {
        return Err(Error::validation_invalid_argument(
            "extensions",
            format!(
                "failed to hydrate Lab extension `{}` on the runner: {}",
                build.id,
                tail_lines(&runner_exec_detail(&output), 40)
            ),
            Some(entry.to_string()),
            None,
        ));
    }
    Ok(())
}

fn tail_lines(text: &str, count: usize) -> String {
    let lines = text.lines().collect::<Vec<_>>();
    lines[lines.len().saturating_sub(count)..].join("\n")
}

fn lab_extension_cache_probe_command(entry: &str) -> String {
    let ready = shell::quote_path(&format!("{entry}/{LAB_EXTENSION_CACHE_READY}"));
    format!("test -f {ready} && touch {ready}")
}

fn lab_extension_cache_publish_command(
    entry: &str,
    build_root: &str,
    extension_dir: &str,
    setup: Option<&str>,
) -> String {
    let parent = Path::new(entry)
        .parent()
        .and_then(Path::to_str)
        .expect("extension cache entry has a UTF-8 parent");
    let setup = setup
        .map(|command| format!("(cd \"$ext\" && {command}) >&2\n"))
        .unwrap_or_default();
    format!(
        "set -eu\nentry={entry}\nbuild={build}\next={ext}\nparent={parent}\n\
{setup}\
touch \"$ext\"/{ready}\n\
if ! ln -sn \"$ext\" \"$entry\" 2>/dev/null; then\n\
  test -f \"$entry\"/{ready} || {{ echo \"extension cache entry $entry exists but is not ready\" >&2; exit 1; }}\n\
  rm -rf \"$build\"\n\
fi\n\
# Reclaim builds unused for {days} days, crashed builds older than a day, then dangling entries.\n\
for dir in \"$parent\"/*.build-*; do\n\
  [ -d \"$dir\" ] || continue\n\
  [ \"$dir\" = \"$build\" ] && continue\n\
  marker=$(find \"$dir\" -mindepth 2 -maxdepth 2 -name {ready} 2>/dev/null | head -n 1)\n\
  if [ -n \"$marker\" ]; then\n\
    [ -n \"$(find \"$marker\" -mtime +{days} 2>/dev/null)\" ] && rm -rf \"$dir\"\n\
  else\n\
    [ -n \"$(find \"$dir\" -maxdepth 0 -mtime +1 2>/dev/null)\" ] && rm -rf \"$dir\"\n\
  fi\n\
done\n\
for link in \"$parent\"/*; do\n\
  if [ -L \"$link\" ] && [ ! -e \"$link\" ]; then rm -f \"$link\"; fi\n\
done\n",
        entry = shell::quote_path(entry),
        build = shell::quote_path(build_root),
        ext = shell::quote_path(extension_dir),
        parent = shell::quote_path(parent),
        ready = LAB_EXTENSION_CACHE_READY,
        days = LAB_EXTENSION_CACHE_RETENTION_DAYS,
    )
}

fn lab_job_extension_overlay_command(
    runtime_root: &str,
    overlays: &[LabJobExtensionOverlay],
) -> String {
    let extension_links = overlays
        .iter()
        .map(|overlay| {
            format!(
                "rm -rf {target}; ln -s {source} {target}",
                target = shell::quote_path(&format!(
                    "{}/home/.config/homeboy/extensions/{}",
                    runtime_root.trim_end_matches('/'),
                    overlay.id
                )),
                source = shell::quote_path(&overlay.remote_path),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let shared_asset_links = overlays
        .iter()
        .flat_map(|overlay| overlay.shared_assets.iter())
        .map(|asset| {
            let target = lab_job_shared_asset_target(runtime_root, &asset.path);
            let parent = Path::new(&target)
                .parent()
                .and_then(Path::to_str)
                .expect("shared asset target has a UTF-8 parent");
            format!(
                "mkdir -p {parent}; rm -rf {target}; ln -s {source} {target}",
                parent = shell::quote_path(parent),
                target = shell::quote_path(&target),
                source = shell::quote_path(&asset.remote_path),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let home_path_links = lab_job_isolated_home_path_links(runtime_root, overlays);
    let links = [extension_links, shared_asset_links, home_path_links]
        .into_iter()
        .filter(|links| !links.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "set -eu\nruntime={runtime}\n# A job gets only its declared extensions; runner-global config may contain mutable links.\nmkdir -p \"$runtime/home/.config/homeboy/extensions\"\n{links}\n",
        runtime = shell::quote_path(runtime_root),
    )
}

/// Link each declared `isolated_home_paths` entry from the runner account's
/// HOME into the job HOME. The overlay script runs before the job swaps HOME,
/// so `$HOME` here is the runner's own. Like the local gate, a path is linked
/// only when the runner has it as a directory and the job HOME has nothing there.
fn lab_job_isolated_home_path_links(
    runtime_root: &str,
    overlays: &[LabJobExtensionOverlay],
) -> String {
    let mut seen = std::collections::BTreeSet::new();
    overlays
        .iter()
        .flat_map(|overlay| overlay.isolated_home_paths.iter())
        .filter(|path| seen.insert((*path).clone()))
        .map(|path| {
            let target = format!("{}/home/{path}", runtime_root.trim_end_matches('/'));
            let parent = Path::new(&target)
                .parent()
                .and_then(Path::to_str)
                .expect("isolated home path target has a UTF-8 parent");
            format!(
                "if [ -n \"${{HOME:-}}\" ] && [ -d \"$HOME\"/{rel} ] && [ ! -e {target} ] && [ ! -L {target} ]; then mkdir -p {parent}; ln -s \"$(cd \"$HOME\"/{rel} && pwd -P)\" {target}; fi",
                rel = shell::quote_path(path),
                parent = shell::quote_path(parent),
                target = shell::quote_path(&target),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn lab_job_shared_asset_target(runtime_root: &str, asset_path: &str) -> String {
    format!(
        "{}/home/.config/homeboy/extensions/{asset_path}",
        runtime_root.trim_end_matches('/')
    )
}

#[cfg(test)]
mod lab_job_overlay_tests {
    use super::*;

    fn overlay(id: &str, root: &str, hash: &str) -> LabJobExtensionOverlay {
        LabJobExtensionOverlay {
            id: id.to_string(),
            source_path: format!("/controller/{id}"),
            remote_path: format!("{root}/extensions/{id}/{hash}"),
            source_revision: Some(hash.to_string()),
            content_hash: hash.to_string(),
            shared_assets: Vec::new(),
            isolated_home_paths: Vec::new(),
        }
    }

    #[test]
    fn declared_isolated_home_paths_link_runner_home_into_job_home() {
        let mut wordpress = overlay("wordpress", "/runner/jobs/a", "aaaaaaaaaaaaaaaa");
        wordpress.isolated_home_paths = vec![
            ".cache/homeboy/wp-codebox".to_string(),
            ".wordpress-playground".to_string(),
        ];
        let mut other = overlay("other", "/runner/jobs/a", "bbbbbbbbbbbbbbbb");
        other.isolated_home_paths = vec![".wordpress-playground".to_string()];
        let command = lab_job_extension_overlay_command("/runner/jobs/a", &[wordpress, other]);

        assert!(command.contains("/runner/jobs/a/home/.cache/homeboy/wp-codebox"));
        assert!(command.contains("\"$HOME\"/'.cache/homeboy/wp-codebox' && pwd -P"));
        assert!(!command.contains("$HOME/.config/homeboy"));
        assert_eq!(
            command
                .matches("\"$HOME\"/'.wordpress-playground' && pwd -P")
                .count(),
            1
        );
    }

    #[cfg(unix)]
    #[test]
    fn isolated_home_path_links_resolve_against_runner_home_and_keep_existing_entries() {
        let root = tempfile::tempdir().expect("tempdir");
        let runner_home = root.path().join("runner-home");
        let cache = runner_home.join(".cache/homeboy/wp-codebox/source");
        std::fs::create_dir_all(&cache).expect("runner cache");
        std::fs::create_dir_all(runner_home.join(".wordpress-playground")).expect("archive cache");
        let runtime = root.path().join("job/extension-runtime");
        let existing = runtime.join("home/.wordpress-playground");
        std::fs::create_dir_all(&existing).expect("existing job entry");
        let mut wordpress = overlay("wordpress", runtime.to_str().unwrap(), "aaaaaaaaaaaaaaaa");
        wordpress.isolated_home_paths = vec![
            ".cache/homeboy/wp-codebox".to_string(),
            ".wordpress-playground".to_string(),
            ".missing/dir".to_string(),
        ];
        let script = lab_job_isolated_home_path_links(runtime.to_str().unwrap(), &[wordpress]);
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("set -eu\n{script}\n"))
            .env("HOME", &runner_home)
            .status()
            .expect("run link script");
        assert!(status.success());

        let linked = runtime.join("home/.cache/homeboy/wp-codebox");
        assert!(std::fs::symlink_metadata(&linked)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            std::fs::canonicalize(&linked).unwrap(),
            std::fs::canonicalize(runner_home.join(".cache/homeboy/wp-codebox")).unwrap()
        );
        assert!(linked.join("source").is_dir());
        assert!(!std::fs::symlink_metadata(&existing)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(!runtime.join("home/.missing").exists());
    }

    fn run_sh(script: &str) -> bool {
        std::process::Command::new("sh")
            .arg("-c")
            .arg(script)
            .status()
            .expect("run script")
            .success()
    }

    /// A build laid out like an installed extensions root: `<build>/wordpress`
    /// beside a declared shared asset `<build>/agent-runtimes`.
    fn make_build(entry: &Path, nonce: &str) -> (PathBuf, PathBuf) {
        let build = PathBuf::from(format!("{}.build-{nonce}", entry.display()));
        let ext = build.join("wordpress");
        std::fs::create_dir_all(&ext).expect("extension dir");
        std::fs::create_dir_all(build.join("agent-runtimes")).expect("shared asset dir");
        std::fs::write(ext.join("composer.json"), "{}").expect("source file");
        (build, ext)
    }

    fn publish(entry: &Path, build: &(PathBuf, PathBuf), setup: Option<&str>) -> bool {
        run_sh(&lab_extension_cache_publish_command(
            entry.to_str().unwrap(),
            build.0.to_str().unwrap(),
            build.1.to_str().unwrap(),
            setup,
        ))
    }

    #[test]
    fn extension_cache_publish_hydrates_once_beside_shared_assets_and_race_losers_discard() {
        let root = tempfile::tempdir().expect("tempdir");
        let entry = root
            .path()
            .join("extension-cache/wordpress/aaaaaaaaaaaaaaaa");
        let entry_str = entry.to_str().unwrap();
        assert!(!run_sh(&lab_extension_cache_probe_command(entry_str)));

        let winner = make_build(&entry, "one");
        let setup = "test -d ../agent-runtimes && mkdir -p vendor && touch vendor/autoload.php";
        assert!(publish(&entry, &winner, Some(setup)));
        assert!(std::fs::symlink_metadata(&entry)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(entry.join("vendor/autoload.php").is_file());
        assert!(run_sh(&lab_extension_cache_probe_command(entry_str)));

        let loser = make_build(&entry, "two");
        assert!(publish(&entry, &loser, Some(setup)));
        assert!(!loser.0.exists(), "race loser discards its build");
        assert_eq!(
            std::fs::canonicalize(&entry).unwrap(),
            std::fs::canonicalize(&winner.1).unwrap()
        );
    }

    #[test]
    fn extension_cache_failed_setup_publishes_nothing() {
        let root = tempfile::tempdir().expect("tempdir");
        let entry = root
            .path()
            .join("extension-cache/wordpress/bbbbbbbbbbbbbbbb");
        let build = make_build(&entry, "one");
        assert!(!publish(&entry, &build, Some("exit 7")));
        assert!(std::fs::symlink_metadata(&entry).is_err());
        assert!(!run_sh(&lab_extension_cache_probe_command(
            entry.to_str().unwrap()
        )));
    }

    #[test]
    fn extension_cache_publish_reclaims_stale_builds_and_dangling_entries() {
        let root = tempfile::tempdir().expect("tempdir");
        let parent = root.path().join("extension-cache/wordpress");
        let old_entry = parent.join("cccccccccccccccc");
        let stale = make_build(&old_entry, "old");
        std::fs::write(stale.1.join(LAB_EXTENSION_CACHE_READY), "").unwrap();
        std::os::unix::fs::symlink(&stale.1, &old_entry).unwrap();
        let crashed = make_build(&parent.join("dddddddddddddddd"), "crashed");
        let recent_entry = parent.join("eeeeeeeeeeeeeeee");
        let recent = make_build(&recent_entry, "recent");
        std::fs::write(recent.1.join(LAB_EXTENSION_CACHE_READY), "").unwrap();
        std::os::unix::fs::symlink(&recent.1, &recent_entry).unwrap();
        assert!(run_sh(&format!(
            "touch -t 202001010000 {} {}",
            shell::quote_path(
                &stale
                    .1
                    .join(LAB_EXTENSION_CACHE_READY)
                    .display()
                    .to_string()
            ),
            shell::quote_path(&crashed.0.display().to_string()),
        )));

        let entry = parent.join("ffffffffffffffff");
        let build = make_build(&entry, "new");
        assert!(publish(&entry, &build, None));
        assert!(entry.join(LAB_EXTENSION_CACHE_READY).is_file());
        assert!(!stale.0.exists() && std::fs::symlink_metadata(&old_entry).is_err());
        assert!(!crashed.0.exists());
        assert!(recent_entry.join(LAB_EXTENSION_CACHE_READY).is_file());
    }
}

pub(crate) fn plan_controller_snapshot_extension(
    workspace_root: &str,
    id: &str,
    local_path: &Path,
) -> Result<RunnerExtensionMaterializationPlan> {
    let source_path = expand_path(&local_path.display().to_string());
    let content_hash = extension_source_content_hash(&source_path)?;
    Ok(RunnerExtensionMaterializationPlan {
        id: id.to_string(),
        source_path: source_path.display().to_string(),
        synced_source_path: dev_extension_path(workspace_root, id, &content_hash),
        content_hash,
    })
}

pub(crate) fn materialize_runner_extension_with_env(
    runner: &Runner,
    homeboy_path: &str,
    env: Option<HashMap<String, String>>,
    request: &RunnerExtensionMaterializationRequest,
) -> Result<RunnerExtensionMaterializationProvenance> {
    materialize_runner_extension_with_exec(
        runner,
        homeboy_path,
        env,
        request,
        &mut |runner_id, options| exec(runner_id, options),
    )
}

pub(crate) fn materialize_runner_extension_with_exec(
    runner: &Runner,
    homeboy_path: &str,
    env: Option<HashMap<String, String>>,
    request: &RunnerExtensionMaterializationRequest,
    exec_runner: &mut impl FnMut(&str, RunnerExecOptions) -> Result<(super::RunnerExecOutput, i32)>,
) -> Result<RunnerExtensionMaterializationProvenance> {
    match &request.source {
        RunnerExtensionMaterializationSource::ControllerSnapshot { local_path } => {
            let workspace_root = runner.workspace_root.as_deref().ok_or_else(|| {
                Error::validation_invalid_argument(
                    "workspace_root",
                    "runner extension snapshot materialization requires runner.workspace_root",
                    Some(runner.id.clone()),
                    None,
                )
            })?;
            let plan = plan_controller_snapshot_extension(workspace_root, &request.id, local_path)?;
            sync_controller_snapshot(runner, &plan)?;
            let exists = runner_extension_exists(
                runner,
                env.as_ref(),
                homeboy_path,
                &request.id,
                exec_runner,
            )
            .map_err(|detail| {
                Error::validation_invalid_argument(
                    "extensions",
                    detail,
                    Some(request.id.clone()),
                    None,
                )
            })?;
            let command = local_path_install_command(
                homeboy_path,
                &plan.synced_source_path,
                &request.id,
                &plan.content_hash,
                exists,
            );
            run_materialization_command(runner, env.as_ref(), command, exec_runner)?;
            Ok(controller_snapshot_provenance(&runner.id, &plan))
        }
        RunnerExtensionMaterializationSource::RunnerPath { path } => {
            let exists = runner_extension_exists(
                runner,
                env.as_ref(),
                homeboy_path,
                &request.id,
                exec_runner,
            )
            .map_err(|detail| {
                Error::validation_invalid_argument(
                    "extensions",
                    detail,
                    Some(request.id.clone()),
                    None,
                )
            })?;
            let command = local_path_install_command(
                homeboy_path,
                path,
                &request.id,
                &request.revision,
                exists,
            );
            run_materialization_command(runner, env.as_ref(), command, exec_runner)?;
            Ok(runner_path_provenance(&runner.id, request, path))
        }
        RunnerExtensionMaterializationSource::RemoteGit { url, git_ref } => {
            let exists = runner_extension_exists(
                runner,
                env.as_ref(),
                homeboy_path,
                &request.id,
                exec_runner,
            )
            .map_err(|detail| {
                Error::validation_invalid_argument(
                    "extensions",
                    detail,
                    Some(request.id.clone()),
                    None,
                )
            })?;
            let mut command = vec![
                homeboy_path.to_string(),
                "extension".to_string(),
                "install".to_string(),
                url.clone(),
                "--id".to_string(),
                request.id.clone(),
                "--ref".to_string(),
                git_ref.clone(),
            ];
            if exists {
                command.push("--replace".to_string());
            }
            run_materialization_command(runner, env.as_ref(), command, exec_runner)?;
            Ok(remote_git_provenance(
                &runner.id, request, url, git_ref, exists,
            ))
        }
    }
}

fn local_path_install_command(
    homeboy_path: &str,
    source_path: &str,
    extension_id: &str,
    revision: &str,
    replace: bool,
) -> Vec<String> {
    let mut command = vec![
        homeboy_path.to_string(),
        "extension".to_string(),
        "install".to_string(),
        source_path.to_string(),
        "--id".to_string(),
        extension_id.to_string(),
        "--ref".to_string(),
        revision.to_string(),
    ];
    if replace {
        command.push("--replace".to_string());
    }
    command
}

fn sync_controller_snapshot(
    runner: &Runner,
    plan: &RunnerExtensionMaterializationPlan,
) -> Result<()> {
    let synced_source_path = plan.synced_source_path.trim_end_matches('/');
    let excludes = extension_snapshot_excludes();
    match runner.kind {
        RunnerKind::Local => {
            let destination = Path::new(synced_source_path);
            copy_snapshot_to_directory(Path::new(&plan.source_path), destination, &excludes)?;
            scrub_extension_snapshot(destination)
        }
        RunnerKind::Ssh => {
            let transfer = RunnerFileTransfer::for_runner(runner)?;
            upload_snapshot(runner, plan, &transfer)
        }
    }
}

fn upload_snapshot(
    runner: &Runner,
    plan: &RunnerExtensionMaterializationPlan,
    transfer: &RunnerFileTransfer,
) -> Result<()> {
    let tempdir = crate::staging_temp::dir().map_err(|err| {
        Error::internal_io(err.to_string(), Some("stage extension overlay".to_string()))
    })?;
    let staged = tempdir.path().join("source");
    let excludes = extension_snapshot_excludes();
    copy_snapshot_to_directory(Path::new(&plan.source_path), &staged, &excludes)?;
    scrub_extension_snapshot(&staged)?;
    let archive = tempdir.path().join("source.tar");
    let status = Command::new("tar")
        .env("COPYFILE_DISABLE", "1")
        .args([
            "--no-xattrs",
            "-C",
            &staged.display().to_string(),
            "-cf",
            &archive.display().to_string(),
            ".",
        ])
        .status()
        .map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some("archive extension overlay".to_string()),
            )
        })?;
    if !status.success() {
        return Err(Error::internal_unexpected(
            "archive extension overlay failed".to_string(),
        ));
    }

    let remote_archive = format!("{}.tar", plan.synced_source_path.trim_end_matches('/'));
    let remote_parent = Path::new(&plan.synced_source_path)
        .parent()
        .and_then(Path::to_str)
        .ok_or_else(|| {
            Error::internal_json("invalid remote extension overlay path".to_string(), None)
        })?;
    transfer.ensure_directory(remote_parent)?;
    transfer.upload_file(&archive.display().to_string(), &remote_archive)?;
    let extract = extension_snapshot_extract_command(&plan.synced_source_path, &remote_archive);
    let (_output, exit_code) = exec(&runner.id, runner_raw_shell(extract))?;
    if exit_code != 0 {
        return Err(Error::validation_invalid_argument(
            "extensions",
            format!(
                "failed to unpack extension snapshot '{}' on runner",
                plan.id
            ),
            Some(plan.synced_source_path.clone()),
            None,
        ));
    }
    Ok(())
}

fn extension_snapshot_extract_command(destination: &str, archive: &str) -> String {
    let destination = shell::quote_path(destination);
    format!(
        "set -e\nrm -rf {destination}\nmkdir -p {destination}\ntar -xf {archive} -C {destination}\nfind {destination} -name '._*' -delete\nrm -f {archive}\n",
        archive = shell::quote_path(archive),
    )
}

fn run_materialization_command(
    runner: &Runner,
    env: Option<&HashMap<String, String>>,
    command: Vec<String>,
    exec_runner: &mut impl FnMut(&str, RunnerExecOptions) -> Result<(super::RunnerExecOutput, i32)>,
) -> Result<()> {
    let result = exec_runner(
        &runner.id,
        runner_exec_options(runner, env, command.clone()),
    );
    match result {
        Ok((_output, 0)) => Ok(()),
        Ok((output, exit_code)) => Err(Error::validation_invalid_argument(
            "extensions",
            format!(
                "runner extension materialization failed with exit code {exit_code}: {}",
                runner_exec_detail(&output)
            ),
            Some(shell_command(&command)),
            None,
        )),
        Err(err) => Err(Error::validation_invalid_argument(
            "extensions",
            format!("runner extension materialization failed: {}", err.message),
            Some(shell_command(&command)),
            None,
        )),
    }
}

/// One extension to replace on a runner from a git source at a revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RunnerExtensionGitInstall<'a> {
    pub(crate) id: &'a str,
    pub(crate) source_url: &'a str,
    pub(crate) source_revision: &'a str,
}

/// Apply git installs planned by the extension-parity owner. Installs sharing
/// one source and revision run from a single checkout in one runner job instead
/// of one job (and one fetch) apiece. A batch failure — including a runner that
/// predates repeated `--id` — falls back to per-extension installs so failures
/// stay exact. Returns one result per install, in input order.
pub(crate) fn apply_runner_extension_git_installs(
    runner: &Runner,
    homeboy_path: &str,
    installs: &[RunnerExtensionGitInstall<'_>],
    exec_runner: &mut impl FnMut(&str, RunnerExecOptions) -> Result<(super::RunnerExecOutput, i32)>,
) -> Vec<Result<()>> {
    let mut results: Vec<Option<Result<()>>> = installs.iter().map(|_| None).collect();
    let mut groups: Vec<(&str, &str, Vec<usize>)> = Vec::new();
    for (index, install) in installs.iter().enumerate() {
        match groups.iter_mut().find(|(url, revision, _)| {
            *url == install.source_url && *revision == install.source_revision
        }) {
            Some((_, _, members)) => members.push(index),
            None => groups.push((install.source_url, install.source_revision, vec![index])),
        }
    }
    for (source_url, source_revision, members) in groups {
        if members.len() > 1 {
            let ids: Vec<&str> = members.iter().map(|index| installs[*index].id).collect();
            let batch = run_materialization_command(
                runner,
                None,
                runner_extension_git_install_command(
                    homeboy_path,
                    source_url,
                    &ids,
                    source_revision,
                ),
                exec_runner,
            );
            if batch.is_ok() {
                for index in members {
                    results[index] = Some(Ok(()));
                }
                continue;
            }
        }
        for index in members {
            results[index] = Some(run_materialization_command(
                runner,
                None,
                runner_extension_git_install_command(
                    homeboy_path,
                    source_url,
                    &[installs[index].id],
                    source_revision,
                ),
                exec_runner,
            ));
        }
    }
    results
        .into_iter()
        .map(|result| result.expect("every install belongs to one group"))
        .collect()
}

/// `extension install --replace` for one or more extensions of one source at
/// one revision. Repeated `--id` installs them all from a single checkout.
pub(crate) fn runner_extension_git_install_command(
    homeboy_path: &str,
    source_url: &str,
    extension_ids: &[&str],
    source_revision: &str,
) -> Vec<String> {
    let mut command = vec![
        homeboy_path.to_string(),
        "extension".to_string(),
        "install".to_string(),
        source_url.to_string(),
    ];
    for extension_id in extension_ids {
        command.extend(["--id".to_string(), extension_id.to_string()]);
    }
    command.extend([
        "--ref".to_string(),
        source_revision.to_string(),
        "--replace".to_string(),
    ]);
    command
}

pub(crate) fn runner_extension_exists(
    runner: &Runner,
    env: Option<&HashMap<String, String>>,
    homeboy_path: &str,
    extension_id: &str,
    exec_runner: &mut impl FnMut(&str, RunnerExecOptions) -> Result<(super::RunnerExecOutput, i32)>,
) -> std::result::Result<bool, String> {
    let result = exec_runner(
        &runner.id,
        runner_exec_options(
            runner,
            env,
            vec![
                homeboy_path.to_string(),
                "extension".to_string(),
                "show".to_string(),
                extension_id.to_string(),
            ],
        ),
    );

    match result {
        Ok((_output, 0)) => Ok(true),
        Ok((_output, 4)) => Ok(false),
        Ok((output, exit_code)) => Err(format!(
            "extension {} lookup failed with exit code {}: {}",
            extension_id,
            exit_code,
            runner_exec_detail(&output)
        )),
        Err(err) => Err(format!(
            "extension {} lookup failed: {}",
            extension_id, err.message
        )),
    }
}

fn runner_exec_options(
    runner: &Runner,
    env: Option<&HashMap<String, String>>,
    command: Vec<String>,
) -> RunnerExecOptions {
    RunnerExecOptions::raw_command(command)
        .with_env(env.cloned().unwrap_or_else(|| runner.env.clone()))
}

fn runner_raw_shell(script: String) -> RunnerExecOptions {
    RunnerExecOptions::raw_command(vec!["bash".to_string(), "-lc".to_string(), script])
}

fn runner_exec_detail(output: &super::RunnerExecOutput) -> String {
    if !output.stderr.trim().is_empty() {
        output.stderr.trim().to_string()
    } else if !output.stdout.trim().is_empty() {
        output.stdout.trim().to_string()
    } else {
        "no runner output".to_string()
    }
}

fn controller_snapshot_provenance(
    runner_id: &str,
    plan: &RunnerExtensionMaterializationPlan,
) -> RunnerExtensionMaterializationProvenance {
    let source = Path::new(&plan.source_path);
    let dirty_fingerprint =
        match homeboy_core::extension::lifecycle::read_source_cleanliness_at(source) {
            homeboy_core::extension::lifecycle::ExtensionSourceCleanliness::Dirty {
                fingerprint,
                ..
            } => Some(fingerprint),
            _ => None,
        };
    RunnerExtensionMaterializationProvenance {
        id: plan.id.clone(),
        source_path: plan.source_path.clone(),
        synced_source_path: plan.synced_source_path.clone(),
        content_hash: plan.content_hash.clone(),
        source_revision: git_revision(source),
        dirty: dirty_fingerprint.is_some(),
        dirty_fingerprint,
        synced_at: chrono::Utc::now().to_rfc3339(),
        dev_overlay: true,
        lifecycle: dev_extension_lifecycle(runner_id, &plan.synced_source_path, &plan.id),
        materialization_source: Some(serde_json::json!({
            "kind": "controller_snapshot",
            "local_path": plan.source_path,
            "content_hash": plan.content_hash,
        })),
    }
}

fn runner_path_provenance(
    runner_id: &str,
    request: &RunnerExtensionMaterializationRequest,
    path: &str,
) -> RunnerExtensionMaterializationProvenance {
    RunnerExtensionMaterializationProvenance {
        id: request.id.clone(),
        source_path: path.to_string(),
        synced_source_path: path.to_string(),
        content_hash: request.revision.clone(),
        source_revision: Some(request.revision.clone()),
        dirty: false,
        dirty_fingerprint: None,
        synced_at: chrono::Utc::now().to_rfc3339(),
        dev_overlay: false,
        lifecycle: installed_extension_lifecycle(
            runner_id,
            path,
            &request.id,
            "linked_runner_path",
        ),
        materialization_source: Some(serde_json::json!({
            "kind": "runner_path",
            "path": path,
        })),
    }
}

fn remote_git_provenance(
    runner_id: &str,
    request: &RunnerExtensionMaterializationRequest,
    url: &str,
    git_ref: &str,
    replaced: bool,
) -> RunnerExtensionMaterializationProvenance {
    RunnerExtensionMaterializationProvenance {
        id: request.id.clone(),
        source_path: url.to_string(),
        synced_source_path: url.to_string(),
        content_hash: request.revision.clone(),
        source_revision: Some(request.revision.clone()),
        dirty: false,
        dirty_fingerprint: None,
        synced_at: chrono::Utc::now().to_rfc3339(),
        dev_overlay: false,
        lifecycle: installed_extension_lifecycle(
            runner_id,
            url,
            &request.id,
            "remote_git_checkout",
        ),
        materialization_source: Some(serde_json::json!({
            "kind": "remote_git",
            "url": url,
            "ref": git_ref,
            "replaced": replaced,
        })),
    }
}

pub(crate) fn dev_extension_path(workspace_root: &str, id: &str, content_hash: &str) -> String {
    format!(
        "{}/_lab_workspaces/dev-extensions/{}/{}/",
        workspace_root.trim_end_matches('/'),
        sanitize_ref(id),
        &content_hash[..16]
    )
}

pub(crate) fn extension_source_content_hash(path: &Path) -> Result<String> {
    let mut entries = Vec::new();
    collect_hash_entries(path, path, &mut entries)?;
    entries.sort();
    let mut hasher = homeboy_engine_primitives::content_hash::sha256_hasher();
    for (relative, path) in entries {
        hasher.update(relative.as_bytes());
        hasher.update([0]);
        let mut file = fs::File::open(&path)
            .map_err(|err| Error::internal_io(err.to_string(), Some(path.display().to_string())))?;
        let mut buffer = [0_u8; 8192];
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|err| Error::internal_json(err.to_string(), None))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn collect_hash_entries(
    root: &Path,
    path: &Path,
    entries: &mut Vec<(String, PathBuf)>,
) -> Result<()> {
    let mut children = fs::read_dir(path)
        .map_err(|err| Error::internal_io(err.to_string(), Some(path.display().to_string())))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|err| Error::internal_io(err.to_string(), Some(path.display().to_string())))?;
    children.sort_by_key(|entry| entry.path());
    for entry in children {
        let path = entry.path();
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if file_name == ".git" || file_name.starts_with("._") {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)
            .map_err(|err| Error::internal_io(err.to_string(), Some(path.display().to_string())))?;
        if metadata.is_dir() {
            collect_hash_entries(root, &path, entries)?;
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            // The hash identifies the snapshot bytes, which never carry
            // generated install metadata.
            if !is_generated_extension_metadata_path(&relative, None) {
                entries.push((relative, path));
            }
        }
    }
    Ok(())
}

/// Strip what is not extension source from a staged snapshot: AppleDouble
/// sidecars and the install-local provenance Homeboy generates. A controller
/// snapshot may come from an *installed* directory, whose `.source-*` files
/// describe the controller install, not the runner's.
fn scrub_extension_snapshot(path: &Path) -> Result<()> {
    remove_appledouble_files(path)?;
    for entry in fs::read_dir(path)
        .map_err(|err| Error::internal_io(err.to_string(), Some(path.display().to_string())))?
    {
        let entry = entry
            .map_err(|err| Error::internal_io(err.to_string(), Some(path.display().to_string())))?;
        let name = entry.file_name();
        if is_generated_extension_metadata_path(&name.to_string_lossy(), None)
            && entry.file_type().is_ok_and(|kind| kind.is_file())
        {
            fs::remove_file(entry.path()).map_err(|err| {
                Error::internal_io(err.to_string(), Some(entry.path().display().to_string()))
            })?;
        }
    }
    Ok(())
}

fn remove_appledouble_files(path: &Path) -> Result<()> {
    let children = fs::read_dir(path)
        .map_err(|err| Error::internal_io(err.to_string(), Some(path.display().to_string())))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|err| Error::internal_io(err.to_string(), Some(path.display().to_string())))?;
    for entry in children {
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|err| Error::internal_io(err.to_string(), Some(path.display().to_string())))?;
        if metadata.is_dir() {
            remove_appledouble_files(&path)?;
        } else if entry.file_name().to_string_lossy().starts_with("._") {
            fs::remove_file(&path).map_err(|err| {
                Error::internal_io(err.to_string(), Some(path.display().to_string()))
            })?;
        }
    }
    Ok(())
}

pub(crate) fn dev_extension_lifecycle(
    runner_id: &str,
    remote_path: &str,
    extension_id: &str,
) -> ResourceLifecycleRecord {
    ResourceLifecycleRecord {
        owner: "runner.dev_sync.extension_overlay".to_string(),
        run_id: format!("runner-dev-sync-{runner_id}-{extension_id}"),
        runner_id: Some(runner_id.to_string()),
        path: remote_path.to_string(),
        root_bound: None,
        kind: "runner_dev_extension_overlay".to_string(),
        ttl: Some("P7D".to_string()),
        cleanup_policy: ResourceCleanupPolicy::DeleteAfterTtl,
        evidence_retention: ResourceEvidenceRetention::Metadata,
        cleanup_intent: Default::default(),
        cleanup_command: Some(format!(
            "homeboy runner dev-sync {} --extensions {}=<path>",
            shell::quote_path(runner_id),
            shell::quote_path(extension_id)
        )),
        status: ResourceLifecycleResourceStatus::Active,
    }
}

fn installed_extension_lifecycle(
    runner_id: &str,
    source: &str,
    extension_id: &str,
    source_kind: &str,
) -> ResourceLifecycleRecord {
    ResourceLifecycleRecord {
        owner: format!("runner.extension.{source_kind}"),
        run_id: format!("runner-extension-{runner_id}-{extension_id}"),
        runner_id: Some(runner_id.to_string()),
        path: source.to_string(),
        root_bound: None,
        kind: "runner_installed_extension".to_string(),
        ttl: None,
        cleanup_policy: ResourceCleanupPolicy::Preserve,
        evidence_retention: ResourceEvidenceRetention::Metadata,
        cleanup_intent: Default::default(),
        cleanup_command: None,
        status: ResourceLifecycleResourceStatus::Retained,
    }
}

pub(crate) fn expand_path(path: &str) -> PathBuf {
    PathBuf::from(shellexpand::tilde(path).to_string())
}

pub(crate) fn git_revision(path: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["-C", &path.display().to_string(), "rev-parse", "HEAD"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn sanitize_ref(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect();
    let sanitized = sanitized.trim_matches('-');
    if sanitized.is_empty() {
        "main".to_string()
    } else {
        sanitized.to_string()
    }
}

fn shell_command(command: &[String]) -> String {
    command
        .iter()
        .map(|arg| posix_quote_arg(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Runner;

    use std::sync::{Arc, Barrier};

    fn runner() -> Runner {
        Runner {
            id: "lab".to_string(),
            kind: RunnerKind::Local,
            server_id: None,
            workspace_root: Some("/tmp/homeboy-runner-ws".to_string()),
            settings: Default::default(),
            env: Default::default(),
            secret_env: Default::default(),
            resources: Default::default(),
            policy: Default::default(),
        }
    }

    fn output() -> super::super::RunnerExecOutput {
        super::super::RunnerExecOutput {
            variant: "runner_exec",
            command: "runner.exec",
            runner_id: "lab".to_string(),
            dry_run: false,
            mode: super::super::RunnerTransport::Local,
            argv: Vec::new(),
            remote_cwd: String::new(),
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            source_snapshot: None,
            job: None,
            runner_job: None,
            job_id: None,
            job_events: None,
            mirror_run_id: None,
            patch: None,
            mutation_artifacts: None,
            artifacts: Vec::new(),
            promoted_outputs: Vec::new(),
            structured_summaries: Vec::new(),
            metrics: None,
            capture: None,
            execution_record: None,
            runner_result: None,
            handoff: None,
            diagnostics: None,
        }
    }

    #[test]
    fn remote_git_materialization_installs_or_replaces_by_runner_state() {
        let runner = runner();
        let request = RunnerExtensionMaterializationRequest {
            id: "rust".to_string(),
            revision: "abc123".to_string(),
            source: RunnerExtensionMaterializationSource::RemoteGit {
                url: "https://example.test/extensions.git".to_string(),
                git_ref: "abc123".to_string(),
            },
        };
        let mut commands = Vec::new();
        let provenance = materialize_runner_extension_with_exec(
            &runner,
            "homeboy",
            None,
            &request,
            &mut |_runner_id, options| {
                commands.push(options.command.clone());
                Ok((output(), 0))
            },
        )
        .expect("materializes");

        assert_eq!(commands[0], vec!["homeboy", "extension", "show", "rust"]);
        assert_eq!(
            commands[1][..4],
            [
                "homeboy",
                "extension",
                "install",
                "https://example.test/extensions.git"
            ]
        );
        assert!(commands[1].contains(&"--replace".to_string()));
        assert_eq!(
            provenance.materialization_source.as_ref().unwrap()["kind"],
            "remote_git"
        );
        assert_eq!(
            provenance.lifecycle.owner,
            "runner.extension.remote_git_checkout"
        );
        assert_eq!(
            provenance.lifecycle.cleanup_policy,
            ResourceCleanupPolicy::Preserve
        );
    }

    #[test]
    fn runner_path_materialization_links_active_path() {
        let runner = runner();
        let request = RunnerExtensionMaterializationRequest {
            id: "rust".to_string(),
            revision: "abc123".to_string(),
            source: RunnerExtensionMaterializationSource::RunnerPath {
                path: "/runner/extensions/rust".to_string(),
            },
        };
        let mut commands = Vec::new();
        materialize_runner_extension_with_exec(
            &runner,
            "homeboy",
            None,
            &request,
            &mut |_runner_id, options| {
                commands.push(options.command);
                Ok((output(), 0))
            },
        )
        .expect("materializes");

        assert_eq!(commands[0], vec!["homeboy", "extension", "show", "rust"]);
        assert_eq!(
            commands[1],
            vec![
                "homeboy",
                "extension",
                "install",
                "/runner/extensions/rust",
                "--id",
                "rust",
                "--ref",
                "abc123",
                "--replace"
            ]
        );
        let provenance = runner_path_provenance(&runner.id, &request, "/runner/extensions/rust");
        assert_eq!(
            provenance.lifecycle.owner,
            "runner.extension.linked_runner_path"
        );
        assert_eq!(
            provenance.lifecycle.status,
            ResourceLifecycleResourceStatus::Retained
        );
    }

    #[test]
    fn controller_snapshot_materialization_links_synced_dev_overlay() {
        let workspace = tempfile::tempdir().expect("runner workspace");
        let mut runner = runner();
        runner.workspace_root = Some(workspace.path().to_string_lossy().to_string());
        let dir = tempfile::tempdir().expect("extension source");
        std::fs::write(dir.path().join("nodejs.json"), r#"{"id":"nodejs"}"#).expect("manifest");
        std::fs::write(dir.path().join("bench-runner.sh"), "echo bench\n").expect("script");
        let request = RunnerExtensionMaterializationRequest {
            id: "nodejs".to_string(),
            revision: "24ebb9fdaa53c3387669ec3222a5cb7fe26e8884".to_string(),
            source: RunnerExtensionMaterializationSource::ControllerSnapshot {
                local_path: dir.path().to_path_buf(),
            },
        };
        let mut commands = Vec::new();

        let provenance = materialize_runner_extension_with_exec(
            &runner,
            "homeboy",
            None,
            &request,
            &mut |_runner_id, options| {
                commands.push(options.command);
                Ok((output(), 0))
            },
        )
        .expect("materializes");

        assert_eq!(commands[0], vec!["homeboy", "extension", "show", "nodejs"]);
        assert_eq!(commands[1][..3], ["homeboy", "extension", "install"]);
        assert_eq!(commands[1][4], "--id");
        assert_eq!(commands[1][5], "nodejs");
        assert_eq!(commands[1][6], "--ref");
        assert_eq!(commands[1][8], "--replace");
        assert!(commands[1][3].starts_with(&format!(
            "{}/_lab_workspaces/dev-extensions/nodejs/",
            workspace.path().display()
        )));
        assert!(provenance.dev_overlay);
        assert!(provenance.synced_source_path.starts_with(&format!(
            "{}/_lab_workspaces/dev-extensions/nodejs/",
            workspace.path().display()
        )));
    }

    #[test]
    fn controller_snapshot_excludes_non_source_metadata_from_hash_and_sync() {
        let workspace = tempfile::tempdir().expect("runner workspace");
        let mut runner = runner();
        runner.workspace_root = Some(workspace.path().to_string_lossy().to_string());
        let source = tempfile::tempdir().expect("installed extension");
        let rules = source
            .path()
            .join("node_modules/eslint-plugin-jest/lib/rules");
        fs::create_dir_all(&rules).expect("rules directory");
        fs::write(source.path().join("nodejs.json"), r#"{"id":"nodejs"}"#).expect("manifest");
        fs::write(rules.join("consistent-test-it.js"), "module.exports = {};")
            .expect("JavaScript rule");
        fs::create_dir_all(source.path().join("fixtures")).expect("fixtures");
        fs::write(source.path().join("fixtures/.source-url"), "fixture").expect("nested source");
        let hash_without_metadata =
            extension_source_content_hash(source.path()).expect("source hash");
        // AppleDouble sidecars anywhere, and install-local provenance at the root.
        fs::write(rules.join("._consistent-test-it.js"), b"\0\x05\x16\x07ATTR")
            .expect("AppleDouble metadata");
        for generated in [".source-url", ".source-revision", ".source-requested-ref"] {
            fs::write(source.path().join(generated), "generated").expect("generated metadata");
        }

        assert_eq!(
            extension_source_content_hash(source.path()).expect("source hash"),
            hash_without_metadata
        );

        let request = RunnerExtensionMaterializationRequest {
            id: "nodejs".to_string(),
            revision: "abc123".to_string(),
            source: RunnerExtensionMaterializationSource::ControllerSnapshot {
                local_path: source.path().to_path_buf(),
            },
        };
        let provenance = materialize_runner_extension_with_exec(
            &runner,
            "homeboy",
            None,
            &request,
            &mut |_runner_id, _options| Ok((output(), 0)),
        )
        .expect("materializes");
        let synced = Path::new(&provenance.synced_source_path);
        let synced_rules = synced.join("node_modules/eslint-plugin-jest/lib/rules");

        assert!(synced.join("nodejs.json").exists());
        assert!(synced.join("fixtures/.source-url").exists());
        assert!(synced_rules.join("consistent-test-it.js").exists());
        assert!(!synced_rules.join("._consistent-test-it.js").exists());
        for generated in [".source-url", ".source-revision", ".source-requested-ref"] {
            assert!(!synced.join(generated).exists(), "{generated} was synced");
        }
    }

    #[test]
    fn runner_extraction_removes_appledouble_metadata() {
        let fixture = tempfile::tempdir().expect("extension fixture");
        let source = fixture.path().join("source");
        let destination = fixture.path().join("destination");
        let archive = fixture.path().join("source.tar");
        fs::create_dir_all(&source).expect("source directory");
        fs::write(source.join("rule.js"), "module.exports = {};").expect("JavaScript rule");
        fs::write(source.join("._rule.js"), b"\0\x05\x16\x07ATTR").expect("AppleDouble metadata");
        let archive_status = Command::new("tar")
            .args([
                "-C",
                &source.display().to_string(),
                "-cf",
                &archive.display().to_string(),
                ".",
            ])
            .status()
            .expect("archive fixture");
        assert!(archive_status.success());

        let extract_status = Command::new("sh")
            .args([
                "-c",
                &extension_snapshot_extract_command(
                    &destination.display().to_string(),
                    &archive.display().to_string(),
                ),
            ])
            .status()
            .expect("extract fixture");

        assert!(extract_status.success());
        assert!(destination.join("rule.js").exists());
        assert!(!destination.join("._rule.js").exists());
    }

    #[test]
    fn materialization_can_use_explicit_command_env() {
        let runner = runner();
        let request = RunnerExtensionMaterializationRequest {
            id: "rust".to_string(),
            revision: "abc123".to_string(),
            source: RunnerExtensionMaterializationSource::RunnerPath {
                path: "/runner/extensions/rust".to_string(),
            },
        };
        let mut env = std::collections::HashMap::new();
        env.insert("PATH".to_string(), "/usr/local/bin:/usr/bin".to_string());
        let mut captured_env = std::collections::HashMap::new();

        materialize_runner_extension_with_exec(
            &runner,
            "homeboy",
            Some(env),
            &request,
            &mut |_runner_id, options| {
                captured_env = options.env;
                Ok((output(), 0))
            },
        )
        .expect("materializes");

        assert_eq!(
            captured_env.get("PATH").map(String::as_str),
            Some("/usr/local/bin:/usr/bin")
        );
        assert_eq!(captured_env.get("HOMEBOY_COMMAND"), None);
    }

    #[test]
    fn linked_lab_overlay_stages_root_assets_for_flat_and_nested_extension_roots() {
        check_linked_overlay_root(false);
        check_linked_overlay_root(true);
    }

    fn check_linked_overlay_root(nested: bool) {
        crate::test_support::with_isolated_home(|_home| {
            let repo = tempfile::tempdir().expect("extension repository");
            let source_root = if nested {
                let path = repo.path().join("extensions");
                fs::create_dir_all(&path).unwrap();
                path
            } else {
                repo.path().to_path_buf()
            };
            let source_root = source_root.as_path();
            let nodejs = source_root.join("nodejs");
            let scripts_lib = source_root.join("scripts/lib");
            fs::create_dir_all(&nodejs).expect("nodejs extension");
            fs::create_dir_all(&scripts_lib).expect("shared scripts library");
            fs::create_dir_all(source_root.join("dependency-adapters"))
                .expect("dependency adapters");
            fs::create_dir_all(source_root.join("shared-tools")).expect("shared tools");
            fs::write(
                source_root.join("homeboy-extension-root.json"),
                r#"{"shared_assets":["scripts/lib",{"path":"dependency-adapters"},"shared-tools"]}"#,
            )
            .expect("root manifest");
            fs::write(
                nodejs.join("nodejs.json"),
                r#"{"id":"nodejs","name":"Node.js","version":"1.0.0"}"#,
            )
            .expect("nodejs manifest");
            fs::write(
                nodejs.join("run-tests.sh"),
                "#!/bin/sh\nset -eu\nexec sh \"$HOME/.config/homeboy/extensions/scripts/lib/nodejs-test-runner.sh\" \"$1\"\n",
            )
            .expect("nodejs runner");
            fs::write(
                scripts_lib.join("nodejs-test-runner.sh"),
                "#!/bin/sh\nset -eu\nexec node --test \"$1\"\n",
            )
            .expect("shared node runner");
            homeboy_core::extension::lifecycle::install(
                &nodejs.display().to_string(),
                Some("nodejs"),
            )
            .expect("install linked nodejs extension");

            let runner_root = tempfile::tempdir().expect("runner root");
            crate::create(
                &format!(
                    r#"{{"id":"lab-nodejs-overlay","kind":"local","workspace_root":"{}"}}"#,
                    runner_root.path().display()
                ),
                false,
            )
            .expect("create runner");
            let runner = crate::load("lab-nodejs-overlay").expect("load runner");
            let runtime_root = runner_root.path().join("extension-runtime");
            let overlays = materialize_lab_job_extension_overlay_snapshots(
                &runner,
                &runtime_root.display().to_string(),
                &["nodejs".to_string()],
            )
            .expect("stage isolated overlay snapshots");
            let status = Command::new("sh")
                .arg("-c")
                .arg(lab_job_extension_overlay_command(
                    &runtime_root.display().to_string(),
                    &overlays,
                ))
                .status()
                .expect("link isolated overlay");
            assert!(status.success(), "link isolated overlay");

            assert_eq!(overlays[0].shared_assets.len(), 3);
            assert!(runtime_root
                .join("home/.config/homeboy/extensions/scripts/lib/nodejs-test-runner.sh")
                .exists());
            assert!(runtime_root
                .join("home/.config/homeboy/extensions/dependency-adapters")
                .exists());
            assert!(runtime_root
                .join("home/.config/homeboy/extensions/shared-tools")
                .exists());

            let project = tempfile::tempdir().expect("project");
            let test_file = project.path().join("project.test.js");
            fs::write(
                &test_file,
                "import test from 'node:test';\nimport assert from 'node:assert/strict';\ntest('project test', () => assert.equal(2 + 2, 4));\n",
            )
            .expect("project test");
            let output = Command::new("sh")
                .arg(runtime_root.join("home/.config/homeboy/extensions/nodejs/run-tests.sh"))
                .arg(&test_file)
                .env("HOME", runtime_root.join("home"))
                .output()
                .expect("run isolated Node.js test runner");
            assert!(
                output.status.success(),
                "Node.js test runner did not reach project tests: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        });
    }

    #[test]
    fn isolated_lab_overlay_stages_shared_assets_from_cloned_install_metadata() {
        crate::test_support::with_isolated_home(|home| {
            let extensions_root = home.path().join(".config/homeboy/extensions");
            let wordpress = extensions_root.join("wordpress");
            let scripts_lib = extensions_root.join("scripts/lib");
            fs::create_dir_all(&wordpress).expect("installed WordPress extension");
            fs::create_dir_all(&scripts_lib).expect("installed shared scripts");
            fs::write(
                wordpress.join("wordpress.json"),
                r#"{"id":"wordpress","name":"WordPress","version":"1.0.0"}"#,
            )
            .expect("WordPress manifest");
            fs::write(
                wordpress.join(".homeboy-extension-root.json"),
                r#"{"shared_assets":["scripts/lib"]}"#,
            )
            .expect("persisted root manifest");
            fs::write(
                scripts_lib.join("settings.sh"),
                "homeboy_setting() { :; }\n",
            )
            .expect("shared helper");

            let runner_root = tempfile::tempdir().expect("runner root");
            crate::create(
                &format!(
                    r#"{{"id":"lab-cloned-overlay","kind":"local","workspace_root":"{}"}}"#,
                    runner_root.path().display()
                ),
                false,
            )
            .expect("create runner");
            let runner = crate::load("lab-cloned-overlay").expect("load runner");
            let runtime_root = runner_root.path().join("extension-runtime");
            let overlays = materialize_lab_job_extension_overlay_snapshots(
                &runner,
                &runtime_root.display().to_string(),
                &["wordpress".to_string()],
            )
            .expect("stage cloned install overlay snapshots");
            let status = Command::new("sh")
                .arg("-c")
                .arg(lab_job_extension_overlay_command(
                    &runtime_root.display().to_string(),
                    &overlays,
                ))
                .status()
                .expect("link cloned install overlay");
            assert!(status.success(), "link cloned install overlay");

            assert_eq!(overlays[0].shared_assets.len(), 1);
            assert!(runtime_root
                .join("home/.config/homeboy/extensions/scripts/lib/settings.sh")
                .exists());
        });
    }

    #[test]
    fn concurrent_job_snapshots_keep_distinct_extension_revisions_after_global_source_changes() {
        crate::test_support::with_isolated_home(|home| {
            let runner_root = tempfile::tempdir().expect("runner root");
            crate::create(
                &format!(
                    r#"{{"id":"lab-extension-isolation","kind":"local","workspace_root":"{}"}}"#,
                    runner_root.path().display()
                ),
                false,
            )
            .expect("create runner");
            let source = home.path().join(".config/homeboy/extensions/fixture");
            fs::create_dir_all(&source).expect("extension source");
            fs::write(
                source.join("fixture.json"),
                r#"{"id":"fixture","name":"Fixture","version":"1.0.0"}"#,
            )
            .expect("extension manifest");
            fs::write(source.join("catalog"), "revision-a\n").expect("first catalog");

            let runner = crate::load("lab-extension-isolation").expect("runner");
            let first_root = runner_root
                .path()
                .join("_lab_workspaces/job-a-homeboy-artifacts/runtime");
            let first = materialize_lab_job_extension_overlay_snapshots(
                &runner,
                &first_root.display().to_string(),
                &["fixture".to_string()],
            )
            .expect("stage first snapshot");
            assert!(Command::new("sh")
                .arg("-c")
                .arg(lab_job_extension_overlay_command(
                    &first_root.display().to_string(),
                    &first,
                ))
                .status()
                .expect("link first snapshot")
                .success());

            // The controller's administrative default changes while A remains
            // admitted. B must get the new source without rewriting A's link.
            fs::write(source.join("catalog"), "revision-b\n").expect("second catalog");
            let second_root = runner_root
                .path()
                .join("_lab_workspaces/job-b-homeboy-artifacts/runtime");
            let second = materialize_lab_job_extension_overlay_snapshots(
                &runner,
                &second_root.display().to_string(),
                &["fixture".to_string()],
            )
            .expect("stage second snapshot");
            assert!(Command::new("sh")
                .arg("-c")
                .arg(lab_job_extension_overlay_command(
                    &second_root.display().to_string(),
                    &second,
                ))
                .status()
                .expect("link second snapshot")
                .success());

            assert_ne!(first[0].content_hash, second[0].content_hash);
            assert_eq!(
                fs::read_to_string(source.join("catalog")).expect("global source"),
                "revision-b\n"
            );
            let barrier = Arc::new(Barrier::new(2));
            let jobs = [
                (first_root.join("home"), "revision-a\n"),
                (second_root.join("home"), "revision-b\n"),
            ];
            let handles = jobs
                .into_iter()
                .map(|(home, expected)| {
                    let barrier = Arc::clone(&barrier);
                    std::thread::spawn(move || {
                        barrier.wait();
                        let output = Command::new("sh")
                            .arg("-c")
                            .arg("cat \"$HOME/.config/homeboy/extensions/fixture/catalog\"")
                            .env("HOME", home)
                            .output()
                            .expect("read private extension catalog");
                        assert!(output.status.success());
                        assert_eq!(
                            String::from_utf8(output.stdout).expect("catalog utf8"),
                            expected
                        );
                    })
                })
                .collect::<Vec<_>>();
            for handle in handles {
                handle.join().expect("extension job");
            }
        });
    }

    #[test]
    fn lab_job_overlays_link_one_hydrated_cache_entry_with_setup_dependencies() {
        crate::test_support::with_isolated_home(|home| {
            let runner_root = tempfile::tempdir().expect("runner root");
            crate::create(
                &format!(
                    r#"{{"id":"lab-extension-hydrate","kind":"local","workspace_root":"{}"}}"#,
                    runner_root.path().display()
                ),
                false,
            )
            .expect("create runner");
            let counter = runner_root.path().join("setup-count");
            let source = home.path().join(".config/homeboy/extensions/fixture");
            fs::create_dir_all(&source).expect("extension source");
            fs::write(
                source.join("fixture.json"),
                format!(
                    r#"{{"id":"fixture","name":"Fixture","version":"1.0.0","executable":{{"runtime":{{"setup_command":"test -f {{{{extension_path}}}}/../fixture-assets/asset.txt && mkdir -p {{{{extension_path}}}}/vendor && touch {{{{extension_path}}}}/vendor/autoload.php && printf x >> {}"}}}}}}"#,
                    counter.display()
                ),
            )
            .expect("extension manifest");
            let extensions_root = source.parent().unwrap();
            fs::write(
                extensions_root.join("homeboy-extension-root.json"),
                r#"{"shared_assets":["fixture-assets"]}"#,
            )
            .expect("root manifest");
            fs::create_dir_all(extensions_root.join("fixture-assets")).expect("shared asset");
            fs::write(extensions_root.join("fixture-assets/asset.txt"), "asset\n").expect("asset");
            let runner = crate::load("lab-extension-hydrate").expect("runner");

            let mut remote_paths = Vec::new();
            for job in ["job-a", "job-b"] {
                let runtime_root = runner_root
                    .path()
                    .join(format!("_lab_workspaces/{job}-homeboy-artifacts/runtime"));
                let overlays = materialize_lab_job_extension_overlay_snapshots(
                    &runner,
                    &runtime_root.display().to_string(),
                    &["fixture".to_string()],
                )
                .expect("stage overlay");
                assert!(Command::new("sh")
                    .arg("-c")
                    .arg(lab_job_extension_overlay_command(
                        &runtime_root.display().to_string(),
                        &overlays,
                    ))
                    .status()
                    .expect("link overlay")
                    .success());
                assert!(runtime_root
                    .join("home/.config/homeboy/extensions/fixture/vendor/autoload.php")
                    .is_file());
                remote_paths.push(overlays[0].remote_path.clone());
            }
            assert_eq!(remote_paths[0], remote_paths[1]);
            assert!(remote_paths[0].contains("/_lab_workspaces/extension-cache/fixture/"));
            assert_eq!(
                fs::read_to_string(&counter).unwrap(),
                "x",
                "setup runs once per revision"
            );
        });
    }
}
