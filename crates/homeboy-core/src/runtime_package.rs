use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};

use crate::agent_runtime_manifest::AgentRuntimeManifest;
use crate::config;
use crate::engine::identifier;
use crate::engine::local_files;
use crate::error::{Error, Result};
use crate::io::{copy_tree, EntryPolicy};
use crate::{git, paths};

const ROOT_MANIFEST: &str = "homeboy-extension-root.json";
const GENERATIONS: &str = "runtime-generations";
const CURRENT: &str = "current";
const JOURNAL: &str = "refresh.json";
const JOURNAL_SCHEMA: &str = "homeboy/runtime-generation-refresh/v1";
const BOUNDARY_MIGRATION: &str = ".agent-runtimes-migration.json";
const BOUNDARY_MIGRATION_SCHEMA: &str = "homeboy/runtime-boundary-migration/v1";

#[cfg(test)]
static CRASH_AFTER_BOUNDARY_BOOTSTRAP: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static CRASH_AFTER_BOUNDARY_BACKUP_RENAME: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone)]
pub struct RuntimePackageRefreshResult {
    pub runtime_id: String,
    pub source: String,
    pub path: PathBuf,
    pub manifest_path: PathBuf,
    pub source_revision: Option<String>,
    pub replaced_existing: bool,
}

#[derive(Debug, Deserialize)]
struct RootManifest {
    #[serde(default)]
    shared_assets: Vec<SharedAssetDeclaration>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum SharedAssetDeclaration {
    Path(String),
    Object { path: String },
}

impl SharedAssetDeclaration {
    fn path(self) -> String {
        match self {
            Self::Path(path) | Self::Object { path } => path,
        }
    }
}

#[derive(Debug, Serialize)]
struct Provenance<'a> {
    schema: &'static str,
    source: &'a str,
    source_revision: Option<&'a str>,
    shared_assets: &'a [String],
}

