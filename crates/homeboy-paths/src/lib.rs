use homeboy_error::{Error, Result};
use std::env;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};

pub const HOMEBOY_DATA_DIR_ENV: &str = "HOMEBOY_DATA_DIR";
pub const CARGO_TARGETS_STORE: &str = "cargo-targets";
pub const CONTROLLER_RUNTIMES_STORE: &str = "controller-runtimes";
pub const CONTROLLER_SCRATCH_STORE: &str = "controller-scratch";

/// Immutable filesystem roots resolved at an application boundary.
///
/// Stateful services receive this value instead of repeatedly consulting
/// process-global environment and overrides. This lets independent in-process
/// workloads own distinct Homeboy state without serializing path resolution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PathRoots {
    config: PathBuf,
    data: PathBuf,
    artifacts: PathBuf,
}

impl PathRoots {
    pub fn new(config: PathBuf, data: PathBuf, artifacts: PathBuf) -> Self {
        Self {
            config,
            data,
            artifacts,
        }
    }

    /// Resolve the current process configuration once.
    pub fn from_environment() -> Result<Self> {
        Ok(Self::new(homeboy()?, homeboy_data()?, artifact_root()?))
    }

    pub fn config(&self) -> &Path {
        &self.config
    }

    pub fn data(&self) -> &Path {
        &self.data
    }

    pub fn artifacts(&self) -> &Path {
        &self.artifacts
    }
}

mod locations;
mod rigs;
#[allow(
    dead_code,
    reason = "Runtime path helpers support platform-specific and integration-test flows."
)]
mod runtime;

pub use locations::*;
pub use rigs::*;
pub use runtime::*;

fn artifact_root_override() -> &'static Mutex<Option<PathBuf>> {
    static OVERRIDE: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();
    OVERRIDE.get_or_init(|| Mutex::new(None))
}

fn home_root_override() -> &'static Mutex<Option<PathBuf>> {
    static OVERRIDE: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();
    OVERRIDE.get_or_init(|| Mutex::new(None))
}

/// Set a process-local home-root override that outranks `$HOME`.
///
/// `getenv`/`setenv` are not thread-safe. A caller that mutates `HOME` while
/// another thread resolves a path does not merely race on the *value* — the
/// reader can observe the variable as absent mid-write, and `homeboy()` then
/// fails with "HOME environment variable not set on Unix-like system" on a
/// machine where `HOME` is plainly set. Rust made `std::env::set_var` unsafe in
/// the 2024 edition for exactly this reason.
///
/// The hermetic test harness is the only mutator in practice: it repoints
/// `HOME` per test so each one owns its config, data, and artifact state.
/// Serializing those mutations behind a lock cannot fix it, because the
/// *readers* never take that lock — including worker threads a test spawns
/// inside itself, which is why serial execution alone left the failures in
/// place (#7505, #10097, #9774).
///
/// Routing the hot resolvers through a `Mutex` replaces an unsynchronized
/// `getenv` with a properly synchronized read, so a concurrent repoint is
/// ordered rather than torn. Environment remains the source of truth when no
/// override is registered, so subprocesses — which read their own environment
/// and are configured explicitly by the harness — are unaffected.
pub fn set_home_root_override(path: Option<PathBuf>) {
    let mut guard = home_root_override()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = path;
}

/// Resolved home root: the process-local override when set, else `$HOME`.
#[cfg(not(windows))]
fn resolved_home_root() -> Result<PathBuf> {
    let override_path = {
        let guard = home_root_override()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.clone()
    };
    if let Some(path) = override_path {
        return Ok(path);
    }
    env::var("HOME").map(PathBuf::from).map_err(|_| {
        Error::internal_unexpected(
            "HOME environment variable not set on Unix-like system".to_string(),
        )
    })
}

/// Set a process-local artifact root override.
///
/// This is intentionally process-scoped so CLI flags can outrank environment
/// and config without mutating global config or environment variables.
pub fn set_artifact_root_override(path: Option<PathBuf>) {
    let mut guard = artifact_root_override()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = path;
}

/// Resolver hook for the config-level `artifact_root` value.
///
/// `paths` is a foundation layer and must not depend on the config/`defaults`
/// layer (that would create a paths <-> defaults dependency cycle, blocking the
/// crate split). Instead, the config layer registers a resolver here at startup
/// via [`set_config_artifact_root_resolver`]; `artifact_root()` calls it to
/// honor the global config override without an inbound dependency on `defaults`.
type ConfigArtifactRootResolver = fn() -> Option<String>;

fn config_artifact_root_resolver() -> &'static Mutex<Option<ConfigArtifactRootResolver>> {
    static RESOLVER: OnceLock<Mutex<Option<ConfigArtifactRootResolver>>> = OnceLock::new();
    RESOLVER.get_or_init(|| Mutex::new(None))
}

/// Register the resolver that supplies the config-level `artifact_root` override.
///
/// Called once during startup by the config layer. Keeps `paths` free of any
/// compile-time dependency on `defaults`.
pub fn set_config_artifact_root_resolver(resolver: ConfigArtifactRootResolver) {
    let mut guard = config_artifact_root_resolver()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = Some(resolver);
}

fn resolved_config_artifact_root() -> Option<String> {
    let resolver = {
        let guard = config_artifact_root_resolver()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard
    };
    resolver.and_then(|f| f())
}

