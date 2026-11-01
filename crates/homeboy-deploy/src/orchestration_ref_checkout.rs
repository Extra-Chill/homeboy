use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use homeboy_core::component::Component;
use homeboy_core::deps;
use homeboy_core::error::{Error, Result};
use homeboy_core::git;

const REMOTE_REF_QUERY_TIMEOUT: Duration = Duration::from_secs(30);

/// Clone the source repository for exact-ref materialization.
///
/// A `git clone --local` hardlinks the object store, which fails with
/// `Invalid cross-device link` when the source and the Homeboy temp root live on
/// different filesystems (a common split, e.g. workspace on `/var/lib` and temp
/// on a mounted `/mnt/...`). Attempt the fast `--local` clone first, and on that
/// specific cross-device failure retry with `--no-hardlinks`, which copies
/// objects instead of linking them and works across devices (#9889).
fn clone_exact_ref_source(source_root: &Path, source_arg: &str, worktree_arg: &str) -> Result<()> {
    let local_result = git::run_git(
        source_root,
        &[
            "clone",
            "--no-checkout",
            "--local",
            source_arg,
            worktree_arg,
        ],
        "clone exact deploy ref source",
    );
    match local_result {
        Ok(_) => Ok(()),
        Err(error) if is_cross_device_link_error(&error) => git::run_git(
            source_root,
            &[
                "clone",
                "--no-checkout",
                "--no-hardlinks",
                source_arg,
                worktree_arg,
            ],
            "clone exact deploy ref source across filesystems",
        )
        .map(|_| ()),
        Err(error) => Err(error),
    }
}

/// Whether a git failure is the cross-device hardlink error produced by
/// `clone --local` when source and destination are on different filesystems.
fn is_cross_device_link_error(error: &Error) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("cross-device link")
        || (message.contains("failed to create link") && message.contains("invalid cross-device"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExactRefIdentity {
    pub(crate) requested_ref: String,
    pub(crate) resolved_sha: String,
    pub(crate) source: String,
    pub(crate) resolution_mode: String,
}

pub(super) struct ExactRefCheckout {
    pub(crate) component: Component,
    pub(crate) identity: ExactRefIdentity,
    tree: SourceTree,
}

/// Where an exact-ref component's source lives.
#[derive(Clone)]
enum SourceTree {
    /// The configured checkout, proven to sit at the resolved commit with no
    /// modified, untracked, or ignored paths under component or dependency prefixes.
    InPlace(PathBuf),
    /// A disposable sparse checkout shared by every component that resolved to
    /// the same (repository, commit) pair in one invocation.
    Materialized(Arc<MaterializedTree>),
}

impl SourceTree {
    fn root(&self) -> &Path {
        match self {
            Self::InPlace(root) => root,
            Self::Materialized(tree) => &tree.0,
        }
    }
}

/// Whether `path` lives inside a disposable exact-ref checkout, which is removed
/// wholesale after the deploy and so needs no per-path cleanup.
pub(crate) fn is_disposable_source(path: &Path) -> bool {
    homeboy_core::artifacts::root()
        .is_ok_and(|root| path.starts_with(materialized_trees_root(&root)))
}

/// Tells component builds their source tree is disposable.
const BUILD_SOURCE_DISPOSABLE_ENV: &str = "HOMEBOY_BUILD_SOURCE_DISPOSABLE";

fn materialized_trees_root(artifacts_root: &Path) -> PathBuf {
    artifacts_root.join("deploy-ref")
}

/// Removes the disposable checkout once the last component referencing it drops.
struct MaterializedTree(PathBuf);

impl Drop for MaterializedTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One component deployed at one requested ref.
pub(crate) struct ExactRefRequest<'a> {
    pub(crate) component: &'a Component,
    pub(crate) requested_ref: &'a str,
    /// A commit already accepted by a batch preflight; skips re-resolution.
    pub(crate) accepted_sha: Option<&'a str>,
}

/// Whether a clean configured checkout already at the resolved commit may be
/// used directly instead of materializing a disposable tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InPlace {
    Allow,
    Never,
}

pub(crate) fn resolve_exact_ref(
    component: &Component,
    requested_ref: &str,
) -> Result<ExactRefIdentity> {
    validate_component_source(component)?;
    let source_root = source_root(component)?;
    let resolution = resolve_commit(&source_root, requested_ref, component)?;
    Ok(ExactRefIdentity {
        requested_ref: requested_ref.to_string(),
        resolved_sha: resolution.sha,
        source: resolution.source,
        resolution_mode: resolution.mode,
    })
}

struct ResolvedRequest<'a> {
    component: &'a Component,
    identity: ExactRefIdentity,
    source_root: PathBuf,
    prefix: Option<String>,
}

impl ExactRefCheckout {
    /// Materialize one component at `requested_ref` in an isolated tree.
    pub(crate) fn materialize(
        component: &Component,
        requested_ref: &str,
        accepted_sha: Option<&str>,
        project: Option<&homeboy_core::project::Project>,
    ) -> Result<Self> {
        let mut checkouts = Self::materialize_all(
            &[ExactRefRequest {
                component,
                requested_ref,
                accepted_sha,
            }],
            project,
            InPlace::Never,
        )?;
        Ok(checkouts.remove(0))
    }