#[derive(Debug, Serialize, Deserialize)]
struct RefreshJournal {
    schema: String,
    generation: String,
    #[serde(default)]
    switched: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct BoundaryMigration {
    schema: String,
    backup: String,
    temporary: String,
}

pub fn refresh(
    runtime_id: &str,
    source: &str,
    revision: Option<&str>,
) -> Result<RuntimePackageRefreshResult> {
    config::with_config_lock(|| refresh_locked(runtime_id, source, revision))
}

fn refresh_locked(
    runtime_id: &str,
    source: &str,
    revision: Option<&str>,
) -> Result<RuntimePackageRefreshResult> {
    let runtime_id = identifier::slugify_id(runtime_id, "runtime_id")?;
    let config_root = paths::homeboy()?;
    let store = config_root.join(GENERATIONS);
    fs::create_dir_all(store.join("staging")).map_err(io("prepare runtime generation store"))?;
    recover(&store)?;
    bootstrap_stable_runtime_boundary(&config_root, &store)?;
    crash_after_boundary_bootstrap()?;

    let source_stage = store.join(format!("source-{}-{}", runtime_id, nonce()));
    remove_if_exists(&source_stage, "clean runtime refresh source")?;
    let (source_root, source_revision) = if crate::extension_update_check::is_git_url(source) {
        git::clone_repo_at_ref(source, &source_stage, revision)?;
        (
            source_stage.as_path(),
            git::short_head_revision(&source_stage),
        )
    } else {
        if revision.is_some() {
            return Err(Error::validation_invalid_argument(
                "ref",
                "--ref is only supported for git URL runtime package sources",
                revision.map(str::to_string),
                None,
            ));
        }
        (
            Path::new(source),
            git::short_head_revision(Path::new(source)),
        )
    };

    let package_source = resolve_package(source_root, &runtime_id)?;
    let manifest_root = manifest_root(source_root, &package_source);
    reject_symlink_tree(&package_source)?;
    validate_package(&package_source, &runtime_id)?;

    let generation_name = format!("{}-{}", runtime_id, nonce());
    let stage = store.join("staging").join(&generation_name);
    remove_if_exists(&stage, "clean runtime generation stage")?;
    seed_generation(&config_root, &store, &stage)?;
    let dependencies = materialize_declared_assets(manifest_root, &stage, &runtime_id)?;

    let staged_package = stage.join("agent-runtimes").join(&runtime_id);
    remove_if_exists(&staged_package, "replace staged runtime package")?;
    copy_regular_tree(&package_source, &staged_package, "stage runtime package")?;
    write_metadata(
        &staged_package,
        source,
        source_revision.as_deref(),
        &dependencies,
    )?;
    validate_package(&staged_package, &runtime_id)?;
    validate_local_module_closure(&stage, &staged_package)?;
    sync_tree(&stage)?;

    let replaced_existing = paths::agent_runtimes()?.join(&runtime_id).exists();
    write_journal(
        &store,
        &RefreshJournal {
            schema: JOURNAL_SCHEMA.to_string(),
            generation: generation_name.clone(),
            switched: false,
        },
    )?;
    let generation = store.join(&generation_name);
    fs::rename(&stage, &generation).map_err(io("publish runtime generation"))?;
    sync_dir(&store)?;
    switch_current(&store, &generation_name)?;
    write_journal(
        &store,
        &RefreshJournal {
            schema: JOURNAL_SCHEMA.to_string(),
            generation: generation_name,
            switched: true,
        },
    )?;
    remove_if_exists(&store.join(JOURNAL), "finalize runtime generation refresh")?;
    sync_dir(&store)?;
    remove_if_exists(&source_stage, "clean runtime refresh source")?;

    let path = paths::agent_runtimes()?.join(&runtime_id);
    Ok(RuntimePackageRefreshResult {
        runtime_id: runtime_id.clone(),
        source: source.to_string(),
        manifest_path: path.join(format!("{runtime_id}.json")),
        path,
        source_revision,
        replaced_existing,
    })
}

/// Snapshot root-manifest runtime assets into a new generation. Linked extension
/// installs call this instead of writing through the stable runtime boundary.
pub fn refresh_shared_assets(source_root: &Path) -> Result<()> {
    config::with_config_lock(|| refresh_shared_assets_locked(source_root))
}

fn refresh_shared_assets_locked(source_root: &Path) -> Result<()> {
    let config_root = paths::homeboy()?;
    let store = config_root.join(GENERATIONS);
    fs::create_dir_all(store.join("staging")).map_err(io("prepare runtime generation store"))?;
    recover(&store)?;
    bootstrap_stable_runtime_boundary(&config_root, &store)?;
    crash_after_boundary_bootstrap()?;
    let source_root =
        canonical_contained_directory(source_root, source_root, "resolve linked runtime source")?;
    let generation_name = format!("extension-assets-{}", nonce());
    let stage = store.join("staging").join(&generation_name);
    remove_if_exists(&stage, "clean linked runtime generation stage")?;
    seed_generation(&config_root, &store, &stage)?;
    materialize_all_declared_runtime_assets(&source_root, &stage)?;
    sync_tree(&stage)?;
    write_journal(
        &store,
        &RefreshJournal {
            schema: JOURNAL_SCHEMA.to_string(),
            generation: generation_name.clone(),
            switched: false,
        },
    )?;
    fs::rename(&stage, store.join(&generation_name))
        .map_err(io("publish linked runtime generation"))?;
    sync_dir(&store)?;
    switch_current(&store, &generation_name)?;
    write_journal(
        &store,
        &RefreshJournal {
            schema: JOURNAL_SCHEMA.to_string(),
            generation: generation_name,
            switched: true,
        },
    )?;
    remove_if_exists(
        &store.join(JOURNAL),
        "finalize linked runtime generation refresh",
    )?;
    sync_dir(&store)
}

fn seed_generation(config_root: &Path, store: &Path, stage: &Path) -> Result<()> {
    if let Some(current) = active_current_generation(store)? {
        reject_symlink_tree(&current)?;
        return copy_regular_tree(&current, stage, "copy active runtime generation");
    }
    seed_legacy_generation(config_root, stage)
}

fn seed_legacy_generation(config_root: &Path, stage: &Path) -> Result<()> {
    // First activation migrates the runtime surface only; all unrelated Homeboy
    // configuration remains in place and is never renamed or copied.
    let legacy = paths::legacy_agent_runtimes()?;
    if legacy.is_dir() {
        reject_symlink_tree_except_root(&legacy)?;
        copy_regular_tree(
            &legacy,
            &stage.join("agent-runtimes"),
            "migrate legacy runtimes",
        )?;
    }
    for shared in ["agent-task-contracts", "runtime-agent-ci"] {
        let source = config_root.join(shared);
        if source.is_dir() {
            reject_symlink_tree_except_root(&source)?;
            copy_regular_tree(
                &source,
                &stage.join(shared),
                "migrate legacy runtime dependency",
            )?;
        }
    }
    fs::create_dir_all(stage.join("agent-runtimes"))
        .map_err(io("prepare legacy runtime generation"))?;
    Ok(())
}

/// Establish the documented runtime boundary before a replacement generation is
/// staged. Both direct and generation consumers therefore see the same seed
/// generation if the process stops before the later current-pointer switch.
fn bootstrap_stable_runtime_boundary(config_root: &Path, store: &Path) -> Result<()> {
    if active_current_generation(store)?.is_none() {
        let generation_name = format!("bootstrap-{}", nonce());
        let stage = store.join("staging").join(&generation_name);
        remove_if_exists(&stage, "clean bootstrap runtime generation stage")?;
        seed_legacy_generation(config_root, &stage)?;
        sync_tree(&stage)?;
        fs::rename(&stage, store.join(&generation_name))
            .map_err(io("publish bootstrap runtime generation"))?;
        sync_dir(store)?;
        switch_current(store, &generation_name)?;
    }
    ensure_stable_runtime_boundary(config_root)
}

fn crash_after_boundary_bootstrap() -> Result<()> {
    #[cfg(test)]
    if CRASH_AFTER_BOUNDARY_BOOTSTRAP.swap(false, Ordering::SeqCst) {
        return Err(Error::internal_io(
            "injected crash after stable runtime boundary bootstrap",
            Some("test runtime generation crash".to_string()),
        ));
    }
    Ok(())
}

fn crash_after_boundary_backup_rename() -> Result<()> {
    #[cfg(test)]
    if CRASH_AFTER_BOUNDARY_BACKUP_RENAME.swap(false, Ordering::SeqCst) {
        return Err(Error::internal_io(
            "injected crash after legacy runtime boundary backup rename",
            Some("test runtime boundary migration crash".to_string()),
        ));
    }
    Ok(())
}

fn active_current_generation(store: &Path) -> Result<Option<PathBuf>> {
    let current = store.join(CURRENT);
    let Ok(target) = fs::read_link(&current) else {
        return Ok(None);
    };
    let target = target.to_str().ok_or_else(|| {
        Error::validation_invalid_argument(
            "runtime_generation",
            "current generation target is not valid UTF-8",
            None,
            None,
        )
    })?;
    safe_relative(target, "runtime_generation")?;
    let generation = canonical_contained_directory(
        &store.join(target),
        store,
        "resolve current runtime generation",
    )?;
    if !generation.join("agent-runtimes").is_dir() {
        return Err(Error::validation_invalid_argument(
            "runtime_generation",
            "current generation has no runtime package directory",
            Some(generation.display().to_string()),
            None,
        ));
    }
    Ok(Some(generation))
}

fn canonical_contained_directory(path: &Path, root: &Path, context: &str) -> Result<PathBuf> {
    let root = fs::canonicalize(root).map_err(io(context))?;
    let path = fs::canonicalize(path).map_err(io(context))?;
    if !path.is_dir() || !path.starts_with(&root) {
        return Err(Error::validation_invalid_argument(
            "runtime_generation",
            "runtime generation escapes its store",
            Some(path.display().to_string()),
            None,
        ));
    }
    Ok(path)
}

fn manifest_root<'a>(source_root: &'a Path, package: &'a Path) -> &'a Path {
    if source_root.join(ROOT_MANIFEST).is_file() {
        source_root
    } else if package.parent().is_some_and(|parent| {
        parent
            .file_name()
            .is_some_and(|name| name == "agent-runtimes")
    }) {
        package
            .parent()
            .and_then(Path::parent)
            .unwrap_or(source_root)
    } else {
        source_root
    }
}

fn materialize_declared_assets(root: &Path, stage: &Path, runtime_id: &str) -> Result<Vec<String>> {
    let manifest = root.join(ROOT_MANIFEST);
    let Ok(raw) = fs::read_to_string(&manifest) else {
        return Ok(Vec::new());
    };
    let parsed: RootManifest = serde_json::from_str(&raw).map_err(|error| {
        Error::validation_invalid_argument(
            "source",
            format!("invalid shared asset manifest: {error}"),
            Some(manifest.display().to_string()),
            None,
        )
    })?;
    let mut assets = parsed
        .shared_assets
        .into_iter()
        .map(SharedAssetDeclaration::path)
        .collect::<Vec<_>>();
    assets.sort();
    assets.dedup();
    let root = fs::canonicalize(root).map_err(io("resolve runtime source root"))?;
    for asset in &assets {
        let relative = safe_relative(asset, "shared_assets")?;
        let source = fs::canonicalize(root.join(relative))
            .map_err(io("resolve shared runtime dependency"))?;
        if !source.starts_with(&root) || !source.is_dir() {
            return Err(Error::validation_invalid_argument(
                "shared_assets",
                "declared shared asset escapes or is missing from its source root",
                Some(asset.clone()),
                None,
            ));
        }
        reject_symlink_tree(&source)?;
        if asset == "agent-runtimes" {
            materialize_runtime_shared_tree(&source, &stage.join("agent-runtimes"), runtime_id)?;
        } else {
            let target = stage.join(relative);
            remove_if_exists(&target, "replace staged runtime dependency")?;
            copy_regular_tree(&source, &target, "stage shared runtime dependency")?;
        }
    }
    Ok(assets)
}

fn materialize_all_declared_runtime_assets(root: &Path, stage: &Path) -> Result<()> {
    let manifest = root.join(ROOT_MANIFEST);
    let raw = fs::read_to_string(&manifest).map_err(io("read linked runtime asset manifest"))?;
    let parsed: RootManifest = serde_json::from_str(&raw).map_err(|error| {
        Error::validation_invalid_argument(
            "source",
            format!("invalid shared asset manifest: {error}"),
            Some(manifest.display().to_string()),
            None,
        )
    })?;
    let root = fs::canonicalize(root).map_err(io("resolve linked runtime source"))?;
    for asset in parsed
        .shared_assets
        .into_iter()
        .map(SharedAssetDeclaration::path)
    {
        if !matches!(
            asset.as_str(),
            "agent-runtimes" | "agent-task-contracts" | "runtime-agent-ci"
        ) {
            continue;
        }
        let relative = safe_relative(&asset, "shared_assets")?;
        let source =
            fs::canonicalize(root.join(relative)).map_err(io("resolve linked runtime asset"))?;
        if !source.is_dir() || !source.starts_with(&root) {
            return Err(Error::validation_invalid_argument(
                "shared_assets",
                "linked runtime asset escapes its source root",
                Some(asset),
                None,
            ));
        }
        reject_symlink_tree(&source)?;
        let target = stage.join(relative);
        remove_if_exists(&target, "replace linked runtime asset")?;
        copy_regular_tree(&source, &target, "stage linked runtime asset")?;
    }
    Ok(())
}

fn materialize_runtime_shared_tree(source: &Path, target: &Path, runtime_id: &str) -> Result<()> {
    fs::create_dir_all(target).map_err(io("prepare shared runtime dependency"))?;
    for entry in fs::read_dir(source).map_err(io("read shared runtime dependency"))? {
        let entry = entry.map_err(io("read shared runtime dependency"))?;
        let name = entry.file_name();
        let child = entry.path();
        if name == runtime_id
            || child
                .join(format!("{}.json", name.to_string_lossy()))
                .is_file()
        {
            continue;
        }
        let target = target.join(name);
        remove_if_exists(&target, "replace shared runtime dependency")?;
        copy_regular_tree(&child, &target, "stage shared runtime dependency")?;
    }
    Ok(())
}

fn resolve_package(source_root: &Path, runtime_id: &str) -> Result<PathBuf> {
    if source_root.join(format!("{runtime_id}.json")).is_file() {
        return Ok(source_root.to_path_buf());
    }
    let package = source_root.join("agent-runtimes").join(runtime_id);
    if package.join(format!("{runtime_id}.json")).is_file() {
        return Ok(package);
    }
    Err(Error::validation_invalid_argument("source", format!("No runtime package manifest '{runtime_id}.json' found at source root or agent-runtimes/{runtime_id}"), Some(source_root.display().to_string()), None))
}

fn validate_package(package: &Path, runtime_id: &str) -> Result<()> {
    let manifest: AgentRuntimeManifest =
        config::from_str(&local_files::local().read(&package.join(format!("{runtime_id}.json")))?)?;
    if manifest.id != runtime_id {
        return Err(Error::validation_invalid_argument(
            "runtime_id",
            format!(
                "Runtime package manifest id '{}' does not match requested id '{runtime_id}'",
                manifest.id
            ),
            Some(runtime_id.to_string()),
            None,
        ));
    }
    Ok(())
}

fn validate_local_module_closure(generation: &Path, package: &Path) -> Result<()> {
    let mut pending = BTreeSet::new();
    collect_javascript(package, &mut pending)?;
    let generation =
        fs::canonicalize(generation).map_err(io("resolve staged runtime generation"))?;
    while let Some(module) = pending.pop_first() {
        for dependency in local_requires(&module)? {
            let resolved = resolve_local_module(&module, &dependency).ok_or_else(|| {
                Error::validation_invalid_argument(
                    "runtime_dependency",
                    "staged runtime has an unresolved local module dependency",
                    Some(format!("{} -> {dependency}", module.display())),
                    None,
                )
            })?;
            let resolved =
                fs::canonicalize(resolved).map_err(io("resolve staged runtime dependency"))?;
            if !resolved.starts_with(&generation) {
                return Err(Error::validation_invalid_argument(
                    "runtime_dependency",
                    "staged runtime dependency escapes its generation",
                    Some(resolved.display().to_string()),
                    None,
                ));
            }
            if resolved
                .extension()
                .is_some_and(|extension| extension == "js" || extension == "cjs")
            {
                pending.insert(resolved);
            }
        }
    }
    Ok(())
}

fn collect_javascript(root: &Path, files: &mut BTreeSet<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(root).map_err(io("read staged runtime package"))? {
        let path = entry.map_err(io("read staged runtime package"))?.path();
        if path.is_dir() {
            collect_javascript(&path, files)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "js" || extension == "cjs")
        {
            files.insert(path);
        }
    }
    Ok(())
}

fn local_requires(module: &Path) -> Result<Vec<String>> {
    let contents = fs::read_to_string(module).map_err(io("read runtime module"))?;
    Ok(contents
        .split("require(")
        .skip(1)
        .filter_map(|suffix| {
            let suffix = suffix.trim_start();
            let quote = suffix.chars().next()?;
            (quote == '\'' || quote == '"').then_some(())?;
            suffix[1..]
                .find(quote)
                .map(|end| suffix[1..1 + end].to_string())
        })
        .filter(|dependency| dependency.starts_with('.'))
        .collect())
}

fn resolve_local_module(module: &Path, dependency: &str) -> Option<PathBuf> {
    let base = module.parent()?.join(dependency);
    [
        base.clone(),
        base.with_extension("js"),
        base.with_extension("cjs"),
        base.with_extension("json"),
        base.join("index.js"),
        base.join("index.json"),
    ]
    .into_iter()
    .find(|path| path.is_file())
}

fn write_metadata(
    package: &Path,
    source: &str,
    revision: Option<&str>,
    assets: &[String],
) -> Result<()> {
    write_synced(
        &package.join(".source-url"),
        source.as_bytes(),
        "write runtime source",
    )?;
    if let Some(revision) = revision {
        write_synced(
            &package.join(".source-revision"),
            revision.as_bytes(),
            "write runtime source revision",
        )?;
    }
    let provenance = serde_json::to_vec_pretty(&Provenance {
        schema: "homeboy/runtime-package-provenance/v1",
        source,
        source_revision: revision,
        shared_assets: assets,
    })
    .map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("serialize runtime provenance".to_string()),
        )
    })?;
    write_synced(
        &package.join(".dependency-provenance.json"),
        &provenance,
        "write runtime provenance",
    )
}

