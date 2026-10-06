//! Content-addressed cache of prepared validation dependencies (#15253).
//!
//! Preparing a validation dependency means copying its checkout and running
//! its full dependency lifecycle (install + build). Before this cache,
//! every sync did that from scratch:
//!
//! - once per declared dependency;
//! - again for every transitive dependency, because staging syncs each
//!   dependency as an extra workspace, which prepares *its* own validation
//!   dependencies;
//! - again on every transport retry;
//! - again in every concurrent Cook on the same controller.
//!
//! A single-plugin Cook declaring eight dependencies ran 21 builds without
//! reaching its provider.
//!
//! An entry is keyed by everything that can change the prepared output:
//!
//! - the component id;
//! - the content identity of the exact source that is copied (the same
//!   `snapshot_identity` the workspace sync uses, which hashes file contents
//!   and honors the same excludes);
//! - the excludes themselves;
//! - the component's effective configuration;
//! - the identity of every extension the lifecycle may run;
//! - the Homeboy version.
//!
//! Anything that cannot be fingerprinted is prepared uncached, the same way
//! it was before.
//!
//! Concurrency: each key is guarded by an advisory `flock` on its own lock
//! file. The first preparer builds while later ones wait, then reuse the entry.
//! `flock` is released when the holder dies, so a crashed build never wedges a
//! key, and the lock is safe to hold for a multi-minute build.
//!
//! Entries are immutable. A build runs in a staging directory inside the cache
//! root and is published with an atomic rename only after the full lifecycle
//! (including output verification) succeeds. A failed build publishes nothing.
//! Consumers always get a private copy, because callers write evidence files
//! into the prepared directory.
//!
//! Not covered by the key: host toolchain upgrades (interpreters, package
//! managers) with no source or config change. The generic layer has no
//! toolchain vocabulary, so entries expire after [`ENTRY_TTL`] instead, and
//! the cache can be disabled with [`CACHE_DISABLE_ENV`].
//!
//! Eviction: after each publish, entries unused for longer than [`ENTRY_TTL`]
//! are removed, and at most [`ENTRIES_PER_COMPONENT`] entries are kept per
//! component (most recently used first). Entries whose lock is held are
//! skipped.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use homeboy_core::component::Component;
use homeboy_core::error::{Error, Result};

use crate::workspace::{copy_snapshot_to_directory, sanitize_path_segment, snapshot_identity};

/// Set to `0`, `false`, `off`, or `no` to disable the cache and prepare every
/// validation dependency from scratch.
pub(crate) const CACHE_DISABLE_ENV: &str = "HOMEBOY_VALIDATION_DEPENDENCY_CACHE";

/// Homeboy data store holding the cache.
const CACHE_STORE: &str = "prepared-validation-dependencies";

/// Bump when the key recipe or entry layout changes.
const KEY_SCHEMA: &str = "homeboy/prepared-validation-dependency/v1";

/// Entries unused for this long are evicted.
pub(crate) const ENTRY_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Maximum retained entries per component id.
pub(crate) const ENTRIES_PER_COMPONENT: usize = 3;

const ENTRY_TREE: &str = "tree";
const ENTRY_META: &str = "meta.json";
const LOCKS_DIR: &str = ".locks";
const STAGING_PREFIX: &str = ".staging-";

#[derive(Debug, Serialize, Deserialize)]
struct EntryMeta {
    schema: String,
    component_id: String,
    key: String,
    source_path: String,
    homeboy_version: String,
    build_seconds: f64,
}

