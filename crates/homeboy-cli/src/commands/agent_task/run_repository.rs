//! Repository identity and workspace adapters.

use super::run::*;

#[derive(Debug, Clone)]
pub(super) struct CookRepositoryIdentity {
    repository_name: String,
    slug: String,
    aliases: Vec<String>,
    remote_identity: String,
    workspace_path: PathBuf,
    provenance: String,
    component: Option<homeboy::core::component::Component>,
}

pub(crate) fn resolve_cook_repository(
    args: &mut AgentTaskCookArgs,
) -> homeboy::core::Result<Option<ResolvedCookRepository>> {
    let selected = 'selection: {
        let configured = match args.component.as_deref().or(args.dispatch.repo.as_deref()) {
            Some(name) => cook_components_for_repository_name(name)?,
            None => Vec::new(),
        };
        if let (Some(repo), Some(id)) = (args.dispatch.repo.as_deref(), args.component.as_deref()) {
            if let Some(component) = homeboy::core::component::inventory::registered_base()?
                .iter()
                .find(|c| c.id == id)
            {
                validate_cook_repository_component_selection(repo, component)?;
            }
        }
        if args.dispatch.workspace.is_none()
            && args.dispatch.cwd.is_none()
            && args.to_worktree.is_none()
        {
            break 'selection bind_cook_repository_identity_from_config(args, configured)?;
        }
        let configured = if configured.is_empty() {
            homeboy::core::component::inventory::registered_base()?
        } else {
            configured
        };
        let mut candidates = BTreeMap::new();
        let mut source_identities = Vec::new();
        // A handle that has not been materialized locally is a provisioning
        // request, not repository evidence. An existing path or native handle can
        // attest its Git remote just like --cwd. This distinction governs both the
        // identity evidence below and whether --to-worktree counts as a supplied
        // workspace when no identity is found.
        let materialized_to_worktree = match args.to_worktree.as_deref() {
            Some(to_worktree) => cook_workspace_path(to_worktree)?
                .is_some()
                .then(|| to_worktree.to_string()),
            None => None,
        };
        for (flag, value) in [
            ("--workspace", args.dispatch.workspace.as_deref()),
            ("--cwd", args.dispatch.cwd.as_deref()),
            ("--to-worktree", materialized_to_worktree.as_deref()),
        ] {
            let Some(value) = value else {
                continue;
            };
            let resolved = cook_repository_identities_for_workspace(
                flag,
                value,
                args.component.as_deref().or(args.dispatch.repo.as_deref()),
                &configured,
            )?;
            source_identities.push((flag, resolved.clone()));
            for identity in resolved {
                candidates.entry(identity.slug.clone()).or_insert(identity);
            }
        }
        if candidates.is_empty() {
            if args.dispatch.workspace.is_some()
                || args.dispatch.cwd.is_some()
                || materialized_to_worktree.is_some()
            {
                let supplied_workspaces = [
                    args.dispatch.workspace.as_deref(),
                    args.dispatch.cwd.as_deref(),
                    materialized_to_worktree.as_deref(),
                ]
                .into_iter()
                .flatten()
                .map(cook_workspace_path)
                .collect::<homeboy::core::Result<Vec<_>>>()?;
                let supplied_git_checkout = supplied_workspaces
                    .into_iter()
                    .flatten()
                    .any(|path| homeboy::core::git::repo_root(&path).is_some());
                if args.dispatch.repo.is_some() {
                    break 'selection bind_cook_repository_identity_from_config(args, configured)?;
                }
                let reason = if supplied_git_checkout {
                    "the supplied Git checkout has no configured repository remote mapping"
                } else {
                    "the supplied workspace is not a Git checkout"
                };
                return Err(homeboy::core::Error::validation_missing_argument(vec![format!(
                "--repo <repo> is required because {reason}; provide --repo <repository-or-configured-component>"
            )]));
            }
            break 'selection bind_cook_repository_identity_from_config(args, configured)?;
        }

        let source_remotes = source_identities
            .iter()
            .flat_map(|(_, identities)| {
                identities
                    .iter()
                    .map(|identity| identity.remote_identity.clone())
            })
            .collect::<std::collections::BTreeSet<_>>();
        if source_remotes.len() != 1
            || source_identities
                .iter()
                .any(|(_, identities)| identities.is_empty())
        {
            return Err(repository_identity_conflict_error(&source_identities));
        }

        let selected = match args.component.as_deref().or(args.dispatch.repo.as_deref()) {
            Some(repo) => select_cook_repository_identity(repo, &candidates)?.ok_or_else(|| {
                repository_identity_error(
                    format!("--repo `{repo}` does not match the supplied workspace repository"),
                    &candidates,
                )
            })?,
            None if candidates.len() == 1 => {
                candidates.values().next().cloned().expect("one candidate")
            }
            None => {
                return Err(repository_identity_error(
                    "the supplied workspace maps to multiple configured repositories".to_string(),
                    &candidates,
                ));
            }
        };
        args.dispatch.repo = Some(selected.repository_name.clone());
        let component = selected.component;
        let component_id = component.as_ref().map(|component| component.id.clone());
        args.component = component_id.clone();
        let root = component
            .as_ref()
            .and_then(|component| homeboy::core::git::repo_root(Path::new(&component.local_path)));
        let component_cwd = component
            .as_ref()
            .map(|component| {
                durable_component_cwd(component, &selected.repository_name, root.as_deref())
            })
            .transpose()?;
        args.repository_identity = Some(serde_json::json!({
            "slug": selected.slug,
            "repository_name": selected.repository_name,
            "component_id": component_id,
            "component_registered": component.is_some(),
            "component_cwd": component_cwd,
            "remote_identity": selected.remote_identity,
            "workspace_path": selected.workspace_path,
            "provenance": selected.provenance,
        }));
        component.map(|component| (component, root))
    };
    let Some((component, root)) = selected else {
        return Ok(None);
    };
    Ok(root.map(|root| {
        let remote = homeboy::core::git::resolve_default_remote(&root);
        let origin_url = if remote == "origin" {
            cook_component_repository_remote(&component)
        } else {
            homeboy::core::git::remote_origin_url(&root)
        };
        let source_checkout = homeboy::core::component::resolution::worktree_source_from_repository(
            &component,
            &root,
            origin_url.as_deref(),
        );
        ResolvedCookRepository {
            component,
            root,
            remote,
            source_checkout,
        }
    }))
}

