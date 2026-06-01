use crate::core::component::{
    discover_from_portable, inventory, load, try_discover_from_portable, Component,
};
use crate::core::error::{Error, Result};
use crate::core::extension::{self, ExtensionCapability};
use std::path::{Path, PathBuf};

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
    pub project: Option<&'a crate::core::project::Project>,

    /// Optional extension capability required by the caller.
    pub capability: Option<ExtensionCapability>,

    /// Whether an explicit path without registration/portable config may
    /// produce a synthetic component.
    pub allow_synthetic: bool,

    /// Whether a positional component value that is a directory is accepted as
    /// an ad-hoc target.
    pub accept_bare_directory: bool,
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
        }
    }
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

fn resolved_target_from_component(mut component: Component, synthetic: bool) -> ResolvedTarget {
    let source_path = PathBuf::from(shellexpand::tilde(&component.local_path).into_owned());
    let git_root = detect_git_root(&source_path);
    component.resolve_remote_path();

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

pub fn resolve_artifact(component: &Component) -> Option<String> {
    if let Some(ref artifact) = component.build_artifact {
        return Some(artifact.clone());
    }

    if let Some(ref extensions) = component.extensions {
        for extension_id in extensions.keys() {
            if let Ok(manifest) = crate::core::extension::load_extension(extension_id) {
                if let Some(ref build) = manifest.build {
                    if let Some(ref pattern) = build.artifact_pattern {
                        let resolved = pattern
                            .replace("{component_id}", &component.id)
                            .replace("{local_path}", &component.local_path);
                        return Some(resolved);
                    }
                }
            }
        }
    }

    None
}

/// Validates component local_path is usable (absolute and exists).
pub fn validate_local_path(component: &Component) -> Result<PathBuf> {
    let expanded = shellexpand::tilde(&component.local_path);
    let path = PathBuf::from(expanded.as_ref());

    if !path.is_absolute() {
        return Err(Error::validation_invalid_argument(
            "local_path",
            format!(
                "Component '{}' has relative local_path '{}' which cannot be resolved. Use absolute path like /Users/chubes/path/to/component",
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
                "Component '{}' local_path does not exist: {}",
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

/// Detect component ID from current working directory.
fn detect_from_cwd() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    let components = inventory().ok()?;

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
fn prefer_cwd_for_component(component_id: &str) -> Option<Component> {
    let cwd = std::env::current_dir().ok()?;

    // Check CWD directly
    if let Some(discovered) = discover_from_portable(&cwd) {
        if discovered.id == component_id {
            return Some(discovered);
        }
    }

    // Check git root if different from CWD
    if let Some(git_root) = detect_git_root(&cwd) {
        if git_root != cwd {
            if let Some(discovered) = discover_from_portable(&git_root) {
                if discovered.id == component_id {
                    return Some(discovered);
                }
            }
        }
    }

    let mut registered = load(component_id).ok()?;
    let registered_path = PathBuf::from(shellexpand::tilde(&registered.local_path).into_owned());
    let cwd_git_root = detect_git_root(&cwd)?;
    if same_git_common_dir(&registered_path, &cwd_git_root)
        || is_named_component_worktree(component_id, &registered_path, &cwd_git_root)
    {
        registered.local_path = cwd_git_root.to_string_lossy().to_string();
        registered.resolve_remote_path();
        return Some(registered);
    }

    None
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
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(dir)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
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

fn resolve_path_override(path: &str) -> Result<Component> {
    if let Some(mut discovered) = try_discover_from_portable(Path::new(path))? {
        discovered.local_path = path.to_string();
        discovered.resolve_remote_path();
        return Ok(discovered);
    }

    let dir = Path::new(path);
    if let Some(git_root) = detect_git_root(dir) {
        if git_root != dir {
            if let Some(mut discovered) = try_discover_from_portable(&git_root)? {
                discovered.local_path = path.to_string();
                discovered.resolve_remote_path();
                return Ok(discovered);
            }
        }
    }

    Ok(synthetic_component_for_path(path))
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

    let component = resolve_effective_inner(
        spec.component_id,
        spec.path_override,
        spec.project,
        spec.accept_bare_directory,
    )?;

    let explicit_path = spec
        .path_override
        .or_else(|| component_id_is_bare_dir.then_some(component.local_path.as_str()));
    let synthetic = explicit_path
        .map(|path| path_has_portable_config(Path::new(path)).map(|has_config| !has_config))
        .transpose()?
        .unwrap_or(false);
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
        Some(extension::resolve_execution_context(&component, capability)?.extension_id)
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
pub(crate) fn detect_git_root(dir: &Path) -> Option<PathBuf> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(dir)
        .output()
        .ok()?;

    if output.status.success() {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    None
}

/// Resolve a Component from an optional ID, with CWD auto-discovery fallback.
pub fn resolve(id: Option<&str>) -> Result<Component> {
    if let Some(id) = id {
        return load(id);
    }

    if let Some(detected_id) = detect_from_cwd() {
        return load(&detected_id);
    }

    let cwd = std::env::current_dir().map_err(|e| Error::internal_io(e.to_string(), None))?;

    if let Some(component) = try_discover_from_portable(&cwd)? {
        return Ok(component);
    }

    if let Some(git_root) = detect_git_root(&cwd) {
        if git_root != cwd {
            if let Some(component) = try_discover_from_portable(&git_root)? {
                return Ok(component);
            }
        }
    }

    let mut hints = vec![
        "Provide a component ID: homeboy <command> <component-id>".to_string(),
        "Or run from a directory containing homeboy.json".to_string(),
    ];
    if detect_from_cwd().is_none() {
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
    project: Option<&crate::core::project::Project>,
) -> Result<Component> {
    resolve_effective_inner(id, path_override, project, true)
}

fn resolve_effective_inner(
    id: Option<&str>,
    path_override: Option<&str>,
    project: Option<&crate::core::project::Project>,
    accept_bare_directory: bool,
) -> Result<Component> {
    if let (Some(project), Some(id)) = (project, id) {
        let mut component = crate::core::project::resolve_project_component(project, id)?;
        if let Some(path) = path_override {
            component.local_path = path.to_string();
        }
        return Ok(component);
    }

    if let Some(id) = id {
        if let Some(path) = path_override {
            if let Some(mut discovered) = try_discover_from_portable(Path::new(path))? {
                discovered.id = id.to_string();
                discovered.local_path = path.to_string();
                discovered.resolve_remote_path();
                Ok(discovered)
            } else {
                // Fallback: create a synthetic component when --path is
                // explicitly provided but the directory has no homeboy.json.
                // This supports ad-hoc operations on unregistered projects.
                Ok(Component {
                    id: id.to_string(),
                    local_path: path.to_string(),
                    ..Component::default()
                })
            }
        } else {
            let id_path = Path::new(id);
            if accept_bare_directory && id_path.is_dir() {
                if let Some(mut discovered) = try_discover_from_portable(id_path)? {
                    discovered.local_path = id.to_string();
                    discovered.resolve_remote_path();
                    return Ok(discovered);
                }

                let name = id_path
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| "unknown".to_string());

                return Ok(Component {
                    id: name,
                    local_path: id.to_string(),
                    ..Component::default()
                });
            }

            // No --path provided. Before falling back to the registry, check
            // if the CWD (or its git root) is a checkout of this component.
            // This ensures `homeboy test foo` from a different clone of `foo`
            // operates on the current checkout, not the registered local_path (#694).
            if let Some(cwd_component) = prefer_cwd_for_component(id) {
                return Ok(cwd_component);
            }
            load(id)
        }
    } else {
        if let Some(path) = path_override {
            return resolve_path_override(path);
        }

        resolve(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::component::ScopedExtensionConfig;
    use std::collections::HashMap;
    use std::fs;
    use std::sync::{Mutex, OnceLock};

    static CWD_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn cwd_lock() -> &'static Mutex<()> {
        CWD_LOCK.get_or_init(|| Mutex::new(()))
    }

    fn with_cwd<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
        let _guard = cwd_lock().lock().expect("cwd lock");
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

    #[test]
    fn test_resolve_artifact() {
        let explicit = Component {
            id: "explicit".to_string(),
            local_path: "/tmp/explicit".to_string(),
            build_artifact: Some("dist/plugin.zip".to_string()),
            ..Component::default()
        };

        assert_eq!(
            resolve_artifact(&explicit),
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

        assert_eq!(resolve_artifact(&missing_extension), None);
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
            assert_eq!(detect_from_cwd(), None);
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
    fn resolve_effective_preserves_explicit_path_override_id() {
        let dir = tempfile::tempdir().expect("temp dir");
        let repo = dir.path().join("override-repo");
        std::fs::create_dir_all(&repo).expect("create repo dir");

        let component = resolve_effective(Some("registered-id"), repo.to_str(), None)
            .expect("explicit path override should resolve");

        assert_eq!(component.id, "registered-id");
        assert_eq!(component.local_path, repo.to_string_lossy());
    }

    #[test]
    fn resolve_effective_accepts_path_override_without_component_id() {
        let dir = tempfile::tempdir().expect("temp dir");
        let repo = dir.path().join("external-repo");
        std::fs::create_dir_all(&repo).expect("create repo dir");

        let component = resolve_effective(None, repo.to_str(), None)
            .expect("path-only override should resolve");

        assert_eq!(component.id, "external-repo");
        assert_eq!(component.local_path, repo.to_string_lossy());
    }

    #[test]
    fn resolve_effective_path_override_reads_portable_config_without_component_id() {
        let dir = tempfile::tempdir().expect("temp dir");
        let repo = dir.path().join("portable-repo");
        std::fs::create_dir_all(&repo).expect("create repo dir");
        std::fs::write(
            repo.join("homeboy.json"),
            r#"{"id":"portable-id","extensions":{"nodejs":{}}}"#,
        )
        .expect("write portable config");

        let component = resolve_effective(None, repo.to_str(), None)
            .expect("path-only portable config should resolve");

        assert_eq!(component.id, "portable-id");
        assert_eq!(component.local_path, repo.to_string_lossy());
        assert!(component
            .extensions
            .as_ref()
            .expect("extensions")
            .contains_key("nodejs"));
    }

    #[test]
    fn resolve_effective_path_override_rejects_portable_config_without_id() {
        let dir = tempfile::tempdir().expect("temp dir");
        let repo = dir.path().join("portable-repo");
        std::fs::create_dir_all(&repo).expect("create repo dir");
        std::fs::write(repo.join("homeboy.json"), r#"{"extensions":{"nodejs":{}}}"#)
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
        assert_eq!(target.source_path, repo);
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
        assert_eq!(target.source_path, repo);
        assert!(target.synthetic);
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
}