/// A prepared dependency tree owned by one consumer.
#[derive(Debug)]
pub(crate) struct PreparedDependencyCopy {
    pub(crate) path: PathBuf,
    pub(crate) outcome: CacheOutcome,
    _tempdir: tempfile::TempDir,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CacheOutcome {
    Hit,
    Miss,
    Disabled,
    Uncacheable,
}

impl CacheOutcome {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            CacheOutcome::Hit => "hit",
            CacheOutcome::Miss => "miss",
            CacheOutcome::Disabled => "disabled",
            CacheOutcome::Uncacheable => "uncacheable",
        }
    }

    fn label(self) -> &'static str {
        match self {
            CacheOutcome::Hit => "cache hit",
            CacheOutcome::Miss => "cache miss, built",
            CacheOutcome::Disabled => "cache disabled, built",
            CacheOutcome::Uncacheable => "uncacheable, built",
        }
    }
}

pub(crate) fn cache_enabled() -> bool {
    match std::env::var(CACHE_DISABLE_ENV) {
        Ok(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        ),
        Err(_) => true,
    }
}

/// Prepare `component` from `source` into a private directory, reusing a
/// cached prepared tree when one exists for the same key.
///
/// `lifecycle` runs the dependency lifecycle in the directory it is given,
/// with `component` already pointing its `local_path` at that directory.
pub(crate) fn prepare_with_cache<F>(
    component: &Component,
    source: &Path,
    excludes: &[String],
    lifecycle: F,
) -> Result<PreparedDependencyCopy>
where
    F: FnOnce(&Component, &Path) -> Result<()>,
{
    let started = Instant::now();
    let consumer = consumer_tempdir(&component.id)?;
    let consumer_path = consumer.path().join(sanitize_path_segment(&component.id));

    let outcome = if !cache_enabled() {
        build_in_place(component, source, &consumer_path, excludes, lifecycle)?;
        CacheOutcome::Disabled
    } else {
        match cacheable_context(component, source, excludes) {
            Ok((root, key)) => prepare_through_cache(
                component,
                source,
                excludes,
                &root,
                &key,
                &consumer_path,
                lifecycle,
            )?,
            Err(reason) => {
                eprintln!(
                    "Validation dependency `{}`: not cacheable ({reason}); preparing uncached.",
                    component.id
                );
                build_in_place(component, source, &consumer_path, excludes, lifecycle)?;
                CacheOutcome::Uncacheable
            }
        }
    };

    eprintln!(
        "Validation dependency `{}`: {} in {:.1}s.",
        component.id,
        outcome.label(),
        started.elapsed().as_secs_f64()
    );

    Ok(PreparedDependencyCopy {
        path: consumer_path,
        outcome,
        _tempdir: consumer,
    })
}

fn prepare_through_cache<F>(
    component: &Component,
    source: &Path,
    excludes: &[String],
    root: &Path,
    key: &str,
    consumer_path: &Path,
    lifecycle: F,
) -> Result<CacheOutcome>
where
    F: FnOnce(&Component, &Path) -> Result<()>,
{
    let _lock = KeyLock::acquire(root, key)?;
    let entry = root.join(key);

    if entry_is_complete(&entry) {
        copy_snapshot_to_directory(&entry.join(ENTRY_TREE), consumer_path, &[])?;
        touch(&entry.join(ENTRY_META));
        return Ok(CacheOutcome::Hit);
    }
    if entry.exists() {
        // An entry without metadata was never published; discard it.
        let _ = fs::remove_dir_all(&entry);
    }

    let staging = tempfile::Builder::new()
        .prefix(&format!("{STAGING_PREFIX}{key}-"))
        .tempdir_in(root)
        .map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some(format!(
                    "create validation dependency cache staging for {}",
                    component.id
                )),
            )
        })?;
    let tree = staging.path().join(ENTRY_TREE);
    let build_started = Instant::now();
    // On failure `staging` is dropped and removed, so nothing is published.
    build_in_place(component, source, &tree, excludes, lifecycle)?;

    let meta = EntryMeta {
        schema: KEY_SCHEMA.to_string(),
        component_id: component.id.clone(),
        key: key.to_string(),
        source_path: source.display().to_string(),
        homeboy_version: env!("CARGO_PKG_VERSION").to_string(),
        build_seconds: build_started.elapsed().as_secs_f64(),
    };
    let meta_json = serde_json::to_vec_pretty(&meta).map_err(|err| {
        Error::internal_unexpected(format!("serialize validation dependency cache meta: {err}"))
    })?;
    fs::write(staging.path().join(ENTRY_META), meta_json).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("write validation dependency cache meta".to_string()),
        )
    })?;

    let staged = staging.keep();
    if let Err(err) = fs::rename(&staged, &entry) {
        let _ = fs::remove_dir_all(&staged);
        return Err(Error::internal_io(
            err.to_string(),
            Some(format!(
                "publish validation dependency cache entry for {}",
                component.id
            )),
        ));
    }

    copy_snapshot_to_directory(&entry.join(ENTRY_TREE), consumer_path, &[])?;
    evict(root, key);
    Ok(CacheOutcome::Miss)
}