/// Base product config directory (universal ~/.config/homeboy/ on all platforms)
pub fn homeboy() -> Result<PathBuf> {
    #[cfg(windows)]
    {
        let appdata = env::var("APPDATA").map_err(|_| {
            Error::internal_unexpected(
                "APPDATA environment variable not set on Windows".to_string(),
            )
        })?;
        Ok(PathBuf::from(appdata).join(homeboy_product_identity::PRODUCT_IDENTITY.config_dirname))
    }

    #[cfg(not(windows))]
    {
        Ok(resolved_home_root()?
            .join(".config")
            .join(homeboy_product_identity::PRODUCT_IDENTITY.config_dirname))
    }
}

/// Global product config file path
pub fn homeboy_json() -> Result<PathBuf> {
    Ok(homeboy_product_identity::PRODUCT_IDENTITY.config_file(homeboy()?))
}

/// Base Homeboy data directory for local observed state.
///
/// Config/spec files remain under `homeboy()` (`~/.config/homeboy`). This
/// directory is for machine-local observations such as the SQLite store.
pub fn homeboy_data() -> Result<PathBuf> {
    if let Ok(path) = env::var(HOMEBOY_DATA_DIR_ENV) {
        if !path.trim().is_empty() {
            return Ok(expand_tilde_path(path));
        }
    }

    #[cfg(windows)]
    {
        let base = env::var("LOCALAPPDATA")
            .or_else(|_| env::var("APPDATA"))
            .map_err(|_| {
                Error::internal_unexpected(
                    "LOCALAPPDATA or APPDATA environment variable not set on Windows".to_string(),
                )
            })?;
        Ok(PathBuf::from(base).join(homeboy_product_identity::PRODUCT_IDENTITY.data_dirname))
    }

    #[cfg(not(windows))]
    {
        // A registered override outranks `XDG_DATA_HOME` deliberately, and
        // reading it avoids a second unsynchronized `getenv` on this hot path.
        //
        // Note this branch is only reached when `HOMEBOY_DATA_DIR` is unset.
        // The test harness sets that variable to `<home>/data`, which the
        // check above short-circuits on — so under test the resolved data root
        // is NOT `<home>/.local/share/homeboy`. A test that hardcodes the XDG
        // layout instead of calling these functions will look in the wrong
        // place; `artifact_content_serves_encoded_artifact_store_locator` did
        // exactly that and failed on main until it was pointed at
        // `artifact_root()`.
        let override_home = {
            let guard = home_root_override()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            guard.clone()
        };
        if let Some(home) = override_home {
            return Ok(home
                .join(".local")
                .join("share")
                .join(homeboy_product_identity::PRODUCT_IDENTITY.data_dirname));
        }

        if let Ok(xdg_data_home) = env::var("XDG_DATA_HOME") {
            if !xdg_data_home.trim().is_empty() {
                return Ok(PathBuf::from(xdg_data_home)
                    .join(homeboy_product_identity::PRODUCT_IDENTITY.data_dirname));
            }
        }

        Ok(resolved_home_root()?
            .join(".local")
            .join("share")
            .join(homeboy_product_identity::PRODUCT_IDENTITY.data_dirname))
    }
}

/// Local SQLite observation-store path.
pub fn observation_db() -> Result<PathBuf> {
    Ok(homeboy_data()?.join("homeboy.sqlite"))
}

/// Resolve a named store below the Homeboy data root.
///
/// Storage owners use this rather than rebuilding paths so reporting and future
/// placement policy can refer to the same stable store identities.
pub fn homeboy_data_store(name: &str) -> Result<PathBuf> {
    Ok(homeboy_data()?.join(name))
}

pub fn cargo_targets_store() -> Result<PathBuf> {
    homeboy_data_store(CARGO_TARGETS_STORE)
}

pub fn controller_runtimes_store() -> Result<PathBuf> {
    homeboy_data_store(CONTROLLER_RUNTIMES_STORE)
}

pub fn controller_scratch_store() -> Result<PathBuf> {
    homeboy_data_store(CONTROLLER_SCRATCH_STORE)
}

/// Root directory for copied run artifacts.
///
/// Precedence:
/// 1. process-local CLI override (`homeboy --artifact-root <path>`)
/// 2. `HOMEBOY_ARTIFACT_ROOT`
/// 3. global config `/artifact_root`
/// 4. historical default: `<homeboy_data>/artifacts`
pub fn artifact_root() -> Result<PathBuf> {
    if let Some(path) = artifact_root_override()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
    {
        return Ok(expand_tilde_path(path));
    }

    if let Ok(path) = env::var("HOMEBOY_ARTIFACT_ROOT") {
        if !path.trim().is_empty() {
            return Ok(expand_tilde_path(path));
        }
    }

    if let Some(path) = resolved_config_artifact_root() {
        if !path.trim().is_empty() {
            return Ok(expand_tilde_path(path));
        }
    }

    homeboy_data_store("artifacts")
}

/// Expand a leading tilde in a local path.
pub fn expand_tilde_path(path: impl AsRef<Path>) -> PathBuf {
    let raw = path.as_ref().to_string_lossy();
    PathBuf::from(shellexpand::tilde(&raw).into_owned())
}