    /// Resolve every request once, then provide one source tree per
    /// (repository, commit): the configured checkout itself when `in_place`
    /// allows it and the checkout is provably that commit, otherwise a single
    /// sparse checkout containing the requested components and their local dependencies.
    pub(crate) fn materialize_all(
        requests: &[ExactRefRequest<'_>],
        project: Option<&homeboy_core::project::Project>,
        in_place: InPlace,
    ) -> Result<Vec<Self>> {
        let resolved = requests
            .iter()
            .map(resolve_request)
            .collect::<Result<Vec<_>>>()?;

        // Group by (repository, commit), preserving request order.
        let mut groups: Vec<Vec<usize>> = Vec::new();
        for (index, request) in resolved.iter().enumerate() {
            match groups.iter_mut().find(|members| {
                let first = &resolved[members[0]];
                first.source_root == request.source_root
                    && first.identity.resolved_sha == request.identity.resolved_sha
            }) {
                Some(members) => members.push(index),
                None => groups.push(vec![index]),
            }
        }

        let mut checkouts: Vec<Option<Self>> = resolved.iter().map(|_| None).collect();
        for members in groups {
            let first = &resolved[members[0]];
            let sha = first.identity.resolved_sha.as_str();
            let prefixes: Vec<Option<&str>> = members
                .iter()
                .map(|&index| resolved[index].prefix.as_deref())
                .collect();
            let dependency_prefixes = local_dependency_prefixes(&first.source_root, sha, &prefixes);
            let mut source_prefixes = prefixes.clone();
            source_prefixes.extend(dependency_prefixes.iter().map(|prefix| {
                if prefix == "." {
                    None
                } else {
                    Some(prefix.as_str())
                }
            }));
            let tree = if in_place == InPlace::Allow
                && checkout_is_exact_commit(&first.source_root, sha, &source_prefixes)
            {
                SourceTree::InPlace(first.source_root.clone())
            } else {
                SourceTree::Materialized(Arc::new(materialize_tree(
                    &first.source_root,
                    sha,
                    &source_prefixes,
                    &dependency_prefixes,
                )?))
            };
            for &index in &members {
                checkouts[index] = Some(bind_source(&resolved[index], &tree, project)?);
            }
        }
        Ok(checkouts.into_iter().flatten().collect())
    }

    pub(crate) fn verify(&self) -> Result<()> {
        let actual_sha = git::run_git(
            self.tree.root(),
            &["rev-parse", "--verify", "HEAD^{commit}"],
            "verify exact deploy ref worktree",
        )?
        .trim()
        .to_string();
        if actual_sha != self.identity.resolved_sha {
            return Err(Error::validation_invalid_argument(
                "ref",
                format!(
                    "Materialized source verification failed for component '{}': expected commit '{}', found '{}'",
                    self.component.id, self.identity.resolved_sha, actual_sha
                ),
                None,
                None,
            ));
        }
        Ok(())
    }

    /// Hydrate dependencies in the selected source tree so an exact-ref build is
    /// self-contained. An in-place tree is the configured checkout, already
    /// proven to be the resolved commit, so this matches an ordinary local build.
    pub(crate) fn hydrate_dependencies(
        &self,
        skip: bool,
    ) -> Result<Option<deps::DependencyInstallResult>> {
        let started = Instant::now();
        let path = Path::new(&self.component.local_path);
        if skip {
            homeboy_core::log_status!(
                "deploy",
                "phase=exact_ref_hydration_skipped component={} ref={} commit={} cwd={} reason=--skip-deps-hydration duration_ms={}",
                self.component.id,
                self.identity.requested_ref,
                self.identity.resolved_sha,
                path.display(),
                started.elapsed().as_millis()
            );
            return Ok(None);
        }
        homeboy_core::log_status!(
            "deploy",
            "phase=exact_ref_hydration component={} ref={} commit={} cwd={}",
            self.component.id,
            self.identity.requested_ref,
            self.identity.resolved_sha,
            path.display()
        );

        match deps::install_for_resolved(&self.component, path)? {
            Some(result) => {
                for install in &result.installs {
                    homeboy_core::log_status!(
                        "deploy",
                        "phase=exact_ref_hydration component={} ref={} commit={} provider={} cwd={} command={} status={:?}",
                        self.component.id,
                        self.identity.requested_ref,
                        self.identity.resolved_sha,
                        result.package_manager,
                        result.component_path,
                        homeboy_core::redaction::redact_string(&install.command.join(" ")),
                        install.status
                    );
                }
                homeboy_core::log_status!(
                    "deploy",
                    "phase=exact_ref_hydration_complete component={} ref={} commit={} cwd={} duration_ms={}",
                    self.component.id,
                    self.identity.requested_ref,
                    self.identity.resolved_sha,
                    result.component_path,
                    started.elapsed().as_millis()
                );
                Ok(Some(result))
            }
            None => {
                homeboy_core::log_status!(
                    "deploy",
                    "phase=exact_ref_hydration_skipped component={} ref={} commit={} cwd={} reason=no_dependency_provider duration_ms={}",
                    self.component.id,
                    self.identity.requested_ref,
                    self.identity.resolved_sha,
                    path.display(),
                    started.elapsed().as_millis()
                );
                Ok(None)
            }
        }
    }

    /// Ensure the selected source carries every explicitly declared package input
    /// before the ordinary component build consumes time or produces output.
    pub(crate) fn reconstruct_package_artifacts(&self) -> Result<()> {
        for artifact in &self.component.package_artifacts {
            let path = Path::new(&self.component.local_path).join(&artifact.path);
            if glob::glob(&path.to_string_lossy())
                .ok()
                .is_some_and(|mut matches| matches.any(|entry| entry.is_ok()))
            {
                continue;
            }
            let command = artifact.reconstruct_command.as_deref().filter(|value| !value.trim().is_empty()).ok_or_else(|| {
                Error::validation_invalid_argument(
                    "package_artifacts.reconstruct_command",
                    format!("Required package artifact '{}' is missing from exact-ref source for component '{}'. Declare package_artifacts[].reconstruct_command to generate it before deploying this ref.", artifact.path, self.component.id),
                    Some(artifact.path.clone()),
                    Some(vec![format!("Declare package_artifacts[].reconstruct_command to generate '{}' before deploying this ref.", artifact.path)]),
                )
            })?;
            let output = std::process::Command::new("sh")
                .args(["-c", command])
                .current_dir(&self.component.local_path)
                .output()
                .map_err(|error| {
                    Error::internal_io(
                        format!("Failed to run package artifact reconstruction: {error}"),
                        None,
                    )
                })?;
            if !output.status.success() {
                return Err(Error::validation_invalid_argument(
                    "package_artifacts.reconstruct_command",
                    format!(
                        "Package artifact reconstruction failed for '{}' (exit {:?}): {}",
                        artifact.path,
                        output.status.code(),
                        String::from_utf8_lossy(&output.stderr).trim()
                    ),
                    Some(artifact.path.clone()),
                    None,
                ));
            }
            if !glob::glob(&path.to_string_lossy())
                .ok()
                .is_some_and(|mut matches| matches.any(|entry| entry.is_ok()))
            {
                return Err(Error::validation_invalid_argument(
                    "package_artifacts.path",
                    format!(
                        "Package artifact reconstruction completed but '{}' is still missing",
                        artifact.path
                    ),
                    Some(artifact.path.clone()),
                    None,
                ));
            }
        }
        Ok(())
    }
}

fn resolve_request<'a>(request: &ExactRefRequest<'a>) -> Result<ResolvedRequest<'a>> {
    let component = request.component;
    let source_root = source_root(component)?;
    let identity = match request.accepted_sha {
        Some(resolved_sha) => ExactRefIdentity {
            requested_ref: request.requested_ref.to_string(),
            resolved_sha: resolved_sha.to_string(),
            source: source_root.to_string_lossy().to_string(),
            resolution_mode: "release-set-preflight".to_string(),
        },
        None => resolve_exact_ref(component, request.requested_ref)?,
    };
    // A local resolution already proved the commit exists in the source object
    // store; only remote or preflighted SHAs may still need fetching.
    if identity.resolution_mode != "local" {
        ensure_resolved_commit_is_available(&source_root, component, &identity)?;
    }
    Ok(ResolvedRequest {
        component,
        identity,
        prefix: git::get_component_path_prefix(&component.local_path),
        source_root,
    })
}

/// Whether the configured checkout is provably `sha` for every prefix: HEAD is
/// exactly that commit and no tracked change, untracked file, or ignored file
/// exists under any prefix. Ignored files count because stale build outputs or
/// dependency trees would otherwise leak local bytes into the artifact.
fn checkout_is_exact_commit(source_root: &Path, sha: &str, prefixes: &[Option<&str>]) -> bool {
    let head = git::run_git(
        source_root,
        &["rev-parse", "--verify", "HEAD^{commit}"],
        "read exact deploy ref checkout head",
    );
    if !head.is_ok_and(|head| head.trim() == sha) {
        return false;
    }
    let mut args = vec![
        "status",
        "--porcelain=v1",
        "--untracked-files=all",
        "--ignored=matching",
        "--",
    ];
    if prefixes.iter().any(Option::is_none) {
        args.push(".");
    } else {
        args.extend(prefixes.iter().flatten().copied());
    }
    git::run_git(
        source_root,
        &args,
        "check exact deploy ref checkout cleanliness",
    )
    .is_ok_and(|status| status.trim().is_empty())
}

/// Resolve simple relative directories lexically, without consulting checkout bytes.
/// Reject URLs, globs, absolute paths, and paths escaping the repository.
fn resolve_local_directory(prefix: &str, relative: &str) -> Option<String> {
    if relative.is_empty()
        || relative.starts_with(['/', '~'])
        || relative.contains([':', '\\', '*', '?', '[', ']', '{', '}'])
    {
        return None;
    }
    let mut parts: Vec<&str> = prefix
        .split('/')
        .filter(|part| *part != "." && !part.is_empty())
        .collect();
    for part in relative.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            ".git" => return None,
            part => parts.push(part),
        }
    }
    Some(if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    })
}