/// Copy the source into `destination` and run the lifecycle there.
fn build_in_place<F>(
    component: &Component,
    source: &Path,
    destination: &Path,
    excludes: &[String],
    lifecycle: F,
) -> Result<()>
where
    F: FnOnce(&Component, &Path) -> Result<()>,
{
    copy_snapshot_to_directory(source, destination, excludes)?;
    let mut prepared = component.clone();
    prepared.local_path = destination.display().to_string();
    lifecycle(&prepared, destination)
}

fn consumer_tempdir(component_id: &str) -> Result<tempfile::TempDir> {
    tempfile::tempdir().map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!(
                "create validation dependency workspace {component_id}"
            )),
        )
    })
}

/// Resolve the cache root and key, or explain why this dependency cannot be
/// cached. Never fails the prepare: callers fall back to an uncached build.
fn cacheable_context(
    component: &Component,
    source: &Path,
    excludes: &[String],
) -> std::result::Result<(PathBuf, String), String> {
    let root = cache_root().map_err(|err| format!("cache root unavailable: {err}"))?;
    let key = cache_key(component, source, excludes)?;
    Ok((root, key))
}

pub(crate) fn cache_root() -> Result<PathBuf> {
    let root = homeboy_core::paths::homeboy_data_store(CACHE_STORE)?;
    fs::create_dir_all(root.join(LOCKS_DIR)).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!(
                "create validation dependency cache {}",
                root.display()
            )),
        )
    })?;
    Ok(root)
}

/// Compute the content-addressed key for a prepared dependency.
pub(crate) fn cache_key(
    component: &Component,
    source: &Path,
    excludes: &[String],
) -> std::result::Result<String, String> {
    let source_identity = snapshot_identity(source, excludes, &[])
        .map_err(|err| format!("source identity unavailable: {err}"))?;

    let mut config = component.clone();
    config.local_path = String::new();
    let config_bytes = homeboy_engine_primitives::canonical_json::canonical_json_bytes(&config)
        .map_err(|err| format!("component config not serializable: {err}"))?;

    let mut hasher = Sha256::new();
    let mut field = |label: &str, value: &[u8]| {
        hasher.update(label.as_bytes());
        hasher.update([0]);
        hasher.update((value.len() as u64).to_le_bytes());
        hasher.update(value);
    };
    field("schema", KEY_SCHEMA.as_bytes());
    field("homeboy", env!("CARGO_PKG_VERSION").as_bytes());
    field("component", component.id.as_bytes());
    field("source", source_identity.as_bytes());
    field("excludes", excludes.join("\n").as_bytes());
    field("config", &config_bytes);
    for extension_id in lifecycle_extension_ids(component) {
        field("extension", extension_id.as_bytes());
        field(
            "extension_identity",
            extension_identity(&extension_id).as_bytes(),
        );
    }

    let digest = hasher.finalize();
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(format!(
        "{}-{}",
        sanitize_path_segment(&component.id),
        &hex[..32]
    ))
}

fn lifecycle_extension_ids(component: &Component) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    if let Some(extensions) = &component.extensions {
        ids.extend(extensions.keys().cloned());
    }
    ids.extend(component.capability_extensions.values().cloned());
    ids
}