pub fn sanitize_path_segment(segment: &str) -> String {
    segment
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

/// Normalize a local path lexically without touching the filesystem.
///
/// This removes `.` segments, collapses internal `..` segments, and preserves
/// leading `..` segments for relative paths. Absolute paths do not escape above
/// their root. Use this before containment checks when the target path may not
/// exist yet.
pub fn normalize_local_path(path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    let mut prefix: Option<OsString> = None;
    let mut rooted = false;
    let mut segments: Vec<OsString> = Vec::new();

    for component in path.components() {
        match component {
            Component::Prefix(value) => {
                prefix = Some(value.as_os_str().to_os_string());
            }
            Component::RootDir => {
                rooted = true;
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if segments.last().is_some_and(|segment| segment != "..") {
                    segments.pop();
                } else if !rooted {
                    segments.push(OsString::from(".."));
                }
            }
            Component::Normal(value) => segments.push(value.to_os_string()),
        }
    }

    let mut normalized = PathBuf::new();
    if let Some(prefix) = prefix {
        normalized.push(prefix);
    }
    if rooted {
        normalized.push(std::path::MAIN_SEPARATOR.to_string());
    }
    for segment in segments {
        normalized.push(segment);
    }

    if normalized.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        normalized
    }
}

/// Resolve an existing local path to its filesystem identity, falling back to
/// lexical normalization when the path does not exist yet.
///
/// This makes aliases such as symlinked roots and macOS's `/var` ->
/// `/private/var` resolve to one identity without conflating distinct paths
/// that cannot be canonicalized.
pub fn canonical_or_normalized_local_path(path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    std::fs::canonicalize(path).unwrap_or_else(|_| normalize_local_path(path))
}

/// Render each path component as a lossy UTF-8 string, in order.
///
/// Centralizes the `path.components()` + `as_os_str().to_string_lossy()`
/// traversal so component-walking helpers (temp-marker detection, case
/// insensitive comparison, etc.) share one primitive instead of each
/// reimplementing the same iteration.
pub fn path_component_strings(path: &Path) -> Vec<String> {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect()
}

/// Return whether `path` is inside `root` after lexical normalization.
pub fn local_path_is_contained(root: impl AsRef<Path>, path: impl AsRef<Path>) -> bool {
    let root = normalize_local_path(root);
    let path = normalize_local_path(path);

    path == root || path.starts_with(root)
}

/// Resolve a local path against a root and reject paths that escape that root.
///
/// Relative candidates are resolved below `root`. Absolute candidates are
/// accepted only when they already point inside `root` after lexical
/// normalization.
pub fn resolve_contained_local_path(
    root: impl AsRef<Path>,
    candidate: impl AsRef<Path>,
    field: &str,
) -> Result<PathBuf> {
    let root = normalize_local_path(root);
    let candidate = candidate.as_ref();
    let resolved = if candidate.is_absolute() {
        normalize_local_path(candidate)
    } else {
        normalize_local_path(root.join(candidate))
    };

    if local_path_is_contained(&root, &resolved) {
        Ok(resolved)
    } else {
        Err(Error::validation_invalid_argument(
            field,
            format!(
                "Path '{}' escapes root '{}'",
                candidate.display(),
                root.display()
            ),
            Some(candidate.to_string_lossy().to_string()),
            Some(vec![
                "Use a relative path inside the configured root".to_string(),
                "Use an absolute path that starts inside the configured root".to_string(),
            ]),
        ))
    }
}

/// Extension directory path
pub fn extension(id: &str) -> Result<PathBuf> {
    Ok(extensions()?.join(id))
}

/// Extension manifest file path
pub fn extension_manifest(id: &str) -> Result<PathBuf> {
    Ok(extensions()?.join(id).join(format!("{}.json", id)))
}

/// Agent runtime manifest file path
pub fn agent_runtime_manifest(id: &str) -> Result<PathBuf> {
    Ok(agent_runtimes()?.join(id).join(format!("{}.json", id)))
}

/// Key file path
pub fn key(server_id: &str) -> Result<PathBuf> {
    Ok(keys()?.join(format!("{}_id_rsa", server_id)))
}

/// Resolve path that may be absolute or relative to base.
pub fn resolve_path(base: &str, file: &str) -> PathBuf {
    if file.starts_with('/') {
        PathBuf::from(file)
    } else {
        PathBuf::from(base).join(file)
    }
}

/// Resolve path and return as String.
pub fn resolve_path_string(base: &str, file: &str) -> String {
    resolve_path(base, file).to_string_lossy().to_string()
}

pub fn resolve_optional_base_path(base_path: Option<&str>) -> Option<&str> {
    base_path.and_then(|value| (!value.trim().is_empty()).then_some(value.trim()))
}

pub fn join_remote_path(base_path: Option<&str>, path: &str) -> Result<String> {
    let path = path.trim();

    if path.is_empty() {
        return Err(Error::validation_invalid_argument(
            "path",
            "Path cannot be empty",
            None,
            None,
        ));
    }

    if path.starts_with('/') {
        return Ok(path.to_string());
    }

    let Some(base) = resolve_optional_base_path(base_path) else {
        return Err(Error::config_missing_key("base_path", None));
    };

    if base.ends_with('/') {
        Ok(format!("{}{}", base, path))
    } else {
        Ok(format!("{}/{}", base, path))
    }
}

pub fn join_remote_child(base_path: Option<&str>, dir: &str, child: &str) -> Result<String> {
    let dir_path = join_remote_path(base_path, dir)?;
    let child = child.trim();

    if child.is_empty() {
        return Err(Error::validation_invalid_argument(
            "child",
            "Child path cannot be empty",
            None,
            None,
        ));
    }

    if dir_path.ends_with('/') {
        Ok(format!("{}{}", dir_path, child))
    } else {
        Ok(format!("{}/{}", dir_path, child))
    }
}