/// A repo-only Cook has no local checkout to attest before a deferred provider
/// lookup. Prefer configured remote identity; otherwise retain a normalized
/// repository name that the resolved checkout must prove through its remote.
pub(super) fn bind_cook_repository_identity_from_config(
    args: &mut AgentTaskCookArgs,
    configured: Vec<homeboy::core::component::Component>,
) -> homeboy::core::Result<Option<(homeboy::core::component::Component, Option<PathBuf>)>> {
    let Some(repo) = args.dispatch.repo.clone() else {
        return Ok(None);
    };
    let requested = args.component.as_deref().unwrap_or(&repo);
    let component =
        cook_components_matching_repository_name(configured, &normalize_repository_name(requested))
            .into_iter()
            .next();
    if let Some(id) = args.component.as_deref() {
        let selected = component.as_ref().ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "component",
                format!("--component `{id}` is not a registered component"),
                Some(id.to_string()),
                None,
            )
        })?;
        validate_cook_repository_component_selection(&repo, selected)?;
    }
    let root = component
        .as_ref()
        .and_then(|component| homeboy::core::git::repo_root(Path::new(&component.local_path)));
    let repository_name = component
        .as_ref()
        .and_then(cook_component_repository_remote)
        .as_deref()
        .map(normalize_repository_name)
        .unwrap_or_else(|| normalize_repository_name(&repo));
    let component_id = component.as_ref().map(|c| c.id.clone());
    let remote_identity = component
        .as_ref()
        .and_then(cook_component_repository_remote)
        .as_deref()
        .and_then(canonical_remote_identity);
    let component_cwd = component
        .as_ref()
        .map(|c| durable_component_cwd(c, &repository_name, root.as_deref()))
        .transpose()?;
    let mut identity = serde_json::json!({
        "slug": component_id.as_deref().unwrap_or(&repository_name),
        "repository_name": repository_name,
        "component_id": component_id.as_deref().unwrap_or(&repository_name),
        "component_registered": component.is_some(),
        "component_cwd": component_cwd,
        "provenance": match remote_identity {
            Some(_) if component_id.as_deref() == Some(&repo) => "--repo:configured-component",
            Some(_) => "--repo:configured-component-alias",
            None => "--repo:requested-repository",
        },
    });
    if let Some(remote) = remote_identity {
        identity["remote_identity"] = Value::String(remote);
    }
    if component.is_none() && Path::new(&repo).is_dir() {
        identity["repository_path"] = Value::String(repo);
    }
    args.dispatch.repo = Some(repository_name);
    args.component = component_id;
    args.repository_identity = Some(identity);
    Ok(component.map(|component| (component, root)))
}