/// Identity of an installed extension: its manifest contents plus, when the
/// extension is a git checkout, its HEAD and working-tree status. The build and
/// dependency scripts live in the extension, so any change to them must change
/// the key. An extension that is not installed hashes to a stable marker; its
/// absence is then part of the configuration the lifecycle ran under.
fn extension_identity(extension_id: &str) -> String {
    let Ok(dir) = homeboy_core::paths::extension(extension_id) else {
        return "unresolved".to_string();
    };
    let manifest = homeboy_core::paths::extension_manifest(extension_id)
        .ok()
        .and_then(|path| fs::read(path).ok())
        .map(|bytes| {
            let digest = Sha256::digest(&bytes);
            digest
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        })
        .unwrap_or_else(|| "no-manifest".to_string());
    let head = homeboy_core::git::output_allow_empty(&dir, &["rev-parse", "HEAD"])
        .unwrap_or_else(|| "nogit".to_string());
    let status = homeboy_core::git::output_allow_empty(&dir, &["status", "--porcelain=v1"])
        .unwrap_or_default();
    format!("{manifest}\n{head}\n{status}")
}

fn entry_is_complete(entry: &Path) -> bool {
    entry.join(ENTRY_META).is_file() && entry.join(ENTRY_TREE).is_dir()
}

fn touch(path: &Path) {
    if let Ok(file) = OpenOptions::new().write(true).open(path) {
        let _ = file.set_modified(SystemTime::now());
    }
}

/// Exclusive advisory lock on one cache key, released on drop or process exit.
struct KeyLock {
    file: File,
}

impl KeyLock {
    fn acquire(root: &Path, key: &str) -> Result<Self> {
        let file = open_lock_file(root, key)?;
        file.lock().map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some(format!("lock validation dependency cache key {key}")),
            )
        })?;
        Ok(Self { file })
    }

    fn try_acquire(root: &Path, key: &str) -> Option<Self> {
        let file = open_lock_file(root, key).ok()?;
        file.try_lock().ok()?;
        Some(Self { file })
    }
}

impl Drop for KeyLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn open_lock_file(root: &Path, key: &str) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join(LOCKS_DIR).join(format!("{key}.lock")))
        .map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some(format!("open validation dependency cache lock {key}")),
            )
        })
}

struct EvictionCandidate {
    key: String,
    component_id: String,
    last_used: SystemTime,
}

