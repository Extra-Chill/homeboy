use crate::component::{
    inventory, inventory_in_root, load, load_in_root, portable::read_portable_config,
    try_discover_from_portable, Component,
};
use crate::error::{Error, Result};
use crate::git::run_git;
use homeboy_extension_contract::ExtensionCapability;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

// ============================================================================
// Config-root boundary (#7505)
// ============================================================================
//
// Component resolution mixes two kinds of input: facts about the invocation
// (CWD, git roots, `--path`, portable `homeboy.json` manifests) which have no
// config root to resolve, and reads of Homeboy's own registry (projects,
// standalone registrations, installed extension manifests) which do. Only the
// second kind flows through the helpers below.
//
// `config_root: None` means "this whole resolution is ambient"; `Some(root)`
// means "this whole resolution is rooted". It is never a per-read choice.

/// The registry inventory at the active boundary.
fn inventory_at(config_root: Option<&Path>) -> Result<Vec<Component>> {
    match config_root {
        Some(config_root) => inventory_in_root(config_root),
        None => inventory(),
    }
}

/// Registered-component lookup at the active boundary.
fn load_at(config_root: Option<&Path>, id: &str) -> Result<Component> {
    match config_root {
        Some(config_root) => load_in_root(config_root, id),
        None => load(id),
    }
}

/// Standalone machine-local fallbacks at the active boundary.
fn apply_standalone_fallbacks_at(config_root: Option<&Path>, component: &mut Component) {
    match config_root {
        Some(config_root) => {
            crate::project::component::resolution::apply_standalone_component_fallbacks_in_root(
                config_root,
                component,
                None,
            )
        }
        None => crate::project::component::resolution::apply_standalone_component_fallbacks(
            component, None,
        ),
    }
}

/// Extension-driven `remote_path` inference at the active boundary.
fn resolve_remote_path_at(config_root: Option<&Path>, component: &mut Component) {
    match config_root {
        Some(config_root) => crate::component::resolve_remote_path_in_root(config_root, component),
        None => crate::component::resolve_remote_path(component),
    }
}

/// Shared target-resolution input for component/path-oriented commands.
///
/// This is the single contract for commands that need to turn user input into
/// an effective component and source path. Callers can keep command-specific
/// validation before this point, then rely on the same resolution order for
/// registered components, `--path`, bare directories, CWD discovery, project
/// scope, synthetic targets, and optional capability checks.
#[derive(Debug, Clone, Default)]
pub struct TargetSpec<'a> {
    /// Positional or flagged component ID. May also be a bare directory when
    /// `accept_bare_directory` is enabled.
    pub component_id: Option<&'a str>,

    /// Explicit `--path` override.
    pub path_override: Option<&'a str>,

    /// Optional project scope for project-attached component lookup.
    pub project: Option<&'a crate::project::Project>,

    /// Optional extension capability required by the caller.
    pub capability: Option<ExtensionCapability>,

    /// Whether an explicit path without registration/portable config may
    /// produce a synthetic component.
    pub allow_synthetic: bool,

    /// Whether a positional component value that is a directory is accepted as
    /// an ad-hoc target.
    pub accept_bare_directory: bool,

    /// Controls whether an ID-only target may fall back to the persisted
    /// component registry after CWD/portable discovery fails.
    pub registry_lookup: RegistryLookupPolicy,
}

impl<'a> TargetSpec<'a> {
    pub fn new(component_id: Option<&'a str>, path_override: Option<&'a str>) -> Self {
        Self {
            component_id,
            path_override,
            project: None,
            capability: None,
            allow_synthetic: true,
            accept_bare_directory: true,
            registry_lookup: RegistryLookupPolicy::Allow,
        }
    }
}

/// Registry lookup policy for component target resolution.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RegistryLookupPolicy {
    /// Allow ID-only targets to resolve through the standalone/project registry.
    #[default]
    Allow,

    /// Resolve only from explicit paths, bare directories, or the current
    /// checkout's portable config. Useful for commands that must not silently
    /// operate on a globally registered checkout.
    CwdOrPortableOnly,
}

/// Resolved target shared by git, audit, refactor, and execution context setup.
#[derive(Debug, Clone)]
pub struct ResolvedTarget {
    pub component: Component,
    pub component_id: String,
    pub source_path: PathBuf,
    pub git_root: Option<PathBuf>,
    pub extension_id: Option<String>,
    pub synthetic: bool,
}

/// Canonical identities that can help correct a rejected repository path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegisteredPathCandidates {
    pub repositories: Vec<String>,
    pub components: Vec<String>,
}

/// Typed result of classifying a filesystem target against persisted ownership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisteredPrimaryPathResolution {
    Primary(String),
    MissingPath,
    NonGitPath,
    UnregisteredRepository(RegisteredPathCandidates),
    StaleRegistry(RegisteredPathCandidates),
    AmbiguousNestedComponent(RegisteredPathCandidates),
}

/// Resolve a registered component slug or classify a repository path.
///
/// Exact persisted IDs win over coincidental relative filesystem entries. This
/// keeps a registered slug stable when the invocation directory happens to
/// contain a file or directory with the same name.
pub fn resolve_registered_primary_identity(input: &str) -> Result<RegisteredPrimaryPathResolution> {
    let components = crate::component::registered_base()?;
    if let Some(component) = components.iter().find(|component| component.id == input) {
        return Ok(RegisteredPrimaryPathResolution::Primary(
            component.id.clone(),
        ));
    }

    let expanded = shellexpand::tilde(input);
    let path = Path::new(expanded.as_ref());
    if path.is_absolute() || input.contains('/') || input.contains('\\') || path.exists() {
        return resolve_registered_primary_path_with_components(path, components);
    }

    let mut repositories = components
        .iter()
        .filter_map(|component| component.remote_url.as_deref())
        .filter_map(repository_name)
        .collect::<Vec<_>>();
    repositories.sort();
    repositories.dedup();
    let components = components
        .into_iter()
        .map(|component| component.id)
        .collect();
    Ok(RegisteredPrimaryPathResolution::UnregisteredRepository(
        RegisteredPathCandidates {
            repositories,
            components,
        },
    ))
}

/// Resolve a path to a registered primary component identity.
///
/// Resolution reads only persisted registrations and bounded Git metadata. It
/// never invokes Git or enriches component checkouts, so deterministic identity
/// failures cannot inherit a caller's planner timeout.
pub fn resolve_registered_primary_path(input: &str) -> Result<RegisteredPrimaryPathResolution> {
    let expanded = shellexpand::tilde(input);
    let components = crate::component::registered_base()?;
    resolve_registered_primary_path_with_components(Path::new(expanded.as_ref()), components)
}

fn resolve_registered_primary_path_with_components(
    path: &Path,
    components: Vec<Component>,
) -> Result<RegisteredPrimaryPathResolution> {
    let Ok(path) = path.canonicalize() else {
        return Ok(RegisteredPrimaryPathResolution::MissingPath);
    };
    let Some(input_git) = static_git_checkout(&path) else {
        return Ok(RegisteredPrimaryPathResolution::NonGitPath);
    };
    let mut primary = Vec::new();
    let mut nested = Vec::new();
    let mut remote_matches = Vec::new();
    let mut stale = Vec::new();

    for component in components {
        let registered_path = PathBuf::from(shellexpand::tilde(&component.local_path).into_owned());
        let Ok(registered_path) = registered_path.canonicalize() else {
            let configured_path = crate::paths::normalize_local_path(&registered_path);
            if configured_path.starts_with(&input_git.root)
                || remotes_match(input_git.remote.as_deref(), component.remote_url.as_deref())
            {
                stale.push(component.id);
            }
            continue;
        };
        if registered_path == path {
            primary.push(component.id);
            continue;
        }
        if let Some(registered_git) = static_git_checkout(&registered_path) {
            if registered_git.root == input_git.root {
                nested.push(component.id);
            } else if remotes_match(
                input_git.remote.as_deref(),
                registered_git.remote.as_deref(),
            ) {
                remote_matches.push(component.id);
            }
        }
    }

    primary.sort();
    primary.dedup();
    if primary.len() == 1 {
        return Ok(RegisteredPrimaryPathResolution::Primary(primary.remove(0)));
    }
    if !primary.is_empty() {
        return Ok(RegisteredPrimaryPathResolution::AmbiguousNestedComponent(
            path_candidates(&input_git, primary),
        ));
    }
    nested.sort();
    nested.dedup();
    if !nested.is_empty() {
        nested.extend(remote_matches);
        nested.extend(stale);
        nested.sort();
        nested.dedup();
        return Ok(RegisteredPrimaryPathResolution::AmbiguousNestedComponent(
            path_candidates(&input_git, nested),
        ));
    }
    remote_matches.sort();
    remote_matches.dedup();
    if !remote_matches.is_empty() {
        remote_matches.extend(stale);
        remote_matches.sort();
        remote_matches.dedup();
        return Ok(RegisteredPrimaryPathResolution::UnregisteredRepository(
            path_candidates(&input_git, remote_matches),
        ));
    }
    stale.sort();
    stale.dedup();
    if !stale.is_empty() {
        return Ok(RegisteredPrimaryPathResolution::StaleRegistry(
            path_candidates(&input_git, stale),
        ));
    }
    Ok(RegisteredPrimaryPathResolution::UnregisteredRepository(
        path_candidates(&input_git, Vec::new()),
    ))
}

const MAX_STATIC_GIT_METADATA_BYTES: u64 = 1024 * 1024;

struct StaticGitCheckout {
    root: PathBuf,
    remote: Option<String>,
}

fn static_git_checkout(path: &Path) -> Option<StaticGitCheckout> {
    let start = if path.is_dir() { path } else { path.parent()? };
    for root in start.ancestors() {
        let marker = root.join(".git");
        let Ok(metadata) = std::fs::metadata(&marker) else {
            continue;
        };
        let git_dir = if metadata.is_dir() {
            marker
        } else if metadata.is_file() && metadata.len() <= MAX_STATIC_GIT_METADATA_BYTES {
            let value = std::fs::read_to_string(&marker).ok()?;
            let git_dir = value.trim().strip_prefix("gitdir:")?.trim();
            let git_dir = PathBuf::from(git_dir);
            if git_dir.is_absolute() {
                git_dir
            } else {
                root.join(git_dir)
            }
        } else {
            return None;
        };
        if !git_dir.is_dir() || !git_dir.join("HEAD").is_file() {
            return None;
        }
        let common_dir = bounded_text_file(&git_dir.join("commondir"))
            .map(|value| {
                let common = PathBuf::from(value.trim());
                if common.is_absolute() {
                    common
                } else {
                    git_dir.join(common)
                }
            })
            .unwrap_or(git_dir);
        return Some(StaticGitCheckout {
            root: root.to_path_buf(),
            remote: bounded_text_file(&common_dir.join("config"))
                .and_then(|config| origin_url_from_git_config(&config)),
        });
    }
    None
}

fn bounded_text_file(path: &Path) -> Option<String> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_STATIC_GIT_METADATA_BYTES {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

fn origin_url_from_git_config(config: &str) -> Option<String> {
    let mut origin = false;
    for line in config.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            origin = line
                .to_ascii_lowercase()
                .strip_prefix("[remote")
                .and_then(|line| line.strip_suffix(']'))
                .is_some_and(|line| line.trim().trim_matches('"') == "origin");
        } else if origin {
            if let Some((key, value)) = line.split_once('=') {
                if key.trim().eq_ignore_ascii_case("url") {
                    return Some(value.trim().trim_matches('"').to_string());
                }
            }
        }
    }
    None
}

fn remotes_match(left: Option<&str>, right: Option<&str>) -> bool {
    left.zip(right)
        .is_some_and(|(left, right)| normalize_git_remote(left) == normalize_git_remote(right))
}

fn normalize_git_remote(remote: &str) -> String {
    remote
        .trim()
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .to_ascii_lowercase()
}