fn switch_current(store: &Path, generation: &str) -> Result<()> {
    let temporary = store.join(format!(".{CURRENT}-{}", nonce()));
    remove_if_exists(&temporary, "clean runtime current pointer")?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(generation, &temporary)
        .map_err(io("stage runtime current pointer"))?;
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(generation, &temporary)
        .map_err(io("stage runtime current pointer"))?;
    sync_dir(store)?;
    fs::rename(&temporary, store.join(CURRENT)).map_err(io("activate runtime generation"))?;
    sync_dir(store)
}

fn ensure_stable_runtime_boundary(config_root: &Path) -> Result<()> {
    let boundary = paths::legacy_agent_runtimes()?;
    let expected = Path::new(GENERATIONS).join(CURRENT).join("agent-runtimes");
    if fs::read_link(&boundary).ok().as_deref() == Some(expected.as_path()) {
        return Ok(());
    }
    // The first generation already contains a validated copy of a legacy
    // directory or linked source. Replace that old boundary once, never again.
    let backup = config_root.join(format!(".agent-runtimes-legacy-{}", nonce()));
    let temporary = config_root.join(format!(".agent-runtimes-boundary-{}", nonce()));
    remove_if_exists(&temporary, "clean staged runtime boundary")?;
    #[cfg(unix)]
    let linked = std::os::unix::fs::symlink(&expected, &temporary);
    #[cfg(windows)]
    let linked = std::os::windows::fs::symlink_dir(&expected, &temporary);
    linked.map_err(io("stage stable runtime boundary"))?;
    sync_dir(config_root)?;
    write_boundary_migration(
        config_root,
        &BoundaryMigration {
            schema: BOUNDARY_MIGRATION_SCHEMA.to_string(),
            backup: backup
                .file_name()
                .and_then(|name| name.to_str())
                .expect("generated boundary backup name is valid UTF-8")
                .to_string(),
            temporary: temporary
                .file_name()
                .and_then(|name| name.to_str())
                .expect("generated boundary temporary name is valid UTF-8")
                .to_string(),
        },
    )?;
    if fs::symlink_metadata(&boundary).is_ok() {
        fs::rename(&boundary, &backup).map_err(io("migrate legacy runtime boundary"))?;
        sync_dir(config_root)?;
        crash_after_boundary_backup_rename()?;
    }
    fs::rename(&temporary, &boundary).map_err(io("publish stable runtime boundary"))?;
    sync_dir(config_root)?;
    remove_if_exists(&backup, "remove migrated legacy runtime boundary")?;
    sync_dir(config_root)?;
    remove_if_exists(
        &config_root.join(BOUNDARY_MIGRATION),
        "finalize runtime boundary migration",
    )?;
    sync_dir(config_root)
}