/// Discover local package inputs from the selected commit, never the working
/// copy. The visited set handles cycles and shared transitive dependencies.
fn local_dependency_prefixes(
    source_root: &Path,
    sha: &str,
    prefixes: &[Option<&str>],
) -> Vec<String> {
    let mut pending: Vec<String> = prefixes
        .iter()
        .map(|prefix| prefix.unwrap_or(".").to_string())
        .collect();
    let mut visited: HashSet<String> = pending.iter().cloned().collect();
    let mut added = Vec::new();
    while let Some(prefix) = pending.pop() {
        for manifest in ["package.json", "composer.json"] {
            let path = if prefix == "." {
                manifest.to_string()
            } else {
                format!("{prefix}/{manifest}")
            };
            let Ok(contents) = git::run_git(
                source_root,
                &["show", &format!("{sha}:{path}")],
                "read exact-ref dependency manifest",
            ) else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&contents) else {
                continue;
            };
            let mut relatives = Vec::new();
            if manifest == "package.json" {
                for section in [
                    "dependencies",
                    "devDependencies",
                    "optionalDependencies",
                    "peerDependencies",
                ] {
                    if let Some(dependencies) = value[section].as_object() {
                        relatives.extend(dependencies.values().filter_map(|value| {
                            let value = value.as_str()?;
                            value
                                .strip_prefix("file:")
                                .or_else(|| value.strip_prefix("link:"))
                        }));
                    }
                }
            } else if let Some(repositories) = value["repositories"].as_array() {
                relatives.extend(
                    repositories
                        .iter()
                        .filter(|repository| repository["type"] == "path")
                        .filter_map(|repository| repository["url"].as_str()),
                );
            }
            for relative in relatives {
                if let Some(path) = resolve_local_directory(&prefix, relative) {
                    if visited.insert(path.clone()) {
                        pending.push(path.clone());
                        added.push(path);
                    }
                }
            }
        }
    }
    added.sort();
    added
}

/// Create one disposable checkout of `sha`. When every component lives below
/// the repository root, a cone-mode sparse checkout limits the working tree to
/// those prefixes (including local dependencies) plus repository-root files.
fn materialize_tree(
    source_root: &Path,
    sha: &str,
    prefixes: &[Option<&str>],
    dependency_prefixes: &[String],
) -> Result<MaterializedTree> {
    // The exact-ref worktree used to land in `std::env::temp_dir()`, which put
    // it outside every cleanup surface -- no owner record, no pin, no run
    // binding, invisible to `cleanup --include runtime-tmp` and to the
    // retained-storage report -- and broke outright on a `noexec` `/tmp`
    // (#11128). The artifact root is the volume the operator already sized for
    // run bytes, so it is where a checkout belongs.
    let parent = materialized_trees_root(&homeboy_core::artifacts::root()?);
    std::fs::create_dir_all(&parent).map_err(|err| {
        Error::internal_io(
            format!("Failed to create exact-ref deploy checkout directory: {err}"),
            Some("deploy.ref.temp".to_string()),
        )
    })?;
    let tree = MaterializedTree(parent.join(uuid::Uuid::new_v4().to_string()));
    let tree_arg = tree.0.to_string_lossy().to_string();
    clone_exact_ref_source(
        source_root,
        source_root.to_str().unwrap_or_default(),
        &tree_arg,
    )?;
    if prefixes.iter().all(Option::is_some) {
        git::run_git(
            &tree.0,
            &["sparse-checkout", "init", "--cone"],
            "enable sparse exact deploy ref checkout",
        )?;
        let mut args = vec!["sparse-checkout", "set", "--"];
        args.extend(prefixes.iter().flatten().copied());
        git::run_git(
            &tree.0,
            &args,
            "limit exact deploy ref checkout to components",
        )?;
    }
    homeboy_core::log_status!(
        "deploy",
        "phase=exact_ref_materialize commit={} cwd={} prefixes={} dependency_prefixes={}",
        sha,
        tree.0.display(),
        prefixes
            .iter()
            .map(|prefix| prefix.unwrap_or("."))
            .collect::<Vec<_>>()
            .join(","),
        dependency_prefixes.join(",")
    );
    git::run_git(
        &tree.0,
        &["checkout", "--detach", sha],
        "materialize exact deploy ref",
    )?;
    Ok(tree)
}

/// Point one resolved component at its source tree. Materialized components
/// reload source-owned configuration from the selected commit.
fn bind_source(
    request: &ResolvedRequest<'_>,
    tree: &SourceTree,
    project: Option<&homeboy_core::project::Project>,
) -> Result<ExactRefCheckout> {
    let component = request.component;
    let checkout = |component: Component| ExactRefCheckout {
        component,
        identity: request.identity.clone(),
        tree: tree.clone(),
    };
    let SourceTree::Materialized(materialized_tree) = tree else {
        homeboy_core::log_status!(
            "deploy",
            "phase=exact_ref_in_place component={} ref={} commit={} cwd={}",
            component.id,
            request.identity.requested_ref,
            request.identity.resolved_sha,
            component.local_path
        );
        return Ok(checkout(component.clone()));
    };

    let materialized_path = request
        .prefix
        .as_deref()
        .map(|prefix| materialized_tree.0.join(prefix))
        .unwrap_or_else(|| materialized_tree.0.clone());
    if !materialized_path.exists() {
        return Err(Error::validation_invalid_argument(
            "ref",
            format!(
                "Resolved ref does not contain the declared component path for '{}'",
                component.id
            ),
            None,
            None,
        ));
    }

    let mut materialized = component.clone();
    materialized.local_path = materialized_path.to_string_lossy().to_string();
    materialized.build_artifact = component.build_artifact.as_deref().map(|artifact| {
        rebase_source_path(
            artifact,
            Path::new(&component.local_path),
            &materialized_path,
        )
    });
    if let Some(mut source_config) =
        homeboy_core::component::try_discover_from_portable(&materialized_path)?
    {
        // Source-owned configuration must match the selected commit.
        source_config.id = materialized.id.clone();
        source_config.local_path = materialized.local_path.clone();
        source_config.build_artifact = source_config.build_artifact.as_deref().map(|artifact| {
            rebase_source_path(
                artifact,
                Path::new(&component.local_path),
                &materialized_path,
            )
        });
        materialized = match project {
            Some(project) => {
                let attachment = project
                    .components
                    .iter()
                    .find(|attachment| attachment.id == component.id)
                    .ok_or_else(|| {
                        Error::validation_invalid_argument(
                            "components",
                            format!(
                                "Project '{}' has no attached component '{}'",
                                project.id, component.id
                            ),
                            Some(project.id.clone()),
                            None,
                        )
                    })?;
                homeboy_core::project::bind_materialized_component_to_project(
                    source_config,
                    project,
                    None,
                    attachment.remote_path.clone(),
                    attachment.deployment_provider.clone(),
                )
            }
            None => source_config,
        };
    }
    // The tree is deleted after the deploy, so builds can skip restoring the
    // development state they would otherwise leave behind for an operator.
    materialized
        .env
        .insert(BUILD_SOURCE_DISPOSABLE_ENV.to_string(), "1".to_string());
    Ok(checkout(materialized))
}

fn ensure_resolved_commit_is_available(
    source_root: &Path,
    component: &Component,
    identity: &ExactRefIdentity,
) -> Result<()> {
    if git::run_git(
        source_root,
        &[
            "rev-parse",
            "--verify",
            &format!("{}^{{commit}}", identity.resolved_sha),
        ],
        "check exact deploy ref availability",
    )
    .is_ok()
    {
        return Ok(());
    }

    let remote = git::resolve_default_remote(source_root);
    let remote_url = git::remote_url(source_root, &remote)
        .ok_or_else(|| unresolvable_ref(&identity.requested_ref, source_root, &component.id))?;
    let transport_env = component_transport_env(component, &remote_url);
    homeboy_core::log_status!(
        "deploy",
        "phase=exact_ref_fetch component={} ref={} commit={}",
        component.id,
        identity.requested_ref,
        identity.resolved_sha
    );
    git::fetch_remote_tracking_refs_until(
        source_root,
        &[
            "fetch",
            "--no-tags",
            "--no-write-fetch-head",
            &remote,
            &format!("+{}:", identity.resolved_sha),
        ],
        "fetch preflighted exact deploy ref",
        &transport_env,
        std::time::Instant::now() + REMOTE_REF_QUERY_TIMEOUT,
    )
    .map_err(|error| remote_transport_error(&remote, &component.id, &error))?;
    let fetched = git::run_git(
        source_root,
        &[
            "rev-parse",
            "--verify",
            &format!("{}^{{commit}}", identity.resolved_sha),
        ],
        "verify fetched preflighted exact deploy ref",
    )
    .map_err(|_| unresolvable_ref(&identity.requested_ref, source_root, &component.id))?;
    if fetched.trim() == identity.resolved_sha {
        Ok(())
    } else {
        Err(identity_mismatch(&identity.requested_ref, &component.id))
    }
}