fn repository_name(remote: &str) -> Option<String> {
    let normalized = normalize_git_remote(remote);
    normalized
        .rsplit(['/', ':'])
        .next()
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

fn path_candidates(git: &StaticGitCheckout, components: Vec<String>) -> RegisteredPathCandidates {
    let repositories = git
        .remote
        .as_deref()
        .and_then(repository_name)
        .or_else(|| git.root.file_name()?.to_str().map(str::to_string))
        .into_iter()
        .collect();
    RegisteredPathCandidates {
        repositories,
        components,
    }
}

fn resolved_target_from_component(mut component: Component, synthetic: bool) -> ResolvedTarget {
    let source_path = PathBuf::from(shellexpand::tilde(&component.local_path).into_owned());
    let git_root = detect_git_root(&source_path);
    crate::component::resolve_remote_path(&mut component);

    ResolvedTarget {
        component_id: component.id.clone(),
        component,
        source_path,
        git_root,
        extension_id: None,
        synthetic,
    }
}

/// Resolve target path details from an already-authoritative component.
///
/// This preserves caller-supplied in-memory component fields while sharing the
/// same path expansion, git-root detection, and remote-path normalization used
/// by [`resolve_target`].
pub fn resolve_target_from_component(
    mut component: Component,
    path_override: Option<&str>,
) -> ResolvedTarget {
    if let Some(path) = path_override {
        component.local_path = path.to_string();
    }

    resolved_target_from_component(component, false)
}

/// Resolve the effective build artifact pattern for a component.
///
/// An explicit `build_artifact` always wins. Otherwise the pattern comes from
/// the component's linked extensions.
///
/// `Component.extensions` is a `HashMap` with `RandomState`, so its iteration
/// order differs on every process. A first-match-wins scan therefore resolved a
/// *different* artifact path on different runs of the same binary whenever two
/// linked extensions (e.g. `wordpress` + `nodejs`) both declared
/// `build.artifact_pattern` — a silent wrong-artifact deploy (#10281). Every
/// provider is collected instead, and the distinct rendered patterns decide:
///
/// - no provider → `Ok(None)`; the component is simply not artifact-producing
/// - one distinct pattern → `Ok(Some(pattern))`, however many extensions
///   declare it (agreeing extensions are not ambiguous)
/// - conflicting patterns → [`crate::extension_execution::disambiguate_capability_owner`],
///   the same ownership rule every other capability uses: explicit
///   `capability_extensions.build`, then `composition.includes` primacy, then a
///   hard ambiguity error the component author must resolve
///
/// Extensions that fail to load are skipped rather than failing resolution:
/// a component may reference an extension that is not installed locally, and
/// callers that require one (deploy planning) validate that separately.
pub fn resolve_artifact(component: &Component) -> Result<Option<String>> {
    resolve_artifact_core(None, component)
}

/// [`resolve_artifact`] against an already-resolved config root (#7505).
///
/// Both the artifact-pattern survey and the ownership tiebreak read extension
/// manifests from `config_root`, so a rooted caller cannot have its artifact
/// decided by whichever extensions are installed in the ambient home.
pub fn resolve_artifact_in_root(
    config_root: &Path,
    component: &Component,
) -> Result<Option<String>> {
    resolve_artifact_core(Some(config_root), component)
}

fn resolve_artifact_core(
    config_root: Option<&Path>,
    component: &Component,
) -> Result<Option<String>> {
    if let Some(ref artifact) = component.build_artifact {
        return Ok(Some(artifact.clone()));
    }

    let providers = artifact_pattern_providers_core(config_root, component);
    let distinct: BTreeSet<&str> = providers.values().map(String::as_str).collect();

    match distinct.len() {
        0 => Ok(None),
        1 => Ok(distinct.into_iter().next().map(ToOwned::to_owned)),
        _ => {
            let candidates: Vec<String> = providers.keys().cloned().collect();
            let owner = match config_root {
                Some(config_root) => {
                    crate::extension_execution::disambiguate_capability_owner_in_root(
                        config_root,
                        component,
                        ExtensionCapability::Build,
                        &candidates,
                    )?
                }
                None => crate::extension_execution::disambiguate_capability_owner(
                    component,
                    ExtensionCapability::Build,
                    &candidates,
                )?,
            };
            Ok(providers.get(&owner).cloned())
        }
    }
}

/// Linked extensions that declare a `build.artifact_pattern`, mapped to the
/// pattern rendered for this component.
///
/// Keyed by extension ID in a `BTreeMap` so the candidate list handed to
/// ambiguity resolution — and the error message listing it — is stable across
/// runs, unlike the component's own `HashMap` iteration order.
fn artifact_pattern_providers_core(
    config_root: Option<&Path>,
    component: &Component,
) -> BTreeMap<String, String> {
    let mut providers = BTreeMap::new();

    let Some(extensions) = component.extensions.as_ref() else {
        return providers;
    };

    for extension_id in extensions.keys() {
        let Ok(manifest) =
            crate::extension::catalog::load_extension_in_optional_root(config_root, extension_id)
        else {
            continue;
        };
        let Some(pattern) = manifest
            .build
            .as_ref()
            .and_then(|build| build.artifact_pattern.as_ref())
        else {
            continue;
        };

        providers.insert(
            extension_id.clone(),
            pattern
                .replace("{component_id}", &component.id)
                .replace("{local_path}", &component.local_path),
        );
    }

    providers
}

/// Validates a persisted component `local_path` is usable (absolute and exists).
pub fn validate_local_path(component: &Component) -> Result<PathBuf> {
    let expanded = shellexpand::tilde(&component.local_path);
    let path = PathBuf::from(expanded.as_ref());

    if !path.is_absolute() {
        return Err(Error::validation_invalid_argument(
            "local_path",
            format!(
                "Component '{}' configured local_path '{}' is relative and cannot be resolved. Use an absolute path like /Users/user/path/to/component",
                component.id, component.local_path
            ),
            Some(component.id.clone()),
            None,
        )
        .with_hint(format!(
            "Set absolute path: homeboy component set {} --local-path \"/full/path/to/{}\"",
            component.id, component.local_path
        ))
        .with_hint("Use 'pwd' in the component directory to get the absolute path".to_string()));
    }

    if !path.exists() {
        return Err(Error::validation_invalid_argument(
            "local_path",
            format!(
                "Component '{}' configured local_path does not exist: {}",
                component.id,
                path.display()
            ),
            Some(component.id.clone()),
            None,
        )
        .with_hint(format!("Verify the path exists: ls -la {}", path.display()))
        .with_hint(format!(
            "Update path: homeboy component set {} --local-path \"/correct/path\"",
            component.id
        )));
    }

    Ok(path)
}

/// Normalize a component `local_path` to an absolute, lexically-normalized
/// string, resolving relative values against `base`.
///
/// Tilde (`~`) is expanded. Absolute inputs are normalized in place (collapsing
/// `.`/`..` segments). Relative inputs (e.g. `php-transformer`) are resolved
/// against `base` — the workspace/current working directory in production — so
/// the stored value is always absolute and survives `release` resolution, which
/// rejects relative `local_path` values (see [`validate_local_path`]). An empty
/// value is returned unchanged so callers can decide how to treat it.
pub fn normalize_component_local_path_against(raw: &str, base: &Path) -> String {
    if raw.trim().is_empty() {
        return raw.to_string();
    }
    let expanded = shellexpand::tilde(raw);
    let candidate = Path::new(expanded.as_ref());
    let absolute = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        base.join(candidate)
    };
    crate::paths::normalize_local_path(absolute)
        .to_string_lossy()
        .into_owned()
}

/// Normalize a component `local_path` to absolute against the current working
/// directory. See [`normalize_component_local_path_against`].
pub fn normalize_component_local_path(raw: &str) -> Result<String> {
    let base = std::env::current_dir().map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some("resolve current directory for local_path normalization".to_string()),
        )
    })?;
    Ok(normalize_component_local_path_against(raw, &base))
}

/// Returns whether a (tilde-expanded) `local_path` value is relative.
pub fn local_path_is_relative(raw: &str) -> bool {
    if raw.trim().is_empty() {
        return false;
    }
    Path::new(shellexpand::tilde(raw).as_ref()).is_relative()
}

/// Detect component ID from current working directory.
fn detect_from_cwd(config_root: Option<&Path>) -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    let components = inventory_at(config_root).ok()?;

    for component in components {
        let expanded = shellexpand::tilde(&component.local_path);
        let local_path = Path::new(expanded.as_ref());

        if cwd.starts_with(local_path) {
            return Some(component.id);
        }
    }
    None
}

/// Check if the CWD (or its git root) is a checkout of the given component.
///
/// Returns the CWD-discovered component when the portable `homeboy.json` in the
/// current directory (or git root) has a matching `id`. This means the user is
/// standing inside a clone of this component and intends to operate on it,
/// even if the registered `local_path` points elsewhere (#694).
fn prefer_cwd_for_component(
    config_root: Option<&Path>,
    component_id: &str,
) -> Result<Option<Component>> {
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(_) => return Ok(None),
    };

    // A managed worktree commonly has its own portable manifest. Resolve its
    // repository identity before accepting that manifest directly so the
    // canonical registered contract remains available beneath explicit local
    // overrides.
    if let Some(component) =
        registered_component_for_worktree_path(config_root, &cwd, Some(component_id))?
    {
        return Ok(Some(component));
    }

    // Check CWD directly
    if let Some(mut discovered) = try_discover_from_portable(&cwd)? {
        if discovered.id == component_id {
            apply_standalone_fallbacks_at(config_root, &mut discovered);
            return Ok(Some(discovered));
        }
    }

    // Check git root if different from CWD
    if let Some(git_root) = detect_git_root(&cwd) {
        if git_root != cwd {
            if let Some(mut discovered) = try_discover_from_portable(&git_root)? {
                if discovered.id == component_id {
                    apply_standalone_fallbacks_at(config_root, &mut discovered);
                    return Ok(Some(discovered));
                }
            }
        }
    }

    let registered = match load_at(config_root, component_id) {
        Ok(registered) => registered,
        Err(_) => return Ok(None),
    };
    let registered_path = PathBuf::from(shellexpand::tilde(&registered.local_path).into_owned());
    let Some(cwd_git_root) = detect_git_root(&cwd) else {
        return Ok(None);
    };

    let remote_matches = crate::git::remote_origin_url(&cwd_git_root).is_some()
        && crate::git::remote_origin_url(&cwd_git_root)
            == crate::git::remote_origin_url(&registered_path);
    if same_git_common_dir(&registered_path, &cwd_git_root) || remote_matches {
        let checkout_path = rebase_registered_path_to_checkout(&registered_path, &cwd_git_root);
        return portable_component_for_checkout(
            config_root,
            component_id,
            &checkout_path,
            &registered,
        )
        .map(Some);
    }

    if is_named_component_worktree(component_id, &registered_path, &cwd_git_root) {
        return portable_component_for_checkout(
            config_root,
            component_id,
            &cwd_git_root,
            &registered,
        )
        .map(Some);
    }

    Ok(None)
}

/// Project a canonical component contract onto a distinct checkout.
///
/// The registered component owns machine-local extension, provider, and script
/// configuration. A worktree manifest owns only the fields it explicitly
/// declares, so it can intentionally override that contract without losing the
/// canonical defaults required to run the checkout.
fn portable_component_for_checkout(
    config_root: Option<&Path>,
    component_id: &str,
    checkout_path: &Path,
    registered: &Component,
) -> Result<Component> {
    let manifest_path = checkout_path.join("homeboy.json");
    let Some(discovered) = try_discover_from_portable(checkout_path)? else {
        // A registration without a primary portable manifest is legacy
        // machine-local metadata, not checkout-owned configuration. Keep that
        // behavior for legacy consumers; otherwise a target manifest is required.
        if try_discover_from_portable(Path::new(&registered.local_path))?.is_none() {
            let mut component = registered.clone();
            component.local_path = checkout_path.to_string_lossy().to_string();
            resolve_remote_path_at(config_root, &mut component);
            return Ok(component);
        }
        return Err(Error::validation_invalid_argument(
            "homeboy.json",
            format!(
                "Matched checkout for component '{}' has no portable manifest at {}",
                component_id,
                manifest_path.display()
            ),
            Some(checkout_path.to_string_lossy().to_string()),
            Some(vec![
                "Check out a revision containing homeboy.json or target a checkout with compatible component config".to_string(),
            ]),
        ));
    };

    if discovered.id != component_id {
        return Err(Error::validation_invalid_argument(
            "component_id",
            format!(
                "Matched checkout manifest at {} declares component id '{}' instead of '{}'",
                manifest_path.display(),
                discovered.id,
                component_id
            ),
            Some(component_id.to_string()),
            Some(vec![
                "Target a checkout whose homeboy.json has the requested component id".to_string(),
            ]),
        ));
    }

    let portable =
        read_portable_config(checkout_path)?.expect("portable discovery read the manifest");
    let mut component = overlay_portable_component_config(registered, portable)?;
    component.id = component_id.to_string();
    component.local_path = checkout_path.to_string_lossy().to_string();
    resolve_remote_path_at(config_root, &mut component);
    Ok(component)
}

/// Apply only manifest-declared values. Objects merge recursively so a checkout
/// can override one extension setting without copying the entire canonical
/// extension contract; arrays and scalars replace their canonical value.
fn overlay_portable_component_config(
    registered: &Component,
    mut portable: serde_json::Value,
) -> Result<Component> {
    let mut effective = serde_json::to_value(registered).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("serialize registered component".to_string()),
        )
    })?;
    normalize_portable_extension_settings(&mut portable);
    merge_json_objects(&mut effective, portable);
    serde_json::from_value(effective).map_err(|error| {
        Error::validation_invalid_json(
            error,
            Some("merge worktree homeboy.json with registered component config".to_string()),
            None,
        )
    })
}

/// `ScopedExtensionConfig` serializes settings as flat extension keys while
/// portable manifests may use a nested `settings` object. Normalize the latter
/// before overlaying while preserving the portable contract that flat keys win
/// over duplicate nested settings.
fn normalize_portable_extension_settings(portable: &mut serde_json::Value) {
    let Some(extensions) = portable
        .as_object_mut()
        .and_then(|component| component.get_mut("extensions"))
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };

    for extension in extensions.values_mut() {
        let Some(extension) = extension.as_object_mut() else {
            continue;
        };
        let Some(serde_json::Value::Object(settings)) = extension.remove("settings") else {
            continue;
        };
        for (key, value) in settings {
            extension.entry(key).or_insert(value);
        }
    }
}