fn recover(store: &Path) -> Result<()> {
    let config_root = store.parent().ok_or_else(|| {
        Error::internal_io(
            "runtime generation store has no config root",
            Some("recover runtime boundary migration".to_string()),
        )
    })?;
    recover_boundary_migration(config_root)?;
    let journal = store.join(JOURNAL);
    let Ok(raw) = fs::read(&journal) else {
        return Ok(());
    };
    let record: RefreshJournal = serde_json::from_slice(&raw).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("read runtime refresh journal".to_string()),
        )
    })?;
    if record.schema != JOURNAL_SCHEMA
        || safe_relative(&record.generation, "runtime_generation").is_err()
    {
        return Err(Error::validation_invalid_argument(
            "runtime_generation",
            "unknown or unsafe runtime refresh journal",
            Some(record.generation),
            None,
        ));
    }
    let generation = store.join(&record.generation);
    let current = active_current_generation(store)?;
    let published_generation = fs::canonicalize(&generation).ok();
    if !record.switched && current.as_ref() != published_generation.as_ref() {
        remove_if_exists(
            &store.join("staging").join(&record.generation),
            "recover staged runtime generation",
        )?;
        remove_if_exists(&generation, "recover unpublished runtime generation")?;
    }
    remove_if_exists(&journal, "finalize recovered runtime generation")?;
    sync_dir(store)
}