/// The lexical authorization failure for a runner-side path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemotePathAuthorizationError {
    NotAbsolute,
    ContainsParentDir,
    OutsideAllowedRoots,
}

/// The containment semantics selected by a runner artifact caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemotePathRootContainment {
    RemoteString,
    NativePath,
}

/// Normalize a remote root without accessing the remote filesystem.
pub fn normalize_remote_root(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Return whether an absolute remote path is contained by a remote root.
pub fn remote_path_is_within_root(path: &str, root: &str) -> bool {
    let root = normalize_remote_root(root);
    path == root || path.starts_with(&format!("{root}/"))
}

/// Validate lexical authorization for a runner-side artifact path.
///
/// This deliberately does not canonicalize filesystem paths: callers use it
/// for remote path policy, while deletion paths enforce stronger symlink-safe
/// invariants separately.
pub fn authorize_remote_artifact_path(
    path: &Path,
    allowed_roots: &[String],
    containment: RemotePathRootContainment,
) -> std::result::Result<(), RemotePathAuthorizationError> {
    if !path.is_absolute() {
        return Err(RemotePathAuthorizationError::NotAbsolute);
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(RemotePathAuthorizationError::ContainsParentDir);
    }
    if allowed_roots.iter().any(|root| match containment {
        RemotePathRootContainment::RemoteString => {
            remote_path_is_within_root(&path.display().to_string(), root)
        }
        RemotePathRootContainment::NativePath => path == Path::new(root) || path.starts_with(root),
    }) {
        Ok(())
    } else {
        Err(RemotePathAuthorizationError::OutsideAllowedRoots)
    }
}

/// Fails closed when a crate on disk is not enumerated as a workspace member.
///
/// The root manifest globs members as `crates/homeboy-*` and
/// `crates/contracts/homeboy-*` rather than a bare `crates/*`, because a bare
/// glob also matches the `crates/contracts` grouping directory (not itself a
/// crate) and cargo then fails to load its manifest. The historical workaround
/// was `exclude = ["crates/contracts"]`, but cargo's `exclude` matches by path
/// PREFIX and outranks the members globs -- it silently dropped all 18
/// `crates/contracts/*` crates out of the workspace. Nothing warned: they still
/// compiled as path dependencies, so `cargo build` was green while
/// `--workspace` selection (test, clippy, `check --tests`) never saw them and
/// their unit tests never ran (#10550).
///
/// The prefix globs remove the need for `exclude`, but they trade one silent
/// failure for another: a crate added under `crates/` without the `homeboy-`
/// prefix would be just as invisible. This test closes that hole by comparing
/// the manifests on disk against the members cargo actually resolved.
#[cfg(test)]
mod workspace_membership {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    fn workspace_root() -> PathBuf {
        // crates/homeboy-paths -> crates -> <root>
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("homeboy-paths should live at <root>/crates/homeboy-paths")
            .to_path_buf()
    }

    /// Every directory under `dir` that contains a `Cargo.toml`.
    fn crate_dirs(dir: &Path) -> BTreeSet<String> {
        let mut found = BTreeSet::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return found;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.join("Cargo.toml").is_file() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    found.insert(name.to_string());
                }
            }
        }
        found
    }

    #[test]
    fn workspace_membership_is_complete() {
        let root = workspace_root();
        let manifest = std::fs::read_to_string(root.join("Cargo.toml")).expect("root manifest");

        // Guard the mechanism itself: `exclude` outranks the members globs, so
        // reintroducing it silently re-orphans crates.
        assert!(
            !manifest.contains("\nexclude = [\"crates/contracts\"]"),
            "root manifest must not exclude `crates/contracts`: cargo's exclude \
             matches by path prefix and would drop every crates/contracts/* \
             member from the workspace (#10550)"
        );

        for (dir, glob) in [
            (root.join("crates"), "crates/homeboy-*"),
            (
                root.join("crates").join("contracts"),
                "crates/contracts/homeboy-*",
            ),
        ] {
            for name in crate_dirs(&dir) {
                assert!(
                    name.starts_with("homeboy-"),
                    "crate `{}` in {} is not matched by the `{}` members glob, so it \
                     is invisible to every `--workspace` gate (test, clippy, \
                     `check --tests`). Rename it with the `homeboy-` prefix or widen \
                     the glob in the root Cargo.toml.",
                    name,
                    dir.display(),
                    glob
                );
            }
        }
    }
}