fn merge_json_objects(base: &mut serde_json::Value, overlay: serde_json::Value) {
    match (base, overlay) {
        (serde_json::Value::Object(base), serde_json::Value::Object(overlay)) => {
            for (key, value) in overlay {
                match base.get_mut(&key) {
                    Some(existing) => merge_json_objects(existing, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (base, overlay) => *base = overlay,
    }
}

fn rebase_registered_path_to_checkout(registered_path: &Path, cwd_git_root: &Path) -> PathBuf {
    let Some(registered_git_root) = detect_git_root(registered_path) else {
        return cwd_git_root.to_path_buf();
    };

    let canonical_registered_path = registered_path
        .canonicalize()
        .unwrap_or_else(|_| registered_path.to_path_buf());
    let canonical_registered_root = registered_git_root
        .canonicalize()
        .unwrap_or(registered_git_root);

    match canonical_registered_path.strip_prefix(&canonical_registered_root) {
        Ok(relative) if !relative.as_os_str().is_empty() => cwd_git_root.join(relative),
        _ => cwd_git_root.to_path_buf(),
    }
}

/// Project a registered component path into a checkout of its repository.
///
/// A component can be registered at a package below the Git root while Cook's
/// managed worktree is necessarily the repository root. Keeping this mapping
/// here makes provider, dependency, and gate callers share the same component
/// path contract.
pub fn rebase_component_path_to_checkout(component: &Component, checkout: &Path) -> PathBuf {
    rebase_registered_path_to_checkout(Path::new(&component.local_path), checkout)
}

fn is_named_component_worktree(
    component_id: &str,
    registered_path: &Path,
    cwd_git_root: &Path,
) -> bool {
    let Some(worktree_name) = cwd_git_root.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if !worktree_name.starts_with(&format!("{component_id}@")) {
        return false;
    }
    if registered_path.file_name().and_then(|name| name.to_str()) != Some(component_id) {
        return false;
    }

    match (registered_path.parent(), cwd_git_root.parent()) {
        (Some(registered_parent), Some(worktree_parent)) => {
            registered_parent == worktree_parent
                || registered_parent.canonicalize().ok() == worktree_parent.canonicalize().ok()
        }
        _ => false,
    }
}

fn same_git_common_dir(a: &Path, b: &Path) -> bool {
    match (git_common_dir(a), git_common_dir(b)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

fn git_common_dir(dir: &Path) -> Option<PathBuf> {
    let raw = run_git(dir, &["rev-parse", "--git-common-dir"], "git common dir")
        .ok()?
        .trim()
        .to_string();
    if raw.is_empty() {
        return None;
    }

    let path = PathBuf::from(raw);
    let absolute = if path.is_absolute() {
        path
    } else {
        dir.join(path)
    };
    absolute.canonicalize().ok()
}

fn synthetic_component_for_path(path: &str) -> Component {
    let path_ref = Path::new(path);
    let id_source = path_ref
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(path));

    let id = id_source
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "unknown".to_string());

    Component {
        id,
        local_path: path.to_string(),
        ..Component::default()
    }
}

fn resolve_path_override(config_root: Option<&Path>, path: &str) -> Result<Component> {
    if let Some(component) =
        registered_component_for_worktree_path(config_root, Path::new(path), None)?
    {
        return Ok(component);
    }

    if let Some(mut discovered) = try_discover_from_portable(Path::new(path))? {
        validate_duplicate_portable_component_ids(
            &discovered.id,
            Path::new(path),
            Some(Path::new(path)),
        )?;
        discovered.local_path = path.to_string();
        apply_standalone_fallbacks_at(config_root, &mut discovered);
        resolve_remote_path_at(config_root, &mut discovered);
        return Ok(discovered);
    }

    let dir = Path::new(path);
    if let Some(git_root) = detect_git_root(dir) {
        if git_root != dir {
            if let Some(mut discovered) = try_discover_from_portable(&git_root)? {
                validate_duplicate_portable_component_ids(&discovered.id, &git_root, None)?;
                discovered.local_path = path.to_string();
                apply_standalone_fallbacks_at(config_root, &mut discovered);
                resolve_remote_path_at(config_root, &mut discovered);
                return Ok(discovered);
            }
        }
    }

    Ok(synthetic_component_for_path(path))
}

/// Whether `component_id` names a component registered in the standalone/project
/// registry (as opposed to a synthetic ad-hoc target).
fn component_is_registered(component_id: &str) -> bool {
    crate::component::inventory::registered_base()
        .map(|components| components.iter().any(|c| c.id == component_id))
        .unwrap_or(false)
}

/// Resolve a bare path to the registered component whose checkout owns it as a
/// worktree — shared git common dir, `<component>@<branch>` naming, or an
/// unambiguous origin remote. Returns the canonical component projected onto
/// the checkout path. `None` when the path is not related to a registered
/// component (#9895).
fn registered_component_for_worktree_path(
    config_root: Option<&Path>,
    dir: &Path,
    expected_id: Option<&str>,
) -> Result<Option<Component>> {
    let Some(cwd_git_root) = detect_git_root(dir) else {
        return Ok(None);
    };
    let input_remote = crate::git::remote_origin_url(&cwd_git_root);
    let mut candidates = Vec::new();
    let registrations = match expected_id {
        Some(id) => load_at(config_root, id)
            .map(|component| vec![component])
            .unwrap_or_default(),
        None => crate::component::inventory::registered_base_at(config_root)?,
    };
    for registered in registrations {
        let registered_path =
            PathBuf::from(shellexpand::tilde(&registered.local_path).into_owned());
        // A worktree is a *distinct* checkout of the registered component's repo:
        // it shares the git common dir but the registered checkout does not live
        // inside this working tree. Excluding the contained case avoids matching a
        // monorepo root to one of its sub-directory components, which must remain
        // a monorepo root rather than a single component (#9895).
        let registered_is_contained = path_is_at_or_inside(&cwd_git_root, &registered_path);
        let remote_matches = input_remote.is_some()
            && input_remote == crate::git::remote_origin_url(&registered_path);
        if !registered_is_contained
            && (same_git_common_dir(&registered_path, &cwd_git_root)
                || is_named_component_worktree(&registered.id, &registered_path, &cwd_git_root)
                || remote_matches)
        {
            candidates.push(registered);
        }
    }
    candidates.sort_by(|left, right| left.id.cmp(&right.id));
    candidates.dedup_by(|left, right| left.id == right.id);
    match candidates.len() {
        0 => Ok(None),
        1 => {
            let base = candidates.pop().expect("one candidate");
            let registered = load_at(config_root, &base.id).unwrap_or(base);
            let registered_path =
                PathBuf::from(shellexpand::tilde(&registered.local_path).into_owned());
            let checkout_path = rebase_registered_path_to_checkout(&registered_path, &cwd_git_root);
            portable_component_for_checkout(
                config_root,
                &registered.id,
                &checkout_path,
                &registered,
            )
            .map(Some)
        }
        _ => Err(Error::validation_invalid_argument(
            "path",
            format!(
                "Checkout '{}' matches multiple registered component configurations: {}",
                cwd_git_root.display(),
                candidates.iter().map(|component| component.id.as_str()).collect::<Vec<_>>().join(", ")
            ),
            Some(dir.to_string_lossy().to_string()),
            Some(vec!["Use an explicit component ID or register one canonical component for this repository".to_string()]),
        )),
    }
}

fn path_has_portable_config(path: &Path) -> Result<bool> {
    if try_discover_from_portable(path)?.is_some() {
        return Ok(true);
    }

    if let Some(git_root) = detect_git_root(path) {
        if git_root != path {
            return Ok(try_discover_from_portable(&git_root)?.is_some());
        }
    }

    Ok(false)
}

/// Resolve a target from the shared command-facing contract.
///
/// Resolution order:
/// 1. project-scoped component ID, when a project is supplied
/// 2. explicit `--path`, optionally preserving an explicit component ID
/// 3. positional bare directory, when enabled
/// 4. CWD checkout matching the requested component ID
/// 5. registered component lookup
/// 6. CWD registry/portable discovery
pub fn resolve_target(spec: TargetSpec<'_>) -> Result<ResolvedTarget> {
    let component_id_is_bare_dir = spec
        .component_id
        .map(|id| Path::new(id).is_dir())
        .unwrap_or(false);

    if component_id_is_bare_dir && !spec.accept_bare_directory && spec.path_override.is_none() {
        return Err(Error::validation_invalid_argument(
            "component",
            "Bare directory targets are not accepted by this command",
            spec.component_id.map(ToOwned::to_owned),
            Some(vec![
                "Use --path when this command supports ad-hoc paths".to_string()
            ]),
        ));
    }

    // `resolve_target` has no rooted sibling yet (#7505): its synthetic-target
    // check reaches `component_is_registered`, and its callers all resolve
    // ambiently today. It is therefore explicitly ambient here rather than
    // accidentally so.
    let component = resolve_effective_inner(
        None,
        spec.component_id,
        spec.path_override,
        spec.project,
        spec.accept_bare_directory,
        spec.registry_lookup,
        !spec.allow_synthetic,
    )?;

    let explicit_path = spec
        .path_override
        .map(|_| component.local_path.as_str())
        .or_else(|| component_id_is_bare_dir.then_some(component.local_path.as_str()));
    let synthetic = explicit_path
        .map(|path| path_has_portable_config(Path::new(path)).map(|has_config| !has_config))
        .transpose()?
        .unwrap_or(false)
        // A registered component resolved for a managed worktree has no portable
        // homeboy.json at the worktree path, but it is a real registered
        // component — not a synthetic ad-hoc target (#9895).
        && !component_is_registered(&component.id);
    if synthetic && !spec.allow_synthetic {
        return Err(Error::validation_invalid_argument(
            "target",
            "Target is not registered and has no homeboy.json",
            Some(component.local_path.clone()),
            Some(vec![
                "Register the component or add a repo-owned homeboy.json".to_string(),
            ]),
        ));
    }

    let extension_id = if let Some(capability) = spec.capability {
        Some(
            crate::extension_execution::resolve_execution_context(&component, capability)?
                .extension_id,
        )
    } else {
        None
    };

    let mut target = resolved_target_from_component(component, synthetic);
    target.extension_id = extension_id;
    Ok(target)
}

pub(crate) fn component_contains_path(component: &Component, path: &Path) -> bool {
    let expanded = shellexpand::tilde(&component.local_path);
    path_is_at_or_inside(Path::new(expanded.as_ref()), path)
}

pub(crate) fn component_is_contained_in_path(component: &Component, path: &Path) -> bool {
    let expanded = shellexpand::tilde(&component.local_path);
    path_strictly_contains(path, Path::new(expanded.as_ref()))
}

fn path_is_at_or_inside(parent: &Path, path: &Path) -> bool {
    match (parent.canonicalize().ok(), path.canonicalize().ok()) {
        (Some(parent), Some(path)) => path == parent || path.starts_with(&parent),
        _ => false,
    }
}

fn path_strictly_contains(parent: &Path, child: &Path) -> bool {
    match (parent.canonicalize().ok(), child.canonicalize().ok()) {
        (Some(parent), Some(child)) => child.starts_with(&parent) && child != parent,
        _ => false,
    }
}

/// Find the git root directory for a given path.
///
/// Delegates to the canonical [`crate::git::repo_root`] helper — which runs
/// `rev-parse --show-toplevel` and returns the trimmed, non-empty toplevel as a
/// `PathBuf` on success — rather than assembling the raw arg-vector here. The
/// observable contract (best-effort `Option<PathBuf>`) is unchanged.
pub fn detect_git_root(dir: &Path) -> Option<PathBuf> {
    crate::git::repo_root(dir)
}

/// Resolve a Component from an optional ID, with CWD auto-discovery fallback.
pub fn resolve(id: Option<&str>) -> Result<Component> {
    resolve_core(None, id)
}

/// [`resolve`] against an already-resolved config root (#7505).
pub fn resolve_in_root(config_root: &Path, id: Option<&str>) -> Result<Component> {
    resolve_core(Some(config_root), id)
}

fn resolve_core(config_root: Option<&Path>, id: Option<&str>) -> Result<Component> {
    if let Some(id) = id {
        return load_at(config_root, id);
    }

    if let Some(detected_id) = detect_from_cwd(config_root) {
        return load_at(config_root, &detected_id);
    }

    let cwd = std::env::current_dir().map_err(|e| Error::internal_io(e.to_string(), None))?;

    if let Some(component) = try_discover_from_portable(&cwd)? {
        validate_duplicate_portable_component_ids(&component.id, &cwd, None)?;
        return Ok(component);
    }

    if let Some(git_root) = detect_git_root(&cwd) {
        if git_root != cwd {
            if let Some(component) = try_discover_from_portable(&git_root)? {
                validate_duplicate_portable_component_ids(&component.id, &git_root, None)?;
                return Ok(component);
            }
        }
    }

    let mut hints = vec![
        "Provide a component ID: homeboy <command> <component-id>".to_string(),
        "Or run from a directory containing homeboy.json".to_string(),
    ];
    if detect_from_cwd(config_root).is_none() {
        hints.push("Initialize the repo: homeboy component create --local-path .".to_string());
        hints.push(
            "Or attach the repo to a project: homeboy project components attach-path <project> ."
                .to_string(),
        );
    }

    Err(Error::validation_invalid_argument(
        "component_id",
        "No component ID provided and no homeboy.json found in current directory",
        None,
        Some(hints),
    ))
}

/// Resolve the effective component for runtime operations.
pub fn resolve_effective(
    id: Option<&str>,
    path_override: Option<&str>,
    project: Option<&crate::project::Project>,
) -> Result<Component> {
    resolve_effective_inner(
        None,
        id,
        path_override,
        project,
        true,
        RegistryLookupPolicy::Allow,
        true,
    )
}

/// [`resolve_effective`] against an already-resolved config root (#7505).
///
/// Every registry read this resolution makes — the project attachment, the
/// standalone registration fallbacks, the registered-component lookup, the
/// worktree inventory scan, and extension-driven `remote_path` inference —
/// resolves from `config_root`. A supplied `project` is the caller's value and
/// is used as given; it is the caller's job to have loaded it from the same
/// root (`project::load_in_root`).
pub fn resolve_effective_in_root(
    config_root: &Path,
    id: Option<&str>,
    path_override: Option<&str>,
    project: Option<&crate::project::Project>,
) -> Result<Component> {
    resolve_effective_inner(
        Some(config_root),
        id,
        path_override,
        project,
        true,
        RegistryLookupPolicy::Allow,
        true,
    )
}

fn resolve_effective_inner(
    config_root: Option<&Path>,
    id: Option<&str>,
    path_override: Option<&str>,
    project: Option<&crate::project::Project>,
    accept_bare_directory: bool,
    registry_lookup: RegistryLookupPolicy,
    require_existing_path_override: bool,
) -> Result<Component> {
    // CLI --path values describe the invocation target, unlike persisted
    // component.local_path values. Resolve them once before any portable-config
    // lookup or local_path validation so relative paths have CWD semantics.
    let resolved_path_override = path_override
        .map(|path| resolve_explicit_path_override(path, require_existing_path_override))
        .transpose()?;
    let path_override = resolved_path_override.as_deref();

    if let (Some(project), Some(id)) = (project, id) {
        let component = match config_root {
            Some(config_root) => {
                crate::project::resolve_project_component_in_root(config_root, project, id)?
            }
            None => crate::project::resolve_project_component(project, id)?,
        };
        if let Some(path) = path_override {
            let component =
                portable_component_for_checkout(config_root, id, Path::new(path), &component)?;
            return match config_root {
                Some(config_root) => crate::project::bind_materialized_component_at_path_in_root(
                    config_root,
                    component,
                    project,
                ),
                None => crate::project::bind_materialized_component_at_path(component, project),
            };
        }
        return Ok(component);
    }

    if let Some(id) = id {
        if let Some(path) = path_override {
            if let Some(component) =
                registered_component_for_worktree_path(config_root, Path::new(path), Some(id))?
            {
                return Ok(component);
            }
            if let Some(mut discovered) = try_discover_from_portable(Path::new(path))? {
                if discovered.id != id {
                    return Err(Error::validation_invalid_argument(
                        "component_id",
                        format!(
                            "Component ID '{}' does not match homeboy.json id '{}' at {}",
                            id,
                            discovered.id,
                            Path::new(path).join("homeboy.json").display()
                        ),
                        Some(id.to_string()),
                        Some(vec![
                            format!("Use the path-owned component id: homeboy release {} --path {}", discovered.id, path),
                            "Or rename one homeboy.json id so component IDs are unique within the checkout".to_string(),
                        ]),
                    ));
                }
                validate_duplicate_portable_component_ids(
                    id,
                    Path::new(path),
                    Some(Path::new(path)),
                )?;
                discovered.local_path = path.to_string();
                apply_standalone_fallbacks_at(config_root, &mut discovered);
                resolve_remote_path_at(config_root, &mut discovered);
                Ok(discovered)
            } else {
                // Fallback: create a synthetic component when --path is
                // explicitly provided but the directory has no homeboy.json.
                // This supports ad-hoc operations on unregistered projects.
                if let Some(component) =
                    registered_component_for_worktree_path(config_root, Path::new(path), Some(id))?
                {
                    return Ok(component);
                }
                Ok(Component {
                    id: id.to_string(),
                    local_path: path.to_string(),
                    ..Component::default()
                })
            }
        } else {
            let id_path = Path::new(id);
            if accept_bare_directory && id_path.is_dir() {
                // The positional identifier resolves to a directory, so treat it as
                // a bare-directory target. Normalize it to an absolute `local_path`:
                // a relative value like `studio-native` (a sibling dir of the CWD)
                // would otherwise be stored verbatim and later rejected by
                // `validate_local_path` ("has relative local_path ... cannot be
                // resolved"), even though `component set` and `deploy` resolve the
                // same component to an absolute checkout. Normalizing here keeps all
                // three in agreement (#7410).
                let local_path =
                    normalize_component_local_path(id).unwrap_or_else(|_| id.to_string());

                if let Some(mut discovered) = try_discover_from_portable(id_path)? {
                    validate_duplicate_portable_component_ids(
                        &discovered.id,
                        id_path,
                        Some(id_path),
                    )?;
                    discovered.local_path = local_path;
                    resolve_remote_path_at(config_root, &mut discovered);
                    return Ok(discovered);
                }

                let name = id_path
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| "unknown".to_string());

                return Ok(Component {
                    id: name,
                    local_path,
                    ..Component::default()
                });
            }

            // No --path provided. Before falling back to the registry, check
            // if the CWD (or its git root) is a checkout of this component.
            // This ensures `homeboy test foo` from a different clone of `foo`
            // operates on the current checkout, not the registered local_path (#694).
            if let Some(cwd_component) = prefer_cwd_for_component(config_root, id)? {
                validate_duplicate_portable_component_ids(
                    id,
                    Path::new(&cwd_component.local_path),
                    None,
                )?;
                return Ok(cwd_component);
            }

            if registry_lookup == RegistryLookupPolicy::CwdOrPortableOnly {
                return Err(Error::validation_invalid_argument(
                    "component_id",
                    format!(
                        "Component '{}' was not found in the current checkout and registry lookup is disabled",
                        id
                    ),
                    Some(id.to_string()),
                    Some(vec![
                        "Run from the component checkout or pass --path <checkout>".to_string(),
                        "Allow registry lookup for commands that should use the registered local_path".to_string(),
                    ]),
                ));
            }
            load_at(config_root, id)
        }
    } else {
        if let Some(path) = path_override {
            return resolve_path_override(config_root, path);
        }

        resolve_core(config_root, None)
    }
}

/// Resolve an explicit `--path` override from the invocation directory.
///
/// Existing targets are canonicalized so subsequent component resolution and
/// validation use one absolute spelling, including when the override traverses
/// a symlink. Persisted component `local_path` values deliberately do not use
/// this helper: their configured semantics remain unchanged.
fn resolve_explicit_path_override(raw: &str, require_existing: bool) -> Result<String> {
    let expanded = shellexpand::tilde(raw);
    let candidate = Path::new(expanded.as_ref());
    let absolute = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("resolve current directory for --path override".to_string()),
                )
            })?
            .join(candidate)
    };
    let display_path = crate::paths::normalize_local_path(&absolute);
    match absolute.canonicalize() {
        Ok(canonical) => Ok(canonical.to_string_lossy().into_owned()),
        Err(_) if require_existing => Err(Error::validation_invalid_argument(
            "path",
            format!("--path override does not exist: {}", display_path.display()),
            Some(raw.to_string()),
            Some(vec![format!(
                "Verify the override path exists: ls -la {}",
                display_path.display()
            )]),
        )),
        // Synthetic ad-hoc targets deliberately permit a missing path. Retain an
        // absolute, lexically normalized spelling even though it cannot yet be
        // canonicalized.
        Err(_) => Ok(display_path.to_string_lossy().into_owned()),
    }
}