fn write_boundary_migration(config_root: &Path, migration: &BoundaryMigration) -> Result<()> {
    let bytes = serde_json::to_vec(migration).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("serialize runtime boundary migration".to_string()),
        )
    })?;
    let intent = config_root.join(BOUNDARY_MIGRATION);
    let temporary = config_root.join(format!(".{BOUNDARY_MIGRATION}-{}", nonce()));
    write_synced(&temporary, &bytes, "write runtime boundary migration")?;
    fs::rename(&temporary, &intent).map_err(io("publish runtime boundary migration"))?;
    sync_dir(config_root)
}

fn recover_boundary_migration(config_root: &Path) -> Result<()> {
    let intent = config_root.join(BOUNDARY_MIGRATION);
    let Ok(raw) = fs::read(&intent) else {
        return Ok(());
    };
    let migration: BoundaryMigration = serde_json::from_slice(&raw).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("read runtime boundary migration".to_string()),
        )
    })?;
    if migration.schema != BOUNDARY_MIGRATION_SCHEMA {
        return Err(Error::validation_invalid_argument(
            "runtime_generation",
            "unknown runtime boundary migration",
            Some(migration.schema),
            None,
        ));
    }
    let backup = config_root.join(single_config_entry(&migration.backup, "runtime_boundary")?);
    let temporary = config_root.join(single_config_entry(
        &migration.temporary,
        "runtime_boundary",
    )?);
    let boundary = paths::legacy_agent_runtimes()?;
    let expected = Path::new(GENERATIONS).join(CURRENT).join("agent-runtimes");

    if fs::read_link(&boundary).ok().as_deref() != Some(expected.as_path()) {
        if fs::symlink_metadata(&boundary).is_err()
            && fs::read_link(&temporary).ok().as_deref() == Some(expected.as_path())
        {
            fs::rename(&temporary, &boundary)
                .map_err(io("recover stable runtime boundary publication"))?;
            sync_dir(config_root)?;
        } else if fs::symlink_metadata(&boundary).is_err() && fs::symlink_metadata(&backup).is_ok()
        {
            fs::rename(&backup, &boundary).map_err(io("restore legacy runtime boundary"))?;
            sync_dir(config_root)?;
        }
    }

    if fs::read_link(&boundary).ok().as_deref() == Some(expected.as_path()) {
        remove_if_exists(&backup, "finalize recovered runtime boundary backup")?;
    } else if fs::symlink_metadata(&boundary).is_err() {
        return Err(Error::validation_invalid_argument(
            "runtime_generation",
            "runtime boundary migration has no recoverable boundary",
            Some(intent.display().to_string()),
            None,
        ));
    }
    remove_if_exists(&temporary, "finalize recovered runtime boundary stage")?;
    remove_if_exists(&intent, "finalize recovered runtime boundary migration")?;
    sync_dir(config_root)
}

fn single_config_entry<'a>(value: &'a str, field: &str) -> Result<&'a Path> {
    let path = safe_relative(value, field)?;
    if path.components().count() != 1 {
        return Err(Error::validation_invalid_argument(
            field,
            "path is not a config-root entry",
            Some(value.to_string()),
            None,
        ));
    }
    Ok(path)
}