pub(super) fn durable_component_cwd(
    component: &homeboy::core::component::Component,
    repository_name: &str,
    root: Option<&Path>,
) -> homeboy::core::Result<String> {
    let component_path = PathBuf::from(&component.local_path);
    let component_path = component_path.canonicalize().map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "component",
            format!(
                "canonicalize configured component `{}`: {error}",
                component.id
            ),
            Some(component.local_path.clone()),
            None,
        )
    })?;
    let Some(repository_root) = root else {
        if normalize_repository_name(&component.id) == repository_name {
            return Ok(".".to_string());
        }
        return Err(homeboy::core::Error::validation_invalid_argument(
            "component",
            format!(
                "configured component `{}` is not inside a Git repository",
                component.id
            ),
            Some(component.local_path.clone()),
            None,
        ));
    };
    let relative = component_path.strip_prefix(repository_root).map_err(|_| {
        homeboy::core::Error::validation_invalid_argument(
            "component",
            format!(
                "configured component `{}` escapes its Git repository",
                component.id
            ),
            Some(component.local_path.clone()),
            None,
        )
    })?;
    Ok(if relative.as_os_str().is_empty() {
        ".".to_string()
    } else {
        relative.display().to_string()
    })
}

pub(crate) fn cook_repository_names_for_selection(
    repo: &str,
) -> homeboy::core::Result<(String, String)> {
    let Some(component) = cook_components_for_repository_name(repo)?
        .into_iter()
        .next()
    else {
        return Err(cook_repository_not_registered_error(repo)?);
    };
    let component_id = component.id.clone();
    let repository_name = cook_component_repository_remote(&component)
        .as_deref()
        .map(normalize_repository_name)
        .unwrap_or_else(|| normalize_repository_name(repo));
    Ok((repository_name, component_id))
}

/// A nested component is owned by its enclosing checkout, not by a historical
/// standalone remote (or its component ID). Keep root-component configured
/// mappings intact: they may intentionally select a non-default remote.
pub(super) fn cook_component_repository_remote(
    component: &homeboy::core::component::Component,
) -> Option<String> {
    let path = Path::new(&component.local_path);
    // Both ordinary checkouts and linked worktrees have a .git entry. Avoid
    // probing Git for root components, especially unrelated inventory entries.
    if path.join(".git").exists()
        || !path
            .ancestors()
            .skip(1)
            .any(|parent| parent.join(".git").exists())
    {
        return component.remote_url.clone();
    }
    if let (Ok(path), Some(root)) = (path.canonicalize(), homeboy::core::git::repo_root(path)) {
        if path != root && path.starts_with(&root) {
            let remote = homeboy::core::git::resolve_default_remote(&root);
            if let Some(url) = homeboy::core::git::remote_url(&root, &remote) {
                return Some(url);
            }
        }
    }
    component.remote_url.clone()
}

pub(crate) fn validate_cook_repository_component_selection(
    repo: &str,
    component: &homeboy::core::component::Component,
) -> homeboy::core::Result<()> {
    let component_id = &component.id;
    let repository = cook_component_repository_remote(component)
        .as_deref()
        .map(normalize_repository_name)
        .unwrap_or_else(|| normalize_repository_name(&component.id));
    let requested = normalize_repository_name(repo);
    let matches = requested == repository
        || requested == normalize_repository_name(&component.id)
        || component
            .aliases
            .iter()
            .any(|alias| requested == normalize_repository_name(alias));
    if !matches {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "component",
            format!(
                "--component `{component_id}` belongs to repository `{repository}`, not `{repo}`"
            ),
            Some(component_id.to_string()),
            None,
        ));
    }
    Ok(())
}

pub(super) fn cook_component_path_for_repository_name(
    repo: &str,
) -> homeboy::core::Result<Option<PathBuf>> {
    Ok(cook_components_for_repository_name(repo)?
        .into_iter()
        .next()
        .map(|component| PathBuf::from(component.local_path)))
}