/// Fails closed when a source file stops being reachable from a crate root by
/// declarations rustfmt can follow.
///
/// rustfmt builds the module tree by *parsing*. It follows `mod name;` and
/// `#[path = "..."] mod name;`, and it does not expand macros. So a module
/// declared inside a `macro_rules!` body, or spliced in with `include!`, still
/// compiles and still runs its tests -- while `cargo fmt --all` never visits
/// it. `cargo fmt --all --check` then passes over files it has never read, and
/// formatting drift accumulates behind a green gate.
///
/// That is not hypothetical. `commands/mod.rs` declared the fifteen ops-family
/// command modules with `$(pub mod $module;)*` inside a `macro_rules!` body.
/// Thirty-three files -- those fifteen subtrees plus the four
/// `tests/commands/*.rs` files `#[path]`-mounted only from inside them -- were
/// invisible to the formatter, and had collected 36 rustfmt diff blocks of
/// drift by the time it was noticed (#10298). `runtime_helper.rs` hid another
/// 1,073-line file the same way with `include!`.
///
/// Measured with a probe rather than inferred: append a deliberately
/// misformatted function to all 1,819 `.rs` files, run `cargo fmt --all`, and
/// see which copies survive verbatim. 44 did. Six were the deliberately
/// standalone `tests/fixtures/audit_runtime` corpus; the rest are the two
/// mechanisms above plus four files in no module tree at all.
///
/// Note the `#[path]` attribute itself is *not* a blind spot -- rustfmt,
/// rustc, and clippy all follow it. Only macro expansion hides modules.
///
/// The reachability walk below deliberately fails **open** on ambiguity: a
/// `mod` declaration whose target file does not exist is ignored rather than
/// reported, so a `mod x;` appearing inside a multi-line raw string can only
/// ever make this test more permissive, never spuriously red.
#[cfg(test)]
mod formatter_visibility {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    /// Files that are in no module tree at all: rustc never compiles them, so
    /// they are dead weight that also confuses filesystem-based tooling.
    ///
    /// Both are left in place deliberately, not overlooked:
    ///
    /// - `structural_tests.rs` lives in `homeboy-code-audit`, which is being
    ///   restructured under #10557/#10558.
    /// - `decompose_test.rs` is carried in the `homeboy.json` audit baseline
    ///   (`test_quality::tests/core/refactor/decompose_test.rs::VacuousTest`) --
    ///   the audit corpus walks the filesystem, so it scans and reports on a
    ///   file the compiler never sees. Deleting it requires a baseline edit,
    ///   and `homeboy.json` is owned by the same in-flight work.
    ///
    /// This list is asserted as an exact set in both directions, so fixing one
    /// of these forces its removal here rather than letting the list rot.
    const KNOWN_ORPHANS: &[&str] = &[
        "crates/homeboy-code-audit/src/structural_tests.rs",
        "tests/core/refactor/decompose_test.rs",
    ];

    /// Not a crate: a fixture corpus the audit detectors scan as *data*. It is
    /// correctly unreachable from any crate root.
    const FIXTURE_PREFIX: &str = "tests/fixtures/";

    fn workspace_root() -> PathBuf {
        // crates/homeboy-paths -> crates -> <root>
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("homeboy-paths should live at <root>/crates/homeboy-paths")
            .to_path_buf()
    }

