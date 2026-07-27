use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

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

fn seed_generation(config_root: &Path, store: &Path, stage: &Path) -> Result<()> {
    let current = store.join(CURRENT);
    if active_current_generation(store).is_some() {
        reject_symlink_tree_except_root(&current)?;
        return copy_regular_tree(&current, stage, "copy active runtime generation");
    }
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
    Ok(())
}

fn active_current_generation(store: &Path) -> Option<PathBuf> {
    let target = fs::read_link(store.join(CURRENT)).ok()?;
    safe_relative(target.to_str()?, "runtime_generation").ok()?;
    let generation = store.join(target);
    generation
        .join("agent-runtimes")
        .is_dir()
        .then_some(generation)
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

fn recover(store: &Path) -> Result<()> {
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
    let current = active_current_generation(store);
    if !record.switched && current.as_deref() != Some(generation.as_path()) {
        remove_if_exists(
            &store.join("staging").join(&record.generation),
            "recover staged runtime generation",
        )?;
        remove_if_exists(&generation, "recover unpublished runtime generation")?;
    }
    remove_if_exists(&journal, "finalize recovered runtime generation")?;
    sync_dir(store)
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
    fn installed_runtime_and_declared_dependencies_execute_without_source_siblings() {
        with_isolated_home(|_| {
            let source = tempfile::tempdir().unwrap();
            let runtime = package(source.path(), "opencode", "new");
            fs::create_dir_all(source.path().join("agent-runtimes/lib")).unwrap();
            fs::create_dir_all(source.path().join("agent-task-contracts")).unwrap();
            fs::write(runtime.join("run.cjs"), "console.log(require('../lib/shared').value + require('../../agent-task-contracts').value)").unwrap();
            fs::write(
                source.path().join("agent-runtimes/lib/shared.js"),
                "exports.value='installed-'",
            )
            .unwrap();
            fs::write(
                source.path().join("agent-task-contracts/index.js"),
                "exports.value='boundary'",
            )
            .unwrap();
            manifest(source.path(), &["agent-runtimes", "agent-task-contracts"]);
            let result = refresh("opencode", &source.path().to_string_lossy(), None).unwrap();
            fs::remove_dir_all(source.path()).unwrap();
            let output = Command::new("node")
                .arg(result.path.join("run.cjs"))
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout),
                "installed-boundary\n"
            );
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