pub(super) fn cook_component_id(args: &AgentTaskCookArgs) -> Option<&str> {
    args.component
        .as_deref()
        .or_else(|| {
            args.repository_identity
                .as_ref()
                .and_then(|identity| identity.get("component_id"))
                .and_then(Value::as_str)
        })
        .or(args.dispatch.repo.as_deref())
}

pub(super) fn cook_components_for_repository_name(
    repository_name: &str,
) -> homeboy::core::Result<Vec<homeboy::core::component::Component>> {
    let path_id;
    let repository_name = if Path::new(repository_name).is_absolute() {
        match homeboy::core::component::resolve_registered_primary_identity(repository_name)? {
            homeboy::core::component::RegisteredPrimaryPathResolution::Primary(id) => {
                path_id = id;
                path_id.as_str()
            }
            _ => return Ok(Vec::new()),
        }
    } else {
        repository_name
    };
    let repository_name = normalize_repository_name(repository_name);
    // An exact component ID is the common Cook path. Do not hydrate every
    // registered primary checkout before selecting it: portable enrichment of
    // one stale checkout can otherwise block destination resolution.
    if let Some(component) =
        homeboy::core::component::inventory::registered_primary_by_id(&repository_name)?
    {
        // The ID names this repository slug, but the component's own remote
        // can name a different repository (a renamed repository whose old
        // name is still a component ID, #16787). When another registered
        // component's remote is the requested repository, the slug is
        // ambiguous: report both rather than silently choosing the ID.
        let id_repository = component
            .remote_url
            .as_deref()
            .map(normalize_repository_name);
        if id_repository
            .as_deref()
            .is_some_and(|repository| repository != repository_name)
        {
            let remote_matches = homeboy::core::component::inventory::registered_primary()?
                .into_iter()
                .filter(|candidate| candidate.id != component.id)
                .filter(|candidate| {
                    candidate
                        .remote_url
                        .as_deref()
                        .is_some_and(|remote| normalize_repository_name(remote) == repository_name)
                })
                .collect::<Vec<_>>();
            if !remote_matches.is_empty() {
                let mut candidates = vec![component];
                candidates.extend(remote_matches);
                return Err(repository_component_identity_ambiguity_error(
                    repository_name,
                    &candidates,
                ));
            }
        }
        return Ok(vec![component]);
    }
    let primary_matches = cook_components_matching_repository_name(
        homeboy::core::component::inventory::registered_primary()?,
        &repository_name,
    );
    if !primary_matches.is_empty() {
        if primary_matches.len() > 1 {
            return Err(repository_component_identity_ambiguity_error(
                repository_name,
                &primary_matches,
            ));
        }
        let selected = &primary_matches[0];
        return Ok(cook_registered_component_by_id(&selected.id)?
            .into_iter()
            .collect());
    }
    let components = homeboy::core::component::inventory::registered_base()?;
    let matches = cook_components_matching_repository_name(components, &repository_name);
    if matches.len() > 1 {
        return Err(repository_component_identity_ambiguity_error(
            repository_name,
            &matches,
        ));
    }
    Ok(matches)
}