    /// Blank out string literals and line comments so a `mod x;` that only
    /// appears inside a literal or a comment is not read as a declaration.
    ///
    /// Brace *character* literals are dropped too. They are rare but they would
    /// otherwise unbalance the brace counter that tracks inline `mod x { .. }`
    /// nesting. A lifetime (`'a`) is left alone: it has no closing quote, so it
    /// is not matched by the three-character form checked here.
    fn declutter(line: &str) -> String {
        let bytes: Vec<char> = line.chars().collect();
        let mut out = String::with_capacity(line.len());
        let mut i = 0;
        while i < bytes.len() {
            let c = bytes[i];
            if c == '/' && i + 1 < bytes.len() && bytes[i + 1] == '/' {
                break;
            }
            if c == '\'' && i + 2 < bytes.len() && bytes[i + 2] == '\'' && bytes[i + 1] != '\\' {
                // `'{'`, `'}'`, `'x'` -- consume the whole literal.
                i += 3;
                continue;
            }
            if c == '\'' && i + 3 < bytes.len() && bytes[i + 1] == '\\' && bytes[i + 3] == '\'' {
                // `'\n'`, `'\\'` -- consume the whole escape literal.
                i += 4;
                continue;
            }
            if c == 'r' && i + 1 < bytes.len() && (bytes[i + 1] == '"' || bytes[i + 1] == '#') {
                // Raw string: r"..." or r#"..."#. Skip to the matching close.
                let mut hashes = 0;
                let mut j = i + 1;
                while j < bytes.len() && bytes[j] == '#' {
                    hashes += 1;
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == '"' {
                    j += 1;
                    while j < bytes.len() {
                        if bytes[j] == '"' {
                            let mut k = j + 1;
                            let mut seen = 0;
                            while k < bytes.len() && bytes[k] == '#' && seen < hashes {
                                seen += 1;
                                k += 1;
                            }
                            if seen == hashes {
                                j = k;
                                break;
                            }
                        }
                        j += 1;
                    }
                    i = j;
                    continue;
                }
            }
            if c == '"' {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == '\\' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == '"' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                continue;
            }
            out.push(c);
            i += 1;
        }
        out
    }

    /// The name in a `mod <name>;` declaration. Accepts a `$` prefix so macro
    /// bodies (`mod $module;`) are recognised too.
    fn mod_decl_name(line: &str) -> Option<String> {
        let mut rest = line;
        loop {
            let at = rest.find("mod ")?;
            let before_ok = at == 0
                || !rest[..at]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_');
            let tail = &rest[at + 4..];
            if before_ok {
                let trimmed = tail.trim_start();
                let body = trimmed.strip_prefix('$').unwrap_or(trimmed);
                let name: String = body
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if !name.is_empty() && body[name.len()..].trim_start().starts_with(';') {
                    return Some(name);
                }
            }
            rest = tail;
        }
    }

    /// The literal in `#[path = "..."]`.
    fn path_attr_value(line: &str) -> Option<String> {
        let at = line.find("#[path")?;
        let after = &line[at..];
        let eq = after.find('=')?;
        let open = after[eq..].find('"')? + eq;
        let close = after[open + 1..].find('"')? + open + 1;
        Some(after[open + 1..close].to_string())
    }

    #[test]
    fn path_attr_value_preserves_the_first_path_character() {
        assert_eq!(
            path_attr_value(r#"#[path = "deps_provider.rs"]"#).as_deref(),
            Some("deps_provider.rs")
        );
    }

    /// The literal in `include!("...")`. `include!(concat!(env!("OUT_DIR"), ..))`
    /// is build-script output, not a checked-in module, and does not match.
    ///
    /// Takes the undecluttered line because the argument it needs *is* a string
    /// literal, so it re-checks the decluttered form to reject an `include!`
    /// that only appears inside one (the audit detectors carry such literals as
    /// template text).
    fn include_value(line: &str) -> Option<String> {
        if !declutter(line).contains("include!(") {
            return None;
        }
        let at = line.find("include!(")?;
        let after = &line[at + "include!(".len()..];
        let rest = after.trim_start();
        let rest = rest.strip_prefix('"')?;
        let close = rest.find('"')?;
        Some(rest[..close].to_string())
    }

    fn is_mod_rs(path: &Path) -> bool {
        matches!(
            path.file_name().and_then(|n| n.to_str()),
            Some("lib.rs") | Some("main.rs") | Some("mod.rs")
        )
    }

    /// `base` with one directory component appended per enclosing inline
    /// `mod x { .. }` block, which is how rustc resolves modules declared
    /// inside them.
    fn nested_dir(base: &Path, inline: &[(i32, String)]) -> PathBuf {
        let mut dir = base.to_path_buf();
        for (_, name) in inline {
            dir.push(name);
        }
        dir
    }

    /// Files this source file pulls into the module tree, as
    /// `(path, behaves_like_mod_rs)`.
    ///
    /// `base_dir` is the directory a plain `mod x;` resolves against: the
    /// file's own directory for mod-rs files and crate roots, and
    /// `<dir>/<stem>/` for every other file.
    fn children(file: &Path, base_dir: &Path) -> Vec<(PathBuf, bool)> {
        let Ok(source) = std::fs::read_to_string(file) else {
            return Vec::new();
        };
        let file_dir = file.parent().unwrap_or(Path::new(".")).to_path_buf();
        let mut out = Vec::new();
        // (brace depth at open, module name) for enclosing inline `mod x { .. }`.
        let mut inline: Vec<(i32, String)> = Vec::new();
        let mut depth: i32 = 0;
        let mut pending_path: Option<String> = None;

        for raw in source.lines() {
            let code = raw.find("//").map_or(raw, |i| &raw[..i]);
            let clean = declutter(raw);

            if let Some(rel) = include_value(code) {
                // `include!` is textual: relative to the including file.
                out.push((file_dir.join(rel), true));
            }

            let attr = path_attr_value(code);
            let decl = mod_decl_name(&clean);
            // A `#[path]` outside an inline `mod` block is relative to the
            // directory the source file is in; inside one, it is relative to
            // the inline module's directory.
            let anchor = if inline.is_empty() {
                file_dir.clone()
            } else {
                nested_dir(base_dir, &inline)
            };

            if let Some(value) = attr {
                let attr_at = code.find("#[path").unwrap_or(0);
                if mod_decl_name(&declutter(&code[attr_at..])).is_some()
                    && code.trim_end().ends_with(';')
                {
                    out.push((anchor.join(value), true));
                } else {
                    pending_path = Some(value);
                }
            } else if let Some(name) = decl {
                if let Some(value) = pending_path.take() {
                    out.push((anchor.join(value), true));
                } else {
                    // A plain `mod x;` resolves under the inline nesting, which
                    // is `base_dir` itself at the top level.
                    let dir = nested_dir(base_dir, &inline);
                    let flat = dir.join(format!("{name}.rs"));
                    let nested = dir.join(&name).join("mod.rs");
                    if flat.is_file() {
                        out.push((flat, false));
                    } else if nested.is_file() {
                        out.push((nested, true));
                    }
                    // A target that exists nowhere is ignored on purpose: see
                    // the fail-open note on the module doc comment.
                }
            } else if !clean.trim().is_empty() && !clean.trim().starts_with('#') {
                pending_path = None;
            }

            let mut search = clean.as_str();
            while let Some(at) = search.find("mod ") {
                let tail = &search[at + 4..];
                let trimmed = tail.trim_start();
                let name: String = trimmed
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if !name.is_empty() && trimmed[name.len()..].trim_start().starts_with('{') {
                    inline.push((depth, name));
                }
                search = tail;
            }
            for c in clean.chars() {
                if c == '{' {
                    depth += 1;
                } else if c == '}' {
                    depth -= 1;
                    while inline.last().is_some_and(|(d, _)| *d >= depth) {
                        inline.pop();
                    }
                }
            }
        }
        out
    }

    fn crate_dirs(root: &Path) -> Vec<PathBuf> {
        let mut dirs = vec![root.to_path_buf()];
        for base in [root.join("crates"), root.join("crates").join("contracts")] {
            let Ok(entries) = std::fs::read_dir(&base) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.join("Cargo.toml").is_file() {
                    dirs.push(path);
                }
            }
        }
        dirs.sort();
        dirs
    }

    /// Cargo target roots: lib/bin/build entry points plus the auto-discovered
    /// `tests/*.rs`, `benches/*.rs`, and `examples/*.rs` at the top of those
    /// directories. All of them behave like mod-rs files.
    fn target_roots(crate_dir: &Path) -> Vec<PathBuf> {
        let mut roots = Vec::new();
        for rel in ["src/lib.rs", "src/main.rs", "build.rs"] {
            let path = crate_dir.join(rel);
            if path.is_file() {
                roots.push(path);
            }
        }
        for sub in ["src/bin", "tests", "benches", "examples"] {
            let Ok(entries) = std::fs::read_dir(crate_dir.join(sub)) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() && path.extension().is_some_and(|e| e == "rs") {
                    roots.push(path);
                }
            }
        }
        roots
    }