fn rebase_source_path(value: &str, original_root: &Path, materialized_root: &Path) -> String {
    let path = Path::new(value);
    if !path.is_absolute() {
        return value.to_string();
    }

    path.strip_prefix(original_root)
        .map(|relative| {
            materialized_root
                .join(relative)
                .to_string_lossy()
                .to_string()
        })
        .unwrap_or_else(|_| value.to_string())
}

fn validate_component_source(component: &Component) -> Result<()> {
    if component.is_file_component() {
        return Err(unsupported_source(
            component,
            "file deploy sources are not Git trees",
        ));
    }
    if component.deploy_config().is_git_deploy() {
        return Err(unsupported_source(
            component,
            "deploy_strategy 'git' updates a remote checkout instead of packaging the selected tree",
        ));
    }
    Ok(())
}

fn unsupported_source(component: &Component, reason: &str) -> Error {
    Error::validation_invalid_argument(
        "ref",
        format!(
            "Component '{}' does not support --ref: {reason}",
            component.id
        ),
        None,
        None,
    )
}

fn source_root(component: &Component) -> Result<PathBuf> {
    git::get_git_root(&component.local_path)
        .map(PathBuf::from)
        .map_err(|_| {
            Error::validation_invalid_argument(
                "ref",
                format!(
                    "Cannot use --ref for component '{}': declared source '{}' is not a Git repository",
                    component.id, component.local_path
                ),
                None,
                None,
            )
        })
}

struct ResolvedCommit {
    sha: String,
    source: String,
    mode: String,
}

fn resolve_commit(
    source_root: &Path,
    requested_ref: &str,
    component: &Component,
) -> Result<ResolvedCommit> {
    if requested_ref.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "ref",
            "--ref must not be empty",
            None,
            None,
        ));
    }
    let commit_ref = format!("{requested_ref}^{{commit}}");
    if let Ok(sha) = git::run_git(
        source_root,
        &["rev-parse", "--verify", &commit_ref],
        "resolve exact deploy ref",
    ) {
        return Ok(ResolvedCommit {
            sha: sha.trim().to_string(),
            source: source_root.to_string_lossy().to_string(),
            mode: "local".to_string(),
        });
    }

    resolve_remote_commit(source_root, requested_ref, component)
}

fn resolve_remote_commit(
    source_root: &Path,
    requested_ref: &str,
    component: &Component,
) -> Result<ResolvedCommit> {
    let component_id = &component.id;
    let remote = git::resolve_default_remote(source_root);
    let Some(remote_url) = git::remote_url(source_root, &remote) else {
        return Err(unresolvable_ref(requested_ref, source_root, component_id));
    };
    let transport_env = component_transport_env(component, &remote_url);

    let (sha, _remote_ref) = resolve_named_remote_ref(
        source_root,
        &remote,
        requested_ref,
        component_id,
        &transport_env,
    )?;
    if is_full_sha(requested_ref) && !sha.eq_ignore_ascii_case(requested_ref) {
        return Err(identity_mismatch(requested_ref, component_id));
    }
    Ok(ResolvedCommit {
        sha,
        source: format!("remote:{remote}"),
        mode: if is_full_sha(requested_ref) {
            "remote_sha".to_string()
        } else {
            "remote_named_ref".to_string()
        },
    })
}

fn resolve_named_remote_ref(
    source_root: &Path,
    remote: &str,
    requested_ref: &str,
    component_id: &str,
    transport_env: &[(String, String)],
) -> Result<(String, String)> {
    let args = if is_full_sha(requested_ref) {
        vec!["ls-remote", "--refs", remote]
    } else {
        vec!["ls-remote", "--refs", remote, requested_ref]
    };
    homeboy_core::log_status!(
        "deploy",
        "phase=exact_ref_remote_query component={} ref={}",
        component_id,
        requested_ref
    );
    let output = git::run_git_with_env_timeout(
        source_root,
        &args,
        "query named exact deploy ref",
        transport_env,
        REMOTE_REF_QUERY_TIMEOUT,
    )
    .map_err(|err| remote_transport_error(remote, component_id, &err))?;
    let candidates: Vec<(&str, &str)> = output
        .lines()
        .filter_map(|line| line.split_once(char::is_whitespace))
        .filter_map(|(sha, reference)| {
            let reference = reference.trim();
            (is_full_sha(requested_ref) && sha.eq_ignore_ascii_case(requested_ref)
                || remote_ref_matches(reference, requested_ref))
            .then_some((sha, reference))
        })
        .collect();
    match candidates.as_slice() {
        [(sha, reference)] => Ok(((*sha).to_string(), (*reference).to_string())),
        candidates if is_full_sha(requested_ref)
            && candidates.iter().all(|(sha, _)| sha.eq_ignore_ascii_case(requested_ref)) =>
        {
            Ok((requested_ref.to_string(), candidates[0].1.to_string()))
        }
        [] => Err(unresolvable_ref(requested_ref, source_root, component_id)),
        _ => Err(Error::validation_invalid_argument(
            "ref",
            format!(
                "Cannot resolve --ref '{}' unambiguously from declared Git remote '{}' for component '{}'",
                requested_ref, remote, component_id
            ),
            None,
            None,
        )),
    }
}

fn component_transport_env(component: &Component, remote_url: &str) -> Vec<(String, String)> {
    remote_host(remote_url)
        .map(|host| homeboy_core::git::github_cli_env(&host, &component.github))
        .unwrap_or_default()
}

fn remote_host(remote_url: &str) -> Option<String> {
    let remote_url = remote_url.trim();
    let authority = remote_url
        .strip_prefix("https://")
        .or_else(|| remote_url.strip_prefix("http://"))
        .or_else(|| remote_url.strip_prefix("ssh://"))
        .or_else(|| remote_url.split_once('@').map(|(_, value)| value))?;
    let host = authority
        .split('@')
        .next_back()?
        .split('/')
        .next()?
        .split(':')
        .next()?
        .trim();
    (!host.is_empty()).then(|| host.to_string())
}

fn remote_ref_matches(reference: &str, requested_ref: &str) -> bool {
    reference == requested_ref
        || reference == format!("refs/heads/{requested_ref}")
        || reference == format!("refs/tags/{requested_ref}")
}

fn is_full_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn unresolvable_ref(requested_ref: &str, source_root: &Path, component_id: &str) -> Error {
    Error::validation_invalid_argument(
        "ref",
        format!(
            "Cannot resolve --ref '{}' to an unambiguous commit in declared repository '{}' for component '{}'",
            requested_ref,
            source_root.display(),
            component_id
        ),
        None,
        None,
    )
}

fn identity_mismatch(requested_ref: &str, component_id: &str) -> Error {
    Error::validation_invalid_argument(
        "ref",
        format!(
            "Declared Git remote resolved exact SHA '{}' to a different commit for component '{}'",
            requested_ref, component_id
        ),
        None,
        Some(vec![
            "Use the immutable commit SHA returned by the repository, then retry the deploy."
                .to_string(),
        ]),
    )
}