fn write_journal(store: &Path, journal: &RefreshJournal) -> Result<()> {
    let bytes = serde_json::to_vec(journal).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("serialize runtime refresh journal".to_string()),
        )
    })?;
    let temporary = store.join(format!(".{JOURNAL}-{}", nonce()));
    write_synced(&temporary, &bytes, "write runtime refresh journal")?;
    fs::rename(&temporary, store.join(JOURNAL)).map_err(io("publish runtime refresh journal"))?;
    sync_dir(store)
}

fn safe_relative<'a>(value: &'a str, field: &str) -> Result<&'a Path> {
    let path = Path::new(value);
    if value.trim().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err(Error::validation_invalid_argument(
            field,
            "path is not a safe relative path",
            Some(value.to_string()),
            None,
        ));
    }
    Ok(path)
}

fn reject_symlink_tree(root: &Path) -> Result<()> {
    reject_symlink_tree_inner(root, false)
}
fn reject_symlink_tree_except_root(root: &Path) -> Result<()> {
    reject_symlink_tree_inner(root, true)
}
fn reject_symlink_tree_inner(root: &Path, allow_root_link: bool) -> Result<()> {
    if !allow_root_link
        && fs::symlink_metadata(root)
            .map_err(io("inspect runtime source"))?
            .file_type()
            .is_symlink()
    {
        return Err(Error::validation_invalid_argument(
            "runtime_source",
            "runtime source contains a symbolic link",
            Some(root.display().to_string()),
            None,
        ));
    }
    for entry in fs::read_dir(root).map_err(io("read runtime source"))? {
        let path = entry.map_err(io("read runtime source"))?.path();
        let meta = fs::symlink_metadata(&path).map_err(io("inspect runtime source"))?;
        if meta.file_type().is_symlink() {
            return Err(Error::validation_invalid_argument(
                "runtime_source",
                "runtime source contains a symbolic link",
                Some(path.display().to_string()),
                None,
            ));
        }
        if meta.is_dir() {
            reject_symlink_tree_inner(&path, false)?;
        }
    }
    Ok(())
}

fn copy_regular_tree(source: &Path, target: &Path, context: &str) -> Result<()> {
    copy_tree(source, target, context, EntryPolicy::CopyRegularFilesOnly)
}

fn remove_if_exists(path: &Path, context: &str) -> Result<()> {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    let result = if meta.file_type().is_symlink() || meta.is_file() {
        fs::remove_file(path)
    } else {
        fs::remove_dir_all(path)
    };
    result.map_err(io(context))
}

fn write_synced(path: &Path, contents: &[u8], context: &str) -> Result<()> {
    fs::write(path, contents).map_err(io(context))?;
    File::open(path)
        .map_err(io(context))?
        .sync_all()
        .map_err(io(context))
}

fn sync_tree(root: &Path) -> Result<()> {
    for entry in fs::read_dir(root).map_err(io("sync runtime generation"))? {
        let path = entry.map_err(io("sync runtime generation"))?.path();
        if path.is_dir() {
            sync_tree(&path)?;
        } else {
            File::open(&path)
                .map_err(io("sync runtime generation"))?
                .sync_all()
                .map_err(io("sync runtime generation"))?;
        }
    }
    sync_dir(root)
}