    fn rust_files_under(dir: &Path, out: &mut BTreeSet<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name == "target" || name == ".git" {
                    continue;
                }
                rust_files_under(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.insert(path);
            }
        }
    }

    fn reachable(root: &Path) -> BTreeSet<PathBuf> {
        let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
        let mut stack: Vec<(PathBuf, bool)> = crate_dirs(root)
            .into_iter()
            .flat_map(|dir| target_roots(&dir))
            .map(|path| (path, true))
            .collect();

        while let Some((file, mod_rs_like)) = stack.pop() {
            if !file.is_file() || !seen.insert(file.clone()) {
                continue;
            }
            let dir = file.parent().unwrap_or(Path::new(".")).to_path_buf();
            let base_dir = if mod_rs_like {
                dir
            } else {
                let stem = file
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_string();
                dir.join(stem)
            };
            for (child, child_is_mod_rs_like) in children(&file, &base_dir) {
                let child = normalize(&child);
                if !seen.contains(&child) {
                    let mod_rs_like = child_is_mod_rs_like || is_mod_rs(&child);
                    stack.push((child, mod_rs_like));
                }
            }
        }
        seen
    }

    /// Resolve `..` segments lexically. The paths are all built from real
    /// directory names, so this is safe without touching the filesystem.
    fn normalize(path: &Path) -> PathBuf {
        let mut out = PathBuf::new();
        for component in path.components() {
            match component {
                std::path::Component::ParentDir => {
                    out.pop();
                }
                std::path::Component::CurDir => {}
                other => out.push(other.as_os_str()),
            }
        }
        out
    }

    fn universe(root: &Path) -> BTreeSet<PathBuf> {
        let mut files = BTreeSet::new();
        for dir in crate_dirs(root) {
            for sub in ["src", "tests", "benches", "examples"] {
                rust_files_under(&dir.join(sub), &mut files);
            }
            let build = dir.join("build.rs");
            if build.is_file() {
                files.insert(build);
            }
        }
        files
            .into_iter()
            .filter(|path| {
                let rel = path.strip_prefix(root).unwrap_or(path).to_string_lossy();
                let rel = rel.replace('\\', "/");
                !rel.starts_with(FIXTURE_PREFIX) && !rel.contains(&format!("/{FIXTURE_PREFIX}"))
            })
            .collect()
    }

    /// rustfmt does not expand macros, so a `mod` declared inside a
    /// `macro_rules!` body is a module it never learns exists.
    #[test]
    fn no_modules_are_declared_inside_macro_bodies() {
        let root = workspace_root();
        let mut files = BTreeSet::new();
        for dir in crate_dirs(&root) {
            rust_files_under(&dir.join("src"), &mut files);
            rust_files_under(&dir.join("tests"), &mut files);
        }

        let mut offenders = Vec::new();
        for file in files {
            let Ok(source) = std::fs::read_to_string(&file) else {
                continue;
            };
            // `depth` is only meaningful while `in_macro` is set, and it is
            // reset to zero at every `macro_rules!` opener.
            let mut depth: i32 = 0;
            let mut in_macro = false;
            for (index, raw) in source.lines().enumerate() {
                let clean = declutter(raw);
                if !in_macro {
                    if !clean.contains("macro_rules!") {
                        continue;
                    }
                    in_macro = true;
                    depth = 0;
                }
                if mod_decl_name(&clean).is_some() {
                    offenders.push(format!(
                        "{}:{}: {}",
                        file.strip_prefix(&root).unwrap_or(&file).display(),
                        index + 1,
                        raw.trim()
                    ));
                }
                for c in clean.chars() {
                    if c == '{' {
                        depth += 1;
                    } else if c == '}' {
                        depth -= 1;
                        if depth <= 0 {
                            in_macro = false;
                            break;
                        }
                    }
                }
            }
        }

        assert!(
            offenders.is_empty(),
            "module declared inside a `macro_rules!` body:\n  {}\n\nrustfmt resolves the \
             module tree by parsing and does not expand macros, so `cargo fmt --all` would \
             never visit this module or anything below it, and `cargo fmt --all --check` \
             would pass over unformatted files (#10298). Declare the module with a literal \
             `mod` instead.",
            offenders.join("\n  ")
        );
    }

    /// `include!` is a macro too, so rustfmt cannot see through it either.
    #[test]
    fn no_rust_modules_are_pulled_in_with_include() {
        let root = workspace_root();
        let mut files = BTreeSet::new();
        for dir in crate_dirs(&root) {
            rust_files_under(&dir.join("src"), &mut files);
            rust_files_under(&dir.join("tests"), &mut files);
        }

        let mut offenders = Vec::new();
        for file in files {
            let Ok(source) = std::fs::read_to_string(&file) else {
                continue;
            };
            for (index, raw) in source.lines().enumerate() {
                let clean = raw.find("//").map_or(raw, |i| &raw[..i]);
                if let Some(value) = include_value(clean) {
                    if value.ends_with(".rs") {
                        offenders.push(format!(
                            "{}:{}: include!(\"{}\")",
                            file.strip_prefix(&root).unwrap_or(&file).display(),
                            index + 1,
                            value
                        ));
                    }
                }
            }
        }

        assert!(
            offenders.is_empty(),
            "Rust source spliced in with `include!`:\n  {}\n\nrustfmt does not expand \
             macros, so the included file compiles but is never formatted (#10298). Use \
             `#[path = \"...\"] mod name;` -- it produces the same module with the same \
             `super`, and rustfmt does follow `#[path]`.",
            offenders.join("\n  ")
        );
    }

    /// Every checked-in source file should be reachable from a cargo target
    /// root through declarations rustfmt can follow.
    #[test]
    fn every_source_file_is_reachable_from_a_crate_root() {
        let root = workspace_root();
        let reached = reachable(&root);
        let orphans: Vec<String> = universe(&root)
            .into_iter()
            .filter(|path| !reached.contains(path))
            .map(|path| {
                path.strip_prefix(&root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();

        let expected: BTreeSet<String> = KNOWN_ORPHANS.iter().map(|s| s.to_string()).collect();
        let actual: BTreeSet<String> = orphans.into_iter().collect();

        let unexpected: Vec<&String> = actual.difference(&expected).collect();
        assert!(
            unexpected.is_empty(),
            "source file reachable from no crate root:\n  {}\n\nrustc never compiles it, \
             `cargo fmt` never formats it, and clippy never lints it -- but filesystem-based \
             tooling (the audit corpus, grep, ownership reports) still counts it. Wire it \
             into the module tree or delete it.",
            unexpected
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("\n  ")
        );

        let fixed: Vec<&String> = expected.difference(&actual).collect();
        assert!(
            fixed.is_empty(),
            "KNOWN_ORPHANS lists files that are no longer orphaned:\n  {}\n\nRemove them \
             from the list so it cannot rot into a permanent exemption.",
            fixed
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("\n  ")
        );
    }
}

#[cfg(test)]
mod home_root_override_tests {
    use super::*;

    /// The override must beat `$HOME`, so a harness can repoint the home root
    /// without a `setenv` that other threads race.
    #[test]
    fn an_override_outranks_the_environment() {
        set_home_root_override(Some(PathBuf::from("/tmp/homeboy-override-root")));
        let resolved = homeboy().expect("config root");
        set_home_root_override(None);

        assert!(
            resolved.starts_with("/tmp/homeboy-override-root"),
            "override must win over $HOME, got {}",
            resolved.display()
        );
    }

    /// Clearing the override must fall back to the environment rather than
    /// stick at the last override — otherwise a dropped guard would leave every
    /// later resolution pointing at a deleted tempdir.
    #[test]
    fn clearing_the_override_restores_environment_resolution() {
        set_home_root_override(Some(PathBuf::from("/tmp/homeboy-override-root")));
        set_home_root_override(None);

        let resolved = homeboy().expect("config root");
        assert!(
            !resolved.starts_with("/tmp/homeboy-override-root"),
            "cleared override must not persist, got {}",
            resolved.display()
        );
    }

    /// The defect this exists for: threads spawned *inside* a test read the
    /// home root while the harness repoints it. `--test-threads=1` does not
    /// serialize those, and `getenv`/`setenv` are not thread-safe, so the
    /// reader could observe `HOME` as absent and fail with "HOME environment
    /// variable not set on Unix-like system" on a host where it is plainly set.
    ///
    /// Reading through the override is a `Mutex` read, so a concurrent repoint
    /// is ordered rather than torn. This asserts every reader resolves *some*
    /// valid root — never an error — while the root is repointed underneath.
    #[test]
    fn concurrent_readers_never_observe_a_missing_home_root() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let stop = Arc::new(AtomicBool::new(false));
        let readers: Vec<_> = (0..4)
            .map(|_| {
                let stop = Arc::clone(&stop);
                std::thread::spawn(move || {
                    let mut failures = 0usize;
                    while !stop.load(Ordering::SeqCst) {
                        if homeboy().is_err() || homeboy_data().is_err() {
                            failures += 1;
                        }
                    }
                    failures
                })
            })
            .collect();

        for i in 0..500 {
            set_home_root_override(Some(PathBuf::from(format!("/tmp/homeboy-root-{i}"))));
        }
        stop.store(true, Ordering::SeqCst);
        set_home_root_override(None);

        let failures: usize = readers.into_iter().map(|h| h.join().expect("reader")).sum();
        assert_eq!(
            failures, 0,
            "readers must never fail to resolve a home root while it is repointed"
        );
    }
}

#[cfg(test)]
mod path_roots_tests {
    use super::*;

    #[test]
    fn explicit_roots_are_available_without_environment() {
        let roots = PathRoots::new(
            PathBuf::from("/config/homeboy"),
            PathBuf::from("/data/homeboy"),
            PathBuf::from("/artifacts/homeboy"),
        );

        assert_eq!(roots.config(), Path::new("/config/homeboy"));
        assert_eq!(roots.data(), Path::new("/data/homeboy"));
        assert_eq!(roots.artifacts(), Path::new("/artifacts/homeboy"));
    }
}