pub(crate) fn validate_duplicate_portable_component_ids(
    component_id: &str,
    selected_dir: &Path,
    explicit_dir: Option<&Path>,
) -> Result<()> {
    let scope = detect_git_root(selected_dir).unwrap_or_else(|| selected_dir.to_path_buf());
    let matches = portable_config_paths_for_id(&scope, component_id)?;
    if matches.len() <= 1 {
        return Ok(());
    }

    if let Some(explicit_dir) = explicit_dir {
        let explicit_config = canonical_config_path(explicit_dir.join("homeboy.json"));
        if matches.iter().any(|path| path == &explicit_config) {
            return Ok(());
        }
    }

    Err(duplicate_portable_id_error(component_id, matches))
}

fn portable_config_paths_for_id(scope: &Path, component_id: &str) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    collect_portable_config_paths_for_id(scope, component_id, &mut seen, &mut paths)?;
    paths.sort();
    Ok(paths)
}

fn collect_portable_config_paths_for_id(
    dir: &Path,
    component_id: &str,
    seen: &mut HashSet<PathBuf>,
    paths: &mut Vec<PathBuf>,
) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Ok(()),
    };

    let config_path = dir.join("homeboy.json");
    if config_path.exists()
        && crate::component::infer_portable_component_id(dir)
            .ok()
            .as_deref()
            == Some(component_id)
    {
        let canonical = canonical_config_path(config_path);
        if seen.insert(canonical.clone()) {
            paths.push(canonical);
        }
    }

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() || should_skip_portable_duplicate_scan_dir(&path) {
            continue;
        }
        collect_portable_config_paths_for_id(&path, component_id, seen, paths)?;
    }

    Ok(())
}

fn should_skip_portable_duplicate_scan_dir(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        // `.homeboy-build`/`.homeboy-bin` hold reconstructable Homeboy build
        // artifacts, which can contain a *copy* of a component's `homeboy.json`
        // (e.g. `.homeboy-build/<component>/homeboy.json`). Scanning them makes a
        // single component appear declared by multiple manifests and fails
        // discovery ("declared by multiple homeboy.json files"). They are not
        // source declarations, so exclude them from the duplicate scan. (#8210)
        Some(
            ".git"
                | ".homeboy"
                | ".homeboy-build"
                | ".homeboy-bin"
                | "target"
                | "node_modules"
                | "vendor"
        )
    )
}

fn canonical_config_path(path: PathBuf) -> PathBuf {
    path.canonicalize().unwrap_or(path)
}