fn sync_dir(path: &Path) -> Result<()> {
    File::open(path)
        .map_err(io("sync runtime generation directory"))?
        .sync_all()
        .map_err(io("sync runtime generation directory"))
}
fn io(context: &str) -> impl FnOnce(std::io::Error) -> Error {
    let context = context.to_string();
    move |error| Error::internal_io(error.to_string(), Some(context))
}
fn nonce() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_isolated_home;
    use std::process::Command;

    fn package(root: &Path, id: &str, marker: &str) -> PathBuf {
        let path = root.join("agent-runtimes").join(id);
        fs::create_dir_all(&path).unwrap();
        fs::write(
            path.join(format!("{id}.json")),
            format!(r#"{{"schema":"homeboy/agent-runtime-manifest/v1","id":"{id}"}}"#),
        )
        .unwrap();
        fs::write(path.join("marker"), marker).unwrap();
        path
    }
    fn manifest(root: &Path, assets: &[&str]) {
        fs::write(
            root.join(ROOT_MANIFEST),
            serde_json::json!({"shared_assets": assets}).to_string(),
        )
        .unwrap();
    }

    #[test]
    fn package_only_refresh_migrates_legacy_runtime_and_preserves_unrelated_config() {
        with_isolated_home(|_| {
            let source = tempfile::tempdir().unwrap();
            package(source.path(), "neutral", "new");
            let root = paths::homeboy().unwrap();
            package(&root, "other", "old");
            fs::write(root.join("keep"), "yes").unwrap();
            let result = refresh("neutral", &source.path().to_string_lossy(), None).unwrap();
            assert_eq!(
                fs::read_to_string(result.path.join("marker")).unwrap(),
                "new"
            );
            assert_eq!(
                fs::read_to_string(paths::agent_runtimes().unwrap().join("other/marker")).unwrap(),
                "old"
            );
            assert_eq!(fs::read_to_string(root.join("keep")).unwrap(), "yes");
        });
    }

    #[test]
    #[ignore = "requires HOMEBOY_OPENCODE_RUNTIME_SOURCE with a Homeboy Extensions checkout"]
    fn installed_opencode_boundary_suite_runs_without_source_siblings() {
        with_isolated_home(|_| {
            let source =
                PathBuf::from(std::env::var_os("HOMEBOY_OPENCODE_RUNTIME_SOURCE").expect(
                    "set HOMEBOY_OPENCODE_RUNTIME_SOURCE to a Homeboy Extensions source root",
                ));
            assert!(source.join(ROOT_MANIFEST).is_file());
            assert!(source
                .join("agent-runtimes/opencode/tests/opencode-agent-task-executor-boundary.test.js")
                .is_file());

            let staged_source = tempfile::tempdir().unwrap();
            fs::copy(
                source.join(ROOT_MANIFEST),
                staged_source.path().join(ROOT_MANIFEST),
            )
            .unwrap();
            let manifest: RootManifest =
                serde_json::from_slice(&fs::read(source.join(ROOT_MANIFEST)).unwrap()).unwrap();
            for asset in manifest.shared_assets {
                let asset = asset.path();
                copy_regular_tree(
                    &source.join(&asset),
                    &staged_source.path().join(&asset),
                    "stage real OpenCode runtime test source",
                )
                .unwrap();
            }

            let result =
                refresh("opencode", &staged_source.path().to_string_lossy(), None).unwrap();
            fs::remove_dir_all(staged_source.path()).unwrap();
            let documented = paths::legacy_agent_runtimes().unwrap();
            assert_eq!(result.path, documented.join("opencode"));
            assert_eq!(
                fs::read_link(&documented).unwrap(),
                Path::new("runtime-generations/current/agent-runtimes")
            );
            let output = Command::new("node")
                .arg(
                    documented.join("opencode/tests/opencode-agent-task-executor-boundary.test.js"),
                )
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        });
    }

    #[test]
    fn crash_after_boundary_bootstrap_keeps_direct_and_generation_consumers_coherent() {
        with_isolated_home(|_| {
            let root = paths::homeboy().unwrap();
            package(&root, "legacy", "old");
            let source = tempfile::tempdir().unwrap();
            package(source.path(), "neutral", "new");

            CRASH_AFTER_BOUNDARY_BOOTSTRAP.store(true, Ordering::SeqCst);
            assert!(refresh("neutral", &source.path().to_string_lossy(), None).is_err());

            let boundary = paths::legacy_agent_runtimes().unwrap();
            let current = active_current_generation(&root.join(GENERATIONS))
                .unwrap()
                .unwrap();
            assert_eq!(
                fs::read_link(&boundary).unwrap(),
                Path::new("runtime-generations/current/agent-runtimes")
            );
            assert_eq!(
                fs::read_to_string(boundary.join("legacy/marker")).unwrap(),
                "old"
            );
            assert_eq!(
                fs::read_to_string(current.join("agent-runtimes/legacy/marker")).unwrap(),
                "old"
            );
            assert!(!boundary.join("neutral").exists());
        });
    }

    #[test]
    fn recovery_after_boundary_backup_rename_restores_a_coherent_stable_boundary() {
        with_isolated_home(|_| {
            let root = paths::homeboy().unwrap();
            package(&root, "legacy", "old");
            let source = tempfile::tempdir().unwrap();
            package(source.path(), "neutral", "new");
            let store = root.join(GENERATIONS);

            CRASH_AFTER_BOUNDARY_BACKUP_RENAME.store(true, Ordering::SeqCst);
            assert!(refresh("neutral", &source.path().to_string_lossy(), None).is_err());

            let boundary = paths::legacy_agent_runtimes().unwrap();
            assert!(fs::symlink_metadata(&boundary).is_err());
            let current = active_current_generation(&store).unwrap().unwrap();
            assert_eq!(
                fs::read_to_string(current.join("agent-runtimes/legacy/marker")).unwrap(),
                "old"
            );

            recover(&store).unwrap();

            assert_eq!(
                fs::read_link(&boundary).unwrap(),
                Path::new("runtime-generations/current/agent-runtimes")
            );
            assert_eq!(
                fs::read_to_string(boundary.join("legacy/marker")).unwrap(),
                "old"
            );
            assert_eq!(
                fs::read_to_string(current.join("agent-runtimes/legacy/marker")).unwrap(),
                "old"
            );
            assert!(!root.join(BOUNDARY_MIGRATION).exists());
        });
    }

    #[test]
    fn staged_failure_and_pre_switch_crash_keep_previous_generation() {
        with_isolated_home(|_| {
            let source = tempfile::tempdir().unwrap();
            package(source.path(), "neutral", "old");
            refresh("neutral", &source.path().to_string_lossy(), None).unwrap();
            package(source.path(), "neutral", "broken");
            manifest(source.path(), &["missing"]);
            assert!(refresh("neutral", &source.path().to_string_lossy(), None).is_err());
            assert_eq!(
                fs::read_to_string(paths::agent_runtimes().unwrap().join("neutral/marker"))
                    .unwrap(),
                "old"
            );
            let store = paths::homeboy().unwrap().join(GENERATIONS);
            let staged = store.join("staging/crash");
            fs::create_dir_all(&staged).unwrap();
            write_journal(
                &store,
                &RefreshJournal {
                    schema: JOURNAL_SCHEMA.into(),
                    generation: "crash".into(),
                    switched: false,
                },
            )
            .unwrap();
            recover(&store).unwrap();
            assert!(!staged.exists());
            assert_eq!(
                fs::read_to_string(paths::agent_runtimes().unwrap().join("neutral/marker"))
                    .unwrap(),
                "old"
            );
        });
    }

    #[test]
    fn post_switch_crash_recovers_idempotently_and_consumers_observe_whole_generation() {
        with_isolated_home(|_| {
            let source = tempfile::tempdir().unwrap();
            package(source.path(), "neutral", "one");
            refresh("neutral", &source.path().to_string_lossy(), None).unwrap();
            package(source.path(), "neutral", "two");
            let store = paths::homeboy().unwrap().join(GENERATIONS);
            let generation = fs::read_link(store.join(CURRENT)).unwrap();
            write_journal(
                &store,
                &RefreshJournal {
                    schema: JOURNAL_SCHEMA.into(),
                    generation: generation.to_string_lossy().to_string(),
                    switched: false,
                },
            )
            .unwrap();
            recover(&store).unwrap();
            assert_eq!(
                fs::read_to_string(paths::agent_runtimes().unwrap().join("neutral/marker"))
                    .unwrap(),
                "one"
            );
        });
    }

    #[test]
    fn concurrent_consumer_observes_only_complete_generations() {
        with_isolated_home(|_| {
            let source = tempfile::tempdir().unwrap();
            package(source.path(), "neutral", "one");
            fs::create_dir_all(source.path().join("agent-task-contracts")).unwrap();
            fs::write(source.path().join("agent-task-contracts/marker"), "one").unwrap();
            manifest(source.path(), &["agent-task-contracts"]);
            refresh("neutral", &source.path().to_string_lossy(), None).unwrap();

            package(source.path(), "neutral", "two");
            fs::write(source.path().join("agent-task-contracts/marker"), "two").unwrap();
            let store = paths::homeboy().unwrap().join(GENERATIONS);
            let reader = std::thread::spawn(move || {
                for _ in 0..1_000 {
                    let runtimes = paths::agent_runtimes().unwrap();
                    // A consumer resolves the stable boundary once, so both
                    // package and sibling dependency stay in that generation.
                    let runtimes = fs::canonicalize(runtimes).unwrap();
                    let generation = runtimes.parent().unwrap();
                    let runtime = fs::read_to_string(runtimes.join("neutral/marker")).unwrap();
                    let dependency =
                        fs::read_to_string(generation.join("agent-task-contracts/marker")).unwrap();
                    assert_eq!(runtime, dependency);
                }
            });
            refresh("neutral", &source.path().to_string_lossy(), None).unwrap();
            reader.join().unwrap();
            assert!(fs::read_link(store.join(CURRENT)).is_ok());
        });
    }

    #[cfg(unix)]
    #[test]
    fn linked_legacy_runtime_is_migrated_without_mutating_its_source() {
        use std::os::unix::fs::symlink;

        with_isolated_home(|_| {
            let linked = tempfile::tempdir().unwrap();
            package(linked.path(), "legacy", "linked");
            let source = tempfile::tempdir().unwrap();
            package(source.path(), "neutral", "new");
            let legacy = paths::legacy_agent_runtimes().unwrap();
            fs::create_dir_all(legacy.parent().unwrap()).unwrap();
            symlink(linked.path().join("agent-runtimes"), &legacy).unwrap();

            refresh("neutral", &source.path().to_string_lossy(), None).unwrap();

            assert!(fs::symlink_metadata(&legacy)
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(
                fs::read_to_string(linked.path().join("agent-runtimes/legacy/marker")).unwrap(),
                "linked"
            );
            assert_eq!(
                fs::read_to_string(paths::agent_runtimes().unwrap().join("legacy/marker")).unwrap(),
                "linked"
            );
        });
    }

    #[test]
    fn linked_runtime_asset_reinstall_publishes_a_new_generation() {
        with_isolated_home(|_| {
            let runtime = tempfile::tempdir().unwrap();
            package(runtime.path(), "neutral", "one");
            refresh("neutral", &runtime.path().to_string_lossy(), None).unwrap();
            let first =
                fs::read_link(paths::homeboy().unwrap().join(GENERATIONS).join(CURRENT)).unwrap();

            let linked = tempfile::tempdir().unwrap();
            package(linked.path(), "linked", "first");
            manifest(linked.path(), &["agent-runtimes"]);
            refresh_shared_assets(linked.path()).unwrap();
            let second =
                fs::read_link(paths::homeboy().unwrap().join(GENERATIONS).join(CURRENT)).unwrap();

            assert_ne!(first, second);
            assert_eq!(
                fs::read_to_string(paths::agent_runtimes().unwrap().join("linked/marker")).unwrap(),
                "first"
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn rejects_nested_current_generation_symlink_escape() {
        use std::os::unix::fs::symlink;

        with_isolated_home(|_| {
            let source = tempfile::tempdir().unwrap();
            package(source.path(), "neutral", "working");
            refresh("neutral", &source.path().to_string_lossy(), None).unwrap();
            let store = paths::homeboy().unwrap().join(GENERATIONS);
            let outside = tempfile::tempdir().unwrap();
            fs::create_dir_all(outside.path().join("agent-runtimes")).unwrap();
            fs::remove_file(store.join(CURRENT)).unwrap();
            symlink(outside.path(), store.join(CURRENT)).unwrap();

            assert!(refresh("neutral", &source.path().to_string_lossy(), None).is_err());
        });
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape_without_replacing_active_runtime() {
        use std::os::unix::fs::symlink;
        with_isolated_home(|_| {
            let source = tempfile::tempdir().unwrap();
            package(source.path(), "neutral", "working");
            refresh("neutral", &source.path().to_string_lossy(), None).unwrap();
            let outside = tempfile::tempdir().unwrap();
            fs::create_dir_all(source.path().join("agent-task-contracts")).unwrap();
            symlink(
                outside.path(),
                source.path().join("agent-task-contracts/escape"),
            )
            .unwrap();
            manifest(source.path(), &["agent-task-contracts"]);
            assert!(refresh("neutral", &source.path().to_string_lossy(), None).is_err());
            assert_eq!(
                fs::read_to_string(paths::agent_runtimes().unwrap().join("neutral/marker"))
                    .unwrap(),
                "working"
            );
        });
    }
}