fn remote_transport_error(remote: &str, component_id: &str, error: &Error) -> Error {
    let detail = error
        .details
        .get("stderr")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let message = if [
        "authentication",
        "authorization",
        "could not read username",
        "terminal prompts disabled",
        "http 401",
        "http 403",
    ]
    .iter()
    .any(|needle| detail.contains(needle))
    {
        format!(
            "Git authentication failed while querying declared remote '{}' for component '{}'",
            remote, component_id
        )
    } else if [
        "could not resolve host",
        "connection refused",
        "connection timed out",
        "network is unreachable",
        "proxy",
    ]
    .iter()
    .any(|needle| detail.contains(needle))
    {
        format!(
            "Git connectivity failed while querying declared remote '{}' for component '{}'",
            remote, component_id
        )
    } else {
        format!(
            "Unable to query declared Git remote '{}' for component '{}'",
            remote, component_id
        )
    };
    Error::validation_invalid_argument(
        "ref",
        message,
        None,
        Some(vec!["Check the remote URL, configured Git authentication, and transport proxy settings. Git credentials are not included in this error.".to_string()]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn exact_sha_branch_and_tag_resolve_to_immutable_commit() {
        let repo = fixture_repo();
        let component = fixture_component(repo.path());
        let sha = git_output(repo.path(), &["rev-parse", "HEAD"]);

        assert_eq!(
            resolve_exact_ref(&component, &sha)
                .expect("exact SHA")
                .resolved_sha,
            sha
        );
        assert_eq!(
            resolve_exact_ref(&component, "accepted")
                .expect("branch")
                .resolved_sha,
            sha
        );
        assert_eq!(
            resolve_exact_ref(&component, "v1.0.0")
                .expect("tag")
                .resolved_sha,
            sha
        );
    }

    #[test]
    fn remote_host_supports_https_and_ssh_transports() {
        assert_eq!(
            remote_host("https://git.example.test/owner/repo.git"),
            Some("git.example.test".to_string())
        );
        assert_eq!(
            remote_host("ssh://git@git.example.test:2222/owner/repo.git"),
            Some("git.example.test".to_string())
        );
        assert_eq!(
            remote_host("git@git.example.test:owner/repo.git"),
            Some("git.example.test".to_string())
        );
    }

    #[test]
    fn exact_ref_transport_uses_component_host_policy_without_exposing_values() {
        let mut component = Component::default();
        component.github.hosts.insert(
            "git.example.test".to_string(),
            homeboy_core::component::GithubHostConfig {
                proxy: Some("socks5://127.0.0.1:9911".to_string()),
                env: std::collections::HashMap::from([(
                    "GIT_ASKPASS".to_string(),
                    "/private/credential-helper".to_string(),
                )]),
            },
        );

        let env = component_transport_env(&component, "https://git.example.test/acme/repo.git");

        assert!(env.contains(&(
            "HTTPS_PROXY".to_string(),
            "socks5://127.0.0.1:9911".to_string()
        )));
        assert!(env.contains(&(
            "GIT_ASKPASS".to_string(),
            "/private/credential-helper".to_string()
        )));
    }

    #[test]
    fn materialized_ref_is_independent_of_stale_checkout_head_and_does_not_mutate_it() {
        let repo = fixture_repo();
        let component = fixture_component(repo.path());
        let accepted_sha = git_output(repo.path(), &["rev-parse", "accepted"]);
        std::fs::write(repo.path().join("payload.txt"), "newer checkout\n").expect("write");
        git(repo.path(), &["add", "payload.txt"]);
        commit(repo.path(), "new checkout head");
        let checkout_head = git_output(repo.path(), &["rev-parse", "HEAD"]);
        assert_ne!(accepted_sha, checkout_head);

        let worktree_path = {
            let checkout = ExactRefCheckout::materialize(&component, "accepted", None, None)
                .expect("materialize accepted branch");
            assert_eq!(checkout.identity.resolved_sha, accepted_sha);
            assert_eq!(
                std::fs::read_to_string(
                    Path::new(&checkout.component.local_path).join("payload.txt")
                )
                .expect("materialized payload"),
                "accepted\n"
            );
            PathBuf::from(&checkout.component.local_path)
        };

        assert!(
            !worktree_path.exists(),
            "temporary worktree should be removed"
        );
        assert_eq!(
            git_output(repo.path(), &["rev-parse", "HEAD"]),
            checkout_head
        );
        assert_eq!(
            std::fs::read_to_string(repo.path().join("payload.txt")).expect("checkout payload"),
            "newer checkout\n"
        );
    }

    #[test]
    fn dry_resolution_is_read_only_and_unresolvable_refs_fail_clearly() {
        let repo = fixture_repo();
        let component = fixture_component(repo.path());
        let before = git_output(repo.path(), &["status", "--porcelain=v1"]);
        let identity = resolve_exact_ref(&component, "accepted").expect("resolve branch");

        assert_eq!(identity.requested_ref, "accepted");
        assert_eq!(
            Path::new(&identity.source)
                .canonicalize()
                .expect("canonical source"),
            repo.path().canonicalize().expect("canonical repo")
        );
        assert_eq!(
            git_output(repo.path(), &["status", "--porcelain=v1"]),
            before
        );
        let err =
            resolve_exact_ref(&component, "missing-ref").expect_err("missing ref should fail");
        assert!(err.message.contains("Cannot resolve --ref 'missing-ref'"));
        assert!(err.message.contains(&identity.source));
    }

    #[test]
    fn stale_checkout_fetches_exact_sha_and_named_ref_without_changing_head_or_index() {
        let fixture = remote_fixture();
        let sha_checkout = stale_clone(&fixture);
        let sha_component = fixture_component(&sha_checkout);
        let sha_head = git_output(&sha_checkout, &["rev-parse", "HEAD"]);
        let sha_index = git_output(&sha_checkout, &["write-tree"]);

        let sha_identity = resolve_exact_ref(&sha_component, &fixture.target_sha)
            .expect("fetch exact SHA from remote");
        assert_eq!(sha_identity.resolved_sha, fixture.target_sha);
        assert_eq!(sha_identity.source, "remote:origin");
        assert_eq!(sha_identity.resolution_mode, "remote_sha");
        assert_eq!(git_output(&sha_checkout, &["rev-parse", "HEAD"]), sha_head);
        assert_eq!(git_output(&sha_checkout, &["write-tree"]), sha_index);

        let named_checkout = stale_clone(&fixture);
        let named_component = fixture_component(&named_checkout);
        let named_head = git_output(&named_checkout, &["rev-parse", "HEAD"]);
        let named_index = git_output(&named_checkout, &["write-tree"]);
        let named_state = materialization_source_state(&named_checkout);
        let checkout = ExactRefCheckout::materialize(&named_component, "accepted", None, None)
            .expect("fetch and materialize named remote ref");
        assert_eq!(checkout.identity.resolved_sha, fixture.target_sha);
        assert_eq!(checkout.identity.resolution_mode, "remote_named_ref");
        assert_eq!(
            git_output(&named_checkout, &["rev-parse", "HEAD"]),
            named_head
        );
        assert_eq!(git_output(&named_checkout, &["write-tree"]), named_index);
        assert_eq!(materialization_source_state(&named_checkout), named_state);
    }

    #[test]
    fn batch_preflight_leaves_every_checkout_unchanged_when_a_later_ref_fails() {
        let fixture = remote_fixture();
        let remote_only_checkout = stale_clone(&fixture);
        let remote_only_component = fixture_component(&remote_only_checkout);
        let failing_repo = fixture_repo();
        let failing_component = fixture_component(failing_repo.path());
        let before = [&remote_only_checkout, failing_repo.path()]
            .into_iter()
            .map(git_state_snapshot)
            .collect::<Vec<_>>();

        let error = crate::preflight_exact_refs(&[
            (&remote_only_component, "accepted"),
            (&failing_component, "missing-ref"),
        ])
        .expect_err("a later failed member must reject the complete preflight");

        assert!(error.message.contains("missing-ref"));
        for (path, expected) in [&remote_only_checkout, failing_repo.path()]
            .into_iter()
            .zip(before)
        {
            assert_eq!(git_state_snapshot(path), expected);
        }
    }

    #[test]
    fn preflighted_sha_is_materialized_after_the_requested_branch_moves() {
        let fixture = remote_fixture();
        let checkout = stale_clone(&fixture);
        let component = fixture_component(&checkout);
        let accepted_sha = crate::preflight_exact_refs(&[(&component, "accepted")])
            .expect("preflight accepted branch")
            .remove("fixture")
            .expect("accepted SHA");

        std::fs::write(checkout.join("payload.txt"), "moved\n").expect("moved payload");
        git(&checkout, &["add", "payload.txt"]);
        commit(&checkout, "move accepted branch");
        let moved_sha = git_output(&checkout, &["rev-parse", "HEAD"]);
        git(
            &checkout,
            &["push", "-q", "--force", "origin", "HEAD:accepted"],
        );
        assert_ne!(accepted_sha, moved_sha);

        let materialized =
            ExactRefCheckout::materialize(&component, "accepted", Some(&accepted_sha), None)
                .expect("materialize preflighted commit");
        assert_eq!(materialized.identity.resolved_sha, accepted_sha);
        assert_eq!(
            std::fs::read_to_string(
                Path::new(&materialized.component.local_path).join("payload.txt")
            )
            .expect("materialized payload"),
            "accepted\n"
        );
    }

    #[test]
    fn missing_remote_ref_and_transport_failure_are_actionable() {
        let fixture = remote_fixture();
        let checkout = stale_clone(&fixture);
        let component = fixture_component(&checkout);
        let missing = resolve_exact_ref(&component, "missing-ref").expect_err("missing ref");
        assert!(missing
            .message
            .contains("Cannot resolve --ref 'missing-ref'"));

        git(
            &checkout,
            &["remote", "set-url", "origin", "/missing/remote.git"],
        );
        let transport =
            resolve_exact_ref(&component, "also-missing").expect_err("transport failure");
        assert!(transport
            .message
            .contains("Unable to query declared Git remote"));
        assert!(transport.message.contains("remote 'origin'"));
    }

    #[test]
    fn materialized_ref_verification_rejects_a_different_checkout_head() {
        let repo = fixture_repo();
        let component = fixture_component(repo.path());
        std::fs::write(repo.path().join("other.txt"), "other\n").expect("other payload");
        git(repo.path(), &["add", "other.txt"]);
        commit(repo.path(), "other commit");
        let other_sha = git_output(repo.path(), &["rev-parse", "HEAD"]);
        let checkout = ExactRefCheckout::materialize(&component, "accepted", None, None)
            .expect("materialize accepted branch");
        git(
            Path::new(&checkout.component.local_path),
            &["checkout", "--detach", &other_sha],
        );

        let error = checkout
            .verify()
            .expect_err("changed materialized HEAD must fail closed");
        assert!(error
            .message
            .contains("Materialized source verification failed"));
    }

    #[test]
    fn hydration_failure_is_actionable_and_removes_only_the_temporary_checkout() {
        let repo = fixture_repo();
        std::fs::write(
            repo.path().join("homeboy.json"),
            r#"{"id":"fixture-deps","deps":{"provider":"failing-fixture","commands":{"install":{"argv":["sh","-c","exit 42"]}}}}"#,
        )
        .expect("dependency provider");
        git(repo.path(), &["add", "homeboy.json"]);
        commit(repo.path(), "add failing provider");
        let component = fixture_component(repo.path());

        let temporary_path = {
            let checkout = ExactRefCheckout::materialize(&component, "HEAD", None, None)
                .expect("materialize exact ref");
            let temporary_path = PathBuf::from(&checkout.component.local_path);
            let error = checkout
                .hydrate_dependencies(false)
                .expect_err("failed hydration must stop before build or deploy");
            assert!(error.message.contains("Dependency provider install failed"));
            assert!(format!("{error:?}").contains("Run manually in"));
            temporary_path
        };

        assert!(
            !temporary_path.exists(),
            "temporary checkout is removed on failure"
        );
        assert!(
            repo.path().join("homeboy.json").exists(),
            "configured checkout is never cleaned"
        );
    }

    #[test]
    fn hydration_opt_out_does_not_run_the_materialized_provider() {
        let repo = fixture_repo();
        std::fs::write(
            repo.path().join("homeboy.json"),
            r#"{"id":"fixture-deps","deps":{"provider":"failing-fixture","commands":{"install":{"argv":["sh","-c","exit 42"]}}}}"#,
        )
        .expect("dependency provider");
        git(repo.path(), &["add", "homeboy.json"]);
        commit(repo.path(), "add failing provider");
        let component = fixture_component(repo.path());
        let checkout =
            ExactRefCheckout::materialize(&component, "HEAD", None, None).expect("materialize");

        assert!(checkout
            .hydrate_dependencies(true)
            .expect("explicit opt-out must skip hydration")
            .is_none());
    }

    #[test]
    fn exact_ref_reconstructs_declared_package_artifact_before_build() {
        let repo = fixture_repo();
        let mut component = fixture_component(repo.path());
        component.package_artifacts = vec![homeboy_core::component::PackageArtifact {
            path: "generated/package.zip".to_string(),
            reconstruct_command: Some(
                "mkdir -p generated && printf package > generated/package.zip".to_string(),
            ),
        }];
        let checkout =
            ExactRefCheckout::materialize(&component, "HEAD", None, None).expect("materialize");

        checkout
            .reconstruct_package_artifacts()
            .expect("reconstruct package artifact");

        assert_eq!(
            std::fs::read_to_string(
                Path::new(&checkout.component.local_path).join("generated/package.zip")
            )
            .expect("artifact"),
            "package"
        );
        assert!(!repo.path().join("generated/package.zip").exists());
    }

    #[test]
    fn exact_ref_missing_package_artifact_requires_reconstruction_command_before_build() {
        let repo = fixture_repo();
        let mut component = fixture_component(repo.path());
        component.package_artifacts = vec![homeboy_core::component::PackageArtifact {
            path: "generated/package.zip".to_string(),
            reconstruct_command: None,
        }];
        let checkout =
            ExactRefCheckout::materialize(&component, "HEAD", None, None).expect("materialize");

        let error = checkout
            .reconstruct_package_artifacts()
            .expect_err("missing declaration fails");

        assert!(error.message.contains("Required package artifact"));
        assert!(error.message.contains("reconstruct_command"));
    }

    #[test]
    fn exact_ref_rejects_incompatible_deploy_source_types() {
        let repo = fixture_repo();
        let mut component = fixture_component(repo.path());
        component.deploy_strategy = Some("git".to_string());
        let git_error = resolve_exact_ref(&component, "accepted")
            .expect_err("git deploy strategy should be rejected");
        assert!(git_error
            .message
            .contains("deploy_strategy 'git' updates a remote checkout"));

        component.deploy_strategy = Some("file".to_string());
        component.local_path = repo
            .path()
            .join("payload.txt")
            .to_string_lossy()
            .to_string();
        let file_error = resolve_exact_ref(&component, "accepted")
            .expect_err("file deploy strategy should be rejected");
        assert!(file_error.message.contains("file deploy sources"));
    }

    #[test]
    fn cross_device_link_error_is_detected() {
        // Mirrors the real run_git failure, whose Display message carries the
        // git stderr detail (see git_failure_message).
        let cross_device = Error::internal_unexpected(
            "clone exact deploy ref source failed: fatal: failed to create link '/tmp/x/.git/objects/aa': Invalid cross-device link",
        );
        assert!(is_cross_device_link_error(&cross_device));

        // An unrelated git failure must not trigger the --no-hardlinks retry.
        let other = Error::internal_unexpected(
            "clone exact deploy ref source failed: fatal: repository not found",
        );
        assert!(!is_cross_device_link_error(&other));
    }

    #[test]
    fn clone_exact_ref_source_succeeds_on_same_filesystem() {
        // Happy path: --local clone within one filesystem (the tempdir) works
        // without needing the cross-device fallback.
        let repo = fixture_repo();
        let dest = tempfile::tempdir().expect("dest parent");
        let worktree = dest.path().join("clone");
        clone_exact_ref_source(
            repo.path(),
            repo.path().to_str().unwrap(),
            worktree.to_str().unwrap(),
        )
        .expect("same-filesystem clone succeeds");
        assert!(worktree.join(".git").exists());
    }

    /// Two components under one repository: `a/` and `b/`, plus an unrelated
    /// `c/` and a root file. Returns the repo and the commit before `HEAD`.
    fn monorepo_fixture() -> (tempfile::TempDir, String) {
        let repo = tempfile::tempdir().expect("repo");
        git(repo.path(), &["init", "-q"]);
        for dir in ["a", "b", "c"] {
            std::fs::create_dir_all(repo.path().join(dir)).expect("component dir");
            std::fs::write(repo.path().join(dir).join("payload.txt"), "old\n").expect("payload");
        }
        std::fs::write(repo.path().join("root.txt"), "root\n").expect("root file");
        std::fs::write(repo.path().join(".gitignore"), "*.log\n").expect("gitignore");
        git(repo.path(), &["add", "."]);
        commit(repo.path(), "old");
        let old_sha = git_output(repo.path(), &["rev-parse", "HEAD"]);
        for dir in ["a", "b"] {
            std::fs::write(repo.path().join(dir).join("payload.txt"), "new\n").expect("payload");
        }
        git(repo.path(), &["add", "."]);
        commit(repo.path(), "new");
        (repo, old_sha)
    }

    fn monorepo_components(root: &Path) -> Vec<Component> {
        ["a", "b"]
            .into_iter()
            .map(|dir| Component {
                id: dir.to_string(),
                local_path: root.join(dir).to_string_lossy().to_string(),
                ..Component::default()
            })
            .collect()
    }

    fn materialize_monorepo(
        components: &[Component],
        requested_ref: &str,
    ) -> Vec<ExactRefCheckout> {
        let requests: Vec<_> = components
            .iter()
            .map(|component| ExactRefRequest {
                component,
                requested_ref,
                accepted_sha: None,
            })
            .collect();
        ExactRefCheckout::materialize_all(&requests, None, InPlace::Allow).expect("materialize")
    }

    #[test]
    fn head_ref_builds_in_place_only_when_the_checkout_is_provably_that_commit() {
        type Mutation = fn(&Path);
        let cases: [(&str, Mutation, bool); 5] = [
            ("clean", |_| {}, true),
            (
                "modified tracked file",
                |root| {
                    std::fs::write(root.join("a/payload.txt"), "local\n").expect("modify");
                },
                false,
            ),
            (
                "untracked file",
                |root| {
                    std::fs::write(root.join("b/extra.txt"), "local\n").expect("untracked");
                },
                false,
            ),
            (
                "ignored file",
                |root| {
                    std::fs::write(root.join("a/stale.log"), "local\n").expect("ignored");
                },
                false,
            ),
            (
                "change outside component prefixes",
                |root| {
                    std::fs::write(root.join("c/payload.txt"), "local\n").expect("other");
                },
                true,
            ),
        ];
        for (name, mutate, expect_in_place) in cases {
            let (repo, _) = monorepo_fixture();
            let head = git_output(repo.path(), &["rev-parse", "HEAD"]);
            let components = monorepo_components(repo.path());
            mutate(repo.path());

            let checkouts = materialize_monorepo(&components, &head);

            assert_eq!(checkouts.len(), 2, "{name}");
            for (checkout, component) in checkouts.iter().zip(&components) {
                checkout.verify().expect("verify");
                assert_eq!(checkout.identity.resolved_sha, head, "{name}");
                assert_eq!(
                    matches!(checkout.tree, SourceTree::InPlace(_)),
                    expect_in_place,
                    "{name}"
                );
                assert_eq!(
                    checkout.component.local_path == component.local_path,
                    expect_in_place,
                    "{name}"
                );
                // Only a disposable tree lets builds skip restoring dev state.
                assert_eq!(
                    checkout
                        .component
                        .env
                        .get(BUILD_SOURCE_DISPOSABLE_ENV)
                        .is_some(),
                    !expect_in_place,
                    "{name}"
                );
                // Either source yields the committed bytes, never local edits.
                assert_eq!(
                    std::fs::read_to_string(
                        Path::new(&checkout.component.local_path).join("payload.txt")
                    )
                    .expect("payload"),
                    "new\n",
                    "{name}"
                );
                assert!(
                    !Path::new(&checkout.component.local_path)
                        .join("extra.txt")
                        .exists()
                        || expect_in_place,
                    "{name}"
                );
            }
        }
    }

    #[test]
    fn older_ref_materializes_one_sparse_tree_shared_by_every_component() {
        let (repo, old_sha) = monorepo_fixture();
        let components = monorepo_components(repo.path());

        let checkouts = materialize_monorepo(&components, &old_sha);

        let roots: Vec<&Path> = checkouts
            .iter()
            .map(|checkout| checkout.tree.root())
            .collect();
        assert_eq!(
            roots[0], roots[1],
            "one materialization per repository + commit"
        );
        let root = roots[0].to_path_buf();
        assert_ne!(root, repo.path());
        for checkout in &checkouts {
            checkout.verify().expect("verify");
            assert_eq!(checkout.identity.resolved_sha, old_sha);
            assert_eq!(
                std::fs::read_to_string(
                    Path::new(&checkout.component.local_path).join("payload.txt")
                )
                .expect("payload"),
                "old\n"
            );
        }
        assert!(
            root.join("root.txt").exists(),
            "repository-root files are kept"
        );
        assert!(
            !root.join("c").exists(),
            "unrequested prefixes are not checked out"
        );

        drop(checkouts);
        assert!(
            !root.exists(),
            "the shared tree is removed with its last component"
        );
        assert_eq!(git_output(repo.path(), &["status", "--porcelain"]), "");
    }

    fn local_dependency_fixture() -> (tempfile::TempDir, String, Component) {
        let repo = fixture_repo();
        for (path, contents) in [
            (
                "plugins/a/package.json",
                r#"{"dependencies":{"dep":"file:../../packages/dep"},"devDependencies":{"dev":"link:../../packages/dev"},"optionalDependencies":{"optional":"file:../../packages/optional"},"peerDependencies":{"peer":"file:../../packages/peer"}}"#,
            ),
            (
                "packages/dep/package.json",
                r#"{"dependencies":{"shared":"file:../shared"}}"#,
            ),
            (
                "packages/shared/package.json",
                r#"{"dependencies":{"cycle":"link:../dep"}}"#,
            ),
            ("packages/dev/package.json", "{}"),
            ("packages/optional/package.json", "not json"),
            ("packages/peer/payload.txt", "peer"),
            (
                "plugins/a/composer.json",
                r#"{"repositories":[{"type":"path","url":"../../packages/php"},{"type":"path","url":"../../packages/*"},{"type":"vcs","url":"../../unrelated"}]}"#,
            ),
            (
                "packages/php/composer.json",
                r#"{"repositories":[{"type":"path","url":"../php-shared"}]}"#,
            ),
            ("packages/php-shared/composer.json", "{}"),
            ("packages/unrelated/payload.txt", "unrelated"),
            ("unrelated/payload.txt", "unrelated"),
        ] {
            let path = repo.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).expect("directory");
            std::fs::write(path, contents).expect("fixture file");
        }
        git(repo.path(), &["add", "."]);
        commit(repo.path(), "local dependencies");
        let sha = git_output(repo.path(), &["rev-parse", "HEAD"]);
        let component = fixture_component(&repo.path().join("plugins/a"));
        (repo, sha, component)
    }

    #[test]
    fn older_ref_materializes_transitive_local_dependencies_from_target_manifests() {
        let (repo, sha, component) = local_dependency_fixture();
        std::fs::write(repo.path().join("plugins/a/package.json"), "{}").expect("new manifest");
        git(repo.path(), &["add", "."]);
        commit(repo.path(), "remove dependencies at HEAD");

        let checkouts = materialize_monorepo(&[component], &sha);
        let root = checkouts[0].tree.root();
        for path in [
            "packages/dep/package.json",
            "packages/shared/package.json",
            "packages/dev/package.json",
            "packages/optional/package.json",
            "packages/peer/payload.txt",
            "packages/php/composer.json",
            "packages/php-shared/composer.json",
        ] {
            assert!(root.join(path).exists(), "missing {path}");
        }
        assert!(!root.join("packages/unrelated").exists());
        assert!(!root.join("unrelated").exists());
        checkouts[0].verify().expect("verify");
    }

    #[test]
    fn escaping_local_dependency_is_ignored() {
        let (repo, _, component) = local_dependency_fixture();
        std::fs::write(
            repo.path().join("plugins/a/package.json"),
            r#"{"dependencies":{"escape":"file:../../../outside"}}"#,
        )
        .expect("manifest");
        std::fs::remove_file(repo.path().join("plugins/a/composer.json")).expect("remove composer");
        git(repo.path(), &["add", "."]);
        commit(repo.path(), "escaping dependency");
        let sha = git_output(repo.path(), &["rev-parse", "HEAD"]);
        assert!(local_dependency_prefixes(repo.path(), &sha, &[Some("plugins/a")]).is_empty());
        let checkout =
            ExactRefCheckout::materialize(&component, &sha, None, None).expect("materialize");
        assert!(!checkout.tree.root().join("packages").exists());
    }

    #[test]
    fn dirty_local_dependencies_prevent_in_place_builds() {
        for path in [
            "packages/dep/package.json",
            "packages/shared/package.json",
            "packages/shared/untracked.txt",
            "packages/shared/ignored.log",
        ] {
            let (repo, _, component) = local_dependency_fixture();
            std::fs::write(repo.path().join(".gitignore"), "*.log\n").expect("ignore");
            git(repo.path(), &["add", "."]);
            commit(repo.path(), "ignore logs");
            let clean = materialize_monorepo(&[component.clone()], "HEAD");
            assert!(matches!(clean[0].tree, SourceTree::InPlace(_)));
            std::fs::write(repo.path().join(path), "dirty").expect("dirty dependency");
            let checkouts = materialize_monorepo(&[component], "HEAD");
            assert!(
                matches!(checkouts[0].tree, SourceTree::Materialized(_)),
                "{path}"
            );
        }
    }

    #[test]
    fn local_directory_resolution_rejects_non_relative_directories() {
        for path in [
            "../../../outside",
            "/tmp/pkg",
            "https://example.test/pkg",
            "../*",
            "~/pkg",
            "../.git",
        ] {
            assert_eq!(resolve_local_directory("plugins/a", path), None, "{path}");
        }
        assert_eq!(
            resolve_local_directory("plugins/a", "../../packages/dep"),
            Some("packages/dep".to_string())
        );
        assert_eq!(
            resolve_local_directory("plugins/a", "../.."),
            Some(".".to_string())
        );
    }

    fn fixture_repo() -> tempfile::TempDir {
        let repo = tempfile::tempdir().expect("repo");
        git(repo.path(), &["init", "-q"]);
        git(repo.path(), &["config", "user.name", "Homeboy Test"]);
        git(
            repo.path(),
            &["config", "user.email", "homeboy@example.test"],
        );
        std::fs::write(repo.path().join("payload.txt"), "accepted\n").expect("payload");
        git(repo.path(), &["add", "payload.txt"]);
        commit(repo.path(), "accepted");
        git(repo.path(), &["branch", "accepted"]);
        git(repo.path(), &["tag", "v1.0.0"]);
        repo
    }

    fn fixture_component(path: &Path) -> Component {
        Component {
            id: "fixture".to_string(),
            local_path: path.to_string_lossy().to_string(),
            build_artifact: Some("build/fixture.zip".to_string()),
            ..Component::default()
        }
    }

    struct RemoteFixture {
        _root: tempfile::TempDir,
        remote: PathBuf,
        target_sha: String,
    }

    fn remote_fixture() -> RemoteFixture {
        let root = tempfile::tempdir().expect("fixture root");
        let remote = root.path().join("remote.git");
        let seed = root.path().join("seed");
        git(
            root.path(),
            &[
                "init",
                "--bare",
                "--initial-branch=main",
                remote.to_str().unwrap(),
            ],
        );
        git(
            root.path(),
            &[
                "clone",
                "-q",
                remote.to_str().unwrap(),
                seed.to_str().unwrap(),
            ],
        );
        git(&seed, &["config", "user.name", "Homeboy Test"]);
        git(&seed, &["config", "user.email", "homeboy@example.test"]);
        std::fs::write(seed.join("payload.txt"), "stale\n").expect("stale payload");
        git(&seed, &["add", "payload.txt"]);
        commit(&seed, "stale");
        git(&seed, &["push", "-q", "origin", "main"]);
        let stale_clone = root.path().join("stale-template");
        git(
            root.path(),
            &[
                "clone",
                "-q",
                remote.to_str().unwrap(),
                stale_clone.to_str().unwrap(),
            ],
        );
        std::fs::write(seed.join("payload.txt"), "accepted\n").expect("accepted payload");
        git(&seed, &["add", "payload.txt"]);
        commit(&seed, "accepted");
        let target_sha = git_output(&seed, &["rev-parse", "HEAD"]);
        git(&seed, &["branch", "accepted"]);
        git(&seed, &["push", "-q", "origin", "main", "accepted"]);
        RemoteFixture {
            _root: root,
            remote,
            target_sha,
        }
    }

    fn stale_clone(fixture: &RemoteFixture) -> PathBuf {
        let checkout = fixture
            .remote
            .parent()
            .expect("fixture parent")
            .join(uuid::Uuid::new_v4().to_string());
        let stale_template = fixture
            .remote
            .parent()
            .expect("fixture parent")
            .join("stale-template");
        git(
            fixture.remote.parent().expect("fixture parent"),
            &[
                "clone",
                "-q",
                stale_template.to_str().unwrap(),
                checkout.to_str().unwrap(),
            ],
        );
        git(
            &checkout,
            &[
                "remote",
                "set-url",
                "origin",
                fixture.remote.to_str().unwrap(),
            ],
        );
        // `git clone` does not inherit the source repository's local config, so
        // this checkout has no committer identity of its own. Tests that commit
        // into it therefore passed only on machines with a global git identity
        // and failed on CI with "Author identity unknown". Set it here, the same
        // way the fixture repository does after `git init`.
        git(&checkout, &["config", "user.name", "Homeboy Test"]);
        git(&checkout, &["config", "user.email", "homeboy@example.test"]);
        checkout
    }

    fn commit(path: &Path, message: &str) {
        git(
            path,
            &[
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=homeboy-test@example.com",
                "commit",
                "-q",
                "-m",
                message,
            ],
        );
    }

    use homeboy_core::test_support::run_git_command as git;

    fn git_output(path: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("git command");
        assert!(
            output.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("utf8")
            .trim()
            .to_string()
    }

    fn git_state_snapshot(path: &Path) -> (String, String, Option<Vec<u8>>) {
        let fetch_head = git_output(path, &["rev-parse", "--git-path", "FETCH_HEAD"]);
        (
            git_output(path, &["status", "--porcelain=v1"]),
            git_output(path, &["worktree", "list", "--porcelain"]),
            std::fs::read(path.join(fetch_head)).ok(),
        )
    }

    fn materialization_source_state(
        path: &Path,
    ) -> (String, String, String, String, String, Option<Vec<u8>>) {
        let fetch_head = git_output(path, &["rev-parse", "--git-path", "FETCH_HEAD"]);
        (
            git_output(path, &["for-each-ref", "--format=%(refname) %(objectname)"]),
            git_output(path, &["status", "--porcelain=v1"]),
            git_output(path, &["rev-parse", "HEAD"]),
            git_output(path, &["write-tree"]),
            git_output(path, &["worktree", "list", "--porcelain"]),
            std::fs::read(path.join(fetch_head)).ok(),
        )
    }
}