fn duplicate_portable_id_error(component_id: &str, paths: Vec<PathBuf>) -> Error {
    let path_list = paths
        .iter()
        .map(|path| format!("- {}", path.display()))
        .collect::<Vec<_>>()
        .join("\n");

    Error::validation_invalid_argument(
        "component_id",
        format!(
            "Component id '{}' is declared by multiple homeboy.json files:\n{}",
            component_id, path_list
        ),
        Some(component_id.to_string()),
        Some(vec![
            "Rename one homeboy.json id so each local component id is unique within the checkout".to_string(),
            "Use --path <component-dir> to disambiguate the intended component when the command supports path overrides".to_string(),
        ]),
    )
    .with_hint("Use --path <component-dir> to disambiguate the intended component when the command supports path overrides")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::ScopedExtensionConfig;
    use crate::project::{Project, ProjectComponentAttachment, ProjectComponentOverrides};
    use std::collections::HashMap;
    use std::fs;
    use std::sync::{Mutex, OnceLock};

    static CWD_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn cwd_lock() -> &'static Mutex<()> {
        CWD_LOCK.get_or_init(|| Mutex::new(()))
    }

    fn with_cwd<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
        let _guard = cwd_lock().lock().unwrap_or_else(|error| error.into_inner());
        let previous = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(dir).expect("set cwd");
        let result = f();
        std::env::set_current_dir(previous).expect("restore cwd");
        result
    }

    fn write_standalone_registration(home: &Path, id: &str, local_path: &Path) {
        let components = home.join(".config").join("homeboy").join("components");
        std::fs::create_dir_all(&components).expect("components dir");
        std::fs::write(
            components.join(format!("{id}.json")),
            serde_json::json!({
                "local_path": local_path,
                "remote_path": format!("wp-content/plugins/{id}")
            })
            .to_string(),
        )
        .expect("standalone registration");
    }

    fn write_standalone_config(home: &Path, id: &str, config: serde_json::Value) {
        let components = home.join(".config/homeboy/components");
        fs::create_dir_all(&components).expect("components dir");
        fs::write(components.join(format!("{id}.json")), config.to_string())
            .expect("standalone config");
    }

    fn git(path: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("git command should run");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn write_portable(dir: &Path, id: &str) {
        fs::create_dir_all(dir).expect("component dir");
        fs::write(
            dir.join("homeboy.json"),
            serde_json::json!({
                "id": id,
                "build_artifact": format!("build/{id}.zip")
            })
            .to_string(),
        )
        .expect("portable config");
    }

    fn write_portable_with_env(dir: &Path, id: &str, env: Option<(&str, &str)>) {
        let mut manifest = serde_json::json!({
            "id": id,
            "build_artifact": format!("build/{id}.zip")
        });
        if let Some((key, value)) = env {
            manifest["env"] = serde_json::json!({ key: value });
        }
        fs::write(dir.join("homeboy.json"), manifest.to_string()).expect("portable config");
    }

    fn add_worktree(primary: &Path, worktree: &Path, branch: &str) {
        git(
            primary,
            &[
                "worktree",
                "add",
                "-b",
                branch,
                worktree.to_str().expect("worktree path"),
            ],
        );
    }

    #[test]
    fn registered_primary_path_resolution_is_exact_and_canonical() {
        crate::test_support::with_isolated_home(|home| {
            let root = tempfile::tempdir().expect("fixture root");
            let primary = root.path().join("primary");
            let link = root.path().join("primary-link");
            fs::create_dir(&primary).expect("primary directory");
            git(&primary, &["init"]);
            write_standalone_registration(home.path(), "fixture", &primary);
            #[cfg(unix)]
            std::os::unix::fs::symlink(&primary, &link).expect("primary symlink");

            let provided = if cfg!(unix) { &link } else { &primary };
            assert_eq!(
                resolve_registered_primary_path(provided.to_str().expect("path"))
                    .expect("resolve primary"),
                RegisteredPrimaryPathResolution::Primary("fixture".to_string())
            );
            assert_eq!(
                resolve_registered_primary_path(
                    root.path().join("unknown").to_str().expect("path")
                )
                .expect("resolve unknown"),
                RegisteredPrimaryPathResolution::MissingPath
            );
            let non_git = root.path().join("non-git");
            fs::create_dir(&non_git).expect("non-Git directory");
            assert_eq!(
                resolve_registered_primary_path(non_git.to_str().expect("path"))
                    .expect("classify non-Git path"),
                RegisteredPrimaryPathResolution::NonGitPath
            );
        });
    }

    #[test]
    fn registered_primary_identity_prefers_an_exact_slug_over_a_relative_path() {
        crate::test_support::with_isolated_home(|home| {
            let root = tempfile::tempdir().expect("fixture root");
            let primary = root.path().join("primary");
            fs::create_dir(&primary).expect("primary directory");
            git(&primary, &["init"]);
            write_standalone_registration(home.path(), "fixture", &primary);
            fs::create_dir(root.path().join("fixture")).expect("colliding relative directory");

            let resolution = with_cwd(root.path(), || {
                resolve_registered_primary_identity("fixture").expect("resolve exact slug")
            });

            assert_eq!(
                resolution,
                RegisteredPrimaryPathResolution::Primary("fixture".to_string())
            );
        });
    }

    #[test]
    fn registered_primary_identity_rejects_unknown_slugs_with_sorted_candidates() {
        crate::test_support::with_isolated_home(|home| {
            let root = tempfile::tempdir().expect("fixture root");
            for index in (0..64).rev() {
                let id = format!("component-{index:02}");
                write_standalone_config(
                    home.path(),
                    &id,
                    serde_json::json!({
                        "local_path": root.path().join(&id),
                        "remote_url": format!("https://example.test/acme/repository-{index:02}.git")
                    }),
                );
            }

            let started = std::time::Instant::now();
            let resolution =
                resolve_registered_primary_identity("unknown").expect("classify unknown slug");
            assert!(started.elapsed() < std::time::Duration::from_secs(2));
            let RegisteredPrimaryPathResolution::UnregisteredRepository(candidates) = resolution
            else {
                panic!("unknown slug must be rejected")
            };
            assert_eq!(candidates.components.len(), 64);
            assert_eq!(candidates.components[0], "component-00");
            assert_eq!(candidates.components[63], "component-63");
            assert_eq!(candidates.repositories.len(), 64);
            assert_eq!(candidates.repositories[0], "repository-00");
            assert_eq!(candidates.repositories[63], "repository-63");
        });
    }

    #[test]
    fn registered_primary_path_resolution_rejects_related_and_ambiguous_checkouts() {
        crate::test_support::with_isolated_home(|home| {
            let root = tempfile::tempdir().expect("fixture root");
            let primary_a = root.path().join("primary-a");
            let primary_b = root.path().join("primary-b");
            let adopted = root.path().join("adopted");
            for path in [&primary_a, &primary_b, &adopted] {
                fs::create_dir(path).expect("checkout directory");
                git(path, &["init"]);
                git(
                    path,
                    &[
                        "remote",
                        "add",
                        "origin",
                        "https://example.test/org/fixture.git",
                    ],
                );
            }
            write_standalone_registration(home.path(), "fixture-a", &primary_a);
            write_standalone_registration(home.path(), "fixture-b", &primary_b);

            assert_eq!(
                resolve_registered_primary_path(adopted.to_str().expect("path"))
                    .expect("resolve related checkout"),
                RegisteredPrimaryPathResolution::UnregisteredRepository(RegisteredPathCandidates {
                    repositories: vec!["fixture".to_string()],
                    components: vec!["fixture-a".to_string(), "fixture-b".to_string()],
                })
            );
        });
    }

    #[test]
    fn registered_primary_path_resolution_distinguishes_nested_and_stale_candidates() {
        crate::test_support::with_isolated_home(|home| {
            let root = tempfile::tempdir().expect("repository root");
            git(root.path(), &["init"]);
            git(
                root.path(),
                &[
                    "remote",
                    "add",
                    "origin",
                    "https://example.test/org/blocks-engine.git",
                ],
            );
            let nested = root.path().join("packages/php-transformer");
            fs::create_dir_all(&nested).expect("nested component");
            write_standalone_registration(home.path(), "php-transformer", &nested);

            assert_eq!(
                resolve_registered_primary_path(root.path().to_str().expect("path"))
                    .expect("classify repository root"),
                RegisteredPrimaryPathResolution::AmbiguousNestedComponent(
                    RegisteredPathCandidates {
                        repositories: vec!["blocks-engine".to_string()],
                        components: vec!["php-transformer".to_string()],
                    }
                )
            );

            fs::remove_dir_all(&nested).expect("make registration stale");
            let registration = home
                .path()
                .join(".config/homeboy/components/php-transformer.json");
            fs::write(
                registration,
                serde_json::json!({
                    "local_path": nested
                })
                .to_string(),
            )
            .expect("stale registration");
            assert_eq!(
                resolve_registered_primary_path(root.path().to_str().expect("path"))
                    .expect("classify stale registration"),
                RegisteredPrimaryPathResolution::StaleRegistry(RegisteredPathCandidates {
                    repositories: vec!["blocks-engine".to_string()],
                    components: vec!["php-transformer".to_string()],
                })
            );
        });
    }

    #[test]
    fn duplicate_portable_ids_error_for_id_only_resolution_scope() {
        let repo = tempfile::tempdir().expect("repo");
        git(repo.path(), &["init"]);
        write_portable(repo.path(), "fixture");
        write_portable(&repo.path().join("plugins/fixture"), "fixture");

        let err = validate_duplicate_portable_component_ids("fixture", repo.path(), None)
            .expect_err("duplicate id should fail");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("multiple homeboy.json files"));
        assert!(err.message.contains("plugins/fixture/homeboy.json"));
        assert!(err
            .hints
            .iter()
            .any(|hint| hint.message.contains("--path <component-dir>")));
    }

    #[test]
    fn generated_homeboy_build_manifest_does_not_trigger_duplicate_ids() {
        // A component repo with a reconstructable `.homeboy-build/<component>/`
        // copy of its `homeboy.json` must still resolve: the build artifact is
        // not a second source declaration. (#8210)
        let repo = tempfile::tempdir().expect("repo");
        git(repo.path(), &["init"]);
        write_portable(repo.path(), "fixture");
        write_portable(&repo.path().join(".homeboy-build/fixture"), "fixture");

        validate_duplicate_portable_component_ids("fixture", repo.path(), None)
            .expect("a generated .homeboy-build manifest must not count as a duplicate");
    }

    #[test]
    fn explicit_path_disambiguates_duplicate_portable_ids() {
        let repo = tempfile::tempdir().expect("repo");
        git(repo.path(), &["init"]);
        write_portable(repo.path(), "fixture");
        let nested = repo.path().join("plugins/fixture");
        write_portable(&nested, "fixture");

        let resolved = resolve_effective(
            Some("fixture"),
            Some(nested.to_string_lossy().as_ref()),
            None,
        )
        .expect("explicit path should select the nested config");

        assert_eq!(resolved.id, "fixture");
        assert_eq!(
            Path::new(&resolved.local_path),
            nested.canonicalize().expect("canonical nested component")
        );
    }

    #[test]
    fn test_resolve_artifact() {
        let explicit = Component {
            id: "explicit".to_string(),
            local_path: "/tmp/explicit".to_string(),
            build_artifact: Some("dist/plugin.zip".to_string()),
            ..Component::default()
        };

        assert_eq!(
            resolve_artifact(&explicit).expect("explicit artifact resolves"),
            Some("dist/plugin.zip".to_string())
        );

        let mut extensions = HashMap::new();
        extensions.insert(
            "unknown-extension".to_string(),
            ScopedExtensionConfig::default(),
        );
        let missing_extension = Component {
            id: "missing-extension".to_string(),
            local_path: "/tmp/missing-extension".to_string(),
            extensions: Some(extensions),
            ..Component::default()
        };

        assert_eq!(
            resolve_artifact(&missing_extension).expect("unloadable extensions are skipped"),
            None
        );
    }

    /// Write an extension manifest that declares `build.artifact_pattern`, and
    /// optionally composes the given extensions via `composition.includes`.
    fn write_build_extension(home: &Path, id: &str, artifact_pattern: &str, includes: &[&str]) {
        let dir = home.join(".config/homeboy/extensions").join(id);
        std::fs::create_dir_all(&dir).expect("extension dir");

        let mut manifest = serde_json::json!({
            "name": id,
            "version": "1.0.0",
            "build": { "artifact_pattern": artifact_pattern },
        });
        if !includes.is_empty() {
            manifest["composition"] = serde_json::json!({ "includes": includes });
        }

        std::fs::write(dir.join(format!("{id}.json")), manifest.to_string())
            .expect("extension manifest");
    }

    fn component_with_extensions(id: &str, extension_ids: &[&str]) -> Component {
        Component {
            id: id.to_string(),
            local_path: format!("/tmp/{id}"),
            extensions: Some(
                extension_ids
                    .iter()
                    .map(|extension_id| {
                        (
                            (*extension_id).to_string(),
                            ScopedExtensionConfig::default(),
                        )
                    })
                    .collect(),
            ),
            ..Component::default()
        }
    }

    /// Resolution repeated enough times that `HashMap` iteration order over the
    /// component's linked extensions would have varied within the run. Every
    /// call must agree — that is the whole point of #10281.
    fn resolve_artifact_repeatedly(component: &Component) -> Vec<Result<Option<String>>> {
        (0..32).map(|_| resolve_artifact(component)).collect()
    }

    #[test]
    fn resolve_artifact_uses_single_extension_artifact_pattern() {
        crate::test_support::with_isolated_home(|home| {
            write_build_extension(home.path(), "wordpress", "build/{component_id}.zip", &[]);

            let component = component_with_extensions("plugin", &["wordpress"]);

            assert_eq!(
                resolve_artifact(&component).expect("single provider resolves"),
                Some("build/plugin.zip".to_string())
            );
        });
    }

    #[test]
    fn resolve_artifact_accepts_two_extensions_declaring_the_same_pattern() {
        crate::test_support::with_isolated_home(|home| {
            // Two providers that agree are not ambiguous: whichever one the
            // HashMap yields first, the resolved artifact is identical.
            write_build_extension(home.path(), "wordpress", "build/{component_id}.zip", &[]);
            write_build_extension(home.path(), "nodejs", "build/{component_id}.zip", &[]);

            let component = component_with_extensions("plugin", &["wordpress", "nodejs"]);

            for resolved in resolve_artifact_repeatedly(&component) {
                assert_eq!(
                    resolved.expect("agreeing providers are unambiguous"),
                    Some("build/plugin.zip".to_string())
                );
            }
        });
    }

    #[test]
    fn resolve_artifact_conflicting_patterns_fail_deterministically() {
        crate::test_support::with_isolated_home(|home| {
            // Two providers that disagree used to be a coin flip decided by
            // HashMap iteration order — the same binary could deploy
            // `build/plugin.zip` on one run and `dist/plugin.tgz` on the next.
            write_build_extension(home.path(), "wordpress", "build/{component_id}.zip", &[]);
            write_build_extension(home.path(), "nodejs", "dist/{component_id}.tgz", &[]);

            let component = component_with_extensions("plugin", &["wordpress", "nodejs"]);

            for resolved in resolve_artifact_repeatedly(&component) {
                let err = resolved.expect_err("conflicting artifact patterns must not be guessed");
                assert_eq!(err.code.as_str(), "validation.invalid_argument");
                // The exact message is stable too, including the provider list.
                assert_eq!(
                    err.message,
                    "Invalid argument 'extension': Component 'plugin' has multiple linked extensions providing 'build': nodejs, wordpress"
                );
            }
        });
    }

    #[test]
    fn resolve_artifact_conflict_is_resolved_by_capability_extensions() {
        crate::test_support::with_isolated_home(|home| {
            write_build_extension(home.path(), "wordpress", "build/{component_id}.zip", &[]);
            write_build_extension(home.path(), "nodejs", "dist/{component_id}.tgz", &[]);

            let mut component = component_with_extensions("plugin", &["wordpress", "nodejs"]);
            component
                .capability_extensions
                .insert("build".to_string(), "nodejs".to_string());

            for resolved in resolve_artifact_repeatedly(&component) {
                assert_eq!(
                    resolved.expect("explicit ownership resolves the conflict"),
                    Some("dist/plugin.tgz".to_string())
                );
            }
        });
    }

    #[test]
    fn resolve_artifact_conflict_is_resolved_by_composition_primary() {
        crate::test_support::with_isolated_home(|home| {
            // WordPress composes Node.js, so it owns the shared build
            // capability — the same rule every other capability follows.
            write_build_extension(
                home.path(),
                "wordpress",
                "build/{component_id}.zip",
                &["nodejs"],
            );
            write_build_extension(home.path(), "nodejs", "dist/{component_id}.tgz", &[]);

            let component = component_with_extensions("plugin", &["wordpress", "nodejs"]);

            for resolved in resolve_artifact_repeatedly(&component) {
                assert_eq!(
                    resolved.expect("composition primary resolves the conflict"),
                    Some("build/plugin.zip".to_string())
                );
            }
        });
    }

    #[test]
    fn test_validate_local_path() {
        let dir = tempfile::tempdir().expect("temp dir");
        let component = Component {
            id: "valid".to_string(),
            local_path: dir.path().to_string_lossy().to_string(),
            ..Component::default()
        };

        assert_eq!(
            validate_local_path(&component).expect("valid path"),
            dir.path()
        );

        let relative = Component {
            id: "relative".to_string(),
            local_path: "relative/path".to_string(),
            ..Component::default()
        };
        assert!(validate_local_path(&relative).is_err());
    }

    #[test]
    fn test_detect_from_cwd() {
        let dir = tempfile::tempdir().expect("temp dir");

        with_cwd(dir.path(), || {
            assert_eq!(detect_from_cwd(None), None);
        });
    }

    #[test]
    fn test_detect_git_root() {
        let dir = tempfile::tempdir().expect("temp dir");
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).expect("create repo dir");

        std::process::Command::new("git")
            .args(["init"])
            .current_dir(&repo)
            .output()
            .expect("git init");

        assert_eq!(detect_git_root(&repo), Some(repo.canonicalize().unwrap()));
    }

    #[test]
    fn resolve_effective_accepts_raw_directory_as_positional_component() {
        let dir = tempfile::tempdir().expect("temp dir");
        let repo = dir.path().join("raw-repo");
        std::fs::create_dir_all(&repo).expect("create repo dir");

        let component = resolve_effective(Some(repo.to_str().unwrap()), None, None)
            .expect("raw directory should resolve");

        assert_eq!(component.id, "raw-repo");
        assert_eq!(component.local_path, repo.to_string_lossy());
    }

    #[test]
    fn resolve_effective_normalizes_relative_bare_directory_to_absolute() {
        // Regression for #7410: `homeboy release studio-native` run from a
        // directory that contains a `studio-native` sibling resolves the
        // positional id as a bare directory. The resulting `local_path` must be
        // absolute so it agrees with what `component set` persists and `deploy`
        // resolves — a relative value here is later rejected by
        // `validate_local_path` ("has relative local_path ... cannot be resolved").
        let dir = tempfile::tempdir().expect("temp dir");
        let repo = dir.path().join("studio-native");
        std::fs::create_dir_all(&repo).expect("create repo dir");

        let component = with_cwd(dir.path(), || {
            resolve_effective(Some("studio-native"), None, None)
                .expect("relative bare directory should resolve")
        });

        assert!(
            Path::new(&component.local_path).is_absolute(),
            "bare-directory local_path must be absolute, got {:?}",
            component.local_path
        );
        // And it must resolve to the real checkout, matching `component set`'s
        // write-side normalization of the same relative value.
        assert_eq!(
            Path::new(&component.local_path)
                .canonicalize()
                .expect("canonical resolved path"),
            repo.canonicalize().expect("canonical repo path"),
        );
        // release's own validation must now accept it.
        assert!(validate_local_path(&component).is_ok());
    }

    #[test]
    fn resolve_effective_resolves_dot_path_override_from_cwd() {
        let dir = tempfile::tempdir().expect("temp dir");

        let component = with_cwd(dir.path(), || {
            resolve_effective(Some("fixture"), Some("."), None)
                .expect("dot path override should resolve")
        });

        assert_eq!(
            Path::new(&component.local_path)
                .canonicalize()
                .expect("canonical path"),
            dir.path().canonicalize().expect("canonical tempdir")
        );
    }

    #[test]
    fn resolve_effective_resolves_parent_path_override_from_cwd() {
        let parent = tempfile::tempdir().expect("parent tempdir");
        let child = parent.path().join("child");
        fs::create_dir(&child).expect("child dir");

        let component = with_cwd(&child, || {
            resolve_effective(Some("fixture"), Some(".."), None)
                .expect("parent path override should resolve")
        });

        assert_eq!(
            Path::new(&component.local_path)
                .canonicalize()
                .expect("canonical path"),
            parent.path().canonicalize().expect("canonical parent")
        );
    }

    #[test]
    fn resolve_effective_resolves_relative_child_path_override_from_cwd() {
        let dir = tempfile::tempdir().expect("temp dir");
        let child = dir.path().join("child");
        fs::create_dir(&child).expect("child dir");

        let component = with_cwd(dir.path(), || {
            resolve_effective(Some("fixture"), Some("child"), None)
                .expect("child path override should resolve")
        });

        assert_eq!(
            Path::new(&component.local_path)
                .canonicalize()
                .expect("canonical path"),
            child.canonicalize().expect("canonical child")
        );
    }

    #[test]
    fn resolve_effective_reports_missing_path_as_override_error() {
        let dir = tempfile::tempdir().expect("temp dir");

        let error = with_cwd(dir.path(), || {
            resolve_effective(Some("fixture"), Some("missing"), None)
                .expect_err("missing path override should fail")
        });

        assert_eq!(error.code.as_str(), "validation.invalid_argument");
        assert!(error.message.contains("--path override does not exist"));
    }

    #[cfg(unix)]
    #[test]
    fn resolve_effective_canonicalizes_symlink_path_override() {
        let dir = tempfile::tempdir().expect("temp dir");
        let target = dir.path().join("target");
        let link = dir.path().join("link");
        fs::create_dir(&target).expect("target dir");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        let component = with_cwd(dir.path(), || {
            resolve_effective(Some("fixture"), Some("link"), None)
                .expect("symlink path override should resolve")
        });

        assert_eq!(
            Path::new(&component.local_path),
            target.canonicalize().expect("canonical target")
        );
    }

    #[test]
    fn resolve_effective_preserves_explicit_path_override_id() {
        let dir = tempfile::tempdir().expect("temp dir");
        let repo = dir.path().join("override-repo");
        std::fs::create_dir_all(&repo).expect("create repo dir");

        let component = resolve_effective(Some("registered-id"), repo.to_str(), None)
            .expect("explicit path override should resolve");

        assert_eq!(component.id, "registered-id");
        assert_eq!(
            Path::new(&component.local_path),
            repo.canonicalize().expect("canonical override repo")
        );
    }

    #[test]
    fn resolve_effective_accepts_path_override_without_component_id() {
        let dir = tempfile::tempdir().expect("temp dir");
        let repo = dir.path().join("external-repo");
        std::fs::create_dir_all(&repo).expect("create repo dir");

        let component = resolve_effective(None, repo.to_str(), None)
            .expect("path-only override should resolve");

        assert_eq!(component.id, "external-repo");
        assert_eq!(
            Path::new(&component.local_path),
            repo.canonicalize().expect("canonical override repo")
        );
    }

    #[test]
    fn resolve_effective_path_override_reads_portable_config_without_component_id() {
        let dir = tempfile::tempdir().expect("temp dir");
        let repo = dir.path().join("portable-repo");
        std::fs::create_dir_all(&repo).expect("create repo dir");
        std::fs::write(
            repo.join("homeboy.json"),
            r#"{"id":"portable-id","extensions":{"fixture-extension":{}}}"#,
        )
        .expect("write portable config");

        let component = resolve_effective(None, repo.to_str(), None)
            .expect("path-only portable config should resolve");

        assert_eq!(component.id, "portable-id");
        assert_eq!(
            Path::new(&component.local_path),
            repo.canonicalize().expect("canonical override repo")
        );
        assert!(component
            .extensions
            .as_ref()
            .expect("extensions")
            .contains_key("fixture-extension"));
    }

    #[test]
    fn resolve_effective_path_override_rejects_portable_config_without_id() {
        let dir = tempfile::tempdir().expect("temp dir");
        let repo = dir.path().join("portable-repo");
        std::fs::create_dir_all(&repo).expect("create repo dir");
        std::fs::write(
            repo.join("homeboy.json"),
            r#"{"extensions":{"fixture-extension":{}}}"#,
        )
        .expect("write portable config");

        let error = resolve_effective(None, repo.to_str(), None)
            .expect_err("path-only portable config without id should fail");

        assert_eq!(error.code.as_str(), "validation.invalid_argument");
        assert!(
            error.to_string().contains("missing required 'id' field"),
            "{error}"
        );
    }

    #[test]
    fn target_spec_resolves_registered_component() {
        crate::test_support::with_isolated_home(|home| {
            let repo = home.path().join("registered-repo");
            std::fs::create_dir_all(&repo).expect("repo dir");
            write_standalone_registration(home.path(), "registered", &repo);

            let target = resolve_target(TargetSpec::new(Some("registered"), None))
                .expect("registered target");

            assert_eq!(target.component_id, "registered");
            assert_eq!(target.source_path, repo);
            assert!(!target.synthetic);
        });
    }

    #[test]
    fn target_spec_can_disable_registry_lookup_for_id_only_targets() {
        crate::test_support::with_isolated_home(|home| {
            let repo = home.path().join("registered-repo");
            std::fs::create_dir_all(&repo).expect("repo dir");
            write_standalone_registration(home.path(), "registered", &repo);

            let err = resolve_target(TargetSpec {
                component_id: Some("registered"),
                registry_lookup: RegistryLookupPolicy::CwdOrPortableOnly,
                ..TargetSpec::default()
            })
            .expect_err("registry lookup should be disabled");

            assert!(err.to_string().contains("registry lookup is disabled"));
        });
    }

    #[test]
    fn target_spec_prefers_cwd_worktree_for_registered_component() {
        crate::test_support::with_isolated_home(|home| {
            let dir = tempfile::tempdir().expect("temp dir");
            let primary = dir.path().join("primary");
            let worktree = dir.path().join("component-worktree");
            std::fs::create_dir_all(&primary).expect("primary dir");
            git(&primary, &["init"]);
            git(&primary, &["config", "user.email", "test@example.com"]);
            git(&primary, &["config", "user.name", "Test User"]);
            std::fs::write(primary.join("README.md"), "fixture\n").expect("readme");
            git(&primary, &["add", "README.md"]);
            git(&primary, &["commit", "-m", "Initial commit"]);
            git(&primary, &["worktree", "add", worktree.to_str().unwrap()]);
            write_standalone_registration(home.path(), "registered", &primary);

            with_cwd(&worktree, || {
                let target = resolve_target(TargetSpec::new(Some("registered"), None))
                    .expect("registered worktree target");
                let canonical_worktree = worktree.canonicalize().expect("canonical worktree");

                assert_eq!(target.component_id, "registered");
                assert_eq!(target.source_path, canonical_worktree);
                assert_eq!(
                    target.component.local_path,
                    target.source_path.to_string_lossy()
                );
                assert!(!target.synthetic);
            });
        });
    }

    #[test]
    fn worktree_resolution_inherits_canonical_config_and_applies_portable_overrides() {
        crate::test_support::with_isolated_home(|home| {
            let dir = tempfile::tempdir().expect("temp dir");
            let primary = dir.path().join("fixture");
            std::fs::create_dir_all(&primary).expect("primary dir");
            git(&primary, &["init"]);
            git(&primary, &["config", "user.email", "test@example.com"]);
            git(&primary, &["config", "user.name", "Test User"]);
            write_portable_with_env(&primary, "fixture", Some(("REPO_RELATIVE_CACHE", "cache")));
            git(&primary, &["add", "homeboy.json"]);
            git(&primary, &["commit", "-m", "stale manifest"]);
            write_standalone_registration(home.path(), "fixture", &primary);

            let primary_component = resolve_effective(Some("fixture"), None, None)
                .expect("registered primary resolves");
            assert_eq!(
                primary_component
                    .env
                    .get("REPO_RELATIVE_CACHE")
                    .map(String::as_str),
                Some("cache"),
                "the registered primary still represents its own revision"
            );

            let configured_worktree = dir.path().join("fixture@configured-worktree");
            let task_worktree = dir.path().join("fixture@task-worktree");
            for (worktree, branch) in [
                (&configured_worktree, "configured-worktree"),
                (&task_worktree, "task-worktree"),
            ] {
                add_worktree(&primary, worktree, branch);
                write_portable_with_env(worktree, "fixture", None);
                git(worktree, &["add", "homeboy.json"]);
                git(worktree, &["commit", "-m", "fresh manifest"]);
            }

            with_cwd(&configured_worktree, || {
                let component = resolve_effective(Some("fixture"), None, None)
                    .expect("configured worktree resolves");
                assert_eq!(
                    Path::new(&component.local_path)
                        .canonicalize()
                        .expect("canonical resolved worktree"),
                    configured_worktree
                        .canonicalize()
                        .expect("canonical configured worktree")
                );
                assert_eq!(
                    component.env.get("REPO_RELATIVE_CACHE").map(String::as_str),
                    Some("cache"),
                    "configured worktrees inherit canonical configuration absent an explicit override"
                );
            });

            let task_component = resolve_effective(
                Some("fixture"),
                Some(task_worktree.to_str().expect("task worktree path")),
                None,
            )
            .expect("task worktree resolves through explicit target");
            assert_eq!(
                Path::new(&task_component.local_path),
                task_worktree
                    .canonicalize()
                    .expect("canonical task worktree")
            );
            assert_eq!(
                task_component
                    .env
                    .get("REPO_RELATIVE_CACHE")
                    .map(String::as_str),
                Some("cache")
            );

            let path_component = resolve_effective(
                None,
                Some(
                    configured_worktree
                        .to_str()
                        .expect("configured worktree path"),
                ),
                None,
            )
            .expect("path-only target resolves");
            assert_eq!(
                Path::new(&path_component.local_path),
                configured_worktree
                    .canonicalize()
                    .expect("canonical configured worktree")
            );
            assert_eq!(
                path_component
                    .env
                    .get("REPO_RELATIVE_CACHE")
                    .map(String::as_str),
                Some("cache")
            );
        });
    }

    #[test]
    fn path_only_worktree_resolution_skips_unrelated_stale_sibling_discovery() {
        crate::test_support::with_isolated_home(|home| {
            let dir = tempfile::tempdir().expect("temp dir");
            let primary = dir.path().join("fixture");
            let worktree = dir.path().join("fixture@task");
            let unrelated = dir.path().join("unrelated");
            fs::create_dir_all(&primary).expect("primary dir");
            git(&primary, &["init"]);
            git(&primary, &["config", "user.email", "test@example.com"]);
            git(&primary, &["config", "user.name", "Test User"]);
            write_portable(&primary, "fixture");
            git(&primary, &["add", "homeboy.json"]);
            git(&primary, &["commit", "-m", "initial manifest"]);
            add_worktree(&primary, &worktree, "task");
            write_portable(&unrelated, "unrelated");
            write_standalone_registration(home.path(), "fixture", &primary);
            write_standalone_registration(home.path(), "stale", &dir.path().join("missing"));

            crate::component::portable::take_discovery_paths_for_test();
            let component = resolve_effective(None, worktree.to_str(), None)
                .expect("path-only worktree resolves");
            let discovery_paths = crate::component::portable::take_discovery_paths_for_test();

            assert_eq!(component.id, "fixture");
            assert_eq!(
                Path::new(&component.local_path),
                worktree.canonicalize().expect("canonical worktree")
            );
            assert!(
                !discovery_paths.contains(&unrelated),
                "unrelated sibling was inspected: {discovery_paths:?}"
            );
        });
    }

    #[test]
    fn worktree_portable_config_overrides_canonical_extension_and_script_config() {
        crate::test_support::with_isolated_home(|home| {
            let dir = tempfile::tempdir().expect("temp dir");
            let primary = dir.path().join("fixture");
            let worktree = dir.path().join("fixture@feature");
            fs::create_dir_all(&primary).expect("primary dir");
            git(&primary, &["init"]);
            git(&primary, &["config", "user.email", "test@example.com"]);
            git(&primary, &["config", "user.name", "Test User"]);
            fs::write(
                primary.join("homeboy.json"),
                r#"{"id":"fixture","extensions":{"runtime":{"settings":{"canonical":true,"mode":"primary"}}},"scripts":{"test":["canonical-test"]}}"#,
            )
            .expect("primary manifest");
            git(&primary, &["add", "homeboy.json"]);
            git(&primary, &["commit", "-m", "primary manifest"]);
            add_worktree(&primary, &worktree, "feature");
            fs::write(
                worktree.join("homeboy.json"),
                r#"{"id":"fixture","extensions":{"runtime":{"settings":{"mode":"nested"},"mode":"worktree"}},"scripts":{"test":["worktree-test"]}}"#,
            )
            .expect("worktree manifest");
            git(&worktree, &["add", "homeboy.json"]);
            git(&worktree, &["commit", "-m", "worktree manifest"]);
            write_standalone_registration(home.path(), "fixture", &primary);

            let component = resolve_effective(Some("fixture"), worktree.to_str(), None)
                .expect("worktree resolves through canonical component");

            assert_eq!(
                component.local_path,
                worktree.canonicalize().unwrap().to_string_lossy()
            );
            let settings = component
                .extensions
                .as_ref()
                .and_then(|extensions| extensions.get("runtime"))
                .map(|extension| &extension.settings)
                .expect("effective extension settings");
            assert_eq!(settings.get("canonical"), Some(&serde_json::json!(true)));
            assert_eq!(settings.get("mode"), Some(&serde_json::json!("worktree")));
            assert_eq!(
                component.scripts.expect("effective scripts").test,
                vec!["worktree-test"]
            );
        });
    }

    #[test]
    fn worktree_remote_identity_fails_closed_when_multiple_components_match() {
        crate::test_support::with_isolated_home(|home| {
            let dir = tempfile::tempdir().expect("temp dir");
            let primary_a = dir.path().join("primary-a");
            let primary_b = dir.path().join("primary-b");
            let checkout = dir.path().join("fixture@branch");
            for path in [&primary_a, &primary_b, &checkout] {
                fs::create_dir_all(path).expect("checkout dir");
                git(path, &["init"]);
                git(
                    path,
                    &[
                        "remote",
                        "add",
                        "origin",
                        "https://example.test/org/fixture.git",
                    ],
                );
            }
            write_standalone_registration(home.path(), "fixture-a", &primary_a);
            write_standalone_registration(home.path(), "fixture-b", &primary_b);

            let error = resolve_effective(None, checkout.to_str(), None)
                .expect_err("ambiguous remote must not select a canonical component");
            assert!(error
                .message
                .contains("multiple registered component configurations"));
            assert!(error.message.contains("fixture-a, fixture-b"));
        });
    }

    #[test]
    fn worktree_resolution_preserves_target_manifest_across_reverse_revision_drift() {
        crate::test_support::with_isolated_home(|home| {
            let dir = tempfile::tempdir().expect("temp dir");
            let primary = dir.path().join("fixture");
            let worktree = dir.path().join("fixture@older-revision");
            std::fs::create_dir_all(&primary).expect("primary dir");
            git(&primary, &["init"]);
            git(&primary, &["config", "user.email", "test@example.com"]);
            git(&primary, &["config", "user.name", "Test User"]);
            write_portable_with_env(&primary, "fixture", Some(("REPO_RELATIVE_CACHE", "cache")));
            git(&primary, &["add", "homeboy.json"]);
            git(&primary, &["commit", "-m", "old manifest"]);
            add_worktree(&primary, &worktree, "older-revision");

            write_portable_with_env(&primary, "fixture", None);
            git(&primary, &["add", "homeboy.json"]);
            git(&primary, &["commit", "-m", "new manifest"]);
            write_standalone_registration(home.path(), "fixture", &primary);

            let primary_component = resolve_effective(Some("fixture"), None, None)
                .expect("fresh registered primary resolves");
            assert!(!primary_component.env.contains_key("REPO_RELATIVE_CACHE"));

            with_cwd(&worktree, || {
                let component = resolve_effective(Some("fixture"), None, None)
                    .expect("older worktree resolves");
                assert_eq!(
                    component.env.get("REPO_RELATIVE_CACHE").map(String::as_str),
                    Some("cache"),
                    "the older worktree keeps its own manifest value"
                );
            });
        });
    }

    #[test]
    fn registered_worktree_without_compatible_manifest_fails_with_target_diagnostic() {
        crate::test_support::with_isolated_home(|home| {
            let dir = tempfile::tempdir().expect("temp dir");
            let primary = dir.path().join("fixture");
            let worktree = dir.path().join("fixture@missing-manifest");
            std::fs::create_dir_all(&primary).expect("primary dir");
            git(&primary, &["init"]);
            git(&primary, &["config", "user.email", "test@example.com"]);
            git(&primary, &["config", "user.name", "Test User"]);
            write_portable_with_env(&primary, "fixture", Some(("REPO_RELATIVE_CACHE", "cache")));
            git(&primary, &["add", "homeboy.json"]);
            git(&primary, &["commit", "-m", "manifest"]);
            add_worktree(&primary, &worktree, "missing-manifest");
            fs::remove_file(worktree.join("homeboy.json")).expect("remove target manifest");
            write_standalone_registration(home.path(), "fixture", &primary);

            let error = resolve_effective(
                Some("fixture"),
                Some(worktree.to_str().expect("worktree path")),
                None,
            )
            .expect_err("registered worktree must not borrow the primary manifest");
            assert_eq!(error.code.as_str(), "validation.invalid_argument");
            assert!(error.message.contains("Matched checkout"), "{error}");
            assert!(error.message.contains("homeboy.json"), "{error}");
            assert!(error
                .details
                .to_string()
                .contains("revision containing homeboy.json"));

            write_portable_with_env(&worktree, "other-component", None);
            let error = resolve_effective(
                Some("fixture"),
                Some(worktree.to_str().expect("worktree path")),
                None,
            )
            .expect_err("incompatible worktree manifest must fail");
            assert_eq!(error.code.as_str(), "validation.invalid_argument");
            assert!(error
                .message
                .contains("declares component id 'other-component' instead of 'fixture'"));
            assert!(error
                .message
                .contains(worktree.join("homeboy.json").to_string_lossy().as_ref()));
        });
    }

    #[test]
    fn target_manifest_reapplies_registration_and_project_owned_layers() {
        crate::test_support::with_isolated_home(|home| {
            let dir = tempfile::tempdir().expect("temp dir");
            let primary = dir.path().join("fixture");
            let worktree = dir.path().join("fixture@target");
            fs::create_dir_all(&primary).expect("primary dir");
            git(&primary, &["init"]);
            git(&primary, &["config", "user.email", "test@example.com"]);
            git(&primary, &["config", "user.name", "Test User"]);
            fs::write(
                primary.join("homeboy.json"),
                r#"{"id":"fixture","remote_path":"primary/path","env":{"REPO_RELATIVE_CACHE":"primary"}}"#,
            )
            .expect("primary manifest");
            git(&primary, &["add", "homeboy.json"]);
            git(&primary, &["commit", "-m", "primary manifest"]);
            add_worktree(&primary, &worktree, "target");
            fs::write(
                worktree.join("homeboy.json"),
                r#"{"id":"fixture","env":{"REPO_RELATIVE_CACHE":"target"}}"#,
            )
            .expect("target manifest");
            git(&worktree, &["add", "homeboy.json"]);
            git(&worktree, &["commit", "-m", "target manifest"]);
            write_standalone_config(
                home.path(),
                "fixture",
                serde_json::json!({
                    "local_path": primary,
                    "remote_path": "registry/path",
                    "remote_url": "https://github.com/example/fixture.git"
                }),
            );
            let project = Project {
                id: "site".to_string(),
                components: vec![ProjectComponentAttachment {
                    id: "fixture".to_string(),
                    local_path: primary.to_string_lossy().to_string(),
                    remote_path: Some("attachment/path".to_string()),
                    ..Default::default()
                }],
                component_overrides: HashMap::from([(
                    "fixture".to_string(),
                    ProjectComponentOverrides {
                        remote_path: Some("project/path".to_string()),
                        build_artifact: Some("project.zip".to_string()),
                        ..Default::default()
                    },
                )]),
                ..Default::default()
            };

            let component = resolve_effective(Some("fixture"), worktree.to_str(), Some(&project))
                .expect("target component resolves");

            assert_eq!(
                component.env.get("REPO_RELATIVE_CACHE").map(String::as_str),
                Some("target")
            );
            assert_eq!(component.remote_path, "project/path");
            assert_eq!(component.build_artifact.as_deref(), Some("project.zip"));
            assert_eq!(
                component.remote_url.as_deref(),
                Some("https://github.com/example/fixture.git")
            );
        });
    }

    #[test]
    fn explicit_id_does_not_match_another_manifestless_worktree_registration() {
        crate::test_support::with_isolated_home(|home| {
            let dir = tempfile::tempdir().expect("temp dir");
            let primary = dir.path().join("other");
            let worktree = dir.path().join("other@legacy");
            fs::create_dir_all(&primary).expect("primary dir");
            git(&primary, &["init"]);
            git(&primary, &["config", "user.email", "test@example.com"]);
            git(&primary, &["config", "user.name", "Test User"]);
            fs::write(primary.join("README.md"), "legacy\n").expect("readme");
            git(&primary, &["add", "README.md"]);
            git(&primary, &["commit", "-m", "legacy"]);
            add_worktree(&primary, &worktree, "legacy");
            write_standalone_registration(home.path(), "other", &primary);

            let component = resolve_effective(Some("fixture"), worktree.to_str(), None)
                .expect("unmatched explicit id remains synthetic");
            assert_eq!(component.id, "fixture");
            assert_eq!(
                Path::new(&component.local_path),
                worktree.canonicalize().expect("canonical worktree")
            );
            assert_ne!(component.remote_path, "wp-content/plugins/other");
        });
    }

    #[test]
    fn manifestless_registered_worktree_preserves_explicit_nested_target_path() {
        crate::test_support::with_isolated_home(|home| {
            let dir = tempfile::tempdir().expect("temp dir");
            let primary = dir.path().join("repo");
            let primary_component = primary.join("packages/fixture");
            let worktree = dir.path().join("repo@legacy");
            fs::create_dir_all(&primary_component).expect("component dir");
            git(&primary, &["init"]);
            git(&primary, &["config", "user.email", "test@example.com"]);
            git(&primary, &["config", "user.name", "Test User"]);
            fs::write(primary_component.join("README.md"), "legacy\n").expect("readme");
            git(&primary, &["add", "."]);
            git(&primary, &["commit", "-m", "legacy"]);
            add_worktree(&primary, &worktree, "legacy");
            write_standalone_registration(home.path(), "fixture", &primary_component);
            let nested = worktree.join("packages/fixture");

            let component = resolve_effective(Some("fixture"), nested.to_str(), None)
                .expect("nested legacy target resolves");
            assert_eq!(
                Path::new(&component.local_path),
                nested.canonicalize().expect("canonical nested target")
            );
        });
    }

    #[test]
    fn malformed_target_manifest_fails_for_cwd_id_path_and_path_only_resolution() {
        crate::test_support::with_isolated_home(|home| {
            let dir = tempfile::tempdir().expect("temp dir");
            let primary = dir.path().join("fixture");
            let worktree = dir.path().join("fixture@malformed");
            fs::create_dir_all(&primary).expect("primary dir");
            git(&primary, &["init"]);
            git(&primary, &["config", "user.email", "test@example.com"]);
            git(&primary, &["config", "user.name", "Test User"]);
            write_portable(&primary, "fixture");
            git(&primary, &["add", "homeboy.json"]);
            git(&primary, &["commit", "-m", "manifest"]);
            add_worktree(&primary, &worktree, "malformed");
            fs::write(worktree.join("homeboy.json"), "{").expect("malformed manifest");
            write_standalone_registration(home.path(), "fixture", &primary);

            let errors = [
                with_cwd(&worktree, || resolve_effective(Some("fixture"), None, None)),
                resolve_effective(Some("fixture"), worktree.to_str(), None),
                resolve_effective(None, worktree.to_str(), None),
            ];
            for error in errors {
                let error = error.expect_err("malformed target manifest must fail");
                assert_eq!(error.code.as_str(), "validation.invalid_json");
                assert!(
                    error.details.to_string().contains("homeboy.json"),
                    "{error}"
                );
            }
        });
    }

    #[test]
    fn target_spec_preserves_registered_monorepo_package_subpath_from_repo_root() {
        crate::test_support::with_isolated_home(|home| {
            let dir = tempfile::tempdir().expect("temp dir");
            let repo = dir.path().join("repo");
            let package = repo.join("packages").join("foo");
            std::fs::create_dir_all(&package).expect("package dir");
            git(&repo, &["init"]);
            git(&repo, &["config", "user.email", "test@example.com"]);
            git(&repo, &["config", "user.name", "Test User"]);
            std::fs::write(package.join("README.md"), "fixture\n").expect("readme");
            git(&repo, &["add", "."]);
            git(&repo, &["commit", "-m", "Initial commit"]);
            write_standalone_registration(home.path(), "foo", &package);

            with_cwd(&repo, || {
                let target =
                    resolve_target(TargetSpec::new(Some("foo"), None)).expect("package target");
                let canonical_package = package.canonicalize().expect("canonical package");

                assert_eq!(target.component_id, "foo");
                assert_eq!(target.source_path, canonical_package);
                assert_eq!(
                    target.component.local_path,
                    target.source_path.to_string_lossy()
                );
                let monorepo =
                    crate::git::MonorepoContext::detect(&target.component.local_path, "foo")
                        .expect("package path should still be monorepo-scoped");
                assert_eq!(monorepo.path_prefix, "packages/foo");
                let registered = crate::component::registered_by_id("foo")
                    .expect("registered component lookup")
                    .expect("registered component");
                assert_eq!(
                    rebase_component_path_to_checkout(&registered, &repo),
                    canonical_package
                );
                assert!(!target.synthetic);
            });
        });
    }

    #[test]
    fn target_spec_rebases_registered_monorepo_package_subpath_into_worktree() {
        crate::test_support::with_isolated_home(|home| {
            let dir = tempfile::tempdir().expect("temp dir");
            let primary = dir.path().join("primary");
            let primary_package = primary.join("packages").join("foo");
            let worktree = dir.path().join("worktree");
            let worktree_package = worktree.join("packages").join("foo");
            std::fs::create_dir_all(&primary_package).expect("package dir");
            git(&primary, &["init"]);
            git(&primary, &["config", "user.email", "test@example.com"]);
            git(&primary, &["config", "user.name", "Test User"]);
            std::fs::write(primary_package.join("README.md"), "fixture\n").expect("readme");
            git(&primary, &["add", "."]);
            git(&primary, &["commit", "-m", "Initial commit"]);
            git(&primary, &["worktree", "add", worktree.to_str().unwrap()]);
            write_standalone_registration(home.path(), "foo", &primary_package);

            with_cwd(&worktree, || {
                let target =
                    resolve_target(TargetSpec::new(Some("foo"), None)).expect("package target");
                let canonical_package = worktree_package.canonicalize().expect("canonical package");

                assert_eq!(target.component_id, "foo");
                assert_eq!(target.source_path, canonical_package);
                assert_eq!(
                    target.component.local_path,
                    target.source_path.to_string_lossy()
                );
                let monorepo =
                    crate::git::MonorepoContext::detect(&target.component.local_path, "foo")
                        .expect("package path should still be monorepo-scoped");
                assert_eq!(monorepo.path_prefix, "packages/foo");
                let registered = crate::component::registered_by_id("foo")
                    .expect("registered component lookup")
                    .expect("registered component");
                assert_eq!(
                    rebase_component_path_to_checkout(&registered, &worktree),
                    canonical_package
                );
                assert!(!target.synthetic);
            });
        });
    }

    #[test]
    fn target_spec_prefers_named_sibling_worktree_for_registered_component() {
        crate::test_support::with_isolated_home(|home| {
            let dir = tempfile::tempdir().expect("temp dir");
            let primary = dir.path().join("registered");
            let worktree = dir.path().join("registered@feature-branch");
            std::fs::create_dir_all(&primary).expect("primary dir");
            std::fs::create_dir_all(&worktree).expect("worktree dir");
            git(&primary, &["init"]);
            git(&worktree, &["init"]);
            write_standalone_registration(home.path(), "registered", &primary);

            with_cwd(&worktree, || {
                let target = resolve_target(TargetSpec::new(Some("registered"), None))
                    .expect("named worktree target");
                let canonical_worktree = worktree.canonicalize().expect("canonical worktree");

                assert_eq!(target.component_id, "registered");
                assert_eq!(target.source_path, canonical_worktree);
                assert_eq!(
                    target.component.local_path,
                    target.source_path.to_string_lossy()
                );
                assert!(!target.synthetic);
            });
        });
    }

    #[test]
    fn target_spec_resolves_from_cwd_portable_config() {
        crate::test_support::with_isolated_home(|_home| {
            let dir = tempfile::tempdir().expect("temp dir");
            let repo = dir.path().join("cwd-repo");
            std::fs::create_dir_all(&repo).expect("repo dir");
            std::fs::write(repo.join("homeboy.json"), r#"{"id":"cwd-id"}"#)
                .expect("portable config");

            with_cwd(&repo, || {
                let target = resolve_target(TargetSpec::new(None, None)).expect("cwd target");

                assert_eq!(target.component_id, "cwd-id");
                assert_eq!(target.source_path, repo.canonicalize().unwrap());
                assert!(!target.synthetic);
            });
        });
    }

    #[test]
    fn target_spec_resolves_path_override() {
        let dir = tempfile::tempdir().expect("temp dir");
        let repo = dir.path().join("path-repo");
        std::fs::create_dir_all(&repo).expect("repo dir");
        std::fs::write(repo.join("homeboy.json"), r#"{"id":"path-id"}"#).expect("portable config");

        let target = resolve_target(TargetSpec::new(None, repo.to_str())).expect("path target");

        assert_eq!(target.component_id, "path-id");
        assert_eq!(
            target.source_path,
            repo.canonicalize().expect("canonical path repo")
        );
        assert!(!target.synthetic);
    }

    #[test]
    fn target_spec_rejects_path_override_portable_config_without_id() {
        let dir = tempfile::tempdir().expect("temp dir");
        let repo = dir.path().join("path-repo");
        std::fs::create_dir_all(&repo).expect("repo dir");
        std::fs::write(repo.join("homeboy.json"), r#"{"remote_path":"remote"}"#)
            .expect("portable config");

        let error = resolve_target(TargetSpec::new(None, repo.to_str()))
            .expect_err("portable config without id should fail");

        assert_eq!(error.code.as_str(), "validation.invalid_argument");
        assert!(
            error.to_string().contains("missing required 'id' field"),
            "{error}"
        );
    }

    #[test]
    fn target_spec_accepts_bare_directory_positional_target() {
        let dir = tempfile::tempdir().expect("temp dir");
        let repo = dir.path().join("bare-repo");
        std::fs::create_dir_all(&repo).expect("repo dir");

        let target = resolve_target(TargetSpec::new(repo.to_str(), None)).expect("bare target");

        assert_eq!(target.component_id, "bare-repo");
        assert_eq!(target.source_path, repo);
        assert!(target.synthetic);
    }

    #[test]
    fn target_spec_allows_synthetic_path_target() {
        let dir = tempfile::tempdir().expect("temp dir");
        let repo = dir.path().join("synthetic-repo");
        std::fs::create_dir_all(&repo).expect("repo dir");

        let target =
            resolve_target(TargetSpec::new(None, repo.to_str())).expect("synthetic target");

        assert_eq!(target.component_id, "synthetic-repo");
        assert_eq!(
            target.source_path,
            repo.canonicalize().expect("canonical synthetic repo")
        );
        assert!(target.synthetic);
    }

    #[test]
    fn target_spec_allows_missing_synthetic_path_override() {
        let dir = tempfile::tempdir().expect("temp dir");
        let missing = dir.path().join("missing-repo");

        let target = resolve_target(TargetSpec::new(None, missing.to_str()))
            .expect("synthetic targets allow a missing path override");

        assert_eq!(target.component_id, "missing-repo");
        assert_eq!(
            target.source_path,
            crate::paths::normalize_local_path(&missing)
        );
        assert!(target.synthetic);
    }

    #[test]
    fn target_spec_rejects_missing_non_synthetic_path_override() {
        let dir = tempfile::tempdir().expect("temp dir");
        let missing = dir.path().join("missing-repo");

        let error = resolve_target(TargetSpec {
            component_id: None,
            path_override: missing.to_str(),
            allow_synthetic: false,
            ..TargetSpec::default()
        })
        .expect_err("non-synthetic targets require an existing path override");

        assert_eq!(error.code.as_str(), "validation.invalid_argument");
        assert!(error.message.contains("--path override does not exist"));
    }

    #[cfg(unix)]
    #[test]
    fn target_spec_allows_broken_symlink_only_for_synthetic_target() {
        let dir = tempfile::tempdir().expect("temp dir");
        let broken_link = dir.path().join("broken-link");
        std::os::unix::fs::symlink(dir.path().join("missing-target"), &broken_link)
            .expect("broken symlink");

        let target = resolve_target(TargetSpec::new(None, broken_link.to_str()))
            .expect("synthetic targets allow a broken symlink override");
        assert_eq!(
            target.source_path,
            crate::paths::normalize_local_path(&broken_link)
        );
        assert!(target.synthetic);

        let error = resolve_target(TargetSpec {
            component_id: None,
            path_override: broken_link.to_str(),
            allow_synthetic: false,
            ..TargetSpec::default()
        })
        .expect_err("non-synthetic targets reject a broken symlink override");
        assert!(error.message.contains("--path override does not exist"));
    }

    #[cfg(unix)]
    #[test]
    fn resolve_effective_canonicalizes_symlink_to_file_override() {
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("component-file");
        let link = dir.path().join("component-link");
        fs::write(&file, "fixture").expect("component file");
        std::os::unix::fs::symlink(&file, &link).expect("file symlink");

        let component = resolve_effective(Some("fixture"), link.to_str(), None)
            .expect("existing file overrides retain their prior accepted behavior");

        assert_eq!(
            Path::new(&component.local_path),
            file.canonicalize().expect("canonical component file")
        );
    }

    #[test]
    fn target_spec_can_reject_synthetic_target() {
        let dir = tempfile::tempdir().expect("temp dir");
        let repo = dir.path().join("synthetic-repo");
        std::fs::create_dir_all(&repo).expect("repo dir");

        let err = resolve_target(TargetSpec {
            component_id: None,
            path_override: repo.to_str(),
            allow_synthetic: false,
            ..TargetSpec::default()
        })
        .expect_err("synthetic target should be rejected");

        assert!(err.to_string().contains("not registered"));
    }

    #[test]
    fn normalize_resolves_relative_local_path_against_base() {
        let normalized =
            normalize_component_local_path_against("php-transformer", Path::new("/Users/dev/work"));
        assert_eq!(normalized, "/Users/dev/work/php-transformer");
    }

    #[test]
    fn normalize_keeps_absolute_local_path_and_collapses_segments() {
        let normalized = normalize_component_local_path_against(
            "/Users/dev/work/../work/php-transformer",
            Path::new("/ignored/base"),
        );
        assert_eq!(normalized, "/Users/dev/work/php-transformer");
    }

    #[test]
    fn normalize_preserves_empty_local_path() {
        assert_eq!(
            normalize_component_local_path_against("", Path::new("/Users/dev/work")),
            ""
        );
    }

    #[test]
    fn normalize_uses_current_dir_for_relative_input() {
        let dir = tempfile::tempdir().expect("temp dir");
        let base = std::fs::canonicalize(dir.path()).expect("canonicalize base");
        let normalized = with_cwd(&base, || {
            normalize_component_local_path("php-transformer").expect("normalize")
        });
        assert_eq!(normalized, base.join("php-transformer").to_string_lossy());
    }

    #[test]
    fn local_path_is_relative_distinguishes_absolute_and_relative() {
        assert!(local_path_is_relative("php-transformer"));
        assert!(local_path_is_relative("./php-transformer"));
        assert!(!local_path_is_relative("/Users/dev/php-transformer"));
        assert!(!local_path_is_relative(""));
    }
}