/// Remove expired entries and trim each component to its newest entries.
/// Best effort: entries in use (lock held) and unreadable entries are skipped.
fn evict(root: &Path, keep_key: &str) {
    let Ok(read_dir) = fs::read_dir(root) else {
        return;
    };
    let now = SystemTime::now();
    let mut candidates = Vec::new();
    for entry in read_dir.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let meta_path = path.join(ENTRY_META);
        let Some(meta) = fs::read(&meta_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<EntryMeta>(&bytes).ok())
        else {
            continue;
        };
        let last_used = fs::metadata(&meta_path)
            .and_then(|metadata| metadata.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        candidates.push(EvictionCandidate {
            key: name,
            component_id: meta.component_id,
            last_used,
        });
    }

    candidates.sort_by(|a, b| {
        a.component_id
            .cmp(&b.component_id)
            .then(b.last_used.cmp(&a.last_used))
    });

    let mut kept_for_component: Option<(String, usize)> = None;
    for candidate in candidates {
        let rank = match &mut kept_for_component {
            Some((component, count)) if *component == candidate.component_id => {
                *count += 1;
                *count
            }
            _ => {
                kept_for_component = Some((candidate.component_id.clone(), 1));
                1
            }
        };
        let expired = now
            .duration_since(candidate.last_used)
            .is_ok_and(|age| age > ENTRY_TTL);
        if candidate.key == keep_key || (rank <= ENTRIES_PER_COMPONENT && !expired) {
            continue;
        }
        let Some(_lock) = KeyLock::try_acquire(root, &candidate.key) else {
            continue;
        };
        let _ = fs::remove_dir_all(root.join(&candidate.key));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};

    use super::*;
    use homeboy_core::test_support::{run_git_command as git, EnvVarGuard};

    fn excludes() -> Vec<String> {
        crate::workspace::DEFAULT_EXCLUDES
            .iter()
            .map(|value| value.to_string())
            .collect()
    }

    fn checkout(parent: &Path, id: &str) -> (PathBuf, Component) {
        let path = parent.join(id);
        fs::create_dir_all(path.join("lib")).expect("dependency dir");
        fs::write(
            path.join("homeboy.json"),
            serde_json::json!({ "id": id }).to_string(),
        )
        .expect("manifest");
        fs::write(path.join("lib/runtime.php"), "<?php\n").expect("source file");
        fs::write(path.join(".gitignore"), "built.txt\n").expect("gitignore");
        git(&path, &["init", "-q", "-b", "main"]);
        git(&path, &["config", "user.email", "test@example.com"]);
        git(&path, &["config", "user.name", "Homeboy Test"]);
        git(&path, &["add", "."]);
        git(&path, &["commit", "-q", "-m", "initial"]);
        let component = Component {
            id: id.to_string(),
            local_path: path.display().to_string(),
            ..Component::default()
        };
        (path, component)
    }

    /// Lifecycle double: counts invocations and writes a build output.
    fn counting_build(
        counter: &Arc<AtomicUsize>,
    ) -> impl FnOnce(&Component, &Path) -> Result<()> + '_ {
        move |component, path| {
            counter.fetch_add(1, Ordering::SeqCst);
            assert_eq!(component.local_path, path.display().to_string());
            fs::write(path.join("built.txt"), "built").map_err(|err| {
                Error::internal_io(err.to_string(), Some("write build output".into()))
            })
        }
    }

    fn published_entries(root: &Path) -> Vec<String> {
        fs::read_dir(root)
            .expect("cache root")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| !name.starts_with('.'))
            .collect()
    }

    #[test]
    fn second_prepare_of_same_source_and_config_is_a_cache_hit() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let parent = tempfile::tempdir().expect("parent");
            let (source, component) = checkout(parent.path(), "shared-runtime");
            let counter = Arc::new(AtomicUsize::new(0));

            let first =
                prepare_with_cache(&component, &source, &excludes(), counting_build(&counter))
                    .expect("first prepare");
            let second =
                prepare_with_cache(&component, &source, &excludes(), counting_build(&counter))
                    .expect("second prepare");

            assert_eq!(counter.load(Ordering::SeqCst), 1);
            assert_eq!(first.outcome, CacheOutcome::Miss);
            assert_eq!(second.outcome, CacheOutcome::Hit);
            assert_ne!(first.path, second.path, "each consumer owns its own copy");
            for prepared in [&first, &second] {
                assert_eq!(
                    fs::read_to_string(prepared.path.join("built.txt")).unwrap(),
                    "built"
                );
                assert!(prepared.path.join("lib/runtime.php").exists());
                assert!(!prepared.path.join(".git").exists());
            }
            assert!(!source.join("built.txt").exists(), "source stays clean");
            assert!(cache_root().unwrap().starts_with(home.path()));
        });
    }

    #[test]
    fn consumer_writes_do_not_leak_into_the_cache_entry() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let parent = tempfile::tempdir().expect("parent");
            let (source, component) = checkout(parent.path(), "shared-runtime");
            let counter = Arc::new(AtomicUsize::new(0));

            let first =
                prepare_with_cache(&component, &source, &excludes(), counting_build(&counter))
                    .expect("first prepare");
            fs::write(first.path.join("evidence.json"), "{}").expect("consumer write");
            let second =
                prepare_with_cache(&component, &source, &excludes(), counting_build(&counter))
                    .expect("second prepare");

            assert!(!second.path.join("evidence.json").exists());
        });
    }

    #[test]
    fn source_changes_are_cache_misses() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let parent = tempfile::tempdir().expect("parent");
            let (source, component) = checkout(parent.path(), "shared-runtime");
            let counter = Arc::new(AtomicUsize::new(0));

            prepare_with_cache(&component, &source, &excludes(), counting_build(&counter))
                .expect("baseline");

            // New commit.
            fs::write(source.join("lib/runtime.php"), "<?php // v2\n").expect("edit");
            git(&source, &["commit", "-q", "-am", "v2"]);
            let after_commit =
                prepare_with_cache(&component, &source, &excludes(), counting_build(&counter))
                    .expect("after commit");
            assert_eq!(after_commit.outcome, CacheOutcome::Miss);

            // An ignored file that the snapshot copies still changes the key.
            fs::create_dir_all(source.join("vendor")).expect("vendor");
            fs::write(source.join("vendor/lib.php"), "<?php\n").expect("vendor file");
            let after_untracked =
                prepare_with_cache(&component, &source, &excludes(), counting_build(&counter))
                    .expect("after untracked");
            assert_eq!(after_untracked.outcome, CacheOutcome::Miss);

            assert_eq!(counter.load(Ordering::SeqCst), 3);
        });
    }

    #[test]
    fn config_changes_are_cache_misses() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let parent = tempfile::tempdir().expect("parent");
            let (source, component) = checkout(parent.path(), "shared-runtime");
            let counter = Arc::new(AtomicUsize::new(0));

            prepare_with_cache(&component, &source, &excludes(), counting_build(&counter))
                .expect("baseline");
            let mut changed = component.clone();
            changed.build_command = Some("make dist".to_string());
            let outcome =
                prepare_with_cache(&changed, &source, &excludes(), counting_build(&counter))
                    .expect("changed config")
                    .outcome;

            assert_eq!(outcome, CacheOutcome::Miss);
            assert_eq!(counter.load(Ordering::SeqCst), 2);

            // local_path is the only field that differs per checkout and is
            // deliberately not part of the key.
            let mut relocated = component.clone();
            relocated.local_path = "/elsewhere".to_string();
            assert_eq!(
                cache_key(&component, &source, &excludes()).unwrap(),
                cache_key(&relocated, &source, &excludes()).unwrap()
            );
        });
    }

    #[test]
    fn failed_build_publishes_nothing_and_is_retried() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let parent = tempfile::tempdir().expect("parent");
            let (source, component) = checkout(parent.path(), "shared-runtime");

            let err = prepare_with_cache(&component, &source, &excludes(), |_, path| {
                fs::write(path.join("built.txt"), "partial").ok();
                Err(Error::internal_unexpected("build exploded"))
            })
            .expect_err("build failure propagates");
            assert!(err.to_string().contains("build exploded"));

            let root = cache_root().unwrap();
            assert!(published_entries(&root).is_empty());
            let staging_left = fs::read_dir(&root).unwrap().flatten().any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(STAGING_PREFIX)
            });
            assert!(!staging_left, "staging directory is cleaned up");
            assert!(!source.join("built.txt").exists());

            let counter = Arc::new(AtomicUsize::new(0));
            let retry =
                prepare_with_cache(&component, &source, &excludes(), counting_build(&counter))
                    .expect("retry");
            assert_eq!(retry.outcome, CacheOutcome::Miss);
            assert_eq!(counter.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn concurrent_prepares_of_one_key_build_once() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let parent = tempfile::tempdir().expect("parent");
            let (source, component) = checkout(parent.path(), "shared-runtime");
            let counter = Arc::new(AtomicUsize::new(0));
            let barrier = Arc::new(Barrier::new(3));

            let outcomes = std::thread::scope(|scope| {
                let handles = (0..3)
                    .map(|_| {
                        let counter = Arc::clone(&counter);
                        let barrier = Arc::clone(&barrier);
                        let component = component.clone();
                        let source = source.clone();
                        scope.spawn(move || {
                            barrier.wait();
                            prepare_with_cache(&component, &source, &excludes(), |_, path| {
                                counter.fetch_add(1, Ordering::SeqCst);
                                std::thread::sleep(Duration::from_millis(300));
                                fs::write(path.join("built.txt"), "built")
                                    .map_err(|err| Error::internal_io(err.to_string(), None))
                            })
                            .expect("prepare")
                            .outcome
                        })
                    })
                    .collect::<Vec<_>>();
                handles
                    .into_iter()
                    .map(|handle| handle.join().expect("thread"))
                    .collect::<Vec<_>>()
            });

            assert_eq!(counter.load(Ordering::SeqCst), 1);
            assert_eq!(
                outcomes
                    .iter()
                    .filter(|outcome| **outcome == CacheOutcome::Miss)
                    .count(),
                1
            );
            assert_eq!(
                outcomes
                    .iter()
                    .filter(|outcome| **outcome == CacheOutcome::Hit)
                    .count(),
                2
            );
        });
    }

    #[test]
    fn disabled_cache_always_builds() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let _disabled = EnvVarGuard::set(CACHE_DISABLE_ENV, "0");
            let parent = tempfile::tempdir().expect("parent");
            let (source, component) = checkout(parent.path(), "shared-runtime");
            let counter = Arc::new(AtomicUsize::new(0));

            for _ in 0..2 {
                let prepared =
                    prepare_with_cache(&component, &source, &excludes(), counting_build(&counter))
                        .expect("prepare");
                assert_eq!(prepared.outcome, CacheOutcome::Disabled);
                assert!(prepared.path.join("built.txt").exists());
            }
            assert_eq!(counter.load(Ordering::SeqCst), 2);
        });
    }

    #[test]
    fn eviction_keeps_newest_entries_per_component() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let parent = tempfile::tempdir().expect("parent");
            let (source, component) = checkout(parent.path(), "shared-runtime");
            let (other_source, other) = checkout(parent.path(), "other-runtime");
            let counter = Arc::new(AtomicUsize::new(0));

            prepare_with_cache(&other, &other_source, &excludes(), counting_build(&counter))
                .expect("other component");
            for revision in 0..(ENTRIES_PER_COMPONENT + 2) {
                fs::write(
                    source.join("lib/runtime.php"),
                    format!("<?php // {revision}\n"),
                )
                .expect("edit");
                git(&source, &["commit", "-q", "-am", "revision"]);
                prepare_with_cache(&component, &source, &excludes(), counting_build(&counter))
                    .expect("revision");
            }

            let entries = published_entries(&cache_root().unwrap());
            let own = entries
                .iter()
                .filter(|name| name.starts_with("shared-runtime-"))
                .count();
            assert_eq!(own, ENTRIES_PER_COMPONENT);
            assert!(
                entries
                    .iter()
                    .any(|name| name.starts_with("other-runtime-")),
                "other components are not trimmed by this component's churn"
            );
            // The newest revision is still a hit.
            let latest =
                prepare_with_cache(&component, &source, &excludes(), counting_build(&counter))
                    .expect("latest");
            assert_eq!(latest.outcome, CacheOutcome::Hit);
        });
    }

    #[test]
    fn non_git_sources_are_still_keyed_by_content() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let parent = tempfile::tempdir().expect("parent");
            let source = parent.path().join("plain");
            fs::create_dir_all(&source).expect("plain dir");
            fs::write(source.join("a.txt"), "one").expect("file");
            let component = Component {
                id: "plain".to_string(),
                ..Component::default()
            };
            let before = cache_key(&component, &source, &excludes()).expect("key");
            fs::write(source.join("a.txt"), "two").expect("edit");
            let after = cache_key(&component, &source, &excludes()).expect("key");
            assert_ne!(before, after);
        });
    }
}