pub(super) fn cook_repository_not_registered_error(
    repository: &str,
) -> homeboy::core::Result<homeboy::core::Error> {
    let components = homeboy::core::component::inventory::registered_primary()?;
    let listing = if components.is_empty() {
        "  (none)".to_string()
    } else {
        components
            .iter()
            .map(|component| {
                format!(
                    "  {} — remote: {}; primary path: {}",
                    component.id,
                    component.remote_url.as_deref().unwrap_or("(none)"),
                    component.local_path,
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    Ok(homeboy::core::Error::validation_invalid_argument(
        "repo",
        format!("--repo `{repository}` does not match a registered component. Registered components:\n{listing}"),
        Some(repository.to_string()),
        None,
    ))
}

pub(super) fn cook_components_matching_repository_name(
    components: Vec<homeboy::core::component::Component>,
    repository_name: &str,
) -> Vec<homeboy::core::component::Component> {
    components
        .into_iter()
        .filter(|component| {
            normalize_repository_name(&component.id) == repository_name
                || component
                    .aliases
                    .iter()
                    .any(|alias| normalize_repository_name(alias) == repository_name)
                || cook_component_repository_remote(component)
                    .as_deref()
                    .is_some_and(|remote| normalize_repository_name(remote) == repository_name)
        })
        .collect()
}

pub(super) fn select_cook_repository_identity(
    repository_name: &str,
    candidates: &BTreeMap<String, CookRepositoryIdentity>,
) -> homeboy::core::Result<Option<CookRepositoryIdentity>> {
    let repository_name = normalize_repository_name(repository_name);
    if let Some(candidate) = candidates
        .values()
        .find(|candidate| candidate.slug.eq_ignore_ascii_case(&repository_name))
    {
        return Ok(Some(candidate.clone()));
    }
    let matches = candidates
        .values()
        .filter(|candidate| {
            candidate.repository_name == repository_name
                || candidate
                    .aliases
                    .iter()
                    .any(|alias| normalize_repository_name(alias) == repository_name)
        })
        .cloned()
        .collect::<Vec<_>>();
    if matches.len() > 1 {
        let matches = matches
            .into_iter()
            .map(|candidate| (candidate.slug.clone(), candidate))
            .collect();
        return Err(repository_identity_error(
            format!("--repo `{repository_name}` is ambiguous for the supplied workspace"),
            &matches,
        ));
    }
    Ok(matches.into_iter().next())
}

pub(super) fn repository_component_identity_ambiguity_error(
    repository_name: String,
    components: &[homeboy::core::component::Component],
) -> homeboy::core::Error {
    let candidates = components
        .iter()
        .map(|component| {
            let repository = component
                .remote_url
                .as_deref()
                .map(normalize_repository_name)
                .unwrap_or_else(|| "no configured remote".to_string());
            format!("{} ({repository})", component.id)
        })
        .collect::<Vec<_>>();
    let recovery = components
        .iter()
        .map(|component| format!("homeboy agent-task cook --repo {} ...", component.id))
        .collect();
    homeboy::core::Error::validation_invalid_argument(
        "repo",
        format!(
            "--repo `{repository_name}` matches multiple configured component identities; candidates: {}",
            candidates.join(", ")
        ),
        None,
        Some(recovery),
    )
}

pub(crate) fn normalize_repository_name(repository: &str) -> String {
    homeboy::core::engine::identifier::repository_id(repository).unwrap_or_default()
}

pub(super) fn cook_repository_identities_for_workspace(
    flag: &str,
    value: &str,
    requested_repository: Option<&str>,
    configured: &[homeboy::core::component::Component],
) -> homeboy::core::Result<Vec<CookRepositoryIdentity>> {
    let Some(workspace_path) = cook_workspace_path(value)? else {
        return Ok(Vec::new());
    };
    let Some(git_root) = homeboy::core::git::repo_root(&workspace_path) else {
        return Ok(Vec::new());
    };
    let remotes = homeboy::core::git::output_optional(&git_root, &["remote"])
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|remote| !remote.is_empty())
        .filter_map(|remote| {
            homeboy::core::git::remote_url(&git_root, remote).map(|url| (remote.to_string(), url))
        })
        .collect::<Vec<_>>();
    let mut identities = Vec::new();
    for (remote_name, remote_url) in remotes {
        let Some(remote_identity) = canonical_remote_identity(&remote_url) else {
            continue;
        };
        for component in configured {
            let Some(component_identity) = cook_component_repository_remote(component)
                .as_deref()
                .and_then(canonical_remote_identity)
            else {
                continue;
            };
            if component_identity == remote_identity {
                identities.push(CookRepositoryIdentity {
                    repository_name: normalize_repository_name(&remote_url),
                    slug: component.id.clone(),
                    aliases: component.aliases.clone(),
                    remote_identity: remote_identity.clone(),
                    workspace_path: git_root.clone(),
                    provenance: format!("{flag}:git-remote:{remote_name}"),
                    component: Some(component.clone()),
                });
            }
        }
        if requested_repository.is_some()
            && !identities
                .iter()
                .any(|identity| identity.remote_identity == remote_identity)
        {
            let repository_name = normalize_repository_name(&remote_url);
            identities.push(CookRepositoryIdentity {
                repository_name: repository_name.clone(),
                slug: repository_name,
                aliases: Vec::new(),
                remote_identity,
                workspace_path: git_root.clone(),
                provenance: format!("{flag}:git-remote:{remote_name}"),
                component: None,
            });
        }
    }
    Ok(identities)
}

pub(super) fn cook_workspace_path(value: &str) -> homeboy::core::Result<Option<PathBuf>> {
    let path = Path::new(value);
    if path.is_dir() {
        return std::fs::canonicalize(path).map(Some).map_err(|error| {
            homeboy::core::Error::internal_io(error.to_string(), Some(path.display().to_string()))
        });
    }
    Ok(
        homeboy::core::worktree_provider::resolve_native_worktree_mutation_target(value)?
            .map(|target| target.path),
    )
}

/// Publish an already materialized destination before runtime sealing. Reuse
/// Cook's repository and linked-worktree validators; unresolved destinations
/// remain pending until the normal provisioning owner materializes them.
pub(crate) fn existing_cook_caller_workspace(
    args: &AgentTaskCookArgs,
) -> homeboy::core::Result<Option<Value>> {
    let Some(destination) = args.dispatch.cwd.as_deref().or(args.to_worktree.as_deref()) else {
        return Ok(None);
    };
    let Some(path) = cook_workspace_path(destination)? else {
        return Ok(None);
    };
    let mut resolved = args.clone();
    resolve_cook_repository(&mut resolved)?;
    validate_cook_destination_identity(&resolved, &path)?;
    if let (Some(_), Some(handle)) = (&args.dispatch.cwd, &args.to_worktree) {
        validate_cook_cwd_destination_identity(&path, handle)?;
    }
    Ok(Some(serde_json::json!({
        "repository": resolved.dispatch.repo,
        "working_directory": path,
    })))
}

pub(crate) fn canonical_remote_identity(remote_url: &str) -> Option<String> {
    let remote_url = remote_url.trim();
    let (host, path) = if let Some((_, rest)) = remote_url.split_once("://") {
        let (authority, path) = rest.split_once('/')?;
        (authority.rsplit('@').next()?, path)
    } else {
        let (authority, path) = remote_url.split_once(':')?;
        (authority.rsplit('@').next()?, path)
    };
    let path = path.trim_matches('/').trim_end_matches(".git");
    (!host.is_empty()
        && path
            .split('/')
            .filter(|segment| !segment.is_empty())
            .count()
            >= 2)
        .then(|| {
            format!(
                "git://{}/{}",
                host.to_ascii_lowercase(),
                path.to_ascii_lowercase()
            )
        })
}

pub(super) fn repository_identity_conflict_error(
    sources: &[(&str, Vec<CookRepositoryIdentity>)],
) -> homeboy::core::Error {
    let candidates = sources
        .iter()
        .map(|(flag, identities)| {
            let identities = identities
                .iter()
                .map(|identity| identity.remote_identity.as_str())
                .collect::<Vec<_>>();
            format!(
                "{flag}: {}",
                if identities.is_empty() {
                    "unresolved".to_string()
                } else {
                    identities.join(", ")
                }
            )
        })
        .collect::<Vec<_>>();
    homeboy::core::Error::validation_invalid_argument(
        "workspace",
        format!(
            "--workspace, --cwd, and --to-worktree must resolve to the same configured repository identity; --repo cannot select one conflicting checkout: {}",
            candidates.join("; ")
        ),
        None,
        None,
    )
}

pub(super) fn validate_cook_destination_identity(
    args: &AgentTaskCookArgs,
    destination: &Path,
) -> homeboy::core::Result<()> {
    let identity = args.repository_identity.as_ref();
    validate_cook_worktree_root(destination, &destination.display().to_string())?;
    let Some(expected) = identity
        .and_then(|identity| identity.get("remote_identity"))
        .and_then(Value::as_str)
    else {
        return Ok(());
    };
    let remote = homeboy::core::git::resolve_default_remote(destination);
    let actual = homeboy::core::git::remote_url(destination, &remote)
        .as_deref()
        .and_then(canonical_remote_identity);
    if actual.as_deref() == Some(expected) {
        Ok(())
    } else {
        Err(homeboy::core::Error::validation_invalid_argument(
            "to_worktree",
            "Cook destination does not match the selected repository identity",
            Some(destination.display().to_string()),
            None,
        ))
    }
}

pub(super) fn validate_cook_worktree_root(
    path: &Path,
    reference: &str,
) -> homeboy::core::Result<()> {
    homeboy::core::worktree_provider::require_linked_worktree_mutation_path(path, reference)
        .map(|_| ())
}

pub(super) fn repository_identity_error(
    message: String,
    candidates: &BTreeMap<String, CookRepositoryIdentity>,
) -> homeboy::core::Error {
    let candidates = candidates
        .values()
        .map(|candidate| {
            format!(
                "{} ({}, {})",
                candidate.slug,
                candidate.remote_identity,
                candidate.workspace_path.display()
            )
        })
        .collect::<Vec<_>>();
    let recovery = candidates
        .iter()
        .map(|candidate| {
            let slug = candidate
                .split_whitespace()
                .next()
                .expect("candidate has slug");
            format!("homeboy agent-task cook --repo {slug} ...")
        })
        .collect::<Vec<_>>();
    homeboy::core::Error::validation_invalid_argument(
        "repo",
        format!("{message}; candidates: {}", candidates.join(", ")),
        None,
        Some(recovery),
    )
}

/// Upper bound on branch-name collisions before derivation gives up.
pub(super) const MAX_DERIVED_COOK_GENERATIONS: u32 = 50;

/// A new issue attempt never adopts an earlier branch or workspace.
pub(super) fn fresh_derived_cook_branch(
    repository: &str,
    base: &str,
    source_checkout: Option<&Path>,
) -> homeboy::core::Result<String> {
    use homeboy::core::worktree::convergence::PrHistory;
    // Replayed admissions can carry only the component id, not a source
    // checkout. PR history still retires a generation in that case.
    let registered_source = if source_checkout.is_none() {
        homeboy::core::component::inventory::registered_primary_by_id(repository)?
            .map(|component| PathBuf::from(component.local_path))
    } else {
        None
    };
    let source_checkout = source_checkout.or(registered_source.as_deref());
    for generation in 1..=MAX_DERIVED_COOK_GENERATIONS {
        let branch = if generation == 1 {
            base.to_string()
        } else {
            format!("{base}-{generation}")
        };
        let handle =
            homeboy::core::worktree_provider::native_handle_for_branch(repository, &branch)?;
        let record = homeboy::core::worktree::resolve_workspace_ref_if_present(&handle)?;
        if record.is_some() {
            continue;
        }
        if let Some(path) = source_checkout {
            if homeboy::core::worktree::stale_destination::adoptable_branch_ref(path, &branch)
                .is_some()
            {
                continue;
            }
            // Remote PR history can outlive a deleted local branch.
            if !matches!(
                homeboy::core::worktree::convergence::pr_history(path, &branch, None)?,
                PrHistory::None
            ) {
                continue;
            }
        }
        return Ok(branch);
    }
    Err(homeboy::core::Error::validation_invalid_argument(
        "to_worktree",
        format!(
            "every derived destination for `{base}` up to generation {MAX_DERIVED_COOK_GENERATIONS} is retired; pass --to-worktree and --head explicitly"
        ),
        Some(base.to_string()),
        None,
    ))
}

pub(super) fn derived_cook_branch(task_url: &str) -> homeboy::core::Result<String> {
    let issue = normalize_cook_task_url(task_url);
    let issue = issue.as_str();
    let Some((repository, number)) = issue.rsplit_once("/issues/") else {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "task_url",
            "--task-url must be a GitHub issue URL when --to-worktree is omitted",
            Some(task_url.to_string()),
            None,
        ));
    };
    let number = number.split('/').next().unwrap_or_default();
    if number.is_empty() || !number.chars().all(|character| character.is_ascii_digit()) {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "task_url",
            "--task-url must end in a numeric GitHub issue number when --to-worktree is omitted",
            Some(task_url.to_string()),
            None,
        ));
    }
    let mut segments = repository.trim_end_matches('/').rsplit('/');
    let repo = segments.next().unwrap_or_default();
    let owner = segments.next().unwrap_or_default();
    if owner.is_empty() || repo.is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "task_url",
            "--task-url must include a GitHub owner and repo when --to-worktree is omitted",
            Some(task_url.to_string()),
            None,
        ));
    }
    Ok(format!(
        "fix/issue-{number}-{}",
        homeboy_engine_primitives::identifier::slugify(
            repo,
            homeboy_engine_primitives::identifier::SlugStyle::Branch,
        )
    ))
}

/// Reduce an issue URL to the issue it names.
///
/// Links copied from GitHub commonly carry a query string or a fragment
/// (`.../issues/123?source=...#details`). Neither is part of the issue's
/// identity, so both are dropped before the issue number is read; otherwise a
/// valid issue URL is rejected as not ending in a number.
pub(super) fn normalize_cook_task_url(task_url: &str) -> String {
    task_url
        .trim()
        .split(['?', '#'])
        .next()
        .unwrap_or_default()
        .trim_end_matches('/')
        .to_string()
}
